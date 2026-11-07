//! One technical card per device group, with normalized readings.
//! No formatting, I/O or controls.
use super::super::PrinterEntry;
use super::model::{Card, CardKind, Content};
use crate::shared::machine::*;
use crate::shared::workspace::{
    DetailRequest, DetailState, Device, DeviceKind, QueueStatus, ReadStatus,
};

mod equipment;
pub(in crate::gpui_app) mod presentation;
pub(in crate::gpui_app::overview) mod sections;
pub(in crate::gpui_app::overview) mod view;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::gpui_app) enum Group {
    Heaters,
    Sensors,
    Fans,
    Motion,
    MotionSpeed,
    MotionLimits,
    Extrusion,
    Leveling,
    Probe,
    Motor,
    Peripherals,
    Execution,
    Queue,
    Objects,
    Controller,
}

impl Group {
    /// Fixed order inside the technical details; declaration order follows it.
    pub(super) fn weight(self) -> u8 {
        match self {
            Self::Heaters => 39,
            Self::Sensors => 38,
            Self::Fans => 37,
            Self::Motion | Self::MotionSpeed | Self::MotionLimits => 36,
            Self::Extrusion => 35,
            Self::Leveling => 34,
            Self::Probe => 33,
            Self::Motor => 32,
            Self::Peripherals => 31,
            Self::Execution => 30,
            Self::Queue => 29,
            Self::Objects => 28,
            Self::Controller => 27,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Heaters => "printers.overview.heaters",
            Self::Sensors => "printers.overview.sensors",
            Self::Fans => "printers.overview.fans",
            Self::Motion => "printers.overview.technical.positions_modes",
            Self::MotionSpeed => "printers.overview.technical.speeds_factors",
            Self::MotionLimits => "printers.overview.technical.motion_limits",
            Self::Extrusion => "printers.overview.extrusion",
            Self::Leveling => "printers.overview.leveling",
            Self::Probe => "printers.overview.probe",
            Self::Motor => "printers.overview.motor",
            Self::Peripherals => "printers.overview.peripherals",
            Self::Execution => "printers.overview.execution",
            Self::Queue => "printers.overview.queue",
            Self::Objects => "printers.overview.objects",
            Self::Controller => "printers.overview.controller",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::gpui_app) enum Unit {
    Mm,
    PreciseMm,
    Speed,
    Acceleration,
    Percent,
    Seconds,
    PreciseSeconds,
    Amps,
    Ohms,
    Pascals,
    Celsius,
    Number,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::gpui_app) enum Value<'a> {
    Number(f64, Unit),
    Count(u64),
    Bytes(u64),
    Bool(bool),
    Text(&'a str),
    Xyz([f64; 3]),
    Xy([f64; 2]),
    Axes([bool; 3]),
    Grid(&'a [Vec<f64>]),
    QueueStatus(QueueStatus),
    Timestamp(f64),
    Status(ReadStatus),
    Activity(MachineActivity),
    Issue(PrinterIssueKind),
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::gpui_app) struct Field<'a> {
    pub(super) resource: Option<crate::shared::workspace::ResourceId>,
    pub(super) key: &'static str,
    pub(super) value: Value<'a>,
    pub(super) name: Option<String>,
    pub(super) number: Option<usize>,
    pub(super) status: ReadStatus,
}

impl<'a> Field<'a> {
    pub(super) fn new(key: &'static str, value: Value<'a>) -> Self {
        Self {
            resource: None,
            key,
            value,
            name: None,
            number: None,
            status: ReadStatus::Ready,
        }
    }
}

pub(in crate::gpui_app) struct Readings<'a> {
    pub(super) title: &'static str,
    pub(super) fields: Vec<Field<'a>>,
    pub(super) status: ReadStatus,
}

fn push<'a>(rows: &mut Vec<Field<'a>>, key: &'static str, value: Option<Value<'a>>) {
    if let Some(value) = value {
        rows.push(Field::new(key, value));
    }
}

