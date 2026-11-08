use super::{Choice, Control, availability, choices, control_device, label, motion};
use crate::gpui_app::{
    MakerCalcGpuiApp, PrinterEntry, bay, components::button::Button, icons, materials,
};
use crate::i18n::t;
use crate::shared::control::{CalibrationKind, ControlError};
use crate::shared::machine::CalibrationReadiness;
use crate::state::LocalPrinterId;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div, px,
};

const HEADER_CONTROLS: [Control; 7] = [
    Control::Speed,
    Control::ZOffset,
    Control::ClearPlate,
    Control::AutoShutdown(true),
    Control::Name,
    Control::Calibration(CalibrationKind::BedLeveling),
    Control::Calibration(CalibrationKind::VibrationCompensation),
];

const EXTRUSION_ID: &str = "printer-extrude";

pub(super) fn physical_block_message(
    printer: &PrinterEntry,
    control: Control,
    error: ControlError,
) -> Option<String> {
    if let Control::Extrude(id) = control
        && control_device(printer, id)?.extrusion_requires_material_idle
        && error == ControlError::Busy
        && printer.detail_state.machine.material_motion_idle == Some(false)
    {
        return Some(t!("printers.controls.extrude_material_busy").into_owned());
    }
    if error != ControlError::InvalidState
        || !matches!(control, Control::Calibration(_) | Control::Extrude(_))
    {
        return None;
    }
    if let Err(key) = motion::availability(printer, false) {
        return Some(t!(key).into_owned());
    }
    let key = if let Control::Extrude(id) = control {
        let device = control_device(printer, id)?;
        if device.extrusion_requires_material_idle {
            if printer.state.telemetry.print_state != crate::shared::printer::PrintStatus::Idle {
                return Some(t!("printers.materials.controls.not_idle").into_owned());
            }
            if printer.detail_state.machine.material_motion_idle.is_none() {
                return Some(t!("printers.controls.clear_plate_material_unknown").into_owned());
            }
        }
        match device.extrusion_limit_mm {
            None => "printers.controls.extrude_limit_unknown",
            Some(0.) => "printers.controls.extrude_disabled",
            _ => "printers.controls.extrude_not_ready",
        }
    } else {
        match printer.detail_state.machine.calibration_readiness {
            Some(CalibrationReadiness::NeedsHoming) => {
                "printers.controls.calibration_home_required"
            }
            None | Some(CalibrationReadiness::NotReady) => "printers.controls.calibration_wait",
            _ => return None,
        }
    };
    Some(t!(key).into_owned())
}

pub(super) fn localized_decimal(value: &str) -> String {
    if rust_i18n::locale().starts_with("es") {
        value.replace('.', ",")
    } else {
        value.to_owned()
    }
}

struct HeaderAction {
    id: &'static str,
    svg: &'static [u8],
    badge: Option<&'static str>,
}

impl Control {
    fn header(self) -> Option<HeaderAction> {
        let (id, svg, badge) = match self {
            Self::Speed => ("print-speed-override", bay::SPEED_ICON, Some("%")),
            Self::ZOffset => ("printer-z-offset", icons::SVG_Z_OFFSET, Some("Z")),
            Self::ClearPlate => ("printer-clear-plate", icons::SVG_CLEAR_PLATE, None),
            Self::AutoShutdown(_) => ("printer-auto-shutdown", icons::SVG_AUTO_SHUTDOWN, None),
            Self::Name => ("printer-device-name", icons::SVG_PRINTER_NAME, None),
            Self::Calibration(CalibrationKind::BedLeveling) => {
                ("printer-level-bed", icons::SVG_LEVEL_BED, None)
            }
            Self::Calibration(CalibrationKind::VibrationCompensation) => {
                ("printer-calibrate-vibration", icons::SVG_VIBRATION, None)
            }
            Self::Extrude(_) => (EXTRUSION_ID, icons::SVG_EXTRUDE, None),
            _ => return None,
        };
        Some(HeaderAction { id, svg, badge })
    }

    pub(super) fn header_id(self) -> Option<&'static str> {
        self.header().map(|action| action.id)
    }
}

