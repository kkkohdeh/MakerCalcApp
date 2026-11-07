//! Local machine-control intentions. No provider command strings cross this boundary.
use std::time::Instant;

use serde::Serialize;
use tokio::sync::oneshot;

use super::workspace::ResourceId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CalibrationKind {
    BedLeveling,
    VibrationCompensation,
}

/// Capability, not authorization or a promise that the machine is ready now.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MachineActionKind {
    DisableMotors,
    HomeAxes,
    MoveAxisRelative,
    ExtrudeFilament,
    SetPrintSpeedOverride,
    SetZOffset,
    SetFanPower,
    SetFanEnabled,
    SetCameraStreamEnabled,
    ConfirmBuildPlateCleared,
    RunCalibration,
    SetAutoShutdown,
    SetDeviceName,
    SetDeviceProfileName,
    ConfigureFilamentSupply,
    LoadFilament,
    UnloadFilament,
    CancelFilamentOperation,
    AcknowledgePrinterAlert,
    SetFlowOverride,
    SetPressureAdvance,
    SetRetraction,
    DeleteFile,
    RenameFile,
    EnqueuePrint,
    RemoveQueuedPrint,
    SetPrintQueuePaused,
    ExcludePrintObject,
    SetPowerEnabled,
    SetLightBrightness,
}

/// Explicit local actions; resource IDs must belong to the current connection epoch.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Local control API intentionally has no UI or remote caller yet"
    )
)]
pub(crate) enum MachineAction {
    DisableMotors,
    HomeAxes {
        axes: Vec<Axis>,
    },
    MoveAxisRelative {
        axis: Axis,
        distance_mm: f64,
        speed_mm_s: Option<u8>,
    },
    ExtrudeFilament {
        nozzle_id: ResourceId,
        length_mm: f64,
    },
    SetPrintSpeedOverride {
        percent: u16,
    },
    /// Absolute applied Z compensation, not a relative adjustment or head height.
    SetZOffset {
        offset_mm: f64,
    },
    SetFanPower {
        fan_id: ResourceId,
        percent: u8,
    },
    SetFanEnabled {
        fan_id: ResourceId,
        enabled: bool,
    },
    SetCameraStreamEnabled {
        camera_id: ResourceId,
        enabled: bool,
    },
    ConfirmBuildPlateCleared,
    RunCalibration {
        kind: CalibrationKind,
    },
    /// Delay after a print finishes. Backends must not silently round seconds.
    SetAutoShutdown {
        enabled: bool,
        delay_s: u32,
    },
    SetDeviceName {
        name: String,
    },
    SetDeviceProfileName {
        name: String,
    },
    ConfigureFilamentSupply {
        supply_id: ResourceId,
        material: String,
        color: String,
    },
    LoadFilament {
        supply_id: ResourceId,
    },
    UnloadFilament {
        supply_id: ResourceId,
    },
    CancelFilamentOperation {
        supply_id: ResourceId,
    },
    AcknowledgePrinterAlert {
        alert_id: ResourceId,
    },
    SetFlowOverride {
        percent: u16,
    },
    SetPressureAdvance {
        nozzle_id: ResourceId,
        advance_s: f64,
    },
    SetRetraction {
        length_mm: f64,
        speed_mm_s: f64,
        extra_recovery_mm: f64,
        recovery_speed_mm_s: f64,
    },
    DeleteFile {
        file_id: ResourceId,
    },
    /// Rename within the existing directory; no paths or overwrite intent.
    RenameFile {
        file_id: ResourceId,
        name: String,
    },
    EnqueuePrint {
        file_id: ResourceId,
    },
    RemoveQueuedPrint {
        queue_item_id: ResourceId,
    },
    /// Queue scheduling, not pause/resume of the current physical print.
    SetPrintQueuePaused {
        paused: bool,
    },
    ExcludePrintObject {
        object_id: ResourceId,
    },
    SetPowerEnabled {
        device_id: ResourceId,
        enabled: bool,
    },
    SetLightBrightness {
        light_id: ResourceId,
        percent: u8,
    },
}

