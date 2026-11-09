use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::shared::control::{
    ControlError, ControlResult, MachineAction, MachineActionKind, MachineCommand,
};
use crate::shared::machine::{
    DryingCycle, PrintSpeedProfile, PrintSpeedProfiles, SupplyBlock, SupplyControl,
    SupplyOperation, SupplyOperationPhase,
};
use crate::shared::telemetry::ElementId;
use crate::shared::workspace::{
    DetailState, HeaterKind, LightRole, PrintControlContext, PrinterAction,
    PrinterActionKind as Kind, PrinterControls, ResourceId,
};
use crate::state::{LanConnection, MachineJobTarget, PrinterState};

use super::{MaterialHead, material_head};

pub(super) fn unsigned_controls(print: &Value) -> bool {
    print
        .get("fun")
        .is_none_or(|value| super::hex(value).is_some_and(|bits| bits & 0x2000_0000 == 0))
}

pub(super) fn speed_profile(print: &Value) -> Option<PrintSpeedProfile> {
    let level = usize::try_from(super::integer(&print["spd_lvl"])?).ok()?;
    PrintSpeedProfile::ALL.get(level.checked_sub(1)?).copied()
}

pub(super) fn speed_profiles(print: &Value) -> Option<PrintSpeedProfiles> {
    if !unsigned_controls(print) {
        return None;
    }
    Some(PrintSpeedProfiles {
        current: speed_profile(print)?,
        can_change: print["gcode_state"] == "RUNNING",
    })
}

pub(super) fn temperature_targets(print: &Value) -> Vec<HeaterKind> {
    if !can_heat(print) {
        return Vec::new();
    }
    [HeaterKind::Nozzle, HeaterKind::Bed]
        .into_iter()
        .filter(|heater| heater_target(print, *heater).is_some())
        .collect()
}

pub(super) fn can_heat(print: &Value) -> bool {
    unsigned_controls(print)
        && matches!(print["gcode_state"].as_str(), Some("IDLE" | "FINISH"))
        && print
            .get("ams_status")
            .is_none_or(|_| ams_phase(print) == Some(0))
        && !material_activity(print)
}

fn heater_target(print: &Value, heater: HeaterKind) -> Option<f64> {
    match heater {
        HeaterKind::Nozzle => {
            let extruder = &print["device"]["extruder"];
            if extruder["info"].is_array()
                && !super::integer(&extruder["state"])
                    .is_some_and(|state| state > 0 && (state >> 4) & 0xf == 0)
            {
                return None;
            }
            let mut nozzles = super::nozzles(print)
                .into_iter()
                .filter(|n| n.id == ElementId(1) && n.is_active);
            let nozzle = nozzles.next()?;
            if nozzles.next().is_some()
                || nozzle
                    .nozzle_temp_c
                    .is_none_or(|temp| temp <= 0.0 || temp > 500.0)
            {
                return None;
            }
            nozzle.nozzle_target_temp_c
        }
        HeaterKind::Bed => {
            let bed = super::beds(print).into_iter().next()?;
            bed.bed_temp_c
                .filter(|temp| *temp > 0.0 && *temp <= 200.0)?;
            bed.bed_target_temp_c
        }
    }
    .filter(|target| (0.0..=500.0).contains(target))
}

pub(super) fn availability(report: &Value) -> PrinterControls {
    let print = &report["print"];
    let unsigned = unsigned_controls(print);
    let running = print["gcode_state"].as_str() == Some("RUNNING");
    let paused = print["gcode_state"].as_str() == Some("PAUSE");
    PrinterControls {
        pause: unsigned && running,
        resume: unsigned && paused,
        cancel: unsigned && (running || paused),
        build_area_light: light_target(print, LightRole::BuildArea).is_some(),
        toolhead_light: light_target(print, LightRole::Toolhead).is_some(),
        remote: false,
    }
}

fn light_target(print: &Value, role: LightRole) -> Option<(&'static str, bool)> {
    let mut nodes = print["lights_report"]
        .as_array()?
        .iter()
        .filter_map(|light| {
            let (node, _, actual_role) = super::light_node(light["node"].as_str()?)?;
            (role == actual_role).then_some((node, &light["mode"]))
        });
    let (node, mode) = nodes.next()?;
    if nodes.next().is_some() {
        return None;
    }
    match mode.as_str()? {
        "on" | "flashing" => Some((node, true)),
        "off" => Some((node, false)),
        _ => None,
    }
}

