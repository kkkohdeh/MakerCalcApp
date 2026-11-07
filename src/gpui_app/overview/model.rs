//! Summary maps normalized state into cards without layout or styling.
//! Non-job cards require data; the job card always exists.
//! Fixed weight sets relative order; content shape sets size.
//! Summary shows key readings; technical details group remaining readings by device.

use crate::shared::machine::{FanRole, SoftwareRole, SupplyKind};
use crate::shared::telemetry::{FilamentSupplyTelemetry, SupplyState};
use crate::shared::workspace::{DeviceKind, HistoryTotals, ResourceId, Support};

use super::super::PrinterEntry;
use super::layout::{Block, Shape};
use super::readings::{self, Field, Group, Readings};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::gpui_app) enum CardKind {
    Job,
    Nozzle(usize),
    Bed(usize),
    Materials,
    TemperatureHistory,
    Camera,
    Chamber,
    Environment,
    Motion,
    Machine,
    Readings(Group),
    Lifetime,
    Device,
}

impl CardKind {
    /// Fixed importance from 0 to 100: heavier cards come first on every printer.
    pub(in crate::gpui_app) fn weight(self) -> u8 {
        match self {
            Self::Job => 100,
            Self::Nozzle(_) => 85,
            Self::Bed(_) => 84,
            Self::TemperatureHistory => 83,
            Self::Camera => 80,
            Self::Materials => 84,
            Self::Chamber => 55,
            Self::Environment => 50,
            Self::Motion => 45,
            Self::Machine => 40,
            Self::Readings(group) => group.weight(),
            Self::Lifetime => 25,
            Self::Device => 15,
        }
    }

    /// Cards listed in the technical details, below the summary.
    pub(in crate::gpui_app) fn technical(self) -> bool {
        matches!(self, Self::Readings(_) | Self::Device)
    }

    /// Pair related light cards when both exist to avoid empty surfaces.
    pub(in crate::gpui_app) fn pairs_with(self, next: Self) -> bool {
        matches!((self, next), (Self::Chamber, Self::Environment))
    }
}

pub(in crate::gpui_app) struct Supply<'a> {
    pub(in crate::gpui_app) reading: &'a FilamentSupplyTelemetry,
    pub(in crate::gpui_app) external: bool,
    pub(in crate::gpui_app) resource: Option<&'a crate::shared::machine::SupplyResource>,
}

/// A supply is shown only when it reports something; an empty placeholder takes no space.
fn reports_something(supply: &FilamentSupplyTelemetry) -> bool {
    supply
        .material
        .as_deref()
        .is_some_and(|material| material.chars().any(char::is_alphanumeric))
        || supply.color.is_some()
        || supply.filament_present.is_some()
        || supply.is_active.is_some()
        || supply.state != SupplyState::Unknown
}

pub(in crate::gpui_app) fn supplies(printer: &PrinterEntry) -> Vec<Supply<'_>> {
    let station = printer
        .state
        .telemetry
        .filament_supplies
        .iter()
        .flatten()
        .map(|reading| Supply {
            reading,
            external: false,
            resource: printer.detail_state.machine.supplies.iter().find(|supply| {
                supply.kind == SupplyKind::StationSlot && supply.reading.id == reading.id
            }),
        });
    // The external spool exists only in the machine contract; station slots stay live.
    let external = printer
        .detail_state
        .machine
        .supplies
        .iter()
        .filter(|supply| supply.kind == SupplyKind::ExternalSpool)
        .map(|supply| Supply {
            reading: &supply.reading,
            external: true,
            resource: Some(supply),
        });
    station
        .chain(external)
        .filter(|supply| reports_something(supply.reading))
        .collect()
}

/// The first reported camera that is not switched off.
pub(in crate::gpui_app) fn reported_camera(printer: &PrinterEntry) -> Option<ResourceId> {
    printer
        .detail_state
        .cameras
        .value
        .iter()
        .flatten()
        .find(|camera| camera.enabled != Some(false))
        .map(|camera| camera.id)
}

