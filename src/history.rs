//! One bounded worker owns SQLite. Recording never performs disk IO on the UI or LAN runtime.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use tokio::sync::{mpsc, oneshot, watch};

use crate::shared::history::{
    HISTORY_PAGE_SIZE, HistoryColumn, HistoryQuery, HistoryRecord, HistorySource, LocalHistoryPage,
};
use crate::shared::printer::{PrintStatus, PrinterObservation};
use crate::shared::telemetry::JobTelemetry;

const CAPACITY: usize = 128;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const CHECKPOINT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryFlushError {
    Unavailable,
    WriteFailed,
}

#[derive(Clone)]
pub(crate) struct HistoryHandle {
    sender: mpsc::Sender<Command>,
    pub(crate) changed: watch::Receiver<u64>,
    pub(crate) closed_changed: watch::Receiver<u64>,
    #[cfg(test)]
    pub(crate) closed_queries: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    pub(crate) closed_decodes: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) error: watch::Receiver<Option<String>>,
    error_sender: watch::Sender<Option<String>>,
    restored: Arc<Mutex<BTreeMap<String, HistoryRecord>>>,
}

enum Command {
    GlobalPage {
        offset: u64,
        reply: oneshot::Sender<Result<LocalHistoryPage, String>>,
    },
    Save(Box<HistoryRecord>),
    Import(Vec<HistoryRecord>, oneshot::Sender<Result<(), String>>),
    LatestNativeStart {
        printer: String,
        now_ms: i64,
        reply: oneshot::Sender<Result<Option<DateTime<Utc>>, String>>,
    },
    Page {
        printer: String,
        offset: u64,
        query: HistoryQuery,
        reply: oneshot::Sender<Result<LocalHistoryPage, String>>,
    },
    ClosedIds {
        printer: String,
        since: Option<i64>,
        max: usize,
        reply: oneshot::Sender<Result<Vec<i64>, String>>,
    },
    Closed {
        rows: Vec<i64>,
        reply: oneshot::Sender<Result<Vec<HistoryRecord>, String>>,
    },
    Flush(oneshot::Sender<Result<(), HistoryFlushError>>),
}

pub(crate) struct HistoryWorker {
    handle: HistoryHandle,
}