pub(super) fn ams_phase(print: &Value) -> Option<i64> {
    super::integer(&print["ams_status"])
        .filter(|status| (0..=65535).contains(status))
        .map(|status| status >> 8)
}

fn material_block(print: &Value) -> Result<(), SupplyBlock> {
    if print["gcode_state"] != "IDLE" {
        return Err(SupplyBlock::NotIdle);
    }
    let phase = ams_phase(print).ok_or(SupplyBlock::Unknown)?;
    if phase != 0 || material_activity(print) {
        return Err(SupplyBlock::Busy);
    }
    Ok(())
}

fn material_activity(print: &Value) -> bool {
    super::hex(&print["ams"]["tray_reading_bits"]).is_some_and(|bits| bits != 0)
        || super::integer(&print["stg_cur"]).is_some_and(|stage| !matches!(stage, -1 | 0 | 255))
        || super::integer(&print["print_error"]).is_some_and(|error| error != 0)
}

fn routed_to_head(print: &Value, tray: &super::StationTray<'_>, head: MaterialHead) -> bool {
    if !matches!(tray.ams_id, 0..=3 | 128..=135) || (tray.ams_id >= 128 && tray.slot != 0) {
        return false;
    }
    let Some(extruder) = head.extruder else {
        return true;
    };
    match super::integer(&print["device"]["extruder"]["state"]).map(|state| state & 0xf) {
        Some(1) => true,
        Some(2..=15) if extruder < 14 => super::hex(&tray.unit["info"])
            .is_some_and(|info| (info >> 8) & 0xf == u64::from(extruder)),
        _ => false,
    }
}

fn temperature(tray: &Value) -> Option<i64> {
    let min = super::integer(&tray["nozzle_temp_min"])?;
    let max = super::integer(&tray["nozzle_temp_max"])?;
    let limit = i64::from(HeaterKind::Nozzle.max_target_c());
    if !(150..=limit).contains(&min) || !(min..=350).contains(&max) {
        return None;
    }
    Some((min + max.min(limit)) / 2)
}

fn drying(unit: &Value) -> bool {
    super::integer(&unit["dry_time"]).is_some_and(|minutes| minutes > 0)
        || super::hex(&unit["info"]).is_some_and(|info| (info >> 4) & 0xf != 0)
}

fn filament_target(
    print: &Value,
    tray: &super::StationTray<'_>,
    action: MachineActionKind,
    present: Option<bool>,
) -> Result<(MaterialHead, i64, i64), SupplyBlock> {
    material_block(print)?;
    let head = material_head(print).ok_or(SupplyBlock::Unknown)?;
    if !routed_to_head(print, tray, head) {
        return Err(SupplyBlock::Unsupported);
    }
    let unloading = action == MachineActionKind::UnloadFilament;
    if unloading != (head.loaded == Some(tray.id)) {
        return Err(SupplyBlock::NotCurrent);
    }
    if !unloading && present != Some(true) {
        return Err(if present.is_some() {
            SupplyBlock::NoFilament
        } else {
            SupplyBlock::Unknown
        });
    }
    let target = temperature(tray.data).ok_or(SupplyBlock::Unknown)?;
    let old = if let Some(id) = head.loaded {
        let loaded = super::station_trays(print)
            .find(|source| source.id == id)
            .ok_or(SupplyBlock::Unknown)?;
        if !routed_to_head(print, &loaded, head) || drying(loaded.unit) {
            return Err(SupplyBlock::Busy);
        }
        temperature(loaded.data).ok_or(SupplyBlock::Unknown)?
    } else {
        -1
    };
    if drying(tray.unit) {
        return Err(SupplyBlock::Busy);
    }
    Ok((head, old, target))
}

pub(super) fn supply_controls(
    print: &Value,
    tray: &super::StationTray<'_>,
    present: Option<bool>,
) -> Vec<SupplyControl> {
    let Some(head) = material_head(print) else {
        return Vec::new();
    };
    if !unsigned_controls(print) || !routed_to_head(print, tray, head) {
        return Vec::new();
    }
    [
        MachineActionKind::LoadFilament,
        MachineActionKind::UnloadFilament,
    ]
    .into_iter()
    .filter(|action| *action != MachineActionKind::LoadFilament || head.loaded != Some(tray.id))
    .map(|action| SupplyControl {
        action,
        blocked: filament_target(print, tray, action, present).err(),
    })
    .collect()
}

