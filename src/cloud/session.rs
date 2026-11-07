use crate::i18n::t;
use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{
    HeaderValue,
    header::{AUTHORIZATION, RETRY_AFTER},
};
use tokio_tungstenite::tungstenite::protocol::{
    Message, WebSocketConfig, frame::coding::CloseCode,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use url::Url;

use super::history::HistorySession;
use super::planner::{ObservedPrinter, Planner, is_due};
use super::thumbnails::ThumbnailSession;
use super::wire::{CommandAction, ServerMessage, decode_server_message, valid_remote_id};
use super::{
    ACK_TIMEOUT, CONNECT_TIMEOUT, CloudConfig, CloudRegistry, CloudStatus, CloudView,
    HISTORY_POLL_INTERVAL, INCOMPATIBLE_RETRY, LOCAL_MAX_MESSAGE_BYTES, PING_INTERVAL,
    PONG_TIMEOUT, READY_TIMEOUT, SCHEMA_VERSION,
};
use crate::shared::workspace::{DetailRequest, LightRole, PrinterAction};
use crate::state::{LanConnection, PrintStatus};

#[derive(Clone, Copy)]
pub(super) struct CloudEndpoint<'a> {
    pub(super) url: &'a str,
    pub(super) require_wss: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum SessionResult {
    Stopped,
    ConfigurationChanged,
    NeedsAttention(String),
    Incompatible(String),
    Temporary(String),
    RetryAfter(String, Duration),
}

pub(super) fn retry_delay(attempt: u32) -> Duration {
    let cap = (1_u64 << attempt.min(5)).min(30);
    let seconds = 1 + RandomState::new().hash_one(attempt) % cap;
    Duration::from_secs(seconds)
}

pub(super) fn http_result(status: u16, body: &[u8], retry_after: Option<&str>) -> SessionResult {
    match status {
        401 => SessionResult::NeedsAttention(t!("settings.cloud.errors.key").into_owned()),
        403 if std::str::from_utf8(body)
            .ok()
            .is_some_and(|text| text.trim() == "telemetry_not_allowed") =>
        {
            SessionResult::Temporary(t!("settings.cloud.errors.plan").into_owned())
        }
        409 => SessionResult::Temporary(t!("settings.cloud.errors.key_in_use").into_owned()),
        503 => {
            let retry_after = retry_after
                .and_then(|value| value.parse::<u64>().ok())
                .map(|seconds| Duration::from_secs(seconds.min(30)))
                .unwrap_or_default();
            SessionResult::RetryAfter(
                t!("settings.cloud.errors.unavailable").into_owned(),
                retry_after,
            )
        }
        400 => {
            SessionResult::Incompatible(t!("settings.cloud.errors.update_required").into_owned())
        }
        _ => SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned()),
    }
}

pub(super) fn close_result(code: Option<CloseCode>) -> SessionResult {
    match code {
        Some(CloseCode::Invalid) => {
            SessionResult::Incompatible(t!("settings.cloud.errors.rejected").into_owned())
        }
        Some(CloseCode::Unsupported) => {
            SessionResult::Incompatible(t!("settings.cloud.errors.update_required").into_owned())
        }
        _ => SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned()),
    }
}