impl HistoryWorker {
    pub(crate) fn unavailable(error: String) -> Self {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        drop(receiver);
        let (_, changed) = watch::channel(0);
        let (_, closed_changed) = watch::channel(0);
        let (error_sender, error) = watch::channel(Some(error));
        Self {
            handle: HistoryHandle {
                sender,
                changed,
                closed_changed,
                #[cfg(test)]
                closed_queries: Arc::default(),
                #[cfg(test)]
                closed_decodes: Arc::default(),
                error,
                error_sender,
                restored: Arc::new(Mutex::new(BTreeMap::new())),
            },
        }
    }
    pub(crate) fn open(path: PathBuf) -> Result<Self, String> {
        let (sender, mut receiver) = mpsc::channel(CAPACITY);
        let (changed_sender, changed) = watch::channel(0_u64);
        let (closed_sender, closed_changed) = watch::channel(0_u64);
        #[cfg(test)]
        let closed_queries = Arc::new(std::sync::atomic::AtomicU64::new(0));
        #[cfg(test)]
        let worker_queries = Arc::clone(&closed_queries);
        #[cfg(test)]
        let closed_decodes = Arc::new(std::sync::atomic::AtomicU64::new(0));
        #[cfg(test)]
        let worker_decodes = Arc::clone(&closed_decodes);
        let (error_sender, error) = watch::channel(None);
        let worker_errors = error_sender.clone();
        let restored = Arc::new(Mutex::new(BTreeMap::new()));
        let restored_worker = Arc::clone(&restored);
        let (ready_sender, ready) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("makercalc-history".to_owned())
            .spawn(move || {
                let opened = open_database(path).and_then(|connection| {
                    let mut rows = connection
                        .prepare(
                            "SELECT substr(payload,1,16385) FROM print_history WHERE ended_ms IS NULL ORDER BY started_ms ASC, last_ms ASC LIMIT 250",
                        )
                        .map_err(|_| "No se pudo recuperar el historial".to_owned())?;
                    let records = rows
                        .query_map([], |row| row.get::<_, String>(0))
                        .map_err(|_| "No se pudo recuperar el historial".to_owned())?;
                    let mut cache = restored_worker
                        .lock()
                        .map_err(|_| "Historial no disponible".to_owned())?;
                    let mut displaced = Vec::new();
                    let mut reported_invalid = false;
                    for record in records {
                        let decoded = record.map_err(|_| "No se pudo leer el historial".to_owned()).and_then(|payload| decode_record(&payload));
                        match decoded {
                            Ok(record) => {
                                if let Some(mut older) = cache.insert(record.printer_id.clone(), record) {
                                    older.job.print_state = PrintStatus::Unknown;
                                    older.incomplete = true;
                                    older.ended_at = Some(older.last_observed_at);
                                    displaced.push(older);
                                }
                            }
                            Err(error) => {
                                worker_errors.send_replace(Some(error));
                                if !reported_invalid {
                                    crate::diagnostics::log(crate::diagnostics::LogEvent::new(
                                        crate::diagnostics::LogLevel::Warn,
                                        crate::diagnostics::LogArea::App,
                                        "history.restore_invalid",
                                    ));
                                    reported_invalid = true;
                                }
                            }
                        }
                    }
                    drop(cache);
                    drop(rows);
                    for older in displaced {
                        if let Err(error) = save(&connection, &older) {
                            worker_errors.send_replace(Some(error));
                        }
                    }
                    Ok(connection)
                });
                let mut connection = match opened {
                    Ok(connection) => {
                        let _ = ready_sender.send(Ok(()));
                        connection
                    }
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                // Processing the queue does not prove all writes committed.
                let mut write_failed = false;
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Save(record) => {
                            // Queue acceptance transfers the final to this worker and its bounded retries.
                            let attempts = if record.ended_at.is_some() { 3 } else { 1 };
                            for attempt in 0..attempts {
                                match save(&connection, &record) {
                                    Ok(changed) => {
                                        if changed {
                                            changed_sender.send_modify(|version| *version = version.saturating_add(1));
                                            if record.wire_job().is_some() {
                                                closed_sender.send_modify(|version| *version = version.saturating_add(1));
                                            }
                                        }
                                        break;
                                    }
                                    Err(error) => {
                                        worker_errors.send_replace(Some(error));
                                        if attempt + 1 < attempts {
                                            std::thread::sleep(Duration::from_millis(250));
                                        } else {
                                            write_failed = true;
                                        }
                                    }
                                }
                            }
                        },
                        Command::Import(records, reply) => {
                            let result = connection
                                .transaction()
                                .map_err(|_| "No se pudo importar el historial".to_owned())
                                .and_then(|transaction| {
                                    let mut changed = false;
                                    let mut closed_changed = false;
                                    for record in records {
                                        let saved = save(&transaction, &record)?;
                                        changed |= saved;
                                        closed_changed |= saved && record.wire_job().is_some();
                                    }
                                    transaction.commit().map_err(|_| {
                                        "No se pudo confirmar el historial".to_owned()
                                    })?;
                                    Ok((changed, closed_changed))
                                });
                            match &result {
                                Ok((changed, closed_changed)) => {
                                    if *changed {
                                        changed_sender.send_modify(|version| *version = version.saturating_add(1));
                                    }
                                    if *closed_changed {
                                        closed_sender.send_modify(|version| *version = version.saturating_add(1));
                                    }
                                }
                                Err(error) => {
                                    write_failed = true;
                                    worker_errors.send_replace(Some(error.clone()));
                                }
                            }
                            let _ = reply.send(result.map(|_| ()));
                        }
                        Command::LatestNativeStart { printer, now_ms, reply } => {
                            let result = connection.query_row(
                                "SELECT max(started_ms) FROM print_history WHERE printer_id=?1 AND source='native' AND started_ms<=?2",
                                params![printer, now_ms],
                                |row| row.get::<_, Option<i64>>(0),
                            ).map_err(|_| "No se pudo consultar el historial nativo".to_owned()).and_then(|at| {
                                at.map(|at| DateTime::from_timestamp_millis(at).ok_or_else(|| "Fecha del historial nativo inválida".to_owned())).transpose()
                            });
                            let _ = reply.send(result);
                        }
                        Command::GlobalPage { offset, reply } => {
                            let _ = reply.send(query_global_page(&connection, offset));
                        }
                        Command::Page {
                            printer,
                            offset,
                            query,
                            reply,
                        } => {
                            let result = query_filtered_page(&connection, &printer, offset, &query);
                            let _ = reply.send(result);
                        }
                        Command::ClosedIds {
                            printer,
                            since,
                            max,
                            reply,
                        } => {
                            #[cfg(test)]
                            worker_queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let result = query_closed_ids(&connection, &printer, since, max);
                            let _ = reply.send(result);
                        }
                        Command::Closed { rows, reply } => {
                            #[cfg(test)]
                            worker_queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let result = query_closed(&connection, &rows);
                            #[cfg(test)]
                            if let Ok(records) = &result {
                                worker_decodes.fetch_add(records.len() as u64, std::sync::atomic::Ordering::Relaxed);
                            }
                            let _ = reply.send(result);
                        }
                        Command::Flush(reply) => {
                            let _ = reply.send(if write_failed { Err(HistoryFlushError::WriteFailed) } else { Ok(()) });
                        }
                    }
                }
            })
            .map_err(|_| "No se pudo iniciar el historial local".to_owned())?;
        ready
            .recv_timeout(Duration::from_secs(4))
            .map_err(|_| "El historial no respondió al iniciar".to_owned())??;
        Ok(Self {
            handle: HistoryHandle {
                sender,
                changed,
                closed_changed,
                #[cfg(test)]
                closed_queries,
                #[cfg(test)]
                closed_decodes,
                error,
                error_sender,
                restored,
            },
        })
    }

    pub(crate) fn handle(&self) -> HistoryHandle {
        self.handle.clone()
    }

    pub(crate) async fn flush_checked(&self) -> Result<(), HistoryFlushError> {
        let (reply, received) = oneshot::channel();
        self.handle
            .sender
            .send(Command::Flush(reply))
            .await
            .map_err(|_| HistoryFlushError::Unavailable)?;
        received.await.map_err(|_| HistoryFlushError::Unavailable)?
    }
}