#[derive(Clone, Copy)]
enum Setting {
    Speed(PrintSpeedProfile),
    Temperature(HeaterKind, u16),
    Dryer {
        unit: i64,
        kind: u8,
        cycle: Option<DryingCycle>,
    },
}

enum Target {
    Job(PrintControlContext),
    Light {
        node: &'static str,
        on: bool,
    },
    Setting {
        expected: Setting,
        revision: u64,
        job: u64,
        reply: Option<oneshot::Sender<ControlResult>>,
    },
    Filament {
        head: MaterialHead,
        supply: ResourceId,
        tray: ElementId,
        action: MachineActionKind,
        job: u64,
        ams_idle_reported: bool,
        reply: oneshot::Sender<ControlResult>,
    },
}

pub(super) struct Pending {
    pub(super) kind: Kind,
    pub(super) deadline: tokio::time::Instant,
    id: u16,
    group: &'static str,
    command: &'static str,
    target: Target,
    acknowledged: bool,
}

impl Pending {
    pub(super) fn prepare(
        action: &PrinterAction,
        state: &PrinterState,
        report: &Value,
        id: u16,
    ) -> Option<(Self, Value)> {
        let kind = action.kind();
        let sequence = id.to_string();
        let print = &report["print"];
        if let PrinterAction::SetTemperatureTarget {
            heater,
            target_c,
            connection_epoch,
            submitted_at,
        } = *action
        {
            if !fresh_request(state, connection_epoch, submitted_at)
                || target_c > heater.max_target_c()
                || !can_heat(print)
                || heater_target(print, heater).is_none()
            {
                return None;
            }
            let (command, payload) = match heater {
                HeaterKind::Nozzle if print["device"]["extruder"]["info"].is_array() => (
                    "set_nozzle_temp",
                    json!({"extruder_index": 0, "target_temp": target_c}),
                ),
                HeaterKind::Bed
                    if super::hex(&print["fun"]).is_some_and(|bits| bits & (1 << 39) != 0) =>
                {
                    ("set_bed_temp", json!({"temp": target_c}))
                }
                _ => {
                    let code = if heater == HeaterKind::Nozzle {
                        "M104"
                    } else {
                        "M140"
                    };
                    (
                        "gcode_line",
                        json!({"param": format!("{code} S{target_c}\n")}),
                    )
                }
            };
            return Some(Self::setting(
                id,
                kind,
                state,
                Setting::Temperature(heater, target_c),
                None,
                command,
                payload,
            ));
        }
        let (target, group, command) = match action {
            PrinterAction::PausePrint(context)
            | PrinterAction::ResumePrint(context)
            | PrinterAction::CancelPrint(context)
                if state.permits_print_control(kind, context) =>
            {
                let command = match kind {
                    Kind::PausePrint => "pause",
                    Kind::ResumePrint => "resume",
                    _ => "stop",
                };
                (Target::Job(*context), "print", command)
            }
            PrinterAction::ToggleLight {
                role,
                expected_enabled,
                connection_epoch,
                submitted_at,
            } if state.connection == LanConnection::Connected
                && state.telemetry_ready
                && state.connection_epoch == *connection_epoch
                && state
                    .age_ms(Instant::now())
                    .is_some_and(|age| age <= 15_000)
                && submitted_at.elapsed() <= Duration::from_secs(5) =>
            {
                let (node, on) = light_target(&report["print"], *role)?;
                if *expected_enabled != Some(on) {
                    return None;
                }
                (Target::Light { node, on: !on }, "system", "ledctrl")
            }
            _ => return None,
        };
        let payload = match target {
            Target::Job(_) => json!({"print": {
                "sequence_id": sequence, "command": command, "param": ""
            }}),
            Target::Light { node, on } => json!({"system": {
                "sequence_id": sequence, "command": command, "led_node": node,
                "led_mode": if on { "on" } else { "off" },
                "led_on_time": 500, "led_off_time": 500, "loop_times": 1, "interval_time": 1000
            }}),
            Target::Filament { .. } | Target::Setting { .. } => return None,
        };
        Some((
            Self {
                kind,
                deadline: tokio::time::Instant::now() + Duration::from_secs(10),
                id,
                group,
                command,
                target,
                acknowledged: false,
            },
            payload,
        ))
    }