pub(super) async fn run_cloud_at_endpoint(
    endpoint: &str,
    require_wss: bool,
    mut registry: watch::Receiver<CloudRegistry>,
    mut configuration: watch::Receiver<CloudConfig>,
    view_sender: watch::Sender<CloudView>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut planner = Planner::new();
    let mut retry_attempt = 0_u32;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let config = configuration.borrow().clone();
        if config.edge_token.is_none() {
            publish_view(
                &view_sender,
                CloudStatus::NotConfigured,
                planner.pending_count(Instant::now()),
                None,
                None,
            );
            tokio::select! {
                changed = configuration.changed() => {
                    if changed.is_err() { return; }
                    retry_attempt = 0;
                },
                changed = registry.changed() => if changed.is_err() { return; },
                changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
            }
            continue;
        }
        sync_registry(&mut planner, &registry, Instant::now());
        publish_view(
            &view_sender,
            CloudStatus::Connecting,
            planner.pending_count(Instant::now()),
            None,
            None,
        );
        let session_started = Instant::now();
        let result = run_session(
            CloudEndpoint {
                url: endpoint,
                require_wss,
            },
            &config,
            &mut planner,
            &mut registry,
            &mut configuration,
            &view_sender,
            &mut shutdown,
        )
        .await;
        let session_lifetime = session_started.elapsed();
        planner.disconnect();
        match result {
            SessionResult::Stopped => return,
            SessionResult::ConfigurationChanged => {
                retry_attempt = 0;
            }
            SessionResult::NeedsAttention(message) => {
                publish_view(
                    &view_sender,
                    CloudStatus::NeedsAttention(message),
                    planner.pending_count(Instant::now()),
                    None,
                    None,
                );
                tokio::select! {
                    changed = configuration.changed() => {
                        if changed.is_err() { return; }
                        retry_attempt = 0;
                    },
                    changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
                }
            }
            SessionResult::Incompatible(message) => {
                publish_view(
                    &view_sender,
                    CloudStatus::Incompatible(
                        t!("settings.cloud.retry_rejected", message = message).into_owned(),
                    ),
                    planner.pending_count(Instant::now()),
                    None,
                    None,
                );
                tokio::select! {
                    _ = tokio::time::sleep(INCOMPATIBLE_RETRY) => {},
                    changed = configuration.changed() => {
                        if changed.is_err() { return; }
                        retry_attempt = 0;
                    },
                    changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
                }
            }
            result => {
                let (message, retry_after) = match result {
                    SessionResult::Temporary(message) => (message, None),
                    SessionResult::RetryAfter(message, retry_after) => (message, Some(retry_after)),
                    _ => unreachable!(),
                };
                if session_lifetime >= Duration::from_secs(60) {
                    retry_attempt = 0;
                }
                let mut wait = retry_delay(retry_attempt);
                retry_attempt = retry_attempt.saturating_add(1);
                if let Some(retry_after) = retry_after {
                    wait = wait.max(retry_after);
                }
                publish_view(
                    &view_sender,
                    CloudStatus::Unavailable(message),
                    planner.pending_count(Instant::now()),
                    None,
                    None,
                );
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    changed = configuration.changed() => {
                        if changed.is_err() { return; }
                        retry_attempt = 0;
                    },
                    changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
                }
            }
        }
    }
}

