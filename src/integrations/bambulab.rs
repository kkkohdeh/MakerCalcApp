//! Bambu Lab printers over local MQTT.
//!
//! `mqtt` keeps the latest report, this module translates it into the shared printer
//! observation and `machine` into the device inventory.

mod control;
mod machine;
mod mqtt;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant as TokioInstant, sleep_until};

use crate::i18n::t;
use crate::shared::control::{ControlError, ControlResult, MachineCommand};
use crate::shared::printer::{PrintStatus, PrinterObservation};
use crate::shared::telemetry::{
    BedTelemetry, ElementId, FilamentSensorTelemetry, FilamentSupplyTelemetry, LightRole,
    LightTelemetry, MAX_LIGHTS, NozzleTelemetry, PartCoolingFanTelemetry, PrinterAlert,
    SupplyState,
};
use crate::shared::workspace::{DetailIo, DetailState, OperationIo, OperationState, PrinterAction};
use crate::state::{LanConnection, PrinterState, sanitize_message};

/// `print_error` when the user cancels: the job ends as cancelled, not failed.
const CANCELLED_BY_USER: i64 = 0x0300_400C;
/// The state boundary rejects longer alert lists.
const MAX_ALERTS: usize = 32;
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Units 254 and 255 are the external spools of the second and main nozzle.
const EXTERNAL_SECOND: u32 = 254;
const EXTERNAL_MAIN: u32 = 255;

/// Runs until shutdown. `credential` is `serial:access_code`, the same shape the
/// configuration stores for FlashForge.
pub(crate) async fn run_driver(
    host: String,
    port: u16,
    credential: Option<String>,
    state_sender: watch::Sender<PrinterState>,
    mut shutdown: watch::Receiver<bool>,
    detail_io: DetailIo,
    operation_io: OperationIo,
) {
    let Some((serial, access_code)) = credential
        .as_deref()
        .and_then(crate::config::bambu_credentials)
    else {
        state_sender.send_modify(|state| {
            let _ = state.set_connection(LanConnection::NeedsCredentials);
            let _ = state.record_error(t!("printers.bambulab.errors.credentials").into_owned());
        });
        let _ = shutdown.changed().await;
        return;
    };
    let DetailIo {
        active,
        file_interest,
        requests,
        state: detail_sender,
    } = detail_io;
    drop((file_interest, requests));
    let OperationIo {
        mut actions,
        state: operations,
        mut machine_actions,
    } = operation_io;
    let model = machine::model_for_serial(serial);
    let mut generation = 0_u64;
    let mut retry = RETRY_MIN;
    loop {
        if *shutdown.borrow() {
            break;
        }
        state_sender.send_modify(|state| {
            let _ = state.set_connection(LanConnection::Connecting);
        });
        generation = generation.saturating_add(1);
        let mut projection = machine::Projection::default();
        let mut detail = DetailState::empty(generation);
        let session = Session {
            model,
            state_sender: &state_sender,
            detail_active: &active,
            detail_sender: &detail_sender,
            projection: &mut projection,
            detail: &mut detail,
            actions: &mut actions,
            machine_actions: &mut machine_actions,
            operations: &operations,
        };
        let (error, delivered) = tokio::select! {
            _ = shutdown.changed() => break,
            outcome = session.run(&host, port, serial, access_code) => outcome,
        };
        let rejected = matches!(error, mqtt::Error::Rejected);
        state_sender.send_modify(|state| {
            let connection = if rejected {
                LanConnection::NeedsCredentials
            } else {
                LanConnection::Disconnected
            };
            let _ = state.set_connection(connection);
            state.controls = Default::default();
            let _ = state.record_error(error_message(&error));
        });
        while actions.try_recv().is_ok() {}
        while let Ok(command) = machine_actions.try_recv() {
            let _ = command
                .reply
                .send(ControlResult::Rejected(ControlError::Disconnected));
        }
        let interrupted = match *operations.borrow() {
            OperationState::Sending(kind) => Some(kind),
            _ => None,
        };
        if let Some(kind) = interrupted {
            operations.send_replace(OperationState::Unknown(kind));
        }
        detail.live = false;
        detail.revision = detail.revision.saturating_add(1);
        let _ = detail_sender.send_replace(Arc::new(detail.clone()));
        if rejected {
            // Retrying a refused access code cannot succeed; editing it restarts the driver.
            let _ = shutdown.changed().await;
            break;
        }
        // A session that delivered reports retries fast; repeated connect failures back off.
        retry = if delivered {
            RETRY_MIN
        } else {
            (retry * 2).min(RETRY_MAX)
        };
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(retry) => {}
        }
    }
}

