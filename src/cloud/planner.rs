use crate::i18n::t;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use super::wire::{WirePrinter, encode_batch, same_wire};
use super::{
    ACK_TIMEOUT, Device, LOCAL_MAX_MESSAGE_BYTES, LOCAL_MAX_PRINTERS, LOCAL_MAX_TELEMETRY_BYTES,
    MAX_INTERVAL_MS, MIN_INTERVAL_MS, REFRESH_WINDOW,
};
use crate::shared::workspace::HistoryTotals;
use crate::state::{LocalPrinterId, PrinterState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Limits {
    pub(super) max_message_bytes: usize,
    pub(super) max_printers: usize,
    pub(super) min_interval: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: LOCAL_MAX_MESSAGE_BYTES,
            max_printers: LOCAL_MAX_PRINTERS,
            min_interval: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
pub(super) struct ObservedPrinter {
    pub(super) id: LocalPrinterId,
    pub(super) generation: u64,
    pub(super) cloud_printer_id: String,
    pub(super) name: String,
    pub(super) state: PrinterState,
    pub(super) history_totals: Option<HistoryTotals>,
    pub(super) device: Option<Device>,
    pub(super) thumbnail_sha256: Option<String>,
    pub(super) thumbnail_supported: bool,
}

#[derive(Clone)]
pub(super) struct DeliveryRecord {
    pub(super) observed: ObservedPrinter,
    pub(super) last_observed_revision: u64,
    pub(super) last_observed_wire: Option<String>,
    pub(super) delivery_revision: u64,
    pub(super) dirty_since: Option<Instant>,
    pub(super) last_sent_at: Option<Instant>,
    pub(super) last_accepted_revision: Option<u64>,
    pub(super) blocked_reason: Option<Cow<'static, str>>,
    pub(super) forbidden_retry_at: Option<Instant>,
}

#[derive(Clone)]
pub(super) struct CapturedRevision {
    pub(super) id: LocalPrinterId,
    pub(super) generation: u64,
    pub(super) cloud_printer_id: String,
    pub(super) revision: u64,
}

pub(super) struct InFlight {
    pub(super) epoch: u64,
    pub(super) seq: u64,
    pub(super) captured: Vec<CapturedRevision>,
    pub(super) sent_at: Instant,
}

pub(super) struct Planner {
    pub(super) records: BTreeMap<LocalPrinterId, DeliveryRecord>,
    pub(super) cursor_after: Option<LocalPrinterId>,
    pub(super) limits: Limits,
    pub(super) epoch: u64,
    pub(super) next_seq: u64,
    pub(super) last_batch_started_at: Option<Instant>,
    pub(super) in_flight: Option<InFlight>,
    #[cfg(test)]
    pub(super) work: WorkCounts,
}

#[derive(Clone)]
pub(super) struct PendingBatch {
    pub(super) epoch: u64,
    pub(super) seq: u64,
    pub(super) payload: String,
    pub(super) captured: Vec<CapturedRevision>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum PlannerError {
    RevisionExhausted,
    SequenceExhausted,
    InvalidLimits,
}

impl fmt::Display for PlannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionExhausted => {
                formatter.write_str(&t!("settings.cloud.errors.revision_exhausted"))
            }
            Self::SequenceExhausted => {
                formatter.write_str(&t!("settings.cloud.errors.sequence_exhausted"))
            }
            Self::InvalidLimits => formatter.write_str(&t!("settings.cloud.errors.invalid_limits")),
        }
    }
}

impl std::error::Error for PlannerError {}