pub(super) async fn run_session(
    endpoint: CloudEndpoint<'_>,
    config: &CloudConfig,
    planner: &mut Planner,
    registry: &mut watch::Receiver<CloudRegistry>,
    configuration: &mut watch::Receiver<CloudConfig>,
    view_sender: &watch::Sender<CloudView>,
    shutdown: &mut watch::Receiver<bool>,
) -> SessionResult {
    let Some(token) = config.edge_token.as_deref() else {
        return SessionResult::Stopped;
    };
    let request = match cloud_request(endpoint, token, &config.installation_id) {
        Ok(request) => request,
        Err(_) => {
            return SessionResult::Incompatible(
                t!("settings.cloud.errors.update_required").into_owned(),
            );
        }
    };
    let socket_config = WebSocketConfig::default()
        .max_message_size(Some(LOCAL_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(LOCAL_MAX_MESSAGE_BYTES));
    let (mut socket, _) = match timeout(
        CONNECT_TIMEOUT,
        connect_async_with_config(request, Some(socket_config), false),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok());
            return http_result(
                response.status().as_u16(),
                response.body().as_deref().unwrap_or_default(),
                retry_after,
            );
        }
        Ok(Err(_)) | Err(_) => {
            return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
        }
    };
    publish_view(
        view_sender,
        CloudStatus::WaitingForReady,
        planner.pending_count(Instant::now()),
        None,
        None,
    );
    let ready = match receive_server_message(&mut socket, READY_TIMEOUT).await {
        Ok(ServerMessage::Ready {
            v,
            edge_id,
            workshop_id,
            max_message_bytes,
            max_printers,
            min_interval_ms,
            history_max,
            history_persist,
            thumbnail_max_bytes,
        }) if v == SCHEMA_VERSION && valid_remote_id(&edge_id) && valid_remote_id(&workshop_id) => {
            (
                max_message_bytes,
                max_printers,
                min_interval_ms,
                history_max,
                history_persist,
                thumbnail_max_bytes,
            )
        }
        Ok(ServerMessage::Ready { v, .. }) if v != SCHEMA_VERSION => {
            return SessionResult::Incompatible(
                t!("settings.cloud.errors.update_required").into_owned(),
            );
        }
        Ok(ServerMessage::Ready { .. }) => {
            return SessionResult::Incompatible(
                t!("settings.cloud.errors.update_required").into_owned(),
            );
        }
        Ok(_) => {
            return SessionResult::Incompatible(
                t!("settings.cloud.errors.update_required").into_owned(),
            );
        }
        Err(result) => return result,
    };
    if planner.begin_session(ready.0, ready.1, ready.2).is_err() {
        return SessionResult::Incompatible(
            t!("settings.cloud.errors.update_required").into_owned(),
        );
    }
    let mut history = HistorySession::new(ready.3, planner.limits.min_interval);
    let _history_persist = ready.4; // Server cursors select history in both storage modes.
    let mut thumbnails = ThumbnailSession::new(ready.5);
    sync_registry_with_thumbnails(planner, registry, Instant::now(), Some(&thumbnails));
    let mut ticks = interval(Duration::from_secs(1));
    let mut history_ticks = interval(HISTORY_POLL_INTERVAL);
    history_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pings = interval(PING_INTERVAL);
    pings.tick().await;
    let mut pong_deadline = None;
    loop {
        if planner.in_flight.is_none() && history.flight.is_none() && thumbnails.flight.is_none() {
            let now = Instant::now();
            thumbnails.observe(&registry.borrow(), &history.announced, now);
            let live_due = planner.records.values().any(|record| is_due(record, now))
                && planner.last_batch_started_at.is_none_or(|last| {
                    now.saturating_duration_since(last)
                        >= planner
                            .limits
                            .min_interval
                            .saturating_sub(Duration::from_millis(100))
                });
            if !live_due
                && let Some(payload) =
                    thumbnails.prepare(planner.next_seq, planner.limits.max_message_bytes, now)
            {
                let Some(next_seq) = planner.next_seq.checked_add(1) else {
                    return SessionResult::Incompatible("Secuencia agotada".to_owned());
                };
                planner.next_seq = next_seq;
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    return SessionResult::Temporary(
                        t!("settings.cloud.errors.connection").into_owned(),
                    );
                }
            }
        }
        tokio::select! {
            completed = thumbnails.fetches.join_next(), if !thumbnails.fetches.is_empty() => {
                if let Some(Ok((key, image))) = completed { thumbnails.completed(key, image, Instant::now()); }
                thumbnails.observe(&registry.borrow(), &history.announced, Instant::now());
            }
            _ = history_ticks.tick() => request_history_totals(registry),
            _ = ticks.tick() => {
                let now = Instant::now();
                if pong_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
                    return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                }
                thumbnails.observe(&registry.borrow(), &history.announced, now);
                sync_registry_with_thumbnails(planner, registry, now, Some(&thumbnails));
                if planner.ack_timed_out(now) || history.flight.is_some_and(|(_, sent)| now.saturating_duration_since(sent) >= ACK_TIMEOUT)
                    || thumbnails.flight.as_ref().is_some_and(|flight| now.saturating_duration_since(flight.sent_at) >= ACK_TIMEOUT) {
                    return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                }
                if history.flight.is_none() && thumbnails.flight.is_none() && let Ok(Some(batch)) = planner.select(now) {
                    let mut batch = batch;
                    if socket.send(Message::Text(std::mem::take(&mut batch.payload).into())).await.is_err() {
                        return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                    }
                    planner.mark_sent(batch, now);
                }
                if planner.in_flight.is_none() && history.flight.is_none() && thumbnails.flight.is_none() && now >= history.retry_at
                    && history.last_sent.is_none_or(|last| now.saturating_duration_since(last) >= history.interval)
                {
                    let store = registry.borrow().history.clone();
                    if let Some(store) = store {
                        match tokio::time::timeout(Duration::from_millis(100), history.fill(&store)).await {
                            Ok(Ok(())) => {},
                            Ok(Err(error)) => { view_sender.send_modify(|view| view.last_rejected = Some(error)); },
                            Err(_) => { view_sender.send_modify(|view| view.last_rejected = Some(t!("settings.cloud.errors.history_busy").into_owned())); },
                        }
                        if !history.pending.is_empty() {
                            let seq = planner.next_seq;
                            let count = history.fitting_count(seq, planner.limits.max_message_bytes).unwrap_or(0);
                            let payload = history.payload(seq, count).ok();
                            if count == 0 {
                                history.pending.remove(0);
                                view_sender.send_modify(|view| view.last_rejected = Some(t!("settings.cloud.errors.history_row_size").into_owned()));
                            } else if let Some(payload) = payload {
                                let Some(next_seq) = planner.next_seq.checked_add(1) else { return SessionResult::Incompatible("Secuencia agotada".to_owned()); };
                                planner.next_seq = next_seq;
                                if socket.send(Message::Text(payload.into())).await.is_err() { return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned()); }
                                history.mark_sent(seq, count, now);
                            }
                        }
                    }
                }
                view_sender.send_modify(|view| {
                    view.status = Some(CloudStatus::Connected);
                    view.pending_printers = planner.pending_count(now);
                    if let Some(error) = planner.local_error() {
                        view.last_rejected = Some(error);
                    }
                });
            }
            _ = pings.tick() => {
                if pong_deadline.is_some() {
                    return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                }
                pong_deadline = Some(tokio::time::Instant::now() + PONG_TIMEOUT);
            }
            message = socket.next() => {
                let Some(message) = message else {
                    return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                };
                let message = match message {
                    Ok(message) => message,
                    Err(_) => return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned()),
                };
                if let Message::Close(frame) = &message {
                    return close_result(frame.as_ref().map(|frame| frame.code));
                }
                if matches!(message, Message::Ping(_) | Message::Frame(_)) {
                    if socket.flush().await.is_err() {
                        return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                    }
                    continue;
                }
                if matches!(message, Message::Pong(_)) {
                    pong_deadline = None;
                    continue;
                }
                match decode_server_message(message) {
                    Ok(ServerMessage::Ack { v: SCHEMA_VERSION, seq }) => {
                        if thumbnails.acknowledge(seq) {
                            sync_registry_with_thumbnails(planner, registry, Instant::now(), Some(&thumbnails));
                            continue;
                        }
                        if history.flight.is_some_and(|(pending, _)| pending == seq) {
                            history.accepted();
                            continue;
                        }
                        let announced = planner.in_flight.as_ref().filter(|batch| batch.seq == seq).map(|batch| batch.captured.iter().map(|printer| printer.cloud_printer_id.clone()).collect::<Vec<_>>()).unwrap_or_default();
                        let epoch = planner.epoch;
                        if planner.acknowledge(epoch, seq) {
                            history.announced.extend(announced);
                            thumbnails.observe(&registry.borrow(), &history.announced, Instant::now());
                            publish_view(view_sender, CloudStatus::Connected, planner.pending_count(Instant::now()), Some(seq), planner.local_error());
                        }
                    }
                    Ok(ServerMessage::Reject { v: SCHEMA_VERSION, seq, code, retry_after_ms }) => {
                        if thumbnails.reject(seq, &code, retry_after_ms, Instant::now()) {
                            view_sender.send_modify(|view| view.last_rejected = Some(t!("settings.cloud.errors.thumbnail").into_owned()));
                            continue;
                        }
                        if history.flight.is_some_and(|(pending, _)| pending == seq) {
                            match code.as_str() {
                                "rate_limited" | "unavailable" => {
                                    let now = Instant::now();
                                    let retry_at = now.checked_add(Duration::from_millis(retry_after_ms.unwrap_or(5_000)))
                                        .unwrap_or(now + Duration::from_secs(365 * 24 * 60 * 60));
                                    history.retry_after_rejection(retry_at);
                                }
                                _ => {
                                    history.accepted();
                                    crate::diagnostics::log(crate::diagnostics::LogEvent::new(crate::diagnostics::LogLevel::Warn, crate::diagnostics::LogArea::App, "history.batch_rejected"));
                                    view_sender.send_modify(|view| view.last_rejected = Some(t!("settings.cloud.errors.history_batch").into_owned()));
                                }
                            }
                            continue;
                        }
                        let epoch = planner.epoch;
                        if !planner.reject(epoch, seq, Instant::now(), &code) {
                            continue;
                        }
                        match code.as_str() {
                            "forbidden" => {
                                publish_view(view_sender, CloudStatus::Connected, planner.pending_count(Instant::now()), None, Some(t!("settings.cloud.errors.printer_limit").into_owned()));
                            }
                            "invalid_payload" => {
                                return SessionResult::Incompatible(t!("settings.cloud.errors.rejected").into_owned());
                            }
                            "rate_limited" => {
                                let Some(retry_after_ms) = retry_after_ms else {
                                    return SessionResult::Incompatible(t!("settings.cloud.errors.update_required").into_owned());
                                };
                                planner.adopt_retry_interval(retry_after_ms);
                                publish_view(view_sender, CloudStatus::Connected, planner.pending_count(Instant::now()), None, Some(t!("settings.cloud.errors.rate_limit").into_owned()));
                            }
                            "unavailable" => return SessionResult::Temporary(t!("settings.cloud.errors.unavailable").into_owned()),
                            _ => return SessionResult::Incompatible(t!("settings.cloud.errors.update_required").into_owned()),
                        }
                    }
                    Ok(ServerMessage::SessionError { v: SCHEMA_VERSION, .. }) => {
                        return SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned());
                    }
                    Ok(ServerMessage::HistoryCursors { v: SCHEMA_VERSION, cursors }) => history.cursors(cursors),
                    Ok(ServerMessage::Command { v, printer_id, action, role }) => {
                        if v == SCHEMA_VERSION {
                            dispatch_command(&registry.borrow(), &printer_id, action, role.as_deref());
                        }
                    },
                    Ok(ServerMessage::Unknown) => {},
                    Ok(_) | Err(_) => return SessionResult::Incompatible(t!("settings.cloud.errors.update_required").into_owned()),
                }
            }
            changed = registry.changed() => {
                if changed.is_err() {
                    return SessionResult::Stopped;
                }
                thumbnails.observe(&registry.borrow(), &history.announced, Instant::now());
                sync_registry_with_thumbnails(planner, registry, Instant::now(), Some(&thumbnails));
                request_history_totals(registry);
            }
            changed = configuration.changed() => {
                if changed.is_err() {
                    return SessionResult::Stopped;
                }
                return SessionResult::ConfigurationChanged;
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return SessionResult::Stopped;
                }
            }
        }
    }
}