struct Session<'a> {
    model: Option<&'static machine::Model>,
    state_sender: &'a watch::Sender<PrinterState>,
    detail_active: &'a watch::Receiver<bool>,
    detail_sender: &'a watch::Sender<Arc<DetailState>>,
    projection: &'a mut machine::Projection,
    detail: &'a mut DetailState,
    actions: &'a mut mpsc::Receiver<PrinterAction>,
    machine_actions: &'a mut mpsc::Receiver<MachineCommand>,
    operations: &'a watch::Sender<OperationState>,
}

impl Session<'_> {
    /// Returns why the session ended and whether it delivered any report.
    async fn run(
        mut self,
        host: &str,
        port: u16,
        serial: &str,
        access_code: &str,
    ) -> (mqtt::Error, bool) {
        let mut connection = match mqtt::Connection::open(host, port, serial, access_code).await {
            Ok(connection) => connection,
            Err(error) => return (error, false),
        };
        let mut report = mqtt::Report::default();
        let mut delivered = false;
        let mut deadline = TokioInstant::now() + mqtt::STALE_AFTER;
        let mut pending: Option<control::Pending> = None;
        // Never reuse a packet identifier during this session, even after a timeout.
        let mut packet_id = 1_u16;
        loop {
            let wake = pending
                .as_ref()
                .map_or(deadline, |action| deadline.min(action.deadline));
            let ping_at = connection.ping_at();
            let packet = tokio::select! {
                packet = connection.read_packet() => packet,
                () = sleep_until(wake) => {
                    if TokioInstant::now() >= deadline {
                        return (mqtt::Error::Stale, delivered);
                    }
                    if let Some(action) = pending.take() {
                        self.finish_action(action.finish(ControlResult::Unknown));
                    }
                    continue;
                }
                () = sleep_until(ping_at) => {
                    if let Err(error) = connection.ping().await {
                        return (error, delivered);
                    }
                    continue;
                }
                Some(action) = self.actions.recv() => {
                    if pending.is_none() {
                        let Some(id) = packet_id.checked_add(1) else {
                            self.finish_action(OperationState::Rejected {
                                action: action.kind(),
                                message: t!("printers.bambulab.errors.control_unavailable").into_owned(),
                            });
                            return (mqtt::Error::Protocol("packet identifiers exhausted"), delivered);
                        };
                        let command = control::Pending::prepare(
                            &action, &self.state_sender.borrow(), report.value(), id,
                        );
                        if let Some((prepared, payload)) = command {
                            self.operations.send_replace(OperationState::Sending(prepared.kind));
                            packet_id = id;
                            if let Err(error) = connection.send_control(id, &payload).await {
                                return (error, delivered);
                            }
                            pending = Some(prepared);
                        } else {
                            self.finish_action(OperationState::Rejected {
                                action: action.kind(),
                                message: t!("printers.bambulab.errors.control_unavailable").into_owned(),
                            });
                        }
                    }
                    continue;
                }
                Some(command) = self.machine_actions.recv() => {
                    if pending.is_some() {
                        let _ = command.reply.send(ControlResult::Rejected(ControlError::Busy));
                        continue;
                    }
                    let Some(id) = packet_id.checked_add(1) else {
                        let _ = command.reply.send(ControlResult::Rejected(ControlError::SendFailed));
                        return (mqtt::Error::Protocol("packet identifiers exhausted"), delivered);
                    };
                    let prepared = control::Pending::prepare_filament(
                        command, &self.state_sender.borrow(), report.value(), self.detail, id,
                    );
                    if let Some((prepared, payload)) = prepared {
                        self.operations.send_replace(OperationState::Sending(prepared.kind));
                        packet_id = id;
                        if let Err(error) = connection.send_control(id, &payload).await {
                            return (error, delivered);
                        }
                        pending = Some(prepared);
                    }
                    continue;
                }
            };
            let message = match packet {
                Ok(packet) => match connection.message(packet).await {
                    Ok(Some(message)) => message,
                    Ok(None) => continue,
                    Err(error) => return (error, delivered),
                },
                Err(error) => return (error, delivered),
            };
            if pending
                .as_mut()
                .is_some_and(|action| !action.acknowledge(&message))
                && let Some(action) = pending.take()
            {
                self.finish_action(
                    action.finish(ControlResult::Rejected(ControlError::PrinterRejected)),
                );
            }
            let mut candidate = report.clone();
            let ams_reported = message["print"].get("ams_status").is_some();
            match candidate.apply(message) {
                Ok(true) => {}
                Ok(false) => {
                    report = candidate;
                    self.confirm_action(&mut pending, &report);
                    continue;
                }
                Err(error) => return (error, delivered),
            }
            let observation = observe(candidate.value(), self.model);
            let received_at = Instant::now();
            let mut accepted = false;
            self.state_sender.send_modify(|state| {
                if let Err(error) = state.publish_snapshot(observation.clone(), received_at) {
                    let _ = state.record_error(
                        t!(
                            "printers.bambulab.errors.invalid_report",
                            detail = error.to_string()
                        )
                        .into_owned(),
                    );
                } else {
                    state.controls = control::availability(candidate.value());
                    accepted = true;
                }
            });
            if !accepted {
                continue;
            }
            if matches!(*self.operations.borrow(), OperationState::ReadOnly) {
                self.operations.send_replace(OperationState::Idle);
            }
            delivered = true;
            deadline = TokioInstant::now() + mqtt::STALE_AFTER;
            report = candidate;
            if ams_reported && let Some(action) = pending.as_mut() {
                action.observe_ams_status(report.value());
            }
            self.confirm_action(&mut pending, &report);
            self.projection
                .update(report.value(), &observation, self.model, self.detail);
            if let Some((supply, operation)) = pending
                .as_ref()
                .and_then(|action| action.operation(report.value()))
                && let Some(resource) = self
                    .detail
                    .machine
                    .supplies
                    .iter_mut()
                    .find(|resource| resource.id == supply)
            {
                resource.operation = Some(operation);
            }
            self.detail.live = *self.detail_active.borrow();
            self.detail.revision = self.detail.revision.saturating_add(1);
            let _ = self
                .detail_sender
                .send_replace(Arc::new(self.detail.clone()));
        }
    }

    fn finish_action(&mut self, result: OperationState) {
        while self.actions.try_recv().is_ok() {}
        while let Ok(command) = self.machine_actions.try_recv() {
            let _ = command
                .reply
                .send(ControlResult::Rejected(ControlError::Busy));
        }
        self.operations.send_replace(result);
    }

    fn confirm_action(&mut self, pending: &mut Option<control::Pending>, report: &mqtt::Report) {
        let confirmed = pending
            .as_ref()
            .is_some_and(|action| action.confirmed(report.value(), &self.state_sender.borrow()));
        if confirmed && let Some(action) = pending.take() {
            self.finish_action(action.finish(ControlResult::Accepted));
        }
    }
}