impl Planner {
    pub(super) fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            cursor_after: None,
            limits: Limits::default(),
            epoch: 1,
            next_seq: 1,
            last_batch_started_at: None,
            in_flight: None,
            #[cfg(test)]
            work: WorkCounts::default(),
        }
    }

    pub(super) fn begin_session(
        &mut self,
        max_message_bytes: u64,
        max_printers: u64,
        min_interval_ms: u64,
    ) -> Result<(), PlannerError> {
        let message_limit =
            usize::try_from(max_message_bytes).map_err(|_| PlannerError::InvalidLimits)?;
        let printer_limit =
            usize::try_from(max_printers).map_err(|_| PlannerError::InvalidLimits)?;
        if message_limit == 0
            || printer_limit == 0
            || !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&min_interval_ms)
        {
            return Err(PlannerError::InvalidLimits);
        }
        self.limits = Limits {
            max_message_bytes: message_limit.min(LOCAL_MAX_MESSAGE_BYTES),
            max_printers: printer_limit.min(LOCAL_MAX_PRINTERS),
            min_interval: Duration::from_millis(min_interval_ms),
        };
        self.epoch = self
            .epoch
            .checked_add(1)
            .ok_or(PlannerError::RevisionExhausted)?;
        self.next_seq = 1;
        self.in_flight = None;
        self.last_batch_started_at = None;
        let now = Instant::now();
        for record in self.records.values_mut() {
            record.last_sent_at = None;
            record.last_accepted_revision = None;
            record.dirty_since = Some(now);
            record.blocked_reason = None;
            record.forbidden_retry_at = None;
        }
        Ok(())
    }

    fn encode_observed(&mut self, printer: &ObservedPrinter, now: Instant) -> Option<String> {
        let mut wire = WirePrinter::from_state(printer, now);
        wire.age_ms = None;
        #[cfg(test)]
        {
            self.work.observed_serializations += 1;
        }
        serde_json::to_string(&wire).ok()
    }

    pub(super) fn observe_one(&mut self, printer: ObservedPrinter, now: Instant) {
        let id = printer.id;
        match self.records.remove(&id) {
            Some(mut record)
                if record.observed.generation == printer.generation
                    && record.observed.cloud_printer_id == printer.cloud_printer_id =>
            {
                if record.last_observed_revision != printer.state.revision
                    || record.observed.history_totals != printer.history_totals
                    || record.observed.device != printer.device
                    || record.observed.thumbnail_sha256 != printer.thumbnail_sha256
                    || record.observed.thumbnail_supported != printer.thumbnail_supported
                    || record.observed.name != printer.name
                {
                    record.last_observed_revision = printer.state.revision;
                    let encoded = self.encode_observed(&printer, now);
                    if !same_wire(record.last_observed_wire.as_deref(), encoded.as_deref()) {
                        record.last_observed_wire = encoded;
                        record.delivery_revision = record.delivery_revision.saturating_add(1);
                        record.dirty_since.get_or_insert(now);
                        record.blocked_reason = None;
                    }
                }
                record.observed = printer;
                self.records.insert(id, record);
            }
            _ => {
                let encoded = self.encode_observed(&printer, now);
                self.records.insert(
                    id,
                    DeliveryRecord {
                        last_observed_revision: printer.state.revision,
                        last_observed_wire: encoded,
                        delivery_revision: 1,
                        observed: printer,
                        dirty_since: Some(now),
                        last_sent_at: None,
                        last_accepted_revision: None,
                        blocked_reason: None,
                        forbidden_retry_at: None,
                    },
                );
            }
        }
    }

    pub(super) fn retain(&mut self, ids: &BTreeSet<LocalPrinterId>) {
        self.records.retain(|id, _| ids.contains(id));
        if self
            .cursor_after
            .is_some_and(|id| !self.records.contains_key(&id))
        {
            self.cursor_after = None;
        }
    }

    pub(super) fn pending_count(&self, now: Instant) -> usize {
        self.records
            .values()
            .filter(|record| is_pending(record, now))
            .count()
    }

    pub(super) fn local_error(&self) -> Option<String> {
        self.records.values().find_map(|record| {
            record.blocked_reason.as_ref().map(|reason| {
                t!(
                    "settings.cloud.errors.not_sent",
                    printer = &record.observed.cloud_printer_id,
                    reason = reason
                )
                .into_owned()
            })
        })
    }

    fn interval_elapsed(&self, now: Instant) -> bool {
        self.last_batch_started_at.is_none_or(|last| {
            now.saturating_duration_since(last)
                >= self
                    .limits
                    .min_interval
                    .saturating_sub(Duration::from_millis(100))
        })
    }

    pub(super) fn has_due_batch(&self, now: Instant) -> bool {
        self.interval_elapsed(now) && self.records.values().any(|record| is_due(record, now))
    }

    pub(super) fn select(&mut self, now: Instant) -> Result<Option<PendingBatch>, PlannerError> {
        if self.in_flight.is_some() || !self.interval_elapsed(now) {
            return Ok(None);
        }
        let eligible = self.eligible_ids(now);
        if eligible.is_empty() {
            return Ok(None);
        }
        let seq = self.next_seq;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or(PlannerError::SequenceExhausted)?;
        let mut selected = Vec::new();
        let mut captured = Vec::new();
        let mut skipped = Vec::new();
        let mut payload = encode_batch(seq, &[])?;
        let empty_batch_bytes = payload.len();
        #[cfg(test)]
        {
            self.work.batch_serializations += 1;
        }
        let mut batch_bytes = empty_batch_bytes;
        for id in eligible {
            if selected.len() == self.limits.max_printers {
                break;
            }
            let Some(record) = self.records.get(&id) else {
                continue;
            };
            let candidate = WirePrinter::from_state(&record.observed, now);
            #[cfg(test)]
            {
                self.work.telemetry_serializations += usize::from(candidate.telemetry.is_some());
            }
            let telemetry_bytes = candidate
                .telemetry
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| PlannerError::InvalidLimits)?;
            if telemetry_bytes
                .as_ref()
                .is_some_and(|json| json.len() > LOCAL_MAX_TELEMETRY_BYTES)
            {
                skipped.push((id, t!("settings.cloud.errors.telemetry_size")));
                continue;
            }
            #[cfg(test)]
            {
                self.work.candidate_serializations += 1;
            }
            let candidate_json =
                serde_json::to_string(&candidate).map_err(|_| PlannerError::InvalidLimits)?;
            let candidate_bytes = candidate_json.len();
            if empty_batch_bytes + candidate_bytes > self.limits.max_message_bytes {
                skipped.push((id, t!("settings.cloud.errors.state_size")));
                continue;
            }
            let additional_bytes = candidate_bytes + usize::from(!selected.is_empty());
            if batch_bytes + additional_bytes <= self.limits.max_message_bytes {
                batch_bytes += additional_bytes;
                selected.push(candidate_json);
                captured.push(CapturedRevision {
                    id,
                    generation: record.observed.generation,
                    cloud_printer_id: record.observed.cloud_printer_id.clone(),
                    revision: record.delivery_revision,
                });
            }
        }
        for (id, reason) in skipped {
            if let Some(record) = self.records.get_mut(&id) {
                record.dirty_since = None;
                record.blocked_reason = Some(reason);
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        // Each fragment was already serialized to enforce its exact byte limit.
        payload.truncate(payload.len() - 2);
        payload.reserve(batch_bytes.saturating_sub(payload.len()));
        for (index, candidate) in selected.iter().enumerate() {
            if index != 0 {
                payload.push(',');
            }
            payload.push_str(candidate);
        }
        payload.push_str("]}");
        self.cursor_after = captured.last().map(|captured| captured.id);
        Ok(Some(PendingBatch {
            epoch: self.epoch,
            seq,
            payload,
            captured,
        }))
    }

    pub(super) fn mark_sent(&mut self, batch: PendingBatch, now: Instant) {
        for captured in &batch.captured {
            if let Some(record) = self.records.get_mut(&captured.id) {
                record.last_sent_at = Some(now);
                record.dirty_since = None;
            }
        }
        self.last_batch_started_at = Some(now);
        self.in_flight = Some(InFlight {
            epoch: batch.epoch,
            seq: batch.seq,
            captured: batch.captured,
            sent_at: now,
        });
    }

    pub(super) fn acknowledge(&mut self, epoch: u64, seq: u64) -> bool {
        let Some(in_flight) = self.in_flight.take() else {
            return false;
        };
        if in_flight.epoch != epoch || in_flight.seq != seq {
            self.in_flight = Some(in_flight);
            return false;
        }
        for captured in in_flight.captured {
            let Some(record) = self.records.get_mut(&captured.id) else {
                continue;
            };
            if record.observed.generation == captured.generation
                && record.observed.cloud_printer_id == captured.cloud_printer_id
                && record.delivery_revision == captured.revision
            {
                record.last_accepted_revision = Some(captured.revision);
            }
        }
        true
    }

    pub(super) fn reject(&mut self, epoch: u64, seq: u64, now: Instant, code: &str) -> bool {
        let Some(in_flight) = self.in_flight.take() else {
            return false;
        };
        if in_flight.epoch != epoch || in_flight.seq != seq {
            self.in_flight = Some(in_flight);
            return false;
        }
        for captured in in_flight.captured {
            if let Some(record) = self.records.get_mut(&captured.id) {
                if record.observed.generation != captured.generation
                    || record.observed.cloud_printer_id != captured.cloud_printer_id
                {
                    continue;
                }
                record.dirty_since = Some(now);
                if code == "forbidden" {
                    record.forbidden_retry_at = Some(now + REFRESH_WINDOW);
                }
            }
        }
        true
    }

    pub(super) fn adopt_retry_interval(&mut self, retry_after_ms: u64) {
        let interval =
            Duration::from_millis(retry_after_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS));
        self.limits.min_interval = self.limits.min_interval.max(interval);
    }

    pub(super) fn ack_timed_out(&self, now: Instant) -> bool {
        self.in_flight
            .as_ref()
            .is_some_and(|batch| now.saturating_duration_since(batch.sent_at) >= ACK_TIMEOUT)
    }

    pub(super) fn disconnect(&mut self) {
        self.in_flight = None;
        self.last_batch_started_at = None;
        for record in self.records.values_mut() {
            record.last_sent_at = None;
            record.dirty_since = None;
        }
    }

    fn eligible_ids(&self, now: Instant) -> Vec<LocalPrinterId> {
        let mut ids = self
            .records
            .iter()
            .filter_map(|(id, record)| is_due(record, now).then_some(*id))
            .collect::<Vec<_>>();
        if let Some(after) = self.cursor_after {
            let split = ids.partition_point(|id| *id <= after);
            ids.rotate_left(split);
        }
        ids
    }
}

pub(super) fn is_pending(record: &DeliveryRecord, now: Instant) -> bool {
    record
        .last_accepted_revision
        .is_none_or(|accepted| record.delivery_revision > accepted)
        || record
            .last_sent_at
            .is_some_and(|last| now.saturating_duration_since(last) >= REFRESH_WINDOW)
}

pub(super) fn is_due(record: &DeliveryRecord, now: Instant) -> bool {
    if record.blocked_reason.is_some() || record.forbidden_retry_at.is_some_and(|retry| now < retry)
    {
        return false;
    }
    let changed_due = record.dirty_since.is_some();
    let refresh_due = record
        .last_sent_at
        .is_some_and(|last| now.saturating_duration_since(last) >= REFRESH_WINDOW);
    changed_due || refresh_due
}
