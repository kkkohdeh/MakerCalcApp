//! Bambu Lab hardware per model and the projection of a report into the device inventory.
//!
//! A model shows only the hardware it has: a capability from the table plus the
//! presence of its field. Nothing here branches on a model family.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::shared::control::MachineActionKind;
use crate::shared::machine::{
    DeviceMetadata, EnvironmentalReadings, FanRole, MachineActivity, SoftwareRole, SoftwareVersion,
    SupplyKind, SupplyResource,
};
use crate::shared::printer::PrinterObservation;
use crate::shared::telemetry::{ElementId, FilamentSupplyTelemetry};
use crate::shared::workspace::{
    Capabilities, DetailState, Device, DeviceKind, ResourceId, Support,
};

/// Hardware of one model, from the printer profiles shipped with Bambu Studio
/// (`resources/printers/<code>.json`).
#[derive(Debug)]
pub(super) struct Model {
    pub(super) name: &'static str,
    /// `support_chamber_temp_display`, falling back to `support_chamber`.
    pub(super) chamber_temperature: bool,
    /// `support_chamber_temp_edit`: an active chamber heater with a target.
    pub(super) chamber_heater: bool,
    /// `support_aux_fan`.
    pub(super) aux_fan: bool,
    /// `support_chamber_fan`.
    pub(super) chamber_fan: bool,
    /// Entries in `tool_head_display_names`.
    pub(super) nozzles: u8,
}

const fn model(
    name: &'static str,
    chamber: (bool, bool),
    fans: (bool, bool),
    nozzles: u8,
) -> Model {
    Model {
        name,
        chamber_temperature: chamber.0,
        chamber_heater: chamber.1,
        aux_fan: fans.0,
        chamber_fan: fans.1,
        nozzles,
    }
}

/// Keyed by the first three characters of the serial number.
static MODELS: [(&str, Model); 14] = [
    ("00M", model("X1 Carbon", (true, false), (true, true), 1)),
    ("00W", model("X1", (true, false), (true, true), 1)),
    ("03W", model("X1E", (true, true), (true, true), 1)),
    ("20P", model("X2D", (true, true), (true, true), 2)),
    ("01S", model("P1P", (false, false), (true, true), 1)),
    ("01P", model("P1S", (false, false), (true, true), 1)),
    ("22E", model("P2S", (true, false), (true, true), 1)),
    ("030", model("A1 mini", (false, false), (false, false), 1)),
    ("039", model("A1", (false, false), (false, false), 1)),
    ("26A", model("A2L", (false, false), (false, false), 1)),
    ("31B", model("H2C", (true, true), (true, true), 2)),
    ("094", model("H2D", (true, true), (true, true), 2)),
    ("239", model("H2D Pro", (true, true), (true, true), 2)),
    ("093", model("H2S", (true, true), (true, true), 1)),
];

/// An unknown prefix has no row: every capability counts as absent.
pub(super) fn model_for_serial(serial: &str) -> Option<&'static Model> {
    let prefix = serial.get(..3)?;
    MODELS
        .iter()
        .find(|(known, _)| *known == prefix)
        .map(|(_, model)| model)
}

static NEXT_RESOURCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Target {
    Nozzle(u32),
    Bed,
    Chamber,
    PartFan,
    AuxFan,
    ChamberFan,
    FilamentSensor(u32),
    AmsSensor(i64),
    Supply(u32),
}

/// Keeps one resource id per physical part for the whole session.
#[derive(Default)]
pub(super) struct Projection {
    ids: BTreeMap<Target, ResourceId>,
}