fn error_message(error: &mqtt::Error) -> String {
    match error {
        mqtt::Error::Network(detail) => t!("printers.bambulab.errors.network", detail = detail),
        mqtt::Error::Tls(detail) => t!("printers.bambulab.errors.tls", detail = detail),
        mqtt::Error::Rejected => t!("printers.bambulab.errors.rejected"),
        mqtt::Error::Protocol(detail) => t!("printers.bambulab.errors.protocol", detail = detail),
        mqtt::Error::Stale => t!("printers.bambulab.errors.stale"),
    }
    .into_owned()
}

/// Translates the accumulated report. Only data the printer sends is published:
/// elapsed time, filament used, linear velocity and thumbnails stay unknown.
fn observe(report: &Value, model: Option<&machine::Model>) -> PrinterObservation {
    let print = &report["print"];
    let status = status(print);
    let active = matches!(status, Some(PrintStatus::Printing | PrintStatus::Paused));
    let (layer_current, layer_total) = layers(print, status);
    let fan_gear = integer(&print["fan_gear"]);
    PrinterObservation {
        status,
        progress_pct: integer(&print["mc_percent"])
            .filter(|percent| (0..=100).contains(percent))
            .map(|percent| percent as f64),
        filename: filename(print),
        chamber_c: model
            .filter(|model| model.chamber_temperature)
            .and_then(|_| chamber_temperature(print)),
        layer_current,
        layer_total,
        remaining_s: integer(&print["mc_remaining_time"])
            .filter(|minutes| active && *minutes >= 0)
            .and_then(|minutes| minutes.checked_mul(60))
            .map(|seconds| seconds as f64),
        speed_override_pct: speed_override(print),
        alerts: status.map(|_| alerts(print)),
        nozzles: nozzles(print),
        beds: beds(print),
        part_cooling_fans: fan_pct(fan_gear, 0, &print["cooling_fan_speed"])
            .map(|power| {
                vec![PartCoolingFanTelemetry {
                    id: ElementId(1),
                    power_pct: Some(power),
                    rpm: None,
                }]
            })
            .unwrap_or_default(),
        filament_sensors: filament_sensors(print),
        filament_supplies: filament_supplies(print),
        lights: lights(print),
        material_station_available: hex(&print["ams"]["ams_exist_bits"]).map(|bits| bits != 0),
        ..Default::default()
    }
}

