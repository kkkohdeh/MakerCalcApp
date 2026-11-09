//! Names, formatting and visibility of normalized summary readings.
use super::super::super::format::{
    format_bytes, format_duration, format_percent, format_speed, format_temperature,
};
use super::super::super::machine::Row;
use super::{Field, Readings, Unit, Value};
use crate::i18n::t;
use crate::shared::workspace::{QueueStatus, ReadStatus};

pub(in crate::gpui_app) fn value(value: Value<'_>, locale: &str) -> String {
    match value {
        Value::Number(value, unit) => match unit {
            Unit::Mm => format!("{value:.2} mm"),
            Unit::PreciseMm => format!("{value:.4} mm"),
            Unit::Speed => format_speed(Some(value)),
            Unit::Acceleration => format!("{value:.0} mm/s²"),
            Unit::Percent => format_percent(Some(value)),
            Unit::Seconds => format_duration(value),
            Unit::PreciseSeconds => format!("{value:.4} s"),
            Unit::Amps => format!("{value:.2} A"),
            Unit::Ohms => format!("{value:.0} Ω"),
            Unit::Pascals => format!("{value:.0} Pa"),
            Unit::Celsius => format_temperature(Some(value)),
            Unit::Number => format!("{value:.2}"),
        },
        Value::Count(value) => value.to_string(),
        Value::Bytes(value) if value >= 1 << 30 => {
            format!("{:.1} GB", value as f64 / (1u64 << 30) as f64)
        }
        Value::Bytes(value) => format_bytes(Some(value)),
        Value::Bool(value) => if value {
            t!("printers.overview.yes", locale = locale)
        } else {
            t!("printers.overview.no", locale = locale)
        }
        .into_owned(),
        Value::Text(value) => value.to_owned(),
        Value::Activity(activity) => activity_label(activity, locale),
        Value::Issue(issue) => issue_label(issue, locale),
        Value::Xyz([x, y, z]) => format!("X {x:.2} · Y {y:.2} · Z {z:.2} mm"),
        Value::Xy([x, y]) => format!("X {x:.2} · Y {y:.2} mm"),
        Value::Axes(axes) => {
            let homed = ["X", "Y", "Z"]
                .into_iter()
                .zip(axes)
                .filter_map(|(name, homed)| homed.then_some(name))
                .collect::<Vec<_>>();
            if homed.is_empty() {
                t!("printers.overview.no_axes_homed", locale = locale).into_owned()
            } else {
                homed.join(", ")
            }
        }
        Value::Grid(grid) => {
            if grid.is_empty() || grid.iter().all(Vec::is_empty) {
                return t!("printers.overview.empty_grid", locale = locale).into_owned();
            }
            let rows = grid
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|v| format!("{v:+.3}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect::<Vec<_>>();
            format!("{} mm", rows.join("; "))
        }
        Value::QueueStatus(status) => match status {
            QueueStatus::Unknown => t!("common.status.unknown", locale = locale),
            QueueStatus::Ready => t!("common.status.ready", locale = locale),
            QueueStatus::Running => t!("printers.overview.queue_running", locale = locale),
            QueueStatus::Paused => t!("common.status.paused", locale = locale),
        }
        .into_owned(),
        Value::Timestamp(seconds) => {
            chrono::DateTime::<chrono::Utc>::from_timestamp(seconds.floor() as i64, 0)
                .map(|time| time.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "—".to_owned())
        }
        Value::Status(read_status) => status(read_status, locale).unwrap_or_default(),
    }
}

fn activity_label(activity: crate::shared::machine::MachineActivity, locale: &str) -> String {
    use crate::shared::machine::MachineActivity as A;
    match activity {
        A::Heating => t!("printers.activity.heating", locale = locale),
        A::Pausing => t!("printers.activity.pausing", locale = locale),
        A::Stopping => t!("printers.activity.stopping", locale = locale),
        A::Calibrating => t!("printers.activity.calibrating", locale = locale),
        A::ReceivingFile => t!("printers.activity.receiving_file", locale = locale),
        A::Slicing => t!("printers.activity.slicing", locale = locale),
        A::TransferringFile => t!("printers.activity.transferring_file", locale = locale),
        A::ExtractingFile => t!("printers.activity.extracting_file", locale = locale),
        A::Busy => t!("printers.activity.busy", locale = locale),
    }
    .into_owned()
}

fn issue_label(issue: crate::shared::machine::PrinterIssueKind, locale: &str) -> String {
    use crate::shared::machine::PrinterIssueKind as I;
    match issue {
        I::FilamentFeedTimeout => t!("printers.issues.feed_timeout", locale = locale),
        I::FilamentRetractTimeout => t!("printers.issues.retract_timeout", locale = locale),
        I::FilamentFeedFailed => t!("printers.issues.feed_failed", locale = locale),
        I::FilamentJam => t!("printers.issues.filament_jam", locale = locale),
        I::MaterialStationHomingFailed => {
            t!("printers.issues.station_homing_failed", locale = locale)
        }
    }
    .into_owned()
}

pub(in crate::gpui_app) fn rows(fields: &[Field<'_>], locale: &str) -> Vec<Row> {
    fields
        .iter()
        .map(|field| {
            let label = t!(field.key, locale = locale).into_owned();
            let label = field.number.map_or(label.clone(), |number| {
                t!(
                    "printers.overview.numbered",
                    locale = locale,
                    label = &label,
                    number = number
                )
                .into_owned()
            });
            let label = field.name.as_deref().map_or(label.clone(), |name| {
                t!(
                    "printers.overview.sensor_reading",
                    locale = locale,
                    label = &label,
                    name = name
                )
                .into_owned()
            });
            let value = value(field.value, locale);
            let value = match field.status {
                ReadStatus::Error => t!(
                    "common.read_value.refresh_failed",
                    locale = locale,
                    value = value
                )
                .into_owned(),
                ReadStatus::Denied => t!(
                    "common.read_value.refresh_denied",
                    locale = locale,
                    value = value
                )
                .into_owned(),
                ReadStatus::NotLoaded
                | ReadStatus::Loading
                | ReadStatus::Limited
                | ReadStatus::Unsupported => {
                    t!("common.read_value.retained", locale = locale, value = value).into_owned()
                }
                _ => value,
            };
            Row {
                label,
                value,
                action: None,
            }
        })
        .collect()
}

/// A number distinguishes resources with the same label, never their function.
pub(in crate::gpui_app) fn device_name(
    device: &crate::shared::workspace::Device,
    devices: &[crate::shared::workspace::Device],
    locale: &str,
) -> String {
    use crate::shared::workspace::DeviceKind;
    let base = |device: &crate::shared::workspace::Device| match device.kind {
        DeviceKind::Fan => fan_name(device.fan_role, &device.display_name, locale),
        DeviceKind::Nozzle => t!("printers.overview.nozzle", locale = locale).into_owned(),
        DeviceKind::Bed => t!("printers.overview.bed", locale = locale).into_owned(),
        DeviceKind::ChamberHeater | DeviceKind::ChamberSensor => {
            t!("printers.overview.chamber", locale = locale).into_owned()
        }
        DeviceKind::FilamentSensor if device.display_name.is_empty() => {
            t!("printers.overview.filament_sensor", locale = locale).into_owned()
        }
        _ => device.display_name.clone(),
    };
    let label = base(device);
    let peers = devices
        .iter()
        .filter(|other| base(other) == label)
        .collect::<Vec<_>>();
    if peers.len() < 2 {
        return label;
    }
    let number = peers.iter().filter(|other| other.id <= device.id).count();
    t!(
        "printers.overview.numbered",
        locale = locale,
        label = label,
        number = number
    )
    .into_owned()
}

pub(in crate::gpui_app) fn fan_name(
    role: Option<crate::shared::machine::FanRole>,
    name: &str,
    locale: &str,
) -> String {
    use crate::shared::machine::FanRole;
    let key = match role {
        Some(FanRole::PartCooling) => "printers.overview.fan_part_cooling",
        Some(FanRole::Chamber) => "printers.overview.fan_chamber",
        Some(FanRole::Exhaust) => "printers.overview.fan_exhaust",
        Some(FanRole::Recirculation) => "printers.overview.fan_recirculation",
        Some(FanRole::Auxiliary) => "printers.overview.fan_auxiliary",
        _ if !name.is_empty() => return name.to_owned(),
        _ => "printers.overview.fan",
    };
    t!(key, locale = locale).into_owned()
}

/// Suppress only fields of the exact resource already represented in the live summary.
/// Telemetry IDs and device IDs are separate namespaces; equal values prove nothing.
pub(in crate::gpui_app::overview) fn omit_summary_fields(
    printer: &crate::gpui_app::PrinterEntry,
    summary: &[super::super::model::Card<'_>],
    technical: &mut Vec<super::super::model::Card<'_>>,
) {
    use super::super::model::{CardKind, Content};
    use crate::shared::workspace::{DeviceKind, ResourceId};
    if !printer.detail_state.live {
        return;
    }
    let devices = &printer.detail_state.devices;
    let telemetry = &printer.state.telemetry;
    let mut shown = std::collections::BTreeSet::<(ResourceId, &str)>::new();
    for card in summary {
        match &card.content {
            Content::Heater(heater) => {
                let (kind, index, current, target) = match card.kind {
                    CardKind::Nozzle(index) => (
                        DeviceKind::Nozzle,
                        index,
                        telemetry.nozzles.get(index).and_then(|n| n.nozzle_temp_c),
                        telemetry
                            .nozzles
                            .get(index)
                            .and_then(|n| n.nozzle_target_temp_c),
                    ),
                    CardKind::Bed(index) => (
                        DeviceKind::Bed,
                        index,
                        telemetry.beds.get(index).and_then(|b| b.bed_temp_c),
                        telemetry.beds.get(index).and_then(|b| b.bed_target_temp_c),
                    ),
                    _ => continue,
                };
                if let Some(device) = devices.iter().filter(|d| d.kind == kind).nth(index) {
                    for (key, displayed) in [
                        (
                            "temperature",
                            current.is_none() && heater.current_c.is_some(),
                        ),
                        (
                            "target_temperature",
                            target.is_none() && heater.target_c.is_some(),
                        ),
                        ("nozzle_diameter", heater.diameter_mm.is_some()),
                        ("output_power", heater.power_pct.is_some()),
                    ] {
                        if displayed {
                            shown.insert((device.id, key));
                        }
                    }
                }
            }
            Content::Machine { fans, .. } | Content::Chamber { fans, .. } => {
                for fan in fans {
                    if let Some(id) = fan.resource {
                        for (key, displayed) in [
                            ("output_power", fan.power_pct.is_some()),
                            (
                                "enabled_state",
                                fan.power_pct.is_none() && fan.enabled.is_some(),
                            ),
                            ("fan_rpm", fan.rpm.is_some()),
                        ] {
                            if displayed {
                                shown.insert((id, key));
                            }
                        }
                    }
                }
            }
            Content::Environment(_) => {
                for device in devices.iter().filter(|d| {
                    matches!(
                        d.kind,
                        DeviceKind::TemperatureSensor | DeviceKind::ChamberSensor
                    )
                }) {
                    for key in ["temperature", "humidity", "pressure"] {
                        shown.insert((device.id, key));
                    }
                }
            }
            Content::Motion(_) => {
                let motion = || {
                    devices.iter().filter(|d| {
                        matches!(d.kind, DeviceKind::Motion | DeviceKind::MotionSettings)
                    })
                };
                if let Some((_, id, key)) = crate::gpui_app::machine::motion_position(printer) {
                    shown.insert((id, key));
                }
                if let Some(device) = motion().find(|d| {
                    d.readings
                        .motion
                        .as_ref()
                        .is_some_and(|m| m.homed_axes.is_some())
                }) {
                    shown.insert((device.id, "homed_axes"));
                }
            }
            _ => {}
        }
    }
    technical.retain_mut(|card| {
        let Content::Readings(readings) = &mut card.content else {
            return true;
        };
        let was_empty = readings.fields.is_empty();
        readings.fields.retain(|field| {
            field.status != ReadStatus::Ready
                || !field
                    .resource
                    .zip(field.key.strip_prefix("printers.overview."))
                    .is_some_and(|(id, key)| shown.contains(&(id, key)))
        });
        was_empty || !readings.fields.is_empty()
    });
}

pub(in crate::gpui_app::overview) fn title(readings: &Readings<'_>, locale: &str) -> String {
    t!(readings.title, locale = locale).into_owned()
}

pub(super) fn status(status: ReadStatus, locale: &str) -> Option<String> {
    Some(
        match status {
            ReadStatus::Ready => return None,
            ReadStatus::NotLoaded => t!("printers.overview.retained", locale = locale),
            ReadStatus::Loading => t!("common.status.loading", locale = locale),
            ReadStatus::Unsupported => t!("common.read_value.unsupported", locale = locale),
            ReadStatus::Denied => t!("common.read_value.denied", locale = locale),
            ReadStatus::Limited => t!("common.read_value.limited", locale = locale),
            ReadStatus::Error => t!("common.read_value.error", locale = locale),
        }
        .into_owned(),
    )
}

pub(in crate::gpui_app::overview) fn reading_rows(
    readings: &Readings<'_>,
    locale: &str,
) -> Vec<Row> {
    let mut rows = rows(&readings.fields, locale);
    if let Some(value) = status(readings.status, locale) {
        rows.insert(
            0,
            Row {
                label: t!("printers.overview.read_status", locale = locale).into_owned(),
                value,
                action: None,
            },
        );
    }
    rows
}