impl HistoryHandle {
    pub(crate) async fn global_page(&self, offset: u64) -> Result<LocalHistoryPage, String> {
        let (reply, received) = oneshot::channel();
        self.sender
            .send(Command::GlobalPage { offset, reply })
            .await
            .map_err(|_| crate::i18n::t!("history.query_error").into_owned())?;
        received
            .await
            .map_err(|_| crate::i18n::t!("history.query_error").into_owned())?
    }

    pub(crate) async fn latest_native_start(
        &self,
        printer: &str,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let (reply, received) = oneshot::channel();
        self.sender
            .send(Command::LatestNativeStart {
                printer: printer.to_owned(),
                now_ms: Utc::now().timestamp_millis(),
                reply,
            })
            .await
            .map_err(|_| "Historial no disponible".to_owned())?;
        received
            .await
            .map_err(|_| "Historial no disponible".to_owned())?
    }
    pub(crate) async fn import_committed(&self, records: Vec<HistoryRecord>) -> bool {
        let (reply, received) = oneshot::channel();
        if records.len() > 50
            || self
                .sender
                .try_send(Command::Import(records, reply))
                .is_err()
        {
            self.error_sender.send_replace(Some(
                "La importación está pendiente: cola no disponible".into(),
            ));
            return false;
        }
        matches!(received.await, Ok(Ok(())))
    }
    /// Return queue acceptance, not a commit; revisions advance only after SQLite succeeds.
    pub(crate) fn save(&self, record: HistoryRecord) -> bool {
        if self
            .sender
            .try_send(Command::Save(Box::new(record)))
            .is_err()
        {
            // This message contains neither names nor source payloads.
            if self.error.borrow().is_none() {
                crate::diagnostics::log(crate::diagnostics::LogEvent::new(
                    crate::diagnostics::LogLevel::Error,
                    crate::diagnostics::LogArea::App,
                    "history.queue_unavailable",
                ));
            }
            self.error_sender.send_replace(Some(
                "No se guardó el historial: cola no disponible".to_owned(),
            ));
            return false;
        }
        true
    }

    pub(crate) async fn filtered_page(
        &self,
        printer: &str,
        offset: u64,
        query: HistoryQuery,
    ) -> Result<LocalHistoryPage, String> {
        let (reply, received) = oneshot::channel();
        self.sender
            .send(Command::Page {
                printer: printer.to_owned(),
                offset,
                query,
                reply,
            })
            .await
            .map_err(|_| crate::i18n::t!("history.query_error").into_owned())?;
        received
            .await
            .map_err(|_| crate::i18n::t!("history.query_error").into_owned())?
    }