impl Projection {
    fn id(&mut self, target: Target) -> ResourceId {
        *self
            .ids
            .entry(target)
            .or_insert_with(|| ResourceId(NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed)))
    }

    pub(super) fn update(
        &mut self,
        report: &Value,
        observation: &PrinterObservation,
        model: Option<&Model>,
        state: &mut DetailState,
    ) {
        let print = &report["print"];
        state.devices = self.devices(print, observation, model);
        let station_sensors = self.ams_sensors(print);
        state.machine.material_station_sensors =
            station_sensors.iter().map(|device| device.id).collect();
        state.devices.extend(station_sensors);
        state.machine.device = metadata(report, print, model);
        state.machine.activity = activity(print);
        state.machine.print_speed_profiles = super::control::speed_profiles(print);
        state.machine.actions = state
            .machine
            .print_speed_profiles
            .map_or_else(Vec::new, |_| vec![MachineActionKind::SetPrintSpeedOverride]);
        state.machine.supplies = self.supplies(print, observation);
        state.machine.current_supply = super::material_head(print).and_then(|head| {
            let active = head.loaded?;
            state
                .machine
                .supplies
                .iter()
                .find(|supply| supply.reading.id == active)
                .map(|supply| supply.id)
        });
        state.machine.material_station_slots = station_slots(print);
        state.machine.software = firmware(report)
            .map(|version| SoftwareVersion {
                role: SoftwareRole::PrinterFirmware,
                version,
            })
            .into_iter()
            .collect();
        state.capabilities = Capabilities {
            files: Support::Unavailable,
            history: Support::Unavailable,
            queue: Support::Unavailable,
            cameras: Support::Unavailable,
            power: Support::Unavailable,
            lighting: Support::Unavailable,
            sensors: Support::Unavailable,
            material_station: match observation.material_station_available {
                Some(true) => Support::Available,
                Some(false) => Support::Unavailable,
                None => Support::Unknown,
            },
        };
    }

    fn devices(
        &mut self,
        print: &Value,
        observation: &PrinterObservation,
        model: Option<&Model>,
    ) -> Vec<Device> {
        let mut devices = Vec::new();
        // The single diameter field describes the only nozzle; two nozzles report it elsewhere.
        let diameter = super::number(&print["nozzle_diameter"])
            .filter(|diameter| (0.1..=2.0).contains(diameter))
            .filter(|_| observation.nozzles.len() == 1);
        for nozzle in &observation.nozzles {
            let mut device = Device::new(
                self.id(Target::Nozzle(nozzle.id.0)),
                DeviceKind::Nozzle,
                "Nozzle".into(),
            );
            device.telemetry_id = Some(nozzle.id);
            device.temperature_c = nozzle.nozzle_temp_c;
            device.target_temperature_c = nozzle.nozzle_target_temp_c;
            device.nozzle_diameter_mm = diameter;
            devices.push(device);
        }
        for bed in &observation.beds {
            let mut device = Device::new(self.id(Target::Bed), DeviceKind::Bed, "Bed".into());
            device.telemetry_id = Some(bed.id);
            device.temperature_c = bed.bed_temp_c;
            device.target_temperature_c = bed.bed_target_temp_c;
            devices.push(device);
        }
        devices.extend(self.chamber(print, observation, model));
        devices.extend(self.fans(print, observation, model));
        for sensor in &observation.filament_sensors {
            let mut device = Device::new(
                self.id(Target::FilamentSensor(sensor.id.0)),
                DeviceKind::FilamentSensor,
                String::new(),
            );
            device.telemetry_id = Some(sensor.id);
            device.filament_present = sensor.filament_present;
            devices.push(device);
        }
        devices
    }

    /// A heater when the target is editable; a plain sensor when the model only shows it.
    fn chamber(
        &mut self,
        print: &Value,
        observation: &PrinterObservation,
        model: Option<&Model>,
    ) -> Option<Device> {
        let model = model.filter(|model| model.chamber_temperature)?;
        let target = model
            .chamber_heater
            .then(|| chamber_target(print))
            .flatten();
        if observation.chamber_c.is_none() && target.is_none() {
            return None;
        }
        let kind = if model.chamber_heater {
            DeviceKind::ChamberHeater
        } else {
            DeviceKind::ChamberSensor
        };
        let mut device = Device::new(self.id(Target::Chamber), kind, "Chamber".into());
        device.temperature_c = observation.chamber_c;
        device.target_temperature_c = target;
        Some(device)
    }

    fn fans(
        &mut self,
        print: &Value,
        observation: &PrinterObservation,
        model: Option<&Model>,
    ) -> Vec<Device> {
        let gear = super::integer(&print["fan_gear"]);
        let has = |capability: fn(&Model) -> bool| model.is_some_and(capability);
        let fans = [
            (
                Target::PartFan,
                FanRole::PartCooling,
                observation
                    .part_cooling_fans
                    .first()
                    .and_then(|fan| fan.power_pct),
            ),
            (
                Target::AuxFan,
                FanRole::Auxiliary,
                has(|model| model.aux_fan)
                    .then(|| super::fan_pct(gear, 1, &print["big_fan1_speed"]))
                    .flatten(),
            ),
            (
                Target::ChamberFan,
                FanRole::Chamber,
                has(|model| model.chamber_fan)
                    .then(|| super::fan_pct(gear, 2, &print["big_fan2_speed"]))
                    .flatten(),
            ),
        ];
        fans.into_iter()
            .filter_map(|(target, role, power)| {
                let power = power?;
                let mut device = Device::new(self.id(target), DeviceKind::Fan, "Fan".into());
                device.fan_role = Some(role);
                device.output_pct = Some(power);
                device.enabled = Some(power > 0.0);
                Some(device)
            })
            .collect()
    }

    /// Units with a digital sensor report a real temperature and relative humidity.
    /// Units without one send zeros, and `humidity` is a 1-5 desiccant index, not a percentage.
    fn ams_sensors(&mut self, print: &Value) -> Vec<Device> {
        let units = print["ams"]["ams"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        units
            .iter()
            .filter_map(|unit| {
                let id = super::integer(&unit["id"])
                    .filter(|id| matches!(id, 0..=3 | 16 | 128..=135))?;
                let humidity = super::number(&unit["humidity_raw"])
                    .filter(|humidity| *humidity > 0.0 && *humidity <= 100.0);
                let temperature = super::number(&unit["temp"]).filter(|temp| *temp > 0.0);
                if humidity.is_none() && temperature.is_none() {
                    return None;
                }
                let mut device = Device::new(
                    self.id(Target::AmsSensor(id)),
                    DeviceKind::TemperatureSensor,
                    ams_name(id),
                );
                device.temperature_c = temperature;
                device.readings.environment = humidity.map(|humidity| EnvironmentalReadings {
                    relative_humidity_pct: Some(humidity),
                    ..Default::default()
                });
                Some(device)
            })
            .collect()
    }

    fn supplies(&mut self, print: &Value, observation: &PrinterObservation) -> Vec<SupplyResource> {
        let readings = observation.filament_supplies.as_deref().unwrap_or_default();
        let trays: BTreeMap<_, _> = super::station_trays(print)
            .map(|tray| (tray.id.0, tray))
            .collect();
        let externals = super::external_spools(print);
        readings
            .iter()
            .map(|reading| SupplyResource {
                id: self.id(Target::Supply(reading.id.0)),
                kind: if super::is_external(reading.id) {
                    SupplyKind::ExternalSpool
                } else {
                    SupplyKind::StationSlot
                },
                reading: reading.clone(),
                remaining_pct: remaining_pct(
                    reading,
                    trays.get(&reading.id.0).map(|tray| tray.data),
                    &externals,
                ),
                controls: Some(trays.get(&reading.id.0).map_or_else(Vec::new, |tray| {
                    super::control::supply_controls(print, tray, reading.filament_present)
                })),
                operation: None,
                valid_until: Some(std::time::Instant::now() + std::time::Duration::from_secs(15)),
            })
            .collect()
    }
}