fn status(print: &Value) -> Option<PrintStatus> {
    Some(match print["gcode_state"].as_str()?.trim() {
        "IDLE" => PrintStatus::Idle,
        "PREPARE" | "RUNNING" | "SLICING" | "INIT" => PrintStatus::Printing,
        "PAUSE" => PrintStatus::Paused,
        "FINISH" => PrintStatus::Completed,
        "FAILED" if integer(&print["print_error"]) == Some(CANCELLED_BY_USER) => {
            PrintStatus::Cancelled
        }
        "FAILED" => PrintStatus::Error,
        _ => PrintStatus::Unknown,
    })
}

/// `subtask_name` is the job name. An empty one falls back to the file, except the
/// `Metadata/plate_N.gcode` inside a 3MF, which every job shares.
fn filename(print: &Value) -> Option<String> {
    if let Some(name) = text(&print["subtask_name"], 1024) {
        return Some(name);
    }
    let path = print["gcode_file"].as_str()?.trim();
    (!path.is_empty() && !path.contains("Metadata/")).then(|| path.to_owned())
}

/// Same rules as the other integrations: 0/0 outside a print is no data, and a
/// total that cannot hold the current layer is dropped.
fn layers(print: &Value, status: Option<PrintStatus>) -> (Option<u32>, Option<u32>) {
    let layer = |key: &str| integer(&print[key]).and_then(|value| u32::try_from(value).ok());
    match (layer("layer_num"), layer("total_layer_num")) {
        (Some(0), Some(0)) if status != Some(PrintStatus::Printing) => (None, None),
        (Some(current), Some(total)) if total == 0 || current > total => (Some(current), None),
        pair => pair,
    }
}

/// `spd_mag` is the percentage; `spd_lvl` is the preset it comes from.
fn speed_override(print: &Value) -> Option<f64> {
    if let Some(magnitude) = integer(&print["spd_mag"]).filter(|magnitude| *magnitude > 0) {
        return Some(magnitude as f64);
    }
    match integer(&print["spd_lvl"])? {
        1 => Some(50.0),
        2 => Some(100.0),
        3 => Some(124.0),
        4 => Some(166.0),
        _ => None,
    }
}

/// Codes only, never the vendor text: `print_error` as `XXXX_XXXX` and each HMS entry
/// as `HMS_XXXX_XXXX_XXXX_XXXX`, with its severity as the category.
fn alerts(print: &Value) -> Vec<PrinterAlert> {
    let mut alerts = Vec::new();
    if let Some(code) = integer(&print["print_error"])
        .filter(|code| *code != 0 && *code != CANCELLED_BY_USER)
        .and_then(|code| u32::try_from(code).ok())
    {
        alerts.push(PrinterAlert::new(
            "print_error",
            format!("{:04X}_{:04X}", code >> 16, code & 0xFFFF),
        ));
    }
    if let Some(code) = integer(&print["mc_print_error_code"]).filter(|code| *code > 0) {
        alerts.push(PrinterAlert::new("mc_error", code.to_string()));
    }
    for item in print["hms"].as_array().map_or(&[][..], Vec::as_slice) {
        let field = |key: &str| {
            integer(&item[key])
                .filter(|value| *value > 0)
                .and_then(|value| u32::try_from(value).ok())
        };
        let (Some(attr), Some(code)) = (field("attr"), field("code")) else {
            continue;
        };
        let category = match code >> 16 {
            1 => "hms_fatal",
            2 => "hms_error",
            3 => "hms_warning",
            4 => "hms_info",
            _ => "hms_unknown",
        };
        let alert = PrinterAlert::new(
            category,
            format!(
                "HMS_{:04X}_{:04X}_{:04X}_{:04X}",
                attr >> 16,
                attr & 0xFFFF,
                code >> 16,
                code & 0xFFFF
            ),
        );
        if !alerts.contains(&alert) {
            alerts.push(alert);
        }
    }
    alerts.truncate(MAX_ALERTS);
    alerts
}

