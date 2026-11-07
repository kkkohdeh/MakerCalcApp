//! Process-local printer events, produced before latest-state watch channels can coalesce.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::{mpsc, watch};

use super::printer::{PrintStatus, PrinterObservation};
use crate::state::LocalPrinterId;

pub(crate) const QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoticeKind {
    Completed,
    PrintError,
    Offline,
    Online,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PrinterNotice {
    pub(crate) printer_id: LocalPrinterId,
    pub(crate) kind: NoticeKind,
    pub(crate) observed_at: Instant,
}

#[derive(Debug, Default)]
struct Tracker {
    active: bool,
    offline: bool,
    overflow_reported: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct NoticeCapture {
    id: LocalPrinterId,
    tracker: Arc<Mutex<Tracker>>,
    sender: mpsc::Sender<PrinterNotice>,
    enabled: watch::Receiver<bool>,
}

impl NoticeCapture {
    pub(crate) fn new(
        id: LocalPrinterId,
        sender: mpsc::Sender<PrinterNotice>,
        enabled: watch::Receiver<bool>,
    ) -> Self {
        Self {
            id,
            tracker: Arc::default(),
            sender,
            enabled,
        }
    }

    pub(crate) fn observe(&self, observation: &PrinterObservation, at: Instant) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tracker.offline {
            tracker.offline = false;
            self.emit(NoticeKind::Online, at, &mut tracker);
        }
        match observation.status {
            Some(PrintStatus::Printing | PrintStatus::Paused) => tracker.active = true,
            Some(PrintStatus::Completed | PrintStatus::Error) => {
                if tracker.active {
                    let kind = if observation.status == Some(PrintStatus::Completed) {
                        NoticeKind::Completed
                    } else {
                        NoticeKind::PrintError
                    };
                    self.emit(kind, at, &mut tracker);
                }
                tracker.active = false;
            }
            Some(PrintStatus::Idle | PrintStatus::Cancelled) => tracker.active = false,
            Some(PrintStatus::Unknown) | None => {}
        }
    }

    pub(crate) fn disconnected(&self, at: Instant) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tracker.active && !tracker.offline {
            tracker.offline = true;
            self.emit(NoticeKind::Offline, at, &mut tracker);
        }
    }

    pub(crate) fn cancel_disconnect(&self) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tracker.offline {
            tracker.offline = false;
            self.emit(NoticeKind::Online, Instant::now(), &mut tracker);
        }
    }

    fn emit(&self, kind: NoticeKind, at: Instant, tracker: &mut Tracker) {
        if !*self.enabled.borrow() {
            return;
        }
        let notice = PrinterNotice {
            printer_id: self.id,
            kind,
            observed_at: at,
        };
        match self.sender.try_send(notice) {
            Ok(()) => tracker.overflow_reported = false,
            Err(mpsc::error::TrySendError::Full(_)) if !tracker.overflow_reported => {
                tracker.overflow_reported = true;
                crate::diagnostics::log(
                    crate::diagnostics::LogEvent::new(
                        crate::diagnostics::LogLevel::Warn,
                        crate::diagnostics::LogArea::App,
                        "notifications.queue_full",
                    )
                    .printer(self.id.0)
                    .result("dropped"),
                );
            }
            // Receiver closure is expected during application shutdown.
            Err(_) => {}
        }
    }
}
