use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

const CHANNEL_CAPACITY: usize = 2_048;
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const BACKUP_COUNT: usize = 3;
const MAX_DETAIL_BYTES: usize = 512;
const DISPLAY_LINE_CAPACITY: usize = 200;

static SENDER: OnceLock<SyncSender<Command>> = OnceLock::new();
static CRASH_PATH: OnceLock<PathBuf> = OnceLock::new();
static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);
static DISPLAY_LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogArea {
    App,
    Config,
    Printer,
    Action,
    Integration,
    Normalization,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LogEvent {
    level: LogLevel,
    area: LogArea,
    code: &'static str,
    printer_id: Option<u64>,
    printer_name: Option<String>,
    endpoint: Option<String>,
    integration: Option<&'static str>,
    action: Option<&'static str>,
    result: Option<&'static str>,
    detail: Option<String>,
}

impl LogEvent {
    pub(crate) const fn new(level: LogLevel, area: LogArea, code: &'static str) -> Self {
        Self {
            level,
            area,
            code,
            printer_id: None,
            printer_name: None,
            endpoint: None,
            integration: None,
            action: None,
            result: None,
            detail: None,
        }
    }

    pub(crate) const fn printer(mut self, printer_id: u64) -> Self {
        self.printer_id = Some(printer_id);
        self
    }

    pub(crate) fn printer_name(mut self, name: impl AsRef<str>) -> Self {
        self.printer_name = Some(clean_detail(name.as_ref()));
        self
    }

    pub(crate) fn endpoint(mut self, endpoint: impl AsRef<str>) -> Self {
        self.endpoint = Some(clean_detail(endpoint.as_ref()));
        self
    }

    pub(crate) const fn integration(mut self, integration: &'static str) -> Self {
        self.integration = Some(integration);
        self
    }

    pub(crate) const fn action(mut self, action: &'static str) -> Self {
        self.action = Some(action);
        self
    }

    pub(crate) const fn result(mut self, result: &'static str) -> Self {
        self.result = Some(result);
        self
    }

    pub(crate) fn detail(mut self, detail: impl AsRef<str>) -> Self {
        self.detail = Some(clean_detail(detail.as_ref()));
        self
    }
}

enum Command {
    Event(LogEvent),
    Shutdown,
}

pub(crate) struct LoggerGuard {
    sender: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl LoggerGuard {
    pub(crate) fn shutdown(mut self) {
        let _ = self.sender.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for LoggerGuard {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.sender.send(Command::Shutdown);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

pub(crate) fn init_default() -> io::Result<LoggerGuard> {
    let directory = default_log_directory()?;
    fs::create_dir_all(&directory)?;
    let log_path = directory.join("makercalc.log");
    let crash_path = directory.join("crash.log");
    let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
    let worker = std::thread::Builder::new()
        .name("makercalc-diagnostics".to_owned())
        .spawn(move || run_logger(receiver, &log_path))?;

    if SENDER.set(sender.clone()).is_err() {
        let _ = sender.send(Command::Shutdown);
        let _ = worker.join();
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "el diagnóstico ya fue inicializado",
        ));
    }
    let _ = CRASH_PATH.set(crash_path);
    Ok(LoggerGuard {
        sender,
        worker: Some(worker),
    })
}

fn run_logger(receiver: mpsc::Receiver<Command>, log_path: &Path) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Event(event) => {
                let line = format_event(&event);
                // Periodic profiling must not trigger its own UI repaint via the log viewer.
                if !matches!(event.code, "app.ui_performance" | "app.ui_frame_phases") {
                    push_display_line(&line);
                }
                if let Err(error) = append_rotating(log_path, &line, MAX_LOG_BYTES) {
                    eprintln!("MakerCalc no pudo escribir el log: {error}");
                }
                write_dropped_summary(log_path);
            }
            Command::Shutdown => {
                write_dropped_summary(log_path);
                break;
            }
        }
    }
}

