use crate::cloud::CloudStatus;
use crate::i18n::t;
use crate::shared::printer::PrintStatus;
use crate::shared::workspace::ReadStatus;
use crate::state::{LanConnection, PrinterState};

pub(super) fn format_read_value<T>(
    value: Option<T>,
    status: ReadStatus,
    connection: LanConnection,
    render: impl FnOnce(T) -> String,
) -> String {
    format_read_value_with_locale(
        value,
        status,
        connection,
        render,
        rust_i18n::locale().as_ref(),
    )
}

fn format_read_value_with_locale<T>(
    value: Option<T>,
    status: ReadStatus,
    connection: LanConnection,
    render: impl FnOnce(T) -> String,
    locale: &str,
) -> String {
    if let Some(value) = value {
        let value = render(value);
        return match (connection, status) {
            (connection, _) if connection != LanConnection::Connected => {
                t!("common.read_value.retained", locale = locale, value = value).into_owned()
            }
            (_, ReadStatus::Error) => t!(
                "common.read_value.refresh_failed",
                locale = locale,
                value = value
            )
            .into_owned(),
            (_, ReadStatus::Denied) => t!(
                "common.read_value.refresh_denied",
                locale = locale,
                value = value
            )
            .into_owned(),
            _ => value,
        };
    }
    match connection {
        LanConnection::Disconnected => t!("common.read_value.disconnected", locale = locale),
        LanConnection::Connecting => t!("common.status.connecting", locale = locale),
        LanConnection::NeedsCredentials => t!("common.status.needs_credentials", locale = locale),
        LanConnection::Incompatible => t!("common.status.incompatible", locale = locale),
        LanConnection::Connected => match status {
            ReadStatus::NotLoaded => return "—".to_owned(),
            ReadStatus::Loading => t!("common.status.loading", locale = locale),
            ReadStatus::Ready => t!("common.read_value.missing", locale = locale),
            // Only an integration's explicit evidence may set Unsupported.
            ReadStatus::Unsupported => t!("common.read_value.unsupported", locale = locale),
            ReadStatus::Error => t!("common.read_value.error", locale = locale),
            ReadStatus::Denied => t!("common.read_value.denied", locale = locale),
            ReadStatus::Limited => t!("common.read_value.limited", locale = locale),
        },
    }
    .into_owned()
}

#[allow(dead_code)]
pub(super) fn format_age(age_ms: Option<u64>) -> String {
    match age_ms {
        None => crate::i18n::t!("common.labels.no_sample").into_owned(),
        Some(ms) if ms < 1000 => "< 1 s".to_owned(),
        Some(ms) if ms < 60_000 => {
            crate::i18n::t!("common.labels.seconds", count = (ms / 1000).to_string()).into_owned()
        }
        Some(ms) => {
            crate::i18n::t!("common.labels.minutes", count = (ms / 60_000).to_string()).into_owned()
        }
    }
}

pub(super) fn print_status_label(status: PrintStatus) -> String {
    match status {
        PrintStatus::Unknown => crate::i18n::t!("common.status.unknown").into_owned(),
        PrintStatus::Idle => crate::i18n::t!("common.status.idle").into_owned(),
        PrintStatus::Printing => crate::i18n::t!("common.status.printing").into_owned(),
        PrintStatus::Paused => crate::i18n::t!("common.status.paused").into_owned(),
        PrintStatus::Completed => crate::i18n::t!("common.status.completed").into_owned(),
        PrintStatus::Cancelled => crate::i18n::t!("common.status.cancelled").into_owned(),
        PrintStatus::Error => crate::i18n::t!("common.status.error").into_owned(),
    }
}

pub(super) fn format_bytes(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) if bytes >= 1_048_576 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        Some(bytes) if bytes >= 1024 => format!("{:.1} KB", bytes as f64 / 1024.0),
        Some(bytes) => format!("{bytes} B"),
        None => "—".to_owned(),
    }
}

