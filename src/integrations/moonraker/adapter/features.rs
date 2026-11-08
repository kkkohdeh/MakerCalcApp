//! Bounded projection of optional local features; no raw provider values escape.
use super::*;
use crate::shared::control::{MAX_PRINT_SPEED_OVERRIDE_PCT, MachineActionKind as K};
use crate::shared::gcode::{GcodeFacts, GcodeMaterialChannel};
use crate::shared::machine::{
    FanRole, MotionSource, PrintObject, SoftwareRole, SoftwareVersion, StorageVolume,
};

impl Projection {
    fn command_available(&self, name: &str, source: &raw::DetailState) -> bool {
        source
            .sections
            .get(COMMANDS)
            .filter(|r| r.status == raw::SectionStatus::Ready)
            .and_then(|r| r.data.as_ref())
            .and_then(|v| v.pointer("/status/gcode/commands"))
            .and_then(Value::as_object)
            .is_some_and(|commands| commands.contains_key(name))
    }

    pub(super) fn refresh_features(&mut self, source: &raw::DetailState) {
        let configfile = source
            .sections
            .get(CONFIGURATION)
            .filter(|r| r.status == raw::SectionStatus::Ready)
            .and_then(|r| r.data.as_ref())
            .and_then(|v| v.get("status"))
            .and_then(|v| v.get("configfile"));
        if let Some(value) = configfile
            && let Some(device) = self.device("configfile", value)
        {
            self.state.devices.push(device);
        }
        self.state.machine.actions.clear();
        self.state.machine.motion_idle = control::motor_motion_idle(source);
        self.state.machine.activity = (self.state.machine.motion_idle == Some(false))
            .then_some(crate::shared::machine::MachineActivity::Busy);
        if self.command_available("M18", source) {
            self.state.machine.actions.push(K::DisableMotors);
        }
        self.state.machine.manual_move_max_speed_mm_s = None;
        self.state.machine.speed_override_range_pct = self
            .command_available("M220", source)
            .then_some([1, MAX_PRINT_SPEED_OVERRIDE_PCT]);
        self.state.machine.z_offset_range_mm = self
            .command_available("SET_GCODE_OFFSET", source)
            .then_some([-10.0, 10.0]);
        self.state.machine.z_offset_mm = self
            .state
            .devices
            .iter()
            .find(|device| device.motion_source == Some(MotionSource::CommandInterpreter))
            .and_then(|device| device.readings.motion.as_ref()?.origin_offset_mm)
            .map(|[_, _, z]| z);
        self.state.machine.calibrations.clear();
        self.state.machine.calibration_readiness = Some(control::calibration_readiness(source));
        for (command, kind) in [
            (
                "BED_MESH_CALIBRATE",
                crate::shared::control::CalibrationKind::BedLeveling,
            ),
            (
                "SHAPER_CALIBRATE",
                crate::shared::control::CalibrationKind::VibrationCompensation,
            ),
        ] {
            if self.command_available(command, source) {
                self.state.machine.calibrations.push(kind);
            }
        }
        if self.command_available("G28", source) && self.command_available("M400", source) {
            self.state.machine.actions.push(K::HomeAxes);
        }
        if control::JOG_COMMANDS
            .iter()
            .all(|command| self.command_available(command, source))
        {
            self.state.machine.actions.push(K::MoveAxisRelative);
            self.state.machine.manual_move_max_speed_mm_s =
                control::manual_move_max_speed_mm_s(source);
        }
        for (command, kind) in [
            ("M220", K::SetPrintSpeedOverride),
            ("M221", K::SetFlowOverride),
            ("SET_GCODE_OFFSET", K::SetZOffset),
            ("SET_RETRACTION", K::SetRetraction),
        ] {
            if self.command_available(command, source) {
                self.state.machine.actions.push(kind);
            }
        }
        // Calibration support is additionally checked per calibration kind at dispatch.
        if self.command_available("BED_MESH_CALIBRATE", source)
            || self.command_available("SHAPER_CALIBRATE", source)
        {
            self.state.machine.actions.push(K::RunCalibration);
        }
        let manual_fan =
            self.command_available("M106", source) && self.command_available("M107", source);
        let generic_fan = self.command_available("SET_FAN_SPEED", source);
        let extrude = control::extrusion_supported(source);
        let pressure = self.command_available("SET_PRESSURE_ADVANCE", source);
        for device in &mut self.state.devices {
            device.actions.clear();
            let name = self
                .resources
                .iter()
                .find_map(|(r, id)| (*id == device.id).then_some(r));
            if let Some(Resource::Device(name)) = name {
                if name == "fan" {
                    device.fan_role = Some(FanRole::PartCooling);
                    if manual_fan {
                        device.actions.push(K::SetFanPower);
                    }
                } else if generic_fan && control::generic_fan_name(name).is_some() {
                    device.actions.push(K::SetFanPower);
                }
                device.output_control_range_pct =
                    device.actions.contains(&K::SetFanPower).then_some([0, 100]);
                if device.output_control_range_pct.is_some() {
                    device.output_at_full_control_pct = configfile
                        .and_then(|value| value.get("settings"))
                        .and_then(|settings| settings.get(name))
                        .and_then(Value::as_object)
                        .and_then(|settings| {
                            settings.get("max_power").map_or(Some(1.), Value::as_f64)
                        })
                        .filter(|&max| max.is_finite() && max > 0.0 && max <= 1.0)
                        .map(|max| max * 100.0);
                }
                // Automatic heater/controller fans remain read-only.
                // Enabling PWM is not an independent on/off switch.
                if device.kind == model::DeviceKind::Nozzle {
                    if extrude
                        && device
                            .readings
                            .extrusion
                            .as_ref()
                            .and_then(|state| state.active)
                            == Some(true)
                    {
                        device.actions.push(K::ExtrudeFilament);
                        device.extrusion_limit_mm = control::extrusion_limit_mm(source, name);
                    }
                    if pressure {
                        device.actions.push(K::SetPressureAdvance);
                    }
                }
            }
        }
        for device in self.state.power_devices.value.iter_mut().flatten() {
            device.actions = if device.enabled.is_some() {
                vec![K::SetPowerEnabled]
            } else {
                vec![]
            };
        }
        for device in self.state.lights.value.iter_mut().flatten() {
            device.actions = if device.enabled.is_some() {
                vec![K::SetLightBrightness]
            } else {
                vec![]
            };
            device.output_control_range_pct = device
                .actions
                .contains(&K::SetLightBrightness)
                .then_some([1, 100]);
        }
        if self.state.capabilities.queue == model::Support::Available {
            self.state.machine.actions.extend([
                K::EnqueuePrint,
                K::RemoveQueuedPrint,
                K::SetPrintQueuePaused,
            ]);
        }
        if self.state.capabilities.files == model::Support::Available {
            self.state.machine.actions.push(K::DeleteFile);
        }
        self.state.machine.software.clear();
        for (key, field, role) in [
            (SERVER, "moonraker_version", SoftwareRole::ConnectionServer),
            (FIRMWARE, "software_version", SoftwareRole::PrinterFirmware),
        ] {
            if let Some(version) = source
                .sections
                .get(key)
                .and_then(|r| r.data.as_ref())
                .and_then(|v| limited_string(v, field, 128))
            {
                self.state
                    .machine
                    .software
                    .push(SoftwareVersion { role, version });
            }
        }
        if let Some(entry) = source.sections.get(STORAGE) {
            let volume = entry
                .data
                .as_ref()
                .and_then(|v| v.get("disk_usage"))
                .map(|v| {
                    let total = v.get("total").and_then(Value::as_u64);
                    StorageVolume {
                        id: self.id(Resource::Storage),
                        total_bytes: total,
                        used_bytes: v
                            .get("used")
                            .and_then(Value::as_u64)
                            .filter(|n| total.is_none_or(|t| *n <= t)),
                        free_bytes: v
                            .get("free")
                            .and_then(Value::as_u64)
                            .filter(|n| total.is_none_or(|t| *n <= t)),
                    }
                });
            project_result(&mut self.state.storage, entry, volume.map(|v| vec![v]));
        }
        let job = source.objects.get("print_stats").and_then(|v| {
            Some((
                v.get("filename")?.as_str()?.to_owned(),
                positive(v, "total_duration")?,
            ))
        });
        let changed_job = match (&self.object_job, &job) {
            (Some((old, t)), Some((new, n))) => old != new || n < t,
            (None, None) => false,
            _ => true,
        };
        if changed_job {
            self.resources
                .retain(|r, _| !matches!(r, Resource::PrintObject(_)));
        }
        self.object_job = job;
        let Some(objects) = source.objects.get("exclude_object") else {
            self.state.print_objects = model::ReadResult::default();
            return;
        };
        let items = objects
            .get("objects")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(1000)
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?;
                        if !crate::shared::control::valid_label(name, 128) {
                            return None;
                        }
                        Some(PrintObject {
                            id: self.id(Resource::PrintObject(name.to_owned())),
                            display_name: clean(name),
                            excluded: objects
                                .get("excluded_objects")
                                .and_then(Value::as_array)
                                .map(|list| list.iter().any(|v| v.as_str() == Some(name))),
                            current: objects
                                .get("current_object")
                                .and_then(Value::as_str)
                                .map(|v| v == name),
                        })
                    })
                    .collect::<Vec<_>>()
            });
        let current: Vec<_> = items.iter().flatten().map(|o| o.id).collect();
        self.resources
            .retain(|r, id| !matches!(r, Resource::PrintObject(_)) || current.contains(id));
        self.state.print_objects = model::ReadResult {
            status: if items.is_some() {
                model::ReadStatus::Ready
            } else {
                model::ReadStatus::Error
            },
            value: items,
            error: None,
        };
        if self.command_available("EXCLUDE_OBJECT", source) && self.object_job.is_some() {
            self.state.machine.actions.push(K::ExcludePrintObject);
        }
    }
}