impl MachineAction {
    pub(crate) fn kind(&self) -> MachineActionKind {
        use MachineActionKind as K;
        match self {
            Self::DisableMotors => K::DisableMotors,
            Self::HomeAxes { .. } => K::HomeAxes,
            Self::MoveAxisRelative { .. } => K::MoveAxisRelative,
            Self::ExtrudeFilament { .. } => K::ExtrudeFilament,
            Self::SetPrintSpeedOverride { .. } => K::SetPrintSpeedOverride,
            Self::SetZOffset { .. } => K::SetZOffset,
            Self::SetFanPower { .. } => K::SetFanPower,
            Self::SetFanEnabled { .. } => K::SetFanEnabled,
            Self::SetCameraStreamEnabled { .. } => K::SetCameraStreamEnabled,
            Self::ConfirmBuildPlateCleared => K::ConfirmBuildPlateCleared,
            Self::RunCalibration { .. } => K::RunCalibration,
            Self::SetAutoShutdown { .. } => K::SetAutoShutdown,
            Self::SetDeviceName { .. } => K::SetDeviceName,
            Self::SetDeviceProfileName { .. } => K::SetDeviceProfileName,
            Self::ConfigureFilamentSupply { .. } => K::ConfigureFilamentSupply,
            Self::LoadFilament { .. } => K::LoadFilament,
            Self::UnloadFilament { .. } => K::UnloadFilament,
            Self::CancelFilamentOperation { .. } => K::CancelFilamentOperation,
            Self::AcknowledgePrinterAlert { .. } => K::AcknowledgePrinterAlert,
            Self::SetFlowOverride { .. } => K::SetFlowOverride,
            Self::SetPressureAdvance { .. } => K::SetPressureAdvance,
            Self::SetRetraction { .. } => K::SetRetraction,
            Self::DeleteFile { .. } => K::DeleteFile,
            Self::RenameFile { .. } => K::RenameFile,
            Self::EnqueuePrint { .. } => K::EnqueuePrint,
            Self::RemoveQueuedPrint { .. } => K::RemoveQueuedPrint,
            Self::SetPrintQueuePaused { .. } => K::SetPrintQueuePaused,
            Self::ExcludePrintObject { .. } => K::ExcludePrintObject,
            Self::SetPowerEnabled { .. } => K::SetPowerEnabled,
            Self::SetLightBrightness { .. } => K::SetLightBrightness,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ControlError> {
        let valid = match self {
            Self::HomeAxes { axes } => {
                !axes.is_empty()
                    && axes.len() <= 3
                    && axes
                        .iter()
                        .enumerate()
                        .all(|(i, axis)| !axes[..i].contains(axis))
            }
            Self::MoveAxisRelative {
                distance_mm,
                speed_mm_s,
                ..
            } => {
                distance_mm.is_finite()
                    && *distance_mm != 0.0
                    && distance_mm.abs() <= 100.0
                    && speed_mm_s.is_none_or(|speed| (5..=75).contains(&speed))
            }
            Self::ExtrudeFilament { length_mm, .. } => {
                length_mm.is_finite() && *length_mm != 0.0 && length_mm.abs() <= 100.0
            }
            Self::SetPrintSpeedOverride { percent } => (1..=1000).contains(percent),
            Self::SetFlowOverride { percent } => (1..=1000).contains(percent),
            Self::SetPressureAdvance { advance_s, .. } => {
                advance_s.is_finite() && (0.0..=1.0).contains(advance_s)
            }
            Self::SetRetraction {
                length_mm,
                speed_mm_s,
                extra_recovery_mm,
                recovery_speed_mm_s,
            } => {
                length_mm.is_finite()
                    && (0.0..=100.0).contains(length_mm)
                    && speed_mm_s.is_finite()
                    && (0.01..=100.0).contains(speed_mm_s)
                    && extra_recovery_mm.is_finite()
                    && extra_recovery_mm.abs() <= 100.0
                    && recovery_speed_mm_s.is_finite()
                    && (0.01..=100.0).contains(recovery_speed_mm_s)
            }
            Self::RenameFile { name, .. } => {
                valid_label(name, 128)
                    && !name.contains(['/', '\\', ':', '"', '?', '*', '<', '>', '|'])
                    && (name.to_ascii_lowercase().ends_with(".gcode")
                        || name.to_ascii_lowercase().ends_with(".g"))
            }
            Self::SetZOffset { offset_mm } => offset_mm.is_finite() && offset_mm.abs() <= 10.0,
            Self::SetFanPower { percent, .. } => *percent <= 100,
            Self::SetLightBrightness { percent, .. } => *percent <= 100,
            Self::SetAutoShutdown { delay_s, .. } => (60..=86400).contains(delay_s),
            Self::SetDeviceName { name } | Self::SetDeviceProfileName { name } => {
                valid_label(name, 64)
            }
            Self::ConfigureFilamentSupply {
                material, color, ..
            } => {
                valid_label(material, 64)
                    && color.len() == 7
                    && color.starts_with('#')
                    && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(ControlError::InvalidData)
        }
    }
}

pub(crate) fn valid_label(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

/// Typed errors remain untranslated until a future presentation consumer chooses a label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlError {
    Unsupported,
    InvalidData,
    InvalidState,
    StaleResource,
    Disconnected,
    Busy,
    SendFailed,
    PrinterRejected,
}

/// Accepted confirms dispatch only; it never confirms physical completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlResult {
    Accepted,
    Rejected(ControlError),
    Unknown,
}

pub(crate) struct MachineCommand {
    pub(crate) connection_epoch: u64,
    pub(crate) submitted_at: Instant,
    pub(crate) action: MachineAction,
    pub(crate) reply: oneshot::Sender<ControlResult>,
}
