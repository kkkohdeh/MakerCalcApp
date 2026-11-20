use super::super::components::{alert_dialog::backdrop, button::Button, dialog, input::Input};
use super::{Control, MakerCalcGpuiApp, PrinterEntry, availability, control_device};
use crate::i18n::t;
use crate::shared::control::{ControlError, ControlResult, MachineAction};
use crate::shared::workspace::{Device, ResourceId};
use crate::state::{LocalPrinterId, MachineJobTarget};
use gpui_kit::base::{FocusTrapElement as _, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

#[derive(Default)]
pub(super) struct State {
    editor: Option<Editor>,
    rendered: bool,
}

impl State {
    pub(super) fn is_visible(&self) -> bool {
        self.editor.is_some() || self.rendered
    }
}

#[derive(Clone)]
struct Editor {
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
    nozzle: ResourceId,
    job: MachineJobTarget,
    title: String,
    distance: Entity<InputState>,
    limits: Limits,
    retract: bool,
    submitted: bool,
    focus: FocusHandle,
    opener: Option<FocusHandle>,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
struct Limits {
    distance: f64,
}

fn limits(device: &Device) -> Option<Limits> {
    let distance = device
        .extrusion_limit_mm
        .filter(|max| max.is_finite() && *max > 0.)?;
    Some(Limits { distance })
}

fn input(
    value: String,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> Entity<InputState> {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|text, _| {
                text.len() <= 7
                    && text
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b','))
                    && text.matches(['.', ',']).count() <= 1
                    && text
                        .split(['.', ','])
                        .nth(1)
                        .is_none_or(|fraction| fraction.len() <= 2)
            })
            .default_value(value)
    });
    cx.observe(&input, |_, _, cx| cx.notify()).detach();
    input
}

fn field(
    input: &Entity<InputState>,
    id: &'static str,
    label: String,
    unit: &'static str,
    disabled: bool,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(div().font_medium().child(label.clone()))
        .child(
            Input::new(input)
                .id(id)
                .aria_label(label)
                .disabled(disabled)
                .suffix(div().text_color(cx.theme().muted_foreground).child(unit)),
        )
}

impl Editor {
    fn printer<'a>(&self, app: &'a MakerCalcGpuiApp) -> Result<&'a PrinterEntry, ControlError> {
        let printer = app
            .printers
            .get(&self.printer)
            .ok_or(ControlError::Disconnected)?;
        if app.selected != Some(self.printer)
            || app.detail_tab != super::super::detail::DetailTab::Overview
            || printer.session_generation != self.session
            || printer.state.connection_epoch != self.epoch
            || !printer.state.matches_machine_job_target(&self.job)
            || control_device(printer, self.nozzle).and_then(limits) != Some(self.limits)
        {
            return Err(ControlError::StaleResource);
        }
        if !self.submitted {
            availability(
                printer,
                Control::Extrude(self.nozzle),
                app.machine_controls.is_pending(self.printer),
            )?;
        }
        Ok(printer)
    }

    fn action(&self, printer: &PrinterEntry, cx: &App) -> Result<MachineAction, ControlError> {
        let distance = super::intent::parse_decimal(self.distance.read(cx).value().as_ref())?;
        if !distance.is_finite() || distance <= 0. {
            return Err(ControlError::InvalidData);
        }
        let action = MachineAction::ExtrudeFilament {
            nozzle_id: self.nozzle,
            length_mm: if self.retract { -distance } else { distance },
            speed_mm_s: None,
            target_c: None,
        };
        action.validate_extrusion(
            control_device(printer, self.nozzle).ok_or(ControlError::StaleResource)?,
        )?;
        Ok(action)
    }
}

impl MakerCalcGpuiApp {
    pub(super) fn open_extrusion(
        &mut self,
        id: LocalPrinterId,
        nozzle: ResourceId,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(printer) = self.printers.get(&id) else {
            return;
        };
        let Some(device) = control_device(printer, nozzle) else {
            return;
        };
        let Some(ranges) = limits(device) else {
            return;
        };
        let (session, epoch, job) = (
            printer.session_generation,
            printer.state.connection_epoch,
            printer.state.machine_job_target(),
        );
        let distance = input(
            super::localized_decimal(&ranges.distance.min(10.).to_string()),
            window,
            cx,
        );

        self.machine_controls.extrusion.editor = Some(Editor {
            printer: id,
            session,
            epoch,
            nozzle,
            title,
            job,
            distance,
            limits: ranges,
            retract: false,
            submitted: false,
            focus: cx.focus_handle(),
            opener: window.focused(cx),
            error: None,
        });
        cx.notify();
    }

    fn cancel_extrusion(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.machine_controls.extrusion.editor.take()
            && editor.submitted
        {
            self.machine_controls.tasks.remove(&editor.printer);
        }
        cx.notify();
    }

    pub(super) fn sync_extrusion(&mut self) {
        if self
            .machine_controls
            .extrusion
            .editor
            .as_ref()
            .is_some_and(|editor| editor.printer(self).is_err())
            && let Some(editor) = self.machine_controls.extrusion.editor.take()
            && editor.submitted
        {
            self.machine_controls.tasks.remove(&editor.printer);
        }
    }

