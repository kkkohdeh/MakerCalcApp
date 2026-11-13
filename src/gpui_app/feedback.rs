use super::components::button::Button;
use gpui_kit::TestSupportExt as _;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::config::Protocol;
use crate::diagnostics::{self, LogArea, LogEvent, LogLevel};
use crate::shared::workspace::{OperationState, PrinterActionKind};
use crate::state::{LanConnection, LocalPrinterId};

use super::components::alert::Alert;
use super::{MakerCalcGpuiApp, PrinterEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub(super) const MAX_TOASTS: usize = 5;

pub(super) fn empty_state(
    id: &'static str,
    icon: IconName,
    message: &str,
    _cx: &gpui_kit::App,
) -> AnyElement {
    super::components::empty::Empty::new(id, message.to_owned())
        .icon(icon)
        .into_any_element()
}

pub(super) fn status_badge(
    label: impl Into<gpui_kit::SharedString>,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    super::components::badge::badge()
        .border_color(cx.theme().border)
        .bg(cx.theme().transparent)
        .child(label.into())
}

pub(super) fn error_alert(
    id: impl Into<gpui_kit::ElementId>,
    message: &str,
    cx: &gpui_kit::App,
) -> AnyElement {
    Alert::new(
        id,
        Icon::new(gpui_kit::assets::IconName::CircleAlert),
        message,
        cx.theme().red,
    )
    .into_any_element()
}

pub(super) fn warning_alert(
    id: impl Into<gpui_kit::ElementId>,
    message: &str,
    cx: &gpui_kit::App,
) -> AnyElement {
    Alert::new(
        id,
        Icon::new(IconName::TriangleAlert),
        message,
        cx.theme().warning,
    )
    .into_any_element()
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PrinterAlertKey {
    printer_id: LocalPrinterId,
    category: String,
    message: String,
}

fn next_toast_id() -> u64 {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 63);
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub(super) fn toast_duration(kind: ToastKind) -> Option<std::time::Duration> {
    match kind {
        ToastKind::Error => None, // Errors require dismissal with X.
        ToastKind::Info | ToastKind::Success | ToastKind::Warning => {
            Some(std::time::Duration::from_millis(2500))
        }
    }
}

/// Insert zero-width spaces (\u{200b}) into unbroken text.
/// This lets GPUI wrap long text without overflowing.
pub(crate) fn break_long_words(s: &str, max_chunk: usize) -> String {
    let mut result = String::with_capacity(s.len() + (s.len() / max_chunk.max(1)));
    let mut current_run = 0;
    for ch in s.chars() {
        if ch.is_whitespace() {
            current_run = 0;
            result.push(ch);
        } else {
            if current_run >= max_chunk {
                result.push('\u{200b}');
                current_run = 0;
            }
            result.push(ch);
            current_run += 1;
        }
    }
    result
}

#[derive(Clone, Debug)]
pub(crate) struct Toast {
    pub(crate) id: u64,
    pub(crate) kind: ToastKind,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    alert_key: Option<PrinterAlertKey>,
}

const fn integration_key(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Moonraker => "moonraker",
        Protocol::Flashforge => "flashforge",
        Protocol::Bambulab => "bambulab",
    }
}

const fn action_key(action: PrinterActionKind) -> &'static str {
    match action {
        PrinterActionKind::StartPrint => "start_print",
        PrinterActionKind::PausePrint => "pause_print",
        PrinterActionKind::ResumePrint => "resume_print",
        PrinterActionKind::CancelPrint => "cancel_print",
        PrinterActionKind::SetTemperature => "set_temperature",
        PrinterActionKind::SetLight => "set_light",
        PrinterActionKind::UploadGcode => "upload_gcode",
        PrinterActionKind::MachineControl => "machine_control",
    }
}

pub(super) fn printer_log_context(
    event: LogEvent,
    id: LocalPrinterId,
    printer: &PrinterEntry,
) -> LogEvent {
    event
        .printer(id.0)
        .printer_name(&printer.name)
        .endpoint(format!("{}:{}", printer.host, printer.port))
        .integration(integration_key(printer.protocol))
}

