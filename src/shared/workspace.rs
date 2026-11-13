//! MakerCalc's local workspace contract. No vendor keys, RPCs, URLs or source paths.
//! Adapters own resource resolution. UI consumers send semantic intents only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use super::gcode::GcodeFacts;
use super::telemetry::ElementId;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UiLanguage {
    #[default]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "es")]
    Spanish,
}

/// Local desktop policy, never part of printer telemetry or Cloud's wire contract.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct DesktopPreferences {
    pub(crate) windows_notifications: bool,
    pub(crate) minimize_on_close: bool,
    pub(crate) start_in_background: bool,
    pub(crate) language: UiLanguage,
}
#[allow(unused_imports)]
pub(crate) use super::thumbnail::{
    NormalizedThumbnail, ThumbnailError, ThumbnailHandle, ThumbnailIo, ThumbnailRequest,
    ThumbnailResourceKey, make_thumbnail_channels,
};

/// Opaque, process-local handle; not a filename or a durable business identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct ResourceId(pub(crate) u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadStatus {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Unsupported,
    Denied,
    Limited,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReadResult<T> {
    pub(crate) status: ReadStatus,
    pub(crate) value: Option<T>,
    pub(crate) error: Option<String>,
}

impl<T> Default for ReadResult<T> {
    fn default() -> Self {
        Self {
            status: ReadStatus::NotLoaded,
            value: None,
            error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Support {
    #[default]
    Unknown,
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Capabilities {
    pub(crate) files: Support,
    pub(crate) history: Support,
    pub(crate) queue: Support,
    pub(crate) cameras: Support,
    pub(crate) power: Support,
    pub(crate) lighting: Support,
    pub(crate) sensors: Support,
    pub(crate) material_station: Support,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeviceKind {
    Nozzle,
    Bed,
    TemperatureSensor,
    ChamberSensor,
    ChamberHeater,
    Fan,
    FilamentSensor,
    Motion,
    Power,
    Light,
    Extrusion,
    MotionSettings,
    Leveling,
    Probe,
    Motor,
    FileExecution,
    Controller,
}

/// Explicit meanings/units shared by every provider; unknown properties stay private.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Device {
    pub(crate) id: ResourceId,
    /// Same-kind telemetry identity verified by the provider, local to this session.
    #[serde(skip)]
    pub(crate) telemetry_id: Option<ElementId>,
    pub(crate) kind: DeviceKind,
    pub(crate) display_name: String,
    pub(crate) temperature_c: Option<f64>,
    pub(crate) target_temperature_c: Option<f64>,
    pub(crate) output_pct: Option<f64>,
    pub(crate) rpm: Option<f64>,
    pub(crate) enabled: Option<bool>,
    pub(crate) filament_present: Option<bool>,
    pub(crate) can_extrude: Option<bool>,
    #[serde(skip)]
    pub(crate) extrusion_limit_mm: Option<f64>,
    #[serde(skip)]
    pub(crate) extrusion_requires_material_idle: bool,
    pub(crate) position_mm: Option<[f64; 3]>,
    pub(crate) velocity_mm_s: Option<f64>,
    #[serde(skip)]
    pub(crate) motion_source: Option<super::machine::MotionSource>,
    pub(crate) readings: super::machine::MachineReadings,
    pub(crate) fan_role: Option<super::machine::FanRole>,
    pub(crate) nozzle_diameter_mm: Option<f64>,
    /// Controls verified for this resource, not all resources of the same kind.
    pub(crate) actions: Vec<super::control::MachineActionKind>,
    /// Inclusive writable range for fan power/light brightness, not measured output.
    pub(crate) output_control_range_pct: Option<[u8; 2]>,
    #[serde(skip)]
    pub(crate) output_at_full_control_pct: Option<f64>,
    pub(crate) locked_while_printing: Option<bool>,
}

impl Device {
    pub(crate) fn new(id: ResourceId, kind: DeviceKind, display_name: String) -> Self {
        Self {
            id,
            telemetry_id: None,
            kind,
            display_name,
            temperature_c: None,
            target_temperature_c: None,
            output_pct: None,
            rpm: None,
            enabled: None,
            filament_present: None,
            can_extrude: None,
            extrusion_limit_mm: None,
            extrusion_requires_material_idle: false,
            position_mm: None,
            velocity_mm_s: None,
            motion_source: None,
            fan_role: None,
            nozzle_diameter_mm: None,
            actions: Vec::new(),
            output_control_range_pct: None,
            output_at_full_control_pct: None,
            locked_while_printing: None,
            readings: super::machine::MachineReadings {
                extrusion: (kind == DeviceKind::Nozzle).then(super::machine::Extrusion::default),
                ..Default::default()
            },
        }
    }

    pub(crate) fn observed_output_control_pct(&self) -> Option<f64> {
        let maximum = self.output_at_full_control_pct?;
        let output = self.output_pct?;
        (maximum.is_finite() && maximum > 0.0 && (0.0..=maximum).contains(&output))
            .then(|| output / maximum * 100.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PrinterFile {
    pub(crate) id: ResourceId,
    pub(crate) display_name: String,
    pub(crate) size_bytes: Option<u64>,
    pub(crate) modified_at_unix_s: Option<f64>,
    /// Slicer estimate in seconds, not elapsed print time; unknown stays None.
    pub(crate) estimated_duration_s: Option<f64>,
    /// Local read progress; not telemetry or a persisted file attribute.
    #[serde(skip)]
    pub(crate) duration_read_status: ReadStatus,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct FileDetails {
    pub(crate) size_bytes: Option<u64>,
    pub(crate) analysis: GcodeFacts,
    pub(crate) metadata_status: ReadStatus,
    pub(crate) analysis_status: ReadStatus,
    pub(crate) metadata_error: Option<String>,
    pub(crate) analysis_error: Option<String>,
    #[serde(skip)]
    pub(crate) thumbnail: Option<Bytes>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct HistoryTotals {
    pub(crate) jobs: Option<u64>,
    pub(crate) print_duration_s: Option<f64>,
    pub(crate) total_duration_s: Option<f64>,
    pub(crate) filament_used_mm: Option<f64>,
    pub(crate) longest_print_duration_s: Option<f64>,
    pub(crate) longest_total_duration_s: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueueStatus {
    #[default]
    Unknown,
    Ready,
    Running,
    Paused,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct PrintQueue {
    pub(crate) status: QueueStatus,
    pub(crate) file_names: Vec<String>,
    pub(crate) entries: Vec<QueueEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueEntry {
    pub(crate) id: ResourceId,
    pub(crate) display_name: String,
    pub(crate) added_at_unix_s: Option<f64>,
    pub(crate) waiting_s: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Camera {
    pub(crate) id: ResourceId,
    pub(crate) display_name: String,
    pub(crate) enabled: Option<bool>,
    pub(crate) rotation_degrees: Option<u16>,
    pub(crate) flip_horizontal: Option<bool>,
    pub(crate) flip_vertical: Option<bool>,
    pub(crate) target_fps: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct SystemInfo {
    pub(crate) firmware_ready: Option<bool>,
    pub(crate) software_version: Option<String>,
    pub(crate) discovered_devices: usize,
    pub(crate) unmapped_objects: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DetailState {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) live: bool,
    pub(crate) capabilities: Capabilities,
    pub(crate) devices: Vec<Device>,
    /// Verified primary heater destinations for the existing local target action.
    pub(crate) temperature_targets: Vec<HeaterKind>,
    pub(crate) files: ReadResult<Vec<PrinterFile>>,
    pub(crate) file_estimate_error: Option<String>,
    pub(crate) file_details: BTreeMap<ResourceId, ReadResult<FileDetails>>,
    pub(crate) history_totals: ReadResult<HistoryTotals>,
    pub(crate) queue: ReadResult<PrintQueue>,
    pub(crate) cameras: ReadResult<Vec<Camera>>,
    pub(crate) power_devices: ReadResult<Vec<Device>>,
    pub(crate) lights: ReadResult<Vec<Device>>,
    pub(crate) system: SystemInfo,
    pub(crate) system_read: ReadResult<()>,
    /// Local machine contract; Cloud deliberately does not serialize DetailState.
    pub(crate) machine: super::machine::MachineInfo,
    pub(crate) storage: ReadResult<Vec<super::machine::StorageVolume>>,
    pub(crate) print_objects: ReadResult<Vec<super::machine::PrintObject>>,
}

impl DetailState {
    pub(crate) fn empty(generation: u64) -> Self {
        Self {
            generation,
            revision: 0,
            live: false,
            capabilities: Capabilities::default(),
            devices: Vec::new(),
            temperature_targets: Vec::new(),
            files: ReadResult::default(),
            file_estimate_error: None,
            file_details: BTreeMap::new(),
            history_totals: ReadResult::default(),
            queue: ReadResult::default(),
            cameras: ReadResult::default(),
            power_devices: ReadResult::default(),
            lights: ReadResult::default(),
            system: SystemInfo::default(),
            system_read: ReadResult::default(),
            machine: super::machine::MachineInfo::default(),
            storage: ReadResult::default(),
            print_objects: ReadResult::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DetailRequest {
    RefreshMachine,
    ListFiles,
    InspectFile(ResourceId),
    HistoryTotals,
    #[allow(dead_code)]
    Queue,
    #[allow(dead_code)]
    Cameras,
    #[allow(dead_code)]
    Peripherals,
    #[allow(dead_code)]
    System,
}

#[derive(Clone)]
pub(crate) struct DetailHandle {
    pub(crate) active: watch::Sender<bool>,
    pub(crate) file_interest: watch::Sender<FileReadInterest>,
    pub(crate) requests: mpsc::Sender<DetailRequest>,
    pub(crate) state: watch::Receiver<Arc<DetailState>>,
}
pub(crate) struct DetailIo {
    pub(crate) active: watch::Receiver<bool>,
    pub(crate) file_interest: watch::Receiver<FileReadInterest>,
    pub(crate) requests: mpsc::Receiver<DetailRequest>,
    pub(crate) state: watch::Sender<Arc<DetailState>>,
}
pub(crate) fn make_detail_channels() -> (DetailHandle, DetailIo) {
    let (active, active_io) = watch::channel(false);
    let (file_interest, file_interest_io) = watch::channel(FileReadInterest::default());
    let (requests, request_io) = mpsc::channel(16);
    let (state_io, state) = watch::channel(Arc::new(DetailState::empty(0)));
    (
        DetailHandle {
            active,
            file_interest,
            requests,
            state,
        },
        DetailIo {
            active: active_io,
            file_interest: file_interest_io,
            requests: request_io,
            state: state_io,
        },
    )
}

pub(crate) const FILE_READ_WINDOW: usize = 20;

/// Replaceable read interest, not a queue of commands. Contains no paths or credentials.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FileReadInterest {
    pub(crate) generation: u64,
    /// Changes with navigation, not when estimates refill the pending window.
    pub(crate) view_revision: u64,
    pub(crate) ids: Vec<ResourceId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrinterActionKind {
    StartPrint,
    PausePrint,
    ResumePrint,
    CancelPrint,
    SetTemperature,
    SetLight,
    UploadGcode,
    MachineControl,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PrinterControls {
    pub(crate) pause: bool,
    pub(crate) resume: bool,
    pub(crate) cancel: bool,
    pub(crate) build_area_light: bool,
    pub(crate) toolhead_light: bool,
    pub(crate) remote: bool,
}

impl PrinterControls {
    pub(crate) const STANDARD: Self = Self {
        pause: true,
        resume: true,
        cancel: true,
        build_area_light: true,
        toolhead_light: true,
        remote: true,
    };

    pub(crate) fn allows_print(self, kind: PrinterActionKind) -> bool {
        match kind {
            PrinterActionKind::PausePrint => self.pause,
            PrinterActionKind::ResumePrint => self.resume,
            PrinterActionKind::CancelPrint => self.cancel,
            _ => false,
        }
    }

    pub(crate) fn allows_light(self, role: LightRole) -> bool {
        match role {
            LightRole::BuildArea => self.build_area_light,
            LightRole::Toolhead => self.toolhead_light,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeaterKind {
    Nozzle,
    Bed,
}

pub(crate) use crate::shared::telemetry::LightRole;

impl HeaterKind {
    pub(crate) const fn max_target_c(self) -> u16 {
        match self {
            Self::Nozzle => 230,
            Self::Bed => 65,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaterialAssignment {
    pub(crate) channel_id: u32,
    pub(crate) supply_id: ElementId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrintOptionKind {
    BedLeveling,
    FirstLayerInspection,
    FlowCalibration,
    Timelapse,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct StartPrintOptions {
    pub(crate) bed_leveling: bool,
    pub(crate) material_station: bool,
    pub(crate) material_assignments: Vec<MaterialAssignment>,
    #[serde(default)]
    pub(crate) first_layer_inspection: bool,
    #[serde(default)]
    pub(crate) flow_calibration: bool,
    #[serde(default)]
    pub(crate) timelapse: bool,
}

impl Default for StartPrintOptions {
    fn default() -> Self {
        Self {
            bed_leveling: true,
            material_station: false,
            material_assignments: Vec::new(),
            first_layer_inspection: false,
            flow_calibration: false,
            timelapse: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PrintControlContext {
    pub(crate) connection_epoch: u64,
    pub(crate) job_generation: u64,
    pub(crate) submitted_at: Instant,
}
impl PrintControlContext {
    pub(crate) fn new(connection_epoch: u64, job_generation: u64) -> Self {
        Self {
            connection_epoch,
            job_generation,
            submitted_at: Instant::now(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum PrinterAction {
    StartPrint {
        file_id: ResourceId,
        options: StartPrintOptions,
    },
    PausePrint(PrintControlContext),
    ResumePrint(PrintControlContext),
    CancelPrint(PrintControlContext),
    SetTemperatureTarget {
        heater: HeaterKind,
        target_c: u16,
        connection_epoch: u64,
        submitted_at: Instant,
    },
    ToggleLight {
        role: LightRole,
        expected_enabled: Option<bool>,
        connection_epoch: u64,
        submitted_at: Instant,
    },
    UploadGcode {
        source: PathBuf,
        destination: String,
    },
}
impl PrinterAction {
    pub(crate) fn kind(&self) -> PrinterActionKind {
        match self {
            Self::StartPrint { .. } => PrinterActionKind::StartPrint,
            Self::PausePrint(..) => PrinterActionKind::PausePrint,
            Self::ResumePrint(..) => PrinterActionKind::ResumePrint,
            Self::CancelPrint(..) => PrinterActionKind::CancelPrint,
            Self::SetTemperatureTarget { .. } => PrinterActionKind::SetTemperature,
            Self::ToggleLight { .. } => PrinterActionKind::SetLight,
            Self::UploadGcode { .. } => PrinterActionKind::UploadGcode,
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) enum OperationState {
    Idle,
    ReadOnly,
    Sending(PrinterActionKind),
    Accepted(PrinterActionKind),
    Rejected {
        action: PrinterActionKind,
        message: String,
    },
    Unknown(PrinterActionKind),
}
impl OperationState {
    pub(crate) fn is_pending(&self) -> bool {
        matches!(self, Self::Sending(_))
    }

    pub(crate) fn allows_actions(&self) -> bool {
        !matches!(self, Self::ReadOnly)
    }
}
#[derive(Clone)]
pub(crate) struct OperationHandle {
    pub(crate) actions: mpsc::Sender<PrinterAction>,
    pub(crate) state: watch::Receiver<OperationState>,
    machine_actions: mpsc::Sender<super::control::MachineCommand>,
}
impl OperationHandle {
    /// Non-blocking local API. The adapter revalidates epoch, freshness and readiness.
    pub(crate) fn machine_action(
        &self,
        connection_epoch: u64,
        action: super::control::MachineAction,
    ) -> Result<oneshot::Receiver<super::control::ControlResult>, super::control::ControlError>
    {
        self.machine_action_for_job(connection_epoch, None, action)
    }

    pub(crate) fn machine_action_for_job(
        &self,
        connection_epoch: u64,
        job_target: Option<crate::state::MachineJobTarget>,
        action: super::control::MachineAction,
    ) -> Result<oneshot::Receiver<super::control::ControlResult>, super::control::ControlError>
    {
        use super::control::{ControlError, MachineCommand};
        action.validate()?;
        if !self.state.borrow().allows_actions() {
            return Err(ControlError::Unsupported);
        }
        if self.state.borrow().is_pending() || self.actions.capacity() < self.actions.max_capacity()
        {
            return Err(ControlError::Busy);
        }
        let (reply, result) = oneshot::channel();
        self.machine_actions
            .try_send(MachineCommand {
                connection_epoch,
                job_target,
                submitted_at: Instant::now(),
                action,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ControlError::Busy,
                mpsc::error::TrySendError::Closed(_) => ControlError::Disconnected,
            })?;
        Ok(result)
    }
}
pub(crate) struct OperationIo {
    pub(crate) actions: mpsc::Receiver<PrinterAction>,
    pub(crate) state: watch::Sender<OperationState>,
    pub(crate) machine_actions: mpsc::Receiver<super::control::MachineCommand>,
}
pub(crate) fn make_operation_channels() -> (OperationHandle, OperationIo) {
    let (actions, action_io) = mpsc::channel(4);
    let (state_io, state) = watch::channel(OperationState::Idle);
    let (machine_actions, machine_io) = mpsc::channel(1);
    (
        OperationHandle {
            actions,
            state,
            machine_actions,
        },
        OperationIo {
            actions: action_io,
            state: state_io,
            machine_actions: machine_io,
        },
    )
}

#[derive(Clone, Debug)]
pub(crate) enum CameraRequest {
    Load { id: ResourceId },
}
#[derive(Clone)]
pub(crate) struct CameraState {
    pub(crate) status: ReadStatus,
    pub(crate) frame: Option<Bytes>,
    pub(crate) received_at: Option<Instant>,
    pub(crate) message: Option<String>,
}
impl CameraState {
    pub(crate) fn empty() -> Self {
        Self {
            status: ReadStatus::NotLoaded,
            frame: None,
            received_at: None,
            message: None,
        }
    }
}
#[derive(Clone)]
pub(crate) struct CameraHandle {
    pub(crate) requests: mpsc::Sender<CameraRequest>,
    pub(crate) state: watch::Receiver<Arc<CameraState>>,
}
pub(crate) struct CameraIo {
    pub(crate) requests: mpsc::Receiver<CameraRequest>,
    pub(crate) state: watch::Sender<Arc<CameraState>>,
}
pub(crate) fn make_camera_channels() -> (CameraHandle, CameraIo) {
    let (requests, request_io) = mpsc::channel(2);
    let (state_io, state) = watch::channel(Arc::new(CameraState::empty()));
    (
        CameraHandle { requests, state },
        CameraIo {
            requests: request_io,
            state: state_io,
        },
    )
}

/// Local form validation error; the adapter revalidates immediately before upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UploadValidationError {
    InvalidName,
    Unreadable,
    InvalidSize,
}

impl std::fmt::Display for UploadValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = match self {
            Self::InvalidName => "files.upload.error.invalid_name",
            Self::Unreadable => "files.upload.error.unreadable",
            Self::InvalidSize => "files.upload.error.invalid_size",
        };
        f.write_str(crate::i18n::t!(key).as_ref())
    }
}

pub(crate) fn validate_upload(
    source: &Path,
    destination: &str,
) -> Result<u64, UploadValidationError> {
    let lower = destination.to_ascii_lowercase();
    if !(lower.ends_with(".gcode") || lower.ends_with(".g"))
        || destination.contains(['/', '\\'])
        || destination.len() > 512
        || destination.chars().any(char::is_control)
    {
        return Err(UploadValidationError::InvalidName);
    }
    let metadata = std::fs::metadata(source).map_err(|_| UploadValidationError::Unreadable)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 * 1024 {
        return Err(UploadValidationError::InvalidSize);
    }
    Ok(metadata.len())
}
