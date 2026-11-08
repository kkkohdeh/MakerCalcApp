//! Normalize reported readings; unsupported fields remain `None`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;
use tokio::time::sleep;
use tokio_util::io::ReaderStream;

use crate::shared::gcode::GcodeMaterialChannel;
use crate::shared::printer::PrinterObservation;
use crate::shared::telemetry::{
    BedTelemetry, ElementId, FilamentSupplyTelemetry, LightTelemetry, NozzleTelemetry,
    PartCoolingFanTelemetry, PrinterAlert, SupplyState,
};
use crate::shared::thumbnail::ThumbnailError;
use crate::shared::workspace::{
    DetailIo, DetailRequest, DetailState, FileDetails, HeaterKind, HistoryTotals, LightRole,
    OperationIo, OperationState, PrinterAction, PrinterActionKind, PrinterFile, ReadResult,
    ReadStatus, ResourceId, Support,
};
use crate::state::{
    LanConnection, LocalPrinterId, PrintStatus, PrinterState, ValidationError, sanitize_filename,
    sanitize_message,
};

mod camera;
mod machine;

static NEXT_FILE_RESOURCE: AtomicU64 = AtomicU64::new(1);
const MAX_FILE_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_THUMBNAIL_BYTES: usize = 1024 * 1024;
const MAX_THUMBNAIL_JSON_BYTES: usize = MAX_THUMBNAIL_BYTES * 3 / 2;
const MAX_ACTION_REPLY_BYTES: usize = 64 * 1024;

async fn bounded_flashforge_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ThumbnailError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ThumbnailError::Oversized);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ThumbnailError::Unavailable)?;
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(ThumbnailError::Oversized);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct FlashforgeActionReply {
    code: i64,
    #[serde(default)]
    message: String,
}

async fn flashforge_operation_result(
    request: reqwest::RequestBuilder,
    kind: PrinterActionKind,
) -> OperationState {
    use crate::shared::control::{ControlError, ControlResult};
    let reply = match flashforge_action_reply(request).await {
        Ok(reply) => reply,
        Err(ControlResult::Rejected(ControlError::SendFailed)) => {
            return OperationState::Rejected {
                action: kind,
                message: crate::i18n::t!("common.errors.send").into_owned(),
            };
        }
        Err(_) => return OperationState::Unknown(kind),
    };
    if reply.code == 0 {
        OperationState::Accepted(kind)
    } else {
        OperationState::Rejected {
            action: kind,
            message: format!(
                "FlashForge {}: {}",
                reply.code,
                sanitize_message(&reply.message, 512)
            ),
        }
    }
}

async fn flashforge_action_reply(
    request: reqwest::RequestBuilder,
) -> Result<FlashforgeActionReply, crate::shared::control::ControlResult> {
    use crate::shared::control::{ControlError, ControlResult};
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) if error.is_connect() || error.is_builder() => {
            return Err(ControlResult::Rejected(ControlError::SendFailed));
        }
        Err(_) => return Err(ControlResult::Unknown),
    };
    if !response.status().is_success() {
        return Err(ControlResult::Unknown);
    }
    // A sent order has an unknown outcome unless the printer supplies a valid result.
    let Ok(body) = bounded_flashforge_body(response, MAX_ACTION_REPLY_BYTES).await else {
        return Err(ControlResult::Unknown);
    };
    let Ok(reply) = serde_json::from_slice::<FlashforgeActionReply>(&body) else {
        return Err(ControlResult::Unknown);
    };
    Ok(reply)
}

/// Response envelope from FlashForge HTTP REST API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FlashforgeEnvelope {
    pub(crate) code: i32,
    #[serde(default)]
    pub(crate) message: String,
    pub(crate) detail: Option<FlashforgeDetail>,
}

/// Detail payload from `POST /detail` on FlashForge Adventurer 5M series (port 8898).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlashforgeDetail {
    #[serde(flatten)]
    machine: machine::Detail,
    pub(crate) status: Option<String>,
    pub(crate) print_file_name: Option<String>,
    pub(crate) print_progress: Option<f64>,
    pub(crate) print_layer: Option<u32>,
    pub(crate) target_print_layer: Option<u32>,
    pub(crate) print_duration: Option<f64>,
    pub(crate) estimated_time: Option<f64>,
    pub(crate) right_temp: Option<f64>,
    pub(crate) right_target_temp: Option<f64>,
    pub(crate) left_temp: Option<f64>,
    pub(crate) left_target_temp: Option<f64>,
    pub(crate) plat_temp: Option<f64>,
    pub(crate) plat_target_temp: Option<f64>,
    pub(crate) cooling_fan_speed: Option<f64>,
    pub(crate) print_speed_adjust: Option<f64>,
    pub(crate) current_print_speed: Option<f64>,
    pub(crate) right_filament_type: Option<String>,
    pub(crate) error_code: Option<String>,
    pub(crate) chamber_temp: Option<f64>,
    pub(crate) estimated_right_len: Option<f64>,
    pub(crate) estimated_left_len: Option<f64>,
    pub(crate) estimated_right_weight: Option<f64>,
    pub(crate) estimated_left_weight: Option<f64>,
    pub(crate) cumulative_filament: Option<f64>,
    pub(crate) cumulative_print_time: Option<f64>,
    pub(crate) has_matl_station: Option<FlashforgeFlag>,
    pub(crate) matl_station_info: Option<FlashforgeMaterialStationInfo>,
    pub(crate) light_status: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum FlashforgeFlag {
    Boolean(bool),
    Integer(u8),
}

