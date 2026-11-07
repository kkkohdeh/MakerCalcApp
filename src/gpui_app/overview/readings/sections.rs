//! Presentation groups retain resource identity before the readings become text.
use super::{Field, Group, Readings, Unit, Value, presentation};
use crate::gpui_app::overview::layout::{Block, Shape};
use crate::i18n::t;
use crate::shared::workspace::{Device, DeviceKind, ResourceId};

pub(super) struct Section<'a> {
    pub(super) resource: Option<ResourceId>,
    pub(super) name: Option<&'a str>,
    pub(super) number: Option<usize>,
    pub(super) indices: Vec<usize>,
}

/// Combine heater readings only through a unique, provider-verified identity.
pub(super) fn remaining_heaters(
    telemetry: &crate::shared::telemetry::PrinterTelemetry,
    detail: &crate::shared::workspace::DetailState,
    locale: &str,
) -> Vec<Field<'static>> {
    let mut fields = Vec::new();
    let nozzles = telemetry.nozzles.iter().enumerate().map(|(index, nozzle)| {
        (
            "printers.overview.nozzle",
            DeviceKind::Nozzle,
            nozzle.id,
            telemetry
                .nozzles
                .iter()
                .filter(|other| other.id == nozzle.id)
                .count()
                == 1,
            index,
            nozzle.is_active,
            nozzle.nozzle_temp_c,
            nozzle.nozzle_target_temp_c,
        )
    });
    let beds = telemetry.beds.iter().enumerate().map(|(index, bed)| {
        (
            "printers.overview.bed",
            DeviceKind::Bed,
            bed.id,
            telemetry
                .beds
                .iter()
                .filter(|other| other.id == bed.id)
                .count()
                == 1,
            index,
            telemetry
                .primary_bed()
                .is_some_and(|primary| std::ptr::eq(primary, bed)),
            bed.bed_temp_c,
            bed.bed_target_temp_c,
        )
    });
    for (label, kind, id, unique, index, shown, current, target) in nozzles.chain(beds) {
        let mut matching = detail.devices.iter().filter(|device| {
            detail.live && unique && device.kind == kind && device.telemetry_id == Some(id)
        });
        let linked = matching.next().filter(|_| matching.next().is_none());
        let mut name = t!(
            "printers.overview.numbered",
            locale = locale,
            label = t!(label, locale = locale),
            number = index + 1
        )
        .into_owned();
        if let Some(device) = linked {
            name = device_name(device, &detail.devices, locale);
        } else if detail.devices.iter().any(|device| device.kind == kind) {
            name = t!(
                "printers.overview.technical.primary_heater",
                locale = locale,
                heater = name
            )
            .into_owned();
        }
        for (key, value) in [
            ("printers.overview.temperature", current.filter(|_| !shown)),
            (
                "printers.overview.target_temperature",
                target.filter(|target| !shown || *target <= 0.),
            ),
        ] {
            if let Some(value) = value {
                if linked.is_some_and(|device| match key {
                    "printers.overview.temperature" => device.temperature_c.is_some(),
                    _ => device.target_temperature_c.is_some(),
                }) {
                    continue;
                }
                let mut field = Field::new(key, Value::Number(value, Unit::Celsius));
                field.resource = linked.map(|device| device.id);
                field.name = Some(name.clone());
                fields.push(field);
            }
        }
    }
    fields
}

pub(super) fn sections<'a>(readings: &'a Readings<'_>) -> Vec<Section<'a>> {
    let mut sections: Vec<Section<'_>> = Vec::new();
    for (index, field) in readings.fields.iter().enumerate() {
        let found = sections.iter().position(|section| {
            section.resource == field.resource
                && section.name == field.name.as_deref()
                && section.number == field.number
        });
        if let Some(found) = found {
            sections[found].indices.push(index);
        } else {
            sections.push(Section {
                resource: field.resource,
                name: field.name.as_deref(),
                number: field.number,
                indices: vec![index],
            });
        }
    }
    sections
}

pub(super) fn rows(field: &Field<'_>) -> usize {
    match field.value {
        Value::Xyz(_) | Value::Xy(_) => {
            2 + usize::from(field.status != crate::shared::workspace::ReadStatus::Ready)
        }
        Value::Text(text) if text.chars().count() > 32 => 3,
        Value::Grid(grid) if grid.iter().any(|row| !row.is_empty()) => 3,
        _ => 1,
    }
}

