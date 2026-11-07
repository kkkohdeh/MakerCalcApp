//! Optional local machine and equipment facts, shared by every integration.
use super::*;

pub(super) fn fields(detail: &DetailState) -> Vec<Field<'_>> {
    let mut rows = Vec::new();
    push(
        &mut rows,
        "printers.overview.reported_name",
        detail
            .machine
            .device
            .reported_name
            .as_deref()
            .map(Value::Text),
    );
    push(
        &mut rows,
        "printers.overview.activity",
        detail.machine.activity.map(Value::Activity),
    );
    push(
        &mut rows,
        "printers.overview.enclosure_closed",
        detail.machine.enclosure_closed.map(Value::Bool),
    );
    push(
        &mut rows,
        "printers.overview.material_station_slots",
        detail
            .machine
            .material_station_slots
            .map(|v| Value::Count(u64::from(v))),
    );
    for issue in &detail.machine.issues {
        let mut field = Field::new("printers.overview.printer_issue", Value::Issue(issue.kind));
        field.number = issue.supply_id.map(|id| id.0 as usize);
        rows.push(field);
    }
    for supply in &detail.machine.supplies {
        if let Some(step) = supply.operation.and_then(|operation| operation.step) {
            let mut field = Field::new(
                "printers.overview.operation_step",
                Value::Count(u64::from(step)),
            );
            field.number = Some(supply.reading.id.0 as usize);
            rows.push(field);
        }
    }
    let volumes = detail.storage.value.as_deref().unwrap_or_default();
    for (index, volume) in volumes.iter().enumerate() {
        for (key, value) in [
            ("printers.overview.storage_total", volume.total_bytes),
            ("printers.overview.storage_used", volume.used_bytes),
            ("printers.overview.storage_available", volume.free_bytes),
        ] {
            if let Some(value) = value {
                let mut field = Field::new(key, Value::Bytes(value));
                field.status = detail.storage.status;
                field.number = (volumes.len() > 1).then_some(index + 1);
                rows.push(field);
            }
        }
    }
    for camera in
        detail.cameras.value.iter().flatten().filter(|_| {
            detail.capabilities.cameras != crate::shared::workspace::Support::Unavailable
        })
    {
        let mut fields = vec![Field::new(
            "printers.overview.camera_name",
            Value::Text(&camera.display_name),
        )];
        push(
            &mut fields,
            "printers.overview.enabled_state",
            camera.enabled.map(Value::Bool),
        );
        push(
            &mut fields,
            "printers.overview.camera_rotation",
            camera.rotation_degrees.map(|v| Value::Count(u64::from(v))),
        );
        push(
            &mut fields,
            "printers.overview.camera_flip_horizontal",
            camera.flip_horizontal.map(Value::Bool),
        );
        push(
            &mut fields,
            "printers.overview.camera_flip_vertical",
            camera.flip_vertical.map(Value::Bool),
        );
        push(
            &mut fields,
            "printers.overview.camera_fps",
            camera.target_fps.map(|v| Value::Number(v, Unit::Number)),
        );
        rows.extend(fields.into_iter().map(|field| Field {
            name: Some(camera.display_name.clone()),
            status: detail.cameras.status,
            ..field
        }));
    }
    read_status(
        &mut rows,
        "printers.overview.storage",
        detail.storage.status,
    );
    read_status(&mut rows, "printers.overview.camera", detail.cameras.status);
    read_status(
        &mut rows,
        "printers.overview.firmware_state",
        detail.system_read.status,
    );
    mark_retained(&mut rows, detail.live);
    rows
}