    fn setting(
        id: u16,
        kind: Kind,
        state: &PrinterState,
        expected: Setting,
        reply: Option<oneshot::Sender<ControlResult>>,
        command: &'static str,
        mut print: Value,
    ) -> (Self, Value) {
        print["sequence_id"] = json!(id.to_string());
        print["command"] = json!(command);
        (
            Self {
                kind,
                deadline: tokio::time::Instant::now() + Duration::from_secs(10),
                id,
                group: "print",
                command,
                target: Target::Setting {
                    expected,
                    revision: state.revision,
                    job: state.job_generation,
                    reply,
                },
                acknowledged: false,
            },
            json!({"print": print}),
        )
    }

    pub(super) fn prepare_machine(
        command: MachineCommand,
        state: &PrinterState,
        report: &Value,
        detail: &DetailState,
        id: u16,
    ) -> Option<(Self, Value)> {
        let MachineAction::SetPrintSpeedOverride { percent } = command.action else {
            return Self::prepare_filament(command, state, report, detail, id);
        };
        let prepare = || {
            command.action.validate()?;
            if state.connection != LanConnection::Connected {
                return Err(ControlError::Disconnected);
            }
            if !detail.live
                || !fresh_request(state, command.connection_epoch, command.submitted_at)
                || command.job_target != Some(MachineJobTarget::Active(state.job_generation))
                || state.machine_job_target() != MachineJobTarget::Active(state.job_generation)
            {
                return Err(ControlError::StaleResource);
            }
            let profiles = speed_profiles(&report["print"]).ok_or(ControlError::Unsupported)?;
            if !profiles.can_change {
                return Err(ControlError::InvalidState);
            }
            let profile = PrintSpeedProfile::selectable()
                .find(|profile| profile.percent() == percent)
                .ok_or(ControlError::InvalidData)?;
            let level = PrintSpeedProfile::ALL
                .iter()
                .position(|candidate| *candidate == profile)
                .ok_or(ControlError::Unsupported)?
                + 1;
            Ok((profile, level))
        };
        match prepare() {
            Ok((profile, level)) => Some(Self::setting(
                id,
                Kind::MachineControl,
                state,
                Setting::Speed(profile),
                Some(command.reply),
                "print_speed",
                json!({"param": level.to_string()}),
            )),
            Err(error) => {
                let _ = command.reply.send(ControlResult::Rejected(error));
                None
            }
        }
    }

    pub(super) fn prepare_drying(
        command: MachineCommand,
        state: &PrinterState,
        report: &Value,
        detail: &DetailState,
        projection: &super::machine::Projection,
        id: u16,
    ) -> Option<(Self, Value)> {
        let prepare = || {
            command.action.validate()?;
            let (resource, cycle) = match command.action {
                MachineAction::StartDrying { dryer_id, cycle } => (dryer_id, Some(cycle)),
                MachineAction::StopDrying { dryer_id } => (dryer_id, None),
                _ => return Err(ControlError::Unsupported),
            };
            if state.connection != LanConnection::Connected {
                return Err(ControlError::Disconnected);
            }
            if !detail.live || !fresh_request(state, command.connection_epoch, command.submitted_at)
            {
                return Err(ControlError::StaleResource);
            }
            let (unit, kind) = projection
                .dryer(resource)
                .ok_or(ControlError::StaleResource)?;
            let print = &report["print"];
            let source = super::drying::units(print)
                .find(|source| source.id == unit && source.kind == kind)
                .ok_or(ControlError::StaleResource)?;
            let current = source.resource(resource, print);
            if let Some(cycle) = cycle {
                if !current.can_start {
                    return Err(ControlError::InvalidState);
                }
                if !current.limits.is_some_and(|limits| limits.contains(cycle)) {
                    return Err(ControlError::InvalidData);
                }
            } else if !current.can_stop {
                return Err(ControlError::InvalidState);
            }
            let payload = source
                .payload(print, cycle)
                .ok_or(ControlError::InvalidState)?;
            Ok((Setting::Dryer { unit, kind, cycle }, payload))
        };
        match prepare() {
            Ok((expected, payload)) => Some(Self::setting(
                id,
                Kind::MachineControl,
                state,
                expected,
                Some(command.reply),
                "ams_filament_drying",
                payload,
            )),
            Err(error) => {
                let _ = command.reply.send(ControlResult::Rejected(error));
                None
            }
        }
    }

