use super::components::button::Button;
use super::components::checkbox::Checkbox;
use crate::i18n::t;
use gpui_kit::base::{FocusTrapElement as _, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, KeyDownEvent,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, relative,
};

use crate::config::Protocol;
use crate::edge::{EdgeAction, EdgeCommand};
use crate::shared::printer::PrintStatus;
use crate::shared::workspace::{
    PrintControlContext, PrintOptionKind, PrinterAction, PrinterActionKind, ResourceId,
    StartPrintOptions, Support,
};
use crate::state::LocalPrinterId;

use super::MakerCalcGpuiApp;
use super::components::alert_dialog::{
    AlertDialog, DialogFooter, backdrop, cancel_button, initial_cancel_focus, view_presence,
};
use super::feedback::{ToastKind, log_config_operation};
use super::format::format_bytes;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PrintControl {
    Pause,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gpui_app) struct PendingPrint {
    pub(in crate::gpui_app) printer_id: LocalPrinterId,
    pub(in crate::gpui_app) session_generation: u64,
    pub(in crate::gpui_app) connection_epoch: u64,
    pub(in crate::gpui_app) file_id: ResourceId,
    pub(in crate::gpui_app) file_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gpui_app) struct PendingUpload {
    pub(in crate::gpui_app) printer_id: LocalPrinterId,
    pub(in crate::gpui_app) session_generation: u64,
    pub(in crate::gpui_app) connection_epoch: u64,
    pub(in crate::gpui_app) source: std::path::PathBuf,
    pub(in crate::gpui_app) destination: String,
    pub(in crate::gpui_app) bytes: u64,
}

pub(super) struct PendingPrintControl {
    control: PrintControl,
    printer_id: LocalPrinterId,
    session_generation: u64,
    connection_epoch: u64,
    job_generation: u64,
    file_name: String,
}

impl MakerCalcGpuiApp {
    pub(super) fn request_cancel_print(
        &mut self,
        printer_id: LocalPrinterId,
        job_generation: u64,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        self.request_print_control(
            printer_id,
            job_generation,
            file_name,
            PrintControl::Cancel,
            cx,
        );
    }

    pub(super) fn request_pause_print(
        &mut self,
        printer_id: LocalPrinterId,
        job_generation: u64,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        self.request_print_control(
            printer_id,
            job_generation,
            file_name,
            PrintControl::Pause,
            cx,
        );
    }

    fn request_print_control(
        &mut self,
        printer_id: LocalPrinterId,
        job_generation: u64,
        file_name: String,
        control: PrintControl,
        cx: &mut Context<Self>,
    ) {
        let Some(printer) = self.printers.get(&printer_id) else {
            self.push_toast(
                ToastKind::Error,
                t!("printers.errors.not_found").into_owned(),
                None,
                cx,
            );
            return;
        };
        self.pending_print_control = Some(PendingPrintControl {
            control,
            printer_id,
            session_generation: printer.session_generation,
            connection_epoch: printer.state.connection_epoch,
            job_generation,
            file_name,
        });
        cx.notify();
    }

    pub(super) fn dismiss_print_control(&mut self, cx: &mut Context<Self>) {
        self.pending_print_control = None;
        cx.notify();
    }

