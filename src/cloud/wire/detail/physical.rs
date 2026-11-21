use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::{Value, json};

use super::{Fields, MAX_ELEMENTS, WireDetail, element_id};
use crate::shared::control::{MAX_EXTRUSION_MM, MachineActionKind as A};
use crate::shared::telemetry::ElementId;
use crate::shared::workspace::{DetailState, DeviceKind};
use crate::state::{LanConnection, PrintStatus, PrinterState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Permission {
    reachable: bool,
    physical: bool,
    busy: bool,
}

impl Permission {
    fn current(state: &PrinterState, busy: bool, now: Instant) -> Self {
        let reachable = state.controls.remote
            && state.telemetry_ready
            && state.connection == LanConnection::Connected
            && state.age_ms(now).is_some_and(|age| age <= 15_000);
        Self {
            reachable,
            physical: reachable && state.permits_remote_physical(),
            busy: reachable && busy,
        }
    }
}

impl WireDetail {
    pub(in crate::cloud) fn controls_current(
        &self,
        state: &PrinterState,
        busy: bool,
        now: Instant,
    ) -> bool {
        self.permission == Permission::current(state, busy, now)
            && self.supply_deadline.is_none_or(|deadline| now < deadline)
    }

    pub(in crate::cloud) fn project_controls(
        mut self,
        detail: &DetailState,
        state: &PrinterState,
        busy: bool,
        now: Instant,
    ) -> Self {
        let permission = Permission::current(state, busy, now);
        self.permission = permission;
        let machine = &detail.machine;
        let status = state.telemetry.print_state;
        let idle = matches!(
            status,
            PrintStatus::Idle | PrintStatus::Completed | PrintStatus::Cancelled
        );
        let move_ready = permission.physical
            && !busy
            && idle
            && machine.motion_idle != Some(false)
            && machine.activity.is_none();
        let calibration_ready = move_ready
            && machine
                .calibration_readiness
                .is_some_and(|ready| ready.check().is_ok());
        let plate_ready = permission.physical
            && !busy
            && status.is_completed_or_cancelled()
            && machine.material_motion_idle == Some(true)
            && machine.activity.is_none();
        let actions: Vec<_> = machine
            .actions
            .iter()
            .copied()
            .filter(|action| match action {
                A::HomeAxes => move_ready,
                A::MoveAxisRelative => move_ready && machine.remote_jog,
                A::DisableMotors => move_ready && machine.motion_idle == Some(true),
                A::RunCalibration => calibration_ready && !machine.calibrations.is_empty(),
                A::ConfirmBuildPlateCleared => plate_ready,
                A::ExtrudeFilament | A::LoadFilament | A::UnloadFilament | A::StartDrying => {
                    permission.physical && !busy
                }
                A::CancelFilamentOperation | A::StopDrying => permission.reachable,
                _ => true,
            })
            .collect();
        let mut group = self
            .telemetry
            .remove("machine")
            .and_then(|v| match v {
                Value::Object(fields) => Some(fields),
                _ => None,
            })
            .unwrap_or_default();
        group.remove("actions");
        insert_list(&mut group, "actions", &actions);
        if permission.physical {
            group.insert("remote_physical".into(), json!(true));
        }
        if permission.reachable && !matches!(status, PrintStatus::Printing | PrintStatus::Paused) {
            insert_list(
                &mut group,
                "temperature_targets",
                &detail.temperature_targets,
            );
        }
        if calibration_ready {
            insert_list(&mut group, "calibrations", &machine.calibrations);
        }

        if move_ready {
            let nozzles = detail
                .devices
                .iter()
                .filter(|device| device.kind == DeviceKind::Nozzle);
            let ids = super::super::technical::unique_ids(
                nozzles.clone().filter_map(|device| device.telemetry_id),
            );
            for nozzle in nozzles {
                let Some(id) = nozzle.telemetry_id else {
                    continue;
                };
                let mut rows = state.telemetry.nozzles.iter().filter(|row| row.id == id);
                if ids.get(&id) == Some(&1)
                    && nozzle.can_extrude == Some(true)
                    && nozzle.actions.contains(&A::ExtrudeFilament)
                    && rows.next().is_some_and(|row| row.is_active)
                    && rows.next().is_none()
                    && let Some(limit) = nozzle
                        .extrusion_limit_mm
                        .filter(|limit| limit.is_finite() && *limit > 0.0)
                {
                    let fields = self.elements.entry(("nozzles", id)).or_default();
                    fields.insert(
                        "extrusion_limit_mm".into(),
                        json!(limit.min(MAX_EXTRUSION_MM)),
                    );
                    insert_list(fields, "actions", &[A::ExtrudeFilament]);
                }
            }
        }

        if permission.reachable || machine.current_supply.is_some() || !machine.dryers.is_empty() {
            let mut published = BTreeSet::new();
            let base_ids = super::super::technical::unique_ids(
                state
                    .telemetry
                    .filament_supplies
                    .iter()
                    .flatten()
                    .map(|supply| supply.id),
            );
            for row in state.telemetry.filament_supplies.iter().flatten() {
                if published.len() >= MAX_ELEMENTS {
                    break;
                }
                published.insert(row.id);
            }
            let ids = super::super::technical::unique_ids(
                machine.supplies.iter().map(|supply| supply.reading.id),
            );
            for supply in &machine.supplies {
                let id = supply.reading.id;
                if !published.contains(&id)
                    || ids.get(&id) != Some(&1)
                    || base_ids.get(&id) != Some(&1)
                {
                    continue;
                }
                let current = machine.current_supply == Some(supply.id);
                if current {
                    group.insert("current_supply".into(), json!(id));
                }
                let ready = [
                    A::LoadFilament,
                    A::UnloadFilament,
                    A::CancelFilamentOperation,
                ]
                .into_iter()
                .filter(|action| {
                    permission.reachable
                        && (*action == A::CancelFilamentOperation
                            || (permission.physical
                                && !busy
                                && idle
                                && (*action != A::UnloadFilament || current)))
                        && supply.availability(*action, now).is_ok()
                })
                .collect::<Vec<_>>();
                if !ready.is_empty() {
                    if let Some(deadline) = supply.valid_until {
                        self.supply_deadline = Some(
                            self.supply_deadline
                                .map_or(deadline, |previous| previous.min(deadline)),
                        );
                    }
                    insert_list(
                        self.elements.entry(("filament_supplies", id)).or_default(),
                        "actions",
                        &ready,
                    );
                }
            }
            if let Some(rows) = self
                .telemetry
                .get_mut("dryers")
                .and_then(Value::as_array_mut)
            {
                let ids = super::super::technical::unique_ids(
                    machine.dryers.iter().map(|dryer| dryer.telemetry_id),
                );
                for row in rows {
                    let Some(id) = element_id(row)
                        .and_then(|id| u32::try_from(id).ok())
                        .map(ElementId)
                    else {
                        continue;
                    };
                    if ids.get(&id) != Some(&1) {
                        continue;
                    }
                    let Some(dryer) = machine.dryers.iter().find(|dryer| dryer.telemetry_id == id)
                    else {
                        continue;
                    };
                    let actions = [
                        (
                            permission.physical
                                && !busy
                                && dryer.can_start
                                && dryer.limits.is_some(),
                            A::StartDrying,
                        ),
                        (permission.reachable && dryer.can_stop, A::StopDrying),
                    ]
                    .into_iter()
                    .filter_map(|(available, action)| available.then_some(action))
                    .collect::<Vec<_>>();
                    if let Some(fields) = row.as_object_mut() {
                        insert_list(fields, "actions", &actions);
                        if let Some(limits) = dryer.limits {
                            fields.insert("limits".into(), json!(limits));
                        }
                    }
                }
            }
        }
        if !group.is_empty() {
            self.telemetry
                .insert("machine".into(), Value::Object(group));
        }
        self
    }
}

fn insert_list<T: serde::Serialize>(fields: &mut Fields, name: &str, values: &[T]) {
    if !values.is_empty() {
        fields.insert(
            name.into(),
            json!(&values[..values.len().min(MAX_ELEMENTS)]),
        );
    }
}