    pub(super) fn prepare_filament(
        command: MachineCommand,
        state: &PrinterState,
        report: &Value,
        detail: &DetailState,
        id: u16,
    ) -> Option<(Self, Value)> {
        let prepare = || {
            let kind = command.action.kind();
            let supply_id = match command.action {
                MachineAction::LoadFilament { supply_id }
                | MachineAction::UnloadFilament { supply_id } => supply_id,
                _ => return Err(ControlError::Unsupported),
            };
            if state.connection != LanConnection::Connected {
                return Err(ControlError::Disconnected);
            }
            if state.connection_epoch != command.connection_epoch
                || !state.telemetry_ready
                || !detail.live
                || state.age_ms(Instant::now()).is_none_or(|age| age > 15_000)
                || command.submitted_at.elapsed() > Duration::from_secs(5)
            {
                return Err(ControlError::StaleResource);
            }
            let print = &report["print"];
            if !unsigned_controls(print) {
                return Err(ControlError::Unsupported);
            }
            let resource = detail
                .machine
                .supplies
                .iter()
                .find(|resource| resource.id == supply_id)
                .ok_or(ControlError::StaleResource)?;
            resource
                .availability(kind, Instant::now())
                .map_err(SupplyBlock::error)?;
            let mut trays =
                super::station_trays(print).filter(|tray| tray.id == resource.reading.id);
            let tray = trays.next().ok_or(ControlError::StaleResource)?;
            if trays.next().is_some() {
                return Err(ControlError::InvalidState);
            }
            let (head, old, target) =
                filament_target(print, &tray, kind, resource.reading.filament_present)
                    .map_err(SupplyBlock::error)?;
            let unloading = kind == MachineActionKind::UnloadFilament;
            let mut payload = json!({"print": {
                "sequence_id": id.to_string(), "command": "ams_change_filament",
                "ams_id": tray.ams_id, "slot_id": if unloading { 255 } else { tray.slot },
                "target": if unloading { 255 } else { tray.id.0 },
                "curr_temp": old, "tar_temp": target
            }});
            if let Some(extruder) = head.extruder {
                payload["print"]["extruder_id"] = json!(extruder);
            }
            let expected = MaterialHead {
                loaded: (!unloading).then_some(tray.id),
                ..head
            };
            Ok((expected, supply_id, tray.id, kind, payload))
        };
        match prepare() {
            Ok((head, supply, tray, action, payload)) => Some((
                Self {
                    kind: Kind::MachineControl,
                    deadline: tokio::time::Instant::now() + Duration::from_secs(180),
                    id,
                    group: "print",
                    command: "ams_change_filament",
                    target: Target::Filament {
                        head,
                        supply,
                        tray,
                        action,
                        job: state.job_generation,
                        ams_idle_reported: false,
                        reply: command.reply,
                    },
                    acknowledged: false,
                },
                payload,
            )),
            Err(error) => {
                let _ = command.reply.send(ControlResult::Rejected(error));
                None
            }
        }
    }

    pub(super) fn acknowledge(&mut self, message: &Value) -> bool {
        let reply = &message[self.group];
        if super::integer(&reply["sequence_id"]) != Some(i64::from(self.id))
            || reply["command"]
                .as_str()
                .is_some_and(|command| command != self.command)
        {
            return true;
        }
        match reply["result"].as_str() {
            Some(result) if result.eq_ignore_ascii_case("success") => self.acknowledged = true,
            Some(result)
                if ["fail", "failed", "error"]
                    .iter()
                    .any(|failure| result.eq_ignore_ascii_case(failure)) =>
            {
                return false;
            }
            _ => {}
        }
        true
    }

