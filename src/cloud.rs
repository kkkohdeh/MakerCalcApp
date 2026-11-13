use std::time::{Duration, Instant};

use tokio::sync::watch;

pub(crate) use crate::shared::printer::CloudDeviceMetadata as Device;
use crate::shared::thumbnail::ThumbnailHandle;
use crate::shared::workspace::{DetailHandle, OperationHandle};
use crate::state::{LocalPrinterId, PrinterState};

pub(super) fn retry_deadline(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay).unwrap_or_else(|| {
        now.checked_add(Duration::from_secs(365 * 24 * 60 * 60))
            .unwrap_or(now)
    })
}

mod history;
mod planner;
mod session;
mod thumbnails;
mod wire;

pub(crate) const CLOUD_ENDPOINT: &str = "wss://makercalc.app/telemetry";
const SCHEMA_VERSION: u8 = 2;
const LOCAL_MAX_MESSAGE_BYTES: usize = 64 * 1024;
const LOCAL_MAX_TELEMETRY_BYTES: usize = 16 * 1024;
const LOCAL_MAX_PRINTERS: usize = 100;
const MIN_INTERVAL_MS: u64 = 1_000;
const MAX_INTERVAL_MS: u64 = 60_000;
const REFRESH_WINDOW: Duration = Duration::from_secs(30);
const HISTORY_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const INCOMPATIBLE_RETRY: Duration = Duration::from_secs(10 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct CloudConfig {
    pub(crate) edge_token: Option<String>,
    pub(crate) installation_id: String,
}

#[derive(Clone)]
pub(crate) struct CloudPrinterReceiver {
    pub(crate) id: LocalPrinterId,
    pub(crate) generation: u64,
    pub(crate) cloud_printer_id: String,
    pub(crate) name: String,
    pub(crate) receiver: watch::Receiver<PrinterState>,
    pub(crate) detail: DetailHandle,
    pub(crate) operation: OperationHandle,
    pub(crate) device: Option<Device>,
    pub(crate) thumbnail: Option<ThumbnailHandle>,
}

#[derive(Clone, Default)]
pub(crate) struct CloudRegistry {
    pub(crate) history: Option<crate::history::HistoryHandle>,
    pub(crate) printers: Vec<CloudPrinterReceiver>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CloudStatus {
    NotConfigured,
    Connecting,
    WaitingForReady,
    Connected,
    NeedsAttention(String),
    Unavailable(String),
    Incompatible(String),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CloudView {
    pub(crate) status: Option<CloudStatus>,
    pub(crate) pending_printers: usize,
    pub(crate) last_accepted_seq: Option<u64>,
    pub(crate) last_rejected: Option<String>,
}

pub(crate) async fn run_cloud(
    registry: watch::Receiver<CloudRegistry>,
    configuration: watch::Receiver<CloudConfig>,
    view_sender: watch::Sender<CloudView>,
    shutdown: watch::Receiver<bool>,
) {
    session::run_cloud_at_endpoint(
        CLOUD_ENDPOINT,
        true,
        registry,
        configuration,
        view_sender,
        shutdown,
    )
    .await;
}
