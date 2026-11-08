//! Provider-independent, live machine readings. Units are part of the contract.
//! None means absent, invalid or not reported, never a fabricated zero.
use serde::Serialize;

use super::control::{CalibrationKind, ControlError, MachineActionKind};
use super::printer::PrintStatus;
use super::telemetry::FilamentSupplyTelemetry;
use super::workspace::ResourceId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MotionSource {
    CommandInterpreter,
    MotionPlanner,
}

/// Machine identity reported by the printer, not the local alias or host software.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct DeviceMetadata {
    pub(crate) reported_name: Option<String>,
    pub(crate) manufacturer: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) firmware_version: Option<String>,
    pub(crate) location_label: Option<String>,
    pub(crate) build_volume_mm: Option<[f64; 3]>,
    pub(crate) nozzle_count: Option<u8>,
    /// Network identity is local only, even when this struct is serialized.
    #[serde(skip)]
    pub(crate) mac_address: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FanRole {
    PartCooling,
    Chamber,
    Exhaust,
    Recirculation,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SupplyKind {
    StationSlot,
    ExternalSpool,
    #[default]
    Unknown,
}

/// Opaque control resource paired with a normalized supply reading.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SupplyResource {
    pub(crate) id: ResourceId,
    pub(crate) kind: SupplyKind,
    pub(crate) reading: FilamentSupplyTelemetry,
    /// None means capability discovery is incomplete; an empty list is verified read-only.
    pub(crate) controls: Option<Vec<SupplyControl>>,
    pub(crate) operation: Option<SupplyOperation>,
    /// Local observation lease; expired controls are never actionable in cached views.
    #[serde(skip)]
    pub(crate) valid_until: Option<std::time::Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SupplyBlock {
    Unknown,
    Unsupported,
    Stale,
    Disconnected,
    Busy,
    NotIdle,
    NoFilament,
    NoOperation,
    NotCurrent,
}

impl SupplyBlock {
    pub(crate) fn error(self) -> super::control::ControlError {
        use super::control::ControlError as E;
        match self {
            Self::Unsupported => E::Unsupported,
            Self::Stale => E::StaleResource,
            Self::Disconnected => E::Disconnected,
            Self::Busy => E::Busy,
            _ => E::InvalidState,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SupplyControl {
    pub(crate) action: MachineActionKind,
    pub(crate) blocked: Option<SupplyBlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SupplyOperationPhase {
    Running,
    Cancelling,
    Completed,
}

/// Physical observation, never manufactured from a command acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SupplyOperation {
    pub(crate) action: MachineActionKind,
    pub(crate) phase: SupplyOperationPhase,
    /// Operation ordinal, never a percentage or the provider's global step code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) step: Option<u8>,
}

/// Nonterminal machine activity, independent of a print's final result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MachineActivity {
    Heating,
    Pausing,
    Stopping,
    Calibrating,
    ReceivingFile,
    Slicing,
    TransferringFile,
    ExtractingFile,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrinterIssueKind {
    FilamentFeedTimeout,
    FilamentRetractTimeout,
    FilamentFeedFailed,
    FilamentJam,
    MaterialStationHomingFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PrinterIssue {
    pub(crate) kind: PrinterIssueKind,
    pub(crate) supply_id: Option<super::telemetry::ElementId>,
}

impl SupplyResource {
    pub(crate) fn availability(
        &self,
        action: MachineActionKind,
        now: std::time::Instant,
    ) -> Result<(), SupplyBlock> {
        let controls = self.controls.as_ref().ok_or(SupplyBlock::Unknown)?;
        let control = controls
            .iter()
            .find(|c| c.action == action)
            .ok_or(SupplyBlock::Unsupported)?;
        if self.valid_until.is_none_or(|until| now >= until) {
            return Err(SupplyBlock::Stale);
        }
        control.blocked.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct MachineInfo {
    pub(crate) device: DeviceMetadata,
    /// Confirmed motion/interpreter rest; None does not authorize releasing motors.
    pub(crate) motion_idle: Option<bool>,
    /// Absolute applied Z compensation in mm. Not homing origin or head height.
    pub(crate) z_offset_mm: Option<f64>,
    pub(crate) auto_shutdown_enabled: Option<bool>,
    #[serde(skip)]
    pub(crate) auto_shutdown_delay_step_s: Option<u32>,
    pub(crate) activity: Option<MachineActivity>,
    pub(crate) material_station_slots: Option<u8>,
    #[serde(skip)]
    pub(crate) material_motion_idle: Option<bool>,
    /// True only when all reported access panels are closed; None means no confirmed sensor.
    pub(crate) enclosure_closed: Option<bool>,
    pub(crate) issues: Vec<PrinterIssue>,
    pub(crate) speed_override_range_pct: Option<[u16; 2]>,
    #[serde(skip)]
    pub(crate) print_adjustments_require_active_job: bool,
    /// Optional manual-move speed capability: maximum X/Y/Z speeds in mm/s.
    pub(crate) manual_move_max_speed_mm_s: Option<[f64; 3]>,
    pub(crate) z_offset_range_mm: Option<[f64; 2]>,
    pub(crate) calibrations: Vec<CalibrationKind>,
    pub(crate) print_options: Vec<super::workspace::PrintOptionKind>,
    pub(crate) supplies: Vec<SupplyResource>,
    /// Reported supply routed to the active head; not a filament-presence sensor.
    pub(crate) current_supply: Option<ResourceId>,
    /// Handles identify the currently observed alert, not arbitrary provider codes.
    pub(crate) alerts: Vec<ResourceId>,
    pub(crate) actions: Vec<MachineActionKind>,
    pub(crate) software: Vec<SoftwareVersion>,
}

impl MachineInfo {
    pub(crate) fn idle_setting_availability(
        &self,
        status: PrintStatus,
    ) -> Result<(), ControlError> {
        match self.material_motion_idle {
            Some(true) => {}
            Some(false) => return Err(ControlError::Busy),
            None => return Err(ControlError::InvalidState),
        }
        if status != PrintStatus::Idle || self.activity.is_some() {
            return Err(ControlError::InvalidState);
        }
        Ok(())
    }

    pub(crate) fn auto_shutdown_availability(
        &self,
        status: PrintStatus,
        enabled: bool,
    ) -> Result<(), ControlError> {
        self.idle_setting_availability(status)?;
        if self.auto_shutdown_enabled != Some(!enabled) {
            return Err(ControlError::InvalidState);
        }
        if enabled && self.auto_shutdown_delay_step_s.is_none_or(|step| step == 0) {
            return Err(ControlError::Unsupported);
        }
        Ok(())
    }

    pub(crate) fn allows_print_adjustment(&self, status: PrintStatus) -> bool {
        matches!(status, PrintStatus::Printing | PrintStatus::Paused)
            || (!self.print_adjustments_require_active_job
                && matches!(
                    status,
                    PrintStatus::Idle | PrintStatus::Completed | PrintStatus::Cancelled
                ))
    }
}

/// Versions from distinct components must never overwrite each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SoftwareRole {
    PrinterFirmware,
    ConnectionServer,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SoftwareVersion {
    pub(crate) role: SoftwareRole,
    pub(crate) version: String,
}

/// The physical storage containing printer files, not controller RAM.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StorageVolume {
    pub(crate) id: ResourceId,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) used_bytes: Option<u64>,
    pub(crate) free_bytes: Option<u64>,
}

/// IDs are scoped to the observed job/session, not persistent object identities.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PrintObject {
    pub(crate) id: ResourceId,
    pub(crate) display_name: String,
    pub(crate) excluded: Option<bool>,
    pub(crate) current: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct MachineReadings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) environment: Option<EnvironmentalReadings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extrusion: Option<Extrusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motion: Option<MotionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retraction: Option<Retraction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) leveling: Option<Leveling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) probe: Option<ProbeReadings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motor: Option<MotorReadings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_execution: Option<FileExecution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thermal: Option<ThermalReadings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) filament: Option<FilamentReadings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) controller: Option<ControllerReadings>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct EnvironmentalReadings {
    pub(crate) relative_humidity_pct: Option<f64>,
    pub(crate) pressure_pa: Option<f64>,
    /// Sensor resistance, not an invented air-quality score or gas concentration.
    pub(crate) gas_resistance_ohm: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Extrusion {
    pub(crate) pressure_advance_s: Option<f64>,
    pub(crate) pressure_smoothing_s: Option<f64>,
    pub(crate) active: Option<bool>,
    pub(crate) feed_velocity_mm_s: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct MotionSettings {
    pub(crate) commanded_position_mm: Option<[f64; 3]>,
    pub(crate) work_position_mm: Option<[f64; 3]>,
    pub(crate) origin_offset_mm: Option<[f64; 3]>,
    pub(crate) travel_min_mm: Option<[f64; 3]>,
    pub(crate) travel_max_mm: Option<[f64; 3]>,
    pub(crate) requested_velocity_mm_s: Option<f64>,
    pub(crate) velocity_limit_mm_s: Option<f64>,
    pub(crate) acceleration_limit_mm_s2: Option<f64>,
    pub(crate) corner_velocity_mm_s: Option<f64>,
    pub(crate) minimum_cruise_pct: Option<f64>,
    pub(crate) speed_override_pct: Option<f64>,
    pub(crate) flow_override_pct: Option<f64>,
    pub(crate) homed_axes: Option<[bool; 3]>,
    pub(crate) absolute_positioning: Option<bool>,
    pub(crate) absolute_extrusion: Option<bool>,
    pub(crate) stalls: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Retraction {
    pub(crate) length_mm: Option<f64>,
    pub(crate) speed_mm_s: Option<f64>,
    pub(crate) extra_recovery_mm: Option<f64>,
    pub(crate) recovery_speed_mm_s: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Leveling {
    pub(crate) profile_name: Option<String>,
    pub(crate) area_min_mm: Option<[f64; 2]>,
    pub(crate) area_max_mm: Option<[f64; 2]>,
    pub(crate) measured_grid_mm: Option<Vec<Vec<f64>>>,
    pub(crate) compensation_grid_mm: Option<Vec<Vec<f64>>>,
    pub(crate) applied: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct ProbeReadings {
    pub(crate) last_triggered: Option<bool>,
    pub(crate) last_position_mm: Option<[f64; 3]>,
    pub(crate) last_height_mm: Option<f64>,
    pub(crate) manual_active: Option<bool>,
    pub(crate) manual_height_mm: Option<f64>,
    pub(crate) lower_height_mm: Option<f64>,
    pub(crate) upper_height_mm: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct MotorReadings {
    pub(crate) run_current_a: Option<f64>,
    pub(crate) hold_current_a: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct FileExecution {
    pub(crate) active: Option<bool>,
    pub(crate) paused: Option<bool>,
    pub(crate) read_offset_bytes: Option<u64>,
    pub(crate) file_size_bytes: Option<u64>,
    pub(crate) progress_pct: Option<f64>,
    pub(crate) idle_timeout_s: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct ThermalReadings {
    pub(crate) minimum_c: Option<f64>,
    pub(crate) maximum_c: Option<f64>,
    pub(crate) reference_c: Option<f64>,
    pub(crate) z_adjustment_mm: Option<f64>,
    pub(crate) z_adjustment_enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct FilamentReadings {
    pub(crate) diameter_mm: Option<f64>,
    pub(crate) width_sensor_active: Option<bool>,
    pub(crate) flow_compensation_enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct ControllerReadings {
    pub(crate) firmware_version: Option<String>,
    pub(crate) configuration_pending: Option<bool>,
    pub(crate) receive_errors: Option<u64>,
    pub(crate) transmit_errors: Option<u64>,
    pub(crate) transmit_retries: Option<u64>,
    pub(crate) host_load_1m: Option<f64>,
    pub(crate) process_cpu_time_s: Option<f64>,
    pub(crate) available_memory_bytes: Option<u64>,
}
