use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::super::{Device, LOCAL_MAX_TELEMETRY_BYTES};
use crate::shared::machine::SoftwareRole;
use crate::shared::workspace::{DetailState, DeviceKind};

const MAX_ELEMENTS: usize = 32;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 128;

type Fields = Map<String, Value>;

/// The allowlisted portion of the normalized detail that crosses the socket.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::cloud) struct WireDetail {
    telemetry: Fields,
    device: Fields,
}

impl WireDetail {
    pub(in crate::cloud) fn project(detail: &DetailState) -> Self {
        use DeviceKind as K;

        let mut out = Self::default();
        let machine = &detail.machine;
        if let Some(target) = detail
            .devices
            .iter()
            .filter(|device| device.kind == K::ChamberHeater)
            .find_map(|device| finite(device.target_temperature_c))
        {
            out.telemetry
                .insert("chamber_target_temperature_c".into(), json!(target));
        }

        let issues = machine.issues.iter().take(MAX_ELEMENTS).collect::<Vec<_>>();
        let mut machine_fields = fields([
            ("z_offset_mm", json!(finite(machine.z_offset_mm))),
            (
                "auto_shutdown_enabled",
                json!(machine.auto_shutdown_enabled),
            ),
            ("enclosure_closed", json!(machine.enclosure_closed)),
            (
                "material_station_slots",
                json!(machine.material_station_slots.filter(|count| *count > 0)),
            ),
            ("activity", json!(machine.activity)),
            (
                "print_speed_profile",
                json!(
                    machine
                        .print_speed_profiles
                        .map(|profiles| profiles.current)
                ),
            ),
        ]);
        if !issues.is_empty() {
            machine_fields.insert("issues".into(), json!(issues));
        }
        insert_group(&mut out.telemetry, "machine", machine_fields);

        let motion = detail
            .devices
            .iter()
            .filter(|device| matches!(device.kind, K::Motion | K::MotionSettings));
        let position = motion
            .clone()
            .find_map(|d| d.position_mm.filter(valid_position))
            .or_else(|| {
                motion.clone().find_map(|d| {
                    d.readings
                        .motion
                        .as_ref()?
                        .commanded_position_mm
                        .filter(valid_position)
                })
            });
        let settings = motion.filter_map(|device| device.readings.motion.as_ref());
        insert_group(
            &mut out.telemetry,
            "motion",
            fields([
                ("position_mm", json!(position)),
                (
                    "homed_axes",
                    json!(settings.clone().find_map(|s| s.homed_axes)),
                ),
                (
                    "flow_override_pct",
                    json!(settings.clone().find_map(|s| range(
                        s.flow_override_pct,
                        0.0,
                        f64::INFINITY
                    ))),
                ),
            ]),
        );

        let device = &machine.device;
        let firmware = device.firmware_version.as_deref().or_else(|| {
            machine
                .software
                .iter()
                .find(|version| version.role == SoftwareRole::PrinterFirmware)
                .map(|version| version.version.as_str())
        });
        out.device = fields([
            (
                "reported_name",
                json!(
                    device
                        .reported_name
                        .as_deref()
                        .and_then(|v| label(v, MAX_NAME_BYTES))
                ),
            ),
            (
                "manufacturer",
                json!(
                    device
                        .manufacturer
                        .as_deref()
                        .and_then(|v| label(v, MAX_NAME_BYTES))
                ),
            ),
            (
                "model",
                json!(
                    device
                        .model
                        .as_deref()
                        .and_then(|v| label(v, MAX_NAME_BYTES))
                ),
            ),
            (
                "firmware_version",
                json!(firmware.and_then(|v| label(v, MAX_NAME_BYTES))),
            ),
            (
                "location_label",
                json!(
                    device
                        .location_label
                        .as_deref()
                        .and_then(|v| label(v, MAX_NAME_BYTES))
                ),
            ),
            (
                "build_volume_mm",
                json!(
                    device
                        .build_volume_mm
                        .filter(|dims| dims.iter().all(|x| (1.0..=10_000.0).contains(x)))
                ),
            ),
            (
                "nozzle_count",
                json!(device.nozzle_count.filter(|count| (1..=16).contains(count))),
            ),
        ]);
        let software: Vec<_> = machine
            .software
            .iter()
            .filter(|version| label(&version.version, 512).is_some())
            .take(MAX_ELEMENTS)
            .collect();
        if !software.is_empty() {
            out.device.insert("software".into(), json!(software));
        }
        out
    }