/// Newer firmware reports each nozzle in `device.extruder`; older firmware has one
/// nozzle in flat fields.
fn nozzles(print: &Value) -> Vec<NozzleTelemetry> {
    let extruder = &print["device"]["extruder"];
    if let Some(entries) = extruder["info"].as_array() {
        let active = integer(&extruder["state"]).map(|state| (state >> 4) & 0x0F);
        return entries
            .iter()
            .filter_map(|entry| {
                let id = extruder_id(entry)?;
                let (current, target) = integer(&entry["temp"]).map_or((None, None), unpack);
                Some(NozzleTelemetry {
                    id: ElementId(id + 1),
                    is_active: active.map_or(id == 0, |active| active == i64::from(id)),
                    nozzle_temp_c: current,
                    nozzle_target_temp_c: target,
                })
            })
            .collect();
    }
    let current = number(&print["nozzle_temper"]);
    let target = number(&print["nozzle_target_temper"]);
    if current.is_none() && target.is_none() {
        return Vec::new();
    }
    vec![NozzleTelemetry {
        id: ElementId(1),
        is_active: true,
        nozzle_temp_c: current,
        nozzle_target_temp_c: target,
    }]
}

fn beds(print: &Value) -> Vec<BedTelemetry> {
    let device = &print["device"];
    let packed = integer(&device["bed_temp"]).or_else(|| integer(&device["bed"]["info"]["temp"]));
    let (current, target) = match packed {
        Some(packed) => unpack(packed),
        None => (
            number(&print["bed_temper"]),
            number(&print["bed_target_temper"]),
        ),
    };
    if current.is_none() && target.is_none() {
        return Vec::new();
    }
    vec![BedTelemetry {
        id: ElementId(1),
        bed_temp_c: current,
        bed_target_temp_c: target,
    }]
}

fn chamber_temperature(print: &Value) -> Option<f64> {
    match integer(&print["device"]["ctc"]["info"]["temp"]) {
        Some(packed) => unpack(packed).0,
        None => number(&print["chamber_temper"]).filter(|temperature| *temperature >= 0.0),
    }
}

/// Packed temperatures: current in the low 16 bits, target in the high 16 bits,
/// `0xFFFF` for no reading.
fn unpack(packed: i64) -> (Option<f64>, Option<f64>) {
    let half = |shift: u32| {
        let value = (packed >> shift) & 0xFFFF;
        (value != 0xFFFF).then_some(value as f64)
    };
    (half(0), half(16))
}

/// `fan_gear` packs the part, auxiliary and chamber fans as 0-255 bytes. Without it,
/// each fan comes as a 0-15 step that Bambu Studio maps to multiples of 10 %.
fn fan_pct(fan_gear: Option<i64>, byte: u32, step: &Value) -> Option<f64> {
    match fan_gear {
        Some(gear) => Some((((gear >> (8 * byte)) & 0xFF) as f64 / 255.0 * 100.0).round()),
        None => integer(step)
            .filter(|step| (0..=15).contains(step))
            .map(|step| (step as f64 / 1.5).floor() * 10.0),
    }
}

fn filament_sensors(print: &Value) -> Vec<FilamentSensorTelemetry> {
    if let Some(entries) = print["device"]["extruder"]["info"].as_array() {
        return entries
            .iter()
            .filter_map(|entry| {
                let id = extruder_id(entry)?;
                let info = integer(&entry["info"])?;
                Some(FilamentSensorTelemetry::new(
                    ElementId(id + 1),
                    Some((info >> 1) & 1 == 1),
                ))
            })
            .collect();
    }
    integer(&print["hw_switch_state"])
        .map(|state| vec![FilamentSensorTelemetry::new(ElementId(1), Some(state == 1))])
        .unwrap_or_default()
}