    pub(crate) async fn closed_ids(
        &self,
        printer: &str,
        since: Option<i64>,
        max: usize,
    ) -> Result<Vec<i64>, String> {
        let (reply, received) = oneshot::channel();
        self.sender
            .send(Command::ClosedIds {
                printer: printer.to_owned(),
                since,
                max: max.min(10_000),
                reply,
            })
            .await
            .map_err(|_| "Historial no disponible".to_owned())?;
        received
            .await
            .map_err(|_| "Historial no disponible".to_owned())?
    }

    pub(crate) async fn closed(&self, rows: &[i64]) -> Result<Vec<HistoryRecord>, String> {
        let (reply, received) = oneshot::channel();
        self.sender
            .send(Command::Closed {
                rows: rows.iter().take(100).copied().collect(),
                reply,
            })
            .await
            .map_err(|_| "Historial no disponible".to_owned())?;
        received
            .await
            .map_err(|_| "Historial no disponible".to_owned())?
    }

    pub(crate) fn capture(&self, printer: String) -> Capture {
        let restored = self
            .restored
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&printer);
        Capture(Arc::new(Mutex::new(Tracker {
            handle: self.clone(),
            printer,
            current: restored,
            restored: true,
            last_saved: None,
            dirty: false,
        })))
    }
}

#[derive(Clone)]
pub(crate) struct Capture(Arc<Mutex<Tracker>>);

#[derive(Clone)]
pub(crate) struct NativeHistory {
    pub(crate) handle: HistoryHandle,
    pub(crate) printer_id: String,
}

impl std::fmt::Debug for NativeHistory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeHistory")
    }
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HistoryCapture")
    }
}

impl Capture {
    pub(crate) fn close_unknown(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .close_unknown(Instant::now());
    }
    pub(crate) fn disconnected(&self) {
        let mut tracker = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tracker.restored = true;
        tracker.persist(Instant::now());
    }
    pub(crate) fn observe(&self, observation: &PrinterObservation, now: Instant) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(observation, Utc::now(), now);
    }

    pub(crate) async fn checkpoint_checked(&self) -> Result<(), HistoryFlushError> {
        let handle = {
            let tracker = self.0.lock().map_err(|_| HistoryFlushError::Unavailable)?;
            if !tracker.dirty {
                return Ok(());
            }
            tracker.handle.clone()
        };
        let permit = handle
            .sender
            .reserve()
            .await
            .map_err(|_| HistoryFlushError::Unavailable)?;
        let mut tracker = self.0.lock().map_err(|_| HistoryFlushError::Unavailable)?;
        if tracker.dirty {
            let record = tracker
                .current
                .as_ref()
                .ok_or(HistoryFlushError::Unavailable)?;
            permit.send(Command::Save(Box::new(record.clone())));
            tracker.dirty = false;
            tracker.last_saved = Some(Instant::now());
        }
        Ok(())
    }
}

struct Tracker {
    handle: HistoryHandle,
    printer: String,
    current: Option<HistoryRecord>,
    restored: bool,
    last_saved: Option<Instant>,
    dirty: bool,
}