impl MakerCalcGpuiApp {
    pub(in crate::gpui_app) fn render_machine_header_controls(
        id: LocalPrinterId,
        printer: &PrinterEntry,
        pending: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let focus_handles = HEADER_CONTROLS
            .into_iter()
            .filter_map(Control::header_id)
            .chain([EXTRUSION_ID])
            .map(|name| {
                let focus = window
                    .use_keyed_state((name, id.0), cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone();
                (name, focus)
            })
            .collect::<Vec<_>>();
        let status = printer.state.telemetry.print_state;
        let machine = &printer.detail_state.machine;
        let (fans, other): (Vec<_>, Vec<_>) = choices(printer)
            .into_iter()
            .partition(|choice| matches!(choice.control, Control::Fan(_)));
        let fan_control = fan_control(id, printer, fans, pending, window, cx);
        other
            .into_iter()
            .filter_map(|choice| {
                let header = choice.control.header()?;
                let (_, focus) = focus_handles.iter().find(|(name, _)| *name == header.id)?;
                let blocked = availability(printer, choice.control, pending).err();
                let hint = match blocked {
                    Some(error)
                        if matches!(
                            choice.control,
                            Control::Calibration(_) | Control::Extrude(_)
                        ) =>
                    {
                        physical_block_message(printer, choice.control, error)
                            .unwrap_or_else(|| materials::result_message(error))
                    }
                    Some(ControlError::InvalidState)
                        if choice.control == Control::ClearPlate
                            && !status.is_completed_or_cancelled() =>
                    {
                        t!("printers.controls.clear_plate_terminal").into_owned()
                    }
                    Some(ControlError::InvalidState)
                        if choice.control == Control::ClearPlate
                            && machine.material_motion_idle.is_none() =>
                    {
                        t!("printers.controls.clear_plate_material_unknown").into_owned()
                    }
                    Some(ControlError::InvalidState)
                        if matches!(choice.control, Control::Speed | Control::ZOffset)
                            && machine.print_adjustments_require_active_job
                            && !machine.allows_print_adjustment(status) =>
                    {
                        t!("printers.controls.adjustment_active_job").into_owned()
                    }
                    Some(ControlError::InvalidState)
                        if matches!(choice.control, Control::AutoShutdown(_))
                            && machine.auto_shutdown_enabled.is_none() =>
                    {
                        t!("printers.controls.auto_shutdown_unknown").into_owned()
                    }
                    Some(ControlError::InvalidState)
                        if matches!(choice.control, Control::AutoShutdown(_) | Control::Name)
                            && status != crate::shared::printer::PrintStatus::Idle =>
                    {
                        t!("printers.materials.controls.not_idle").into_owned()
                    }
                    Some(ControlError::InvalidState)
                        if matches!(choice.control, Control::AutoShutdown(_) | Control::Name)
                            && machine.material_motion_idle.is_none() =>
                    {
                        t!("printers.controls.clear_plate_material_unknown").into_owned()
                    }
                    Some(error) => materials::result_message(error),
                    None => choice.label.clone(),
                };
                Some(
                    Button::new(header.id)
                        .focus_handle(focus)
                        .ghost()
                        .small()
                        .w(px(30.))
                        .px_0()
                        .icon(Icon::default().data(header.svg))
                        .when_some(header.badge, |button, badge| {
                            button.child(
                                div()
                                    .absolute()
                                    .right(px(3.))
                                    .top(px(1.))
                                    .text_size(px(9.))
                                    .child(badge),
                            )
                        })
                        .accessibility_label(choice.label.clone())
                        .tooltip(hint)
                        .popup_trigger()
                        .disabled(blocked.is_some())
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.open_machine_editor(id, choice.clone(), window, cx);
                        }))
                        .into_any_element(),
                )
            })
            .chain(fan_control)
            .collect()
    }
}

fn fan_control(
    id: LocalPrinterId,
    printer: &PrinterEntry,
    fans: Vec<Choice>,
    pending: bool,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Option<AnyElement> {
    let first = fans.first()?.clone();
    let blocked = availability(printer, first.control, pending).err();
    let name = if fans.len() == 1 {
        first.label.clone()
    } else {
        label("fan_power")
    };
    let hint = match blocked {
        Some(ControlError::InvalidState)
            if printer
                .detail_state
                .machine
                .print_adjustments_require_active_job
                && !printer
                    .detail_state
                    .machine
                    .allows_print_adjustment(printer.state.telemetry.print_state) =>
        {
            t!("printers.controls.adjustment_active_job").into_owned()
        }
        Some(error) => materials::result_message(error),
        None => name.clone(),
    };
    let focus = window
        .use_keyed_state(("printer-fan-power", id.0), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let button = Button::new("printer-fan-power")
        .focus_handle(&focus)
        .ghost()
        .small()
        .w(px(30.))
        .px_0()
        .icon(Icon::default().data(bay::FAN_ICON))
        .accessibility_label(name)
        .tooltip(hint)
        .disabled(blocked.is_some());
    if fans.len() == 1 {
        return Some(
            button
                .popup_trigger()
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.open_machine_editor(id, first.clone(), window, cx)
                }))
                .into_any_element(),
        );
    }
    let app = cx.entity().downgrade();
    Some(
        button
            .dropdown_menu(move |mut menu, _, _| {
                for choice in &fans {
                    let (choice, app, opener) = (choice.clone(), app.clone(), focus.clone());
                    menu = menu.item(PopupMenuItem::new(choice.label.clone()).on_click(
                        move |_, window, cx| {
                            opener.focus(window, cx);
                            let _ = app.update(cx, |app, cx| {
                                app.open_machine_editor(id, choice.clone(), window, cx)
                            });
                        },
                    ));
                }
                menu.scrollable(true)
            })
            .into_any_element(),
    )
}
