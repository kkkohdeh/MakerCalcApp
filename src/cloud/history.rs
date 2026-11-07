use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::SCHEMA_VERSION;

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct HistoryCursor {
    pub(super) live_printer_id: String,
    pub(super) since: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
pub(super) struct JobsBatch<'a> {
    pub(super) r#type: &'static str,
    pub(super) v: u8,
    pub(super) seq: u64,
    pub(super) jobs: &'a [crate::shared::history::PrintJob],
}

#[derive(Default)]
pub(super) struct UploadPosition {
    pub(super) offset: usize,
    pub(super) selection: Option<(u64, Vec<i64>)>,
    pub(super) finished_version: Option<u64>,
    pub(super) seen: BTreeSet<(chrono::DateTime<chrono::Utc>, String)>,
}

pub(super) struct HistorySession {
    pub(super) max: usize,
    pub(super) cursors: BTreeMap<String, HistoryCursor>,
    pub(super) positions: BTreeMap<String, UploadPosition>,
    pub(super) announced: BTreeSet<String>,
    pub(super) pending: Vec<crate::shared::history::PrintJob>,
    sent_count: usize,
    pub(super) flight: Option<(u64, Instant)>,
    pub(super) retry_at: Instant,
    pub(super) interval: Duration,
    pub(super) last_sent: Option<Instant>,
}

impl HistorySession {
    pub(super) fn new(max: Option<usize>, interval: Duration) -> Self {
        Self {
            max: max.unwrap_or(0).min(10_000),
            cursors: BTreeMap::new(),
            positions: BTreeMap::new(),
            announced: BTreeSet::new(),
            pending: Vec::new(),
            sent_count: 0,
            flight: None,
            retry_at: Instant::now(),
            interval: interval / 2,
            last_sent: None,
        }
    }

    pub(super) fn cursors(&mut self, cursors: Vec<HistoryCursor>) {
        let next: BTreeMap<_, _> = cursors
            .into_iter()
            .take(250)
            .map(|cursor| (cursor.live_printer_id.clone(), cursor))
            .collect();
        self.positions.retain(|id, position| {
            let Some(cursor) = next.get(id) else {
                return false;
            };
            if Some(cursor) != self.cursors.get(id) {
                let advancing = self
                    .cursors
                    .get(id)
                    .is_some_and(|previous| cursor.since >= previous.since);
                if !advancing {
                    *position = UploadPosition::default();
                } else if position.selection.is_none() {
                    position.offset = 0;
                    position.finished_version = None;
                }
            }
            true
        });
        self.cursors = next;
        let prefix = if self.flight.is_some() {
            self.sent_count
        } else {
            0
        };
        let mut tail = self.pending.split_off(prefix);
        tail.retain(|job| self.cursors.contains_key(&job.printer_id));
        self.pending.extend(tail);
    }

    pub(super) fn retry_after_rejection(&mut self, when: Instant) {
        self.flight = None;
        self.sent_count = 0;
        self.pending
            .retain(|job| self.cursors.contains_key(&job.printer_id));
        self.retry_at = when;
    }

    pub(super) fn mark_sent(&mut self, seq: u64, count: usize, now: Instant) {
        self.sent_count = count;
        self.flight = Some((seq, now));
        self.last_sent = Some(now);
    }

    pub(super) fn accepted(&mut self) {
        for job in self.pending.drain(..self.sent_count) {
            let position = self.positions.entry(job.printer_id).or_default();
            position.seen.insert((job.ended_at, job.job_uid));
            while position.seen.len() > self.max {
                position.seen.pop_first();
            }
        }
        self.sent_count = 0;
        self.flight = None;
    }

    pub(super) async fn fill(
        &mut self,
        store: &crate::history::HistoryHandle,
    ) -> Result<(), String> {
        if self.max == 0 || !self.pending.is_empty() {
            return Ok(());
        }
        let version = *store.closed_changed.borrow();
        for (printer, cursor) in &self.cursors {
            if !self.announced.contains(printer) {
                continue;
            }
            let position = self.positions.entry(printer.clone()).or_default();
            if let Some(finished) = position.finished_version {
                if finished == version {
                    continue;
                }
                position.offset = 0;
                position.selection = None;
                position.finished_version = None;
            }
            let since = cursor
                .since
                .map(|at| at.timestamp_millis().saturating_sub(24 * 60 * 60 * 1000));
            if position.selection.is_none() {
                position.selection =
                    Some((version, store.closed_ids(printer, since, self.max).await?));
            }
            let Some((selection_version, rows)) = &position.selection else {
                continue;
            };
            if rows.is_empty() {
                position.finished_version = Some(*selection_version);
                position.selection = None;
                continue;
            }
            let end = position.offset.saturating_add(100).min(rows.len());
            let records = store.closed(&rows[position.offset..end]).await?;
            position.offset = end;
            if end == rows.len() {
                position.finished_version = Some(*selection_version);
                position.selection = None;
            }
            for record in records {
                if let Some(job) = record.wire_job()
                    && !position.seen.contains(&(job.ended_at, job.job_uid.clone()))
                    && job.ended_at >= job.started_at
                    && job.ended_at <= chrono::Utc::now() + chrono::Duration::minutes(5)
                {
                    self.pending.push(job);
                }
            }
            if !self.pending.is_empty() {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn payload(&self, seq: u64, count: usize) -> Result<String, serde_json::Error> {
        serde_json::to_string(&JobsBatch {
            r#type: "jobs",
            v: SCHEMA_VERSION,
            seq,
            jobs: &self.pending[..count],
        })
    }

    pub(super) fn fitting_count(
        &self,
        seq: u64,
        max_bytes: usize,
    ) -> Result<usize, serde_json::Error> {
        let mut bytes = self.payload(seq, 0)?.len();
        let mut count = 0;
        for job in &self.pending {
            let next = serde_json::to_vec(job)?.len() + usize::from(count != 0);
            if bytes.saturating_add(next) > max_bytes {
                break;
            }
            bytes += next;
            count += 1;
        }
        Ok(count)
    }
}
