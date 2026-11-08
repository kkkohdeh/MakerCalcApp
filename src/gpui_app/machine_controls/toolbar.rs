use super::{Control, availability, choices};
use crate::gpui_app::{
    MakerCalcGpuiApp, PrinterEntry, bay, components::button::Button, icons, materials,
};
use crate::i18n::t;
use crate::shared::control::ControlError;
use crate::state::LocalPrinterId;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div, px,
};

const HEADER_CONTROLS: [Control; 4] = [
    Control::Speed,
    Control::ZOffset,
    Control::ClearPlate,
    Control::AutoShutdown(true),
];

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
        let focus_handles = HEADER_CONTROLS.map(|control| {
            let name = control.header_id().expect("toolbar control has an ID");
            let focus = window
                .use_keyed_state((name, id.0), cx, |_, cx| cx.focus_handle())
                .read(cx)
                .clone();
            (name, focus)
        });
        let status = printer.state.telemetry.print_state;
        let machine = &printer.detail_state.machine;
        choices(printer)
            .into_iter()
            .filter_map(|choice| {
                let header = choice.control.header()?;
                let (_, focus) = focus_handles.iter().find(|(name, _)| *name == header.id)?;
                let blocked = availability(printer, choice.control, pending).err();
                let hint = match blocked {
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
                        if matches!(choice.control, Control::AutoShutdown(_))
                            && status != crate::shared::printer::PrintStatus::Idle =>
                    {
                        t!("printers.motion.not_idle").into_owned()
                    }
                    Some(ControlError::InvalidState)
                        if matches!(choice.control, Control::AutoShutdown(_))
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
            .collect()
    }
}