/// Every normalized field of one device, in a fixed order.
fn device_fields(device: &Device) -> Vec<Field<'_>> {
    let mut rows = Vec::new();
    push(
        &mut rows,
        "printers.overview.temperature",
        device
            .temperature_c
            .map(|value| Value::Number(value, Unit::Celsius)),
    );
    push(
        &mut rows,
        "printers.overview.target_temperature",
        device
            .target_temperature_c
            .map(|value| Value::Number(value, Unit::Celsius)),
    );
    push(
        &mut rows,
        "printers.overview.output_power",
        device
            .output_pct
            .map(|value| Value::Number(value, Unit::Percent)),
    );
    push(
        &mut rows,
        "printers.overview.fan_rpm",
        device.rpm.map(|value| Value::Number(value, Unit::Number)),
    );
    push(
        &mut rows,
        "printers.overview.enabled_state",
        device.enabled.map(Value::Bool),
    );
    push(
        &mut rows,
        "printers.overview.filament_present_state",
        device.filament_present.map(Value::Bool),
    );
    push(
        &mut rows,
        "printers.overview.can_extrude",
        device.can_extrude.map(Value::Bool),
    );
    push(
        &mut rows,
        "printers.overview.position",
        device.position_mm.map(Value::Xyz),
    );
    push(
        &mut rows,
        "printers.overview.speed",
        device
            .velocity_mm_s
            .map(|value| Value::Number(value, Unit::Speed)),
    );
    push(
        &mut rows,
        "printers.overview.nozzle_diameter",
        device
            .nozzle_diameter_mm
            .map(|value| Value::Number(value, Unit::Mm)),
    );
    rows.extend(reading_fields(&device.readings));
    for field in &mut rows {
        field.resource = Some(device.id);
    }
    rows
}