pub(super) fn cloud_status_label(status: Option<&CloudStatus>) -> String {
    match status {
        None | Some(CloudStatus::NotConfigured) => t!("common.status.not_configured").into_owned(),
        Some(CloudStatus::Connecting | CloudStatus::WaitingForReady) => {
            t!("common.status.connecting").into_owned()
        }
        Some(CloudStatus::Connected) => t!("common.status.connected").into_owned(),
        Some(
            CloudStatus::NeedsAttention(message)
            | CloudStatus::Unavailable(message)
            | CloudStatus::Incompatible(message),
        ) => message.clone(),
    }
}

pub(super) fn format_temperature(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:.0} °C"))
}

pub(super) fn format_temperature_with_target(current: Option<f64>, target: Option<f64>) -> String {
    let curr = format_temperature(current);
    match target {
        Some(target) => format!("{curr} / {target:.0} °C"),
        None => curr,
    }
}

pub(super) fn format_percent(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:.0}%"))
}

pub(super) fn format_speed(value: Option<f64>) -> String {
    value.map_or_else(
        || "—".to_owned(),
        |value| {
            if value.fract() == 0.0 {
                format!("{value:.0} mm/s")
            } else {
                format!("{value:.1} mm/s")
            }
        },
    )
}

pub(super) fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

pub(super) fn job_description(state: &PrinterState) -> String {
    match state.connection {
        LanConnection::Disconnected => t!("printers.job.disconnected_help").into_owned(),
        LanConnection::Connecting => t!("printers.job.connecting_help").into_owned(),
        LanConnection::NeedsCredentials => t!("printers.job.credentials_help").into_owned(),
        LanConnection::Incompatible => t!("printers.job.incompatible_help").into_owned(),
        LanConnection::Connected => match state.telemetry.print_state {
            PrintStatus::Idle => t!("printers.job.idle_help").into_owned(),
            PrintStatus::Printing => t!("printers.job.printing_help").into_owned(),
            PrintStatus::Paused => t!("printers.status.paused").into_owned(),
            PrintStatus::Completed => t!("printers.status.completed").into_owned(),
            PrintStatus::Cancelled => t!("printers.status.cancelled").into_owned(),
            PrintStatus::Error => t!("printers.status.error").into_owned(),
            PrintStatus::Unknown => t!("printers.status.unknown").into_owned(),
        },
    }
}

pub(super) fn layer_label(state: &PrinterState) -> String {
    let (cur, tot) = state
        .telemetry
        .current_job
        .as_ref()
        .or(state.telemetry.last_job.as_ref())
        .map(|job| (job.print_current_layer, job.print_total_layers))
        .unwrap_or((None, None));

    match (cur, tot) {
        (Some(current), Some(total)) => {
            t!("common.labels.layer", current = current, total = total).into_owned()
        }
        (Some(current), None) => t!("common.labels.layer_single", current = current).into_owned(),
        (None, Some(total)) => t!("common.labels.layer", current = "—", total = total).into_owned(),
        _ => t!("common.labels.layer_unknown").into_owned(),
    }
}

pub(super) fn action_label(action: crate::shared::workspace::PrinterActionKind) -> String {
    use crate::shared::workspace::PrinterActionKind;
    match action {
        PrinterActionKind::StartPrint => t!("printers.actions.start").into_owned(),
        PrinterActionKind::PausePrint => t!("printers.actions.pause").into_owned(),
        PrinterActionKind::ResumePrint => t!("printers.actions.resume").into_owned(),
        PrinterActionKind::CancelPrint => t!("printers.actions.cancel").into_owned(),
        PrinterActionKind::SetTemperature => t!("printers.actions.temperature").into_owned(),
        PrinterActionKind::SetLight => t!("printers.actions.light").into_owned(),
        PrinterActionKind::UploadGcode => t!("printers.actions.upload").into_owned(),
        PrinterActionKind::MachineControl => t!("printers.actions.machine_control").into_owned(),
    }
}
