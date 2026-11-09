use serde_json::{Value, json};

use crate::shared::machine::{DryerResource, DryingCycle, DryingLimits, DryingState};
use crate::shared::workspace::ResourceId;

use super::{control, hex, integer};

#[derive(Clone, Copy)]
pub(super) struct Unit<'a> {
    pub(super) id: i64,
    pub(super) kind: u8,
    info: u64,
    data: &'a Value,
}

impl Unit<'_> {
    fn state(self) -> DryingState {
        match (self.info >> 4) & 0xf {
            0 if self.remaining() == Some(0) => DryingState::Idle,
            1 => DryingState::Starting,
            2 => DryingState::Drying,
            3 => DryingState::Cooling,
            4 => DryingState::Stopping,
            5 | 6 => DryingState::Fault,
            _ => DryingState::Unknown,
        }
    }

    fn remaining(self) -> Option<u16> {
        integer(&self.data["dry_time"])
            .and_then(|minutes| u16::try_from(minutes).ok())
            .filter(|minutes| *minutes <= 24 * 60)
    }

    fn target(self) -> Option<DryingCycle> {
        let setting = &self.data["dry_setting"];
        let cycle = DryingCycle {
            temperature_c: u16::try_from(integer(&setting["dry_temperature"])?).ok()?,
            duration_hours: u16::try_from(integer(&setting["dry_duration"])?).ok()?,
        };
        self.limits(self.hardware_max())
            .contains(cycle)
            .then_some(cycle)
    }

    fn hardware_max(self) -> u16 {
        if self.kind == 3 { 65 } else { 85 }
    }

    fn limits(self, maximum_c: u16) -> DryingLimits {
        DryingLimits {
            temperature_c: [45, maximum_c],
            duration_hours: [1, 24],
        }
    }

    fn material(self, print: &Value) -> Option<Material> {
        let mask = hex(&print["ams"]["tray_exist_bits"])?;
        let trays = self.data["tray"].as_array()?;
        let slots = if self.kind == 3 { 4 } else { 1 };
        let mut limiting: Option<Material> = None;
        for slot in 0..slots {
            let (_, bit) = super::station_slot(self.id, slot)?;
            if mask & (1 << bit) == 0 {
                continue;
            }
            let mut matching = trays
                .iter()
                .filter(|tray| integer(&tray["id"]) == Some(slot));
            let tray = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            let profile = Material::reported(tray)?;
            if limiting.is_none_or(|current| {
                (profile.maximum_c, profile.cooling_c) < (current.maximum_c, current.cooling_c)
            }) {
                limiting = Some(profile);
            }
        }
        limiting
    }

    pub(super) fn resource(self, id: ResourceId, print: &Value) -> DryerResource {
        let state = self.state();
        let limits = self
            .material(print)
            .map(|material| self.limits(self.hardware_max().min(material.maximum_c)));
        let label = if self.kind == 3 {
            format!("AMS 2 Pro {}", self.id + 1)
        } else {
            format!("AMS HT {}", self.id - 127)
        };
        DryerResource {
            id,
            label,
            state,
            limits,
            remaining_minutes: self.remaining(),
            target: self.target(),
            can_start: limits.is_some()
                && state == DryingState::Idle
                && control::can_heat(print)
                && control::ams_phase(print) == Some(0)
                && self
                    .data
                    .get("dry_sf_reason")
                    .is_none_or(|reasons| reasons.as_array().is_some_and(Vec::is_empty)),
            can_stop: matches!(
                state,
                DryingState::Starting | DryingState::Drying | DryingState::Fault
            ),
        }
    }

    pub(super) fn payload(self, print: &Value, cycle: Option<DryingCycle>) -> Option<Value> {
        let (mode, filament, temp, duration, cooling) = if let Some(cycle) = cycle {
            let material = self.material(print)?;
            (
                1,
                material.kind,
                cycle.temperature_c,
                cycle.duration_hours,
                material.cooling_c,
            )
        } else {
            (0, "", 0, 0, 0)
        };
        Some(json!({
            "ams_id": self.id, "mode": mode, "filament": filament,
            "temp": temp, "duration": duration, "humidity": 0,
            "rotate_tray": false, "cooling_temp": cooling, "close_power_conflict": false
        }))
    }

    pub(super) fn confirms(self, cycle: Option<DryingCycle>) -> bool {
        if let Some(cycle) = cycle {
            matches!(self.state(), DryingState::Starting | DryingState::Drying)
                && self.target() == Some(cycle)
        } else {
            matches!(
                self.state(),
                DryingState::Idle | DryingState::Cooling | DryingState::Stopping
            )
        }
    }
}

#[derive(Clone, Copy)]
struct Material {
    kind: &'static str,
    maximum_c: u16,
    cooling_c: u16,
}

impl Material {
    fn reported(tray: &Value) -> Option<Self> {
        // Keep the limits of the exact Bambu Studio profiles, not the tray's material label alone.
        let (kind, maximum_c, cooling_c) =
            match (tray["tray_info_idx"].as_str()?, tray["tray_type"].as_str()?) {
                ("GFA00" | "GFL99", "PLA") => ("PLA", 45, 50),
                ("GFG02" | "GFG99", "PETG") => ("PETG", 75, 60),
                _ => return None,
            };
        Some(Self {
            kind,
            maximum_c,
            cooling_c,
        })
    }
}

fn remote_supported(print: &Value) -> bool {
    let Some(flags) = print["fun2"].as_str().map(str::trim) else {
        return false;
    };
    let flags = flags
        .strip_prefix("0x")
        .or_else(|| flags.strip_prefix("0X"))
        .unwrap_or(flags);
    flags.bytes().all(|byte| byte.is_ascii_hexdigit())
        && flags
            .chars()
            .rev()
            .nth(1)
            .and_then(|digit| digit.to_digit(16))
            .is_some_and(|nibble| nibble & 2 != 0)
}

pub(super) fn units(print: &Value) -> impl Iterator<Item = Unit<'_>> {
    let present = hex(&print["ams"]["ams_exist_bits"]);
    let capable = remote_supported(print) && control::unsigned_controls(print);
    let units = print["ams"]["ams"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    units.iter().filter_map(move |data| {
        let id = integer(&data["id"])?;
        let info = hex(&data["info"])?;
        let kind = (info & 0xf) as u8;
        let bit = match (kind, id) {
            (3, 0..=3) => id,
            (4, 128..=135) => id - 124,
            _ => return None,
        };
        (capable
            && present.is_some_and(|bits| bits & (1 << bit) != 0)
            && units
                .iter()
                .filter(|unit| integer(&unit["id"]) == Some(id))
                .take(2)
                .count()
                == 1)
            .then_some(Unit {
                id,
                kind,
                info,
                data,
            })
    })
}
