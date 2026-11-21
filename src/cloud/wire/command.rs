use serde::Deserialize;
use serde_json::{Map, Value};

use crate::shared::control::{
    Axis, CalibrationKind, MachineAction, MachineActionKind, valid_label,
};
use crate::shared::machine::DryingCycle;
use crate::shared::telemetry::ElementId;
use crate::shared::workspace::{DetailState, DeviceKind, HeaterKind, ReadStatus, ResourceId};
use crate::state::{MachineJobTarget, PrinterState};

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::cloud) enum Command {
    PausePrint,
    ResumePrint,
    CancelPrint,
    ToggleLight {
        role: String,
    },
    SetLightBrightness {
        light_id: ElementId,
        percent: u8,
    },
    SetFanPower {
        fan_id: ElementId,
        percent: u8,
    },
    SetFanEnabled {
        fan_id: ElementId,
        enabled: bool,
    },
    SetPrintSpeedOverride {
        percent: u16,
    },
    SetFlowOverride {
        percent: u16,
    },
    SetZOffset {
        offset_mm: f64,
    },
    SetAutoShutdown {
        enabled: bool,
        delay_s: Option<u32>,
    },
    SetPrintQueuePaused {
        paused: bool,
    },
    RemoveQueuedPrint {
        queue_item_id: ElementId,
    },
    ExcludePrintObject {
        object_id: ElementId,
    },
    AcknowledgePrinterAlert {
        alert_id: ElementId,
    },
    SetDeviceName {
        name: String,
    },
    SetTemperatureTarget {
        heater: HeaterKind,
        target_c: u16,
    },
    HomeAxes {
        axes: Vec<Axis>,
    },
    MoveAxisRelative {
        axis: Axis,
        distance_mm: f64,
    },
    DisableMotors,
    ExtrudeFilament {
        nozzle_id: ElementId,
        length_mm: f64,
    },
    RunCalibration {
        kind: CalibrationKind,
    },
    ConfirmBuildPlateCleared,
    LoadFilament {
        supply_id: ElementId,
    },
    UnloadFilament {
        supply_id: ElementId,
    },
    CancelFilamentOperation {
        supply_id: ElementId,
    },
    StartDrying {
        dryer_id: ElementId,
        temperature_c: u16,
        duration_hours: u16,
    },
    StopDrying {
        dryer_id: ElementId,
    },
    #[serde(other)]
    Unknown,
}

impl Command {
    pub(in crate::cloud) fn parse(action: Value, mut params: Map<String, Value>) -> Option<Self> {
        let name = action.as_str()?;
        if matches!(
            name,
            "pause_print"
                | "resume_print"
                | "cancel_print"
                | "disable_motors"
                | "confirm_build_plate_cleared"
        ) && !params.is_empty()
        {
            return None;
        }
        if name == "set_auto_shutdown" && params.get("delay_s").is_some_and(Value::is_null) {
            return None;
        }
        params.insert("action".into(), action);
        serde_json::from_value(Value::Object(params)).ok()
    }