pub(super) fn dispatch_command(
    registry: &CloudRegistry,
    printer_id: &str,
    action: CommandAction,
    role: Option<&str>,
) {
    let role = match role {
        None => None,
        Some("build_area") => Some(LightRole::BuildArea),
        Some("toolhead") => Some(LightRole::Toolhead),
        Some(_) => return,
    };
    let Some(printer) = registry
        .printers
        .iter()
        .find(|printer| printer.cloud_printer_id == printer_id)
    else {
        return;
    };
    let state = printer.receiver.borrow();
    if state.connection != LanConnection::Connected
        || !state.telemetry_ready
        || printer.operation.state.borrow().is_pending()
        // A queued local/remote action also counts as busy, before the driver publishes Sending.
        || printer.operation.actions.capacity() < printer.operation.actions.max_capacity()
    {
        return;
    }
    let context = crate::shared::workspace::PrintControlContext::new(
        state.connection_epoch,
        state.job_generation,
    );
    let action = match (action, role, state.telemetry.print_state) {
        (CommandAction::PausePrint, None, PrintStatus::Printing) => {
            PrinterAction::PausePrint(context)
        }
        (CommandAction::ResumePrint, None, PrintStatus::Paused) => {
            PrinterAction::ResumePrint(context)
        }
        (CommandAction::CancelPrint, None, PrintStatus::Printing | PrintStatus::Paused) => {
            PrinterAction::CancelPrint(context)
        }
        (CommandAction::ToggleLight, Some(role), _)
            if state
                .telemetry
                .lights
                .iter()
                .any(|light| light.role == role && light.enabled.is_some()) =>
        {
            PrinterAction::ToggleLight { role }
        }
        _ => return,
    };
    // Best effort, at most once: never wait, retry, or replay after reconnecting.
    let _ = printer.operation.actions.try_send(action);
}