fn reading_fields(readings: &MachineReadings) -> Vec<Field<'_>> {
    let mut rows = Vec::new();
    let MachineReadings {
        environment,
        extrusion,
        motion,
        retraction,
        leveling,
        probe,
        motor,
        file_execution,
        thermal,
        filament,
        controller,
    } = readings;
    if let Some(EnvironmentalReadings {
        relative_humidity_pct,
        pressure_pa,
        gas_resistance_ohm,
    }) = environment
    {
        push(
            &mut rows,
            "printers.overview.humidity",
            (*relative_humidity_pct).map(|value| Value::Number(value, Unit::Percent)),
        );
        push(
            &mut rows,
            "printers.overview.pressure",
            (*pressure_pa).map(|value| Value::Number(value, Unit::Pascals)),
        );
        push(
            &mut rows,
            "printers.overview.gas_resistance",
            (*gas_resistance_ohm).map(|value| Value::Number(value, Unit::Ohms)),
        );
    }
    if let Some(Extrusion {
        pressure_advance_s,
        pressure_smoothing_s,
        active,
        feed_velocity_mm_s,
    }) = extrusion
    {
        push(
            &mut rows,
            "printers.overview.pressure_advance",
            (*pressure_advance_s).map(|value| Value::Number(value, Unit::PreciseSeconds)),
        );
        push(
            &mut rows,
            "printers.overview.pressure_smoothing",
            (*pressure_smoothing_s).map(|value| Value::Number(value, Unit::PreciseSeconds)),
        );
        push(
            &mut rows,
            "printers.overview.extrusion_active",
            (*active).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.feed_velocity",
            (*feed_velocity_mm_s).map(|value| Value::Number(value, Unit::Speed)),
        );
    }
    if let Some(MotionSettings {
        commanded_position_mm,
        work_position_mm,
        origin_offset_mm,
        travel_min_mm,
        travel_max_mm,
        requested_velocity_mm_s,
        velocity_limit_mm_s,
        acceleration_limit_mm_s2,
        corner_velocity_mm_s,
        minimum_cruise_pct,
        speed_override_pct,
        flow_override_pct,
        homed_axes,
        absolute_positioning,
        absolute_extrusion,
        stalls,
    }) = motion
    {
        push(
            &mut rows,
            "printers.overview.commanded_position",
            (*commanded_position_mm).map(Value::Xyz),
        );
        push(
            &mut rows,
            "printers.overview.work_position",
            (*work_position_mm).map(Value::Xyz),
        );
        push(
            &mut rows,
            "printers.overview.origin_offset",
            (*origin_offset_mm).map(Value::Xyz),
        );
        push(
            &mut rows,
            "printers.overview.travel_minimum",
            (*travel_min_mm).map(Value::Xyz),
        );
        push(
            &mut rows,
            "printers.overview.travel_maximum",
            (*travel_max_mm).map(Value::Xyz),
        );
        push(
            &mut rows,
            "printers.overview.requested_speed",
            (*requested_velocity_mm_s).map(|value| Value::Number(value, Unit::Speed)),
        );
        push(
            &mut rows,
            "printers.overview.velocity_limit",
            (*velocity_limit_mm_s).map(|value| Value::Number(value, Unit::Speed)),
        );
        push(
            &mut rows,
            "printers.overview.acceleration_limit",
            (*acceleration_limit_mm_s2).map(|value| Value::Number(value, Unit::Acceleration)),
        );
        push(
            &mut rows,
            "printers.overview.corner_velocity",
            (*corner_velocity_mm_s).map(|value| Value::Number(value, Unit::Speed)),
        );
        push(
            &mut rows,
            "printers.overview.minimum_cruise",
            (*minimum_cruise_pct).map(|value| Value::Number(value, Unit::Percent)),
        );
        push(
            &mut rows,
            "printers.overview.speed_override",
            (*speed_override_pct).map(|value| Value::Number(value, Unit::Percent)),
        );
        push(
            &mut rows,
            "printers.overview.flow_override",
            (*flow_override_pct).map(|value| Value::Number(value, Unit::Percent)),
        );
        push(
            &mut rows,
            "printers.overview.homed_axes",
            (*homed_axes).map(Value::Axes),
        );
        push(
            &mut rows,
            "printers.overview.absolute_positioning",
            (*absolute_positioning).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.absolute_extrusion",
            (*absolute_extrusion).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.motion_stalls",
            (*stalls).map(Value::Count),
        );
    }
    if let Some(Retraction {
        length_mm,
        speed_mm_s,
        extra_recovery_mm,
        recovery_speed_mm_s,
    }) = retraction
    {
        push(
            &mut rows,
            "printers.overview.retraction_length",
            (*length_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
        push(
            &mut rows,
            "printers.overview.retraction_speed",
            (*speed_mm_s).map(|value| Value::Number(value, Unit::Speed)),
        );
        push(
            &mut rows,
            "printers.overview.extra_recovery",
            (*extra_recovery_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
        push(
            &mut rows,
            "printers.overview.recovery_speed",
            (*recovery_speed_mm_s).map(|value| Value::Number(value, Unit::Speed)),
        );
    }
    if let Some(Leveling {
        profile_name,
        area_min_mm,
        area_max_mm,
        measured_grid_mm,
        compensation_grid_mm,
        applied,
    }) = leveling
    {
        push(
            &mut rows,
            "printers.overview.leveling_profile",
            profile_name.as_deref().map(Value::Text),
        );
        push(
            &mut rows,
            "printers.overview.leveling_minimum",
            (*area_min_mm).map(Value::Xy),
        );
        push(
            &mut rows,
            "printers.overview.leveling_maximum",
            (*area_max_mm).map(Value::Xy),
        );
        push(
            &mut rows,
            "printers.overview.measured_grid",
            measured_grid_mm.as_deref().map(Value::Grid),
        );
        push(
            &mut rows,
            "printers.overview.compensation_grid",
            compensation_grid_mm.as_deref().map(Value::Grid),
        );
        push(
            &mut rows,
            "printers.overview.leveling_applied",
            (*applied).map(Value::Bool),
        );
    }
    if let Some(ProbeReadings {
        last_triggered,
        last_position_mm,
        last_height_mm,
        manual_active,
        manual_height_mm,
        lower_height_mm,
        upper_height_mm,
    }) = probe
    {
        push(
            &mut rows,
            "printers.overview.probe_triggered",
            (*last_triggered).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.probe_position",
            (*last_position_mm).map(Value::Xyz),
        );
        push(
            &mut rows,
            "printers.overview.probe_height",
            (*last_height_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
        push(
            &mut rows,
            "printers.overview.manual_probe_active",
            (*manual_active).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.manual_probe_height",
            (*manual_height_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
        push(
            &mut rows,
            "printers.overview.probe_lower_height",
            (*lower_height_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
        push(
            &mut rows,
            "printers.overview.probe_upper_height",
            (*upper_height_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
    }
    if let Some(MotorReadings {
        run_current_a,
        hold_current_a,
    }) = motor
    {
        push(
            &mut rows,
            "printers.overview.motor_run_current",
            (*run_current_a).map(|value| Value::Number(value, Unit::Amps)),
        );
        push(
            &mut rows,
            "printers.overview.motor_hold_current",
            (*hold_current_a).map(|value| Value::Number(value, Unit::Amps)),
        );
    }
    if let Some(FileExecution {
        active,
        paused,
        read_offset_bytes,
        file_size_bytes,
        progress_pct,
        idle_timeout_s,
    }) = file_execution
    {
        push(
            &mut rows,
            "printers.overview.execution_active",
            (*active).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.execution_paused",
            (*paused).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.execution_offset",
            (*read_offset_bytes).map(Value::Bytes),
        );
        push(
            &mut rows,
            "printers.overview.execution_size",
            (*file_size_bytes).map(Value::Bytes),
        );
        push(
            &mut rows,
            "printers.overview.execution_progress",
            (*progress_pct).map(|value| Value::Number(value, Unit::Percent)),
        );
        push(
            &mut rows,
            "printers.overview.idle_timeout",
            (*idle_timeout_s).map(|value| Value::Number(value, Unit::Seconds)),
        );
    }
    if let Some(ThermalReadings {
        minimum_c,
        maximum_c,
        reference_c,
        z_adjustment_mm,
        z_adjustment_enabled,
    }) = thermal
    {
        push(
            &mut rows,
            "printers.overview.minimum_temperature",
            (*minimum_c).map(|value| Value::Number(value, Unit::Celsius)),
        );
        push(
            &mut rows,
            "printers.overview.maximum_temperature",
            (*maximum_c).map(|value| Value::Number(value, Unit::Celsius)),
        );
        push(
            &mut rows,
            "printers.overview.reference_temperature",
            (*reference_c).map(|value| Value::Number(value, Unit::Celsius)),
        );
        push(
            &mut rows,
            "printers.overview.thermal_z_adjustment",
            (*z_adjustment_mm).map(|value| Value::Number(value, Unit::PreciseMm)),
        );
        push(
            &mut rows,
            "printers.overview.thermal_z_enabled",
            (*z_adjustment_enabled).map(Value::Bool),
        );
    }
    if let Some(FilamentReadings {
        diameter_mm,
        width_sensor_active,
        flow_compensation_enabled,
    }) = filament
    {
        push(
            &mut rows,
            "printers.overview.filament_diameter",
            (*diameter_mm).map(|value| Value::Number(value, Unit::Mm)),
        );
        push(
            &mut rows,
            "printers.overview.width_sensor_active",
            (*width_sensor_active).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.flow_compensation_enabled",
            (*flow_compensation_enabled).map(Value::Bool),
        );
    }
    if let Some(ControllerReadings {
        firmware_version,
        configuration_pending,
        receive_errors,
        transmit_errors,
        transmit_retries,
        host_load_1m,
        process_cpu_time_s,
        available_memory_bytes,
    }) = controller
    {
        push(
            &mut rows,
            "printers.overview.controller_firmware",
            firmware_version.as_deref().map(Value::Text),
        );
        push(
            &mut rows,
            "printers.overview.configuration_pending",
            (*configuration_pending).map(Value::Bool),
        );
        push(
            &mut rows,
            "printers.overview.receive_errors",
            (*receive_errors).map(Value::Count),
        );
        push(
            &mut rows,
            "printers.overview.transmit_errors",
            (*transmit_errors).map(Value::Count),
        );
        push(
            &mut rows,
            "printers.overview.transmit_retries",
            (*transmit_retries).map(Value::Count),
        );
        push(
            &mut rows,
            "printers.overview.host_load",
            (*host_load_1m).map(|value| Value::Number(value, Unit::Number)),
        );
        push(
            &mut rows,
            "printers.overview.cpu_time",
            (*process_cpu_time_s).map(|value| Value::Number(value, Unit::Seconds)),
        );
        push(
            &mut rows,
            "printers.overview.available_memory",
            (*available_memory_bytes).map(Value::Bytes),
        );
    }
    rows
}

/// One technical card per device group, with every reported field.
/// Name rows when a group contains multiple devices.
pub(super) fn cards(printer: &PrinterEntry) -> Vec<Card<'_>> {
    let detail = &printer.detail_state;
    let status = if detail.live {
        ReadStatus::Ready
    } else {
        ReadStatus::NotLoaded
    };
    let mut groups = std::collections::BTreeMap::<Group, Vec<Field<'_>>>::new();
    let remaining = sections::remaining_heaters(
        &printer.state.telemetry,
        detail,
        rust_i18n::locale().as_ref(),
    );
    if !remaining.is_empty() {
        groups.entry(Group::Heaters).or_default().extend(remaining);
    }
    for device in &detail.devices {
        let group = match device.kind {
            DeviceKind::Nozzle | DeviceKind::Bed | DeviceKind::ChamberHeater => Group::Heaters,
            DeviceKind::TemperatureSensor => Group::Sensors,
            DeviceKind::Fan => Group::Fans,
            DeviceKind::Motion | DeviceKind::MotionSettings => Group::Motion,
            DeviceKind::Extrusion => Group::Extrusion,
            DeviceKind::Leveling => Group::Leveling,
            DeviceKind::Probe => Group::Probe,
            DeviceKind::Motor => Group::Motor,
            DeviceKind::FileExecution => Group::Execution,
            DeviceKind::Controller => Group::Controller,
            // Peripherals carry their own read status; `peripherals` gathers them.
            DeviceKind::FilamentSensor | DeviceKind::Power | DeviceKind::Light => continue,
        };
        for field in device_fields(device) {
            let group = if group == Group::Motion {
                sections::motion_group(field.key)
            } else {
                group
            };
            groups.entry(group).or_default().push(Field {
                name: Some(sections::device_name(
                    device,
                    &detail.devices,
                    rust_i18n::locale().as_ref(),
                )),
                ..field
            });
        }
    }
    let mut cards = Vec::new();
    for (group, fields) in groups {
        if !fields.is_empty() {
            cards.push(Card {
                kind: CardKind::Readings(group),
                content: Content::Readings(Readings {
                    title: group.title(),
                    fields,
                    status,
                }),
            });
        }
    }
    let peripherals = peripherals(detail);
    if !peripherals.is_empty() {
        cards.push(Card {
            kind: CardKind::Readings(Group::Peripherals),
            content: Content::Readings(Readings {
                title: Group::Peripherals.title(),
                fields: peripherals,
                // Each row carries the status of the read it comes from.
                status: ReadStatus::Ready,
            }),
        });
    }
    let mut queue = Vec::new();
    if let Some(value) = &detail.queue.value {
        queue.push(Field::new(
            "printers.overview.queue_status",
            Value::QueueStatus(value.status),
        ));
        queue.push(Field::new(
            "printers.overview.queued",
            Value::Count(if value.entries.is_empty() {
                value.file_names.len()
            } else {
                value.entries.len()
            } as u64),
        ));
        for (index, entry) in value.entries.iter().enumerate() {
            let start = queue.len();
            queue.push(Field::new(
                "printers.overview.queue_file",
                Value::Text(&entry.display_name),
            ));
            push(
                &mut queue,
                "printers.overview.queue_added",
                entry.added_at_unix_s.map(Value::Timestamp),
            );
            push(
                &mut queue,
                "printers.overview.queue_waiting",
                entry.waiting_s.map(|n| Value::Number(n, Unit::Seconds)),
            );
            for field in &mut queue[start..] {
                field.number = Some(index + 1);
            }
        }
        if value.entries.is_empty() {
            queue.extend(
                value
                    .file_names
                    .iter()
                    .map(|name| Field::new("printers.overview.queue_file", Value::Text(name))),
            );
        }
    }
    add_read(&mut cards, Group::Queue, queue, detail.queue.status);
    let mut objects = Vec::new();
    if let Some(value) = &detail.print_objects.value {
        objects.push(Field::new(
            "printers.overview.objects",
            Value::Count(value.len() as u64),
        ));
        for (index, object) in value.iter().enumerate() {
            let start = objects.len();
            objects.push(Field::new(
                "printers.overview.object_name",
                Value::Text(&object.display_name),
            ));
            push(
                &mut objects,
                "printers.overview.object_current",
                object.current.map(Value::Bool),
            );
            push(
                &mut objects,
                "printers.overview.object_excluded",
                object.excluded.map(Value::Bool),
            );
            for field in &mut objects[start..] {
                field.number = Some(index + 1);
            }
        }
    }
    add_read(
        &mut cards,
        Group::Objects,
        objects,
        detail.print_objects.status,
    );
    cards
}

fn add_read<'a>(
    cards: &mut Vec<Card<'a>>,
    group: Group,
    fields: Vec<Field<'a>>,
    status: ReadStatus,
) {
    if fields.is_empty()
        && matches!(
            status,
            ReadStatus::NotLoaded | ReadStatus::Ready | ReadStatus::Unsupported
        )
    {
        return;
    }
    cards.push(Card {
        kind: CardKind::Readings(group),
        content: Content::Readings(Readings {
            title: group.title(),
            fields,
            status,
        }),
    });
}

fn peripherals(detail: &DetailState) -> Vec<Field<'_>> {
    let mut rows = Vec::new();
    for device in detail.devices.iter().filter(|device| {
        matches!(
            device.kind,
            DeviceKind::FilamentSensor | DeviceKind::Power | DeviceKind::Light
        )
    }) {
        rows.extend(device_fields(device).into_iter().map(|field| Field {
            name: Some(device.display_name.clone()),
            ..field
        }));
    }
    for (result, key) in [
        (&detail.power_devices, "printers.overview.power_devices"),
        (&detail.lights, "printers.overview.lights"),
    ] {
        for device in result.value.iter().flatten() {
            rows.extend(device_fields(device).into_iter().map(|field| Field {
                name: Some(device.display_name.clone()),
                status: result.status,
                ..field
            }));
        }
        read_status(&mut rows, key, result.status);
    }
    mark_retained(&mut rows, detail.live);
    rows
}

pub(super) fn equipment(detail: &DetailState) -> Vec<Field<'_>> {
    equipment::fields(detail)
}

fn mark_retained(rows: &mut [Field<'_>], live: bool) {
    if !live {
        for field in rows {
            if field.status == ReadStatus::Ready && !matches!(field.value, Value::Status(_)) {
                field.status = ReadStatus::NotLoaded;
            }
        }
    }
}

fn read_status(rows: &mut Vec<Field<'_>>, key: &'static str, status: ReadStatus) {
    if matches!(
        status,
        ReadStatus::Loading | ReadStatus::Denied | ReadStatus::Limited | ReadStatus::Error
    ) {
        rows.push(Field::new(key, Value::Status(status)));
    }
}

/// One optional read after the preceding area completes, including watch coalescing.
/// A steady Ready/Error never retries and a new generation never reuses old resources.
pub(in crate::gpui_app) fn next_read(
    previous: &DetailState,
    next: &DetailState,
) -> Option<DetailRequest> {
    if !next.live {
        return None;
    }
    let completed = |old: ReadStatus, new: ReadStatus| {
        !matches!(new, ReadStatus::NotLoaded | ReadStatus::Loading)
            && (previous.generation != next.generation
                || !previous.live
                || matches!(old, ReadStatus::NotLoaded | ReadStatus::Loading))
    };
    if completed(previous.system_read.status, next.system_read.status)
        && next.storage.status == ReadStatus::NotLoaded
    {
        return Some(DetailRequest::Peripherals);
    }
    if completed(previous.storage.status, next.storage.status)
        && next.queue.status == ReadStatus::NotLoaded
    {
        return Some(DetailRequest::Queue);
    }
    if completed(previous.queue.status, next.queue.status)
        && next.cameras.status == ReadStatus::NotLoaded
    {
        return Some(DetailRequest::Cameras);
    }
    None
}