impl Tracker {
    fn close_unknown(&mut self, now: Instant) {
        if let Some(record) = self.current.as_mut()
            && record.ended_at.is_none()
        {
            record.job.print_state = PrintStatus::Unknown;
            record.ended_at = Some(record.last_observed_at);
            record.incomplete = true;
            self.dirty = true;
        }
        self.persist(now);
        if !self.dirty {
            self.current = None;
        }
    }
    fn observe(&mut self, observation: &PrinterObservation, at: DateTime<Utc>, now: Instant) {
        if self
            .current
            .as_ref()
            .is_some_and(|record| record.ended_at.is_some())
        {
            self.persist(now);
            if self.dirty {
                return;
            }
            self.current = None;
        }
        let status = observation.status.unwrap_or(PrintStatus::Unknown);
        let active = matches!(status, PrintStatus::Printing | PrintStatus::Paused);
        if self.restored {
            self.restored = false;
            // Without an execution ID, filename equality after restart is not continuity evidence.
            self.close_unknown(now);
            if self.dirty {
                return;
            }
        }
        if active && self.current.is_none() {
            let Ok(uid) = crate::config::generate_installation_id() else {
                self.handle
                    .error_sender
                    .send_replace(Some("No se pudo crear la identidad del historial".into()));
                return;
            };
            let elapsed_ms =
                (observation.elapsed_s.unwrap_or(0.0) * 1000.0).min(i64::MAX as f64) as i64;
            let Some(started_at) =
                at.checked_sub_signed(chrono::Duration::milliseconds(elapsed_ms))
            else {
                self.handle.error_sender.send_replace(Some(
                    "La fecha de inicio del historial está fuera de rango".into(),
                ));
                return;
            };
            self.current = Some(HistoryRecord {
                printer_id: self.printer.clone(),
                job_uid: uid,
                source: HistorySource::Observed,
                started_at,
                first_observed_at: at,
                last_observed_at: at,
                ended_at: None,
                incomplete: observation.elapsed_s.is_none_or(|elapsed| elapsed > 5.0),
                job: JobTelemetry::default(),
                filament_supplies: None,
            });
            self.last_saved = None;
        }
        let Some(record) = self.current.as_mut() else {
            return;
        };
        let previous = record.job.clone();
        let supplies_changed = observation.filament_supplies.is_some()
            && observation.filament_supplies != record.filament_supplies;
        let terminal = matches!(
            status,
            PrintStatus::Completed | PrintStatus::Cancelled | PrintStatus::Error
        );
        if active || terminal {
            record.job =
                crate::shared::telemetry::merge_job_observation(&previous, observation, status);
            if let Some(supplies) = &observation.filament_supplies {
                record.filament_supplies = Some(supplies.clone());
            }
        } else if status == PrintStatus::Idle {
            record.job.print_state = PrintStatus::Unknown;
            record.incomplete = true;
        } else {
            return;
        }
        record.last_observed_at = at;
        self.dirty |= previous != record.job || supplies_changed;
        let changed_state = previous.print_state != record.job.print_state;
        if terminal || status == PrintStatus::Idle {
            record.ended_at = Some(at);
            self.dirty = true;
        }
        if changed_state
            || self
                .last_saved
                .is_none_or(|last| now.saturating_duration_since(last) >= CHECKPOINT)
        {
            self.persist(now);
        }
        if (terminal || status == PrintStatus::Idle) && !self.dirty {
            self.current = None;
        }
    }

    fn persist(&mut self, now: Instant) {
        if self.dirty
            && let Some(record) = &mut self.current
            && self.handle.save(record.clone())
        {
            self.dirty = false;
            self.last_saved = Some(now);
        }
    }
}

fn open_database(path: PathBuf) -> Result<Connection, String> {
    let connection =
        Connection::open(path).map_err(|_| "No se pudo abrir el historial local".to_owned())?;
    connection
        .busy_timeout(Duration::from_secs(1))
        .map_err(|_| "No se pudo configurar el historial".to_owned())?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| "Historial no disponible".to_owned())?;
    if version > 1 {
        return Err("El historial pertenece a una versión más reciente".to_owned());
    }
    connection.execute_batch("PRAGMA synchronous=EXTRA; PRAGMA cache_size=-2048;
        CREATE TABLE IF NOT EXISTS print_history (
            printer_id TEXT NOT NULL, job_uid TEXT NOT NULL, started_ms INTEGER NOT NULL,
            ended_ms INTEGER, last_ms INTEGER NOT NULL, source TEXT NOT NULL, sendable INTEGER NOT NULL,
            payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 16384),
            PRIMARY KEY(printer_id, job_uid));
        CREATE INDEX IF NOT EXISTS history_latest ON print_history(printer_id, ended_ms DESC, job_uid);
        CREATE INDEX IF NOT EXISTS history_upload ON print_history(printer_id, sendable, ended_ms DESC, job_uid);
        CREATE INDEX IF NOT EXISTS history_local ON print_history(printer_id, started_ms DESC, job_uid);
        CREATE INDEX IF NOT EXISTS history_global ON print_history(ended_ms DESC, started_ms DESC, printer_id, job_uid) WHERE ended_ms IS NOT NULL;
        PRAGMA user_version=1;").map_err(|_| "No se pudo preparar el historial local".to_owned())?;
    Ok(connection)
}