pub(in crate::gpui_app) fn shows_camera(printer: &PrinterEntry) -> bool {
    printer.detail_state.capabilities.cameras == Support::Available
        || printer.camera_state.frame.is_some()
}

/// Retry only a reported camera: an unknown capability alone is not a device fact.
pub(in crate::gpui_app) fn camera_retry(printer: &PrinterEntry) -> bool {
    !shows_camera(printer)
        && printer.detail_state.capabilities.cameras == Support::Unknown
        && reported_camera(printer).is_some()
}

/// A fan reading; enabled is not 100%, so power and on/off stay separate.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::gpui_app) struct FanFact {
    pub(in crate::gpui_app) resource: Option<ResourceId>,
    pub(in crate::gpui_app) role: Option<FanRole>,
    /// Presentation label; the normalized name and role remain unchanged.
    pub(in crate::gpui_app) name: String,
    pub(in crate::gpui_app) power_pct: Option<f64>,
    pub(in crate::gpui_app) enabled: Option<bool>,
    pub(in crate::gpui_app) rpm: Option<f64>,
}

/// Use device fans, or telemetry part fans when none are available.
pub(in crate::gpui_app) fn fan_facts(printer: &PrinterEntry) -> Vec<FanFact> {
    let devices = printer
        .detail_state
        .devices
        .iter()
        .filter(|device| device.kind == DeviceKind::Fan)
        .map(|device| FanFact {
            resource: Some(device.id),
            role: device.fan_role,
            name: readings::presentation::device_name(
                device,
                &printer.detail_state.devices,
                rust_i18n::locale().as_ref(),
            ),
            power_pct: device.output_pct,
            enabled: device.enabled,
            rpm: device.rpm,
        })
        .collect::<Vec<_>>();
    let fans = if devices.is_empty() {
        printer
            .state
            .telemetry
            .part_cooling_fans
            .iter()
            .map(|fan| FanFact {
                resource: None,
                role: Some(FanRole::PartCooling),
                name: readings::presentation::fan_name(
                    Some(FanRole::PartCooling),
                    "",
                    rust_i18n::locale().as_ref(),
                ),
                power_pct: fan.power_pct,
                enabled: None,
                rpm: fan.rpm,
            })
            .collect()
    } else {
        devices
    };
    fans.into_iter()
        .filter(|fan| fan.power_pct.is_some() || fan.enabled.is_some() || fan.rpm.is_some())
        .collect()
}

pub(in crate::gpui_app) struct HeaterFacts {
    pub(in crate::gpui_app) current_c: Option<f64>,
    /// A known zero target means off, not an unknown target.
    pub(in crate::gpui_app) target_c: Option<f64>,
    pub(in crate::gpui_app) active: Option<bool>,
    pub(in crate::gpui_app) diameter_mm: Option<f64>,
    /// Heater output, from the device inventory.
    pub(in crate::gpui_app) power_pct: Option<f64>,
}

pub(in crate::gpui_app) struct EnvironmentFact {
    pub(in crate::gpui_app) name: String,
    pub(in crate::gpui_app) temperature_c: Option<f64>,
    pub(in crate::gpui_app) relative_humidity_pct: Option<f64>,
    pub(in crate::gpui_app) pressure_pa: Option<f64>,
}

/// Toolhead position and homed axes.
pub(in crate::gpui_app) struct MotionFacts {
    pub(in crate::gpui_app) position_mm: Option<[f64; 3]>,
    pub(in crate::gpui_app) homed_axes: Option<[bool; 3]>,
}

