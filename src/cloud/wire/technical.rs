use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::detail::{MAX_ELEMENTS, MAX_LABEL_BYTES, label};
use crate::shared::telemetry::ElementId;
use crate::shared::workspace::{DetailState, Device, ReadStatus};

const MAX_BYTES: usize = 24 * 1024;
const MAX_GRID_SIDE: usize = 64;

pub(in crate::cloud) fn unique_ids(
    ids: impl Iterator<Item = ElementId>,
) -> BTreeMap<ElementId, usize> {
    let mut counts = BTreeMap::new();
    for id in ids {
        *counts.entry(id).or_default() += 1;
    }
    counts
}

pub(super) fn project(
    detail: &DetailState,
    commanded_source: Option<&Device>,
    objects_match_job: bool,
) -> Option<Value> {
    let mut out = Map::new();
    if let Some(ready) = detail.system.firmware_ready {
        out.insert("firmware_ready".into(), json!(ready));
    }
    let devices = detail.devices.iter().filter_map(|device| {
        let name = label(&device.display_name, MAX_LABEL_BYTES)?;
        let mut readings = device.readings.clone();
        readings.environment = None;
        if let Some(motion) = &mut readings.motion {
            motion.homed_axes = None;
            motion.flow_override_pct = None;
            if commanded_source.is_some_and(|source| std::ptr::eq(source, device)) {
                motion.commanded_position_mm = None;
            }
        }
        if let Some(leveling) = &mut readings.leveling {
            leveling.profile_name = leveling
                .profile_name
                .take()
                .filter(|name| label(name, MAX_LABEL_BYTES).is_some());
            for grid in [
                &mut leveling.measured_grid_mm,
                &mut leveling.compensation_grid_mm,
            ] {
                if grid.as_ref().is_some_and(|rows| {
                    rows.len() > MAX_GRID_SIDE || rows.iter().any(|row| row.len() > MAX_GRID_SIDE)
                }) {
                    *grid = None;
                }
            }
        }
        if let Some(controller) = &mut readings.controller {
            controller.firmware_version = controller
                .firmware_version
                .take()
                .filter(|version| super::detail::version_label(version, MAX_LABEL_BYTES).is_some());
        }
        let mut readings = serde_json::to_value(readings).ok()?;
        if let Some(motion) = readings.get_mut("motion").and_then(Value::as_object_mut) {
            motion.remove("homed_axes");
            motion.remove("flow_override_pct");
        }
        if device.can_extrude.is_none() && readings.as_object().is_some_and(Map::is_empty) {
            return None;
        }
        Some(json!({
            "kind": device.kind,
            "display_name": name,
            "can_extrude": device.can_extrude,
            "readings": readings,
        }))
    });
    insert_rows(&mut out, "devices", devices);
    insert_rows(&mut out, "power_devices", detail.power_devices.value.iter().flatten().filter_map(|device| {
        Some(json!({"display_name": label(&device.display_name, MAX_LABEL_BYTES)?, "enabled": device.enabled}))
    }));
    if let Some(queue) = &detail.queue.value {
        let counts = unique_ids(queue.entries.iter().filter_map(|entry| entry.telemetry_id));
        let mut value = Map::new();
        value.insert("status".into(), json!(queue.status));
        insert_rows(
            &mut value,
            "entries",
            queue.entries.iter().filter_map(|entry| {
                let mut row = json!({
                    "display_name": file_label(&entry.display_name)?,
                    "added_at_unix_s": entry.added_at_unix_s,
                    "waiting_s": entry.waiting_s,
                });
                if detail.queue.status == ReadStatus::Ready
                    && let Some(id) = entry.telemetry_id
                    && counts.get(&id) == Some(&1)
                {
                    row["id"] = json!(id);
                }
                Some(row)
            }),
        );
        if !value.contains_key("entries") {
            insert_rows(
                &mut value,
                "file_names",
                queue
                    .file_names
                    .iter()
                    .filter_map(|name| file_label(name).map(|name| json!(name))),
            );
        }
        out.insert("queue".into(), Value::Object(value));
    }
    if let Some(objects) = &detail.print_objects.value {
        let counts = unique_ids(objects.iter().filter_map(|object| object.telemetry_id));
        let rows = objects
            .iter()
            .filter_map(|object| {
                let mut row = json!({
                    "display_name": label(&object.display_name, MAX_LABEL_BYTES)?,
                    "excluded": object.excluded,
                    "current": object.current,
                });
                if objects_match_job
                    && let Some(id) = object.telemetry_id
                    && counts.get(&id) == Some(&1)
                {
                    row["id"] = json!(id);
                }
                Some(row)
            })
            .take(MAX_ELEMENTS)
            .collect();
        out.insert("print_objects".into(), Value::Array(rows));
    }
    insert_rows(
        &mut out,
        "storage",
        detail.storage.value.iter().flatten().map(|volume| {
            json!({
                "total_bytes": volume.total_bytes,
                "used_bytes": volume.used_bytes,
                "free_bytes": volume.free_bytes,
            })
        }),
    );
    insert_rows(
        &mut out,
        "cameras",
        detail.cameras.value.iter().flatten().filter_map(|camera| {
            Some(json!({
                "display_name": label(&camera.display_name, MAX_LABEL_BYTES)?,
                "enabled": camera.enabled,
                "rotation_degrees": camera.rotation_degrees,
                "flip_horizontal": camera.flip_horizontal,
                "flip_vertical": camera.flip_vertical,
                "target_fps": camera.target_fps,
            }))
        }),
    );
    bounded(out)
}

fn insert_rows(out: &mut Map<String, Value>, key: &str, rows: impl Iterator<Item = Value>) {
    let rows: Vec<_> = rows.take(MAX_ELEMENTS).collect();
    if !rows.is_empty() {
        out.insert(key.into(), Value::Array(rows));
    }
}

fn file_label(path: &str) -> Option<&str> {
    let mut name = path.rsplit(['/', '\\']).next()?;
    if let [drive, b':', ..] = name.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        name = &name[2..];
    }
    label(name, MAX_LABEL_BYTES)
}

fn bounded(mut out: Map<String, Value>) -> Option<Value> {
    let bytes = |out: &Map<String, Value>| serde_json::to_vec(out).map_or(usize::MAX, |v| v.len());
    if bytes(&out) > MAX_BYTES
        && let Some(devices) = out.get_mut("devices").and_then(Value::as_array_mut)
    {
        for device in devices {
            if let Some(leveling) = device
                .pointer_mut("/readings/leveling")
                .and_then(Value::as_object_mut)
            {
                leveling.insert("measured_grid_mm".into(), Value::Null);
                leveling.insert("compensation_grid_mm".into(), Value::Null);
            }
        }
    }
    for key in [
        "devices",
        "cameras",
        "power_devices",
        "storage",
        "print_objects",
        "queue",
    ] {
        while bytes(&out) > MAX_BYTES {
            if let Some(rows) = out.get_mut(key).and_then(Value::as_array_mut)
                && rows.pop().is_some()
            {
                continue;
            }
            out.remove(key);
            break;
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}