    pub(super) fn confirmed(&self, report: &Value, state: &PrinterState) -> bool {
        if !self.acknowledged || tokio::time::Instant::now() >= self.deadline {
            return false;
        }
        let print = &report["print"];
        match self.target {
            Target::Job(context) => {
                state.job_generation == context.job_generation
                    && match self.kind {
                        Kind::PausePrint => print["gcode_state"] == "PAUSE",
                        Kind::ResumePrint => print["gcode_state"] == "RUNNING",
                        Kind::CancelPrint => {
                            print["gcode_state"] == "IDLE"
                                || super::status(print) == Some(super::PrintStatus::Cancelled)
                        }
                        _ => false,
                    }
            }
            Target::Light { node, on } => {
                let expected = if on { "on" } else { "off" };
                print["lights_report"].as_array().is_some_and(|lights| {
                    lights
                        .iter()
                        .any(|light| light["node"] == node && light["mode"] == expected)
                })
            }
            Target::Setting {
                expected,
                revision,
                job,
                ..
            } => {
                state.revision > revision
                    && match expected {
                        Setting::Speed(profile) => {
                            state.job_generation == job && speed_profile(print) == Some(profile)
                        }
                        Setting::Temperature(heater, target) => {
                            state.job_generation == job
                                && heater_target(print, heater) == Some(f64::from(target))
                        }
                        Setting::Dryer { unit, kind, cycle } => {
                            super::drying::units(print).any(|source| {
                                source.id == unit && source.kind == kind && source.confirms(cycle)
                            })
                        }
                    }
            }
            Target::Filament {
                head,
                tray,
                job,
                ams_idle_reported,
                ..
            } => {
                ams_idle_reported
                    && state.job_generation == job
                    && state
                        .age_ms(Instant::now())
                        .is_some_and(|age| age <= 15_000)
                    && material_block(print).is_ok()
                    && material_head(print) == Some(head)
                    && super::station_trays(print).any(|source| source.id == tray)
            }
        }
    }

    pub(super) fn observe_ams_status(&mut self, report: &Value) {
        if let Target::Filament {
            ams_idle_reported, ..
        } = &mut self.target
        {
            *ams_idle_reported = ams_phase(&report["print"]) == Some(0);
        }
    }

    pub(super) fn dryer_missing(&self, report: &Value) -> bool {
        if let Target::Setting {
            expected: Setting::Dryer { unit, kind, .. },
            ..
        } = self.target
        {
            return !super::drying::units(&report["print"])
                .any(|source| source.id == unit && source.kind == kind);
        }
        false
    }

    pub(super) fn operation(&self, report: &Value) -> Option<(ResourceId, SupplyOperation)> {
        let Target::Filament {
            head,
            supply,
            action,
            ..
        } = self.target
        else {
            return None;
        };
        let print = &report["print"];
        let target = head.loaded.map_or(255, |id| i64::from(id.0));
        (self.acknowledged
            && ams_phase(print) == Some(1)
            && material_head(print).is_some_and(|current| current.extruder == head.extruder)
            && super::integer(&print["ams"]["tray_tar"]) == Some(target))
        .then_some((
            supply,
            SupplyOperation {
                action,
                phase: SupplyOperationPhase::Running,
                step: None,
            },
        ))
    }

    pub(super) fn finish(self, result: ControlResult) -> crate::shared::workspace::OperationState {
        use crate::shared::workspace::OperationState;
        if let Target::Filament { reply, .. }
        | Target::Setting {
            reply: Some(reply), ..
        } = self.target
        {
            let _ = reply.send(result);
            return OperationState::Idle;
        }
        match result {
            ControlResult::Accepted => OperationState::Accepted(self.kind),
            ControlResult::Unknown => OperationState::Unknown(self.kind),
            ControlResult::Rejected(_) => OperationState::Rejected {
                action: self.kind,
                message: crate::i18n::t!("printers.bambulab.errors.control_rejected").into_owned(),
            },
        }
    }
}

fn fresh_request(state: &PrinterState, epoch: u64, submitted_at: Instant) -> bool {
    state.connection == LanConnection::Connected
        && state.telemetry_ready
        && state.connection_epoch == epoch
        && state
            .age_ms(Instant::now())
            .is_some_and(|age| age <= 15_000)
        && submitted_at.elapsed() <= Duration::from_secs(5)
}