impl FlashforgeFlag {
    fn as_bool(self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(value),
            Self::Integer(0) => Some(false),
            Self::Integer(1) => Some(true),
            Self::Integer(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StationAction {
    Idle,
    Load,
    Unload,
    Other,
}

impl StationAction {
    fn from_raw(raw: Option<u32>) -> Self {
        match raw {
            Some(1) => Self::Load,
            Some(2) => Self::Unload,
            Some(0) => Self::Idle,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlashforgeMaterialStationInfo {
    pub(crate) current_slot: Option<u32>,
    pub(crate) current_load_slot: Option<u32>,
    pub(crate) state_action: Option<u32>,
    pub(crate) state_step: Option<u32>,
    pub(crate) slot_cnt: Option<u32>,
    #[serde(default)]
    pub(crate) slot_infos: Vec<FlashforgeMaterialSlot>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlashforgeMaterialSlot {
    pub(crate) slot_id: Option<u32>,
    pub(crate) has_filament: Option<bool>,
    pub(crate) material_name: Option<String>,
    pub(crate) material_color: Option<String>,
}

/// Response envelope for `POST /gcodeList` on FlashForge HTTP REST API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FlashforgeGcodeListEnvelope {
    pub(crate) code: i32,
    #[serde(default)]
    pub(crate) message: String,
    #[serde(rename = "gcodeList", default)]
    pub(crate) gcode_list: Vec<String>,
    #[serde(rename = "gcodeListDetail", default)]
    pub(crate) gcode_list_detail: Vec<FlashforgeGcodeDetailEntry>,
}

/// Detail entry for each G-code item from `POST /gcodeList`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlashforgeGcodeDetailEntry {
    #[serde(default)]
    pub(crate) gcode_file_name: String,
    pub(crate) printing_time: Option<f64>,
    pub(crate) total_filament_weight: Option<f64>,
    pub(crate) gcode_tool_cnt: Option<u32>,
    #[serde(default)]
    pub(crate) gcode_tool_datas: Vec<FlashforgeGcodeToolData>,
    pub(crate) use_matl_station: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlashforgeGcodeToolData {
    pub(crate) tool_id: Option<u32>,
    pub(crate) material_name: Option<String>,
    pub(crate) material_color: Option<String>,
    pub(crate) filament_weight: Option<f64>,
}

#[derive(Debug)]
pub(crate) enum FlashforgeError {
    Transport(String),
    Protocol(String),
    Validation(ValidationError),
}

impl fmt::Display for FlashforgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "Error de transporte FlashForge: {msg}"),
            Self::Protocol(msg) => write!(f, "Respuesta no conforme de FlashForge: {msg}"),
            Self::Validation(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for FlashforgeError {}

impl From<ValidationError> for FlashforgeError {
    fn from(err: ValidationError) -> Self {
        Self::Validation(err)
    }
}

pub(crate) fn map_status(status_str: Option<&str>) -> PrintStatus {
    let Some(raw) = status_str else {
        return PrintStatus::Unknown;
    };
    match raw.trim().to_lowercase().as_str() {
        "completed" => PrintStatus::Completed,
        "ready" | "idle" => PrintStatus::Idle,
        "printing" | "heating" | "pausing" => PrintStatus::Printing,
        "pause" => PrintStatus::Paused,
        "cancel" | "cancelled" => PrintStatus::Cancelled,
        // Transient non-printing/system operations:
        "busy" | "calibrate_doing" | "downloading" | "cloud_slicing" | "sending" | "unzipping" => {
            PrintStatus::Unknown
        }
        "error" => PrintStatus::Error,
        "" => PrintStatus::Unknown,
        _ => PrintStatus::Unknown,
    }
}

/// Normalize `/detail` into `PrinterObservation`.
/// Convert `printProgress` from 0..=1 to percent; `printDuration` is elapsed time.
/// Use `estimatedTime` as remaining time only while printing or paused.
/// Unverified readings remain absent; chamber data is model-dependent.
pub(crate) fn parse_detail_to_observation(
    detail: &FlashforgeDetail,
) -> Result<PrinterObservation, ValidationError> {
    let filename = detail
        .print_file_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(sanitize_filename);

    // A filename can remain on screen during maintenance. It is not evidence of printing.
    // `canceling` is also used while entering pause, so only a confirmed final closes a job.
    let status = map_status(detail.status.as_deref());

    let progress_pct = match detail.print_progress {
        Some(ratio) => {
            if ratio.is_nan() || ratio.is_infinite() || !(0.0..=1.0).contains(&ratio) {
                return Err(ValidationError::InvalidProgress);
            }
            Some(ratio * 100.0)
        }
        None => None,
    };

    // Nozzle temperature & target (5M uses rightTemp / rightTargetTemp)
    let nozzle_temp = detail.right_temp.or(detail.left_temp);
    let nozzle_target = detail.right_target_temp.or(detail.left_target_temp);

    if let Some(temp) = nozzle_temp
        && !temp.is_finite()
    {
        return Err(ValidationError::InvalidTemperature);
    }
    if let Some(target) = nozzle_target
        && (!target.is_finite() || target < 0.0)
    {
        return Err(ValidationError::InvalidTemperature);
    }

    if let Some(temp) = detail.plat_temp
        && !temp.is_finite()
    {
        return Err(ValidationError::InvalidTemperature);
    }
    if let Some(target) = detail.plat_target_temp
        && (!target.is_finite() || target < 0.0)
    {
        return Err(ValidationError::InvalidTemperature);
    }

    let elapsed_s = match detail.print_duration {
        Some(s) => {
            if !s.is_finite() || s < 0.0 {
                return Err(ValidationError::InvalidDuration);
            }
            if s > 0.0 || status == PrintStatus::Printing {
                Some(s)
            } else {
                None
            }
        }
        None => None,
    };

    // Remaining print duration:
    // Firmware resets this value when idle/ready, but reports it during pauses too.
    let remaining_s = if matches!(status, PrintStatus::Printing | PrintStatus::Paused) {
        match detail.estimated_time {
            Some(s) if s.is_finite() && s > 0.0 => Some(s),
            _ => None,
        }
    } else {
        None
    };

    let (layer_current, layer_total) = match (detail.print_layer, detail.target_print_layer) {
        (Some(c), Some(t)) if c == 0 && t == 0 && status != PrintStatus::Printing => (None, None),
        (Some(c), Some(t)) if t == 0 || c > t => (Some(c), None),
        (c, t) => (c, t),
    };

    let fan_power_pct = match detail.cooling_fan_speed {
        Some(pct) => {
            if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
                return Err(ValidationError::InvalidFan);
            }
            Some(pct)
        }
        None => None,
    };

    let speed_override_pct = match detail.print_speed_adjust {
        Some(pct) => {
            if !pct.is_finite() || pct < 0.0 {
                return Err(ValidationError::InvalidSpeedOverride);
            }
            Some(pct)
        }
        None => None,
    };

    // Head velocity mm/s:
    // Only exposed while printing or paused.
    let velocity_mm_s = match detail.current_print_speed {
        Some(v) => {
            if !v.is_finite() || v < 0.0 {
                return Err(ValidationError::InvalidVelocity);
            }
            if status == PrintStatus::Printing || status == PrintStatus::Paused {
                Some(v)
            } else {
                None
            }
        }
        None => None,
    };

    let alerts = if let Some(code) = &detail.error_code {
        let trimmed = code.trim();
        if !trimmed.is_empty() && trimmed != "0" {
            Some(vec![PrinterAlert::new(
                "print_error",
                sanitize_message(trimmed, 512),
            )])
        } else if status == PrintStatus::Error {
            Some(vec![PrinterAlert::new(
                "print_error",
                "Fallo confirmado de impresora FlashForge".to_owned(),
            )])
        } else if matches!(
            status,
            PrintStatus::Idle
                | PrintStatus::Printing
                | PrintStatus::Paused
                | PrintStatus::Completed
                | PrintStatus::Cancelled
        ) {
            Some(Vec::new())
        } else {
            None
        }
    } else if status == PrintStatus::Error {
        Some(vec![PrinterAlert::new(
            "print_error",
            "Fallo confirmado de impresora FlashForge".to_owned(),
        )])
    } else if matches!(
        status,
        PrintStatus::Idle
            | PrintStatus::Printing
            | PrintStatus::Paused
            | PrintStatus::Completed
            | PrintStatus::Cancelled
    ) {
        Some(Vec::new())
    } else {
        None
    };

    let nozzles = if matches!(machine::product(&detail.machine), Some(40 | 41)) {
        // These machines publish per-tool arrays without an authoritative active-tool signal.
        // The local resource inventory preserves all tools and an unknown active state.
        Vec::new()
    } else if nozzle_temp.is_some() || nozzle_target.is_some() {
        vec![NozzleTelemetry {
            id: ElementId(1),
            is_active: true,
            nozzle_temp_c: nozzle_temp,
            nozzle_target_temp_c: nozzle_target,
        }]
    } else {
        Vec::new()
    };

    let beds = if detail.plat_temp.is_some() || detail.plat_target_temp.is_some() {
        vec![BedTelemetry {
            id: ElementId(1),
            bed_temp_c: detail.plat_temp,
            bed_target_temp_c: detail.plat_target_temp,
        }]
    } else {
        Vec::new()
    };

    let part_cooling_fans = if fan_power_pct.is_some() {
        vec![PartCoolingFanTelemetry {
            id: ElementId(1),
            power_pct: fan_power_pct,
            rpm: None,
        }]
    } else {
        Vec::new()
    };

    let material_station_available = detail
        .has_matl_station
        .and_then(FlashforgeFlag::as_bool)
        .or_else(|| detail.matl_station_info.as_ref().map(|_| true));

    // Inactive IFS metadata can remain in an AD5X response while using the external feed.
    let filament_supplies = if machine::product(&detail.machine) == Some(38)
        && material_station_available == Some(false)
    {
        None
    } else if let Some(station) = &detail.matl_station_info {
        if station.slot_infos.len() > 16 {
            return Err(ValidationError::InvalidFilament);
        }
        let action = StationAction::from_raw(station.state_action);
        let supplies = station
            .slot_infos
            .iter()
            .filter_map(|slot| {
                let id = slot.slot_id.filter(|id| *id > 0)?;
                let material = slot
                    .material_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| sanitize_message(value, 64));
                let color = slot
                    .material_color
                    .as_deref()
                    .and_then(normalize_material_color);
                let is_active = station
                    .current_slot
                    .map(|active| active != 0 && active == id);
                let is_target = station.current_load_slot.is_some_and(|target| target == id);
                let active_flag = is_active.unwrap_or(false);

                let state = match (slot.has_filament, action, is_target, active_flag) {
                    (None, _, _, _) => SupplyState::Unknown,
                    (Some(false), _, _, _) => SupplyState::Empty,
                    (Some(true), StationAction::Unload, true, _) => SupplyState::Unloading,
                    (Some(true), StationAction::Load, true, _) => SupplyState::Loading,
                    (Some(true), _, _, true) => SupplyState::Active,
                    (Some(true), _, _, false) => SupplyState::Available,
                };

                Some(FilamentSupplyTelemetry {
                    id: ElementId(id),
                    filament_present: slot.has_filament,
                    material,
                    color,
                    is_active,
                    state,
                })
            })
            .collect::<Vec<_>>();
        Some(supplies)
    } else {
        detail
            .right_filament_type
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|material| {
                vec![FilamentSupplyTelemetry {
                    id: ElementId(1),
                    filament_present: None,
                    material: Some(sanitize_message(material, 64)),
                    color: None,
                    is_active: None,
                    state: SupplyState::Unknown,
                }]
            })
    };

    let filament_used_mm = match (detail.estimated_right_len, detail.estimated_left_len) {
        (Some(r), Some(l)) => {
            if !r.is_finite() || r < 0.0 || !l.is_finite() || l < 0.0 {
                return Err(ValidationError::InvalidFilament);
            }
            Some(r + l)
        }
        (Some(r), None) => {
            if !r.is_finite() || r < 0.0 {
                return Err(ValidationError::InvalidFilament);
            }
            Some(r)
        }
        (None, Some(l)) => {
            if !l.is_finite() || l < 0.0 {
                return Err(ValidationError::InvalidFilament);
            }
            Some(l)
        }
        (None, None) => None,
    };

    let filament_used_g = match (detail.estimated_right_weight, detail.estimated_left_weight) {
        (Some(r), Some(l)) => {
            if !r.is_finite() || r < 0.0 || !l.is_finite() || l < 0.0 {
                return Err(ValidationError::InvalidFilament);
            }
            if r > 0.0 || l > 0.0 {
                Some(r + l)
            } else {
                None
            }
        }
        (Some(r), None) => {
            if !r.is_finite() || r < 0.0 {
                return Err(ValidationError::InvalidFilament);
            }
            if r > 0.0 { Some(r) } else { None }
        }
        (None, Some(l)) => {
            if !l.is_finite() || l < 0.0 {
                return Err(ValidationError::InvalidFilament);
            }
            if l > 0.0 { Some(l) } else { None }
        }
        (None, None) => None,
    };

    let observation = PrinterObservation {
        status: Some(status),
        progress_pct,
        filename,
        chamber_c: (machine::product(&detail.machine) == Some(41))
            .then(|| {
                detail
                    .chamber_temp
                    .filter(|value| value.is_finite() && *value >= 0.0)
            })
            .flatten(),
        layer_current,
        layer_total,
        elapsed_s,
        remaining_s,
        filament_used_mm,
        filament_used_g,
        velocity_mm_s,
        speed_override_pct,
        alerts,
        nozzles,
        beds,
        part_cooling_fans,
        filament_sensors: Vec::new(),
        filament_supplies,
        lights: match detail.light_status.as_deref() {
            Some("open") => vec![LightTelemetry {
                id: ElementId(1),
                role: LightRole::BuildArea,
                enabled: Some(true),
            }],
            Some("close") => vec![LightTelemetry {
                id: ElementId(1),
                role: LightRole::BuildArea,
                enabled: Some(false),
            }],
            _ => Vec::new(),
        },
        material_station_available,
        print_thumbnail_sha256: None,
    };

    observation.validate()?;
    Ok(observation)
}

fn normalize_material_color(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| value.to_ascii_uppercase())
}

fn material_station_support(state: &PrinterState) -> Support {
    match state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.material_station_available)
    {
        Some(true) => Support::Available,
        Some(false) => Support::Unavailable,
        None => Support::Unknown,
    }
}

/// Parses full JSON response payload from `POST /detail`.
pub(crate) fn parse_json_response(json_str: &str) -> Result<PrinterObservation, FlashforgeError> {
    parse_json_response_with_history(json_str).map(|(observation, _)| observation)
}

fn parse_json_response_with_history(
    json_str: &str,
) -> Result<(PrinterObservation, Option<HistoryTotals>), FlashforgeError> {
    parse_json_response_full(json_str).map(|(observation, details)| (observation, details.totals))
}

fn parse_json_response_full(
    json_str: &str,
) -> Result<(PrinterObservation, machine::PollDetails), FlashforgeError> {
    let envelope: FlashforgeEnvelope = serde_json::from_str(json_str)
        .map_err(|e| FlashforgeError::Protocol(format!("JSON inválido: {e}")))?;

    if envelope.code != 0 {
        return Err(FlashforgeError::Protocol(format!(
            "FlashForge respondió error code {}: {}",
            envelope.code, envelope.message
        )));
    }

    let detail = envelope
        .detail
        .ok_or_else(|| FlashforgeError::Protocol("Payload sin objeto 'detail'".to_owned()))?;

    let observation = parse_detail_to_observation(&detail).map_err(FlashforgeError::Validation)?;
    let history_totals = normalize_history_totals(&detail)?;
    let details = machine::PollDetails {
        totals: history_totals,
        detail,
        observation: observation.clone(),
        observed_at: Instant::now(),
        connection_epoch: 0,
    };
    Ok((observation, details))
}

fn normalize_history_totals(
    detail: &FlashforgeDetail,
) -> Result<Option<HistoryTotals>, FlashforgeError> {
    let print_duration_s = match detail.cumulative_print_time {
        Some(minutes) if minutes.is_finite() && minutes >= 0.0 && (minutes * 60.0).is_finite() => {
            Some(minutes * 60.0)
        }
        Some(_) => {
            return Err(FlashforgeError::Protocol(
                "cumulativePrintTime inválido".to_owned(),
            ));
        }
        None => None,
    };
    let filament_used_mm = match detail.cumulative_filament {
        Some(meters) if meters.is_finite() && meters >= 0.0 && (meters * 1_000.0).is_finite() => {
            Some(meters * 1_000.0)
        }
        Some(_) => {
            return Err(FlashforgeError::Protocol(
                "cumulativeFilament inválido".to_owned(),
            ));
        }
        None => None,
    };

    if print_duration_s.is_none() && filament_used_mm.is_none() {
        return Ok(None);
    }

    Ok(Some(HistoryTotals {
        jobs: None,
        print_duration_s,
        total_duration_s: None,
        filament_used_mm,
        longest_print_duration_s: None,
        longest_total_duration_s: None,
    }))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_thumbnail_image(bytes: &[u8]) -> Option<Bytes> {
    crate::shared::thumbnail::normalize_thumbnail_png(bytes)
}

/// Response envelope for `POST /gcodeThumb` on FlashForge HTTP REST API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FlashforgeThumbEnvelope {
    pub(crate) code: i32,
    #[serde(default)]
    pub(crate) message: String,
    #[serde(rename = "imageData")]
    pub(crate) image_data: Option<String>,
}

/// Parses the JSON response string from `POST /gcodeThumb`, decodes base64 payload,
/// and returns normalized PNG thumbnail bytes with dimensions.
pub(crate) fn parse_gcode_thumb_image(json_str: &str) -> Option<(Bytes, u32, u32)> {
    if json_str.len() > MAX_THUMBNAIL_JSON_BYTES {
        return None;
    }
    let envelope: FlashforgeThumbEnvelope = serde_json::from_str(json_str).ok()?;
    if envelope.code != 0 {
        return None;
    }
    let data = envelope.image_data?;
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_THUMBNAIL_BYTES.div_ceil(3) * 4 {
        return None;
    }
    let raw_bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .ok()?;
    if raw_bytes.len() > MAX_THUMBNAIL_BYTES {
        return None;
    }
    crate::shared::thumbnail::normalize_thumbnail_image(&raw_bytes)
}

/// Parses the JSON response string from `POST /gcodeThumb`, decodes base64 payload,
/// and returns normalized PNG thumbnail bytes.
pub(crate) fn parse_gcode_thumb_response(json_str: &str) -> Option<Bytes> {
    parse_gcode_thumb_image(json_str).map(|(bytes, _, _)| bytes)
}

/// Parses the JSON response string from `POST /gcodeList`.
pub(crate) fn parse_gcode_list_response(
    json_str: &str,
) -> Result<FlashforgeGcodeListEnvelope, FlashforgeError> {
    let envelope: FlashforgeGcodeListEnvelope = serde_json::from_str(json_str)
        .map_err(|e| FlashforgeError::Protocol(format!("JSON inválido en /gcodeList: {e}")))?;

    if envelope.code != 0 {
        return Err(FlashforgeError::Protocol(format!(
            "FlashForge respondió error code {} en gcodeList: {}",
            envelope.code, envelope.message
        )));
    }

    Ok(envelope)
}

pub(crate) fn verify_request(
    client: &reqwest::Client,
    candidate: &crate::discovery::Candidate,
    api_key: Option<&str>,
) -> Result<reqwest::RequestBuilder, String> {
    let (serial, code) = api_key
        .and_then(|s| s.split_once(':'))
        .filter(|(serial, code)| !serial.is_empty() && !code.is_empty())
        .ok_or("Introduce el serial y el checkCode de la impresora")?;
    let url = crate::discovery::endpoint_url(candidate, "/detail")?;
    Ok(client
        .post(url)
        .header("Content-Type", "application/json")
        .body(serde_json::json!({"serialNumber": serial, "checkCode": code}).to_string()))
}

pub(crate) fn verify_response(body: &[u8]) -> bool {
    std::str::from_utf8(body)
        .ok()
        .is_some_and(|text| parse_json_response(text).is_ok())
}

/// Parses raw byte response from TCP port 8899 command `~M661`.
/// Supports binary format with length prefixes and fallback to delimiter-based text.
pub(crate) fn parse_m661_response(data: &[u8]) -> Vec<String> {
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut i = 0;
    while i + 8 <= data.len() {
        if data[i] == b':' && data[i + 1] == b':' && data[i + 2] == 0xA3 && data[i + 3] == 0xA3 {
            let len =
                u32::from_be_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
            let start = i + 8;
            let end = start + len;
            if end <= data.len() {
                if let Ok(path_str) = std::str::from_utf8(&data[start..end]) {
                    let name = path_str.rsplit('/').next().unwrap_or(path_str).trim();
                    if !name.is_empty() && seen.insert(name.to_owned()) {
                        files.push(name.to_owned());
                    }
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }

    if files.is_empty()
        && let Ok(text) = std::str::from_utf8(data)
    {
        for part in text.split("::") {
            let trimmed = part.trim();
            let name = trimmed.rsplit('/').next().unwrap_or(trimmed).trim();
            if (name.ends_with(".gcode") || name.ends_with(".3mf") || name.ends_with(".gx"))
                && seen.insert(name.to_owned())
            {
                files.push(name.to_owned());
            }
        }
    }

    files
}

pub(crate) type FlashforgeFileCatalog =
    BTreeMap<ResourceId, (String, Option<FlashforgeGcodeDetailEntry>)>;

fn replace_file_catalog(
    catalog: &mut FlashforgeFileCatalog,
    replacement: FlashforgeFileCatalog,
    state: &mut DetailState,
) {
    state
        .file_details
        .retain(|id, _| match (catalog.get(id), replacement.get(id)) {
            (Some((old_name, _)), Some((new_name, _))) => old_name == new_name,
            _ => false,
        });
    *catalog = replacement;
}

/// Transforms FlashForge file sources (TCP ~M661 file list and/or HTTP envelope)
/// into normalized `PrinterFile` structs and internal catalog.
pub(crate) fn map_gcode_sources_to_files(
    tcp_files: &[String],
    envelope: Option<&FlashforgeGcodeListEnvelope>,
    previous: &FlashforgeFileCatalog,
) -> (Vec<PrinterFile>, FlashforgeFileCatalog) {
    let mut files = Vec::new();
    let mut catalog = BTreeMap::new();
    let previous_ids: BTreeMap<&str, ResourceId> = previous
        .iter()
        .map(|(id, (name, _))| (name.as_str(), *id))
        .collect();

    let details_by_name: BTreeMap<&str, &FlashforgeGcodeDetailEntry> = envelope
        .map(|env| {
            env.gcode_list_detail
                .iter()
                .map(|entry| (entry.gcode_file_name.trim(), entry))
                .collect()
        })
        .unwrap_or_default();

    let details_by_clean: BTreeMap<String, &FlashforgeGcodeDetailEntry> = envelope
        .map(|env| {
            env.gcode_list_detail
                .iter()
                .map(|entry| {
                    (
                        sanitize_filename(entry.gcode_file_name.trim()).to_lowercase(),
                        entry,
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let mut raw_names: Vec<&str> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for raw in tcp_files {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && seen.insert(trimmed) {
            raw_names.push(trimmed);
        }
    }

    if let Some(env) = envelope {
        for raw in &env.gcode_list {
            let trimmed = raw.trim();
            if !trimmed.is_empty() && seen.insert(trimmed) {
                raw_names.push(trimmed);
            }
        }
        for detail in &env.gcode_list_detail {
            let trimmed = detail.gcode_file_name.trim();
            if !trimmed.is_empty() && seen.insert(trimmed) {
                raw_names.push(trimmed);
            }
        }
    }

    for raw_name in raw_names {
        let clean_name = sanitize_filename(raw_name);
        if clean_name.is_empty() {
            continue;
        }
        let id = previous_ids
            .get(raw_name)
            .copied()
            .unwrap_or_else(|| ResourceId(NEXT_FILE_RESOURCE.fetch_add(1, Ordering::Relaxed)));
        let detail_entry = details_by_name
            .get(raw_name)
            .copied()
            .or_else(|| details_by_clean.get(&clean_name.to_lowercase()).copied())
            .cloned();
        let estimated_duration_s = detail_entry
            .as_ref()
            .and_then(|entry| entry.printing_time)
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0);

        files.push(PrinterFile {
            id,
            display_name: clean_name,
            size_bytes: None,
            modified_at_unix_s: None,
            duration_read_status: if estimated_duration_s.is_some() {
                ReadStatus::Ready
            } else if envelope.is_some() {
                // HTTP metadata covers recent files only; neither TCP names nor
                // the thumbnail endpoint can recover another file's duration.
                ReadStatus::Unsupported
            } else {
                ReadStatus::NotLoaded
            },
            estimated_duration_s,
        });

        catalog.insert(id, (raw_name.to_owned(), detail_entry));
    }

    (files, catalog)
}

/// Transforms FlashForge `/gcodeList` envelope into normalized `PrinterFile` structs
/// and a secure internal catalog for resolving `ResourceId` to file metadata.
#[allow(dead_code)]
pub(crate) fn map_gcode_list_to_files(
    envelope: &FlashforgeGcodeListEnvelope,
) -> (Vec<PrinterFile>, FlashforgeFileCatalog) {
    map_gcode_sources_to_files(&[], Some(envelope), &BTreeMap::new())
}

/// Builds normalized `FileDetails` from FlashForge metadata and decoded thumbnail.
pub(crate) fn build_file_details(
    detail_entry: Option<&FlashforgeGcodeDetailEntry>,
    thumbnail_bytes: Option<Bytes>,
) -> FileDetails {
    let mut details = FileDetails {
        metadata_status: ReadStatus::Ready,
        analysis_status: ReadStatus::Ready,
        ..Default::default()
    };

    if let Some(entry) = detail_entry {
        if let Some(secs) = entry.printing_time
            && secs.is_finite()
            && secs > 0.0
        {
            details.analysis.print.estimated_seconds = Some(secs);
        }
        if let Some(weight) = entry.total_filament_weight
            && weight.is_finite()
            && weight > 0.0
        {
            details.analysis.filament.weight_g = Some(weight);
        }
        details.analysis.material_channels = entry
            .gcode_tool_datas
            .iter()
            .filter_map(|tool| {
                let id = tool.tool_id?;
                let material = tool
                    .material_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
                let color = tool
                    .material_color
                    .as_deref()
                    .and_then(normalize_material_color);
                let weight_g = tool
                    .filament_weight
                    .filter(|value| value.is_finite() && *value >= 0.0);
                Some(GcodeMaterialChannel {
                    id,
                    material,
                    color,
                    weight_g,
                })
            })
            .collect();
    }

    details.thumbnail = thumbnail_bytes;
    details
}

/// Builds the JSON request payload for `POST /printGcode` mapping normalized `StartPrintOptions`.
pub(crate) fn build_print_gcode_payload(
    serial_number: &str,
    check_code: &str,
    filename: &str,
    options: &crate::shared::workspace::StartPrintOptions,
    detail_entry: Option<&FlashforgeGcodeDetailEntry>,
    supplies: &[FilamentSupplyTelemetry],
) -> Result<serde_json::Value, String> {
    if !options.material_station && !options.material_assignments.is_empty() {
        return Err("Hay asignaciones de material pero la estación está desactivada".to_owned());
    }

    let tools = detail_entry
        .map(|entry| entry.gcode_tool_datas.as_slice())
        .unwrap_or_default();
    if options.material_station
        && !tools.is_empty()
        && options.material_assignments.len() != tools.len()
    {
        return Err("Asigna todos los materiales del archivo antes de imprimir".to_owned());
    }

    let mut mappings = Vec::with_capacity(options.material_assignments.len());
    for assignment in &options.material_assignments {
        if assignment.channel_id > 3 {
            return Err("FlashForge sólo admite materiales lógicos entre 0 y 3".to_owned());
        }
        let tool = tools
            .iter()
            .find(|tool| tool.tool_id == Some(assignment.channel_id))
            .ok_or_else(|| {
                "La asignación ya no corresponde a un material del archivo".to_owned()
            })?;
        let supply = supplies
            .iter()
            .find(|supply| supply.id == assignment.supply_id)
            .ok_or_else(|| "El suministro seleccionado ya no está disponible".to_owned())?;
        if !(1..=4).contains(&supply.id.0) {
            return Err("FlashForge sólo admite suministros físicos entre 1 y 4".to_owned());
        }
        if supply.filament_present == Some(false) {
            return Err(format!("El suministro {} está vacío", supply.id));
        }
        let material_name = tool
            .material_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "FlashForge no informó el material lógico del archivo".to_owned())?;
        let tool_color = tool
            .material_color
            .as_deref()
            .and_then(normalize_material_color)
            .ok_or_else(|| {
                "FlashForge no informó un color válido para el material lógico".to_owned()
            })?;
        let slot_color = supply
            .color
            .as_deref()
            .and_then(normalize_material_color)
            .ok_or_else(|| "FlashForge no informó un color válido para el suministro".to_owned())?;
        mappings.push(json!({
            "toolId": assignment.channel_id,
            "slotId": supply.id.0,
            "materialName": material_name,
            "toolMaterialColor": tool_color,
            "slotMaterialColor": slot_color,
        }));
    }

    let mut payload = json!({
        "serialNumber": serial_number,
        "checkCode": check_code,
        "fileName": filename,
        "levelingBeforePrint": options.bed_leveling,
        "useMatlStation": options.material_station,
    });
    if !mappings.is_empty() {
        payload["firstLayerInspection"] = json!(options.first_layer_inspection);
        payload["flowCalibration"] = json!(options.flow_calibration);
        payload["timeLapseVideo"] = json!(options.timelapse);
        payload["gcodeToolCnt"] = json!(mappings.len());
        payload["materialMappings"] = json!(mappings);
    }
    if options.first_layer_inspection {
        payload["firstLayerInspection"] = json!(true);
    }
    if options.flow_calibration {
        payload["flowCalibration"] = json!(true);
    }
    if options.timelapse {
        payload["timeLapseVideo"] = json!(true);
    }
    Ok(payload)
}

/// Queries the complete list of files stored in FlashForge internal memory
/// via TCP port 8899 command `~M661`.
async fn fetch_tcp_gcode_list(host: &str) -> Option<Vec<String>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect((host, 8899)),
    )
    .await
    .ok()?
    .ok()?;

    tokio::time::timeout(Duration::from_secs(2), stream.write_all(b"~M661\r\n"))
        .await
        .ok()?
        .ok()?;

    let mut buffer = Vec::with_capacity(32768);
    let mut chunk = [0u8; 4096];
    let start = tokio::time::Instant::now();

    loop {
        match tokio::time::timeout(Duration::from_millis(400), stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.len() >= 65536 {
                    break;
                }
            }
            _ => break,
        }
        if start.elapsed() > Duration::from_secs(3) {
            break;
        }
    }

    if buffer.is_empty() {
        return None;
    }

    let files = parse_m661_response(&buffer);
    if files.is_empty() { None } else { Some(files) }
}

/// Runs the FlashForge detail request handler task.
///
/// Dispatches `DetailIo` requests (such as `ListFiles` and `InspectFile`)
/// without blocking the passive telemetry loop.
#[allow(clippy::too_many_arguments)]
async fn run_detail_handler(
    client: reqwest::Client,
    host: String,
    port: u16,
    serial_number: String,
    check_code: String,
    mut detail_io: DetailIo,
    mut operation_io: OperationIo,
    mut shutdown: watch::Receiver<bool>,
    mut state_receiver: watch::Receiver<PrinterState>,
    mut history_receiver: watch::Receiver<Option<machine::PollDetails>>,
) {
    let mut current_state = DetailState::empty(1);
    current_state.capabilities.files = Support::Available;
    let mut catalog: BTreeMap<ResourceId, (String, Option<FlashforgeGcodeDetailEntry>)> =
        BTreeMap::new();
    let mut catalog_epoch = state_receiver.borrow().connection_epoch;
    let mut machine = machine::Projection::default();
    let mut last_operation_finished = None;
    let mut pending_motion = false;
    let gcode_list_url = format!("http://{host}:{port}/gcodeList");
    let gcode_list_body = json!({
        "serialNumber": serial_number,
        "checkCode": check_code,
    })
    .to_string();

    loop {
        tokio::select! {
            biased;

            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }

            changed = state_receiver.changed() => {
                if changed.is_err() {
                    break;
                }
                let connected = state_receiver.borrow().connection == LanConnection::Connected;
                if !connected {
                    machine.abort_load(crate::shared::control::ControlResult::Unknown);
                    pending_motion = false;
                    if matches!(*operation_io.state.borrow(), OperationState::Sending(PrinterActionKind::MachineControl)) {
                        operation_io.state.send_replace(OperationState::Idle);
                    }
                }
                let epoch = state_receiver.borrow().connection_epoch;
                let catalog_invalidated = catalog_epoch != epoch;
                if catalog_invalidated {
                    catalog_epoch = epoch;
                    catalog.clear();
                    current_state.files = ReadResult::default();
                    current_state.file_details.clear();
                    machine.invalidate();
                    current_state.machine = Default::default();
                    current_state.devices.clear();
                    current_state.cameras = ReadResult::default();
                }
                let active = *detail_io.active.borrow();
                let live = connected && active;
                let station_support = material_station_support(&state_receiver.borrow());
                if catalog_invalidated || current_state.live != live
                    || current_state.capabilities.material_station != station_support
                {
                    current_state.live = live;
                    current_state.capabilities.material_station = station_support;
                    current_state.revision = current_state.revision.saturating_add(1);
                    let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                }
            }

            changed = history_receiver.changed() => {
                if changed.is_err() {
                    break;
                }
                let details = history_receiver.borrow_and_update().clone();
                if let Some(details) = details.as_ref() {
                    if details.connection_epoch != state_receiver.borrow().connection_epoch { continue; }
                    if let Some(totals) = &details.totals {
                        current_state.capabilities.history = Support::Available;
                        current_state.history_totals = ReadResult {
                            status: ReadStatus::Limited,
                            value: Some(totals.clone()),
                            error: None,
                        };
                    }
                    machine.update(details, state_receiver.borrow().connection_epoch, &host, &mut current_state);
                    if current_state.live && machine.needs_motor_probe() {
                        let verified = machine::motors::probe(&host).await;
                        machine.set_motor_support(verified, &mut current_state);
                    }
                    let motion_busy = pending_motion && current_state.machine.activity.is_some()
                        && !matches!(state_receiver.borrow().telemetry.print_state, PrintStatus::Printing | PrintStatus::Paused);
                    if !machine.waiting_for_observation() && !motion_busy
                        && matches!(*operation_io.state.borrow(), OperationState::Sending(PrinterActionKind::MachineControl)) {
                        pending_motion = false;
                        operation_io.state.send_replace(OperationState::Idle);
                    }
                    let next = machine.next_load(&state_receiver.borrow(), &mut current_state);
                    if let Some((payload, reply)) = next {
                        let url = format!("http://{host}:{port}/control");
                        let result = tokio::select! {
                            biased;
                            _ = shutdown.changed() => crate::shared::control::ControlResult::Unknown,
                            result = machine::dispatch(&client, &url, &serial_number, &check_code, payload) => result,
                        };
                        last_operation_finished = Some(Instant::now());
                        machine.dispatched(Instant::now());
                        machine.refresh_supply_controls(&mut current_state);
                        let _ = reply.send(result);
                        while let Ok(queued) = operation_io.machine_actions.try_recv() {
                            let _ = queued.reply.send(crate::shared::control::ControlResult::Rejected(crate::shared::control::ControlError::Busy));
                        }
                    }
                    current_state.revision = current_state.revision.saturating_add(1);
                    let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                }
            }

            changed = detail_io.active.changed() => {
                if changed.is_err() {
                    break;
                }
                let active = *detail_io.active.borrow_and_update();
                let connected = state_receiver.borrow().connection == LanConnection::Connected;
                let live = connected && active;
                if !active {
                    pending_motion = false;
                    if matches!(*operation_io.state.borrow(), OperationState::Sending(PrinterActionKind::MachineControl)) {
                        operation_io.state.send_replace(OperationState::Idle);
                    }
                    machine.abort_load(crate::shared::control::ControlResult::Unknown);
                    let history_totals = current_state.history_totals.clone();
                    let history_support = current_state.capabilities.history;
                    current_state = DetailState::empty(1);
                    current_state.capabilities.files = Support::Available;
                    current_state.capabilities.history = history_support;
                    current_state.capabilities.material_station =
                        material_station_support(&state_receiver.borrow());
                    current_state.history_totals = history_totals;
                    machine.invalidate();
                    catalog.clear();
                    let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                } else if current_state.live != live {
                    current_state.live = live;
                    current_state.revision = current_state.revision.saturating_add(1);
                    let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                }
            }

            maybe_req = detail_io.requests.recv() => {
                let Some(request) = maybe_req else {
                    break;
                };
                match request {
                    DetailRequest::ListFiles => {
                        current_state.files.status = ReadStatus::Loading;
                        current_state.revision = current_state.revision.saturating_add(1);
                        let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));

                        let tcp_names = if machine.http_only() { Vec::new() } else {
                            fetch_tcp_gcode_list(&host).await.unwrap_or_default()
                        };

                        let req = client
                            .post(&gcode_list_url)
                            .header("Content-Type", "application/json")
                            .body(gcode_list_body.clone());

                        let fetch_res = match req.send().await {
                            Ok(resp) if resp.status().is_success() => {
                                bounded_flashforge_body(resp, MAX_FILE_LIST_BYTES)
                                    .await.ok()
                                    .and_then(|body| std::str::from_utf8(&body).ok()
                                        .and_then(|text| parse_gcode_list_response(text).ok()))
                            }
                            _ => None,
                        };

                        if !tcp_names.is_empty() || fetch_res.is_some() {
                            let (files, new_catalog) =
                                map_gcode_sources_to_files(&tcp_names, fetch_res.as_ref(), &catalog);
                            replace_file_catalog(&mut catalog, new_catalog, &mut current_state);
                            current_state.files = ReadResult {
                                status: ReadStatus::Ready,
                                value: Some(files),
                                error: None,
                            };
                        } else {
                            current_state.files = ReadResult {
                                status: ReadStatus::Error,
                                value: current_state.files.value.clone(),
                                error: Some(
                                    "No se pudo obtener la lista de archivos de la impresora".to_owned(),
                                ),
                            };
                        }
                        current_state.revision = current_state.revision.saturating_add(1);
                        let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                    }

                    DetailRequest::InspectFile(id) => {
                        let (filename, detail_opt) = match catalog.get(&id) {
                            Some(val) => (val.0.clone(), val.1.clone()),
                            None => {
                                let entry = current_state.file_details.entry(id).or_default();
                                entry.status = ReadStatus::Error;
                                entry.error = Some("Archivo no encontrado en la impresora".to_owned());
                                current_state.revision = current_state.revision.saturating_add(1);
                                let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                                continue;
                            }
                        };

                        let entry = current_state.file_details.entry(id).or_default();
                        entry.status = ReadStatus::Loading;
                        current_state.revision = current_state.revision.saturating_add(1);
                        let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));

                        let thumb_url = format!("http://{host}:{port}/gcodeThumb");
                        let thumb_body = json!({
                            "serialNumber": serial_number,
                            "checkCode": check_code,
                            "fileName": filename,
                        })
                        .to_string();

                        let thumb_bytes = match client
                            .post(&thumb_url)
                            .header("Content-Type", "application/json")
                            .body(thumb_body)
                            .timeout(Duration::from_secs(5))
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                match bounded_flashforge_body(resp, MAX_THUMBNAIL_JSON_BYTES).await {
                                    Ok(body) => crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
                                        std::str::from_utf8(&body).ok().and_then(parse_gcode_thumb_response)
                                    }).await.ok().flatten(),
                                    Err(_) => None,
                                }
                            }
                            _ => None,
                        };