pub(in crate::gpui_app) struct DeviceFacts<'a> {
    pub(in crate::gpui_app) manufacturer: Option<&'a str>,
    pub(in crate::gpui_app) model: Option<&'a str>,
    pub(in crate::gpui_app) firmware: Option<&'a str>,
    pub(in crate::gpui_app) server: Option<&'a str>,
    pub(in crate::gpui_app) firmware_ready: Option<bool>,
    pub(in crate::gpui_app) nozzle_count: Option<u8>,
    pub(in crate::gpui_app) build_volume_mm: Option<[f64; 3]>,
    pub(in crate::gpui_app) location: Option<&'a str>,
    pub(in crate::gpui_app) serial_suffix: Option<&'a str>,
    pub(in crate::gpui_app) camera_retry: bool,
    pub(in crate::gpui_app) extra: Vec<Field<'a>>,
}

pub(in crate::gpui_app) enum Content<'a> {
    Job,
    Heater(HeaterFacts),
    Camera(Option<ResourceId>),
    Materials(Vec<Supply<'a>>),
    TemperatureHistory,
    Chamber {
        temperature_c: Option<f64>,
        fans: Vec<FanFact>,
    },
    Environment(Vec<EnvironmentFact>),
    Motion(MotionFacts),
    Machine {
        z_offset_mm: Option<f64>,
        auto_shutdown: Option<bool>,
        /// Whether each filament sensor detects filament.
        filament_sensors: Vec<bool>,
        fans: Vec<FanFact>,
    },
    Readings(Readings<'a>),
    Lifetime(&'a HistoryTotals),
    Device(DeviceFacts<'a>),
}

impl Content<'_> {
    /// Report whether a card has a main value and count its label/value rows.
    /// The count must match rendered rows so the layout fits.
    fn shape(&self) -> (bool, usize) {
        let some = |present: &[bool]| present.iter().filter(|present| **present).count();
        match self {
            Self::Job | Self::Camera(_) | Self::TemperatureHistory => (true, 0),
            // The target sits beside the temperature; the rest are rows.
            Self::Heater(heater) => (
                heater.current_c.is_some() || heater.target_c.is_some(),
                some(&[heater.diameter_mm.is_some(), heater.power_pct.is_some()]),
            ),
            Self::Materials(supplies) => (false, supplies.len()),
            Self::Chamber {
                temperature_c,
                fans,
            } => (temperature_c.is_some(), fans.len()),
            Self::Environment(sensors) => (
                false,
                sensors
                    .iter()
                    .map(|sensor| {
                        some(&[
                            sensor.temperature_c.is_some(),
                            sensor.relative_humidity_pct.is_some(),
                            sensor.pressure_pa.is_some(),
                        ])
                    })
                    .sum(),
            ),
            // One row per axis, and the homed axes.
            Self::Motion(motion) => (
                false,
                3 * usize::from(motion.position_mm.is_some())
                    + usize::from(motion.homed_axes.is_some()),
            ),
            Self::Machine {
                z_offset_mm,
                auto_shutdown,
                filament_sensors,
                fans,
            } => (
                false,
                some(&[z_offset_mm.is_some(), auto_shutdown.is_some()])
                    + filament_sensors.len()
                    + fans.len(),
            ),
            Self::Readings(readings) => (
                false,
                readings.fields.len()
                    + usize::from(readings.status != crate::shared::workspace::ReadStatus::Ready),
            ),
            Self::Lifetime(totals) => (
                false,
                some(&[
                    totals.print_duration_s.is_some(),
                    totals.filament_used_mm.is_some(),
                    totals.jobs.is_some(),
                    totals.total_duration_s.is_some(),
                    totals.longest_print_duration_s.is_some(),
                    totals.longest_total_duration_s.is_some(),
                ]),
            ),
            Self::Device(device) => (
                false,
                some(&[
                    device.manufacturer.is_some() || device.model.is_some(),
                    device.firmware.is_some(),
                    device.server.is_some(),
                    device.firmware_ready.is_some(),
                    device.nozzle_count.is_some(),
                    device.build_volume_mm.is_some(),
                    device.location.is_some(),
                    device.serial_suffix.is_some(),
                    device.camera_retry,
                ]) + device.extra.len(),
            ),
        }
    }

    /// Count facts; non-job cards without facts are omitted.
    pub(in crate::gpui_app) fn facts(&self) -> usize {
        let (hero, rows) = self.shape();
        usize::from(hero) + rows
    }
}

pub(in crate::gpui_app) struct Card<'a> {
    pub(in crate::gpui_app) kind: CardKind,
    pub(in crate::gpui_app) content: Content<'a>,
}

impl Card<'_> {
    /// Describe card content for sizing.
    pub(in crate::gpui_app) fn shape(&self) -> Shape {
        match &self.content {
            Content::Camera(_) => Shape::Camera,
            Content::TemperatureHistory => Shape::Chart,
            Content::Materials(supplies) => Shape::Spools(supplies.len()),
            Content::Readings(readings) => readings::sections::shape(readings),
            content => {
                let (hero, rows) = content.shape();
                Shape::Block(Block { hero, rows })
            }
        }
    }
}

