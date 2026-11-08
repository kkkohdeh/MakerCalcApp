//! Closed local command translation. No caller can submit a G-code/RPC string.
use super::*;
use crate::shared::control::{
    Axis, CalibrationKind, ControlError as E, MachineAction as A, MachineCommand,
};
use crate::state::{LanConnection, PrintStatus};
use std::time::Instant;

pub(super) const JOG_COMMANDS: [&str; 6] = [
    "SAVE_GCODE_STATE",
    "G91",
    "M220",
    "G1",
    "RESTORE_GCODE_STATE",
    "M400",
];

pub(super) fn motor_motion_idle(source: &raw::DetailState) -> Option<bool> {
    if !source.live {
        return None;
    }
    if ["manual_probe", "bed_screws"].iter().any(|name| {
        source
            .objects
            .get(*name)
            .and_then(|value| value.get("is_active"))
            .and_then(Value::as_bool)
            == Some(true)
    }) {
        return Some(false);
    }
    match source.objects.pointer("/idle_timeout/state")?.as_str()? {
        "Printing" => return Some(false),
        "Idle" | "Ready" => {}
        _ => return None,
    }
    let objects = source
        .sections
        .get("objects")
        .filter(|section| section.status == raw::SectionStatus::Ready)?
        .data
        .as_ref()?
        .get("objects")?
        .as_array()?;
    if !objects.iter().all(Value::is_string) {
        return None;
    }
    let mut unknown = false;
    for name in ["manual_probe", "bed_screws"] {
        if !objects.iter().any(|object| object.as_str() == Some(name)) {
            continue;
        }
        match source
            .objects
            .get(name)
            .and_then(|value| value.get("is_active"))
            .and_then(Value::as_bool)
        {
            Some(false) => {}
            _ => unknown = true,
        }
    }
    (!unknown).then_some(true)
}

pub(super) fn calibration_readiness(
    source: &raw::DetailState,
) -> crate::shared::machine::CalibrationReadiness {
    use crate::shared::machine::CalibrationReadiness;
    if motor_motion_idle(source) == Some(false) {
        return CalibrationReadiness::Busy;
    }
    match source
        .objects
        .pointer("/toolhead/homed_axes")
        .and_then(Value::as_str)
    {
        Some("xyz") => CalibrationReadiness::Ready,
        Some(axes) if axes.bytes().all(|axis| matches!(axis, b'x' | b'y' | b'z')) => {
            CalibrationReadiness::NeedsHoming
        }
        _ => CalibrationReadiness::NotReady,
    }
}