fn extruder_id(entry: &Value) -> Option<u32> {
    u32::try_from(integer(&entry["id"])?)
        .ok()
        .filter(|id| *id < 16)
}

fn filament_supplies(print: &Value) -> Option<Vec<FilamentSupplyTelemetry>> {
    let ams = &print["ams"];
    let units = ams["ams"].as_array();
    let externals = external_spools(print);
    if units.is_none() && externals.is_empty() {
        return None;
    }
    let exist_bits = hex(&ams["tray_exist_bits"]);
    let active = active_supplies(print);
    let mut supplies = Vec::new();
    for tray in station_trays(print) {
        let present = exist_bits.map(|bits| (bits >> tray.bit) & 1 == 1);
        supplies.push(supply(tray.id, tray.data, present, active.as_deref()));
    }
    for (id, tray) in externals {
        supplies.push(supply(id, tray, None, active.as_deref()));
    }
    Some(supplies)
}

struct StationTray<'a> {
    id: ElementId,
    bit: u32,
    ams_id: i64,
    slot: i64,
    unit: &'a Value,
    data: &'a Value,
}

fn station_trays(print: &Value) -> impl Iterator<Item = StationTray<'_>> {
    print["ams"]["ams"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|unit| {
            unit["tray"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |data| {
                    let ams_id = integer(&unit["id"])?;
                    let slot = integer(&data["id"])?;
                    let (id, bit) = station_slot(ams_id, slot)?;
                    Some(StationTray {
                        id,
                        bit,
                        ams_id,
                        slot,
                        unit,
                        data,
                    })
                })
        })
}

/// The main nozzle's external spool is always 255. Single-nozzle firmware reports it
/// as `vt_tray` with id 254, which Bambu Studio also renames to 255.
fn external_spools(print: &Value) -> Vec<(ElementId, &Value)> {
    if let Some(slots) = print["vir_slot"].as_array() {
        return slots
            .iter()
            .filter_map(|slot| {
                let id = u32::try_from(integer(&slot["id"])?).ok()?;
                is_external(ElementId(id)).then_some((ElementId(id), slot))
            })
            .collect();
    }
    if print["vt_tray"].is_object() {
        return vec![(ElementId(EXTERNAL_MAIN), &print["vt_tray"])];
    }
    Vec::new()
}

fn is_external(id: ElementId) -> bool {
    matches!(id.0, EXTERNAL_SECOND | EXTERNAL_MAIN)
}

/// Supply id and its bit in `tray_exist_bits` for a slot of a material station unit,
/// following Bambu Studio's `DevAms::GetTrayId`.
fn station_slot(unit: i64, slot: i64) -> Option<(ElementId, u32)> {
    let slot = u32::try_from(slot).ok().filter(|slot| *slot < 4)?;
    let unit = u32::try_from(unit).ok()?;
    match unit {
        0..=3 => Some((ElementId(unit * 4 + slot), unit * 4 + slot)),
        128..=135 => Some((ElementId(unit), 16 + (unit - 128) + slot)),
        16 => Some((ElementId(24 + slot), 24 + slot)),
        _ => None,
    }
}

fn supply(
    id: ElementId,
    tray: &Value,
    present: Option<bool>,
    active: Option<&[ElementId]>,
) -> FilamentSupplyTelemetry {
    let material = (present != Some(false))
        .then(|| match tray["tray_info_idx"].as_str() {
            Some("GFS00") => Some("PLA-S".to_owned()),
            Some("GFS01") => Some("PA-S".to_owned()),
            _ => text(&tray["tray_type"], 64).filter(|kind| kind != "Empty"),
        })
        .flatten();
    let color = material
        .as_ref()
        .and_then(|_| color(&tray["tray_color"]).or_else(|| color(&tray["cols"][0])));
    let is_active = active.map(|active| active.contains(&id));
    let state = match (is_active, present) {
        (Some(true), _) => SupplyState::Active,
        (_, Some(false)) => SupplyState::Empty,
        (_, Some(true)) => SupplyState::Available,
        _ => SupplyState::Unknown,
    };
    FilamentSupplyTelemetry {
        id,
        filament_present: present,
        material,
        color,
        is_active,
        state,
    }
}