pub(super) fn sync_registry(
    planner: &mut Planner,
    registry: &watch::Receiver<CloudRegistry>,
    now: Instant,
) {
    sync_registry_with_thumbnails(planner, registry, now, None);
}

pub(super) fn sync_registry_with_thumbnails(
    planner: &mut Planner,
    registry: &watch::Receiver<CloudRegistry>,
    now: Instant,
    thumbnails: Option<&ThumbnailSession>,
) {
    let registry = registry.borrow();
    let _registry_revision = registry.revision;
    let mut ids = std::collections::BTreeSet::new();
    for printer in registry.printers.iter().take(250) {
        ids.insert(printer.id);
        let state = printer.receiver.borrow();
        let detail = printer.detail.state.borrow();
        let thumbnail_sha256 = thumbnails.and_then(|session| session.current_hash(printer, &state));
        let thumbnail_supported = thumbnails.is_some_and(|session| session.max_bytes.is_some());
        if let Some(record) = planner.records.get_mut(&printer.id)
            && record.observed.generation == printer.generation
            && record.observed.cloud_printer_id == printer.cloud_printer_id
            && record.last_observed_revision == state.revision
            && record.observed.name == printer.name
            && record.observed.history_totals == detail.history_totals.value
            && record.observed.device == printer.device
            && record.observed.thumbnail_sha256.as_deref() == thumbnail_sha256
            && record.observed.thumbnail_supported == thumbnail_supported
        {
            record.observed.state.last_valid_at = state.last_valid_at;
            continue;
        }
        #[cfg(test)]
        {
            planner.work.state_clones += 1;
        }
        planner.observe_one(
            ObservedPrinter {
                id: printer.id,
                generation: printer.generation,
                cloud_printer_id: printer.cloud_printer_id.clone(),
                name: printer.name.clone(),
                state: state.clone(),
                history_totals: detail.history_totals.value.clone(),
                device: printer.device.clone(),
                thumbnail_sha256: thumbnail_sha256.map(str::to_owned),
                thumbnail_supported,
            },
            now,
        );
    }
    planner.retain(&ids);
}