pub(super) fn manual_move_max_speed_mm_s(source: &raw::DetailState) -> Option<[f64; 3]> {
    if !source.live {
        return None;
    }
    let settings = source
        .sections
        .get(CONFIGURATION)
        .filter(|entry| entry.status == raw::SectionStatus::Ready)?
        .data
        .as_ref()?
        .pointer("/status/configfile/settings/printer")?;
    if !matches!(
        settings.get("kinematics")?.as_str()?,
        "cartesian" | "corexy"
    ) {
        return None;
    }
    let max = positive(source.objects.get("toolhead")?, "max_velocity")?;
    let z_max = positive(settings, "max_z_velocity")?;
    let limits = [max.min(75.), max.min(75.), z_max.min(max).min(75.)];
    limits.iter().all(|limit| *limit >= 5.).then_some(limits)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedControl {
    action: A,
    target: Option<Resource>,
    generation: u64,
    connection_epoch: u64,
    job_generation: u64,
    job_target: Option<crate::state::MachineJobTarget>,
    submitted_at: Instant,
    file_key: Option<file_estimates::FileKey>,
    job: Option<(String, f64)>,
}

impl PreparedControl {
    pub(in crate::integrations::moonraker) fn response_timeout(&self) -> Duration {
        match self.action {
            A::RunCalibration { .. } => Duration::from_secs(15 * 60),
            A::HomeAxes { .. } | A::MoveAxisRelative { .. } | A::ExtrudeFilament { .. } => {
                Duration::from_secs(2 * 60)
            }
            _ => super::super::REQUEST_TIMEOUT,
        }
    }

    pub(super) fn refresh_request(&self) -> Option<model::DetailRequest> {
        match self.action {
            A::DeleteFile { .. } => Some(model::DetailRequest::ListFiles),
            A::EnqueuePrint { .. }
            | A::RemoveQueuedPrint { .. }
            | A::SetPrintQueuePaused { .. } => Some(model::DetailRequest::Queue),
            A::SetPowerEnabled { .. } | A::SetLightBrightness { .. } => {
                Some(model::DetailRequest::Peripherals)
            }
            _ => None,
        }
    }
}

impl Projection {
    pub(super) fn prepare_control(
        &self,
        command: &MachineCommand,
        telemetry: &PrinterState,
        busy: bool,
    ) -> Result<PreparedControl, E> {
        if telemetry.connection != LanConnection::Connected {
            return Err(E::Disconnected);
        }
        if busy {
            return Err(E::Busy);
        }
        if command.connection_epoch != telemetry.connection_epoch
            || command
                .job_target
                .as_ref()
                .is_some_and(|job| !telemetry.matches_machine_job_target(job))
        {
            return Err(E::StaleResource);
        }
        if !self.state.live
            || self
                .observed_at
                .is_none_or(|at| at.elapsed() > Duration::from_secs(30))
        {
            return Err(E::InvalidState);
        }
        command.action.validate()?;
        let id = match command.action {
            A::ExtrudeFilament { nozzle_id, .. } | A::SetPressureAdvance { nozzle_id, .. } => {
                Some(nozzle_id)
            }
            A::SetFanPower { fan_id, .. } | A::SetFanEnabled { fan_id, .. } => Some(fan_id),
            A::DeleteFile { file_id }
            | A::RenameFile { file_id, .. }
            | A::EnqueuePrint { file_id } => Some(file_id),
            A::RemoveQueuedPrint { queue_item_id } => Some(queue_item_id),
            A::ExcludePrintObject { object_id } => Some(object_id),
            A::SetPowerEnabled { device_id, .. } => Some(device_id),
            A::SetLightBrightness { light_id, .. } => Some(light_id),
            _ => None,
        };
        let target = id
            .map(|id| self.resolve(id).cloned().ok_or(E::StaleResource))
            .transpose()?;
        let device_action = matches!(
            command.action,
            A::ExtrudeFilament { .. }
                | A::SetPressureAdvance { .. }
                | A::SetFanPower { .. }
                | A::SetPowerEnabled { .. }
                | A::SetLightBrightness { .. }
        );
        let supported = if device_action {
            self.state
                .devices
                .iter()
                .chain(self.state.power_devices.value.iter().flatten())
                .chain(self.state.lights.value.iter().flatten())
                .any(|d| Some(d.id) == id && d.actions.contains(&command.action.kind()))
        } else {
            self.state.machine.actions.contains(&command.action.kind())
        };
        if !supported {
            return Err(E::Unsupported);
        }
        if let A::RemoveQueuedPrint { queue_item_id } = command.action
            && !self
                .state
                .queue
                .value
                .as_ref()
                .is_some_and(|q| q.entries.iter().any(|e| e.id == queue_item_id))
        {
            return Err(E::StaleResource);
        }
        if let A::ExcludePrintObject { object_id } = command.action
            && !self.state.print_objects.value.as_ref().is_some_and(|o| {
                o.iter()
                    .any(|e| e.id == object_id && e.excluded == Some(false))
            })
        {
            return Err(E::StaleResource);
        }
        let prepared = PreparedControl {
            action: command.action.clone(),
            target,
            generation: self.state.generation,
            submitted_at: command.submitted_at,
            connection_epoch: telemetry.connection_epoch,
            job_generation: telemetry.job_generation,
            job_target: command.job_target.clone(),
            file_key: id.and_then(|id| file_estimates::FileKey::current(&self.state, id)),
            job: self.object_job.clone(),
        };
        let source = self.previous.as_ref().ok_or(E::InvalidState)?;
        build_request(0, &prepared, source, telemetry)?;
        Ok(prepared)
    }
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn need_command(source: &raw::DetailState, command: &str) -> Result<(), E> {
    source
        .sections
        .get(COMMANDS)
        .filter(|r| r.status == raw::SectionStatus::Ready)
        .and_then(|r| r.data.as_ref())
        .and_then(|v| v.pointer("/status/gcode/commands"))
        .and_then(Value::as_object)
        .is_some_and(|v| v.contains_key(command))
        .then_some(())
        .ok_or(E::Unsupported)
}

fn target_device(control: &PreparedControl) -> Result<&str, E> {
    match &control.target {
        Some(Resource::Device(name)) => Ok(name),
        _ => Err(E::StaleResource),
    }
}

fn target_file(control: &PreparedControl, source: &raw::DetailState) -> Result<String, E> {
    let Some(Resource::File(path)) = &control.target else {
        return Err(E::StaleResource);
    };
    let key = control.file_key.as_ref().ok_or(E::StaleResource)?;
    let file = source
        .sections
        .get(FILES)
        .filter(|r| r.status == raw::SectionStatus::Ready)
        .and_then(|r| r.data.as_ref())
        .and_then(Value::as_array)
        .and_then(|files| {
            files
                .iter()
                .find(|v| v.get("path").and_then(Value::as_str) == Some(path))
        })
        .ok_or(E::StaleResource)?;
    // A reused path is not sufficient: size and modification must still agree.
    let projected = model::PrinterFile {
        id: key.id,
        display_name: basename(path),
        size_bytes: file.get("size").and_then(Value::as_u64),
        modified_at_unix_s: positive(file, "modified"),
        estimated_duration_s: None,
        duration_read_status: model::ReadStatus::NotLoaded,
    };
    if projected.size_bytes.is_none() || projected.modified_at_unix_s.is_none() {
        return Err(E::InvalidState);
    }
    if file_estimates::FileKey::new(source.generation, &projected) != *key {
        return Err(E::StaleResource);
    }
    Ok(path.clone())
}

pub(in crate::integrations::moonraker) fn build_request(
    id: u64,
    control: &PreparedControl,
    source: &raw::DetailState,
    telemetry: &PrinterState,
) -> Result<Value, E> {
    if telemetry.connection != LanConnection::Connected {
        return Err(E::Disconnected);
    }
    if source.generation != control.generation {
        return Err(E::StaleResource);
    }
    if telemetry.connection_epoch != control.connection_epoch
        || telemetry.job_generation != control.job_generation
        || control
            .job_target
            .as_ref()
            .is_some_and(|job| !telemetry.matches_machine_job_target(job))
    {
        return Err(E::StaleResource);
    }
    if !telemetry.telemetry_ready
        || telemetry
            .last_valid_at
            .is_none_or(|at| at.elapsed() > Duration::from_secs(15))
    {
        return Err(E::InvalidState);
    }
    if !source.live || control.submitted_at.elapsed() > Duration::from_secs(2) {
        return Err(E::InvalidState);
    }
    control.action.validate()?;
    if matches!(
        &control.action,
        A::HomeAxes { .. }
            | A::MoveAxisRelative { .. }
            | A::ExtrudeFilament { .. }
            | A::RunCalibration { .. }
    ) && motor_motion_idle(source) == Some(false)
    {
        return Err(E::Busy);
    }
    let idle = matches!(
        telemetry.telemetry.print_state,
        PrintStatus::Idle | PrintStatus::Completed | PrintStatus::Cancelled
    );
    let printing = matches!(
        telemetry.telemetry.print_state,
        PrintStatus::Printing | PrintStatus::Paused
    );
    let active_or_idle = idle || printing;
    let objects = &source.objects;
    let require_idle = || idle.then_some(()).ok_or(E::InvalidState);
    let mut method = "printer.gcode.script";
    let mut script = None;
    let mut params = None;
    match &control.action {
        A::DisableMotors => {
            require_idle()?;
            need_command(source, "M18")?;
            if motor_motion_idle(source) != Some(true) {
                return Err(E::InvalidState);
            }
            script = Some("M18".into());
        }
        A::HomeAxes { axes } => {
            require_idle()?;
            need_command(source, "G28")?;
            need_command(source, "M400")?;
            let axes = axes
                .iter()
                .map(|a| match a {
                    Axis::X => "X",
                    Axis::Y => "Y",
                    Axis::Z => "Z",
                })
                .collect::<Vec<_>>()
                .join(" ");
            script = Some(format!("G28 {axes}\nM400"));
        }
        A::MoveAxisRelative {
            axis,
            distance_mm,
            speed_mm_s,
        } => {
            require_idle()?;
            for c in JOG_COMMANDS {
                need_command(source, c)?;
            }
            let index = match axis {
                Axis::X => 0,
                Axis::Y => 1,
                Axis::Z => 2,
            };
            let letter = ["X", "Y", "Z"][index];
            if let Some(speed) = speed_mm_s {
                let limits = manual_move_max_speed_mm_s(source).ok_or(E::Unsupported)?;
                if f64::from(*speed) > limits[index] {
                    return Err(E::InvalidData);
                }
            }
            let tool = objects.get("toolhead").ok_or(E::InvalidState)?;
            if !tool
                .get("homed_axes")
                .and_then(Value::as_str)
                .is_some_and(|v| v.contains(letter.to_ascii_lowercase().as_str()))
            {
                return Err(E::InvalidState);
            }
            let coordinate = |field| {
                tool.get(field)
                    .and_then(Value::as_array)
                    .and_then(|v| v.get(index))
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
                    .ok_or(E::InvalidState)
            };
            let dest = coordinate("position")? + distance_mm;
            if !(coordinate("axis_minimum")?..=coordinate("axis_maximum")?).contains(&dest) {
                return Err(E::InvalidData);
            }
            let feedrate = u16::from(speed_mm_s.unwrap_or(5)) * 60;
            script = Some(format!(
                "SAVE_GCODE_STATE NAME=MAKERCALC_CONTROL\nG91\nM220 S100\nG1 {letter}{distance_mm} F{feedrate}\nRESTORE_GCODE_STATE NAME=MAKERCALC_CONTROL\nM400"
            ));
        }
        A::ExtrudeFilament { length_mm, .. } => {
            require_idle()?;
            for c in [
                "SAVE_GCODE_STATE",
                "M83",
                "M220",
                "M221",
                "G1",
                "RESTORE_GCODE_STATE",
            ] {
                need_command(source, c)?;
            }
            let name = target_device(control)?;
            let nozzle = objects.get(name).ok_or(E::StaleResource)?;
            if objects
                .get("toolhead")
                .and_then(|v| v.get("extruder"))
                .and_then(Value::as_str)
                != Some(name)
                || nozzle.get("can_extrude").and_then(Value::as_bool) != Some(true)
            {
                return Err(E::InvalidState);
            }
            script = Some(format!(
                "SAVE_GCODE_STATE NAME=MAKERCALC_CONTROL\nM83\nM220 S100\nM221 S100\nG1 E{length_mm} F60\nRESTORE_GCODE_STATE NAME=MAKERCALC_CONTROL"
            ));
        }
        A::SetPrintSpeedOverride { percent } | A::SetFlowOverride { percent } => {
            if !active_or_idle {
                return Err(E::InvalidState);
            }
            let cmd = if matches!(control.action, A::SetFlowOverride { .. }) {
                "M221"
            } else {
                "M220"
            };
            need_command(source, cmd)?;
            script = Some(format!("{cmd} S{percent}"));
        }
        A::SetZOffset { offset_mm } => {
            if !active_or_idle {
                return Err(E::InvalidState);
            }
            need_command(source, "SET_GCODE_OFFSET")?;
            script = Some(format!("SET_GCODE_OFFSET Z={offset_mm}"));
        }
        A::SetFanPower { percent, .. } => {
            if !active_or_idle {
                return Err(E::InvalidState);
            }
            let name = target_device(control)?;
            if objects.get(name).is_none() {
                return Err(E::StaleResource);
            }
            script = Some(if name == "fan" {
                if *percent == 0 {
                    need_command(source, "M107")?;
                    "M107".into()
                } else {
                    need_command(source, "M106")?;
                    format!("M106 S{}", f64::from(*percent) * 255.0 / 100.0)
                }
            } else if let Some(name) = name.strip_prefix("fan_generic ").filter(|n| identifier(n)) {
                need_command(source, "SET_FAN_SPEED")?;
                format!(
                    "SET_FAN_SPEED FAN={name} SPEED={}",
                    f64::from(*percent) / 100.0
                )
            } else {
                return Err(E::Unsupported);
            });
        }
        A::SetPressureAdvance { advance_s, .. } => {
            if !active_or_idle {
                return Err(E::InvalidState);
            }
            need_command(source, "SET_PRESSURE_ADVANCE")?;
            let name = target_device(control)?;
            if !identifier(name)
                || objects
                    .get(name)
                    .and_then(|v| v.get("pressure_advance"))
                    .is_none()
            {
                return Err(E::Unsupported);
            }
            script = Some(format!(
                "SET_PRESSURE_ADVANCE EXTRUDER={name} ADVANCE={advance_s}"
            ));
        }
        A::SetRetraction {
            length_mm,
            speed_mm_s,
            extra_recovery_mm,
            recovery_speed_mm_s,
        } => {
            require_idle()?;
            need_command(source, "SET_RETRACTION")?;
            if objects.get("firmware_retraction").is_none() {
                return Err(E::Unsupported);
            }
            script = Some(format!(
                "SET_RETRACTION RETRACT_LENGTH={length_mm} RETRACT_SPEED={speed_mm_s} UNRETRACT_EXTRA_LENGTH={extra_recovery_mm} UNRETRACT_SPEED={recovery_speed_mm_s}"
            ));
        }
        A::RunCalibration { kind } => {
            require_idle()?;
            calibration_readiness(source).check()?;
            let cmd = match kind {
                CalibrationKind::BedLeveling => "BED_MESH_CALIBRATE",
                CalibrationKind::VibrationCompensation => "SHAPER_CALIBRATE",
            };
            need_command(source, cmd)?;
            script = Some(cmd.to_owned());
        }
        A::DeleteFile { .. } | A::EnqueuePrint { .. } => {
            require_idle()?;
            let path = target_file(control, source)?;
            match &control.action {
                A::DeleteFile { .. } => {
                    method = "server.files.delete_file";
                    params = Some(json!({"path":format!("gcodes/{path}")}));
                }
                _ => {
                    method = "server.job_queue.post_job";
                    params = Some(json!({"filenames":[path],"reset":false}));
                }
            }
        }
        // The source API overwrites destinations without atomic no-replace.
        // A stale local catalogue cannot make that safe.
        A::RenameFile { .. } => return Err(E::Unsupported),
        A::RemoveQueuedPrint { .. } => {
            require_idle()?;
            let Some(Resource::QueueItem(uid)) = &control.target else {
                return Err(E::StaleResource);
            };
            if !source
                .sections
                .get(QUEUE)
                .filter(|r| r.status == raw::SectionStatus::Ready)
                .and_then(|r| r.data.as_ref())
                .and_then(|v| v.get("queued_jobs"))
                .and_then(Value::as_array)
                .is_some_and(|a| {
                    a.iter()
                        .any(|v| v.get("job_id").and_then(Value::as_str) == Some(uid))
                })
            {
                return Err(E::StaleResource);
            }
            method = "server.job_queue.delete_job";
            params = Some(json!({"job_ids":[uid]}));
        }
        A::SetPrintQueuePaused { paused } => {
            if !active_or_idle {
                return Err(E::InvalidState);
            }
            method = if *paused {
                "server.job_queue.pause"
            } else {
                "server.job_queue.start"
            };
        }
        A::ExcludePrintObject { .. } => {
            if !printing {
                return Err(E::InvalidState);
            }
            need_command(source, "EXCLUDE_OBJECT")?;
            let Some(Resource::PrintObject(name)) = &control.target else {
                return Err(E::StaleResource);
            };
            let Some((file, duration)) = &control.job else {
                return Err(E::InvalidState);
            };
            let stats = objects.get("print_stats").ok_or(E::InvalidState)?;
            if stats.get("filename").and_then(Value::as_str) != Some(file)
                || positive(stats, "total_duration").is_none_or(|d| d < *duration)
            {
                return Err(E::StaleResource);
            }
            if !identifier(name)
                || !objects
                    .get("exclude_object")
                    .and_then(|v| v.get("objects"))
                    .and_then(Value::as_array)
                    .is_some_and(|a| {
                        a.iter()
                            .any(|v| v.get("name").and_then(Value::as_str) == Some(name))
                    })
            {
                return Err(E::StaleResource);
            }
            script = Some(format!("EXCLUDE_OBJECT NAME={name}"));
        }
        A::SetPowerEnabled { enabled, .. } => {
            require_idle()?;
            let target = target_device(control)?
                .strip_prefix("model:power:")
                .ok_or(E::StaleResource)?;
            if !source
                .sections
                .get(POWER)
                .filter(|r| r.status == raw::SectionStatus::Ready)
                .and_then(|r| r.data.as_ref())
                .and_then(|v| v.get("devices"))
                .and_then(Value::as_array)
                .is_some_and(|a| {
                    a.iter().any(|v| {
                        v.get("device").and_then(Value::as_str) == Some(target)
                            && matches!(v.get("status").and_then(Value::as_str), Some("on" | "off"))
                    })
                })
            {
                return Err(E::StaleResource);
            }
            method = "machine.device_power.post_device";
            params = Some(json!({"device":target,"action":if *enabled {"on"} else {"off"}}));
        }
        A::SetLightBrightness { percent, .. } => {
            if !active_or_idle {
                return Err(E::InvalidState);
            }
            if *percent == 0 {
                return Err(E::InvalidData);
            } // WLED cannot set zero without switching the strip off.
            let target = target_device(control)?
                .strip_prefix("model:lights:")
                .ok_or(E::StaleResource)?;
            if !source
                .sections
                .get(LIGHTS)
                .filter(|r| r.status == raw::SectionStatus::Ready)
                .and_then(|r| r.data.as_ref())
                .and_then(|v| v.get("strips"))
                .and_then(Value::as_object)
                .is_some_and(|a| {
                    a.values()
                        .any(|v| v.get("strip").and_then(Value::as_str) == Some(target))
                })
            {
                return Err(E::StaleResource);
            }
            method = "machine.wled.post_strip";
            params = Some(
                json!({"strip":target,"action":"control","brightness":(f64::from(*percent)*255.0/100.0).round() as u16}),
            );
        }
        _ => return Err(E::Unsupported),
    }
    if let Some(script) = script {
        params = Some(json!({"script":script}));
    }
    let mut value = json!({"jsonrpc":"2.0","id":id,"method":method});
    if let Some(params) = params {
        value["params"] = params;
    }
    Ok(value)
}