    pub(super) fn finish_extrusion(
        &mut self,
        id: LocalPrinterId,
        session: u64,
        epoch: u64,
        result: ControlResult,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.machine_controls.extrusion.editor
            && editor.printer == id
            && editor.session == session
            && editor.epoch == epoch
            && editor.submitted
        {
            if result == ControlResult::Accepted {
                self.machine_controls.extrusion.editor = None;
            } else {
                editor.submitted = false;
                editor.error = Some(match result {
                    ControlResult::Rejected(error) => super::machine_result_message(error),
                    _ => t!("printers.controls.uncertain").into_owned(),
                });
            }
            cx.notify();
        }
    }

    fn submit_extrusion(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.machine_controls.extrusion.editor.clone() else {
            return;
        };
        if editor.submitted {
            return;
        }
        let result = editor.printer(self).and_then(|printer| {
            let action = editor.action(printer, cx)?;
            printer
                .operation
                .machine_action_for_job(editor.epoch, Some(editor.job.clone()), action.clone())
                .map(|receiver| (action, receiver))
        });
        match result {
            Ok((action, receiver)) => {
                if let Some(current) = &mut self.machine_controls.extrusion.editor {
                    current.submitted = true;
                    current.error = None;
                }
                self.track_machine_request(
                    editor.printer,
                    editor.session,
                    editor.epoch,
                    action,
                    receiver,
                    cx,
                );
            }
            Err(error) => {
                if let Some(current) = &mut self.machine_controls.extrusion.editor {
                    current.error = Some(super::machine_result_message(error));
                }
            }
        }
        cx.notify();
    }

    pub(super) fn render_extrusion(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = &self.machine_controls.extrusion;
        let Some(display) = dialog::present(
            "extrusion-presence",
            state.editor.clone(),
            state.editor.as_ref().and_then(|e| e.opener.clone()),
            window,
            cx,
        ) else {
            self.machine_controls.extrusion.rendered = false;
            return None;
        };
        self.machine_controls.extrusion.rendered = true;
        let editor = display.value;
        let invalid = editor
            .printer(self)
            .and_then(|printer| editor.action(printer, cx))
            .err();
        let disabled = display.closing || editor.submitted;
        let error = editor
            .error
            .clone()
            .or_else(|| invalid.map(super::machine_result_message));
        let direction = v_flex()
            .gap_2()
            .child(
                div()
                    .font_medium()
                    .child(t!("printers.extrusion.direction")),
            )
            .child(
                h_flex().gap_1().children(
                    [
                        (false, "extrusion-forward", "printers.extrusion.extrude"),
                        (true, "extrusion-retract", "printers.extrusion.retract"),
                    ]
                    .into_iter()
                    .map(|(retract, id, key)| {
                        Button::new(id)
                            .outline()
                            .flex_1()
                            .label(t!(key).into_owned())
                            .toggled(editor.retract == retract)
                            .disabled(disabled)
                            .when(editor.retract == retract, |button| {
                                button.border_color(cx.theme().muted_foreground)
                            })
                            .on_click(cx.listener(move |app, _, _, cx| {
                                if let Some(editor) = &mut app.machine_controls.extrusion.editor {
                                    editor.retract = retract;
                                }
                                cx.notify();
                            }))
                    }),
                ),
            );
        let fields = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(field(
                &editor.distance,
                "machine-extrusion-length",
                t!("printers.extrusion.distance").into_owned(),
                "mm",
                disabled,
                cx,
            ))
            .child(direction);
        if !display.closing && !editor.focus.contains_focused(window, cx) {
            editor.focus.focus(window, cx);
        }
        Some(
            backdrop("machine-editor-backdrop", cx.theme().overlay, display.alpha)
                .on_key_down(cx.listener(|app, event: &gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        app.cancel_extrusion(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(dialog::animated(
                    "extrusion-zoom",
                    dialog::surface("machine-editor-dialog", editor.title.clone(), cx)
                        .max_h(window.viewport_size().height - px(48.))
                        .track_focus(&editor.focus)
                        .child(dialog::header().child(dialog::title().child(editor.title.clone())))
                        .child(
                            h_flex()
                                .id("extrusion-body")
                                .min_h_0()
                                .overflow_y_scroll()
                                .items_stretch()
                                .gap_4()
                                .child(fields),
                        )
                        .when_some(error, |dialog, error| {
                            dialog.child(
                                div()
                                    .id("machine-editor-error")
                                    .test_support()
                                    .text_color(cx.theme().danger)
                                    .child(error),
                            )
                        })
                        .child(
                            dialog::footer(window, cx)
                                .child(
                                    Button::new("machine-editor-cancel")
                                        .outline()
                                        .label(t!("common.actions.cancel").into_owned())
                                        .disabled(display.closing)
                                        .on_click(
                                            cx.listener(|app, _, _, cx| app.cancel_extrusion(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("machine-editor-submit")
                                        .label(t!("printers.controls.extrude_apply").into_owned())
                                        .disabled(disabled || invalid.is_some())
                                        .on_click(
                                            cx.listener(|app, _, _, cx| app.submit_extrusion(cx)),
                                        ),
                                ),
                        ),
                    display.alpha,
                    display.closing,
                ))
                .focus_trap("extrusion-focus-trap", &editor.focus)
                .into_any_element(),
        )
    }
}
