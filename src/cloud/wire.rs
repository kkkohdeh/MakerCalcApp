use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

use super::history::HistoryCursor;
use super::planner::{ObservedPrinter, PlannerError};
use super::{Device, LOCAL_MAX_MESSAGE_BYTES, SCHEMA_VERSION};
use crate::shared::workspace::HistoryTotals;
use crate::state::LanConnection;

mod command;
mod detail;
mod technical;
pub(super) use command::Command;
pub(super) use detail::WireDetail;

#[derive(Serialize)]
pub(super) struct WireBatch<'a> {
    pub(super) r#type: &'static str,
    pub(super) v: u8,
    pub(super) seq: u64,
    pub(super) printers: &'a [WirePrinter],
}

#[derive(Clone)]
pub(super) struct WirePrinter {
    pub(super) printer_id: String,
    pub(super) name: Option<String>,
    pub(super) connected: bool,
    pub(super) age_ms: Option<u64>,
    pub(super) telemetry: Option<crate::shared::telemetry::PrinterTelemetry>,
    pub(super) history_totals: Option<HistoryTotals>,
    pub(super) device: Option<Device>,
    pub(super) thumbnail_supported: bool,
    pub(super) detail: WireDetail,
}

impl Serialize for WirePrinter {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.payload()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[derive(Serialize)]
pub(super) struct PrinterPayload<'a> {
    printer_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) telemetry: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_totals: Option<&'a HistoryTotals>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    technical: Option<&'a serde_json::Value>,
}

// Compare wire payloads; local-only changes must not trigger updates.
pub(super) fn same_wire(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

impl WirePrinter {
    pub(super) fn payload(&self) -> serde_json::Result<PrinterPayload<'_>> {
        let name = self.name.as_deref().map(str::trim).filter(|name| {
            !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
        });
        Ok(PrinterPayload {
            printer_id: &self.printer_id,
            name,
            connected: self.connected,
            age_ms: self.age_ms,
            telemetry: self.telemetry_json()?,
            history_totals: self.history_totals.as_ref(),
            device: self.detail.device_json(self.device.as_ref()),
            technical: self.detail.technical.as_ref(),
        })
    }

    fn telemetry_json(&self) -> serde_json::Result<Option<serde_json::Value>> {
        let Some(telemetry) = &self.telemetry else {
            return Ok(None);
        };
        let mut wire = serde_json::to_value(telemetry)?;
        if !self.thumbnail_supported {
            // Legacy peers reject the thumbnail hash field, including null.
            for name in ["current_job", "last_job"] {
                if let Some(job) = wire
                    .get_mut(name)
                    .and_then(serde_json::Value::as_object_mut)
                {
                    job.remove("PrintThumbnailSha256");
                }
            }
        }
        detail::bound_base_telemetry(&mut wire);
        self.detail.extend_telemetry(&mut wire);
        detail::bound_alert_ids(&mut wire);
        Ok(Some(wire))
    }

    pub(super) fn from_state(observed: &ObservedPrinter, now: Instant) -> Self {
        let is_fresh = observed.state.age_ms(now).is_none_or(|age| age <= 60_000);
        let telemetry = observed.state.snapshot.as_ref().map(|_| {
            let mut telemetry = observed.state.telemetry.clone();
            let hash = observed
                .thumbnail_supported
                .then(|| observed.thumbnail_sha256.clone())
                .flatten();
            // Put the hash on the visible job: current, or last after it ends.
            let running = telemetry.current_job.is_some();
            if let Some(job) = &mut telemetry.current_job {
                job.print_thumbnail_sha256 = hash.clone();
            }
            if let Some(job) = &mut telemetry.last_job {
                job.print_thumbnail_sha256 = if running { None } else { hash };
            }
            telemetry
        });
        let connected = observed.state.connection == LanConnection::Connected
            && observed.state.telemetry_ready
            && is_fresh;
        Self {
            printer_id: observed.cloud_printer_id.clone(),
            name: Some(observed.name.clone()),
            connected,
            age_ms: observed.state.age_ms(now),
            telemetry,
            history_totals: observed.history_totals.clone(),
            device: observed.device.clone(),
            thumbnail_supported: observed.thumbnail_supported,
            detail: if connected {
                observed.detail.clone()
            } else {
                WireDetail::default()
            },
        }
    }
}

pub(super) fn encode_batch(seq: u64, printers: &[WirePrinter]) -> Result<String, PlannerError> {
    serde_json::to_string(&WireBatch {
        r#type: "states",
        v: SCHEMA_VERSION,
        seq,
        printers,
    })
    .map_err(|_| PlannerError::InvalidLimits)
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CommandAction {
    PausePrint,
    ResumePrint,
    CancelPrint,
    ToggleLight,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum ServerMessage {
    #[serde(rename = "command")]
    Command {
        v: u8,
        printer_id: String,
        #[serde(default)]
        action: serde_json::Value,
        #[serde(flatten)]
        params: serde_json::Map<String, serde_json::Value>,
    },
    #[serde(rename = "ready")]
    Ready {
        v: u8,
        edge_id: String,
        workshop_id: String,
        max_message_bytes: u64,
        max_printers: u64,
        min_interval_ms: u64,
        #[serde(default)]
        history_max: Option<usize>,
        #[serde(default)]
        history_persist: bool,
        #[serde(default)]
        thumbnail_max_bytes: Option<usize>,
    },
    #[serde(rename = "history_cursors")]
    HistoryCursors { v: u8, cursors: Vec<HistoryCursor> },
    #[serde(rename = "ack")]
    Ack { v: u8, seq: u64 },
    #[serde(rename = "reject")]
    Reject {
        v: u8,
        seq: u64,
        code: String,
        retry_after_ms: Option<u64>,
    },
    #[serde(rename = "session_error")]
    SessionError {
        v: u8,
        #[serde(rename = "code")]
        _code: String,
    },
    #[serde(other)]
    Unknown,
}

pub(super) fn decode_server_message(message: Message) -> Result<ServerMessage, String> {
    match message {
        Message::Text(text) => {
            if text.len() > LOCAL_MAX_MESSAGE_BYTES {
                return Err("mensaje cloud supera 64 KiB".to_owned());
            }
            serde_json::from_str(&text).map_err(|_| "mensaje cloud inválido".to_owned())
        }
        Message::Close(_) => Err("la entrada cerró la conexión".to_owned()),
        Message::Binary(_) => Err("la entrada envió mensaje binario".to_owned()),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
            Err("la entrada no envió un mensaje de sesión".to_owned())
        }
    }
}

pub(super) fn valid_remote_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
