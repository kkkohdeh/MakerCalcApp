//! FlashForge's translation of the local, manufacturer-independent machine contract.
use super::*;
use crate::shared::control::{
    Axis, CalibrationKind, ControlError, ControlResult, MachineAction, MachineActionKind as K,
    valid_label,
};
use crate::shared::machine::{
    DeviceMetadata, FanRole, MachineActivity, MachineInfo, PrinterIssue, PrinterIssueKind,
    SupplyBlock, SupplyControl, SupplyKind, SupplyOperation, SupplyOperationPhase, SupplyResource,
};
use crate::shared::workspace::{Camera, Device, DeviceKind};

mod filament;
pub(super) mod motors;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Detail {
    pid: Option<serde_json::Value>,
    name: Option<String>,
    door_status: Option<String>,
    nozzle_temps: Option<Vec<f64>>,
    nozzle_target_temps: Option<Vec<f64>>,
    chamber_target_temp: Option<f64>,
    firmware_version: Option<String>,
    location: Option<String>,
    measure: Option<String>,
    nozzle_cnt: Option<u8>,
    nozzle_model: Option<String>,
    mac_addr: Option<String>,
    z_axis_compensation: Option<f64>,
    auto_shutdown: Option<String>,
    auto_shutdown_time: Option<serde_json::Value>,
    camera_stream_url: Option<String>,
    chamber_fan_speed: Option<f64>,
    cooling_fan_left_speed: Option<f64>,
    internal_fan_status: Option<String>,
    external_fan_status: Option<String>,
    indep_matl_info: Option<IndependentSupply>,
    coordinate: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndependentSupply {
    state_action: Option<u32>,
    material_name: Option<String>,
    material_color: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct PollDetails {
    pub(super) totals: Option<HistoryTotals>,
    pub(super) detail: FlashforgeDetail,
    pub(super) observation: PrinterObservation,
    pub(super) observed_at: Instant,
    pub(super) connection_epoch: u64,
}
impl Default for PollDetails {
    fn default() -> Self {
        Self {
            totals: None,
            detail: Default::default(),
            observation: Default::default(),
            observed_at: Instant::now(),
            connection_epoch: 0,
        }
    }
}

impl PollDetails {
    fn motion_idle(&self) -> Option<bool> {
        if !matches!(self.detail.status.as_deref()?, "ready" | "idle")
            || matches!(
                self.observation.status,
                Some(PrintStatus::Printing | PrintStatus::Paused)
            )
        {
            return Some(false);
        }
        if product(&self.detail.machine) != Some(38) {
            return Some(true);
        }
        match self.observation.material_station_available? {
            true => self.detail.matl_station_info.as_ref()?.state_action,
            false => self.detail.machine.indep_matl_info.as_ref()?.state_action,
        }
        .map(|action| action == 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Motion,
    Nozzle,
    Tool(u8),
    ChamberHeater,
    PartFan,
    LeftPartFan,
    ChamberFan,
    InternalFan,
    ExternalFan,
    Camera,
    Slot(u32),
    ExternalSupply,
    Alert(String),
}

#[derive(Default)]
pub(super) struct Projection {
    targets: BTreeMap<ResourceId, Target>,
    epoch: Option<u64>,
    product: Option<u32>,
    observed_at: Option<Instant>,
    status: Option<String>,
    auto_shutdown_minutes: Option<u64>,
    independent_action: Option<u32>,
    station_action: Option<u32>,
    station_step: Option<u32>,
    dispatched_at: Option<Instant>,
    station_operation: Option<(u32, K)>,
    current_slot: Option<u32>,
    pending_load: Option<filament::PendingLoad>,
    printing: bool,
    motor_tcp_verified: Option<bool>,
}

fn label(value: Option<&str>, limit: usize) -> Option<String> {
    value
        .filter(|value| valid_label(value, limit))
        .map(str::to_owned)
}
fn percent(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
}
fn switch(value: Option<&str>) -> Option<bool> {
    match value {
        Some("open") => Some(true),
        Some("close") => Some(false),
        _ => None,
    }
}
pub(super) fn product(detail: &Detail) -> Option<u32> {
    match detail.pid.as_ref()? {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        serde_json::Value::String(s) if s.len() <= 8 => u32::from_str_radix(s, 16).ok(),
        _ => None,
    }
}
fn volume(value: Option<&str>) -> Option<[f64; 3]> {
    let mut parts = value.filter(|v| v.len() <= 64)?.split(['X', 'x']);
    let mut result = [0.0; 3];
    for dimension in &mut result {
        *dimension = parts.next()?.trim().parse::<f64>().ok()?;
        if !dimension.is_finite() || !(1.0..=10_000.0).contains(dimension) {
            return None;
        }
    }
    parts.next().is_none().then_some(result)
}

fn activity(status: Option<&str>) -> Option<MachineActivity> {
    Some(match status?.trim().to_ascii_lowercase().as_str() {
        "heating" => MachineActivity::Heating,
        "pausing" => MachineActivity::Pausing,
        "canceling" => MachineActivity::Stopping,
        "calibrate_doing" => MachineActivity::Calibrating,
        "downloading" => MachineActivity::ReceivingFile,
        "cloud_slicing" => MachineActivity::Slicing,
        "sending" => MachineActivity::TransferringFile,
        "unzipping" => MachineActivity::ExtractingFile,
        "busy" => MachineActivity::Busy,
        _ => return None,
    })
}

fn issue(code: Option<&str>) -> Option<PrinterIssue> {
    use PrinterIssueKind as I;
    let (kind, slot) = match code? {
        "E0100" => (I::FilamentFeedTimeout, Some(1)),
        "E0101" => (I::FilamentFeedTimeout, Some(2)),
        "E0102" => (I::FilamentFeedTimeout, Some(3)),
        "E0103" => (I::FilamentFeedTimeout, Some(4)),
        "E0104" => (I::FilamentRetractTimeout, Some(1)),
        "E0105" => (I::FilamentRetractTimeout, Some(2)),
        "E0106" => (I::FilamentRetractTimeout, Some(3)),
        "E0107" => (I::FilamentRetractTimeout, Some(4)),
        "E0108" => (I::FilamentFeedFailed, None),
        "E0109" => (I::FilamentJam, None),
        "E0114" => (I::MaterialStationHomingFailed, None),
        _ => return None,
    };
    Some(PrinterIssue {
        kind,
        supply_id: slot.map(ElementId),
    })
}

impl Projection {
    pub(super) fn validate_motor_poll(
        &self,
        poll: &PollDetails,
        epoch: u64,
        submitted_at: Instant,
    ) -> Result<(), ControlError> {
        if self.epoch != Some(epoch)
            || poll.connection_epoch != epoch
            || self.product != product(&poll.detail.machine)
            || poll.observed_at.elapsed() > Duration::from_secs(15)
            || submitted_at.elapsed() > Duration::from_secs(5)
        {
            return Err(ControlError::StaleResource);
        }
        (poll.motion_idle() == Some(true))
            .then_some(())
            .ok_or(ControlError::InvalidState)
    }

    pub(super) fn needs_motor_probe(&self) -> bool {
        self.motor_tcp_verified.is_none() && matches!(self.product, Some(35 | 36 | 38))
    }

    pub(super) fn set_motor_support(&mut self, verified: bool, state: &mut DetailState) {
        self.motor_tcp_verified = Some(verified);
        if verified {
            state.machine.actions.push(K::DisableMotors);
        }
    }
    pub(super) fn http_only(&self) -> bool {
        matches!(self.product, Some(40 | 41))
    }
    fn resource(&mut self, target: Target) -> ResourceId {
        if let Some((id, _)) = self
            .targets
            .iter()
            .find(|(_, existing)| **existing == target)
        {
            return *id;
        }
        let id = ResourceId(NEXT_FILE_RESOURCE.fetch_add(1, Ordering::Relaxed));
        self.targets.insert(id, target);
        id
    }

    pub(super) fn invalidate(&mut self) {
        *self = Self::default();
    }

    pub(super) fn dispatched(&mut self, at: Instant) {
        self.dispatched_at = Some(at);
    }

    pub(super) fn waiting_for_observation(&self) -> bool {
        self.dispatched_at
            .is_some_and(|sent| self.observed_at.is_none_or(|at| at <= sent))
    }

    pub(super) fn update(
        &mut self,
        poll: &PollDetails,
        epoch: u64,
        host: &str,
        state: &mut DetailState,
    ) {
        if self.epoch != Some(epoch) {
            self.invalidate();
            self.epoch = Some(epoch);
        }
        if self.observed_at.is_some_and(|at| {
            poll.observed_at.saturating_duration_since(at) > Duration::from_secs(15)
        }) {
            self.station_operation = None;
            self.abort_load(ControlResult::Unknown);
        }
        self.observed_at = Some(poll.observed_at);
        let detail = &poll.detail;
        let extra = &detail.machine;
        self.product = product(extra);
        self.status = detail.status.clone();
        self.auto_shutdown_minutes = extra
            .auto_shutdown_time
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .filter(|minutes| (1..=1440).contains(minutes));
        self.printing = matches!(
            poll.observation.status,
            Some(PrintStatus::Printing | PrintStatus::Paused)
        );
        let station = detail
            .matl_station_info
            .as_ref()
            .filter(|_| poll.observation.material_station_available != Some(false));
        let independent = extra
            .indep_matl_info
            .as_ref()
            .filter(|_| poll.observation.material_station_available == Some(false));
        self.independent_action = independent.and_then(|s| s.state_action);
        self.station_action = station.and_then(|s| s.state_action);
        self.station_step = station.and_then(|s| s.state_step);
        self.current_slot = station.and_then(|s| s.current_slot);
        let station_operation = self.observe_station_operation(station);
        let known = matches!(self.product, Some(35 | 36 | 38 | 40 | 41));
        let device = DeviceMetadata {
            reported_name: label(extra.name.as_deref(), 128),
            manufacturer: known.then(|| "FlashForge".into()),
            model: match self.product {
                Some(35) => Some("Adventurer 5M".into()),
                Some(36) => Some("Adventurer 5M Pro".into()),
                Some(38) => Some("AD5X".into()),
                Some(40) => Some("Creator 5".into()),
                Some(41) => Some("Creator 5 Pro".into()),
                _ => None,
            },
            firmware_version: label(extra.firmware_version.as_deref(), 128),
            location_label: label(extra.location.as_deref(), 128),
            build_volume_mm: volume(extra.measure.as_deref()),
            nozzle_count: extra.nozzle_cnt.filter(|n| (1..=16).contains(n)),
            mac_address: extra
                .mac_addr
                .as_ref()
                .filter(|value| {
                    value.len() == 17
                        && value.split(':').count() == 6
                        && value.split(':').all(|part| {
                            part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                })
                .cloned(),
        };
        let mut info = MachineInfo {
            device,
            motion_idle: poll
                .motion_idle()
                .map(|idle| idle && self.pending_load.is_none()),
            z_offset_mm: extra
                .z_axis_compensation
                .filter(|v| v.is_finite() && (-5.0..=5.0).contains(v)),
            auto_shutdown_enabled: switch(extra.auto_shutdown.as_deref()),
            activity: activity(detail.status.as_deref()),
            material_station_slots: detail
                .matl_station_info
                .as_ref()
                .and_then(|station| station.slot_cnt)
                .and_then(|count| u8::try_from(count).ok())
                .filter(|count| (1..=16).contains(count)),
            enclosure_closed: (self.product == Some(41))
                .then_some(match extra.door_status.as_deref() {
                    Some("close") => Some(true),
                    Some("open") => Some(false),
                    _ => None,
                })
                .flatten(),
            issues: issue(detail.error_code.as_deref()).into_iter().collect(),
            ..Default::default()
        };
        if self.motor_tcp_verified == Some(true) && matches!(self.product, Some(35 | 36 | 38)) {
            info.actions.push(K::DisableMotors);
        }
        if known {
            info.z_offset_range_mm = Some([-5.0, 5.0]);
            info.calibrations = vec![
                CalibrationKind::BedLeveling,
                CalibrationKind::VibrationCompensation,
            ];
            info.print_options
                .push(crate::shared::workspace::PrintOptionKind::BedLeveling);
            if self.product == Some(38) {
                use crate::shared::workspace::PrintOptionKind as P;
                info.print_options.extend([
                    P::FirstLayerInspection,
                    P::FlowCalibration,
                    P::Timelapse,
                ]);
            }
            info.actions.extend([
                K::SetZOffset,
                K::ConfirmBuildPlateCleared,
                K::RunCalibration,
                K::SetAutoShutdown,
                K::SetDeviceName,
            ]);
        }
        if matches!(self.product, Some(35 | 36 | 38)) {
            info.speed_override_range_pct =
                Some([50, if self.product == Some(38) { 500 } else { 150 }]);
            info.actions.push(K::SetPrintSpeedOverride);
        }
        state.devices.clear();
        if self.product == Some(38)
            && let Some(position) = reported_position(extra.coordinate.as_ref())
        {
            let mut motion = Device::new(
                self.resource(Target::Motion),
                DeviceKind::Motion,
                "Motion".into(),
            );
            motion.position_mm = Some(position);
            state.devices.push(motion);
        }
        state.temperature_targets.clear();
        if matches!(self.product, Some(35 | 36 | 38)) {
            if extra.nozzle_cnt == Some(1) && !poll.observation.nozzles.is_empty() {
                state.temperature_targets.push(super::HeaterKind::Nozzle);
            }
            if !poll.observation.beds.is_empty() {
                state.temperature_targets.push(super::HeaterKind::Bed);
            }
        }
        if matches!(self.product, Some(40 | 41)) {
            for index in 0..4 {
                let current = extra
                    .nozzle_temps
                    .as_ref()
                    .filter(|v| v.len() == 4)
                    .and_then(|v| v.get(index))
                    .copied()
                    .filter(|v| v.is_finite() && *v >= 0.0);
                let target = extra
                    .nozzle_target_temps
                    .as_ref()
                    .filter(|v| v.len() == 4)
                    .and_then(|v| v.get(index))
                    .copied()
                    .filter(|v| v.is_finite() && *v >= 0.0);
                if current.is_some() || target.is_some() {
                    let mut nozzle = Device::new(
                        self.resource(Target::Tool(index as u8)),
                        DeviceKind::Nozzle,
                        format!("T{}", index + 1),
                    );
                    nozzle.temperature_c = current;
                    nozzle.target_temperature_c = target;
                    state.devices.push(nozzle);
                }
            }
            if self.product == Some(41) {
                let mut chamber = Device::new(
                    self.resource(Target::ChamberHeater),
                    DeviceKind::ChamberHeater,
                    "Chamber".into(),
                );
                chamber.temperature_c = detail.chamber_temp.filter(|v| v.is_finite() && *v >= 0.0);
                chamber.target_temperature_c = extra
                    .chamber_target_temp
                    .filter(|v| v.is_finite() && *v >= 0.0);
                if chamber.temperature_c.is_some() || chamber.target_temperature_c.is_some() {
                    state.devices.push(chamber);
                }
            }
        }
        if known && extra.nozzle_cnt == Some(1) {
            let mut nozzle = Device::new(
                self.resource(Target::Nozzle),
                DeviceKind::Nozzle,
                "Nozzle".into(),
            );
            nozzle.telemetry_id = poll
                .observation
                .nozzles
                .first()
                .filter(|_| poll.observation.nozzles.len() == 1)
                .map(|observed| observed.id);
            nozzle.temperature_c = poll
                .observation
                .nozzles
                .first()
                .and_then(|n| n.nozzle_temp_c);
            nozzle.target_temperature_c = poll
                .observation
                .nozzles
                .first()
                .and_then(|n| n.nozzle_target_temp_c);
            nozzle.nozzle_diameter_mm = extra
                .nozzle_model
                .as_deref()
                .and_then(|s| s.strip_suffix("mm"))
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && (0.1..=2.0).contains(v));
            state.devices.push(nozzle);
        }
        if known && detail.cooling_fan_speed.is_some() {
            self.fan(
                state,
                Target::PartFan,
                FanRole::PartCooling,
                percent(detail.cooling_fan_speed),
                None,
            );
            if matches!(self.product, Some(35 | 36 | 38)) {
                info.actions.push(K::SetFanPower);
            }
        }
        if self.product == Some(38) && extra.cooling_fan_left_speed.is_some() {
            // Distinct observed fan; do not claim an unverified control parameter.
            self.fan(
                state,
                Target::LeftPartFan,
                FanRole::PartCooling,
                percent(extra.cooling_fan_left_speed),
                None,
            );
        }
        // Models without filtration hardware do not have these independent fan resources.
        if self.product == Some(36) {
            if extra.chamber_fan_speed.is_some() {
                self.fan(
                    state,
                    Target::ChamberFan,
                    FanRole::Chamber,
                    percent(extra.chamber_fan_speed),
                    None,
                );
                if !info.actions.contains(&K::SetFanPower) {
                    info.actions.push(K::SetFanPower);
                }
            }
            for (raw, target, role) in [
                (
                    extra.internal_fan_status.as_deref(),
                    Target::InternalFan,
                    FanRole::Recirculation,
                ),
                (
                    extra.external_fan_status.as_deref(),
                    Target::ExternalFan,
                    FanRole::Exhaust,
                ),
            ] {
                if let Some(enabled) = switch(raw) {
                    self.fan(state, target, role, None, Some(enabled));
                }
            }
            if state
                .devices
                .iter()
                .any(|d| matches!(d.fan_role, Some(FanRole::Exhaust | FanRole::Recirculation)))
            {
                info.actions.push(K::SetFanEnabled);
            }
        }
        let camera = extra.camera_stream_url.as_deref();
        let camera_valid = camera
            .and_then(|s| url::Url::parse(s).ok())
            .is_some_and(|u| {
                u.scheme() == "http"
                    && u.host_str() == Some(host.trim_matches(['[', ']']))
                    && u.port() == Some(8080)
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.path() == "/"
                    && u.query() == Some("action=stream")
                    && u.fragment().is_none()
            });
        let camera_known =
            self.product == Some(36) || self.targets.values().any(|t| *t == Target::Camera);
        let mut cameras = Vec::new();
        if known && (camera_valid || (camera == Some("") && camera_known)) {
            cameras.push(Camera {
                id: self.resource(Target::Camera),
                display_name: "Camera".into(),
                enabled: Some(camera_valid),
                rotation_degrees: None,
                flip_horizontal: None,
                flip_vertical: None,
                target_fps: None,
            });
            if matches!(self.product, Some(35 | 36 | 38)) {
                info.actions.push(K::SetCameraStreamEnabled);
            }
            state.capabilities.cameras = Support::Available;
        } else {
            state.capabilities.cameras = Support::Unknown;
        }
        state.cameras = ReadResult {
            status: if cameras.is_empty() {
                ReadStatus::NotLoaded
            } else {
                ReadStatus::Ready
            },
            value: Some(cameras),
            error: None,
        };
        if self.product == Some(38) {
            info.actions.extend([
                K::HomeAxes,
                K::MoveAxisRelative,
                K::ExtrudeFilament,
                K::AcknowledgePrinterAlert,
            ]);
            for reading in poll.observation.filament_supplies.iter().flatten() {
                if station.is_some_and(|station| {
                    station
                        .slot_infos
                        .iter()
                        .any(|s| s.slot_id == Some(reading.id.0))
                }) {
                    info.supplies.push(SupplyResource {
                        id: self.resource(Target::Slot(reading.id.0)),
                        kind: SupplyKind::StationSlot,
                        reading: reading.clone(),
                        controls: None,
                        operation: station_operation
                            .filter(|(slot, _)| *slot == reading.id.0)
                            .map(|(_, operation)| operation),
                        valid_until: Some(poll.observed_at + Duration::from_secs(15)),
                    });
                }
            }
            if let Some(supply) = independent {
                let state = match supply.state_action {
                    Some(1) => SupplyState::Loading,
                    Some(2) => SupplyState::Unloading,
                    _ => SupplyState::Unknown,
                };
                info.supplies.push(SupplyResource {
                    id: self.resource(Target::ExternalSupply),
                    kind: SupplyKind::ExternalSpool,
                    reading: FilamentSupplyTelemetry {
                        id: ElementId(1),
                        material: label(supply.material_name.as_deref(), 64),
                        color: valid_color(supply.material_color.as_deref()),
                        state,
                        ..Default::default()
                    },
                    controls: None,
                    operation: match supply.state_action {
                        Some(1 | 2) => Some(SupplyOperation {
                            step: None,
                            action: if supply.state_action == Some(1) {
                                K::LoadFilament
                            } else {
                                K::UnloadFilament
                            },
                            phase: SupplyOperationPhase::Running,
                        }),
                        Some(3) => Some(SupplyOperation {
                            step: None,
                            action: K::CancelFilamentOperation,
                            phase: SupplyOperationPhase::Cancelling,
                        }),
                        _ => None,
                    },
                    valid_until: Some(poll.observed_at + Duration::from_secs(15)),
                });
            }
            if !info.supplies.is_empty() {
                info.actions.extend([
                    K::ConfigureFilamentSupply,
                    K::LoadFilament,
                    K::UnloadFilament,
                    K::CancelFilamentOperation,
                ]);
            }
            if let Some(error) = detail
                .error_code
                .as_deref()
                .filter(|e| valid_label(e, 64) && *e != "0")
            {
                info.alerts
                    .push(self.resource(Target::Alert(error.to_owned())));
            }
        }
        info.current_supply = info
            .supplies
            .iter()
            .find(|supply| {
                supply.kind == SupplyKind::StationSlot && supply.reading.is_active == Some(true)
            })
            .map(|supply| supply.id);
        if self.current_slot.is_some_and(|slot| slot != 0) && info.current_supply.is_none() {
            self.current_slot = None;
        }
        for supply in &mut info.supplies {
            supply.controls = Some(
                [
                    K::LoadFilament,
                    K::UnloadFilament,
                    K::ConfigureFilamentSupply,
                    K::CancelFilamentOperation,
                ]
                .into_iter()
                .map(|action| SupplyControl {
                    action,
                    blocked: self.supply_block(supply, action),
                })
                .collect(),
            );
        }
        if self.station_operation.is_some_and(|(slot, _)| {
            !info
                .supplies
                .iter()
                .any(|supply| supply.kind == SupplyKind::StationSlot && supply.reading.id.0 == slot)
        }) {
            self.station_operation = None;
        }
        // Drop disappeared targets; their opaque handles must never be revived.
        self.targets.retain(|id, _| {
            state.devices.iter().any(|d| d.id == *id)
                || state.cameras.value.iter().flatten().any(|c| c.id == *id)
                || info.supplies.iter().any(|s| s.id == *id)
                || info.alerts.contains(id)
        });
        for device in &mut state.devices {
            device.actions = info
                .actions
                .iter()
                .copied()
                .filter(|kind| {
                    matches!(
                        (self.targets.get(&device.id), kind),
                        (Some(Target::Nozzle), K::ExtrudeFilament)
                            | (Some(Target::PartFan | Target::ChamberFan), K::SetFanPower)
                            | (
                                Some(Target::InternalFan | Target::ExternalFan),
                                K::SetFanEnabled
                            )
                    )
                })
                .collect();
            device.output_control_range_pct =
                device.actions.contains(&K::SetFanPower).then_some([0, 100]);
        }
        if let Some(version) = info.device.firmware_version.clone() {
            info.software.push(crate::shared::machine::SoftwareVersion {
                role: crate::shared::machine::SoftwareRole::PrinterFirmware,
                version,
            });
        }
        state.machine = info;
    }

    fn observe_station_operation(
        &mut self,
        station: Option<&FlashforgeMaterialStationInfo>,
    ) -> Option<(u32, SupplyOperation)> {
        let Some(station) = station else {
            self.station_operation = None;
            return None;
        };
        if let Some(action @ (1 | 2)) = station.state_action {
            self.station_operation =
                station
                    .current_load_slot
                    .filter(|slot| *slot > 0)
                    .map(|slot| {
                        (
                            slot,
                            if action == 1 {
                                K::LoadFilament
                            } else {
                                K::UnloadFilament
                            },
                        )
                    });
        }
        let (slot, action) = self.station_operation?;
        let phase = match station.state_action {
            Some(1 | 2) => SupplyOperationPhase::Running,
            Some(3)
                if station
                    .current_load_slot
                    .is_none_or(|target| target == 0 || target == slot) =>
            {
                SupplyOperationPhase::Cancelling
            }
            Some(0)
                if station.state_step == Some(6)
                    && matches!(self.status.as_deref(), Some("ready" | "idle")) =>
            {
                SupplyOperationPhase::Completed
            }
            _ => {
                self.station_operation = None;
                return None;
            }
        };
        let step = match (action, station.state_step) {
            (K::LoadFilament, Some(2 | 3)) => station.state_step.map(|step| (step - 1) as u8),
            (K::UnloadFilament, Some(4 | 5)) => station.state_step.map(|step| (step - 3) as u8),
            _ => None,
        };
        Some((
            slot,
            SupplyOperation {
                action,
                phase,
                step,
            },
        ))
    }

    fn supply_block(&self, supply: &SupplyResource, action: K) -> Option<SupplyBlock> {
        if self.waiting_for_observation() {
            return Some(SupplyBlock::Busy);
        }
        if self.printing {
            return Some(SupplyBlock::NotIdle);
        }
        if self.pending_load.is_some() && action != K::CancelFilamentOperation {
            return Some(SupplyBlock::Busy);
        }
        if action == K::CancelFilamentOperation {
            return match supply.operation.map(|op| op.phase) {
                Some(SupplyOperationPhase::Running) => None,
                Some(SupplyOperationPhase::Cancelling) => Some(SupplyBlock::Busy),
                _ if matches!(
                    (supply.kind, self.station_action, self.independent_action),
                    (SupplyKind::StationSlot, Some(0), _) | (SupplyKind::ExternalSpool, _, Some(0))
                ) =>
                {
                    Some(SupplyBlock::NoOperation)
                }
                _ => Some(SupplyBlock::Unknown),
            };
        }
        if !matches!(self.status.as_deref(), Some("ready" | "idle")) {
            return Some(SupplyBlock::NotIdle);
        }
        if matches!(self.station_action, Some(1..=3))
            || matches!(self.independent_action, Some(1..=3))
        {
            return Some(SupplyBlock::Busy);
        }
        if !matches!(
            (supply.kind, self.station_action, self.independent_action),
            (SupplyKind::StationSlot, Some(0), _) | (SupplyKind::ExternalSpool, _, Some(0))
        ) {
            return Some(SupplyBlock::Unknown);
        }
        if action == K::ConfigureFilamentSupply {
            return None;
        }
        if supply.kind == SupplyKind::StationSlot {
            let Some(current) = self.current_slot else {
                return Some(SupplyBlock::Unknown);
            };
            if action == K::UnloadFilament && current != supply.reading.id.0 {
                return Some(SupplyBlock::NotCurrent);
            }
        }
        match supply.kind {
            SupplyKind::StationSlot => match supply.reading.filament_present {
                Some(true) if self.station_action == Some(0) => None,
                Some(false) => Some(SupplyBlock::NoFilament),
                _ => Some(SupplyBlock::Unknown),
            },
            SupplyKind::ExternalSpool if self.independent_action == Some(0) => None,
            _ => Some(SupplyBlock::Unknown),
        }
    }

    fn fan(
        &mut self,
        state: &mut DetailState,
        target: Target,
        role: FanRole,
        power: Option<f64>,
        enabled: Option<bool>,
    ) {
        let mut fan = Device::new(self.resource(target), DeviceKind::Fan, "Fan".into());
        fan.fan_role = Some(role);
        fan.output_pct = power;
        fan.enabled = enabled.or_else(|| power.map(|p| p > 0.0));
        state.devices.push(fan);
    }

    /// Validate current physical state and translate one intention to one command.
    pub(super) fn validate_control(
        &self,
        action: &MachineAction,
        state: &PrinterState,
        detail: &DetailState,
        epoch: u64,
    ) -> Result<(), ControlError> {
        action.validate()?;
        if state.connection != LanConnection::Connected || !state.telemetry_ready {
            return Err(ControlError::Disconnected);
        }
        if self.epoch != Some(epoch)
            || state.connection_epoch != epoch
            || self
                .observed_at
                .is_none_or(|at| at.elapsed() > Duration::from_secs(15))
            || state
                .last_valid_at
                .is_none_or(|at| at.elapsed() > Duration::from_secs(15))
        {
            return Err(ControlError::StaleResource);
        }
        if !detail.machine.actions.contains(&action.kind()) {
            return Err(ControlError::Unsupported);
        }
        if self.waiting_for_observation() {
            return Err(ControlError::Busy);
        }
        if self.pending_load.is_some() && action.kind() != K::CancelFilamentOperation {
            return Err(ControlError::Busy);
        }
        if let MachineAction::LoadFilament { supply_id }
        | MachineAction::UnloadFilament { supply_id }
        | MachineAction::ConfigureFilamentSupply { supply_id, .. }
        | MachineAction::CancelFilamentOperation { supply_id } = action
        {
            if matches!(
                state.telemetry.print_state,
                PrintStatus::Printing | PrintStatus::Paused
            ) {
                return Err(ControlError::InvalidState);
            }
            detail
                .machine
                .supplies
                .iter()
                .find(|supply| supply.id == *supply_id)
                .ok_or(ControlError::StaleResource)?
                .availability(action.kind(), Instant::now())
                .map_err(SupplyBlock::error)?;
        }
        let idle = matches!(self.status.as_deref(), Some("ready" | "idle"));
        let material_busy = matches!(self.station_action, Some(1..=3))
            || matches!(self.independent_action, Some(1..=3));
        let needs_idle = matches!(
            action,
            MachineAction::DisableMotors
                | MachineAction::HomeAxes { .. }
                | MachineAction::MoveAxisRelative { .. }
                | MachineAction::ExtrudeFilament { .. }
                | MachineAction::RunCalibration { .. }
                | MachineAction::SetAutoShutdown { .. }
                | MachineAction::SetDeviceName { .. }
        );
        if needs_idle && (!idle || material_busy) {
            return Err(ControlError::InvalidState);
        }
        if matches!(action, MachineAction::DisableMotors)
            && (!detail.live
                || !matches!(
                    state.telemetry.print_state,
                    PrintStatus::Idle | PrintStatus::Completed | PrintStatus::Cancelled
                )
                || detail.machine.motion_idle != Some(true)
                || detail.machine.activity.is_some())
        {
            return Err(ControlError::InvalidState);
        }
        Ok(())
    }

    pub(super) fn payload(
        &self,
        action: &MachineAction,
        state: &PrinterState,
        detail: &DetailState,
        epoch: u64,
    ) -> Result<serde_json::Value, ControlError> {
        self.validate_control(action, state, detail, epoch)?;
        let printing = matches!(
            state.telemetry.print_state,
            PrintStatus::Printing | PrintStatus::Paused
        );
        let (cmd, args) = match action {
            MachineAction::DisableMotors => return Err(ControlError::Unsupported),
            MachineAction::HomeAxes { axes } => {
                if axes.len() != 3 {
                    return Err(ControlError::Unsupported);
                }
                ("homingCtrl_cmd", json!({}))
            }
            MachineAction::MoveAxisRelative {
                axis,
                distance_mm,
                speed_mm_s,
            } => {
                if speed_mm_s.is_some() {
                    return Err(ControlError::Unsupported);
                }
                // Relative distance is bounded; an ACK supplies no evidence of homing or position.
                let axis = match axis {
                    Axis::X => "x",
                    Axis::Y => "y",
                    Axis::Z => "z",
                };
                ("moveCtrl_cmd", json!({"axis":axis,"delta":distance_mm}))
            }
            MachineAction::ExtrudeFilament {
                nozzle_id,
                length_mm,
            } => {
                self.target(*nozzle_id, &Target::Nozzle)?;
                let hot = detail
                    .devices
                    .iter()
                    .find(|d| d.id == *nozzle_id)
                    .and_then(|d| d.temperature_c)
                    .is_some_and(|c| (170.0..=300.0).contains(&c));
                if !hot {
                    return Err(ControlError::InvalidState);
                }
                ("extrudeCtrl_cmd", json!({"axis":"e","delta":length_mm}))
            }
            MachineAction::SetPrintSpeedOverride { percent } => {
                let maximum = if self.product == Some(38) { 500 } else { 150 };
                if !(50..=maximum).contains(percent) {
                    return Err(ControlError::InvalidData);
                }
                if !printing {
                    return Err(ControlError::InvalidState);
                }
                ("printerCtl_cmd", json!({"speed":percent}))
            }
            MachineAction::SetZOffset { offset_mm } => {
                if offset_mm.abs() > 5.0 {
                    return Err(ControlError::InvalidData);
                }
                if !printing {
                    return Err(ControlError::InvalidState);
                }
                ("printerCtl_cmd", json!({"zAxisCompensation":offset_mm}))
            }
            MachineAction::SetFanPower { fan_id, percent } => {
                if !printing {
                    return Err(ControlError::InvalidState);
                }
                let key = match self.targets.get(fan_id) {
                    Some(Target::PartFan) => "coolingFan",
                    Some(Target::ChamberFan) => "chamberFan",
                    Some(_) => return Err(ControlError::Unsupported),
                    None => return Err(ControlError::StaleResource),
                };
                ("printerCtl_cmd", json!({key:percent}))
            }
            MachineAction::SetFanEnabled { fan_id, enabled } => {
                let (key, other) = match self.targets.get(fan_id) {
                    Some(Target::InternalFan) => ("internal", Target::ExternalFan),
                    Some(Target::ExternalFan) => ("external", Target::InternalFan),
                    Some(_) => return Err(ControlError::Unsupported),
                    None => return Err(ControlError::StaleResource),
                };
                // Both fields are sent by firmware clients; preserve the other fan's observed switch.
                let other_id = self
                    .targets
                    .iter()
                    .find(|(_, t)| **t == other)
                    .map(|(id, _)| *id)
                    .ok_or(ControlError::InvalidState)?;
                let other_enabled = detail
                    .devices
                    .iter()
                    .find(|d| d.id == other_id)
                    .and_then(|d| d.enabled)
                    .ok_or(ControlError::InvalidState)?
                    && !*enabled;
                let other_key = if key == "internal" {
                    "external"
                } else {
                    "internal"
                };
                (
                    "circulateCtl_cmd",
                    json!({key:open(*enabled), other_key:open(other_enabled)}),
                )
            }
            MachineAction::SetCameraStreamEnabled { camera_id, enabled } => {
                self.target(*camera_id, &Target::Camera)?;
                ("streamCtrl_cmd", json!({"action":open(*enabled)}))
            }
            MachineAction::ConfirmBuildPlateCleared => {
                if !matches!(
                    self.status.as_deref(),
                    Some("completed" | "cancelled" | "cancel")
                ) {
                    return Err(ControlError::InvalidState);
                }
                ("stateCtrl_cmd", json!({"action":"setClearPlatform"}))
            }
            MachineAction::RunCalibration { kind } => (
                "calibration_cmd",
                json!({"levelingDetection":open(*kind == CalibrationKind::BedLeveling),"vibrationCompensation":open(*kind == CalibrationKind::VibrationCompensation)}),
            ),
            MachineAction::SetAutoShutdown { enabled, delay_s } => {
                let minutes = if *enabled {
                    if !delay_s.is_multiple_of(60) {
                        return Err(ControlError::InvalidData);
                    }
                    u64::from(delay_s / 60)
                } else {
                    // Keep the reported delay in the firmware's two-field command.
                    self.auto_shutdown_minutes.unwrap_or(30)
                };
                (
                    "delayClose_cmd",
                    json!({"automaticShutdown":open(*enabled),"shutdownAfterTime":minutes}),
                )
            }
            MachineAction::SetDeviceName { name } => ("reName_cmd", json!({"name":name})),
            MachineAction::SetDeviceProfileName { .. } => return Err(ControlError::Unsupported),
            MachineAction::SetFlowOverride { .. }
            | MachineAction::SetPressureAdvance { .. }
            | MachineAction::SetRetraction { .. }
            | MachineAction::DeleteFile { .. }
            | MachineAction::RenameFile { .. }
            | MachineAction::EnqueuePrint { .. }
            | MachineAction::RemoveQueuedPrint { .. }
            | MachineAction::SetPrintQueuePaused { .. }
            | MachineAction::ExcludePrintObject { .. }
            | MachineAction::SetPowerEnabled { .. }
            | MachineAction::SetLightBrightness { .. } => return Err(ControlError::Unsupported),
            MachineAction::ConfigureFilamentSupply {
                supply_id,
                material,
                color,
            } => match self.targets.get(supply_id) {
                Some(Target::Slot(slot)) => (
                    "msConfig_cmd",
                    json!({"slot":slot,"mt":material,"rgb":color}),
                ),
                Some(Target::ExternalSupply) => {
                    ("ipdMsConfig_cmd", json!({"mt":material,"rgb":color}))
                }
                Some(_) => return Err(ControlError::Unsupported),
                None => return Err(ControlError::StaleResource),
            },
            MachineAction::LoadFilament { supply_id }
            | MachineAction::UnloadFilament { supply_id }
            | MachineAction::CancelFilamentOperation { supply_id } => {
                let action = match action {
                    MachineAction::LoadFilament { .. } => 0,
                    MachineAction::UnloadFilament { .. } => 1,
                    _ => 2,
                };
                match self.targets.get(supply_id) {
                    Some(Target::Slot(slot)) => ("ms_cmd", json!({"action":action,"slot":slot})),
                    Some(Target::ExternalSupply) => ("ipdMs_cmd", json!({"action":action})),
                    _ => return Err(ControlError::StaleResource),
                }
            }
            MachineAction::AcknowledgePrinterAlert { alert_id } => {
                if printing {
                    return Err(ControlError::InvalidState);
                }
                match self.targets.get(alert_id) {
                    Some(Target::Alert(code)) => (
                        "errorCodeCtrl_cmd",
                        json!({"action":"clearErrorCode","errorCode":code}),
                    ),
                    _ => return Err(ControlError::StaleResource),
                }
            }
        };
        Ok(json!({"cmd":cmd,"args":args}))
    }

    fn target(&self, id: ResourceId, expected: &Target) -> Result<(), ControlError> {
        match self.targets.get(&id) {
            Some(target) if target == expected => Ok(()),
            _ => Err(ControlError::StaleResource),
        }
    }
}

fn valid_color(value: Option<&str>) -> Option<String> {
    value
        .filter(|s| {
            s.len() == 7
                && s.starts_with('#')
                && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        })
        .map(str::to_owned)
}
fn open(enabled: bool) -> &'static str {
    if enabled { "open" } else { "close" }
}

pub(super) async fn dispatch(
    client: &reqwest::Client,
    url: &str,
    serial: &str,
    check: &str,
    payload: serde_json::Value,
) -> ControlResult {
    let body = json!({"serialNumber":serial,"checkCode":check,"payload":payload});
    let result = flashforge_action_reply(
        client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .timeout(Duration::from_secs(10)),
    )
    .await;
    match result {
        Ok(reply) if reply.code == 0 => ControlResult::Accepted,
        Ok(_) => ControlResult::Rejected(ControlError::PrinterRejected),
        Err(result) => result,
    }
}

fn reported_position(value: Option<&serde_json::Value>) -> Option<[f64; 3]> {
    let [x, y, z] = value?.as_array()?.as_slice() else {
        return None;
    };
    let position = [x.as_f64()?, y.as_f64()?, z.as_f64()?];
    // AD5X reports 10000 for unavailable coordinates, including before HTTP homing.
    position
        .iter()
        .all(|axis| axis.is_finite() && *axis != 10000.)
        .then_some(position)
}