                        let details = build_file_details(detail_opt.as_ref(), thumb_bytes);
                        if let Some(seconds) = details.analysis.print.estimated_seconds
                            && let Some(file) = current_state.files.value.as_mut().and_then(|files| files.iter_mut().find(|file| file.id == id))
                        {
                            file.estimated_duration_s = Some(seconds);
                            file.duration_read_status = ReadStatus::Ready;
                        }
                        current_state.file_details.insert(id, ReadResult {
                            status: ReadStatus::Ready,
                            value: Some(details),
                            error: None,
                        });
                        current_state.revision = current_state.revision.saturating_add(1);
                        let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                    }

                    DetailRequest::HistoryTotals => {
                        if let Some(totals) = history_receiver.borrow().as_ref().and_then(|details| details.totals.clone()) {
                            current_state.capabilities.history = Support::Available;
                            current_state.history_totals = ReadResult {
                                status: ReadStatus::Limited,
                                value: Some(totals),
                                error: None,
                            };
                        } else {
                            current_state.history_totals.status = ReadStatus::Loading;
                        }
                        current_state.revision = current_state.revision.saturating_add(1);
                        let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                    }

                    _ => {}
                }
            }

            Some(command) = operation_io.machine_actions.recv() => {
                use crate::shared::control::{ControlError, ControlResult};
                if command.reply.is_closed() { continue; }
                let validation = {
                    let state = state_receiver.borrow();
                    if command.submitted_at.elapsed() > Duration::from_secs(5)
                        || command.job_target.is_some_and(|job| job != state.machine_job_target()) {
                        Err(ControlError::StaleResource)
                    } else if last_operation_finished.is_some_and(|at| command.submitted_at <= at)
                        || operation_io.state.borrow().is_pending() || !operation_io.actions.is_empty() {
                        Err(ControlError::Busy)
                    } else if matches!(command.action, crate::shared::control::MachineAction::DisableMotors) {
                        machine.validate_control(&command.action, &state, &current_state, command.connection_epoch).map(|_| None)
                    } else {
                        machine.payload(&command.action, &state, &current_state, command.connection_epoch).map(Some)
                    }
                };
                let source = machine.load_source(&command.action, &current_state);
                let validation = validation.and_then(|payload| match source {
                    Some(supply_id) => machine.payload(&crate::shared::control::MachineAction::UnloadFilament { supply_id }, &state_receiver.borrow(), &current_state, command.connection_epoch).map(Some),
                    None => Ok(payload),
                });
                if matches!(command.action, crate::shared::control::MachineAction::CancelFilamentOperation { .. }) {
                    machine.abort_load(ControlResult::Unknown);
                }
                let dispatch_started = Instant::now();
                let result = match validation {
                    Err(error) => ControlResult::Rejected(error),
                    Ok(payload) => {
                        let url = format!("http://{host}:{port}/control");
                        let result = match payload {
                            Some(payload) => tokio::select! {
                                biased;
                                _ = shutdown.changed() => ControlResult::Unknown,
                                result = machine::dispatch(&client, &url, &serial_number, &check_code, payload) => result,
                            },
                            // Finish bounded TCP cleanup even when the view or app closes.
                            None => machine::motors::release(&host, || {
                                if *shutdown.borrow() || command.reply.is_closed() || !*detail_io.active.borrow() {
                                    return Err(ControlError::InvalidState);
                                }
                                machine.validate_control(&command.action, &state_receiver.borrow(), &current_state, command.connection_epoch)?;
                                let latest = history_receiver.borrow();
                                let poll = latest.as_ref().ok_or(ControlError::StaleResource)?;
                                machine.validate_motor_poll(poll, command.connection_epoch, command.submitted_at)
                            }).await,
                        };
                        last_operation_finished = Some(Instant::now());
                        if matches!(result, ControlResult::Accepted | ControlResult::Unknown) {
                            machine.dispatched(Instant::now());
                            pending_motion = matches!(command.action, crate::shared::control::MachineAction::HomeAxes { .. } | crate::shared::control::MachineAction::MoveAxisRelative { .. });
                            operation_io.state.send_replace(OperationState::Sending(PrinterActionKind::MachineControl));
                            for supply in &mut current_state.machine.supplies {
                                for control in supply.controls.iter_mut().flatten() {
                                    control.blocked = Some(crate::shared::machine::SupplyBlock::Busy);
                                }
                            }
                            current_state.revision = current_state.revision.saturating_add(1);
                            detail_io.state.send_replace(Arc::new(current_state.clone()));
                        }
                        while let Ok(queued) = operation_io.machine_actions.try_recv() {
                            let _ = queued.reply.send(ControlResult::Rejected(ControlError::Busy));
                        }
                        while let Ok(queued) = operation_io.actions.try_recv() {
                            operation_io.state.send_replace(OperationState::Rejected { action: queued.kind(), message: crate::i18n::t!("common.errors.operation_busy").into_owned() });
                        }
                        result
                    }
                };
                if let (Some(source), crate::shared::control::MachineAction::LoadFilament { supply_id }, ControlResult::Accepted) = (source, &command.action, result) {
                    machine.defer_load(source, *supply_id, command.connection_epoch, dispatch_started, command.reply);
                } else {
                    let _ = command.reply.send(result);
                }
            }

            _ = async {
                if let Some(deadline) = machine.load_deadline() {
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                machine.abort_load(crate::shared::control::ControlResult::Unknown);
                machine.refresh_supply_controls(&mut current_state);
                current_state.revision = current_state.revision.saturating_add(1);
                detail_io.state.send_replace(Arc::new(current_state.clone()));
            }

            action_opt = operation_io.actions.recv() => {
                let Some(action) = action_opt else {
                    break;
                };
                if machine.waiting_for_observation() || machine.load_deadline().is_some() {
                    operation_io.state.send_replace(OperationState::Rejected { action: action.kind(), message: crate::i18n::t!("common.errors.operation_busy").into_owned() });
                    continue;
                }
                let context = match &action {
                    PrinterAction::PausePrint(context) | PrinterAction::ResumePrint(context)
                    | PrinterAction::CancelPrint(context) => Some(context),
                    _ => None,
                };
                if context.is_some_and(|context| !state_receiver.borrow().permits_print_control(action.kind(), context)) {
                    operation_io.state.send_replace(OperationState::Rejected {
                        action: action.kind(),
                        message: crate::i18n::t!("printers.actions.state_changed").into_owned(),
                    });
                    continue;
                }
                match action {
                    PrinterAction::StartPrint { file_id, options } => {
                        use crate::shared::workspace::PrintOptionKind as P;
                        let unsupported = [
                            (options.first_layer_inspection, P::FirstLayerInspection),
                            (options.flow_calibration, P::FlowCalibration),
                            (options.timelapse, P::Timelapse),
                        ].iter().any(|(requested, kind)| *requested && !current_state.machine.print_options.contains(kind));
                        if unsupported {
                            operation_io.state.send_replace(OperationState::Rejected {
                                action: PrinterActionKind::StartPrint,
                                message: crate::i18n::t!("printers.print_options.unsupported").into_owned(),
                            });
                            continue;
                        }
                        let current_status = state_receiver.borrow().telemetry.print_state;
                        if current_status != PrintStatus::Idle {
                            let _ = operation_io.state.send_replace(OperationState::Rejected {
                                action: PrinterActionKind::StartPrint,
                                message: "La impresora no está lista (debe estar en espera; retira la pieza previa y confirma en su pantalla)".to_owned(),
                            });
                            continue;
                        }
                        let Some((filename, detail_entry)) = catalog.get(&file_id) else {
                            let _ = operation_io.state.send_replace(OperationState::Rejected {
                                action: PrinterActionKind::StartPrint,
                                message: "El archivo seleccionado no se encuentra en el catálogo local".to_owned(),
                            });
                            continue;
                        };
                        let filename = filename.clone();
                        let detail_entry = detail_entry.clone();
                        let supplies = state_receiver
                            .borrow()
                            .telemetry
                            .filament_supplies
                            .clone()
                            .unwrap_or_default();
                        let print_body = match build_print_gcode_payload(
                            &serial_number,
                            &check_code,
                            &filename,
                            &options,
                            detail_entry.as_ref(),
                            &supplies,
                        ) {
                            Ok(payload) => payload.to_string(),
                            Err(message) => {
                                let _ = operation_io.state.send_replace(OperationState::Rejected {
                                    action: PrinterActionKind::StartPrint,
                                    message,
                                });
                                continue;
                            }
                        };
                        let _ = operation_io
                            .state
                            .send_replace(OperationState::Sending(PrinterActionKind::StartPrint));
                        let print_url = format!("http://{host}:{port}/printGcode");
                        let request = client
                            .post(&print_url)
                            .header("Content-Type", "application/json")
                            .body(print_body)
                            .timeout(Duration::from_secs(10));
                        let result = flashforge_operation_result(request, PrinterActionKind::StartPrint).await;
                        let _ = operation_io.state.send_replace(result);
                    }

                    PrinterAction::PausePrint(..) => {
                        send_flashforge_job_control(
                            &client,
                            &host,
                            port,
                            &serial_number,
                            &check_code,
                            "pause",
                            PrinterActionKind::PausePrint,
                            &operation_io,
                        )
                        .await;
                    }

                    PrinterAction::ResumePrint(..) => {
                        send_flashforge_job_control(
                            &client,
                            &host,
                            port,
                            &serial_number,
                            &check_code,
                            "continue",
                            PrinterActionKind::ResumePrint,
                            &operation_io,
                        )
                        .await;
                    }

                    PrinterAction::CancelPrint(..) => {
                        send_flashforge_job_control(
                            &client,
                            &host,
                            port,
                            &serial_number,
                            &check_code,
                            "cancel",
                            PrinterActionKind::CancelPrint,
                            &operation_io,
                        )
                        .await;
                    }

                    PrinterAction::SetTemperatureTarget { heater, target_c, connection_epoch, submitted_at } => {
                        let kind = PrinterActionKind::SetTemperature;
                        let current_status = state_receiver.borrow().telemetry.print_state;
                        if matches!(current_status, PrintStatus::Printing | PrintStatus::Paused)
                            || target_c > heater.max_target_c()
                            || !current_state.live
                            || !current_state.temperature_targets.contains(&heater)
                            || !state_receiver.borrow().age_ms(Instant::now()).is_some_and(|age| age <= 15_000)
                            || state_receiver.borrow().connection != LanConnection::Connected
                            || !state_receiver.borrow().telemetry_ready
                            || state_receiver.borrow().connection_epoch != connection_epoch
                            || submitted_at.elapsed() > Duration::from_secs(5)
                        {
                            let _ = operation_io.state.send_replace(OperationState::Rejected {
                                action: kind,
                                message: crate::i18n::t!("printers.temperature.unavailable").into_owned(),
                            });
                            continue;
                        }
                        let body = build_temperature_control_payload(
                            &serial_number,
                            &check_code,
                            heater,
                            target_c,
                        );
                        send_flashforge_control(
                            &client,
                            &host,
                            port,
                            body,
                            kind,
                            &operation_io,
                        )
                        .await;
                    }

                    PrinterAction::ToggleLight { role } => {
                        let kind = PrinterActionKind::SetLight;
                        if role != LightRole::BuildArea {
                            let _ = operation_io.state.send_replace(OperationState::Rejected {
                                action: kind,
                                message: "FlashForge no expone esa luz por su API local".to_owned(),
                            });
                            continue;
                        }
                        let Some(enabled) = state_receiver
                            .borrow()
                            .telemetry
                            .lights
                            .iter()
                            .find(|light| light.role == role)
                            .and_then(|light| light.enabled)
                        else {
                            let _ = operation_io.state.send_replace(OperationState::Rejected {
                                action: kind,
                                message: "FlashForge no informó el estado actual de la luz"
                                    .to_owned(),
                            });
                            continue;
                        };
                        let body = build_light_control_payload(
                            &serial_number,
                            &check_code,
                            !enabled,
                        );
                        send_flashforge_control(
                            &client,
                            &host,
                            port,
                            body,
                            kind,
                            &operation_io,
                        )
                        .await;
                    }

                    PrinterAction::UploadGcode { source, destination } => {
                        upload_flashforge_gcode(
                            &client,
                            &host,
                            port,
                            &serial_number,
                            &check_code,
                            &source,
                            &destination,
                            &operation_io,
                        )
                        .await;

                        if matches!(
                            &*operation_io.state.borrow(),
                            OperationState::Accepted(PrinterActionKind::UploadGcode)
                        ) {
                            let mut tcp_names =
                                fetch_tcp_gcode_list(&host).await.unwrap_or_default();
                            if !tcp_names.iter().any(|n| n == &destination) {
                                tcp_names.insert(0, destination.clone());
                            }

                            let req = client
                                .post(&gcode_list_url)
                                .header("Content-Type", "application/json")
                                .body(gcode_list_body.clone());
                            let fetch_res = match req.send().await {
                                Ok(resp) if resp.status().is_success() => {
                                    bounded_flashforge_body(resp, MAX_FILE_LIST_BYTES)
                                        .await.ok()
                                        .and_then(|body| std::str::from_utf8(&body).ok()
                                            .and_then(|text| parse_gcode_list_response(text).ok()))
                                }
                                _ => None,
                            };

                            let (files, new_catalog) =
                                map_gcode_sources_to_files(&tcp_names, fetch_res.as_ref(), &catalog);
                            replace_file_catalog(&mut catalog, new_catalog, &mut current_state);
                            current_state.files = ReadResult {
                                status: ReadStatus::Ready,
                                value: Some(files),
                                error: None,
                            };
                            current_state.revision = current_state.revision.saturating_add(1);
                            let _ = detail_io.state.send_replace(Arc::new(current_state.clone()));
                        }
                    }
                }
                last_operation_finished = Some(Instant::now());
                while let Ok(queued) = operation_io.machine_actions.try_recv() {
                    let _ = queued.reply.send(crate::shared::control::ControlResult::Rejected(crate::shared::control::ControlError::Busy));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_flashforge_job_control(
    client: &reqwest::Client,
    host: &str,
    port: u16,
    serial_number: &str,
    check_code: &str,
    action: &str,
    kind: PrinterActionKind,
    operation_io: &OperationIo,
) {
    let ctl_body = json!({
        "serialNumber": serial_number,
        "checkCode": check_code,
        "payload": {
            "cmd": "jobCtl_cmd",
            "args": {
                "action": action,
            }
        }
    });

    send_flashforge_control(client, host, port, ctl_body, kind, operation_io).await;
}

fn build_temperature_control_payload(
    serial_number: &str,
    check_code: &str,
    heater: HeaterKind,
    target_c: u16,
) -> serde_json::Value {
    let vendor_target = if target_c == 0 {
        -100
    } else {
        i64::from(target_c)
    };
    let (right_nozzle, platform) = match heater {
        HeaterKind::Nozzle => (vendor_target, -200),
        HeaterKind::Bed => (-200, vendor_target),
    };
    json!({
        "serialNumber": serial_number,
        "checkCode": check_code,
        "payload": {
            "cmd": "temperatureCtl_cmd",
            "args": {
                "rightNozzle": right_nozzle,
                "platform": platform,
            }
        }
    })
}

fn build_light_control_payload(
    serial_number: &str,
    check_code: &str,
    enabled: bool,
) -> serde_json::Value {
    json!({
        "serialNumber": serial_number,
        "checkCode": check_code,
        "payload": {
            "cmd": "lightControl_cmd",
            "args": { "status": if enabled { "open" } else { "close" } }
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn send_flashforge_control(
    client: &reqwest::Client,
    host: &str,
    port: u16,
    ctl_body: serde_json::Value,
    kind: PrinterActionKind,
    operation_io: &OperationIo,
) {
    let _ = operation_io
        .state
        .send_replace(OperationState::Sending(kind));
    let ctl_url = format!("http://{host}:{port}/control");

    let request = client
        .post(&ctl_url)
        .header("Content-Type", "application/json")
        .body(ctl_body.to_string())
        .timeout(Duration::from_secs(10));
    let result = flashforge_operation_result(request, kind).await;
    let _ = operation_io.state.send_replace(result);
}

/// Upload a G-code file using `POST /uploadGcode` multipart HTTP.
#[allow(clippy::too_many_arguments)]
async fn upload_flashforge_gcode(
    client: &reqwest::Client,
    host: &str,
    port: u16,
    serial_number: &str,
    check_code: &str,
    source: &Path,
    destination: &str,
    operation_io: &OperationIo,
) {
    let length = match crate::shared::workspace::validate_upload(source, destination) {
        Ok(len) => len,
        Err(err) => {
            let _ = operation_io.state.send_replace(OperationState::Rejected {
                action: PrinterActionKind::UploadGcode,
                message: err.to_string(),
            });
            return;
        }
    };

    let _ = operation_io
        .state
        .send_replace(OperationState::Sending(PrinterActionKind::UploadGcode));

    let file = match tokio::fs::File::open(source).await {
        Ok(f) => f,
        Err(e) => {
            let _ = operation_io.state.send_replace(OperationState::Rejected {
                action: PrinterActionKind::UploadGcode,
                message: format!("No se pudo abrir el archivo local: {e}"),
            });
            return;
        }
    };

    let stream = ReaderStream::new(file);
    let body = reqwest::Body::wrap_stream(stream);
    let part = match reqwest::multipart::Part::stream_with_length(body, length)
        .file_name(destination.to_owned())
        .mime_str("application/octet-stream")
    {
        Ok(p) => p,
        Err(e) => {
            let _ = operation_io.state.send_replace(OperationState::Rejected {
                action: PrinterActionKind::UploadGcode,
                message: format!("Error al preparar multipart: {e}"),
            });
            return;
        }
    };

    let form = reqwest::multipart::Form::new().part("gcodeFile", part);
    let upload_url = format!("http://{host}:{port}/uploadGcode");

    let req = client
        .post(&upload_url)
        .header("serialNumber", serial_number)
        .header("checkCode", check_code)
        .header("fileSize", length.to_string())
        .header("printNow", "false")
        .header("levelingBeforePrint", "false")
        .header("flowCalibration", "false")
        .header("useMatlStation", "false")
        .header("gcodeToolCnt", "0")
        .header("materialMappings", "W10=")
        .multipart(form)
        .timeout(Duration::from_secs(180));

    let result = flashforge_operation_result(req, PrinterActionKind::UploadGcode).await;
    let _ = operation_io.state.send_replace(result);
}

pub(crate) async fn fetch_thumbnail(
    client: reqwest::Client,
    host: String,
    port: u16,
    serial: String,
    check_code: String,
    key: crate::shared::thumbnail::ThumbnailResourceKey,
) -> Result<crate::shared::thumbnail::NormalizedThumbnail, crate::shared::thumbnail::ThumbnailError>
{
    let port = if port == 0 { 8898 } else { port };

    // 1. Try active print thumbnail from GET /getThum
    let active_url = format!("http://{host}:{port}/getThum");
    if let Ok(resp) = client.get(&active_url).send().await
        && resp.status().is_success()
    {
        let body = bounded_flashforge_body(resp, MAX_THUMBNAIL_BYTES).await?;
        if !body.is_empty() {
            let normalized = crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
                crate::shared::thumbnail::normalize_thumbnail_image(&body)
            })
            .await
            .ok()
            .flatten();
            if let Some((png_bytes, width, height)) = normalized {
                return Ok(crate::shared::thumbnail::NormalizedThumbnail {
                    key,
                    png_bytes,
                    width,
                    height,
                });
            }
        }
    }

    // 2. Fallback to POST /gcodeThumb for stored files
    let thumb_url = format!("http://{host}:{port}/gcodeThumb");
    let body = json!({
        "serialNumber": serial,
        "checkCode": check_code,
        "fileName": key.print_name,
    })
    .to_string();

    let resp = client
        .post(&thumb_url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| crate::shared::thumbnail::ThumbnailError::Unavailable)?;

    if !resp.status().is_success() {
        return Err(crate::shared::thumbnail::ThumbnailError::Unavailable);
    }

    let body = bounded_flashforge_body(resp, MAX_THUMBNAIL_JSON_BYTES).await?;
    if body.is_empty() {
        return Err(crate::shared::thumbnail::ThumbnailError::NotFound);
    }

    let text = String::from_utf8(body)
        .map_err(|_| crate::shared::thumbnail::ThumbnailError::Unavailable)?;
    let normalized = crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
        parse_gcode_thumb_image(&text)
    })
    .await
    .map_err(|_| crate::shared::thumbnail::ThumbnailError::Unavailable)?
    .ok_or(crate::shared::thumbnail::ThumbnailError::NotFound)?;

    let (png_bytes, width, height) = normalized;
    Ok(crate::shared::thumbnail::NormalizedThumbnail {
        key,
        png_bytes,
        width,
        height,
    })
}

/// Poll `/detail` every 2 s while printing, otherwise every 5 s.
/// Valid telemetry connects; repeated network errors reconnect while preserving stale readings.
#[allow(clippy::too_many_arguments, dead_code)]
pub(crate) async fn run_driver(
    id: LocalPrinterId,
    host: String,
    port: u16,
    api_key: Option<String>,
    state_sender: watch::Sender<PrinterState>,
    shutdown: watch::Receiver<bool>,
    detail_io: DetailIo,
    operation_io: OperationIo,
) {
    let (_thumb_handle, thumb_io) = crate::shared::thumbnail::make_thumbnail_channels();
    let (_camera_handle, camera_io) = crate::shared::workspace::make_camera_channels();
    run_driver_with_thumbnail(
        id,
        host,
        port,
        api_key,
        state_sender,
        shutdown,
        detail_io,
        operation_io,
        thumb_io,
        camera_io,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_driver_with_thumbnail(
    _id: LocalPrinterId,
    host: String,
    port: u16,
    api_key: Option<String>,
    state_sender: watch::Sender<PrinterState>,
    mut shutdown: watch::Receiver<bool>,
    detail_io: DetailIo,
    operation_io: OperationIo,
    thumbnail_io: crate::shared::thumbnail::ThumbnailIo,
    camera_io: crate::shared::workspace::CameraIo,
) {
    let port = if port == 0 { 8898 } else { port };
    let url = format!("http://{host}:{port}/detail");
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let mut state = state_sender.borrow().clone();
            let _ = state.record_error(format!("No se pudo inicializar cliente HTTP: {e}"));
            let _ = state_sender.send(state);
            return;
        }
    };

    let (serial_number, check_code) = match api_key.as_deref() {
        Some(s) if s.contains(':') => {
            let mut parts = s.splitn(2, ':');
            (
                parts.next().unwrap_or_default().trim(),
                parts.next().unwrap_or_default().trim(),
            )
        }
        Some(s) => (s.trim(), ""),
        None => ("", ""),
    };
    let body = json!({
        "serialNumber": serial_number,
        "checkCode": check_code,
    });

    let detail_shutdown = shutdown.clone();
    let state_receiver = state_sender.subscribe();
    let (history_sender, history_receiver) = watch::channel(None);
    let mut operation_updates = operation_io.state.subscribe();
    let camera_task = tokio::spawn(camera::run(
        client.clone(),
        host.clone(),
        camera_io,
        detail_io.state.subscribe(),
        state_sender.subscribe(),
        shutdown.clone(),
    ));
    let detail_task = tokio::spawn(run_detail_handler(
        client.clone(),
        host.clone(),
        port,
        serial_number.to_string(),
        check_code.to_string(),
        detail_io,
        operation_io,
        detail_shutdown,
        state_receiver,
        history_receiver,
    ));

    let thumb_host = host.clone();
    let thumb_port = port;
    let thumb_serial = serial_number.to_string();
    let thumb_check_code = check_code.to_string();
    let thumb_client = client.clone();
    let thumb_state = state_sender.subscribe();
    let _thumbnail_task = tokio::spawn(crate::shared::thumbnail::run_thumbnail_actor(
        thumbnail_io,
        thumb_state,
        move |key| {
            let client = thumb_client.clone();
            let host = thumb_host.clone();
            let serial = thumb_serial.clone();
            let check_code = thumb_check_code.clone();
            async move { fetch_thumbnail(client, host, thumb_port, serial, check_code, key).await }
        },
    ));

    let mut consecutive_failures: u32 = 0;

    loop {
        if *shutdown.borrow() {
            break;
        }

        let requested_at = Instant::now();
        let request = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string());

        let poll_result = match request.send().await {
            Ok(response) => {
                if response.status().is_success() {
                    let mut bytes = Vec::new();
                    let mut stream = response.bytes_stream();
                    let mut read_err = None;
                    while let Some(chunk_res) = stream.next().await {
                        match chunk_res {
                            Ok(chunk) => {
                                if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                                    read_err = Some(FlashforgeError::Protocol(
                                        "Respuesta supera 1 MiB".to_owned(),
                                    ));
                                    break;
                                }
                                bytes.extend_from_slice(&chunk);
                            }
                            Err(e) => {
                                read_err = Some(FlashforgeError::Transport(e.to_string()));
                                break;
                            }
                        }
                    }
                    if let Some(err) = read_err {
                        Err(err)
                    } else {
                        match String::from_utf8(bytes) {
                            Ok(text) => parse_json_response_full(&text),
                            Err(e) => Err(FlashforgeError::Protocol(format!(
                                "Respuesta no es UTF-8 válido: {e}"
                            ))),
                        }
                    }
                } else {
                    Err(FlashforgeError::Transport(format!(
                        "HTTP {}",
                        response.status()
                    )))
                }
            }
            Err(e) => Err(FlashforgeError::Transport(e.to_string())),
        };

        match poll_result {
            Ok((observation, mut details)) => {
                consecutive_failures = 0;
                details.observed_at = requested_at;
                let mut state = state_sender.borrow().clone();
                let now = Instant::now();
                if let Err(e) = state.publish_snapshot(observation, now) {
                    let _ = state.record_error(format!("Error de validación: {e}"));
                }
                details.connection_epoch = state.connection_epoch;
                let _ = state_sender.send(state);
                let _ = history_sender.send_replace(Some(details));
            }
            Err(err) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                if consecutive_failures >= 3 {
                    let mut state = state_sender.borrow().clone();
                    let _ = state.set_connection(LanConnection::Connecting);
                    let _ = state.record_error(err.to_string());
                    let _ = state_sender.send(state);
                }
            }
        }

        let is_printing = state_sender.borrow().telemetry.print_state == PrintStatus::Printing;
        let poll_interval = if matches!(
            *operation_updates.borrow(),
            OperationState::Sending(PrinterActionKind::MachineControl)
        ) {
            Duration::from_millis(300)
        } else if is_printing {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(5)
        };

        tokio::select! {
            _ = sleep(poll_interval) => {},
            _ = async {
                while operation_updates.changed().await.is_ok() {
                    if matches!(*operation_updates.borrow_and_update(), OperationState::Sending(PrinterActionKind::MachineControl)) {
                        return;
                    }
                }
                std::future::pending::<()>().await;
            } => {},
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    detail_task.abort();
    camera_task.abort();
}
