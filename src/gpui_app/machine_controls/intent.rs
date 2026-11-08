use super::{Control, PrinterEntry};
use crate::shared::control::{Axis, ControlError, MachineAction};
use crate::shared::workspace::{Device, ResourceId};

pub(super) fn extrusion_device(printer: &PrinterEntry, id: ResourceId) -> Option<&Device> {
    printer
        .detail_state
        .devices
        .iter()
        .find(|device| device.id == id)
}

pub(super) fn action(
    printer: &PrinterEntry,
    control: Control,
    value: &str,
) -> Result<MachineAction, ControlError> {
    let number = || {
        value
            .trim()
            .parse::<f64>()
            .map_err(|_| ControlError::InvalidData)
    };
    let integer = || {
        value
            .trim()
            .parse::<u16>()
            .map_err(|_| ControlError::InvalidData)
    };
    let machine = &printer.detail_state.machine;
    let action = match control {
        Control::Speed => {
            let percent = integer()?;
            let [min, max] = machine
                .speed_override_range_pct
                .ok_or(ControlError::Unsupported)?;
            if !(min..=max).contains(&percent) {
                return Err(ControlError::InvalidData);
            }
            MachineAction::SetPrintSpeedOverride { percent }
        }
        Control::ZOffset => {
            let offset_mm = value
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .map_err(|_| ControlError::InvalidData)?;
            let [min, max] = machine.z_offset_range_mm.ok_or(ControlError::Unsupported)?;
            if !(min..=max).contains(&offset_mm) {
                return Err(ControlError::InvalidData);
            }
            MachineAction::SetZOffset { offset_mm }
        }
        Control::Fan(fan_id) => {
            let percent = integer()?;
            let range = printer
                .detail_state
                .devices
                .iter()
                .find(|d| d.id == fan_id)
                .and_then(|d| d.output_control_range_pct)
                .ok_or(ControlError::Unsupported)?;
            if !(u16::from(range[0])..=u16::from(range[1])).contains(&percent) {
                return Err(ControlError::InvalidData);
            }
            MachineAction::SetFanPower {
                fan_id,
                percent: u8::try_from(percent).map_err(|_| ControlError::InvalidData)?,
            }
        }
        Control::FanEnabled(fan_id, enabled) => MachineAction::SetFanEnabled { fan_id, enabled },
        Control::Camera(camera_id, enabled) => {
            MachineAction::SetCameraStreamEnabled { camera_id, enabled }
        }
        Control::Calibration(kind) => MachineAction::RunCalibration { kind },
        Control::Home => MachineAction::HomeAxes {
            axes: vec![Axis::X, Axis::Y, Axis::Z],
        },
        Control::Move(axis) => MachineAction::MoveAxisRelative {
            axis,
            distance_mm: number()?,
            speed_mm_s: None,
        },
        Control::Extrude(nozzle_id) => {
            let length_mm = value
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .map_err(|_| ControlError::InvalidData)?;
            let limit = extrusion_device(printer, nozzle_id)
                .and_then(|device| device.extrusion_limit_mm)
                .ok_or(ControlError::InvalidState)?;
            if length_mm.abs() > limit {
                return Err(ControlError::InvalidData);
            }
            MachineAction::ExtrudeFilament {
                nozzle_id,
                length_mm,
            }
        }
        Control::ClearPlate => MachineAction::ConfirmBuildPlateCleared,
        Control::AutoShutdown(enabled) => MachineAction::SetAutoShutdown {
            enabled,
            delay_s: if enabled {
                let seconds: u32 = value
                    .trim()
                    .parse()
                    .map_err(|_| ControlError::InvalidData)?;
                let step = machine
                    .auto_shutdown_delay_step_s
                    .ok_or(ControlError::Unsupported)?;
                if step == 0 || !seconds.is_multiple_of(step) {
                    return Err(ControlError::InvalidData);
                }
                seconds
            } else {
                0
            },
        },
        Control::Name => MachineAction::SetDeviceName {
            name: value.trim().into(),
        },
        Control::Acknowledge(alert_id) => MachineAction::AcknowledgePrinterAlert { alert_id },
    };
    action.validate()?;
    Ok(action)
}
