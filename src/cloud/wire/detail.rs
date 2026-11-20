use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::super::{Device, LOCAL_MAX_TELEMETRY_BYTES};
use crate::shared::control::MachineActionKind;
use crate::shared::machine::{FanRole, SoftwareRole, SupplyKind};
use crate::shared::telemetry::{ElementId, PrinterTelemetry, SupplyState};
use crate::shared::workspace::{DetailState, DeviceKind};

pub(super) const MAX_ELEMENTS: usize = 32;
pub(super) const MAX_LABEL_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 128;

type Fields = Map<String, Value>;

/// The allowlisted portion of the normalized detail that crosses the socket.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::cloud) struct WireDetail {
    telemetry: Fields,
    elements: BTreeMap<(&'static str, ElementId), Fields>,
    extra_elements: BTreeMap<&'static str, Vec<Value>>,
    device: Fields,
    pub(super) technical: Option<Value>,
}

impl WireDetail {
    pub(in crate::cloud) fn project(
        detail: &DetailState,
        telemetry: &PrinterTelemetry,
        job: Option<(u64, u64)>,
    ) -> Self {
        use DeviceKind as K;

        let mut out = Self::default();
        let machine = &detail.machine;
        let mut fans = BTreeMap::new();
        let mut environment = BTreeMap::new();
        let mut part_fans = BTreeMap::new();
        let mut dryers = BTreeMap::new();
        let mut lights = BTreeMap::new();
        let resources = detail
            .devices
            .iter()
            .chain(detail.lights.value.iter().flatten());
        let fan_ids = super::technical::unique_ids(
            resources
                .clone()
                .filter(|device| device.kind == K::Fan)
                .filter_map(|device| device.telemetry_id),
        );
        let light_ids = super::technical::unique_ids(
            resources
                .clone()
                .filter(|device| device.kind == K::Light)
                .filter_map(|device| device.telemetry_id),
        );

        for device in resources {
            let output = range(device.output_pct, 0.0, 100.0);
            let rpm = range(device.rpm, 0.0, f64::INFINITY);
            let display_name = label(&device.display_name, MAX_LABEL_BYTES);
            if device.kind == K::ChamberHeater
                && let Some(target) = finite(device.target_temperature_c)
            {
                out.telemetry
                    .entry("chamber_target_temperature_c")
                    .or_insert(json!(target));
            }
            let Some(id) = device.telemetry_id else {
                continue;
            };
            match device.kind {
                K::Fan if device.fan_role == Some(FanRole::PartCooling) => {
                    let actions = (fan_ids.get(&id) == Some(&1) && !device.actions.is_empty())
                        .then_some(&device.actions);
                    if let Some(actions) = actions {
                        out.elements.insert(
                            ("part_cooling_fans", id),
                            fields([("actions", json!(actions))]),
                        );
                    }
                    if output.is_some() || rpm.is_some() || actions.is_some() {
                        part_fans.entry(id).or_insert_with(|| json!({
                            "id": id, "PartCoolingFanPowerPct": output, "PartCoolingFanRpm": rpm,
                            "actions": actions.map_or(&[][..], Vec::as_slice),
                        }));
                    }
                }
                K::Fan => {
                    let actions = (fan_ids.get(&id) == Some(&1) && !device.actions.is_empty())
                        .then_some(&device.actions);
                    if output.is_some()
                        || rpm.is_some()
                        || device.enabled.is_some()
                        || actions.is_some()
                    {
                        fans.entry(id).or_insert_with(|| {
                            with_id(
                                id,
                                [
                                    ("fan_role", json!(device.fan_role)),
                                    ("display_name", json!(display_name)),
                                    ("output_pct", json!(output)),
                                    ("enabled", json!(device.enabled)),
                                    ("rpm", json!(rpm)),
                                    ("actions", json!(actions)),
                                ],
                            )
                        });
                    }
                }
                K::TemperatureSensor => {
                    let readings = device.readings.environment.as_ref();
                    let fields = fields([
                        ("temperature_c", json!(finite(device.temperature_c))),
                        (
                            "relative_humidity_pct",
                            json!(range(
                                readings.and_then(|r| r.relative_humidity_pct),
                                0.0,
                                100.0
                            )),
                        ),
                        (
                            "pressure_pa",
                            json!(range(
                                readings.and_then(|r| r.pressure_pa),
                                0.0,
                                f64::INFINITY
                            )),
                        ),
                        (
                            "gas_resistance_ohm",
                            json!(range(
                                readings.and_then(|r| r.gas_resistance_ohm),
                                0.0,
                                f64::INFINITY
                            )),
                        ),
                    ]);
                    if !fields.is_empty() {
                        environment.entry(id).or_insert_with(|| {
                            let mut fields = fields;
                            fields.insert("id".into(), json!(id));
                            if let Some(name) = display_name {
                                fields.insert("display_name".into(), json!(name));
                            }
                            Value::Object(fields)
                        });
                    }
                }
                K::Nozzle => {
                    out.elements.insert(
                        ("nozzles", id),
                        fields([
                            (
                                "nozzle_diameter_mm",
                                json!(range(device.nozzle_diameter_mm, 0.1, 2.0)),
                            ),
                            ("output_pct", json!(output)),
                        ]),
                    );
                }
                K::Bed | K::Light => {
                    let key = if device.kind == K::Bed {
                        "beds"
                    } else {
                        "lights"
                    };
                    let actions = (key == "lights"
                        && light_ids.get(&id) == Some(&1)
                        && !device.actions.is_empty())
                    .then_some(&device.actions);
                    let extra =
                        fields([("output_pct", json!(output)), ("actions", json!(actions))]);
                    if key == "lights"
                        && (actions.is_some() || output.is_some() || device.enabled.is_some())
                    {
                        let mut light = extra.clone();
                        light.extend(fields([("id", json!(id)), ("role", json!("unknown"))]));
                        light.insert("LightEnabled".into(), json!(device.enabled));
                        lights.entry(id).or_insert(Value::Object(light));
                    }
                    out.elements.insert((key, id), extra);
                }
                _ => {}
            }
        }
        for sensor in &telemetry.filament_sensors {
            if let Some(name) = sensor
                .name
                .as_deref()
                .and_then(|name| label(name, MAX_LABEL_BYTES))
            {
                out.elements.insert(
                    ("filament_sensors", sensor.id),
                    fields([("name", json!(name))]),
                );
            }
        }
        for supply in &machine.supplies {
            let operation = supply.operation.filter(|operation| {
                matches!(
                    operation.action,
                    MachineActionKind::LoadFilament
                        | MachineActionKind::UnloadFilament
                        | MachineActionKind::CancelFilamentOperation
                )
            });
            out.elements
                .entry(("filament_supplies", supply.reading.id))
                .or_insert_with(|| {
                    fields([
                        (
                            "kind",
                            json!((supply.kind != SupplyKind::Unknown).then_some(supply.kind)),
                        ),
                        (
                            "state",
                            json!(
                                (supply.reading.state != SupplyState::Unknown)
                                    .then_some(supply.reading.state)
                            ),
                        ),
                        (
                            "remaining_pct",
                            json!(supply.remaining_pct.filter(|pct| *pct <= 100)),
                        ),
                        ("operation", json!(operation)),
                    ])
                });
        }
        out.elements.retain(|_, fields| !fields.is_empty());

        for dryer in &machine.dryers {
            if let Some(label) = label(&dryer.label, MAX_LABEL_BYTES) {
                dryers.entry(dryer.telemetry_id).or_insert_with(|| {
                    with_id(
                        dryer.telemetry_id,
                        [
                            ("label", json!(label)),
                            ("state", json!(dryer.state)),
                            ("remaining_minutes", json!(dryer.remaining_minutes)),
                            ("target", json!(dryer.target)),
                        ],
                    )
                });
            }
        }
        for (key, values) in [
            ("fans", fans),
            ("environment_sensors", environment),
            ("dryers", dryers),
        ] {
            if !values.is_empty() {
                out.telemetry.insert(
                    key.into(),
                    Value::Array(values.into_values().take(MAX_ELEMENTS).collect()),
                );
            }
        }
        for (key, values) in [("part_cooling_fans", part_fans), ("lights", lights)] {
            if !values.is_empty() {
                out.extra_elements
                    .insert(key, values.into_values().take(MAX_ELEMENTS).collect());
            }
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
                "actions",
                json!((!machine.actions.is_empty()).then_some(&machine.actions)),
            ),
            (
                "speed_override_range_pct",
                json!(
                    machine
                        .speed_override_range_pct
                        .filter(|[min, max]| min <= max)
                ),
            ),
            (
                "z_offset_range_mm",
                json!(
                    machine
                        .z_offset_range_mm
                        .filter(|[min, max]| min.is_finite() && max.is_finite() && min <= max)
                ),
            ),
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
        let reported = motion
            .clone()
            .find(|device| device.position_mm.is_some_and(|v| valid_position(&v)));
        let commanded = |device: &crate::shared::workspace::Device| {
            device
                .readings
                .motion
                .as_ref()?
                .commanded_position_mm
                .filter(valid_position)
        };
        let commanded_source = if let Some(device) = reported {
            (commanded(device) == device.position_mm).then_some(device)
        } else {
            motion.clone().find(|device| commanded(device).is_some())
        };
        let position = reported
            .and_then(|device| device.position_mm)
            .or_else(|| commanded_source.and_then(commanded));
        out.technical = super::technical::project(
            detail,
            commanded_source,
            job.is_some() && detail.print_objects_job == job,
        );
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
        let firmware = device
            .firmware_version
            .as_deref()
            .and_then(|v| version_label(v, MAX_NAME_BYTES))
            .or_else(|| {
                machine
                    .software
                    .iter()
                    .filter(|version| version.role == SoftwareRole::PrinterFirmware)
                    .find_map(|version| version_label(&version.version, MAX_NAME_BYTES))
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
            ("firmware_version", json!(firmware)),
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
            .filter(|version| version_label(&version.version, 512).is_some())
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
        if self.telemetry.is_empty() && self.elements.is_empty() && self.extra_elements.is_empty() {
            return;
        }
        let Some(telemetry) = base.as_object_mut() else {
            return;
        };
        let mut original = Fields::new();
        for ((key, id), fields) in &self.elements {
            let Some(items) = telemetry.get_mut(*key).and_then(Value::as_array_mut) else {
                continue;
            };
            if !original.contains_key(*key) {
                original.insert((*key).into(), Value::Array(items.clone()));
            }
            if let Some(item) = items
                .iter_mut()
                .find(|item| element_id(item) == Some(u64::from(id.0)))
                && let Some(item) = item.as_object_mut()
            {
                item.extend(fields.clone());
            }
        }
        for (key, extra) in &self.extra_elements {
            let Some(items) = telemetry.get_mut(*key).and_then(Value::as_array_mut) else {
                continue;
            };
            original
                .entry(*key)
                .or_insert_with(|| Value::Array(items.clone()));
            let mut seen: BTreeSet<_> = items.iter().filter_map(element_id).collect();
            for item in extra {
                if items.len() < MAX_ELEMENTS && seen.insert(element_id(item).unwrap_or_default()) {
                    items.push(item.clone());
                }
            }
        }
        telemetry.extend(self.telemetry.clone());

        for key in [
            "environment_sensors",
            "fans",
            "dryers",
            "filament_sensors",
            "lights",
            "beds",
            "nozzles",
            "part_cooling_fans",
            "filament_supplies",
            "motion",
            "machine",
            "chamber_target_temperature_c",
        ] {
            if base.to_string().len() <= LOCAL_MAX_TELEMETRY_BYTES {
                break;
            }
            let telemetry = base.as_object_mut().expect("telemetry object");
            if let Some(original) = original.remove(key) {
                telemetry.insert(key.into(), original);
            } else if self.telemetry.contains_key(key) {
                telemetry.remove(key);
            }
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

fn with_id<const N: usize>(id: ElementId, values: [(&str, Value); N]) -> Value {
    let mut map = fields(values);
    map.insert("id".into(), json!(id));
    Value::Object(map)
}

fn insert_group(out: &mut Fields, key: &str, group: Fields) {
    if !group.is_empty() {
        out.insert(key.into(), Value::Object(group));
    }
}

pub(super) fn label(text: &str, max_bytes: usize) -> Option<&str> {
    (!text.trim().is_empty() && text.len() <= max_bytes && !text.chars().any(char::is_control))
        .then_some(text)
}

pub(super) fn version_label(text: &str, max_bytes: usize) -> Option<&str> {
    label(text, max_bytes).filter(|version| version.trim() != "?")
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

pub(super) fn bound_alert_ids(wire: &mut Value) {
    if !wire["PrinterAlerts"].is_array() {
        return;
    }
    let mut remaining = LOCAL_MAX_TELEMETRY_BYTES.saturating_sub(wire.to_string().len());
    let Some(alerts) = wire["PrinterAlerts"].as_array_mut() else {
        return;
    };
    let alert_id = |alert: &Value| u32::try_from(element_id(alert)?).ok().map(ElementId);
    for alert in alerts.iter_mut() {
        if alert_id(alert).is_none()
            && let (Some(category), Some(message)) =
                (alert["category"].as_str(), alert["message"].as_str())
        {
            let id = ElementId::from_key(category, message, 0);
            let bytes = r#","id":"#.len() + id.to_string().len();
            if remaining >= bytes {
                alert["id"] = json!(id);
                remaining -= bytes;
            }
        }
    }
    let counts = super::technical::unique_ids(alerts.iter().filter_map(alert_id));
    for alert in alerts {
        if let Some(id) = alert_id(alert)
            && counts.get(&id) != Some(&1)
            && let Some(fields) = alert.as_object_mut()
        {
            fields.remove("id");
        }
    }
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
