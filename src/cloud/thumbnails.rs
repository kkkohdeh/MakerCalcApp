use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::task::JoinSet;

use super::{CloudPrinterReceiver, CloudRegistry, SCHEMA_VERSION};
use crate::shared::thumbnail::{
    ThumbnailError, ThumbnailResourceKey, run_blocking_thumbnail_cpu, thumbnail_for_cloud,
};
use crate::state::{LocalPrinterId, PrinterState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ThumbnailKey {
    pub(super) resource: ThumbnailResourceKey,
    pub(super) cloud_printer_id: String,
}

impl ThumbnailKey {
    pub(super) fn current(printer: &CloudPrinterReceiver, state: &PrinterState) -> Option<Self> {
        Self::for_job(printer, state, state.telemetry.current_job.as_ref()?)
    }

    /// A finished job keeps its key; generation advances only when the next print starts.
    pub(super) fn finished(printer: &CloudPrinterReceiver, state: &PrinterState) -> Option<Self> {
        if state.telemetry.current_job.is_some() {
            return None;
        }
        Self::for_job(printer, state, state.telemetry.last_job.as_ref()?)
    }

    fn for_job(
        printer: &CloudPrinterReceiver,
        state: &PrinterState,
        job: &crate::shared::telemetry::JobTelemetry,
    ) -> Option<Self> {
        let name = job.print_name.as_ref()?;
        if name.trim().is_empty() {
            return None;
        }
        Some(Self {
            resource: ThumbnailResourceKey {
                printer_id: printer.id,
                session_generation: printer.generation,
                connection_epoch: state.connection_epoch,
                job_generation: state.job_generation,
                print_name: name.clone(),
            },
            cloud_printer_id: printer.cloud_printer_id.clone(),
        })
    }
}

pub(super) enum ThumbnailState {
    Pending,
    Fetching,
    Prepared(crate::shared::thumbnail::CloudThumbnail),
    Accepted(String),
    Skipped,
}

pub(super) struct ThumbnailEntry {
    pub(super) key: ThumbnailKey,
    pub(super) state: ThumbnailState,
    pub(super) retry_at: Instant,
    pub(super) attempts: u8,
}

pub(super) struct ThumbnailFlight {
    pub(super) key: ThumbnailKey,
    pub(super) sha256: String,
    pub(super) seq: u64,
    pub(super) sent_at: Instant,
}

pub(super) struct ThumbnailSession {
    pub(super) max_bytes: Option<usize>,
    pub(super) entries: BTreeMap<LocalPrinterId, ThumbnailEntry>,
    pub(super) fetches: JoinSet<(
        ThumbnailKey,
        Result<Option<crate::shared::thumbnail::CloudThumbnail>, ThumbnailError>,
    )>,
    pub(super) flight: Option<ThumbnailFlight>,
    pub(super) rejected_hashes: BTreeSet<String>,
}

impl ThumbnailSession {
    pub(super) fn new(max_bytes: Option<usize>) -> Self {
        Self {
            max_bytes: max_bytes
                .filter(|limit| *limit > 0)
                .map(|limit| limit.min(40960)),
            entries: BTreeMap::new(),
            fetches: JoinSet::new(),
            flight: None,
            rejected_hashes: BTreeSet::new(),
        }
    }

    pub(super) fn current_hash<'a>(
        &'a self,
        printer: &CloudPrinterReceiver,
        state: &PrinterState,
    ) -> Option<&'a str> {
        let entry = self.entries.get(&printer.id)?;
        let key = &entry.key.resource;
        if key.session_generation != printer.generation
            || key.connection_epoch != state.connection_epoch
            || key.job_generation != state.job_generation
            || entry.key.cloud_printer_id != printer.cloud_printer_id
            || state
                .telemetry
                .current_job
                .as_ref()
                .or(state.telemetry.last_job.as_ref())?
                .print_name
                .as_deref()
                != Some(key.print_name.as_str())
        {
            return None;
        }
        match &entry.state {
            ThumbnailState::Accepted(hash) => Some(hash),
            _ => None,
        }
    }

    pub(super) fn observe(
        &mut self,
        registry: &CloudRegistry,
        announced: &BTreeSet<String>,
        now: Instant,
    ) {
        let Some(max_bytes) = self.max_bytes else {
            return;
        };
        let mut finished = BTreeMap::new();
        let active = registry
            .printers
            .iter()
            .take(250)
            .filter_map(|printer| {
                let state = printer.receiver.borrow();
                if let Some(key) = ThumbnailKey::finished(printer, &state) {
                    finished.insert(printer.id, key);
                }
                ThumbnailKey::current(printer, &state).map(|key| {
                    (
                        printer.id,
                        (key, printer.thumbnail.clone(), state.telemetry_ready),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        // Keep an accepted thumbnail on the finished job until the next print.
        // Never fetch it again.
        self.entries.retain(|id, entry| match active.get(id) {
            Some((key, _, _)) => entry.key == *key,
            None => {
                matches!(entry.state, ThumbnailState::Accepted(_))
                    && finished.get(id) == Some(&entry.key)
            }
        });
        for (id, (key, source, fresh)) in active {
            let entry = self.entries.entry(id).or_insert_with(|| ThumbnailEntry {
                key: key.clone(),
                state: ThumbnailState::Pending,
                retry_at: now,
                attempts: 0,
            });
            // Image work never runs on the WebSocket loop and cannot fan out with printer count.
            if self.fetches.len() < 2
                && fresh
                && announced.contains(&key.cloud_printer_id)
                && matches!(entry.state, ThumbnailState::Pending)
                && now >= entry.retry_at
            {
                if let Some(source) = source {
                    entry.state = ThumbnailState::Fetching;
                    entry.attempts += 1;
                    let resource_key = key.resource.clone();
                    self.fetches.spawn(async move {
                        let normalized = source.request_active(resource_key.clone()).await;
                        let image = match normalized {
                            Ok(thumb) if thumb.key == resource_key => {
                                run_blocking_thumbnail_cpu(move || {
                                    thumbnail_for_cloud(&thumb.png_bytes, max_bytes)
                                })
                                .await
                            }
                            Ok(_) => Err(ThumbnailError::JobChanged),
                            Err(error) => Err(error),
                        };
                        (key, image)
                    });
                } else {
                    entry.state = ThumbnailState::Skipped;
                }
            }
        }
    }

    pub(super) fn completed(
        &mut self,
        key: ThumbnailKey,
        image: Result<Option<crate::shared::thumbnail::CloudThumbnail>, ThumbnailError>,
        now: Instant,
    ) {
        let Some(entry) = self
            .entries
            .get_mut(&key.resource.printer_id)
            .filter(|entry| entry.key == key)
        else {
            return;
        };
        entry.state = match image {
            Ok(Some(image)) if !self.rejected_hashes.contains(&image.sha256) => {
                ThumbnailState::Prepared(image)
            }
            Err(ThumbnailError::Unavailable | ThumbnailError::QueueFull) if entry.attempts < 3 => {
                entry.retry_at = now + Duration::from_secs(5);
                ThumbnailState::Pending
            }
            _ => ThumbnailState::Skipped,
        };
    }

    pub(super) fn prepare(
        &mut self,
        seq: u64,
        max_message_bytes: usize,
        now: Instant,
    ) -> Option<String> {
        if self.max_bytes.is_none() || self.flight.is_some() {
            return None;
        }
        for entry in self.entries.values_mut() {
            let ThumbnailState::Prepared(image) = &entry.state else {
                continue;
            };
            if self.rejected_hashes.contains(&image.sha256) {
                entry.state = ThumbnailState::Skipped;
                continue;
            }
            if now < entry.retry_at {
                continue;
            }
            let payload = serde_json::to_string(&ThumbnailMessage {
                r#type: "thumbnail",
                v: SCHEMA_VERSION,
                seq,
                printer_id: &entry.key.cloud_printer_id,
                sha256: &image.sha256,
                webp_base64: &image.webp_base64,
            })
            .ok()?;
            if payload.len() > max_message_bytes {
                entry.state = ThumbnailState::Skipped;
                continue;
            }
            self.flight = Some(ThumbnailFlight {
                key: entry.key.clone(),
                sha256: image.sha256.clone(),
                seq,
                sent_at: now,
            });
            return Some(payload);
        }
        None
    }

    pub(super) fn acknowledge(&mut self, seq: u64) -> bool {
        let Some(flight) = self.flight.take() else {
            return false;
        };
        if flight.seq != seq {
            self.flight = Some(flight);
            return false;
        }
        if let Some(entry) = self
            .entries
            .get_mut(&flight.key.resource.printer_id)
            .filter(|entry| entry.key == flight.key)
            && let ThumbnailState::Prepared(image) = &entry.state
        {
            entry.state = ThumbnailState::Accepted(image.sha256.clone());
        }
        true
    }

    pub(super) fn reject(
        &mut self,
        seq: u64,
        code: &str,
        retry_after_ms: Option<u64>,
        now: Instant,
    ) -> bool {
        let Some(flight) = self.flight.take() else {
            return false;
        };
        if flight.seq != seq {
            self.flight = Some(flight);
            return false;
        }
        if !matches!(code, "rate_limited" | "unavailable") {
            self.rejected_hashes.insert(flight.sha256);
            // Bound even a long-lived session with repeated malformed images; live is unaffected.
            if self.rejected_hashes.len() >= 4096 {
                self.max_bytes = None;
            }
        }
        let Some(entry) = self
            .entries
            .get_mut(&flight.key.resource.printer_id)
            .filter(|entry| entry.key == flight.key)
        else {
            return true;
        };
        if matches!(code, "rate_limited" | "unavailable") {
            entry.retry_at = now
                .checked_add(Duration::from_millis(retry_after_ms.unwrap_or(5000)))
                .unwrap_or(now + Duration::from_secs(365 * 24 * 60 * 60));
        } else {
            entry.state = ThumbnailState::Skipped;
        }
        true
    }
}

#[derive(Serialize)]
pub(super) struct ThumbnailMessage<'a> {
    pub(super) r#type: &'static str,
    pub(super) v: u8,
    pub(super) seq: u64,
    pub(super) printer_id: &'a str,
    pub(super) sha256: &'a str,
    pub(super) webp_base64: &'a str,
}