    pub(super) fn confirm_print_control(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.pending_print_control.take() else {
            return;
        };
        let Some(printer) = self.printers.get(&target.printer_id) else {
            self.push_toast(
                ToastKind::Error,
                t!("printers.errors.not_found").into_owned(),
                Some(t!("printers.errors.unavailable").into_owned()),
                cx,
            );
            cx.notify();
            return;
        };
        let pause = target.control == PrintControl::Pause;
        let kind = if pause {
            PrinterActionKind::PausePrint
        } else {
            PrinterActionKind::CancelPrint
        };
        let context = PrintControlContext::new(target.connection_epoch, target.job_generation);
        let is_same_execution = printer.session_generation == target.session_generation
            && printer.state.connection_epoch == target.connection_epoch
            && printer.state.job_generation == target.job_generation
            && printer.state.connection == crate::state::LanConnection::Connected
            && printer.state.telemetry_ready
            && !printer.operation_state.is_pending()
            && printer.state.controls.allows_print(kind)
            && if pause {
                printer.state.telemetry.print_state == PrintStatus::Printing
            } else {
                matches!(
                    printer.state.telemetry.print_state,
                    PrintStatus::Printing | PrintStatus::Paused
                )
            };
        if !is_same_execution {
            let (title, reason) = if pause {
                (
                    t!("printers.confirmations.pause.discarded"),
                    t!("printers.confirmations.pause.job_changed"),
                )
            } else {
                (
                    t!("printers.confirmations.cancel.discarded"),
                    t!("printers.confirmations.cancel.job_changed"),
                )
            };
            self.push_toast(
                ToastKind::Warning,
                title.into_owned(),
                Some(reason.into_owned()),
                cx,
            );
            cx.notify();
            return;
        }
        let action = if pause {
            PrinterAction::PausePrint(context)
        } else {
            PrinterAction::CancelPrint(context)
        };
        match printer.actions.try_send(action) {
            Ok(()) => {
                let title = if pause {
                    t!("printers.confirmations.pause.sent")
                } else {
                    t!("printers.confirmations.cancel.sent")
                };
                self.push_toast(
                    ToastKind::Info,
                    title.into_owned(),
                    Some(target.file_name),
                    cx,
                );
            }
            Err(error) => {
                let title = if pause {
                    t!("printers.confirmations.pause.error")
                } else {
                    t!("printers.confirmations.cancel.error")
                };
                let reason = match error {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => t!("printers.errors.busy"),
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        t!("printers.errors.channel_closed")
                    }
                };
                self.push_toast(
                    ToastKind::Error,
                    title.into_owned(),
                    Some(reason.into_owned()),
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn confirm_printer_ready(
        &mut self,
        printer_id: LocalPrinterId,
        session_generation: u64,
        connection_epoch: u64,
        title_key: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let error = match self.printers.get(&printer_id) {
            None => Some((
                ToastKind::Error,
                t!("printers.errors.not_found").into_owned(),
                t!("printers.errors.unavailable").into_owned(),
            )),
            Some(printer)
                if printer.session_generation != session_generation
                    || printer.state.connection_epoch != connection_epoch
                    || printer.state.connection != crate::state::LanConnection::Connected =>
            {
                Some((
                    ToastKind::Warning,
                    t!(title_key).into_owned(),
                    t!("printers.actions.state_changed").into_owned(),
                ))
            }
            Some(printer) if printer.operation_state.is_pending() => Some((
                ToastKind::Warning,
                t!(title_key).into_owned(),
                t!("printers.actions.busy_control").into_owned(),
            )),
            Some(_) => None,
        };
        if let Some((kind, title, detail)) = error {
            self.push_toast(kind, title, Some(detail), cx);
            false
        } else {
            true
        }
    }

    pub(super) fn request_start_print(
        &mut self,
        printer_id: LocalPrinterId,
        file_id: ResourceId,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        let (session_generation, connection_epoch) = self
            .printers
            .get(&printer_id)
            .map_or((0, 0), |p| (p.session_generation, p.state.connection_epoch));
        self.pending_print = Some(PendingPrint {
            printer_id,
            session_generation,
            connection_epoch,
            file_id,
            file_name,
        });
        self.print_bed_leveling = true;
        self.print_material_station = false;
        self.print_first_layer_inspection = false;
        self.print_flow_calibration = false;
        self.print_timelapse = false;
        self.print_safety_confirmed = false;
        self.print_countdown = None;
        self.print_countdown_task = None;
        cx.notify();
    }

    pub(super) fn cancel_start_print(&mut self, cx: &mut Context<Self>) {
        self.pending_print = None;
        self.print_safety_confirmed = false;
        self.print_countdown = None;
        self.print_countdown_task = None;
        cx.notify();
    }

    pub(super) fn set_print_safety_confirmed(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        self.print_safety_confirmed = confirmed;
        if confirmed {
            self.print_countdown = Some(3);
            self.print_countdown_task = Some(cx.spawn(async move |app, cx| {
                for i in (1..=3).rev() {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let res = app.update(cx, |app, cx| {
                        if app.print_safety_confirmed
                            && let Some(cd) = app.print_countdown.as_mut()
                        {
                            *cd = i - 1;
                            cx.notify();
                        }
                    });
                    if res.is_err() {
                        break;
                    }
                }
            }));
        } else {
            self.print_countdown = None;
            self.print_countdown_task = None;
        }
        cx.notify();
    }

    pub(super) fn confirm_start_print(&mut self, cx: &mut Context<Self>) {
        if !self.print_safety_confirmed || self.print_countdown.unwrap_or(0) > 0 {
            return;
        }
        let Some(pending) = self.pending_print.take() else {
            return;
        };
        if self.confirm_printer_ready(
            pending.printer_id,
            pending.session_generation,
            pending.connection_epoch,
            "printers.confirmations.start.error",
            cx,
        ) && let Some(printer) = self.printers.get(&pending.printer_id)
        {
            let supports_ifs =
                printer.detail_state.capabilities.material_station == Support::Available;
            let has_option = |kind| printer.detail_state.machine.print_options.contains(&kind);
            let send_result = printer.actions.try_send(PrinterAction::StartPrint {
                file_id: pending.file_id,
                options: StartPrintOptions {
                    bed_leveling: self.print_bed_leveling
                        && has_option(PrintOptionKind::BedLeveling),
                    material_station: supports_ifs && self.print_material_station,
                    material_assignments: Vec::new(),
                    first_layer_inspection: self.print_first_layer_inspection
                        && has_option(PrintOptionKind::FirstLayerInspection),
                    flow_calibration: self.print_flow_calibration
                        && has_option(PrintOptionKind::FlowCalibration),
                    timelapse: self.print_timelapse && has_option(PrintOptionKind::Timelapse),
                },
            });
            match send_result {
                Ok(()) => {
                    self.push_toast(
                        ToastKind::Info,
                        t!("printers.confirmations.start.sent").into_owned(),
                        Some(pending.file_name),
                        cx,
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    self.push_toast(
                        ToastKind::Error,
                        t!("printers.confirmations.start.error").into_owned(),
                        Some(t!("printers.errors.busy").into_owned()),
                        cx,
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    self.push_toast(
                        ToastKind::Error,
                        t!("printers.confirmations.start.error").into_owned(),
                        Some(t!("printers.errors.channel_closed").into_owned()),
                        cx,
                    );
                }
            }
        }
        self.print_safety_confirmed = false;
        self.print_countdown = None;
        self.print_countdown_task = None;
        cx.notify();
    }

    pub(super) fn request_delete_printer(
        &mut self,
        id: LocalPrinterId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_delete = Some(id);
        self.mutation_error = None;
        cx.notify();
    }

    pub(super) fn cancel_delete_printer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mutation_pending {
            self.pending_delete = None;
            self.printer_form_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn confirm_delete_printer(&mut self, cx: &mut Context<Self>) {
        if self.mutation_pending {
            return;
        }
        let Some(id) = self.pending_delete else {
            return;
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .edge_actions
            .try_send(EdgeCommand {
                action: EdgeAction::RemovePrinter(id),
                reply,
            })
            .is_err()
        {
            self.mutation_error = Some(t!("common.errors.send").into_owned());
            cx.notify();
            return;
        }
        self.mutation_pending = true;
        cx.spawn(async move |app, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("common.errors.controller_closed").into_owned()));
            let _ = app.update(cx, |app, cx| {
                app.mutation_pending = false;
                log_config_operation("remove_printer", Some(id), &result, app);
                match result {
                    Ok(()) => {
                        app.pending_delete = None;
                        app.clear_finder();
                        app.printer_form = None;
                        app.selected = None;
                        app.push_toast(
                            ToastKind::Info,
                            t!("printers.confirmations.delete.done").into_owned(),
                            None,
                            cx,
                        );
                    }
                    Err(error) => {
                        app.push_toast(
                            ToastKind::Error,
                            t!("printers.confirmations.delete.error").into_owned(),
                            Some(error.clone()),
                            cx,
                        );
                        app.mutation_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cancel_upload(&mut self, cx: &mut Context<Self>) {
        self.pending_upload = None;
        cx.notify();
    }

    pub(super) fn confirm_upload(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_upload.take() else {
            return;
        };
        if self.confirm_printer_ready(
            pending.printer_id,
            pending.session_generation,
            pending.connection_epoch,
            "printers.confirmations.upload.error",
            cx,
        ) && let Some(printer) = self.printers.get(&pending.printer_id)
        {
            let send_result = printer.actions.try_send(PrinterAction::UploadGcode {
                source: pending.source,
                destination: pending.destination.clone(),
            });
            match send_result {
                Ok(()) => {
                    self.push_toast(
                        ToastKind::Info,
                        t!("printers.confirmations.upload.sending").into_owned(),
                        Some(pending.destination),
                        cx,
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    self.push_toast(
                        ToastKind::Error,
                        t!("printers.confirmations.upload.error").into_owned(),
                        Some(t!("printers.errors.queue_full").into_owned()),
                        cx,
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    self.push_toast(
                        ToastKind::Error,
                        t!("printers.confirmations.upload.error").into_owned(),
                        Some(t!("printers.errors.channel_closed").into_owned()),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }

    pub(super) fn render_delete_confirmation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let incoming = self.pending_delete.map(|id| {
            self.printers.get(&id).map_or_else(
                || t!("printers.confirmations.this_printer").into_owned(),
                |printer| printer.name.clone(),
            )
        });
        let presence = view_presence("delete-printer", incoming, window, cx)?;
        let name = presence.value;
        let cancel_focus = initial_cancel_focus(
            "delete-printer",
            &self.printer_delete_focus,
            presence.closing,
            window,
            cx,
        );
        Some(
            backdrop("delete-printer-modal", cx.theme().overlay, presence.alpha)
                .on_key_down(cx.listener(|app, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        app.cancel_delete_printer(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    AlertDialog::new("delete-printer-dialog", presence.alpha)
                        .title(t!("printers.form.delete"))
                        .description(t!("printers.confirmations.delete.description", name = name))
                        .when_some(self.mutation_error.as_ref(), |dialog, error| {
                            dialog.child(super::feedback::error_alert(
                                "delete-printer-error",
                                error,
                                cx,
                            ))
                        })
                        .footer(
                            DialogFooter::new()
                                .child(
                                    cancel_button(
                                        "cancel-delete",
                                        &cancel_focus,
                                        self.mutation_pending || presence.closing,
                                        t!("common.actions.cancel").into_owned(),
                                        cx,
                                    )
                                    .on_click(cx.listener(
                                        |app, _, window, cx| {
                                            app.cancel_delete_printer(window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    Button::new("confirm-delete")
                                        .danger()
                                        .label(if self.mutation_pending {
                                            t!("printers.confirmations.delete.pending").into_owned()
                                        } else {
                                            t!("common.actions.delete").into_owned()
                                        })
                                        .disabled(self.mutation_pending || presence.closing)
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.confirm_delete_printer(cx);
                                        })),
                                ),
                        ),
                )
                .focus_trap("delete-printer-focus-trap", &self.printer_delete_focus)
                .into_any_element(),
        )
    }

    pub(super) fn render_upload_confirmation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let incoming = self
            .pending_upload
            .as_ref()
            .map(|pending| (pending.destination.clone(), pending.bytes));
        let presence = view_presence("upload", incoming, window, cx)?;
        let focus = window
            .use_keyed_state("upload-dialog-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let cancel_focus = initial_cancel_focus("upload", &focus, presence.closing, window, cx);
        let (destination, bytes) = presence.value;
        Some(
            backdrop(
                "upload-confirmation-modal",
                cx.theme().overlay,
                presence.alpha,
            )
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    app.cancel_upload(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                AlertDialog::new("upload-dialog", presence.alpha)
                    .title(t!("printers.confirmations.upload.title"))
                    .description(format!("{destination} ({})", format_bytes(Some(bytes))))
                    .footer(
                        DialogFooter::new()
                            .child(
                                cancel_button(
                                    "cancel-upload",
                                    &cancel_focus,
                                    presence.closing,
                                    t!("common.actions.cancel").into_owned(),
                                    cx,
                                )
                                .on_click(cx.listener(|app, _, _, cx| app.cancel_upload(cx))),
                            )
                            .child(
                                Button::new("confirm-upload")
                                    .primary()
                                    .disabled(presence.closing)
                                    .label(t!("printers.confirmations.upload.action").into_owned())
                                    .on_click(cx.listener(|app, _, _, cx| app.confirm_upload(cx))),
                            ),
                    ),
            )
            .focus_trap("upload-dialog-trap", &focus)
            .into_any_element(),
        )
    }

    pub(super) fn render_start_print_confirmation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let presence = view_presence("start-print", self.pending_print.clone(), window, cx)?;
        let focus = window
            .use_keyed_state("start-print-dialog-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let cancel_focus =
            initial_cancel_focus("start-print", &focus, presence.closing, window, cx);
        let pending = presence.value;
        let printer = self.printers.get(&pending.printer_id);
        let print_options = printer
            .map(|p| p.detail_state.machine.print_options.as_slice())
            .unwrap_or_default();
        let supports_options = !print_options.is_empty();
        let supports_ifs = printer
            .is_some_and(|p| p.detail_state.capabilities.material_station == Support::Available);
        let printer_name = printer.map_or_else(
            || t!("printers.confirmations.this_printer").into_owned(),
            |p| p.name.clone(),
        );
        let printer_label =
            t!("printers.confirmations.start.printer", name = printer_name).into_owned();
        let file_label = t!(
            "printers.confirmations.start.file",
            name = pending.file_name
        )
        .into_owned();
        let countdown = self.print_countdown.unwrap_or(0);
        let is_counting = self.print_safety_confirmed && countdown > 0;
        let is_ready = self.print_safety_confirmed && countdown == 0;
        let status_label = if is_counting {
            t!("printers.confirmations.start.wait", seconds = countdown).into_owned()
        } else {
            t!("printers.confirmations.start.ready").into_owned()
        };
        Some(backdrop("start-print-confirmation-modal", cx.theme().overlay, presence.alpha)
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    app.cancel_start_print(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                AlertDialog::new("start-print-dialog", presence.alpha)
                    .max_h(relative(0.8))
                    .title(t!("printers.confirmations.start.title"))
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .py_2()
                            .child(
                                v_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_1()
                                    .p_3()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().group_box)
                                    .child(
                                        div()
                                            .id("start-print-printer")
                                            .test_support()
                                            .w_full()
                                            .text_sm()
                                            .font_semibold()
                                            .whitespace_normal()
                                            .aria_label(printer_label.clone())
                                            .child(printer_label),
                                    )
                                    .child(
                                        div()
                                            .id("start-print-file")
                                            .test_support()
                                            .w_full()
                                            .text_sm()
                                            .whitespace_normal()
                                            .aria_label(file_label.clone())
                                            .child(file_label),
                                    ),
                            )
                            .when(supports_options || supports_ifs, |col| {
                                col.child(
                                    v_flex()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_semibold()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(t!("printers.confirmations.start.options")),
                                        )
                                        .when(print_options.contains(&crate::shared::workspace::PrintOptionKind::BedLeveling), |col| col.child(
                                            Checkbox::new("print-bed-leveling")
                                                .disabled(presence.closing)
                                                .label(
                                                    t!("printers.confirmations.start.level_bed")
                                                        .into_owned(),
                                                )
                                                .checked(self.print_bed_leveling)
                                                .on_change(cx.listener(|app, value, _, cx| {
                                                    app.print_bed_leveling = *value;
                                                    cx.notify();
                                                })),
                                        ))
                                        .when(print_options.contains(&crate::shared::workspace::PrintOptionKind::FirstLayerInspection), |col| col.child(
                                            Checkbox::new("print-first-layer-inspection")
                                                .disabled(presence.closing)
                                                .label(t!("printers.print_options.first_layer_inspection").into_owned())
                                                .checked(self.print_first_layer_inspection)
                                                .on_change(cx.listener(|app, value, _, cx| { app.print_first_layer_inspection = *value; cx.notify(); }))
                                        ))
                                        .when(print_options.contains(&crate::shared::workspace::PrintOptionKind::FlowCalibration), |col| col.child(
                                            Checkbox::new("print-flow-calibration")
                                                .disabled(presence.closing)
                                                .label(t!("printers.print_options.flow_calibration").into_owned())
                                                .checked(self.print_flow_calibration)
                                                .on_change(cx.listener(|app, value, _, cx| { app.print_flow_calibration = *value; cx.notify(); }))
                                        ))
                                        .when(print_options.contains(&crate::shared::workspace::PrintOptionKind::Timelapse), |col| col.child(
                                            Checkbox::new("print-timelapse")
                                                .disabled(presence.closing)
                                                .label(t!("printers.print_options.timelapse").into_owned())
                                                .checked(self.print_timelapse)
                                                .on_change(cx.listener(|app, value, _, cx| { app.print_timelapse = *value; cx.notify(); }))
                                        ))
                                        .when(supports_ifs, |options| {
                                            options
                                                .child(
                                                    Checkbox::new("print-material-station")
                                                        .disabled(presence.closing)
                                                        .label(
                                                            t!("printers.confirmations.start.material_station")
                                                                .into_owned(),
                                                        )
                                                        .checked(self.print_material_station)
                                                        .on_change(cx.listener(|app, value, _, cx| {
                                                            app.print_material_station = *value;
                                                            cx.notify();
                                                        })),
                                                )
                                                .child(
                                                    div()
                                                        .id("start-print-material-help")
                                                        .test_support()
                                                        .text_xs()
                                                        .whitespace_normal()
                                                        .text_color(cx.theme().warning)
                                                        .child(t!("printers.confirmations.start.material_help")),
                                                )
                                        }),
                                )
                            })
                            .when(printer.is_some_and(|p| p.protocol == Protocol::Moonraker), |col| {
                                col.child(
                                    div()
                                        .id("start-print-gcode-options")
                                        .test_support()
                                        .text_sm()
                                        .whitespace_normal()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("printers.confirmations.start.gcode_options")),
                                )
                            })
                            .child(
                                v_flex()
                                    .gap_2()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_semibold()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!("printers.confirmations.start.safety_title")),
                                    )
                                    .child(
                                        Checkbox::new("print-safety-confirmation")
                                            .disabled(presence.closing)
                                            .label(t!("printers.confirmations.start.safety").into_owned())
                                            .checked(self.print_safety_confirmed)
                                            .on_change(cx.listener(|app, value, _, cx| {
                                                app.set_print_safety_confirmed(*value, cx);
                                            })),
                                    )
                                    .when(self.print_safety_confirmed && self.print_countdown.is_some(), |col| {
                                        col.child(
                                            div()
                                                .id("start-print-safety-status")
                                                .test_support()
                                                .px_3()
                                                .py_2()
                                                .rounded(cx.theme().radius)
                                                .bg(cx.theme().group_box)
                                                .border_1()
                                                .border_color(if is_counting { cx.theme().yellow } else { cx.theme().green })
                                                .text_xs()
                                                .font_medium()
                                                .whitespace_normal()
                                                .text_color(if is_counting { cx.theme().yellow } else { cx.theme().green })
                                                .aria_label(status_label.clone())
                                                .child(status_label),
                                        )
                                    }),
                            ),
                    )
                    .footer(
                        DialogFooter::new()
                            .child(
                                cancel_button("cancel-start-print", &cancel_focus, presence.closing,
                                    t!("common.actions.cancel").into_owned(), cx)
                                    .on_click(
                                        cx.listener(|app, _, _, cx| app.cancel_start_print(cx)),
                                    ),
                            )
                            .child({
                                let label = if is_counting {
                                    t!(
                                        "printers.confirmations.start.countdown",
                                        seconds = countdown
                                    )
                                    .into_owned()
                                } else {
                                    t!("printers.confirmations.start.title").into_owned()
                                };
                                Button::new("confirm-start-print")
                                    .primary()
                                    .label(label)
                                    .disabled(!is_ready || presence.closing)
                                    .on_click(
                                        cx.listener(|app, _, _, cx| app.confirm_start_print(cx)),
                                    )
                            }),
                    ),
            )
            .focus_trap("start-print-dialog-trap", &focus)
            .into_any_element())
    }

    pub(super) fn render_print_control_confirmation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let incoming = self.pending_print_control.as_ref().map(|target| {
            let printer_name = self.printers.get(&target.printer_id).map_or_else(
                || t!("printers.confirmations.this_printer").into_owned(),
                |printer| printer.name.clone(),
            );
            (target.control, target.file_name.clone(), printer_name)
        });
        let presence = view_presence("print-control", incoming, window, cx)?;
        let focus = window
            .use_keyed_state("print-control-dialog-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let cancel_focus =
            initial_cancel_focus("print-control", &focus, presence.closing, window, cx);
        let (control, file_name, printer_name) = presence.value;
        let pause = control == PrintControl::Pause;
        let title = if pause {
            t!("printers.confirmations.pause.title")
        } else {
            t!("printers.confirmations.cancel.title")
        };
        let description = if pause {
            t!(
                "printers.confirmations.pause.description",
                name = file_name,
                printer = printer_name
            )
        } else {
            t!(
                "printers.confirmations.cancel.description",
                name = file_name,
                printer = printer_name
            )
        };
        Some(
            backdrop(
                "print-control-confirmation-modal",
                cx.theme().overlay,
                presence.alpha,
            )
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    app.dismiss_print_control(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                AlertDialog::new("print-control-dialog", presence.alpha)
                    .title(title.clone())
                    .description(description)
                    .footer(
                        DialogFooter::new()
                            .child(
                                cancel_button(
                                    "dismiss-print-control",
                                    &cancel_focus,
                                    presence.closing,
                                    t!("common.actions.back").into_owned(),
                                    cx,
                                )
                                .on_click(
                                    cx.listener(|app, _, _, cx| app.dismiss_print_control(cx)),
                                ),
                            )
                            .child(
                                Button::new("confirm-print-control")
                                    .when(pause, |button| button.primary())
                                    .when(!pause, |button| button.danger())
                                    .disabled(presence.closing)
                                    .label(title.into_owned())
                                    .on_click(
                                        cx.listener(|app, _, _, cx| app.confirm_print_control(cx)),
                                    ),
                            ),
                    ),
            )
            .focus_trap("print-control-dialog-trap", &focus)
            .into_any_element(),
        )
    }
}