fn remaining_pct(
    reading: &FilamentSupplyTelemetry,
    tray: Option<&Value>,
    externals: &[(ElementId, &Value)],
) -> Option<u8> {
    let data = if let Some(tray) = tray {
        (reading.filament_present == Some(true)).then_some(tray)?
    } else {
        if reading.filament_present == Some(false)
            || (reading.material.is_none() && reading.is_active != Some(true))
        {
            return None;
        }
        externals.iter().find(|(id, _)| *id == reading.id)?.1
    };
    let remaining = super::integer(&data["remain"]).and_then(|value| u8::try_from(value).ok())?;
    (remaining <= 100).then_some(remaining)
}

fn ams_name(id: i64) -> String {
    match id {
        128..=135 => format!("AMS HT {}", id - 127),
        0..=3 => format!("AMS {}", id + 1),
        _ => "AMS".into(),
    }
}

fn chamber_target(print: &Value) -> Option<f64> {
    match super::integer(&print["device"]["ctc"]["info"]["temp"]) {
        Some(packed) => super::unpack(packed).1,
        None => super::number(&print["ctt"]),
    }
}

fn metadata(report: &Value, print: &Value, model: Option<&Model>) -> DeviceMetadata {
    let modules = report["info"]["module"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let extruders = super::integer(&print["device"]["extruder"]["state"])
        .and_then(|state| u8::try_from(state & 0x0F).ok())
        .filter(|count| (1..=16).contains(count));
    DeviceMetadata {
        reported_name: modules
            .iter()
            .find_map(|module| super::text(&module["product_name"], 128)),
        manufacturer: Some("Bambu Lab".into()),
        model: model.map(|model| model.name.into()),
        firmware_version: firmware(report),
        nozzle_count: extruders.or(model.map(|model| model.nozzles)),
        ..Default::default()
    }
}

fn firmware(report: &Value) -> Option<String> {
    report["info"]["module"]
        .as_array()?
        .iter()
        .find(|module| module["name"] == "ota")
        .and_then(|module| super::text(&module["sw_ver"], 128))
}

/// Stage codes from Bambu Studio's `DevPrintingStage`. Printing itself, idle and
/// finished carry no extra activity.
fn activity(print: &Value) -> Option<MachineActivity> {
    if print["gcode_state"] == "SLICING" {
        return Some(MachineActivity::Slicing);
    }
    match super::integer(&print["stg_cur"])? {
        2 | 7 | 49 | 50 | 54 | 58 => Some(MachineActivity::Heating),
        1 | 3 | 8..=12 | 18 | 19 | 25 | 31 | 36..=40 | 43 | 46..=48 | 56 | 57 | 60 | 61 | 65 => {
            Some(MachineActivity::Calibrating)
        }
        4 | 13 | 14 | 22 | 24 | 68 | 70 => Some(MachineActivity::Busy),
        _ => None,
    }
}

/// Declared slots of the connected units; never inferred from loaded spools.
fn station_slots(print: &Value) -> Option<u8> {
    let units = print["ams"]["ams"].as_array()?;
    let slots: usize = units
        .iter()
        .map(|unit| unit["tray"].as_array().map_or(0, Vec::len))
        .sum();
    u8::try_from(slots).ok().filter(|slots| *slots > 0)
}
