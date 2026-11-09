use super::{Choice, Control, availability, choices, control_device, label, motion};
use crate::gpui_app::{
    MakerCalcGpuiApp, PrinterEntry, bay, components::button::Button, icons, materials,
};
use crate::i18n::t;
use crate::shared::control::{CalibrationKind, ControlError, MachineAction};
use crate::shared::machine::{CalibrationReadiness, PrintSpeedProfile};
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
        let drying_control = super::drying::toolbar_button(id, printer, window, cx);
        other
            .into_iter()
            .filter_map(|choice| {
                let header = choice.control.header()?;
                let (_, focus) = focus_handles.iter().find(|(name, _)| *name == header.id)?;
                let blocked = availability(printer, choice.control, pending).err();
                let profiles = machine
                    .print_speed_profiles
                    .filter(|_| choice.control == Control::Speed);
                let hint = match blocked {
                    Some(ControlError::InvalidState) if profiles.is_some_and(|p| !p.can_change) => {
                        t!("printers.controls.speed_profile_running").into_owned()
                    }
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
                    None => profiles.map_or_else(
                        || choice.label.clone(),
                        |p| {
                            t!(
                                "printers.controls.speed_profile_current",
                                profile = profile_label(p.current)
                            )
                            .into_owned()
                        },
                    ),
                };
                let button = Button::new(header.id)
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
                    .disabled(blocked.is_some());
                if let Some(profiles) = profiles {
                    return Some(speed_profile_menu(
                        button,
                        id,
                        printer,
                        profiles.current,
                        cx,
                    ));
                }
                Some(
                    button
                        .popup_trigger()
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.open_machine_editor(id, choice.clone(), window, cx);
                        }))
                        .into_any_element(),
                )
            })
            .chain(fan_control)
            .chain(drying_control)
            .collect()
    }
}

fn profile_label(profile: PrintSpeedProfile) -> String {
    let key = match profile {
        PrintSpeedProfile::Silent => "printers.controls.speed_silent",
        PrintSpeedProfile::Standard => "printers.controls.speed_standard",
        PrintSpeedProfile::Sport => "printers.controls.speed_sport",
        PrintSpeedProfile::Ludicrous => "printers.controls.speed_ludicrous",
    };
    t!(key).into_owned()
}

fn speed_profile_menu(
    button: Button,
    id: LocalPrinterId,
    printer: &PrinterEntry,
    current: PrintSpeedProfile,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let app = cx.entity().downgrade();
    let session = printer.session_generation;
    let epoch = printer.state.connection_epoch;
    let job = printer.state.machine_job_target();
    button
        .dropdown_menu(move |mut menu, _, _| {
            for profile in PrintSpeedProfile::selectable() {
                let name = t!(
                    "printers.controls.speed_profile",
                    profile = profile_label(profile),
                    percent = profile.percent()
                );
                let (app, job) = (app.clone(), job.clone());
                menu = menu.item(
                    PopupMenuItem::new(name.into_owned())
                        .checked(profile == current)
                        .on_click(move |_, _, cx| {
                            let _ = app.update(cx, |app, cx| {
                                let Some(printer) = app.printers.get(&id).filter(|printer| {
                                    app.selected == Some(id)
                                        && app.detail_tab
                                            == crate::gpui_app::detail::DetailTab::Overview
                                        && printer.session_generation == session
                                        && printer.state.connection_epoch == epoch
                                        && printer.state.matches_machine_job_target(&job)
                                        && printer
                                            .detail_state
                                            .machine
                                            .print_speed_profiles
                                            .is_some_and(|p| p.current != profile)
                                        && availability(
                                            printer,
                                            Control::Speed,
                                            app.machine_controls.is_pending(id),
                                        )
                                        .is_ok()
                                }) else {
                                    return;
                                };
                                let action = MachineAction::SetPrintSpeedOverride {
                                    percent: profile.percent(),
                                };
                                match printer.operation.machine_action_for_job(
                                    epoch,
                                    Some(job.clone()),
                                    action.clone(),
                                ) {
                                    Ok(reply) => app.track_machine_request(
                                        id, session, epoch, action, reply, cx,
                                    ),
                                    Err(error) => app.push_toast(
                                        super::ToastKind::Error,
                                        materials::result_message(error),
                                        None,
                                        cx,
                                    ),
                                }
                            });
                        }),
                );
            }
            menu
        })
        .into_any_element()
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
