//! Translation of live readings only; never fall back to startup configuration.
use super::{fraction, number, positive, string};
use crate::shared::{machine::*, workspace::DeviceKind};
use serde_json::Value;

pub(super) fn classify(name: &str) -> Option<(DeviceKind, &'static str)> {
    use DeviceKind as K;
    Some(match name {
        _ if [
            "bme280 ",
            "htu21d ",
            "sht3x ",
            "lm75 ",
            "temperature_host ",
            "temperature_combined ",
        ]
        .iter()
        .any(|p| name.starts_with(p)) =>
        {
            (K::TemperatureSensor, "Temperatura")
        }
        "toolhead" => (K::MotionSettings, "Límites de movimiento"),
        "gcode_move" => (K::MotionSettings, "Coordenadas de trabajo"),
        "firmware_retraction" => (K::Extrusion, "Retracción"),
        "bed_mesh" | "quad_gantry_level" | "z_tilt" => (K::Leveling, "Nivelación"),
        "probe" | "manual_probe" => (K::Probe, "Sonda"),
        "virtual_sdcard" | "pause_resume" | "idle_timeout" => (K::FileExecution, "Ejecución"),
        "hall_filament_width_sensor" => (K::FilamentSensor, "Diámetro de filamento"),
        "z_thermal_adjust" => (K::TemperatureSensor, "Compensación térmica"),
        "mcu" => (K::Controller, "Microcontrolador"),
        "configfile" => (K::Controller, "Configuración"),
        "system_stats" => (K::Controller, "Recursos del host"),
        _ if name.starts_with("extruder_stepper ") => (K::Extrusion, "Alimentador"),
        _ if name.starts_with("mcu ") || name.starts_with("canbus_stats ") => {
            (K::Controller, "Controlador")
        }
        _ if [
            "tmc2130 ", "tmc2208 ", "tmc2209 ", "tmc2240 ", "tmc2660 ", "tmc5160 ",
        ]
        .iter()
        .any(|p| name.starts_with(p)) =>
        {
            (K::Motor, "Motor")
        }
        _ => return None,
    })
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    v.get(key)?.as_bool()
}
fn count(v: &Value, key: &str) -> Option<u64> {
    v.get(key)?.as_u64()
}

fn coordinates<const N: usize>(v: &Value, key: &str) -> Option<[f64; N]> {
    let array = v.get(key)?.as_array()?;
    let mut result = [0.0; N];
    for (index, item) in result.iter_mut().enumerate() {
        *item = array.get(index)?.as_f64().filter(|v| v.is_finite())?;
    }
    Some(result)
}

// Overrides are multipliers, unlike PWM duty cycle: 200% is valid.
fn override_pct(v: &Value, key: &str) -> Option<f64> {
    positive(v, key)
        .map(|n| n * 100.0)
        .filter(|n| n.is_finite())
}

fn grid(v: &Value, key: &str) -> Option<Vec<Vec<f64>>> {
    let rows = v.get(key)?.as_array()?;
    if rows.len() > 64 {
        return None;
    }
    let width = rows.first().and_then(Value::as_array).map_or(0, Vec::len);
    if width > 64 {
        return None;
    }
    rows.iter()
        .map(|row| {
            let cells = row.as_array()?;
            if cells.len() != width {
                return None;
            }
            cells
                .iter()
                .map(|v| v.as_f64().filter(|n| n.is_finite()))
                .collect()
        })
        .collect()
}

