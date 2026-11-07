//! Lightweight, replaceable file reads. Never acquires or decodes thumbnails.
use std::collections::BTreeMap;
use std::time::Duration;

use tokio::time::Instant;

use crate::shared::workspace::{DetailState, FileReadInterest, PrinterFile, ResourceId};

pub(super) const DEBOUNCE: Duration = Duration::from_millis(300);
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
static PARSER_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FileKey {
    generation: u64,
    pub(super) id: ResourceId,
    size: Option<u64>,
    modified: Option<u64>,
}

impl FileKey {
    pub(super) fn new(generation: u64, file: &PrinterFile) -> Self {
        Self {
            generation,
            id: file.id,
            size: file.size_bytes,
            modified: file.modified_at_unix_s.map(f64::to_bits),
        }
    }
    pub(super) fn current(state: &DetailState, id: ResourceId) -> Option<Self> {
        state
            .files
            .value
            .as_ref()?
            .iter()
            .find(|file| file.id == id)
            .map(|file| Self::new(state.generation, file))
    }
}

enum Outcome {
    Known(Option<f64>),
    Failed {
        retry_at: Instant,
        final_attempt: bool,
    },
}

pub(super) struct Estimates {
    interest: FileReadInterest,
    changed_at: Instant,
    paused_until: Instant,
    cached: BTreeMap<ResourceId, (FileKey, Outcome)>,
}

impl Estimates {
    pub(super) fn new() -> Self {
        let now = Instant::now();
        Self {
            interest: Default::default(),
            changed_at: now,
            paused_until: now,
            cached: Default::default(),
        }
    }
    pub(super) fn replace(&mut self, mut interest: FileReadInterest) {
        let mut seen = std::collections::BTreeSet::new();
        interest.ids.retain(|id| seen.insert(*id));
        interest
            .ids
            .truncate(crate::shared::workspace::FILE_READ_WINDOW);
        if self.interest != interest {
            if self.interest.generation != interest.generation
                || self.interest.view_revision != interest.view_revision
                || self.interest.ids.is_empty()
            {
                self.changed_at = Instant::now();
            }
            self.interest = interest;
        }
    }

    fn eligible_at(&self, state: &DetailState, id: ResourceId) -> Option<(FileKey, Instant)> {
        if !state.live || self.interest.generation != state.generation {
            return None;
        }
        let file = state
            .files
            .value
            .as_ref()?
            .iter()
            .find(|file| file.id == id)?;
        if matches!(
            state.files.status,
            crate::shared::workspace::ReadStatus::Loading
                | crate::shared::workspace::ReadStatus::Denied
                | crate::shared::workspace::ReadStatus::Error
        ) {
            return None;
        }
        if file.estimated_duration_s.is_some()
            || state.file_details.get(&id).is_some_and(|details| {
                details.status == crate::shared::workspace::ReadStatus::Loading
            })
        {
            return None;
        }
        let key = FileKey::new(state.generation, file);
        let deadline = self.paused_until.max(self.changed_at + DEBOUNCE);
        if let Some((_, outcome)) = self
            .cached
            .get(&id)
            .filter(|(cached_key, _)| *cached_key == key)
        {
            match outcome {
                Outcome::Known(_)
                | Outcome::Failed {
                    final_attempt: true,
                    ..
                } => return None,
                Outcome::Failed { retry_at, .. } => return Some((key, deadline.max(*retry_at))),
            }
        }
        Some((key, deadline))
    }

    pub(super) fn deadline(&self, state: &DetailState) -> Option<Instant> {
        self.interest
            .ids
            .iter()
            .filter_map(|id| self.eligible_at(state, *id).map(|(_, at)| at))
            .min()
    }

    pub(super) fn next(&self, state: &DetailState) -> Option<FileKey> {
        self.interest.ids.iter().find_map(|id| {
            self.eligible_at(state, *id)
                .filter(|(_, at)| *at <= Instant::now())
                .map(|(key, _)| key)
        })
    }

    pub(super) fn retry_failed(&mut self) {
        self.cached
            .retain(|_, (_, outcome)| matches!(outcome, Outcome::Known(_)));
    }