/// `RRGGBBAA` from the printer to `#RRGGBB`; a fully transparent color is no color.
fn color(value: &Value) -> Option<String> {
    let raw = value.as_str()?.trim();
    if raw.len() != 8 || !raw.chars().all(|ch| ch.is_ascii_hexdigit()) || raw.ends_with("00") {
        return None;
    }
    Some(format!("#{}", raw[..6].to_ascii_uppercase()))
}

/// Supplies feeding a nozzle right now: one per extruder on newer firmware,
/// `tray_now` on older firmware. `None` when the report says nothing about it.
fn active_supplies(print: &Value) -> Option<Vec<ElementId>> {
    if let Some(entries) = print["device"]["extruder"]["info"].as_array() {
        return Some(
            entries
                .iter()
                .filter_map(|entry| loaded_from_snow(integer(&entry["snow"])?))
                .collect(),
        );
    }
    let tray_now = print["ams"]["tray_now"].as_str()?;
    Some(loaded_from_tray_now(tray_now).into_iter().collect())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MaterialHead {
    extruder: Option<u32>,
    loaded: Option<ElementId>,
}

fn material_head(print: &Value) -> Option<MaterialHead> {
    let extruder = &print["device"]["extruder"];
    if let Some(entries) = extruder["info"].as_array() {
        let state = integer(&extruder["state"]).filter(|state| *state >= 0)?;
        let active = ((state >> 4) & 0xf) as u32;
        let mut matching = entries
            .iter()
            .filter(|entry| extruder_id(entry) == Some(active));
        let entry = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        let snow = integer(&entry["snow"]).filter(|snow| (0..=65535).contains(snow))?;
        let loaded = if snow & 0xff == 0xff {
            None
        } else {
            Some(loaded_from_snow(snow)?)
        };
        return Some(MaterialHead {
            extruder: Some(active),
            loaded,
        });
    }
    let tray = integer(&print["ams"]["tray_now"])?;
    let loaded = if tray == 255 {
        None
    } else {
        Some(loaded_from_tray_now(&tray.to_string())?)
    };
    Some(MaterialHead {
        extruder: None,
        loaded,
    })
}

/// `snow` is unit in the high byte and slot in the low byte; `0xFFFF` or slot `0xFF`
/// means nothing loaded.
fn loaded_from_snow(snow: i64) -> Option<ElementId> {
    let unit = (snow >> 8) & 0xFF;
    let slot = snow & 0xFF;
    if snow == 0xFFFF || slot == 0xFF {
        return None;
    }
    let unit = u32::try_from(unit).ok()?;
    if is_external(ElementId(unit)) {
        return Some(ElementId(unit));
    }
    station_slot(i64::from(unit), slot).map(|(id, _)| id)
}

/// Older firmware: 255 is nothing loaded and 254 the external spool.
fn loaded_from_tray_now(tray_now: &str) -> Option<ElementId> {
    match tray_now.trim().parse::<u32>().ok()? {
        EXTERNAL_MAIN => None,
        EXTERNAL_SECOND => Some(ElementId(EXTERNAL_MAIN)),
        unit @ 128..=135 => Some(ElementId(unit)),
        tray @ 0..=15 => Some(ElementId(tray)),
        _ => None,
    }
}

/// Fixed ids keep each light stable when another one appears or disappears.
fn light_node(node: &str) -> Option<(&'static str, ElementId, LightRole)> {
    let (name, id, role) = match node {
        "chamber_light" => ("chamber_light", 1, LightRole::BuildArea),
        "chamber_light2" => ("chamber_light2", 2, LightRole::BuildArea),
        "work_light" => ("work_light", 3, LightRole::Toolhead),
        _ => return None,
    };
    Some((name, ElementId(id), role))
}

fn lights(print: &Value) -> Vec<LightTelemetry> {
    let mut lights: Vec<LightTelemetry> = print["lights_report"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|light| {
            let (_, id, role) = light_node(light["node"].as_str()?)?;
            let enabled = match light["mode"].as_str() {
                Some("on" | "flashing") => Some(true),
                Some("off") => Some(false),
                _ => None,
            };
            Some(LightTelemetry { id, role, enabled })
        })
        .collect();
    lights.truncate(MAX_LIGHTS);
    lights
}

/// The printer sends numbers as JSON numbers or as numeric strings.
fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|number: &f64| number.is_finite())
}

fn hex(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.trim(), 16).ok()
}

fn text(value: &Value, maximum_bytes: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| sanitize_message(text, maximum_bytes))
}