pub(super) fn populate(name: &str, v: &Value, result: &mut MachineReadings) {
    if ["bme280 ", "htu21d ", "sht3x "]
        .iter()
        .any(|p| name.starts_with(p))
    {
        result.environment = Some(EnvironmentalReadings {
            relative_humidity_pct: number(v, "humidity").filter(|n| (0.0..=100.0).contains(n)),
            pressure_pa: name
                .starts_with("bme280 ")
                .then(|| {
                    positive(v, "pressure")
                        .map(|n| n * 100.0)
                        .filter(|n| n.is_finite())
                })
                .flatten(),
            gas_resistance_ohm: name
                .starts_with("bme280 ")
                .then(|| positive(v, "gas"))
                .flatten(),
        });
    }
    if name == "extruder"
        || name.starts_with("extruder_stepper ")
        || name
            .strip_prefix("extruder")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        result.extrusion = Some(Extrusion {
            pressure_advance_s: positive(v, "pressure_advance"),
            pressure_smoothing_s: positive(v, "smooth_time"),
            ..Default::default()
        });
    }
    match name {
        "motion_report" => {
            result.extrusion = Some(Extrusion {
                feed_velocity_mm_s: number(v, "live_extruder_velocity"),
                ..Default::default()
            })
        }
        "toolhead" => {
            result.motion = Some(MotionSettings {
                commanded_position_mm: coordinates(v, "position"),
                travel_min_mm: coordinates(v, "axis_minimum"),
                travel_max_mm: coordinates(v, "axis_maximum"),
                velocity_limit_mm_s: positive(v, "max_velocity"),
                acceleration_limit_mm_s2: positive(v, "max_accel"),
                corner_velocity_mm_s: positive(v, "square_corner_velocity"),
                minimum_cruise_pct: fraction(v, "minimum_cruise_ratio"),
                homed_axes: v
                    .get("homed_axes")
                    .and_then(Value::as_str)
                    .filter(|s| s.chars().all(|c| "xyz".contains(c)))
                    .map(|s| ['x', 'y', 'z'].map(|axis| s.contains(axis))),
                stalls: count(v, "stalls"),
                ..Default::default()
            })
        }
        "gcode_move" => {
            result.motion = Some(MotionSettings {
                commanded_position_mm: coordinates(v, "position"),
                work_position_mm: coordinates(v, "gcode_position"),
                origin_offset_mm: coordinates(v, "homing_origin"),
                // get_status returns the G-code F value, in mm/min, before M220.
                requested_velocity_mm_s: positive(v, "speed").map(|speed| speed / 60.),
                speed_override_pct: override_pct(v, "speed_factor"),
                flow_override_pct: override_pct(v, "extrude_factor"),
                absolute_positioning: flag(v, "absolute_coordinates"),
                absolute_extrusion: flag(v, "absolute_extrude"),
                ..Default::default()
            })
        }
        "firmware_retraction" => {
            result.retraction = Some(Retraction {
                length_mm: positive(v, "retract_length"),
                speed_mm_s: positive(v, "retract_speed"),
                extra_recovery_mm: number(v, "unretract_extra_length"),
                recovery_speed_mm_s: positive(v, "unretract_speed"),
            })
        }
        "bed_mesh" => {
            result.leveling = Some(Leveling {
                profile_name: string(v, "profile_name"),
                area_min_mm: coordinates(v, "mesh_min"),
                area_max_mm: coordinates(v, "mesh_max"),
                measured_grid_mm: grid(v, "probed_matrix"),
                compensation_grid_mm: grid(v, "mesh_matrix"),
                ..Default::default()
            })
        }
        "quad_gantry_level" | "z_tilt" => {
            result.leveling = Some(Leveling {
                applied: flag(v, "applied"),
                ..Default::default()
            })
        }
        "probe" => {
            result.probe = Some(ProbeReadings {
                last_triggered: flag(v, "last_query"),
                last_position_mm: coordinates(v, "last_probe_position"),
                last_height_mm: coordinates::<3>(v, "last_probe_position")
                    .map(|p| p[2])
                    .or_else(|| number(v, "last_z_result")),
                ..Default::default()
            })
        }
        "manual_probe" => {
            result.probe = Some(ProbeReadings {
                manual_active: flag(v, "is_active"),
                manual_height_mm: number(v, "z_position"),
                lower_height_mm: number(v, "z_position_lower"),
                upper_height_mm: number(v, "z_position_upper"),
                ..Default::default()
            })
        }
        "virtual_sdcard" => {
            result.file_execution = Some(FileExecution {
                active: flag(v, "is_active"),
                read_offset_bytes: count(v, "file_position"),
                file_size_bytes: count(v, "file_size"),
                progress_pct: fraction(v, "progress"),
                ..Default::default()
            })
        }
        "pause_resume" => {
            result.file_execution = Some(FileExecution {
                paused: flag(v, "is_paused"),
                ..Default::default()
            })
        }
        "idle_timeout" => {
            result.file_execution = Some(FileExecution {
                idle_timeout_s: positive(v, "idle_timeout"),
                ..Default::default()
            })
        }
        "hall_filament_width_sensor" => {
            result.filament = Some(FilamentReadings {
                diameter_mm: positive(v, "Diameter"),
                width_sensor_active: flag(v, "is_active"),
                flow_compensation_enabled: flag(v, "flow_compensation_enabled"),
            })
        }
        "z_thermal_adjust" => {
            result.thermal = Some(ThermalReadings {
                reference_c: number(v, "z_adjust_ref_temperature"),
                z_adjustment_mm: number(v, "current_z_adjust"),
                minimum_c: number(v, "measured_min_temp"),
                maximum_c: number(v, "measured_max_temp"),
                z_adjustment_enabled: flag(v, "enabled"),
            })
        }
        "configfile" => {
            result.controller = Some(ControllerReadings {
                configuration_pending: flag(v, "save_config_pending"),
                ..Default::default()
            })
        }
        "system_stats" => {
            result.controller = Some(ControllerReadings {
                host_load_1m: positive(v, "sysload"),
                process_cpu_time_s: positive(v, "cputime"),
                available_memory_bytes: count(v, "memavail").and_then(|n| n.checked_mul(1024)),
                ..Default::default()
            })
        }
        _ => {}
    }
    if name.starts_with("temperature_sensor ") {
        result.thermal = Some(ThermalReadings {
            minimum_c: number(v, "measured_min_temp"),
            maximum_c: number(v, "measured_max_temp"),
            ..Default::default()
        });
    }
    if matches!(classify(name), Some((DeviceKind::Motor, _))) {
        result.motor = Some(MotorReadings {
            run_current_a: positive(v, "run_current"),
            hold_current_a: positive(v, "hold_current"),
        });
    }
    if name == "mcu" || name.starts_with("mcu ") {
        result.controller = Some(ControllerReadings {
            firmware_version: string(v, "mcu_version"),
            ..Default::default()
        });
    }
    if name.starts_with("canbus_stats ") {
        result.controller = Some(ControllerReadings {
            receive_errors: count(v, "rx_error"),
            transmit_errors: count(v, "tx_error"),
            transmit_retries: count(v, "tx_retries"),
            ..Default::default()
        });
    }
}