    pub(super) fn finish(
        &mut self,
        state: &mut DetailState,
        key: FileKey,
        value: Option<f64>,
    ) -> bool {
        if FileKey::current(state, key.id).as_ref() != Some(&key)
            || value.is_some_and(|value| !value.is_finite() || value < 0.)
        {
            return false;
        }
        let Some(file) = state
            .files
            .value
            .as_mut()
            .and_then(|files| files.iter_mut().find(|file| file.id == key.id))
        else {
            return false;
        };
        let value = file.estimated_duration_s.or(value);
        file.estimated_duration_s = value;
        file.duration_read_status = crate::shared::workspace::ReadStatus::Ready;
        if let Some(details) = state
            .file_details
            .get_mut(&key.id)
            .and_then(|result| result.value.as_mut())
        {
            details.analysis.print.estimated_seconds =
                details.analysis.print.estimated_seconds.or(value);
        }
        self.cached.insert(key.id, (key, Outcome::Known(value)));
        if state.file_estimate_error.is_some()
            && !self
                .cached
                .values()
                .any(|(_, outcome)| matches!(outcome, Outcome::Failed { .. }))
        {
            state.file_estimate_error = None;
        }
        true
    }

    pub(super) fn fail(&mut self, state: &mut DetailState, key: FileKey) -> bool {
        if FileKey::current(state, key.id).as_ref() != Some(&key)
            || state.files.value.as_ref().is_some_and(|files| {
                files.iter().any(|file| {
                    file.id == key.id
                        && (file.estimated_duration_s.is_some()
                            || file.duration_read_status
                                == crate::shared::workspace::ReadStatus::Ready)
                })
            })
        {
            return false;
        }
        let final_attempt = self
            .cached
            .get(&key.id)
            .is_some_and(|(cached_key, outcome)| {
                *cached_key == key && matches!(outcome, Outcome::Failed { .. })
            });
        self.paused_until = Instant::now() + RETRY_INTERVAL;
        if let Some(file) = state
            .files
            .value
            .as_mut()
            .and_then(|files| files.iter_mut().find(|file| file.id == key.id))
        {
            file.duration_read_status = crate::shared::workspace::ReadStatus::Error;
        }
        self.cached.insert(
            key.id,
            (
                key,
                Outcome::Failed {
                    retry_at: self.paused_until,
                    final_attempt,
                },
            ),
        );
        state.file_estimate_error =
            Some(crate::i18n::t!("files.table.duration_read_failed").into_owned());
        true
    }

    pub(super) fn sync_catalog(&mut self, generation: u64, files: &mut [PrinterFile]) {
        let keys: BTreeMap<_, _> = files
            .iter()
            .map(|file| (file.id, FileKey::new(generation, file)))
            .collect();
        self.cached.retain(|id, (key, _)| keys.get(id) == Some(key));
        for file in files {
            if let Some((_, Outcome::Known(value))) = self.cached.get(&file.id) {
                file.estimated_duration_s = file.estimated_duration_s.or(*value);
                file.duration_read_status = crate::shared::workspace::ReadStatus::Ready;
            }
        }
    }
}

pub(super) async fn wait_until(deadline: Option<Instant>) {
    if let Some(at) = deadline {
        tokio::time::sleep_until(at).await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(super) async fn read(
    client: reqwest::Client,
    base: String,
    path: String,
    secrets: Vec<String>,
    key: &FileKey,
) -> Result<Option<f64>, ()> {
    tokio::time::timeout(Duration::from_secs(4), async {
        use super::super::detail::{FILE_PREVIEW_BYTES, fetch_file_preview};
        let secrets: Vec<_> = secrets.iter().map(String::as_str).collect();
        let head = fetch_file_preview(&client, &base, "gcodes", &path, 0, &secrets)
            .await
            .map_err(|_| ())?;
        if key
            .size
            .zip(head.total_bytes)
            .is_some_and(|(expected, actual)| expected != actual)
        {
            return Err(());
        }
        let Some(text) = head.text else {
            return Ok(None);
        };
        let seconds = parse(text, None).await?;
        if seconds.is_some() {
            return Ok(seconds);
        }
        let tail_offset = head
            .total_bytes
            .unwrap_or(0)
            .saturating_sub(FILE_PREVIEW_BYTES as u64);
        if tail_offset == 0 {
            return Ok(None);
        }
        let tail = fetch_file_preview(&client, &base, "gcodes", &path, tail_offset, &secrets)
            .await
            .map_err(|_| ())?;
        if head.total_bytes != tail.total_bytes {
            return Err(());
        }
        match tail.text {
            Some(text) => parse(String::new(), Some(text)).await,
            None => Ok(None),
        }
    })
    .await
    .map_err(|_| ())?
}

async fn parse(head: String, tail: Option<String>) -> Result<Option<f64>, ()> {
    let slot = PARSER_SLOTS.acquire().await.map_err(|_| ())?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        crate::shared::gcode::inspect_gcode_file_parts(&head, tail.as_deref())
            .print
            .estimated_seconds
    })
    .await
    .map_err(|_| ())
}