fn limited_string(v: &Value, key: &str, max: usize) -> Option<String> {
    v.get(key)?
        .as_str()
        .filter(|s| crate::shared::control::valid_label(s, max))
        .map(str::to_owned)
}

pub(super) fn metadata(v: &Value, facts: &mut GcodeFacts) {
    facts.slicer_version = facts
        .slicer_version
        .take()
        .or_else(|| limited_string(v, "slicer_version", 128));
    facts.print.max_z_height_mm = facts
        .print
        .max_z_height_mm
        .or_else(|| positive(v, "object_height"));
    facts.print.nozzle_diameter_mm = facts
        .print
        .nozzle_diameter_mm
        .or_else(|| positive(v, "nozzle_diameter").filter(|n| (0.05..=5.0).contains(n)));
    facts.print.first_layer_height_mm = facts
        .print
        .first_layer_height_mm
        .or_else(|| positive(v, "first_layer_height"));
    facts.print.chamber_temperature_c = facts
        .print
        .chamber_temperature_c
        .or_else(|| number(v, "chamber_temp").filter(|n| (-100.0..=300.0).contains(n)));
    facts.print.nozzle_temperature_c = facts
        .print
        .nozzle_temperature_c
        .or_else(|| positive(v, "first_layer_extr_temp"));
    facts.print.bed_temperature_c = facts
        .print
        .bed_temperature_c
        .or_else(|| positive(v, "first_layer_bed_temp"));
    if !facts.material_channels.is_empty() {
        return;
    }
    let Some(tools) = v
        .get("referenced_tools")
        .and_then(Value::as_array)
        .filter(|t| t.len() <= 32)
    else {
        return;
    };
    // Do not guess sparse tool/colour array correspondence. Only accept a verified
    // dense 0..N layout; other files keep aggregate metadata without invented slots.
    if !tools
        .iter()
        .enumerate()
        .all(|(i, t)| t.as_u64() == Some(i as u64))
    {
        return;
    }
    let colors = v
        .get("filament_colors")
        .and_then(Value::as_array)
        .filter(|a| a.len() == tools.len());
    let weights = v
        .get("filament_weights")
        .and_then(Value::as_array)
        .filter(|a| a.len() == tools.len());
    if colors.is_none() && weights.is_none() {
        return;
    }
    facts.material_channels = (0..tools.len())
        .map(|i| GcodeMaterialChannel {
            id: i as u32,
            material: None,
            color: colors
                .and_then(|a| a[i].as_str())
                .filter(|s| {
                    s.len() == 7
                        && s.starts_with('#')
                        && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
                })
                .map(str::to_owned),
            weight_g: weights
                .and_then(|a| a[i].as_f64())
                .filter(|v| v.is_finite() && *v >= 0.0),
        })
        .collect();
}

pub(super) fn merge_facts(facts: &mut GcodeFacts, fallback: &GcodeFacts) {
    facts.slicer_version = facts
        .slicer_version
        .take()
        .or_else(|| fallback.slicer_version.clone());
    if facts.material_channels.is_empty() {
        facts
            .material_channels
            .clone_from(&fallback.material_channels);
    }
    facts.print.max_z_height_mm = facts
        .print
        .max_z_height_mm
        .or(fallback.print.max_z_height_mm);
    facts.print.nozzle_diameter_mm = facts
        .print
        .nozzle_diameter_mm
        .or(fallback.print.nozzle_diameter_mm);
    facts.print.first_layer_height_mm = facts
        .print
        .first_layer_height_mm
        .or(fallback.print.first_layer_height_mm);
    facts.print.chamber_temperature_c = facts
        .print
        .chamber_temperature_c
        .or(fallback.print.chamber_temperature_c);
    facts.print.nozzle_temperature_c = facts
        .print
        .nozzle_temperature_c
        .or(fallback.print.nozzle_temperature_c);
    facts.print.bed_temperature_c = facts
        .print
        .bed_temperature_c
        .or(fallback.print.bed_temperature_c);
}