    pub(in crate::cloud) fn resolve(
        self,
        detail: &DetailState,
        state: &PrinterState,
    ) -> Option<MachineAction> {
        use MachineAction as A;
        use MachineActionKind as K;

        let devices = detail
            .devices
            .iter()
            .chain(detail.lights.value.iter().flatten());
        let resolve_device = |id, kind, action| {
            let mut matches = devices
                .clone()
                .filter(|device| device.telemetry_id == Some(id) && device.kind == kind);
            let device = matches.next()?;
            (matches.next().is_none() && device.actions.contains(&action)).then_some(device.id)
        };
        let resolve_supply = |id, kind| {
            if state
                .telemetry
                .filament_supplies
                .iter()
                .flatten()
                .filter(|supply| supply.id == id)
                .count()
                != 1
            {
                return None;
            }
            let mut matches = detail
                .machine
                .supplies
                .iter()
                .filter(|supply| supply.reading.id == id);
            let supply = matches.next()?;
            (matches.next().is_none()
                && supply.availability(kind, std::time::Instant::now()).is_ok())
            .then_some(supply.id)
        };
        let resolve_dryer = |id| {
            let mut matches = detail
                .machine
                .dryers
                .iter()
                .filter(|dryer| dryer.telemetry_id == id);
            let dryer = matches.next()?;
            matches.next().is_none().then_some(dryer)
        };
        let action = match self {
            Self::SetLightBrightness { light_id, percent } => {
                return checked(A::SetLightBrightness {
                    light_id: resolve_device(light_id, DeviceKind::Light, K::SetLightBrightness)?,
                    percent,
                });
            }
            Self::SetFanPower { fan_id, percent } => {
                return checked(A::SetFanPower {
                    fan_id: resolve_device(fan_id, DeviceKind::Fan, K::SetFanPower)?,
                    percent,
                });
            }
            Self::SetFanEnabled { fan_id, enabled } => {
                return checked(A::SetFanEnabled {
                    fan_id: resolve_device(fan_id, DeviceKind::Fan, K::SetFanEnabled)?,
                    enabled,
                });
            }
            Self::SetPrintSpeedOverride { percent } => A::SetPrintSpeedOverride { percent },
            Self::SetFlowOverride { percent } => A::SetFlowOverride { percent },
            Self::SetZOffset { offset_mm } => A::SetZOffset { offset_mm },
            Self::SetAutoShutdown {
                enabled: true,
                delay_s: Some(delay_s),
            } => A::SetAutoShutdown {
                enabled: true,
                delay_s,
            },
            Self::SetAutoShutdown {
                enabled: false,
                delay_s: None,
            } => A::SetAutoShutdown {
                enabled: false,
                delay_s: 0,
            },
            Self::SetPrintQueuePaused { paused } => A::SetPrintQueuePaused { paused },
            Self::RemoveQueuedPrint { queue_item_id }
                if detail.queue.status == ReadStatus::Ready =>
            {
                let matching = detail
                    .queue
                    .value
                    .as_ref()?
                    .entries
                    .iter()
                    .filter(|entry| entry.telemetry_id == Some(queue_item_id))
                    .map(|entry| entry.id);
                A::RemoveQueuedPrint {
                    queue_item_id: unique(matching)?,
                }
            }
            Self::ExcludePrintObject { object_id }
                if detail.print_objects_job
                    == Some((state.connection_epoch, state.job_generation))
                    && matches!(state.machine_job_target(), MachineJobTarget::Active(_)) =>
            {
                let matching = detail
                    .print_objects
                    .value
                    .as_ref()?
                    .iter()
                    .filter(|object| object.telemetry_id == Some(object_id))
                    .map(|object| object.id);
                A::ExcludePrintObject {
                    object_id: unique(matching)?,
                }
            }
            Self::AcknowledgePrinterAlert { alert_id } => {
                let (id, resource) = detail.machine.acknowledgeable_alert?;
                if id != alert_id
                    || state
                        .telemetry
                        .printer_alerts
                        .iter()
                        .flatten()
                        .filter(|alert| alert.id == Some(alert_id))
                        .count()
                        != 1
                {
                    return None;
                }
                A::AcknowledgePrinterAlert { alert_id: resource }
            }
            Self::SetDeviceName { name } if valid_label(&name, 64) => A::SetDeviceName { name },
            Self::HomeAxes { axes } => A::HomeAxes { axes },
            Self::MoveAxisRelative { axis, distance_mm } if detail.machine.remote_jog => {
                A::MoveAxisRelative {
                    axis,
                    distance_mm,
                    speed_mm_s: None,
                }
            }
            Self::DisableMotors if detail.machine.motion_idle == Some(true) => A::DisableMotors,
            Self::RunCalibration { kind }
                if detail.machine.calibrations.contains(&kind)
                    && detail
                        .machine
                        .calibration_readiness
                        .is_some_and(|ready| ready.check().is_ok()) =>
            {
                A::RunCalibration { kind }
            }
            Self::ConfirmBuildPlateCleared => A::ConfirmBuildPlateCleared,
            Self::ExtrudeFilament {
                nozzle_id,
                length_mm,
            } => {
                let mut rows = state
                    .telemetry
                    .nozzles
                    .iter()
                    .filter(|nozzle| nozzle.id == nozzle_id);
                if !rows.next().is_some_and(|nozzle| nozzle.is_active) || rows.next().is_some() {
                    return None;
                }
                let id = resolve_device(nozzle_id, DeviceKind::Nozzle, K::ExtrudeFilament)?;
                let nozzle = detail.devices.iter().find(|device| device.id == id)?;
                let limit = nozzle
                    .extrusion_limit_mm
                    .filter(|limit| limit.is_finite() && *limit > 0.0)?;
                if nozzle.can_extrude != Some(true) || length_mm.abs() > limit {
                    return None;
                }
                return checked(A::ExtrudeFilament {
                    nozzle_id: id,
                    length_mm,
                    speed_mm_s: None,
                    target_c: None,
                });
            }
            Self::LoadFilament { supply_id } => {
                return checked(A::LoadFilament {
                    supply_id: resolve_supply(supply_id, K::LoadFilament)?,
                });
            }
            Self::UnloadFilament { supply_id } => {
                let supply_id = resolve_supply(supply_id, K::UnloadFilament)?;
                if detail.machine.current_supply != Some(supply_id) {
                    return None;
                }
                return checked(A::UnloadFilament { supply_id });
            }
            Self::CancelFilamentOperation { supply_id } => {
                return checked(A::CancelFilamentOperation {
                    supply_id: resolve_supply(supply_id, K::CancelFilamentOperation)?,
                });
            }
            Self::StartDrying {
                dryer_id,
                temperature_c,
                duration_hours,
            } => {
                let dryer = resolve_dryer(dryer_id)?;
                let cycle = DryingCycle {
                    temperature_c,
                    duration_hours,
                };
                if !dryer.can_start || !dryer.limits.is_some_and(|limits| limits.contains(cycle)) {
                    return None;
                }
                return checked(A::StartDrying {
                    dryer_id: dryer.id,
                    cycle,
                });
            }
            Self::StopDrying { dryer_id } => {
                let dryer = resolve_dryer(dryer_id)?;
                return dryer
                    .can_stop
                    .then_some(A::StopDrying { dryer_id: dryer.id });
            }
            _ => return None,
        };
        detail
            .machine
            .actions
            .contains(&action.kind())
            .then_some(())?;
        checked(action)
    }
}

fn checked(action: MachineAction) -> Option<MachineAction> {
    action.validate().ok().map(|()| action)
}

fn unique(mut matches: impl Iterator<Item = ResourceId>) -> Option<ResourceId> {
    let id = matches.next()?;
    matches.next().is_none().then_some(id)
}
