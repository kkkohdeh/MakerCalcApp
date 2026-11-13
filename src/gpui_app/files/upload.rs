use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::base::{StyledExt as _, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    AnyElement, Context, DragMoveEvent, ExternalPaths, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::super::components::{alert_dialog::view_presence, folder_float::FolderFloat};
use super::super::feedback::ToastKind;
use super::super::{DetailTab, MakerCalcGpuiApp};
use crate::i18n::t;
use crate::shared::workspace::{UploadValidationError, validate_upload};
use crate::state::{LanConnection, LocalPrinterId};

#[derive(Clone, Copy, PartialEq)]
struct UploadTarget {
    printer: LocalPrinterId,
    session: u64,
    epoch: u64,
}

fn offered_gcode(paths: &[PathBuf]) -> bool {
    paths.len() == 1
        && paths[0]
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gcode") || ext.eq_ignore_ascii_case("g"))
}

fn prepare(source: PathBuf) -> Result<(PathBuf, String, u64), UploadValidationError> {
    let destination = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(UploadValidationError::InvalidName)?
        .to_owned();
    let bytes = validate_upload(&source, &destination)?;
    Ok((source, destination, bytes))
}

impl MakerCalcGpuiApp {
    fn upload_target(&self, id: LocalPrinterId) -> Option<UploadTarget> {
        let printer = self.printers.get(&id)?;
        (self.selected == Some(id)
            && self.detail_tab == DetailTab::Files
            && printer.state.connection == LanConnection::Connected
            && printer.state.telemetry_ready
            && !printer.operation_state.is_pending())
        .then_some(UploadTarget {
            printer: id,
            session: printer.session_generation,
            epoch: printer.state.connection_epoch,
        })
    }

    fn upload_input_available(&self, id: LocalPrinterId) -> bool {
        !self.file_picker_open && self.upload_target(id).is_some() && self.upload_dialogs_clear()
    }

    fn upload_dialogs_clear(&self) -> bool {
        !self.settings_open
            && self.printer_form.is_none()
            && self.pending_upload.is_none()
            && self.pending_delete.is_none()
            && self.pending_print.is_none()
            && self.pending_print_control.is_none()
    }

    fn upload_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.mutation_error = Some(message.clone());
        self.push_toast(
            ToastKind::Error,
            t!("files.upload.error.title").into_owned(),
            Some(message),
            cx,
        );
    }

    pub(in crate::gpui_app) fn pick_gcode(&mut self, id: LocalPrinterId, cx: &mut Context<Self>) {
        if !self.upload_input_available(id) {
            return;
        }
        let target = self.upload_target(id).expect("checked upload target");
        self.mutation_error = None;
        self.file_picker_open = true;
        cx.notify();
        cx.spawn(async move |app, cx| {
            let selected = rfd::AsyncFileDialog::new()
                .set_title(t!("files.upload.select_file").as_ref())
                .add_filter("G-code", &["gcode", "g"])
                .pick_file()
                .await
                .map(|file| file.path().to_owned());
            let _ = app.update(cx, |app, cx| {
                if let Some(source) = selected {
                    app.prepare_upload(target, source, cx);
                } else {
                    app.file_picker_open = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn drop_gcode(&mut self, id: LocalPrinterId, paths: &[PathBuf], cx: &mut Context<Self>) {
        if !self.upload_input_available(id) {
            return;
        }
        if paths.len() != 1 {
            self.upload_error(t!("files.upload.error.one_file").into_owned(), cx);
            return;
        }
        let target = self.upload_target(id).expect("checked upload target");
        self.file_picker_open = true;
        self.mutation_error = None;
        self.prepare_upload(target, paths[0].clone(), cx);
    }

    fn prepare_upload(&mut self, target: UploadTarget, source: PathBuf, cx: &mut Context<Self>) {
        // Run one bounded metadata check off the UI thread for both entry points.
        let operation = cx
            .background_executor()
            .spawn(async move { prepare(source) });
        cx.notify();
        cx.spawn(async move |app, cx| {
            let result = operation.await;
            let _ = app.update(cx, |app, cx| {
                app.file_picker_open = false;
                if app.upload_target(target.printer) != Some(target) || !app.upload_dialogs_clear()
                {
                    app.upload_error(t!("files.upload.error.unavailable").into_owned(), cx);
                } else {
                    match result {
                        Ok((source, destination, bytes)) => {
                            app.pending_upload = Some(super::super::confirmations::PendingUpload {
                                printer_id: target.printer,
                                session_generation: target.session,
                                connection_epoch: target.epoch,
                                source,
                                destination,
                                bytes,
                            });
                        }
                        Err(error) => {
                            let key = match error {
                                UploadValidationError::InvalidName => {
                                    "files.upload.error.invalid_name"
                                }
                                UploadValidationError::Unreadable => {
                                    "files.upload.error.unreadable"
                                }
                                UploadValidationError::InvalidSize => {
                                    "files.upload.error.invalid_size"
                                }
                            };
                            app.upload_error(t!(key).into_owned(), cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[derive(Default)]
struct DropState {
    offer: Option<(bool, Instant)>,
}

pub(super) fn drop_area(
    printer: LocalPrinterId,
    printer_name: &str,
    enabled: bool,
    content: AnyElement,
    window: &mut Window,
    cx: &mut Context<MakerCalcGpuiApp>,
) -> AnyElement {
    let state = window.use_keyed_state(("files-drop-state", printer.0), cx, |_, _| {
        DropState::default()
    });
    if (!enabled || !cx.has_active_drag()) && state.read(cx).offer.is_some() {
        state.update(cx, |state, _| state.offer = None);
    }
    let offer = state.read(cx).offer;
    let presence = view_presence(("files-drop-presence", printer.0), offer, window, cx);
    let moving = state.clone();
    let leaving = state.clone();
    let dropping = state.clone();
    let escaping = state.clone();
    div()
        .id("files-drop-area")
        .test_support()
        .relative()
        .size_full()
        .min_h_0()
        .min_w_0()
        .child(content)
        .children(presence.map(|presence| {
            let (valid, started) = presence.value;
            let elapsed = cx
                .background_executor()
                .now()
                .saturating_duration_since(started);
            v_flex()
                .id("files-drop-overlay")
                .test_support()
                .role(gpui_kit::Role::Status)
                .aria_label(if valid {
                    t!("files.upload.drop_title").into_owned()
                } else {
                    t!("files.upload.drop_invalid").into_owned()
                })
                .absolute()
                .inset_0()
                .size_full()
                .overflow_hidden()
                .items_center()
                .justify_center()
                .gap_2()
                .p_3()
                .bg(cx.theme().background.opacity(0.96))
                .opacity(presence.alpha)
                .child(FolderFloat::new(elapsed, !presence.closing, valid))
                .child(
                    div()
                        .id("files-drop-title")
                        .test_support()
                        .font_semibold()
                        .text_sm()
                        .child(if valid {
                            t!("files.upload.drop_title")
                        } else {
                            t!("files.upload.drop_invalid")
                        }),
                )
                .child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            t!("files.upload.drop_description", printer = printer_name)
                                .into_owned(),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("files.upload.drop_hint")),
                )
        }))
        .on_drag_move(
            cx.listener(move |app, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                let active = enabled
                    && app.upload_input_available(printer)
                    && event.bounds.contains(&event.event.position);
                let next = active.then(|| offered_gcode(event.drag(cx).paths()));
                if moving.read(cx).offer.map(|(valid, _)| valid) != next {
                    let now = cx.background_executor().now();
                    moving.update(cx, |state, _| state.offer = next.map(|valid| (valid, now)));
                    cx.notify();
                }
            }),
        )
        .on_file_drop_exit(move |_, _, cx| {
            leaving.update(cx, |state, cx| {
                state.offer = None;
                cx.notify();
            });
        })
        .on_key_down(move |event, window, cx| {
            if event.keystroke.key == "escape" && escaping.read(cx).offer.is_some() {
                cx.stop_active_drag(window);
                escaping.update(cx, |state, cx| {
                    state.offer = None;
                    cx.notify();
                });
            }
        })
        .on_drop(cx.listener(move |app, paths: &ExternalPaths, _, cx| {
            dropping.update(cx, |state, _| state.offer = None);
            app.drop_gcode(printer, paths.paths(), cx);
        }))
        .into_any_element()
}
