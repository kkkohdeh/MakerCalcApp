use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::shared::workspace::{
    LightRole, PrintControlContext, PrinterAction, PrinterActionKind as Kind, PrinterControls,
};
use crate::state::{LanConnection, PrinterState};

pub(super) fn availability(report: &Value) -> PrinterControls {
    let print = &report["print"];
    let unsigned = print
        .get("fun")
        .is_none_or(|value| super::hex(value).is_some_and(|bits| bits & 0x2000_0000 == 0));
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

enum Target {
    Job(PrintControlContext),
    Light { node: &'static str, on: bool },
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
            Some("success") => self.acknowledged = true,
            Some("fail" | "failed" | "error") => return false,
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
        }
    }
}
