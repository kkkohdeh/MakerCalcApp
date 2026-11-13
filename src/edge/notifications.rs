//! One event consumer for the whole app, independent of GPUI visibility.
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::UiRegistry;
use crate::i18n::t;
use crate::shared::notifications::{NoticeKind, PrinterNotice};
use crate::state::{LanConnection, LocalPrinterId};

const DISCONNECT_GRACE: Duration = Duration::from_secs(30);

#[derive(Default)]
struct PendingDisconnects(BTreeMap<LocalPrinterId, Instant>);

impl PendingDisconnects {
    fn consume(&mut self, notice: PrinterNotice) -> Option<NoticeKind> {
        match notice.kind {
            NoticeKind::Offline => {
                self.0
                    .entry(notice.printer_id)
                    .or_insert_with(|| Instant::from_std(notice.observed_at) + DISCONNECT_GRACE);
                None
            }
            NoticeKind::Online => {
                self.0.remove(&notice.printer_id);
                None
            }
            kind => {
                self.0.remove(&notice.printer_id);
                Some(kind)
            }
        }
    }
    fn next(&self) -> Option<Instant> {
        self.0.values().copied().min()
    }
    fn take_due(&mut self, now: Instant) -> Vec<LocalPrinterId> {
        let due: Vec<_> = self
            .0
            .iter()
            .filter_map(|(&id, &at)| (at <= now).then_some(id))
            .collect();
        for id in &due {
            self.0.remove(id);
        }
        due
    }
}

async fn wait_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(super) async fn run(
    mut notices: mpsc::Receiver<PrinterNotice>,
    mut registry: watch::Receiver<UiRegistry>,
    mut enabled: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pending = PendingDisconnects::default();
    let mut reported_failure = false;
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
            changed = enabled.changed() => {
                if changed.is_err() { return; }
                if !*enabled.borrow_and_update() { pending.0.clear(); }
            }
            changed = registry.changed() => {
                if changed.is_err() { return; }
                let registry = registry.borrow_and_update();
                pending.0.retain(|id, _| registry.printers.iter().any(|printer| printer.id == *id));
            }
            notice = notices.recv() => {
                let Some(notice) = notice else { return; };
                if !*enabled.borrow() { continue; }
                if let Some(kind) = pending.consume(notice) {
                    let name = registry.borrow().printers.iter().find(|p| p.id == notice.printer_id).map(|p| p.name.clone());
                    if let Some(name) = name { deliver(notice.printer_id, kind, name, &mut reported_failure).await; }
                }
            }
            _ = wait_until(pending.next()) => {
                if !*enabled.borrow() { pending.0.clear(); continue; }
                for id in pending.take_due(Instant::now()) {
                    let name = registry.borrow().printers.iter().find(|p| p.id == id && p.receiver.borrow().connection != LanConnection::Connected).map(|p| p.name.clone());
                    if let Some(name) = name { deliver(id, NoticeKind::Offline, name, &mut reported_failure).await; }
                }
            }
        }
    }
}

async fn deliver(id: LocalPrinterId, kind: NoticeKind, name: String, reported_failure: &mut bool) {
    let message = match kind {
        NoticeKind::Completed => t!("printers.notifications.completed", name = name).into_owned(),
        NoticeKind::PrintError => t!("printers.notifications.error", name = name).into_owned(),
        NoticeKind::Offline => t!("printers.notifications.disconnected", name = name).into_owned(),
        NoticeKind::Online => return,
    };
    // Only this consumer awaits native calls: at most one is in flight, never on a LAN or UI task.
    let result =
        tokio::task::spawn_blocking(move || crate::desktop::notifications::show(Some(id), message))
            .await;
    match result {
        Ok(Ok(())) => *reported_failure = false,
        error if !*reported_failure => {
            *reported_failure = true;
            let detail = match error {
                Ok(Err(error)) => error,
                _ => t!("settings.notifications.worker_failed").into_owned(),
            };
            crate::diagnostics::log(
                crate::diagnostics::LogEvent::new(
                    crate::diagnostics::LogLevel::Warn,
                    crate::diagnostics::LogArea::App,
                    "notifications.failed",
                )
                .printer(id.0)
                .result("failed")
                .detail(detail),
            );
        }
        _ => {}
    }
}
