use super::*;
use tokio::sync::oneshot;

pub(super) struct PendingLoad {
    source: ResourceId,
    target: ResourceId,
    epoch: u64,
    job_target: Option<crate::state::MachineJobTarget>,
    unload_heating_generation: Option<u64>,
    started: Instant,
    saw_unloading: bool,
    saw_nonterminal_step: bool,
    remote: Option<crate::shared::control::RemoteSession>,
    reply: oneshot::Sender<ControlResult>,
}

impl Projection {
    pub(in crate::integrations::flashforge) fn load_source(
        &self,
        action: &MachineAction,
        detail: &DetailState,
    ) -> Option<ResourceId> {
        match action {
            MachineAction::LoadFilament { .. } => detail.machine.current_supply,
            _ => None,
        }
    }

    pub(in crate::integrations::flashforge) fn defer_load(
        &mut self,
        source: ResourceId,
        target: ResourceId,
        command: crate::shared::control::MachineCommand,
        started: Instant,
    ) {
        self.pending_load = Some(PendingLoad {
            source,
            target,
            epoch: command.connection_epoch,
            job_target: command.job_target,
            unload_heating_generation: None,
            started,
            saw_unloading: false,
            saw_nonterminal_step: matches!(self.station_step, Some(0 | 4 | 5)),
            remote: command.remote,
            reply: command.reply,
        });
    }

    pub(in crate::integrations::flashforge) fn remote_load_pending(&self) -> bool {
        self.pending_load
            .as_ref()
            .is_some_and(|load| load.remote.is_some())
    }

    pub(in crate::integrations::flashforge) fn load_deadline(&self) -> Option<Instant> {
        self.pending_load
            .as_ref()
            .map(|load| load.started + Duration::from_secs(180))
    }

    pub(in crate::integrations::flashforge) fn abort_load(&mut self, result: ControlResult) {
        if let Some(load) = self.pending_load.take() {
            let _ = load.reply.send(result);
        }
    }

    pub(in crate::integrations::flashforge) fn refresh_supply_controls(
        &self,
        detail: &mut DetailState,
    ) {
        for supply in &mut detail.machine.supplies {
            if let Some(mut controls) = supply.controls.take() {
                for control in &mut controls {
                    control.blocked = self.supply_block(supply, control.action);
                }
                supply.controls = Some(controls);
            }
        }
    }

    /// Continue only with fresh evidence that the requested unload finished.
    pub(in crate::integrations::flashforge) fn next_load(
        &mut self,
        state: &PrinterState,
        detail: &mut DetailState,
    ) -> Option<(serde_json::Value, oneshot::Sender<ControlResult>)> {
        let mut load = self.pending_load.take()?;
        let source = detail.machine.supplies.iter().find(|s| s.id == load.source);
        let target = detail.machine.supplies.iter().find(|s| s.id == load.target);
        let valid = !load.reply.is_closed()
            && load.started.elapsed() < Duration::from_secs(180)
            && state.connection == LanConnection::Connected
            && state.connection_epoch == load.epoch
            && load.remote.as_ref().map_or(detail.live, |permit| {
                permit.permits(
                    &MachineAction::LoadFilament {
                        supply_id: load.target,
                    },
                    state,
                )
            })
            && source.is_some()
            && target.is_some()
            && detail.machine.issues.is_empty()
            && !state
                .telemetry
                .printer_alerts
                .as_ref()
                .is_some_and(|alerts| !alerts.is_empty())
            && status_load_active(self.status.as_deref())
            && self
                .current_slot
                .is_some_and(|slot| slot == 0 || source.is_some_and(|s| slot == s.reading.id.0));
        if !valid {
            let _ = load.reply.send(ControlResult::Unknown);
            self.refresh_supply_controls(detail);
            return None;
        }
        if self.observed_at.is_none_or(|at| at <= load.started) {
            self.pending_load = Some(load);
            return None;
        }
        let operation = source.and_then(|s| s.operation);
        if let Some(crate::state::MachineJobTarget::Inactive(generation)) = load.job_target.as_ref()
            && load.unload_heating_generation.is_none()
            && generation.checked_add(1) == Some(state.job_generation)
            && status_matches(self.status.as_deref(), &["heating"])
            && operation.is_some_and(|op| {
                op.action == K::UnloadFilament && op.phase == SupplyOperationPhase::Running
            })
            && state
                .telemetry
                .current_job
                .as_ref()
                .is_some_and(|job| job.print_name.is_none())
        {
            // FlashForge reports anonymous unload heating as printing.
            load.unload_heating_generation = Some(state.job_generation);
        }
        if load.job_target.as_ref().is_some_and(|target| {
            !state.matches_machine_job_target(target)
                && load.unload_heating_generation != Some(state.job_generation)
        }) {
            let _ = load.reply.send(ControlResult::Unknown);
            self.refresh_supply_controls(detail);
            return None;
        }
        if let Some(operation) = operation {
            if operation.action != K::UnloadFilament
                || operation.phase == SupplyOperationPhase::Cancelling
            {
                let _ = load.reply.send(ControlResult::Unknown);
                self.refresh_supply_controls(detail);
                return None;
            }
            load.saw_unloading |= operation.phase == SupplyOperationPhase::Running;
        }
        let station_idle = self.station_action == Some(0)
            && matches!(self.status.as_deref(), Some("ready" | "idle"));
        load.saw_nonterminal_step |= matches!(self.station_step, Some(0 | 4 | 5));
        let unload_completed = load.saw_unloading
            && operation.is_some_and(|op| op.phase == SupplyOperationPhase::Completed);
        // A new completion with no selected slot can arrive between running samples.
        let completed_between_polls = station_idle
            && load.saw_nonterminal_step
            && self.current_slot == Some(0)
            && self.station_step == Some(6)
            && self.station_operation.is_none();
        if !unload_completed && !completed_between_polls {
            if station_idle
                && self.station_step == Some(0)
                && (load.saw_unloading || self.current_slot == Some(0))
            {
                let _ = load.reply.send(ControlResult::Unknown);
                self.refresh_supply_controls(detail);
            } else {
                self.pending_load = Some(load);
            }
            return None;
        }
        self.refresh_supply_controls(detail);
        match self.payload(
            &MachineAction::LoadFilament {
                supply_id: load.target,
            },
            state,
            detail,
            load.epoch,
        ) {
            Ok(payload) => Some((payload, load.reply)),
            Err(error) => {
                let _ = load.reply.send(ControlResult::Rejected(error));
                None
            }
        }
    }
}