/// Build cards in weighted order; omit empty cards except the job card.
pub(in crate::gpui_app) fn cards(printer: &PrinterEntry) -> Vec<Card<'_>> {
    let telemetry = &printer.state.telemetry;
    let detail = &printer.detail_state;
    let fans = fan_facts(printer);
    let mut cards = Vec::new();

    cards.push(Card {
        kind: CardKind::Job,
        content: Content::Job,
    });

    // Neither fan order nor the active nozzle establishes a resource association.
    let nozzle_devices = detail
        .devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Nozzle)
        .collect::<Vec<_>>();
    let count = telemetry.nozzles.len().max(nozzle_devices.len());
    for index in 0..count {
        let nozzle = telemetry.nozzles.get(index);
        let device = nozzle_devices.get(index).copied();
        cards.push(Card {
            kind: CardKind::Nozzle(index),
            content: Content::Heater(HeaterFacts {
                current_c: nozzle
                    .and_then(|n| n.nozzle_temp_c)
                    .or(device.and_then(|d| d.temperature_c)),
                target_c: nozzle
                    .and_then(|n| n.nozzle_target_temp_c)
                    .or(device.and_then(|d| d.target_temperature_c)),
                active: nozzle.map(|n| n.is_active),
                diameter_mm: device.and_then(|d| d.nozzle_diameter_mm),
                power_pct: device.and_then(|d| d.output_pct),
            }),
        });
    }
    let bed_devices = detail
        .devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Bed)
        .collect::<Vec<_>>();
    for index in 0..telemetry.beds.len().max(bed_devices.len()) {
        let bed = telemetry.beds.get(index);
        let device = bed_devices.get(index).copied();
        cards.push(Card {
            kind: CardKind::Bed(index),
            content: Content::Heater(HeaterFacts {
                current_c: bed
                    .and_then(|b| b.bed_temp_c)
                    .or(device.and_then(|d| d.temperature_c)),
                target_c: bed
                    .and_then(|b| b.bed_target_temp_c)
                    .or(device.and_then(|d| d.target_temperature_c)),
                active: None,
                diameter_mm: None,
                power_pct: device.and_then(|d| d.output_pct),
            }),
        });
    }
    if shows_camera(printer) {
        cards.push(Card {
            kind: CardKind::Camera,
            content: Content::Camera(reported_camera(printer)),
        });
    }
    cards.push(Card {
        kind: CardKind::Materials,
        content: Content::Materials(supplies(printer)),
    });
    if !printer.temperature_history.is_empty() {
        cards.push(Card {
            kind: CardKind::TemperatureHistory,
            content: Content::TemperatureHistory,
        });
    }

    let (chamber_fans, machine_fans): (Vec<_>, Vec<_>) = fans.into_iter().partition(|fan| {
        matches!(
            fan.role,
            Some(FanRole::Chamber | FanRole::Exhaust | FanRole::Recirculation)
        )
    });
    cards.push(Card {
        kind: CardKind::Chamber,
        content: Content::Chamber {
            temperature_c: telemetry.chamber_temp_c,
            fans: chamber_fans,
        },
    });
    cards.push(Card {
        kind: CardKind::Environment,
        content: Content::Environment(
            detail
                .devices
                .iter()
                .filter(|device| device.kind == DeviceKind::TemperatureSensor)
                .map(|device| {
                    let environment = device.readings.environment.as_ref();
                    EnvironmentFact {
                        name: device.display_name.clone(),
                        temperature_c: device.temperature_c,
                        relative_humidity_pct: environment
                            .and_then(|environment| environment.relative_humidity_pct),
                        pressure_pa: environment.and_then(|environment| environment.pressure_pa),
                    }
                })
                .collect(),
        ),
    });
    let motion = detail
        .devices
        .iter()
        .filter(|device| matches!(device.kind, DeviceKind::Motion | DeviceKind::MotionSettings))
        .collect::<Vec<_>>();
    let settings = || {
        motion
            .iter()
            .filter_map(|device| device.readings.motion.as_ref())
    };
    cards.push(Card {
        kind: CardKind::Motion,
        content: Content::Motion(MotionFacts {
            position_mm: super::super::machine::motion_position(printer)
                .map(|(position, _, _)| position),
            homed_axes: settings().find_map(|motion| motion.homed_axes),
        }),
    });
    cards.push(Card {
        kind: CardKind::Machine,
        content: Content::Machine {
            z_offset_mm: detail.machine.z_offset_mm,
            auto_shutdown: detail.machine.auto_shutdown_enabled,
            filament_sensors: telemetry
                .filament_sensors
                .iter()
                .filter_map(|sensor| sensor.filament_present)
                .collect(),
            fans: machine_fans,
        },
    });
    if let Some(totals) = detail.history_totals.value.as_ref() {
        cards.push(Card {
            kind: CardKind::Lifetime,
            content: Content::Lifetime(totals),
        });
    }

    let device = &detail.machine.device;
    let software = |role: SoftwareRole| {
        detail
            .machine
            .software
            .iter()
            .find(|software| software.role == role)
            .map(|software| software.version.as_str())
    };
    let mut extra = readings::equipment(detail);
    if let Some(speed) = telemetry
        .current_job
        .as_ref()
        .and_then(|job| job.print_speed_override_pct)
    {
        extra.push(Field::new(
            "printers.overview.speed_override",
            readings::Value::Number(speed, readings::Unit::Percent),
        ));
    }
    cards.push(Card {
        kind: CardKind::Device,
        content: Content::Device(DeviceFacts {
            manufacturer: device.manufacturer.as_deref(),
            model: device.model.as_deref(),
            firmware: software(SoftwareRole::PrinterFirmware)
                .or(device.firmware_version.as_deref()),
            server: software(SoftwareRole::ConnectionServer)
                .or(detail.system.software_version.as_deref()),
            firmware_ready: detail.system.firmware_ready,
            nozzle_count: device.nozzle_count,
            build_volume_mm: device.build_volume_mm,
            location: device.location_label.as_deref(),
            serial_suffix: printer
                .metadata
                .serial_suffix
                .as_deref()
                .filter(|suffix| !suffix.trim().is_empty()),
            camera_retry: camera_retry(printer),
            extra,
        }),
    });

    // The shared printer card carries temperatures. Keep resource details below;
    // removing these cards before omission preserves diameter, power and targets.
    cards.retain(|card| !matches!(card.kind, CardKind::Nozzle(_) | CardKind::Bed(_)));
    let mut technical = readings::cards(printer);
    readings::presentation::omit_summary_fields(printer, &cards, &mut technical);
    cards.extend(technical);
    cards.retain(|card| card.kind == CardKind::Job || card.content.facts() > 0);
    // Sort by weight, then card kind; nozzle indices stay ascending.
    cards.sort_by_key(|card| (std::cmp::Reverse(card.kind.weight()), card.kind));
    cards
}