fn decode_record(payload: &str) -> Result<HistoryRecord, String> {
    if payload.len() > MAX_RECORD_BYTES {
        return Err("Registro de historial demasiado grande".to_owned());
    }
    let record: HistoryRecord =
        serde_json::from_str(payload).map_err(|_| "Registro de historial inválido".to_owned())?;
    validate_record(&record)?;
    Ok(record)
}

fn validate_record(record: &HistoryRecord) -> Result<(), String> {
    record
        .job
        .validate()
        .map_err(|_| "Datos de historial inválidos".to_owned())?;
    if record.printer_id.is_empty()
        || record.printer_id.len() > 128
        || !record
            .printer_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        || record.job_uid.is_empty()
        || record.job_uid.len() > 128
        || record.ended_at.is_some_and(|end| end < record.started_at)
    {
        return Err("Identidad o fechas de historial inválidas".to_owned());
    }
    if record.job.print_name.as_ref().is_some_and(|name| {
        name.len() > 1024 || name.contains(['/', '\\']) || name.chars().any(char::is_control)
    }) {
        return Err("Nombre de historial sin normalizar".into());
    }
    Ok(())
}

fn save(connection: &Connection, record: &HistoryRecord) -> Result<bool, String> {
    validate_record(record)?;
    let payload =
        serde_json::to_string(record).map_err(|_| "No se pudo preparar el historial".to_owned())?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err("Registro de historial demasiado grande".to_owned());
    }
    connection.execute("INSERT INTO print_history(printer_id,job_uid,started_ms,ended_ms,last_ms,source,payload,sendable)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(printer_id,job_uid) DO UPDATE SET
        ended_ms=excluded.ended_ms,last_ms=excluded.last_ms,payload=excluded.payload,sendable=excluded.sendable
        WHERE print_history.ended_ms IS NULL AND print_history.last_ms <= excluded.last_ms",
        params![record.printer_id, record.job_uid, record.started_at.timestamp_millis(), record.ended_at.map(|at| at.timestamp_millis()), record.last_observed_at.timestamp_millis(), if record.source == HistorySource::Native { "native" } else { "observed" }, payload, record.wire_job().is_some()])
        .map(|count| count != 0).map_err(|_| "No se pudo guardar el historial; comprueba el disco".to_owned())
}