pub(super) fn log_connection_change(printer: &PrinterEntry, previous: LanConnection) {
    let current = printer.state.connection;
    let (level, code, result) = match current {
        LanConnection::Connecting => (LogLevel::Info, "printer.connecting", "connecting"),
        LanConnection::Connected => (
            LogLevel::Info,
            if matches!(
                previous,
                LanConnection::Disconnected | LanConnection::NeedsCredentials
            ) {
                "printer.recovered"
            } else {
                "printer.connected"
            },
            "connected",
        ),
        LanConnection::Disconnected => (LogLevel::Warn, "printer.disconnected", "disconnected"),
        LanConnection::NeedsCredentials => {
            (LogLevel::Warn, "printer.credentials_required", "blocked")
        }
        LanConnection::Incompatible => (LogLevel::Error, "printer.incompatible", "failed"),
    };
    let mut event = LogEvent::new(level, LogArea::Printer, code).result(result);
    if printer.state.last_error.is_some() {
        event = event.detail("Connection error; details available in UI");
    }
    diagnostics::log(printer_log_context(event, printer.state.id, printer));
}

pub(super) fn log_operation_state(printer: &PrinterEntry) {
    let event = match &printer.operation_state {
        OperationState::Idle | OperationState::ReadOnly => return,
        OperationState::Sending(action) => {
            LogEvent::new(LogLevel::Info, LogArea::Action, "action.requested")
                .action(action_key(*action))
                .result("queued")
        }
        OperationState::Accepted(action) => {
            LogEvent::new(LogLevel::Info, LogArea::Action, "action.completed")
                .action(action_key(*action))
                .result("ok")
        }
        OperationState::Rejected { action, .. } => {
            LogEvent::new(LogLevel::Error, LogArea::Action, "action.failed")
                .action(action_key(*action))
                .result("failed")
                .detail("Operation rejected; details available in UI")
        }
        OperationState::Unknown(action) => {
            LogEvent::new(LogLevel::Warn, LogArea::Action, "action.unknown")
                .action(action_key(*action))
                .result("unknown")
        }
    };
    diagnostics::log(printer_log_context(event, printer.state.id, printer));
}

pub(super) fn operation_state_eq(left: &OperationState, right: &OperationState) -> bool {
    match (left, right) {
        (OperationState::Idle, OperationState::Idle)
        | (OperationState::ReadOnly, OperationState::ReadOnly) => true,
        (OperationState::Sending(a), OperationState::Sending(b))
        | (OperationState::Accepted(a), OperationState::Accepted(b))
        | (OperationState::Unknown(a), OperationState::Unknown(b)) => a == b,
        (
            OperationState::Rejected {
                action: left_action,
                message: left_message,
            },
            OperationState::Rejected {
                action: right_action,
                message: right_message,
            },
        ) => left_action == right_action && left_message == right_message,
        _ => false,
    }
}

pub(super) fn log_config_operation(
    action: &'static str,
    id: Option<LocalPrinterId>,
    result: &Result<(), String>,
    app: &MakerCalcGpuiApp,
) {
    let (level, outcome) = if result.is_ok() {
        (LogLevel::Info, "ok")
    } else {
        (LogLevel::Error, "failed")
    };
    let mut event = LogEvent::new(level, LogArea::Config, "config.operation")
        .action(action)
        .result(outcome);
    if let Some(id) = id {
        event = app.printers.get(&id).map_or_else(
            || event.clone().printer(id.0),
            |printer| printer_log_context(event.clone(), id, printer),
        );
    }
    if let Err(error) = result {
        event = event.detail(error);
    }
    diagnostics::log(event);
}

impl MakerCalcGpuiApp {
    pub(super) fn sync_printer_alerts(&mut self, cx: &mut Context<Self>) {
        self.dismissed_alerts.retain(|key| {
            self.printers.get(&key.printer_id).is_some_and(|printer| {
                // Missing readings do not establish that a dismissed episode ended.
                printer
                    .state
                    .telemetry
                    .printer_alerts
                    .as_ref()
                    .is_none_or(|alerts| {
                        alerts.iter().any(|alert| {
                            alert.category == key.category && alert.message == key.message
                        })
                    })
            })
        });
        let mut alerts_to_push = Vec::new();
        for (&printer_id, printer) in &self.printers {
            if let Some(alerts) = &printer.state.telemetry.printer_alerts {
                for alert in alerts {
                    let key = PrinterAlertKey {
                        printer_id,
                        category: alert.category.clone(),
                        message: alert.message.clone(),
                    };
                    if !self.dismissed_alerts.contains(&key)
                        && !self
                            .toasts
                            .iter()
                            .any(|t| t.alert_key.as_ref() == Some(&key))
                    {
                        let kind = match alert.category.as_str() {
                            "error" | "print_error" | "system_error" => ToastKind::Error,
                            _ => ToastKind::Warning,
                        };
                        alerts_to_push.push((key, kind, printer.name.clone()));
                    }
                }
            }
        }
        for (key, kind, title) in alerts_to_push {
            if !self
                .toasts
                .iter()
                .any(|t| t.alert_key.as_ref() == Some(&key))
            {
                self.push_toast_internal(
                    next_toast_id(),
                    kind,
                    title,
                    Some(key.message.clone()),
                    Some(key),
                    cx,
                );
            }
        }
    }