pub(crate) fn log(event: LogEvent) {
    if let Some(sender) = SENDER.get()
        && matches!(
            sender.try_send(Command::Event(event)),
            Err(mpsc::TrySendError::Full(_))
        )
    {
        DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn drain_display_lines(maximum: usize) -> Vec<String> {
    let mut lines = DISPLAY_LINES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let amount = maximum.min(lines.len());
    lines.drain(..amount).collect()
}

pub(crate) fn open_log_directory() -> io::Result<()> {
    let directory = default_log_directory()?;
    fs::create_dir_all(&directory)?;
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(|_| ())
}

fn push_display_line(line: &str) {
    let mut lines = DISPLAY_LINES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    push_bounded(
        &mut lines,
        line.trim_end_matches(['\r', '\n']).to_owned(),
        DISPLAY_LINE_CAPACITY,
    );
}

fn push_bounded(lines: &mut VecDeque<String>, line: String, capacity: usize) {
    if capacity == 0 {
        return;
    }
    if lines.len() >= capacity {
        lines.pop_front();
    }
    lines.push_back(line);
}

fn write_dropped_summary(log_path: &Path) {
    let dropped = DROPPED_EVENTS.swap(0, Ordering::Relaxed);
    if dropped == 0 {
        return;
    }
    let summary = LogEvent::new(LogLevel::Warn, LogArea::App, "diagnostics.events_dropped")
        .result("limited")
        .detail(format!("count={dropped}"));
    let line = format_event(&summary);
    push_display_line(&line);
    let _ = append_rotating(log_path, &line, MAX_LOG_BYTES);
}

pub(crate) fn write_crash(detail: &str) {
    let path = CRASH_PATH.get().cloned().or_else(|| {
        default_log_directory()
            .ok()
            .map(|dir| dir.join("crash.log"))
    });
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let line = format!(
        "[{}] [App/ERROR]: app.panicked detail=\"{}\"\n",
        utc_timestamp(unix_millis()),
        escape(&clean_detail(detail))
    );
    let _ = append_rotating(&path, &line, MAX_LOG_BYTES);
}

fn default_log_directory() -> io::Result<PathBuf> {
    crate::config::default_data_directory()
        .map(|root| root.join("logs"))
        .map_err(io::Error::other)
}

fn format_event(event: &LogEvent) -> String {
    format_event_at(event, unix_millis())
}

fn format_event_at(event: &LogEvent, unix_ms: u128) -> String {
    let mut line = format!(
        "[{}] [{}/{}]: {}",
        utc_timestamp(unix_ms),
        area_display_name(event.area),
        level_name(event.level),
        event.code,
    );
    if let Some(printer_id) = event.printer_id {
        line.push_str(&format!(" id={printer_id}"));
    }
    if let Some(printer_name) = &event.printer_name {
        line.push_str(" name=\"");
        line.push_str(&escape(printer_name));
        line.push('"');
    }
    if let Some(endpoint) = &event.endpoint {
        line.push_str(" endpoint=");
        line.push_str(&escape(endpoint));
    }
    if let Some(integration) = event.integration {
        line.push_str(" integration=");
        line.push_str(integration);
    }
    if let Some(action) = event.action {
        line.push_str(" action=");
        line.push_str(action);
    }
    if let Some(result) = event.result {
        line.push_str(" result=");
        line.push_str(result);
    }
    if let Some(detail) = &event.detail {
        line.push_str(" detail=\"");
        line.push_str(&escape(detail));
        line.push('"');
    }
    line.push('\n');
    line
}

fn level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    }
}

fn area_display_name(area: LogArea) -> &'static str {
    match area {
        LogArea::App => "App",
        LogArea::Config => "Config",
        LogArea::Printer => "Printer",
        LogArea::Action => "Action",
        LogArea::Integration => "Integration",
        LogArea::Normalization => "Normalization",
    }
}

fn clean_detail(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    // Errors may contain headers or JSON; omit the suspect field before truncating.
    let lower = cleaned.to_ascii_lowercase();
    if [
        "mkc_",
        "api_key",
        "api-key",
        "apikey",
        "api key",
        "checkcode",
        "check_code",
        "check-code",
        "authorization",
        "bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[REDACTED]".to_owned();
    }
    truncate_utf8(&cleaned, MAX_DETAIL_BYTES).to_owned()
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn utc_timestamp(unix_ms: u128) -> String {
    let total_seconds = (unix_ms / 1_000).min(i64::MAX as u128) as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_unix_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let milliseconds = unix_ms % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milliseconds:03}Z")
}

fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = month_position + if month_position < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn append_rotating(path: &Path, line: &str, maximum_bytes: u64) -> io::Result<()> {
    let current_bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
    if current_bytes.saturating_add(line.len() as u64) > maximum_bytes {
        rotate(path)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

fn rotate(path: &Path) -> io::Result<()> {
    let backup = |number: usize| PathBuf::from(format!("{}.{}", path.display(), number));
    let oldest = backup(BACKUP_COUNT);
    if oldest.exists() {
        fs::remove_file(oldest)?;
    }
    for number in (1..BACKUP_COUNT).rev() {
        let source = backup(number);
        if source.exists() {
            fs::rename(source, backup(number + 1))?;
        }
    }
    if path.exists() {
        fs::rename(path, backup(1))?;
    }
    Ok(())
}