pub(super) fn request_history_totals(registry: &watch::Receiver<CloudRegistry>) {
    for printer in registry.borrow().printers.iter().take(250) {
        let _ = printer
            .detail
            .requests
            .try_send(DetailRequest::HistoryTotals);
    }
}

pub(super) fn cloud_request(
    endpoint: CloudEndpoint<'_>,
    token: &str,
    installation_id: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let url = Url::parse(endpoint.url).map_err(|_| "URL cloud inválida".to_owned())?;
    if endpoint.require_wss && url.scheme() != "wss" {
        return Err("la entrada cloud debe usar WSS".to_owned());
    }
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err("la entrada cloud debe usar WebSocket".to_owned());
    }
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| "solicitud cloud inválida".to_owned())?;
    let header_value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "credencial cloud inválida".to_owned())?;
    request.headers_mut().insert(AUTHORIZATION, header_value);
    let installation_header = HeaderValue::from_str(installation_id)
        .map_err(|_| "id de instalación inválido".to_owned())?;
    request
        .headers_mut()
        .insert("MakerCalc-Installation-Id", installation_header);
    request
        .headers_mut()
        .insert("MakerCalc-Edge-Features", HeaderValue::from_static("jobs"));
    Ok(request)
}

pub(super) async fn receive_server_message(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    wait: Duration,
) -> Result<ServerMessage, SessionResult> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let message = timeout(remaining, socket.next())
            .await
            .map_err(|_| {
                SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned())
            })?
            .ok_or_else(|| {
                SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned())
            })?
            .map_err(|_| {
                SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned())
            })?;
        if let Message::Close(frame) = &message {
            return Err(close_result(frame.as_ref().map(|frame| frame.code)));
        }
        if matches!(
            message,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_)
        ) {
            socket.flush().await.map_err(|_| {
                SessionResult::Temporary(t!("settings.cloud.errors.connection").into_owned())
            })?;
            continue;
        }
        let decoded = decode_server_message(message).map_err(|_| {
            SessionResult::Incompatible(t!("settings.cloud.errors.update_required").into_owned())
        })?;
        // Commands received before ready cannot execute or make the handshake incompatible.
        if matches!(decoded, ServerMessage::Command { .. }) {
            continue;
        }
        return Ok(decoded);
    }
}

pub(super) fn publish_view(
    sender: &watch::Sender<CloudView>,
    status: CloudStatus,
    pending_printers: usize,
    last_accepted_seq: Option<u64>,
    last_rejected: Option<String>,
) {
    sender.send_modify(|view| {
        view.status = Some(status);
        view.pending_printers = pending_printers;
        if let Some(seq) = last_accepted_seq {
            view.last_accepted_seq = Some(seq);
            view.last_rejected = last_rejected;
        } else if let Some(rejected) = last_rejected {
            view.last_rejected = Some(rejected);
        }
    });
}