    pub(in crate::cloud) fn device_json(&self, device: Option<&Device>) -> Option<Value> {
        if device.is_none() && self.device.is_empty() {
            return None;
        }
        let mut out = self.device.clone();
        if let Some(device) = device {
            let suffix = device.serial_suffix.as_deref().filter(|suffix| {
                !suffix.is_empty()
                    && suffix.len() <= 4
                    && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
            });
            let local_ip = device.local_ip.filter(|ip| match ip {
                std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
                std::net::IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
            });
            out.insert("serial_suffix".into(), json!(suffix));
            out.insert("local_ip".into(), json!(local_ip));
        }
        Some(Value::Object(out))
    }

    pub(in crate::cloud) fn extend_telemetry(&self, base: &mut Value) {
        if self.telemetry.is_empty() {
            return;
        }
        let Some(telemetry) = base.as_object_mut() else {
            return;
        };
        telemetry.extend(self.telemetry.clone());
        for key in ["motion", "machine", "chamber_target_temperature_c"] {
            if base.to_string().len() <= LOCAL_MAX_TELEMETRY_BYTES {
                break;
            }
            base.as_object_mut().expect("telemetry object").remove(key);
        }
    }
}

fn fields<const N: usize>(values: [(&str, Value); N]) -> Fields {
    values
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.into(), v))
        .collect()
}

fn insert_group(out: &mut Fields, key: &str, group: Fields) {
    if !group.is_empty() {
        out.insert(key.into(), Value::Object(group));
    }
}

fn label(text: &str, max_bytes: usize) -> Option<&str> {
    (!text.trim().is_empty() && text.len() <= max_bytes && !text.chars().any(char::is_control))
        .then_some(text)
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

fn range(value: Option<f64>, min: f64, max: f64) -> Option<f64> {
    finite(value).filter(|v| (min..=max).contains(v))
}

fn valid_position(position: &[f64; 3]) -> bool {
    position.iter().all(|axis| axis.is_finite())
}

pub(super) fn element_id(element: &Value) -> Option<u64> {
    element.get("id")?.as_u64()
}

pub(super) fn bound_base_telemetry(wire: &mut Value) {
    for key in [
        "nozzles",
        "beds",
        "part_cooling_fans",
        "filament_sensors",
        "filament_supplies",
        "lights",
    ] {
        if let Some(items) = wire.get_mut(key).and_then(Value::as_array_mut) {
            let mut seen = BTreeSet::new();
            items.retain(|item| {
                seen.len() < MAX_ELEMENTS && element_id(item).is_some_and(|id| seen.insert(id))
            });
        }
    }
    if let Some(alerts) = wire.get_mut("PrinterAlerts").and_then(Value::as_array_mut) {
        alerts.retain(|alert| {
            alert["category"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= MAX_NAME_BYTES)
                && alert["message"].as_str().is_some_and(|s| s.len() <= 512)
        });
        alerts.truncate(MAX_ELEMENTS);
    }
    if let Some(supplies) = wire
        .get_mut("filament_supplies")
        .and_then(Value::as_array_mut)
    {
        for supply in supplies {
            if supply["FilamentMaterial"]
                .as_str()
                .is_some_and(|text| text.len() > MAX_LABEL_BYTES)
            {
                supply["FilamentMaterial"] = Value::Null;
            }
            if supply["FilamentColor"].as_str().is_some_and(|color| {
                color.len() != 7
                    || !color.starts_with('#')
                    || !color.bytes().skip(1).all(|b| b.is_ascii_hexdigit())
            }) {
                supply["FilamentColor"] = Value::Null;
            }
        }
    }
    for key in ["current_job", "last_job"] {
        if let Some(job) = wire.get_mut(key).and_then(Value::as_object_mut)
            && job
                .get("PrintName")
                .and_then(Value::as_str)
                .is_some_and(|name| name.len() > MAX_LABEL_BYTES)
        {
            job.insert("PrintName".into(), Value::Null);
        }
    }
}