fn query_filtered_page(
    connection: &Connection,
    printer: &str,
    offset: u64,
    query: &HistoryQuery,
) -> Result<LocalHistoryPage, String> {
    use crate::i18n::t;
    if query.name.len() > 1024 {
        return Err(t!("history.filters.name_too_long").into_owned());
    }
    if query
        .from
        .zip(query.until)
        .is_some_and(|(from, until)| from > until)
    {
        return Err(t!("history.filters.invalid_dates").into_owned());
    }
    let midnight = |date: chrono::NaiveDate| {
        date.and_time(chrono::NaiveTime::MIN)
            .and_utc()
            .timestamp_millis()
    };
    let from = query.from.map(midnight);
    let until = query
        .until
        .map(|date| {
            date.succ_opt()
                .map(midnight)
                .ok_or_else(|| t!("history.filters.invalid_dates").into_owned())
        })
        .transpose()?;
    let state = query.state.map(|state| match state {
        PrintStatus::Unknown => "unknown",
        PrintStatus::Idle => "idle",
        PrintStatus::Printing => "printing",
        PrintStatus::Paused => "paused",
        PrintStatus::Completed => "completed",
        PrintStatus::Cancelled => "cancelled",
        PrintStatus::Error => "error",
    });
    let predicate = "printer_id=?1
        AND (?2='' OR instr(lower(COALESCE(json_extract(payload,'$.job.PrintName'),'')),lower(?2))>0)
        AND (?3 IS NULL OR json_extract(payload,'$.job.PrintState')=?3)
        AND (?4 IS NULL OR started_ms>=?4) AND (?5 IS NULL OR started_ms<?5)";
    let total: i64 = connection
        .query_row(
            &format!("SELECT count(*) FROM print_history WHERE {predicate}"),
            params![printer, query.name.trim(), state, from, until],
            |row| row.get(0),
        )
        .map_err(|_| t!("history.query_error").into_owned())?;
    let total = total as u64;
    let offset = offset.min(total.saturating_sub(1) / HISTORY_PAGE_SIZE * HISTORY_PAGE_SIZE);
    // Only enum-selected SQL fragments are interpolated; all user values remain parameters.
    let column = match query.column {
        HistoryColumn::Name => "json_extract(payload,'$.job.PrintName') COLLATE NOCASE",
        HistoryColumn::State => "json_extract(payload,'$.job.PrintState')",
        HistoryColumn::Started => "started_ms",
        HistoryColumn::PrintTime => "json_extract(payload,'$.job.PrintElapsedTimeS')",
        HistoryColumn::Grams => "json_extract(payload,'$.job.PrintFilamentUsedG')",
        HistoryColumn::Length => "json_extract(payload,'$.job.PrintFilamentUsedMm')",
    };
    let direction = if query.descending { "DESC" } else { "ASC" };
    let mut statement = connection
        .prepare(&format!(
            "SELECT substr(payload,1,16385) FROM print_history WHERE {predicate}
         ORDER BY {column} {direction} NULLS LAST,started_ms DESC,job_uid LIMIT ?6 OFFSET ?7"
        ))
        .map_err(|_| t!("history.query_error").into_owned())?;
    let records = statement
        .query_map(
            params![
                printer,
                query.name.trim(),
                state,
                from,
                until,
                HISTORY_PAGE_SIZE as i64,
                offset as i64
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| t!("history.query_error").into_owned())?
        .map(|row| decode_record(&row.map_err(|_| t!("history.query_error").into_owned())?))
        .collect::<Result<_, _>>()?;
    Ok(LocalHistoryPage {
        total,
        offset,
        records,
    })
}

fn query_global_page(connection: &Connection, offset: u64) -> Result<LocalHistoryPage, String> {
    let error = || crate::i18n::t!("history.query_error").into_owned();
    let total: i64 = connection
        .query_row(
            "SELECT count(*) FROM print_history WHERE ended_ms IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|_| error())?;
    let total = total as u64;
    let offset = offset.min(total.saturating_sub(1) / HISTORY_PAGE_SIZE * HISTORY_PAGE_SIZE);
    let mut statement = connection
        .prepare(
            "SELECT substr(payload,1,16385) FROM print_history WHERE ended_ms IS NOT NULL
         ORDER BY ended_ms DESC,started_ms DESC,printer_id,job_uid LIMIT ?1 OFFSET ?2",
        )
        .map_err(|_| error())?;
    let records = statement
        .query_map(params![HISTORY_PAGE_SIZE as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| error())?
        .map(|row| decode_record(&row.map_err(|_| error())?))
        .collect::<Result<_, _>>()?;
    Ok(LocalHistoryPage {
        total,
        offset,
        records,
    })
}

fn query_closed_ids(
    connection: &Connection,
    printer: &str,
    since: Option<i64>,
    max: usize,
) -> Result<Vec<i64>, String> {
    let mut statement = connection
        .prepare(
            "SELECT rowid FROM (
        SELECT rowid,ended_ms,job_uid FROM print_history WHERE printer_id=?1 AND ended_ms IS NOT NULL
        AND (?2 IS NULL OR ended_ms > ?2) AND sendable=1
        ORDER BY ended_ms DESC,job_uid LIMIT ?3)
        ORDER BY ended_ms ASC,job_uid",
        )
        .map_err(|_| "No se pudo consultar el historial".to_owned())?;
    let rows = statement
        .query_map(params![printer, since, max as i64], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|_| "No se pudo consultar el historial".to_owned())?;
    rows.map(|row| row.map_err(|_| "No se pudo leer el historial".to_owned()))
        .collect()
}

fn query_closed(connection: &Connection, rows: &[i64]) -> Result<Vec<HistoryRecord>, String> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; rows.len()].join(",");
    let mut statement = connection.prepare(&format!("SELECT substr(payload,1,16385) FROM print_history WHERE rowid IN ({placeholders}) ORDER BY ended_ms ASC,job_uid")).map_err(|_| "No se pudo consultar el historial".to_owned())?;
    let records = statement
        .query_map(rusqlite::params_from_iter(rows), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| "No se pudo consultar el historial".to_owned())?;
    records
        .map(|row| decode_record(&row.map_err(|_| "No se pudo leer el historial".to_owned())?))
        .collect()
}
