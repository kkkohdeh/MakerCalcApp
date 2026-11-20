use serde::Deserialize;
use serde_json::{Map, Value};

use crate::shared::control::{MachineAction, MachineActionKind, valid_label};
use crate::shared::telemetry::ElementId;
use crate::shared::workspace::{DetailState, DeviceKind, ReadStatus, ResourceId};
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
    #[serde(other)]
    Unknown,
}

impl Command {
    pub(in crate::cloud) fn parse(action: Value, mut params: Map<String, Value>) -> Option<Self> {
        let name = action.as_str()?;
        if matches!(name, "pause_print" | "resume_print" | "cancel_print") && !params.is_empty() {
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
            Self::SetPrintQueuePaused { paused: true } => A::SetPrintQueuePaused { paused: true },
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