    pub(super) fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self.toasts.iter().position(|t| t.id == id) {
            let toast = self.toasts.remove(index);
            if let Some(key) = toast.alert_key {
                self.dismissed_alerts.insert(key);
            }
        }
        cx.notify();
    }

    pub(super) fn push_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<String>,
        description: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_toast_internal(next_toast_id(), kind, title.into(), description, None, cx);
    }

    pub(super) fn push_toast_internal(
        &mut self,
        id: u64,
        kind: ToastKind,
        title: String,
        description: Option<String>,
        alert_key: Option<PrinterAlertKey>,
        cx: &mut Context<Self>,
    ) {
        if self.toasts.len() >= MAX_TOASTS {
            if let Some(pos) = self.toasts.iter().position(|t| t.kind != ToastKind::Error) {
                self.toasts.remove(pos);
            } else {
                if kind == ToastKind::Error {
                    // Keep visible errors and indicate omitted ones; do not add another queue.
                    self.toast_overflowed = true;
                    cx.notify();
                }
                return;
            }
        }

        self.toasts.push(Toast {
            id,
            kind,
            title,
            description,
            alert_key,
        });

        if let Some(duration) = toast_duration(kind)
            && tokio::runtime::Handle::try_current().is_ok()
        {
            cx.spawn(async move |app, cx| {
                tokio::time::sleep(duration).await;
                let _ = app.update(cx, |app, cx| {
                    app.dismiss_toast(id, cx);
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn render_toasts(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let displays = super::components::toast::present(&self.toasts, window, cx);
        if displays.is_empty() && !self.toast_overflowed {
            return div().into_any_element();
        }

        let error_count = self
            .toasts
            .iter()
            .filter(|t| t.kind == ToastKind::Error)
            .count();
        let saturated = error_count >= MAX_TOASTS || self.toast_overflowed;
        let max_stack_height = (window.viewport_size().height - px(64.)).max(px(0.));
        let stack_scroll = window
            .use_keyed_state("toast-stack-scroll", cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();

        v_flex()
            .id("toast-container")
            .test_support()
            .absolute()
            .bottom_4()
            .right_4()
            .gap(px(12.))
            .w(px(340.))
            .max_h(max_stack_height)
            .occlude()
            .track_scroll(&stack_scroll)
            .overflow_y_scroll()
            .map(|area| super::components::scroll_area::vertical(area, &stack_scroll, window, cx))
            .when(saturated, |container| {
                container.child(
                    h_flex()
                        .id("toast-saturation-alert")
                        .test_support()
                        .w_full()
                        .flex_shrink_0()
                        .items_center()
                        .gap_2()
                        .p_2p5()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().group_box)
                        .border_1()
                        .border_color(cx.theme().red)
                        .child(
                            Label::new(if self.toast_overflowed {
                                crate::i18n::t!("common.feedback.toast_overflow")
                            } else {
                                crate::i18n::t!("common.feedback.toast_limit")
                            })
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().red)
                            .whitespace_normal(),
                        )
                        .when(self.toast_overflowed, |banner| {
                            banner.child(
                                Button::new("dismiss-toast-overflow")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .accessibility_label(crate::i18n::t!(
                                        "common.feedback.dismiss_overflow"
                                    ))
                                    .flex_shrink_0()
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        app.toast_overflowed = false;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
            })
            .children(displays.into_iter().map(|display| {
                super::components::toast::render(display, max_stack_height - px(8.), window, cx)
            }))
            .into_any_element()
    }
}