/// Presentation categories use normalized field meanings, never a resource's name or ID.
pub(super) fn motion_group(key: &str) -> Group {
    match key {
        "printers.overview.speed"
        | "printers.overview.feed_velocity"
        | "printers.overview.requested_speed"
        | "printers.overview.speed_override"
        | "printers.overview.flow_override" => Group::MotionSpeed,
        "printers.overview.travel_minimum"
        | "printers.overview.travel_maximum"
        | "printers.overview.velocity_limit"
        | "printers.overview.acceleration_limit"
        | "printers.overview.corner_velocity"
        | "printers.overview.minimum_cruise"
        | "printers.overview.motion_stalls" => Group::MotionLimits,
        _ => Group::Motion,
    }
}

pub(in crate::gpui_app::overview) fn shape(readings: &Readings<'_>) -> Shape {
    let blocks: Vec<_> = sections(readings)
        .iter()
        .map(|section| Block {
            hero: false,
            rows: section
                .indices
                .iter()
                .map(|&index| rows(&readings.fields[index]))
                .sum(),
        })
        .collect();
    Shape::Sections(
        blocks,
        readings.status != crate::shared::workspace::ReadStatus::Ready,
    )
}

pub(super) fn device_name(device: &Device, devices: &[Device], locale: &str) -> String {
    // A kind identifies a function. A name or ordinal never assigns one.
    let key_for = |device: &Device| match device.kind {
        DeviceKind::Nozzle => Some("printers.overview.nozzle"),
        DeviceKind::Bed => Some("printers.overview.bed"),
        DeviceKind::ChamberHeater => Some("printers.overview.chamber"),
        DeviceKind::MotionSettings => Some(match device.motion_source {
            Some(crate::shared::machine::MotionSource::CommandInterpreter) => {
                "printers.overview.technical.command_coordinates"
            }
            Some(crate::shared::machine::MotionSource::MotionPlanner) => {
                "printers.overview.technical.planner_coordinates"
            }
            None => "printers.overview.technical.motion_settings",
        }),
        DeviceKind::Motion => Some("printers.overview.motion"),
        _ => None,
    };
    let key = match key_for(device) {
        Some(key) => key,
        _ => return presentation::device_name(device, devices, locale),
    };
    let label = t!(key, locale = locale);
    let mut peers: Vec<_> = devices
        .iter()
        .filter(|other| key_for(other) == Some(key))
        .map(|other| other.id)
        .collect();
    if peers.len() < 2 {
        return label.into_owned();
    }
    peers.sort_unstable();
    let number = peers.iter().position(|&id| id == device.id).unwrap_or(0) + 1;
    t!(
        "printers.overview.numbered",
        locale = locale,
        label = label,
        number = number
    )
    .into_owned()
}

pub(super) fn caption(
    section: &Section<'_>,
    readings: &Readings<'_>,
    locale: &str,
) -> Option<String> {
    let first = &readings.fields[*section.indices.first()?];
    if section.resource.is_none() {
        let key = match first.key {
            "printers.overview.host_load"
            | "printers.overview.cpu_time"
            | "printers.overview.available_memory" => Some("printers.overview.technical.host"),
            "printers.overview.configuration_pending" => {
                Some("printers.overview.technical.configuration")
            }
            _ => None,
        };
        if let Some(key) = key {
            return Some(t!(key, locale = locale).into_owned());
        }
    }
    if let Some(name) = section.name {
        return Some(name.to_owned());
    }
    section.number.map(|number| {
        let key = match readings.title {
            "printers.overview.queue" => "printers.overview.queue_file",
            "printers.overview.objects" => "printers.overview.object_name",
            _ => first.key,
        };
        t!(
            "printers.overview.numbered",
            locale = locale,
            label = t!(key, locale = locale),
            number = number
        )
        .into_owned()
    })
}

pub(super) fn label(field: &Field<'_>, locale: &str) -> String {
    let key = match field.key {
        "printers.overview.temperature" => "printers.overview.technical.current_temperature",
        "printers.overview.target_temperature" => "printers.overview.technical.target_temperature",
        "printers.overview.execution_active" => "printers.overview.technical.execution_active",
        "printers.overview.execution_paused" => "printers.overview.technical.execution_paused",
        "printers.overview.execution_progress" => "printers.overview.technical.execution_progress",
        "printers.overview.execution_offset" => "printers.overview.technical.execution_offset",
        "printers.overview.execution_size" => "printers.overview.technical.execution_size",
        "printers.overview.controller_firmware" => "printers.overview.firmware",
        _ => field.key,
    };
    t!(key, locale = locale).into_owned()
}
