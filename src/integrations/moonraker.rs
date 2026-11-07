pub(crate) mod adapter;
pub(crate) mod camera;
pub(crate) mod detail;
pub(crate) mod local;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::fs::File;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout, timeout_at};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use tokio_util::io::ReaderStream;
use url::Url;

use self::camera::{
    CameraIo, CameraRequest, CameraState, CameraStatus, fetch_snapshot, same_origin_snapshot_url,
};
use self::detail::{
    DETAIL_MAX_BYTES, DetailCache, DetailError, DetailIo, DetailRequest, DetailState,
    PreviewResult, SectionData, SectionStatus, build_read_request, fetch_file_preview,
    fetch_gcode_preview, redact_json, subscription_params,
};
use self::local::{
    OperationIo, OperationState, PrinterAction, build_control_request, validate_upload,
};
use crate::config::{
    MoonrakerAuth, MoonrakerConnection, MoonrakerLightBinding, MoonrakerLightBindings,
};
use crate::shared::gcode::inspect_gcode_file_parts;
use crate::shared::telemetry::{
    BedTelemetry, ElementId, FilamentSensorTelemetry, LightRole, LightTelemetry, NozzleTelemetry,
    PartCoolingFanTelemetry, PrinterAlert,
};
use crate::state::{
    LanConnection, LocalPrinterId, PrintStatus, PrinterState, Snapshot, ValidationError,
    sanitize_message,
};

const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const FILE_METADATA_DIRECTORY_LIMIT: usize = 64;
const FILE_METADATA_BUDGET: Duration = Duration::from_secs(20);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const IDENTITY_NAME: &str = "MakerCalc";
const IDENTITY_URL: &str = "https://cloud.makercalc.app";
type MoonrakerSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub(crate) struct DriverConfig {
    pub(crate) id: LocalPrinterId,
    pub(crate) connection: MoonrakerConnection,
    pub(crate) api_key: Option<String>,
}

#[derive(Debug)]
pub(crate) enum MoonrakerError {
    InvalidMessage(String),
    Incompatible,
    Transport(String),
    RequestTimeout,
    Validation(ValidationError),
    Cancelled,
}

impl fmt::Display for MoonrakerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMessage(message) => {
                write!(formatter, "Moonraker respondió datos inválidos: {message}")
            }
            Self::Incompatible => formatter.write_str("Moonraker no expone telemetría compatible"),
            Self::Transport(message) => {
                write!(formatter, "Moonraker no está disponible: {message}")
            }
            Self::RequestTimeout => {
                formatter.write_str("Moonraker no está disponible: timeout de Moonraker")
            }
            Self::Validation(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("consulta de detalle cancelada"),
        }
    }
}

impl std::error::Error for MoonrakerError {}
impl From<ValidationError> for MoonrakerError {
    fn from(error: ValidationError) -> Self {
        Self::Validation(error)
    }
}

#[derive(Default)]
pub(crate) struct StatusParser {
    base: Option<Snapshot>,
    last_eventtime: Option<f64>,
    active_extruder: Option<String>,
    light_bindings: MoonrakerLightBindings,
    chamber_temperature_sensor: Option<String>,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum ParserEvent {
    Snapshot(Snapshot),
    Reset,
    Ignored,
}

impl StatusParser {
    pub(crate) fn with_light_bindings(light_bindings: MoonrakerLightBindings) -> Self {
        Self {
            light_bindings,
            ..Self::default()
        }
    }

    pub(crate) fn apply_base(
        &mut self,
        status: &Value,
        eventtime: f64,
    ) -> Result<Snapshot, MoonrakerError> {
        validate_eventtime(eventtime)?;
        let active_extruder = self.active_extruder_for(status)?;
        let (mut snapshot, mut recognized) = apply_fields(
            &Snapshot::default(),
            status,
            true,
            active_extruder.as_deref(),
            &self.light_bindings,
        )?;
        recognized |= self.apply_chamber_sensor(status, &mut snapshot)?;
        if !recognized {
            return Err(MoonrakerError::Incompatible);
        }
        self.active_extruder = active_extruder;
        self.base = Some(snapshot.clone());
        self.last_eventtime = Some(eventtime);
        Ok(snapshot)
    }

    pub(crate) fn apply_delta(
        &mut self,
        status: &Value,
        eventtime: f64,
    ) -> Result<ParserEvent, MoonrakerError> {
        validate_eventtime(eventtime)?;
        let Some(current) = self.base.clone() else {
            return Ok(ParserEvent::Reset);
        };
        if self.last_eventtime.is_some_and(|last| eventtime < last) {
            return Ok(ParserEvent::Ignored);
        }
        let active_extruder = self.active_extruder_for(status)?;
        let tool_changed = active_extruder != self.active_extruder;
        let (mut next, mut recognized) = apply_fields(
            &current,
            status,
            false,
            active_extruder.as_deref(),
            &self.light_bindings,
        )?;
        recognized |= self.apply_chamber_sensor(status, &mut next)?;
        if !recognized && !tool_changed {
            return Ok(ParserEvent::Ignored);
        }
        self.active_extruder = active_extruder;
        self.base = Some(next.clone());
        self.last_eventtime = Some(eventtime);
        Ok(ParserEvent::Snapshot(next))
    }

    pub(crate) fn reset(&mut self) {
        self.base = None;
        self.last_eventtime = None;
        self.active_extruder = None;
    }

    fn apply_chamber_sensor(
        &self,
        status: &Value,
        snapshot: &mut Snapshot,
    ) -> Result<bool, MoonrakerError> {
        let Some(value) = self
            .chamber_temperature_sensor
            .as_deref()
            .and_then(|name| status.get(name))
        else {
            return Ok(false);
        };
        if value.is_null() {
            snapshot.chamber_c = None;
            return Ok(true);
        }
        let Some(temperature) = value.get("temperature") else {
            return Ok(false);
        };
        snapshot.chamber_c = if temperature.is_null() {
            None
        } else {
            Some(temperature.as_f64().filter(|v| v.is_finite()).ok_or(
                MoonrakerError::Validation(ValidationError::InvalidTemperature),
            )?)
        };
        Ok(true)
    }

    fn subscriptions(&self, objects: &[String], expanded: bool) -> Value {
        let mut params = subscription_params(objects, expanded);
        if let Some(sensor) = &self.chamber_temperature_sensor
            && objects.contains(sensor)
            && params["objects"].get(sensor).is_none()
        {
            params["objects"][sensor] = json!(["temperature"]);
        }
        params
    }

    fn active_extruder_for(&self, status: &Value) -> Result<Option<String>, MoonrakerError> {
        if !status.is_object() {
            return Err(MoonrakerError::InvalidMessage(
                "estado no es un objeto".to_owned(),
            ));
        }
        let Some(value) = status
            .get("toolhead")
            .and_then(Value::as_object)
            .and_then(|object| object.get("extruder"))
        else {
            return Ok(self.active_extruder.clone());
        };
        if value.is_null() {
            return Ok(None);
        }
        let value = value.as_str().ok_or_else(|| {
            MoonrakerError::InvalidMessage("toolhead.extruder no es texto".to_owned())
        })?;
        Ok(Some(value.to_owned()))
    }
}

struct DetailSession {
    generation: u64,
    active: bool,
    cache: DetailCache,
    state: DetailState,
    objects: Vec<String>,
    roots: BTreeSet<String>,
    namespaces: BTreeSet<String>,
    devices: BTreeSet<String>,
    sensors: BTreeSet<String>,
    wled_strips: BTreeSet<String>,
    cameras: BTreeSet<String>,
    camera_snapshots: BTreeMap<String, String>,
    pending_deltas: Vec<(Value, f64)>,
}

struct FileReader {
    client: reqwest::Client,
    base: String,
}

impl DetailSession {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            active: false,
            cache: DetailCache::new(generation, DETAIL_MAX_BYTES),
            state: DetailState::empty(generation),
            objects: Vec::new(),
            roots: BTreeSet::new(),
            namespaces: BTreeSet::new(),
            devices: BTreeSet::new(),
            sensors: BTreeSet::new(),
            wled_strips: BTreeSet::new(),
            cameras: BTreeSet::new(),
            camera_snapshots: BTreeMap::new(),
            pending_deltas: Vec::new(),
        }
    }
    fn clear(&mut self) {
        self.active = false;
        self.cache.reset(self.generation);
        self.state = DetailState::empty(self.generation);
        self.pending_deltas.clear();
    }
    fn set_section(
        &mut self,
        key: String,
        status: SectionStatus,
        data: Option<Value>,
        error: Option<String>,
    ) {
        if let Some((id, token)) = file_section_token(&key) {
            if status == SectionStatus::Loading {
                self.state.sections.retain(|old, _| {
                    file_section_token(old)
                        .is_none_or(|(old_id, old_token)| old_id != id || old_token == token)
                });
                let mut files: BTreeMap<_, _> = self
                    .state
                    .sections
                    .keys()
                    .filter_map(|key| file_section_token(key))
                    .collect();
                files.insert(id, token);
                while files.len() > 8 {
                    let Some((&oldest, _)) = files.iter().min_by_key(|(_, token)| **token) else {
                        break;
                    };
                    files.remove(&oldest);
                    self.state.sections.retain(|old, _| {
                        file_section_token(old).is_none_or(|(old_id, _)| old_id != oldest)
                    });
                }
            } else if !self.state.sections.contains_key(&key) {
                // A newer inspection or bounded cache eviction superseded this completion.
                return;
            }
        }
        let retained_thumbnail = if data.is_none()
            && matches!(
                status,
                SectionStatus::Loading | SectionStatus::Error | SectionStatus::Limited
            ) {
            self.state
                .sections
                .get(&key)
                .and_then(|previous| previous.thumbnail.clone())
        } else {
            None
        };
        let retained_facts = if data.is_none()
            && matches!(
                status,
                SectionStatus::Loading | SectionStatus::Error | SectionStatus::Limited
            ) {
            self.state
                .sections
                .get(&key)
                .and_then(|previous| previous.gcode_facts.clone())
        } else {
            None
        };
        let retained_data = matches!(
            status,
            SectionStatus::Loading | SectionStatus::Error | SectionStatus::Limited
        )
        .then(|| {
            self.state
                .sections
                .get(&key)
                .and_then(|previous| previous.data.clone())
        })
        .flatten();
        self.state.sections.insert(
            key,
            SectionData {
                status,
                data: data.or(retained_data),
                gcode_facts: retained_facts,
                thumbnail: retained_thumbnail,
                error,
            },
        );
    }
}

fn file_section_token(key: &str) -> Option<(u64, u64)> {
    let suffix = key
        .strip_prefix("model:metadata:")
        .or_else(|| key.strip_prefix("model:preview:"))?;
    let (id, token) = suffix.split_once(':')?;
    Some((id.parse().ok()?, token.parse().ok()?))
}

enum RpcReply {
    Result(Value),
    Error { code: Option<i64>, message: String },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RpcPurpose {
    Detail,
    NativeHistory,
}
enum SessionSignal {
    Continue,
    Reset,
    KlippyReady,
}

pub(crate) async fn run_driver(
    config: DriverConfig,
    sender: watch::Sender<PrinterState>,
    mut shutdown: watch::Receiver<bool>,
    mut detail_io: DetailIo,
    mut operation_io: OperationIo,
    mut camera_io: CameraIo,
) {
    let mut state = sender.borrow().clone();
    debug_assert_eq!(state.id, config.id);
    let mut delay_seconds = 1_u64;
    let mut generation = 0_u64;
    let mut consecutive_errors = 0_u32;
    loop {
        if *shutdown.borrow() {
            return;
        }
        generation = generation.saturating_add(1);
        if !matches!(&*operation_io.state.borrow(), OperationState::Unknown(_)) {
            let _ = operation_io.state.send_replace(OperationState::Idle);
        }
        // Commands queued for a previous connection are not safe to replay.
        if generation > 1 {
            while let Ok(action) = operation_io.actions.try_recv() {
                operation_io.state.send_replace(OperationState::Rejected {
                    action: action.kind(),
                    message: "La sesión cambió antes de enviar la acción. Confírmala de nuevo."
                        .to_owned(),
                });
            }
        }
        let _ = detail_io
            .state
            .send_replace(Arc::new(DetailState::empty(generation)));
        let _ = camera_io.state.send_replace(Arc::new(CameraState::empty()));
        if publish_connection(&mut state, &sender, LanConnection::Connecting).is_err() {
            return;
        }
        let result = connect_once(
            &config,
            &mut state,
            &sender,
            &mut shutdown,
            &mut detail_io,
            &mut operation_io,
            &mut camera_io,
            generation,
        )
        .await;
        match result {
            Ok(()) => {
                consecutive_errors = 0;
                delay_seconds = 1;
            }
            Err(MoonrakerError::Incompatible) => {
                let _ = publish_connection(&mut state, &sender, LanConnection::Incompatible);
                let _ = state.record_error("Moonraker no expone telemetría compatible");
                let _ = sender.send(state.clone());
                return;
            }
            Err(MoonrakerError::Cancelled) => continue,
            Err(error) => {
                if is_credential_error(&error) {
                    let _ =
                        publish_connection(&mut state, &sender, LanConnection::NeedsCredentials);
                    let _ = state.record_error(error.to_string());
                    let _ = sender.send(state.clone());
                    return;
                }
                if consecutive_errors == 0 {
                    consecutive_errors = 1;
                    let _ = publish_connection(&mut state, &sender, LanConnection::Connecting);
                    delay_seconds = 1;
                } else {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    let _ = publish_connection(&mut state, &sender, LanConnection::Disconnected);
                    let _ = state.record_error(error.to_string());
                    let _ = sender.send(state.clone());
                }
            }
        }
        let delay = Duration::from_secs(delay_seconds);
        delay_seconds = (delay_seconds.saturating_mul(2)).min(30);
        tokio::select! {
            _ = sleep(delay) => {}
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_once(
    config: &DriverConfig,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    shutdown: &mut watch::Receiver<bool>,
    detail_io: &mut DetailIo,
    operation_io: &mut OperationIo,
    camera_io: &mut CameraIo,
    generation: u64,
) -> Result<(), MoonrakerError> {
    let request = handshake_request(config)?;
    let websocket_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (mut socket, _) = timeout(
        REQUEST_TIMEOUT,
        connect_async_with_config(request, Some(websocket_config), false),
    )
    .await
    .map_err(|_| MoonrakerError::Transport("timeout de conexión".to_owned()))?
    .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
    let mut parser = StatusParser::with_light_bindings(config.connection.light_bindings.clone());
    parser
        .chamber_temperature_sensor
        .clone_from(&config.connection.chamber_temperature_sensor);
    let mut detail = DetailSession::new(generation);
    // A watch value survives reconnects even when its change was already consumed.
    // Restore the user's current screen before the first RPC, not only on changed().
    detail.active = *detail_io.active.borrow_and_update();
    let file_reader = make_file_reader(config)?;
    publish_detail(detail_io, &mut detail);
    let mut next_id = 1_u64;
    let identify = send_and_wait(&mut socket, next_id, "server.connection.identify", Some(json!({"client_name":IDENTITY_NAME,"version":env!("CARGO_PKG_VERSION"),"type":"desktop","url":IDENTITY_URL})), &mut parser, state, sender, &mut detail, detail_io, shutdown, config.api_key.as_deref()).await?;
    next_id += 1;
    if let RpcReply::Error { code, message } = identify
        && code != Some(-32601)
    {
        return Err(rpc_error(code, message));
    }
    let server_info = send_and_wait(
        &mut socket,
        next_id,
        "server.info",
        None,
        &mut parser,
        state,
        sender,
        &mut detail,
        detail_io,
        shutdown,
        config.api_key.as_deref(),
    )
    .await?;
    next_id += 1;
    let RpcReply::Result(server_info) = server_info else {
        return Err(reply_error(server_info));
    };
    record_result_capabilities(&mut detail, "server.info", &server_info);
    detail.set_section(
        "server:server.info:base".to_owned(),
        SectionStatus::Ready,
        Some(redact(&server_info, config.api_key.as_deref())),
        None,
    );
    publish_detail(detail_io, &mut detail);
    if server_is_ready(&server_info) {
        let expanded = detail.active;
        discover_and_subscribe(
            &mut socket,
            &mut next_id,
            &mut parser,
            state,
            sender,
            &mut detail,
            detail_io,
            shutdown,
            config.api_key.as_deref(),
            expanded,
        )
        .await?;
    } else {
        publish_connection(state, sender, LanConnection::Connected)?;
        state.record_error(
            "Moonraker conectado, pero Klipper todavía no está listo para entregar telemetría",
        )?;
        sender.send_replace(state.clone());
    }
    event_loop(
        &mut socket,
        &mut next_id,
        &mut parser,
        state,
        sender,
        &mut detail,
        detail_io,
        operation_io,
        camera_io,
        shutdown,
        config.api_key.as_deref(),
        &file_reader,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn discover_and_subscribe(
    socket: &mut MoonrakerSocket,
    next_id: &mut u64,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    expanded: bool,
) -> Result<(), MoonrakerError> {
    let listed = send_and_wait(
        socket,
        *next_id,
        "printer.objects.list",
        None,
        parser,
        state,
        sender,
        detail,
        detail_io,
        shutdown,
        api_key,
    )
    .await?;
    *next_id += 1;
    let RpcReply::Result(listed) = listed else {
        return Err(reply_error(listed));
    };
    detail.objects = objects_from_list(&listed)?;
    if detail.objects.is_empty() {
        return Err(MoonrakerError::Incompatible);
    }
    detail.set_section(
        "objects".to_owned(),
        SectionStatus::Ready,
        Some(redact(&listed, api_key)),
        None,
    );
    publish_detail(detail_io, detail);
    let discovery_objects = ["heaters", "toolhead"]
        .into_iter()
        .filter(|name| detail.objects.iter().any(|object| object == *name))
        .map(|name| (name.to_owned(), Value::Null))
        .collect::<serde_json::Map<_, _>>();
    if !discovery_objects.is_empty() {
        let discovery = send_and_wait(
            socket,
            *next_id,
            "printer.objects.query",
            Some(json!({"objects":discovery_objects})),
            parser,
            state,
            sender,
            detail,
            detail_io,
            shutdown,
            api_key,
        )
        .await?;
        *next_id += 1;
        if let RpcReply::Result(discovery) = discovery {
            record_result_capabilities(detail, "printer.objects.query", &discovery);
        }
    }
    let subscribed = send_and_wait(
        socket,
        *next_id,
        "printer.objects.subscribe",
        Some(parser.subscriptions(&detail.objects, expanded)),
        parser,
        state,
        sender,
        detail,
        detail_io,
        shutdown,
        api_key,
    )
    .await?;
    *next_id += 1;
    let RpcReply::Result(subscribed) = subscribed else {
        return Err(reply_error(subscribed));
    };
    apply_subscription_base(
        &subscribed,
        parser,
        state,
        sender,
        detail,
        detail_io,
        api_key,
        expanded,
    )
}

#[allow(clippy::too_many_arguments)]
async fn event_loop(
    socket: &mut MoonrakerSocket,
    next_id: &mut u64,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    operation_io: &mut OperationIo,
    camera_io: &mut CameraIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    file_reader: &FileReader,
) -> Result<(), MoonrakerError> {
    let mut ping_interval = tokio::time::interval(Duration::from_secs(20));
    let mut history_offset = 0_u64;
    let mut history_next_scan = tokio::time::Instant::now();
    let mut camera_captures = JoinSet::new();
    let mut history_latest = None;
    if let Some(native) = &state.native_history {
        match native.handle.latest_native_start(&native.printer_id).await {
            Ok(latest) => history_latest = latest,
            Err(error) => {
                state.record_error(&error)?;
                sender.send_replace(state.clone());
            }
        }
    }
    let mut scan_latest = None;
    loop {
        tokio::select! {
            // The first detail request can arrive immediately after navigation.
            // Apply the activation first instead of rejecting that valid request.
            biased;
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return Ok(()); },
            changed = detail_io.active.changed() => {
                if changed.is_err() { return Ok(()); }
                let expand = *detail_io.active.borrow();
                if expand == detail.active { continue; }
                if expand {
                    detail.active = true;
                    detail.cache.reset(detail.generation);
                    detail.state = DetailState::empty(detail.generation);
                    detail.state.live = true;
                    publish_detail(detail_io, detail);
                    let server_info = send_and_wait(
                        socket,
                        *next_id,
                        "server.info",
                        None,
                        parser,
                        state,
                        sender,
                        detail,
                        detail_io,
                        shutdown,
                        api_key,
                    )
                    .await?;
                    *next_id += 1;
                    match server_info {
                        RpcReply::Result(server_info) => {
                            record_result_capabilities(detail, "server.info", &server_info);
                            detail.set_section(
                                "server:server.info:base".to_owned(),
                                SectionStatus::Ready,
                                Some(redact(&server_info, api_key)),
                                None,
                            );
                        }
                        RpcReply::Error { code, message } => {
                            let status = match code {
                                Some(-32601) => SectionStatus::Unsupported,
                                Some(401 | 403) => SectionStatus::Denied,
                                _ => SectionStatus::Error,
                            };
                            detail.set_section(
                                "server:server.info:base".to_owned(),
                                status,
                                None,
                                Some(sanitize_message(&message, 512)),
                            );
                        }
                    }
                    publish_detail(detail_io, detail);
                    discover_and_subscribe(socket, next_id, parser, state, sender, detail, detail_io, shutdown, api_key, true).await?;
                } else {
                    camera_captures.shutdown().await;
                    camera_io.state.send_replace(Arc::new(CameraState::empty()));
                    detail.clear();
                    publish_detail(detail_io, detail);
                    let subscribed = send_and_wait(socket, *next_id, "printer.objects.subscribe", Some(parser.subscriptions(&detail.objects, false)), parser, state, sender, detail, detail_io, shutdown, api_key).await?;
                    *next_id += 1;
                    let RpcReply::Result(subscribed) = subscribed else { return Err(reply_error(subscribed)); };
                    apply_subscription_base(&subscribed, parser, state, sender, detail, detail_io, api_key, false)?;
                }
            },
            request = detail_io.requests.recv() => {
                let Some(request) = request else { return Ok(()); };
                handle_detail_request(socket, next_id, parser, state, sender, detail, detail_io, shutdown, api_key, file_reader, operation_io, request).await?;
            },
            action = operation_io.actions.recv() => {
                let Some(action) = action else { return Ok(()); };
                handle_printer_action(socket, next_id, parser, state, sender, detail, detail_io, shutdown, api_key, file_reader, operation_io, action).await?;
            },
            camera_request = camera_io.requests.recv() => {
                let Some(camera_request) = camera_request else { return Ok(()); };
                handle_camera_request(file_reader, detail, camera_io, &mut camera_captures, camera_request).await;
            },
            capture = camera_captures.join_next(), if !camera_captures.is_empty() => {
                match capture {
                    Some(Ok(next)) => { camera_io.state.send_replace(Arc::new(next)); }
                    Some(Err(error)) if !error.is_cancelled() => {
                        let mut next = camera_io.state.borrow().as_ref().clone();
                        next.status = CameraStatus::Error;
                        next.message = Some("No se pudo obtener el snapshot de cámara".to_owned());
                        camera_io.state.send_replace(Arc::new(next));
                    }
                    _ => {}
                }
            },
            preview = detail_io.preview_results.recv() => {
                let Some(preview) = preview else { return Ok(()); };
                handle_preview_result(detail, detail_io, preview);
            },
            _ = ping_interval.tick() => {
                let _ = socket.send(Message::Ping(Vec::new().into())).await;
            },
            _ = tokio::time::sleep_until(history_next_scan), if state.native_history.is_some() => {
                let Some(native) = state.native_history.clone() else { continue; };
                let request_id = *next_id;
                send_json(socket, request_id, "server.history.list", Some(json!({"limit":50,"start":history_offset,"order":"desc"}))).await?;
                *next_id += 1;
                let result = match wait_for_detail_reply(socket, request_id, next_id, RpcPurpose::NativeHistory, parser, state, sender, detail, detail_io, shutdown, api_key, file_reader, operation_io).await {
                    Ok(result) => result,
                    Err(MoonrakerError::RequestTimeout) => {
                        history_next_scan = tokio::time::Instant::now() + Duration::from_secs(60);
                        state.record_error(MoonrakerError::RequestTimeout.to_string())?;
                        sender.send_replace(state.clone());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                match result {
                    RpcReply::Result(result) => {
                        let rows = result.get("jobs").and_then(Value::as_array);
                        if let Some(rows) = rows.filter(|rows| rows.len() <= 50) {
                            let mut reached_known = false;
                            let mut normalized = Vec::new();
                            for row in rows {
                                if let Some(record) = native_history_record(row, &native.printer_id, chrono::Utc::now()) {
                                    reached_known |= history_latest.is_some_and(|latest| record.started_at <= latest);
                                    scan_latest = Some(scan_latest.map_or(record.started_at, |latest: chrono::DateTime<chrono::Utc>| latest.max(record.started_at)));
                                    normalized.push(record);
                                }
                            }
                            if import_history_page(&native, normalized, &mut history_next_scan).await {
                                if rows.len() < 50 || reached_known {
                                    history_latest = scan_latest.or(history_latest);
                                    history_offset = 0;
                                    history_next_scan = tokio::time::Instant::now() + Duration::from_secs(60);
                                } else {
                                    history_offset = history_offset.saturating_add(rows.len() as u64);
                                    history_next_scan = tokio::time::Instant::now() + Duration::from_millis(500);
                                }
                            }
                        } else { history_next_scan = tokio::time::Instant::now() + Duration::from_secs(60); }
                    }
                    RpcReply::Error { .. } => { history_next_scan = tokio::time::Instant::now() + Duration::from_secs(60); }
                }
            },
            received = socket.next() => {
                let message = received.ok_or_else(|| MoonrakerError::Transport("Moonraker cerró la conexión".to_owned()))?.map_err(|error| MoonrakerError::Transport(error.to_string()))?;
                handle_socket_message(socket, message, parser, state, sender, detail, detail_io, api_key).await?;
            },
        }
    }
}

async fn import_history_page(
    native: &crate::history::NativeHistory,
    records: Vec<crate::shared::history::HistoryRecord>,
    next_scan: &mut tokio::time::Instant,
) -> bool {
    let committed = native.handle.import_committed(records).await;
    if !committed {
        *next_scan = tokio::time::Instant::now() + Duration::from_secs(60);
    }
    committed
}

fn native_history_record(
    row: &Value,
    printer_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Option<crate::shared::history::HistoryRecord> {
    use crate::shared::history::{HistoryRecord, HistorySource};
    use crate::shared::telemetry::JobTelemetry;
    let state = match row.get("status")?.as_str()? {
        "completed" => PrintStatus::Completed,
        "cancelled" => PrintStatus::Cancelled,
        "error" | "klippy_shutdown" => PrintStatus::Error,
        "klippy_disconnect" | "interrupted" => PrintStatus::Unknown,
        _ => return None,
    };
    let uid = row.get("job_id")?.as_str()?;
    let start = row.get("start_time")?.as_f64()?;
    let total = row.get("total_duration")?.as_f64()?;
    if uid.is_empty()
        || uid.len() > 64
        || !start.is_finite()
        || !total.is_finite()
        || start < 0.0
        || total < 0.0
    {
        return None;
    }
    let started_at = chrono::DateTime::from_timestamp_millis((start * 1000.0) as i64)?;
    let ended_at = started_at.checked_add_signed(chrono::Duration::milliseconds(
        (total * 1000.0).min(i64::MAX as f64) as i64,
    ))?;
    let job = JobTelemetry {
        print_name: row
            .get("filename")
            .and_then(Value::as_str)
            .map(crate::state::sanitize_filename),
        print_state: state,
        print_elapsed_time_s: row.get("print_duration").and_then(Value::as_f64),
        print_filament_used_mm: row.get("filament_used").and_then(Value::as_f64),
        ..JobTelemetry::default()
    };
    job.validate().ok()?;
    Some(HistoryRecord {
        printer_id: printer_id.to_owned(),
        job_uid: format!("{uid}@{start}"),
        source: HistorySource::Native,
        started_at,
        ended_at: Some(ended_at),
        first_observed_at: observed_at,
        last_observed_at: observed_at,
        incomplete: state == PrintStatus::Unknown,
        job,
        filament_supplies: None,
    })
}

fn handle_preview_result(detail: &mut DetailSession, detail_io: &DetailIo, preview: PreviewResult) {
    if !detail.active || detail.generation != preview.generation {
        return;
    }
    if file_section_token(&preview.key).is_some()
        && !detail.state.sections.contains_key(&preview.key)
    {
        return;
    }
    match preview.result {
        Ok(preview_data) => {
            let gcode_facts =
                (preview.root == "gcodes" && preview.offset == 0 && !preview_data.binary)
                    .then(|| {
                        preview_data.text.as_deref().map(|head| {
                            inspect_gcode_file_parts(head, preview_data.analysis_tail.as_deref())
                        })
                    })
                    .flatten()
                    .map(Arc::new);
            let mut data = json!({
                "root": preview.root,
                "path": preview.path,
                "offset": preview.offset,
                "content_type": preview_data.content_type,
                "size": preview_data.total_bytes,
                "next_offset": preview_data.next_offset,
                "binary": preview_data.binary,
            });
            if let Some(text) = preview_data.text {
                data["text"] = Value::String(text);
            }
            detail.set_section(preview.key.clone(), SectionStatus::Ready, Some(data), None);
            if let Some(entry) = detail.state.sections.get_mut(&preview.key) {
                entry.gcode_facts = gcode_facts;
                entry.thumbnail = preview.thumbnail;
            }
        }
        Err(error) => detail.set_section(
            preview.key,
            section_status_for_detail_error(&error),
            None,
            Some(error.to_string()),
        ),
    }
    publish_detail(detail_io, detail);
}

async fn handle_camera_request(
    file_reader: &FileReader,
    detail: &DetailSession,
    camera_io: &mut CameraIo,
    captures: &mut JoinSet<CameraState>,
    request: CameraRequest,
) {
    // A capture belongs to this session and the latest requested camera only.
    captures.shutdown().await;
    let CameraRequest::Load { uid } = request;
    let previous = CameraState::loading(uid.clone(), &camera_io.state.borrow());
    let Some(raw_url) = detail.camera_snapshots.get(&uid) else {
        let _ = camera_io.state.send_replace(Arc::new(CameraState {
            uid: Some(uid),
            status: CameraStatus::Unsupported,
            frame: previous.frame,
            received_at: previous.received_at,
            message: Some("Esta cámara no expone un snapshot descubierto".to_owned()),
        }));
        return;
    };
    let endpoint = match same_origin_snapshot_url(&file_reader.base, raw_url) {
        Ok(endpoint) => endpoint,
        Err(status) => {
            let _ = camera_io.state.send_replace(Arc::new(CameraState {
                uid: Some(uid),
                status,
                frame: previous.frame,
                received_at: previous.received_at,
                message: Some(
                    "Esta cámara usa otro origen; configura ese origen antes de mostrarla"
                        .to_owned(),
                ),
            }));
            return;
        }
    };
    let _ = camera_io
        .state
        .send_replace(Arc::new(CameraState::loading(uid.clone(), &previous)));
    let client = file_reader.client.clone();
    captures.spawn(async move {
        match fetch_snapshot(client, endpoint).await {
            Ok(frame) => CameraState {
                uid: Some(uid),
                status: CameraStatus::Ready,
                frame: Some(frame),
                received_at: Some(Instant::now()),
                message: None,
            },
            Err((status, message)) => CameraState {
                uid: Some(uid),
                status,
                frame: previous.frame,
                received_at: previous.received_at,
                message: Some(sanitize_message(&message, 256)),
            },
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn handle_printer_action(
    socket: &mut MoonrakerSocket,
    next_id: &mut u64,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    file_reader: &FileReader,
    operation_io: &mut OperationIo,
    action: PrinterAction,
) -> Result<(), MoonrakerError> {
    let kind = action.kind();
    if operation_io.state.borrow().is_pending() {
        let _ = operation_io.state.send_replace(OperationState::Rejected {
            action: kind,
            message: "Ya hay una operación de impresora en curso".to_owned(),
        });
        return Ok(());
    }
    if let Err(message) = action_allowed(&action, state) {
        let _ = operation_io.state.send_replace(OperationState::Rejected {
            action: kind,
            message,
        });
        return Ok(());
    }
    if let PrinterAction::UploadGcode {
        source,
        destination,
    } = action
    {
        return start_upload(file_reader, operation_io, source, destination);
    }
    let request = match runtime_control_request(*next_id, &action, detail, state) {
        Ok(request) => request,
        Err(error) => {
            let _ = operation_io.state.send_replace(OperationState::Rejected {
                action: kind,
                message: error.to_string(),
            });
            return Ok(());
        }
    };
    let _ = operation_io
        .state
        .send_replace(OperationState::Sending(kind));
    send_value(socket, &request).await?;
    let reply = wait_for_reply(
        socket,
        *next_id,
        parser,
        state,
        sender,
        detail,
        detail_io,
        shutdown,
        api_key,
        action_response_timeout(&action),
    )
    .await;
    *next_id += 1;
    match reply {
        Ok(RpcReply::Result(_)) => {
            let _ = operation_io
                .state
                .send_replace(OperationState::Accepted(kind));
            Ok(())
        }
        Ok(RpcReply::Error { message, .. }) => {
            let _ = operation_io.state.send_replace(OperationState::Rejected {
                action: kind,
                message: sanitize_message(&message, 256),
            });
            Ok(())
        }
        Err(error) => {
            let _ = operation_io
                .state
                .send_replace(OperationState::Unknown(kind));
            Err(error)
        }
    }
}

fn start_upload(
    file_reader: &FileReader,
    operation_io: &mut OperationIo,
    source: std::path::PathBuf,
    destination: String,
) -> Result<(), MoonrakerError> {
    let kind = crate::integrations::moonraker::local::PrinterActionKind::UploadGcode;
    let length = match validate_upload(&source, &destination) {
        Ok(length) => length,
        Err(error) => {
            let _ = operation_io.state.send_replace(OperationState::Rejected {
                action: kind,
                message: error.to_string(),
            });
            return Ok(());
        }
    };
    let _ = operation_io
        .state
        .send_replace(OperationState::Sending(kind));
    let client = file_reader.client.clone();
    let base = file_reader.base.clone();
    let result_sender = operation_io.state.clone();
    tokio::spawn(async move {
        let next_state = match upload_gcode(&client, &base, &source, &destination, length).await {
            Ok(()) => OperationState::Accepted(kind),
            Err(message) => OperationState::Rejected {
                action: kind,
                message,
            },
        };
        let _ = result_sender.send_replace(next_state);
    });
    Ok(())
}

async fn upload_gcode(
    client: &reqwest::Client,
    base: &str,
    source: &std::path::Path,
    destination: &str,
    length: u64,
) -> Result<(), String> {
    let file = File::open(source)
        .await
        .map_err(|_| "No se pudo abrir el archivo local seleccionado".to_owned())?;
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let part = reqwest::multipart::Part::stream_with_length(body, length)
        .file_name(destination.to_owned())
        .mime_str("text/x.gcode")
        .map_err(|_| "No se pudo preparar el G-code".to_owned())?;
    let form = reqwest::multipart::Form::new()
        .text("root", "gcodes")
        .text("print", "false")
        .part("file", part);
    let endpoint = Url::parse(base)
        .and_then(|url| url.join("/server/files/upload"))
        .map_err(|_| "La dirección local de Moonraker no es válida".to_owned())?;
    let response = timeout(UPLOAD_TIMEOUT, client.post(endpoint).multipart(form).send())
        .await
        .map_err(|_| "La subida superó el tiempo permitido".to_owned())?
        .map_err(|_| "La subida a Moonraker no se pudo completar".to_owned())?;
    if response.status() == reqwest::StatusCode::CREATED {
        return Ok(());
    }
    let status = response.status();
    let mut message = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Moonraker cerró la respuesta de subida".to_owned())?;
        let remaining = 512usize.saturating_sub(message.len());
        if remaining == 0 {
            break;
        }
        message.push_str(&String::from_utf8_lossy(
            &chunk[..chunk.len().min(remaining)],
        ));
    }
    let response_message = sanitize_message(&message, 512);
    if response_message.is_empty() {
        Err(format!("Moonraker rechazó la subida ({status})"))
    } else {
        Err(format!(
            "Moonraker rechazó la subida ({status}): {response_message}"
        ))
    }
}

fn runtime_control_request(
    id: u64,
    action: &PrinterAction,
    detail: &DetailSession,
    state: &PrinterState,
) -> Result<Value, local::ControlError> {
    match action {
        PrinterAction::MachineControl { control } if detail.active => {
            adapter::control::build_request(id, control, &detail.state, state)
                .map_err(|_| local::ControlError::Machine)
        }
        PrinterAction::MachineControl { .. } => Err(local::ControlError::Machine),
        _ => build_control_request(id, action),
    }
}

fn action_response_timeout(action: &PrinterAction) -> Duration {
    match action {
        PrinterAction::MachineControl { control } => control.response_timeout(),
        _ => REQUEST_TIMEOUT,
    }
}

fn action_allowed(action: &PrinterAction, state: &PrinterState) -> Result<(), String> {
    let status = state.telemetry.print_state;
    let allowed = match action {
        PrinterAction::MachineControl { .. } => true, // Revalidated against live detail at dispatch.
        PrinterAction::StartPrint { .. } => status == PrintStatus::Idle,
        PrinterAction::PausePrint(context) => state.permits_print_control(
            crate::shared::workspace::PrinterActionKind::PausePrint,
            context,
        ),
        PrinterAction::ResumePrint(context) => state.permits_print_control(
            crate::shared::workspace::PrinterActionKind::ResumePrint,
            context,
        ),
        PrinterAction::CancelPrint(context) => state.permits_print_control(
            crate::shared::workspace::PrinterActionKind::CancelPrint,
            context,
        ),
        PrinterAction::SetTemperatureTarget { .. } => {
            !matches!(status, PrintStatus::Printing | PrintStatus::Paused)
        }
        PrinterAction::SetLightOutput { .. } | PrinterAction::RunLightMacro { .. } => true,
        PrinterAction::UploadGcode { destination, .. } => !matches!(
            (status, state.telemetry.current_job.as_ref().and_then(|job| job.print_name.as_deref())),
            (PrintStatus::Printing | PrintStatus::Paused, Some(filename)) if filename == destination
        ),
    };
    if allowed {
        Ok(())
    } else {
        Err(crate::i18n::t!("printers.actions.state_changed").into_owned())
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_detail_request(
    socket: &mut MoonrakerSocket,
    next_id: &mut u64,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    file_reader: &FileReader,
    operation_io: &mut OperationIo,
    request: DetailRequest,
) -> Result<(), MoonrakerError> {
    let command_catalog = matches!(&request, DetailRequest::CommandCatalog { .. });
    let (key, method, params) = match request {
        DetailRequest::CommandCatalog { key } => {
            if detail.objects.iter().any(|name| name == "gcode") {
                (
                    key,
                    "printer.objects.query".to_owned(),
                    Some(json!({"objects":{"gcode":["commands"]}})),
                )
            } else {
                (key, "printer.gcode.help".to_owned(), None)
            }
        }
        DetailRequest::Rpc {
            key,
            method,
            params,
        } => (key, method, params),
        DetailRequest::PreviewFile {
            key,
            root,
            path,
            offset,
        } => {
            if !detail.active || !detail.roots.contains(&root) {
                detail.set_section(
                    key,
                    SectionStatus::Error,
                    None,
                    Some("Archivo o raíz no descubierto".to_owned()),
                );
                publish_detail(detail_io, detail);
                return Ok(());
            }
            detail.set_section(key.clone(), SectionStatus::Loading, None, None);
            publish_detail(detail_io, detail);
            let client = file_reader.client.clone();
            let base = file_reader.base.clone();
            let known_secrets = api_key.map_or_else(Vec::new, |api_key| vec![api_key.to_owned()]);
            let results = detail_io.preview_sender.clone();
            let generation = detail.generation;
            tokio::spawn(async move {
                let secret_refs = known_secrets.iter().map(String::as_str).collect::<Vec<_>>();
                let mut result = if root == "gcodes" && offset == 0 {
                    fetch_gcode_preview(&client, &base, &path, &secret_refs).await
                } else {
                    fetch_file_preview(&client, &base, &root, &path, offset, &secret_refs).await
                };
                let thumbnail_text = (root == "gcodes" && offset == 0)
                    .then(|| {
                        result
                            .as_ref()
                            .ok()
                            .and_then(|preview| preview.text.clone())
                    })
                    .flatten();
                let thumbnail = if let Some(text) = thumbnail_text {
                    match crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
                        crate::shared::thumbnail::embedded_thumbnail(&text)
                    })
                    .await
                    {
                        Ok(thumbnail) => thumbnail,
                        Err(_) => {
                            result = Err(DetailError::InvalidPayload);
                            None
                        }
                    }
                } else {
                    None
                };
                let _ = results
                    .send(PreviewResult {
                        generation,
                        key,
                        root,
                        path,
                        offset,
                        result,
                        thumbnail,
                    })
                    .await;
            });
            return Ok(());
        }
    };
    if !detail.active && method != "server.history.totals" {
        detail.set_section(
            key,
            SectionStatus::Error,
            None,
            Some("Abre el detalle antes de consultar esta sección".to_owned()),
        );
        publish_detail(detail_io, detail);
        return Ok(());
    }
    let request_json = match build_read_request(*next_id, &method, params.clone()) {
        Ok(request) if runtime_request_allowed(&method, params.as_ref(), detail) => request,
        _ => {
            detail.set_section(
                key,
                SectionStatus::Error,
                None,
                Some("Consulta no permitida para esta impresora".to_owned()),
            );
            publish_detail(detail_io, detail);
            return Ok(());
        }
    };
    detail.set_section(key.clone(), SectionStatus::Loading, None, None);
    publish_detail(detail_io, detail);
    let expected_id = *next_id;
    *next_id += 1;
    send_value(socket, &request_json).await?;
    let response = wait_for_detail_reply(
        socket,
        expected_id,
        next_id,
        RpcPurpose::Detail,
        parser,
        state,
        sender,
        detail,
        detail_io,
        shutdown,
        api_key,
        file_reader,
        operation_io,
    )
    .await;
    match response {
        Ok(RpcReply::Result(mut result)) => {
            if command_catalog && method == "printer.gcode.help" {
                let Some(help) = result
                    .as_object()
                    .filter(|help| help.values().all(Value::is_string))
                else {
                    detail.set_section(
                        key,
                        SectionStatus::Error,
                        None,
                        Some(DetailError::InvalidPayload.to_string()),
                    );
                    publish_detail(detail_io, detail);
                    return Ok(());
                };
                let mut commands: serde_json::Map<String, Value> = help
                    .iter()
                    .map(|(name, description)| (name.clone(), json!({"help":description})))
                    .collect();
                // Legacy Klipper omits standard commands from HELP and has no gcode status object.
                if ["toolhead", "gcode_move"]
                    .iter()
                    .all(|required| detail.objects.iter().any(|name| name == required))
                {
                    for name in ["G28", "G1", "G91", "M220", "M400"] {
                        commands.entry(name.to_owned()).or_insert_with(|| json!({}));
                    }
                    // Old Klipper registers motor commands without exposing a status object.
                    if detail.objects.iter().any(|name| name == "stepper_enable")
                        || commands.contains_key("SET_STEPPER_ENABLE")
                    {
                        commands
                            .entry("M18".to_owned())
                            .or_insert_with(|| json!({}));
                    }
                }
                result = json!({"status":{"gcode":{"commands":commands}}});
            }
            record_result_capabilities(detail, &method, &result);
            let enriched = if method == "server.files.list"
                && params
                    .as_ref()
                    .and_then(|p| p.get("root"))
                    .and_then(Value::as_str)
                    == Some("gcodes")
            {
                // Search and ordering use the lightweight catalogue before folder metadata finishes.
                detail.set_section(
                    key.clone(),
                    SectionStatus::Loading,
                    Some(redact(&result, api_key)),
                    None,
                );
                publish_detail(detail_io, detail);
                enrich_file_catalog(
                    socket,
                    next_id,
                    parser,
                    state,
                    sender,
                    detail,
                    detail_io,
                    shutdown,
                    api_key,
                    file_reader,
                    operation_io,
                    &mut result,
                )
                .await
            } else {
                Ok(SectionStatus::Ready)
            };
            match enriched {
                Ok(status) => detail.set_section(
                    key,
                    status,
                    Some(redact(&result, api_key)),
                    (status == SectionStatus::Limited).then(|| {
                        "Lectura parcial: faltan metadatos de algunas carpetas".to_owned()
                    }),
                ),
                Err(MoonrakerError::Cancelled) => {
                    detail.set_section(key, SectionStatus::NotLoaded, None, None)
                }
                Err(error) => detail.set_section(
                    key,
                    SectionStatus::Error,
                    None,
                    Some(sanitize_message(&error.to_string(), 512)),
                ),
            }
        }
        Ok(RpcReply::Error { code, message }) => {
            let status = match code {
                Some(-32601) => SectionStatus::Unsupported,
                Some(401 | 403) => SectionStatus::Denied,
                _ => SectionStatus::Error,
            };
            detail.set_section(key, status, None, Some(sanitize_message(&message, 512)));
        }
        Err(MoonrakerError::Cancelled) => {
            detail.set_section(key, SectionStatus::NotLoaded, None, None)
        }
        Err(error) => detail.set_section(
            key,
            SectionStatus::Error,
            None,
            Some(sanitize_message(&error.to_string(), 512)),
        ),
    }
    publish_detail(detail_io, detail);
    Ok(())
}

/// Enrich only the bounded recursive catalogue, using one read per folder.
/// No G-code, preview bytes, extra connection, background task or periodic scan.
#[allow(clippy::too_many_arguments)]
async fn enrich_file_catalog(
    socket: &mut MoonrakerSocket,
    next_id: &mut u64,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    file_reader: &FileReader,
    operation_io: &mut OperationIo,
    files: &mut Value,
) -> Result<SectionStatus, MoonrakerError> {
    let Some(entries) = files.as_array() else {
        return Ok(SectionStatus::Limited);
    };
    let mut limited = entries.len() > 10_000;
    let paths: BTreeMap<String, usize> = entries
        .iter()
        .take(10_000)
        .enumerate()
        .filter_map(|(index, file)| {
            let path = file.get("path")?.as_str()?;
            local::valid_gcode_filename(path).then(|| (path.to_owned(), index))
        })
        .collect();
    let directories = file_catalog_directories(paths.keys().map(String::as_str));
    limited |= directories.len() > FILE_METADATA_DIRECTORY_LIMIT;
    let started = Instant::now();
    for directory in directories.iter().take(FILE_METADATA_DIRECTORY_LIMIT) {
        if !*detail_io.active.borrow() || *shutdown.borrow() {
            return Err(MoonrakerError::Cancelled);
        }
        if started.elapsed() >= FILE_METADATA_BUDGET {
            limited = true;
            break;
        }
        let params = json!({"path":directory,"extended":true});
        let request = match build_read_request(
            *next_id,
            "server.files.get_directory",
            Some(params.clone()),
        ) {
            Ok(request)
                if runtime_request_allowed("server.files.get_directory", Some(&params), detail) =>
            {
                request
            }
            _ => {
                limited = true;
                break;
            }
        };
        let expected_id = *next_id;
        *next_id += 1;
        send_value(socket, &request).await?;
        match wait_for_detail_reply(
            socket,
            expected_id,
            next_id,
            RpcPurpose::Detail,
            parser,
            state,
            sender,
            detail,
            detail_io,
            shutdown,
            api_key,
            file_reader,
            operation_io,
        )
        .await
        {
            Ok(RpcReply::Result(result)) => {
                limited |= !apply_directory_estimates(files, directory, &result, &paths);
            }
            Ok(RpcReply::Error { .. }) => {
                limited = true;
                break;
            }
            Err(MoonrakerError::Cancelled) => return Err(MoonrakerError::Cancelled),
            Err(_) => {
                limited = true;
                break;
            }
        }
    }
    Ok(if limited {
        SectionStatus::Limited
    } else {
        SectionStatus::Ready
    })
}

fn file_catalog_directories<'a>(paths: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    paths
        .map(|path| {
            path.rsplit_once('/').map_or_else(
                || "gcodes".to_owned(),
                |(parent, _)| format!("gcodes/{parent}"),
            )
        })
        .collect()
}

fn apply_directory_estimates(
    files: &mut Value,
    directory: &str,
    metadata: &Value,
    paths: &BTreeMap<String, usize>,
) -> bool {
    let Some(entries) = metadata.get("files").and_then(Value::as_array) else {
        return false;
    };
    let parent = directory.strip_prefix("gcodes/").unwrap_or("");
    for entry in entries.iter().take(10_000) {
        let Some(filename) = entry
            .get("filename")
            .and_then(Value::as_str)
            .filter(|name| !name.contains(['/', '\\']))
        else {
            continue;
        };
        let path = if parent.is_empty() {
            filename.to_owned()
        } else {
            format!("{parent}/{filename}")
        };
        let Some(&index) = paths.get(&path) else {
            continue;
        };
        if let Some(seconds) = entry
            .get("estimated_time")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        {
            files[index]["estimated_time"] = json!(seconds);
        }
    }
    entries.len() <= 10_000
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_detail_reply(
    socket: &mut MoonrakerSocket,
    expected_id: u64,
    next_id: &mut u64,
    purpose: RpcPurpose,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    file_reader: &FileReader,
    operation_io: &mut OperationIo,
) -> Result<RpcReply, MoonrakerError> {
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let mut control_pending: Option<(u64, local::PrinterActionKind, tokio::time::Instant)> = None;
    let mut auxiliary_reply = None;
    loop {
        tokio::select! {
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return Err(MoonrakerError::Cancelled); },
            changed = detail_io.active.changed(), if purpose == RpcPurpose::Detail => {
                if changed.is_err() || *detail_io.active.borrow() != detail.active {
                    return Err(MoonrakerError::Cancelled);
                }
            },
            action = operation_io.actions.recv(), if control_pending.is_none() => {
                let Some(action) = action else { return Err(MoonrakerError::Cancelled); };
                let kind = action.kind();
                if operation_io.state.borrow().is_pending() {
                    let _ = operation_io.state.send_replace(OperationState::Rejected {
                        action: kind,
                        message: "Ya hay una operación de impresora en curso".to_owned(),
                    });
                    continue;
                }
                if let Err(message) = action_allowed(&action, state) {
                    let _ = operation_io.state.send_replace(OperationState::Rejected { action: kind, message });
                    continue;
                }
                if let PrinterAction::UploadGcode { source, destination } = action {
                    start_upload(file_reader, operation_io, source, destination)?;
                    continue;
                }
                let control_id = *next_id;
                let request = match runtime_control_request(control_id, &action, detail, state) {
                    Ok(request) => request,
                    Err(error) => {
                        let _ = operation_io.state.send_replace(OperationState::Rejected {
                            action: kind,
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                *next_id += 1;
                let _ = operation_io.state.send_replace(OperationState::Sending(kind));
                if let Err(error) = send_value(socket, &request).await {
                    let _ = operation_io.state.send_replace(OperationState::Unknown(kind));
                    return Err(error);
                }
                control_pending = Some((control_id, kind, tokio::time::Instant::now() + action_response_timeout(&action)));
            },
            received = timeout_at(control_pending.map_or(deadline, |(_, _, until)| until), socket.next()) => {
                let message = match received {
                    Err(_) => {
                        if let Some((_, kind, _)) = control_pending.take() {
                            let _ = operation_io.state.send_replace(OperationState::Unknown(kind));
                        }
                        if let Some(reply) = auxiliary_reply.take() { return Ok(reply); }
                        return Err(MoonrakerError::RequestTimeout);
                    }
                    Ok(None) => {
                        if let Some((_, kind, _)) = control_pending.take() {
                            let _ = operation_io.state.send_replace(OperationState::Unknown(kind));
                        }
                        return Err(MoonrakerError::Transport("Moonraker cerró la conexión".to_owned()));
                    }
                    Ok(Some(Err(error))) => {
                        if let Some((_, kind, _)) = control_pending.take() {
                            let _ = operation_io.state.send_replace(OperationState::Unknown(kind));
                        }
                        return Err(MoonrakerError::Transport(error.to_string()));
                    }
                    Ok(Some(Ok(message))) => message,
                };
                if let Message::Ping(payload) = &message {
                    socket.send(Message::Pong(payload.clone())).await.map_err(|error| MoonrakerError::Transport(error.to_string()))?;
                    continue;
                }
                let value = match message_to_json(message) {
                    Ok(Some(value)) => value,
                    Ok(None) => continue,
                    Err(error) => {
                        if let Some((_, kind, _)) = control_pending.take() {
                            operation_io.state.send_replace(OperationState::Unknown(kind));
                        }
                        return Err(error);
                    }
                };
                let id = value.get("id").and_then(Value::as_u64);
                if id == Some(expected_id) {
                    let reply = if let Some(error) = value.get("error") {
                        RpcReply::Error { code: error.get("code").and_then(Value::as_i64), message: sanitize_message(&error.to_string(), 512) }
                    } else {
                        RpcReply::Result(value.get("result").cloned().ok_or_else(|| MoonrakerError::InvalidMessage("respuesta sin result".to_owned()))?)
                    };
                    if control_pending.is_none() { return Ok(reply); }
                    auxiliary_reply = Some(reply);
                    continue;
                }
                if let Some((control_id, kind, _)) = control_pending
                    && id == Some(control_id)
                {
                    control_pending = None;
                    if let Some(error) = value.get("error") {
                        let _ = operation_io.state.send_replace(OperationState::Rejected {
                            action: kind,
                            message: sanitize_message(&error.to_string(), 256),
                        });
                    } else if value.get("result").is_some() {
                        let _ = operation_io.state.send_replace(OperationState::Accepted(kind));
                    } else {
                        let _ = operation_io.state.send_replace(OperationState::Unknown(kind));
                    }
                    if let Some(reply) = auxiliary_reply.take() { return Ok(reply); }
                    continue;
                }
                match apply_notification(&value, parser, state, sender, detail, detail_io, api_key)? {
                    SessionSignal::Continue => {}
                    SessionSignal::Reset => return Err(MoonrakerError::Transport("Klippy requiere una base nueva".to_owned())),
                    SessionSignal::KlippyReady => return Err(MoonrakerError::Transport("Klippy está listo; renovando base".to_owned())),
                }
            },
        }
    }
}

fn runtime_request_allowed(method: &str, params: Option<&Value>, detail: &DetailSession) -> bool {
    let Some(params) = params else {
        return true;
    };
    match method {
        "printer.objects.query" => params
            .get("objects")
            .and_then(Value::as_object)
            .is_some_and(|objects| objects.keys().all(|name| detail.objects.contains(name))),
        "server.files.list" => params
            .get("root")
            .and_then(Value::as_str)
            .is_some_and(|root| detail.roots.contains(root)),
        "server.files.get_directory" => params
            .get("path")
            .and_then(Value::as_str)
            .and_then(|path| path.split('/').next())
            .is_some_and(|root| detail.roots.contains(root)),
        "server.database.get_item" => params
            .get("namespace")
            .and_then(Value::as_str)
            .is_some_and(|namespace| detail.namespaces.contains(namespace)),
        "machine.device_power.get_device" => params
            .get("device")
            .and_then(Value::as_str)
            .is_some_and(|device| detail.devices.contains(device)),
        "server.sensors.info" | "server.sensors.measurements" => params
            .get("sensor")
            .and_then(Value::as_str)
            .is_some_and(|sensor| detail.sensors.contains(sensor)),
        "machine.wled.get_strip" => params
            .get("strip")
            .and_then(Value::as_str)
            .is_some_and(|strip| detail.wled_strips.contains(strip)),
        "machine.wled.status" => params.as_object().is_some_and(|strips| {
            strips
                .keys()
                .all(|strip| detail.wled_strips.contains(strip))
        }),
        "server.webcams.get_item" => params
            .get("uid")
            .and_then(Value::as_str)
            .is_some_and(|uid| detail.cameras.contains(uid)),
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_and_wait(
    socket: &mut MoonrakerSocket,
    id: u64,
    method: &str,
    params: Option<Value>,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
) -> Result<RpcReply, MoonrakerError> {
    send_json(socket, id, method, params).await?;
    wait_for_reply(
        socket,
        id,
        parser,
        state,
        sender,
        detail,
        detail_io,
        shutdown,
        api_key,
        REQUEST_TIMEOUT,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_reply(
    socket: &mut MoonrakerSocket,
    expected_id: u64,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    shutdown: &mut watch::Receiver<bool>,
    api_key: Option<&str>,
    response_timeout: Duration,
) -> Result<RpcReply, MoonrakerError> {
    let deadline = tokio::time::Instant::now() + response_timeout;
    loop {
        tokio::select! {
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return Err(MoonrakerError::Cancelled); },
            received = timeout_at(deadline, socket.next()) => {
                let message = received.map_err(|_| MoonrakerError::Transport("timeout de Moonraker".to_owned()))?
                    .ok_or_else(|| MoonrakerError::Transport("Moonraker cerró la conexión".to_owned()))?
                    .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
                if let Message::Ping(payload) = &message {
                    socket.send(Message::Pong(payload.clone())).await.map_err(|error| MoonrakerError::Transport(error.to_string()))?;
                    continue;
                }
                let Some(value) = message_to_json(message)? else { continue; };
                if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
                    if let Some(error) = value.get("error") {
                        return Ok(RpcReply::Error { code: error.get("code").and_then(Value::as_i64), message: sanitize_message(&error.to_string(), 512) });
                    }
                    return Ok(RpcReply::Result(value.get("result").cloned().ok_or_else(|| MoonrakerError::InvalidMessage("respuesta sin result".to_owned()))?));
                }
                match apply_notification(&value, parser, state, sender, detail, detail_io, api_key)? {
                    SessionSignal::Continue => {}
                    SessionSignal::Reset => return Err(MoonrakerError::Transport("Klippy requiere una base nueva".to_owned())),
                    SessionSignal::KlippyReady => return Err(MoonrakerError::Transport("Klippy está listo; renovando base".to_owned())),
                }
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket_message(
    socket: &mut MoonrakerSocket,
    message: Message,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    api_key: Option<&str>,
) -> Result<(), MoonrakerError> {
    if let Message::Ping(payload) = &message {
        socket
            .send(Message::Pong(payload.clone()))
            .await
            .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
        return Ok(());
    }
    let Some(value) = message_to_json(message)? else {
        return Ok(());
    };
    match apply_notification(&value, parser, state, sender, detail, detail_io, api_key)? {
        SessionSignal::Continue => Ok(()),
        SessionSignal::Reset => Err(MoonrakerError::Transport(
            "Klippy requiere una base nueva".to_owned(),
        )),
        SessionSignal::KlippyReady => Err(MoonrakerError::Transport(
            "Klippy está listo; renovando base".to_owned(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_notification(
    value: &Value,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    api_key: Option<&str>,
) -> Result<SessionSignal, MoonrakerError> {
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return Ok(SessionSignal::Continue);
    };
    if matches!(
        method,
        "notify_klippy_disconnected" | "notify_klippy_shutdown"
    ) {
        parser.reset();
        detail.clear();
        publish_detail(detail_io, detail);
        return Ok(SessionSignal::Reset);
    }
    if method == "notify_klippy_ready" {
        return Ok(SessionSignal::KlippyReady);
    }
    if method != "notify_status_update" {
        return Ok(SessionSignal::Continue);
    }
    let params = value
        .get("params")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MoonrakerError::InvalidMessage("notify_status_update sin params".to_owned())
        })?;
    let status = params.first().ok_or_else(|| {
        MoonrakerError::InvalidMessage("notify_status_update sin estado".to_owned())
    })?;
    let eventtime = params.get(1).and_then(Value::as_f64).ok_or_else(|| {
        MoonrakerError::InvalidMessage("notify_status_update sin eventtime".to_owned())
    })?;
    match parser.apply_delta(status, eventtime)? {
        ParserEvent::Snapshot(snapshot) => {
            state.publish_snapshot(snapshot, Instant::now())?;
            sender
                .send(state.clone())
                .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
        }
        ParserEvent::Reset => {
            if detail.pending_deltas.len() < 8 {
                detail.pending_deltas.push((status.clone(), eventtime));
            } else {
                detail.set_section(
                    "telemetry".to_owned(),
                    SectionStatus::Limited,
                    None,
                    Some("Se excedió la cola de eventos antes de la base".to_owned()),
                );
                publish_detail(detail_io, detail);
            }
            return Ok(SessionSignal::Continue);
        }
        ParserEvent::Ignored => {}
    }
    if detail.active {
        let safe = redact(status, api_key);
        match detail
            .cache
            .apply_delta(detail.generation, eventtime, &safe)
        {
            Ok(true) => {
                detail.state.objects = detail.cache.objects().clone();
                detail.state.live = true;
            }
            Ok(false) => {}
            Err(error) => detail.set_section(
                "telemetry".to_owned(),
                section_status_for_detail_error(&error),
                None,
                Some(error.to_string()),
            ),
        }
        publish_detail(detail_io, detail);
    }
    Ok(SessionSignal::Continue)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Aplica una base al estado resumido y al detalle sin crear una capa de servicio adicional."
)]
fn apply_subscription_base(
    response: &Value,
    parser: &mut StatusParser,
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    detail: &mut DetailSession,
    detail_io: &mut DetailIo,
    api_key: Option<&str>,
    expanded: bool,
) -> Result<(), MoonrakerError> {
    let eventtime = response
        .get("eventtime")
        .and_then(Value::as_f64)
        .ok_or_else(|| MoonrakerError::InvalidMessage("suscripción sin eventtime".to_owned()))?;
    let status = response
        .get("status")
        .ok_or_else(|| MoonrakerError::InvalidMessage("suscripción sin estado".to_owned()))?;
    let snapshot = parser.apply_base(status, eventtime)?;
    state.publish_snapshot(snapshot, Instant::now())?;
    sender
        .send(state.clone())
        .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
    if expanded {
        detail.cache.reset(detail.generation);
        let safe = redact(status, api_key);
        match detail
            .cache
            .replace_base(detail.generation, eventtime, &safe)
        {
            Ok(true) => {
                detail.state.objects = detail.cache.objects().clone();
                detail.state.live = true;
                detail.set_section("telemetry".to_owned(), SectionStatus::Ready, None, None);
            }
            Ok(false) => {}
            Err(error) => detail.set_section(
                "telemetry".to_owned(),
                section_status_for_detail_error(&error),
                None,
                Some(error.to_string()),
            ),
        }
    }
    let delayed = std::mem::take(&mut detail.pending_deltas);
    for (delta, eventtime) in delayed {
        match parser.apply_delta(&delta, eventtime)? {
            ParserEvent::Snapshot(snapshot) => {
                state.publish_snapshot(snapshot, Instant::now())?;
                sender
                    .send(state.clone())
                    .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
            }
            ParserEvent::Reset => {
                return Err(MoonrakerError::Transport("deltas sin base".to_owned()));
            }
            ParserEvent::Ignored => {}
        }
        if expanded {
            let safe = redact(&delta, api_key);
            if detail
                .cache
                .apply_delta(detail.generation, eventtime, &safe)
                .is_ok()
            {
                detail.state.objects = detail.cache.objects().clone();
            }
        }
    }
    publish_detail(detail_io, detail);
    Ok(())
}

fn section_status_for_detail_error(error: &DetailError) -> SectionStatus {
    match error {
        DetailError::PayloadLimit | DetailError::DepthLimit => SectionStatus::Limited,
        _ => SectionStatus::Error,
    }
}

fn objects_from_list(value: &Value) -> Result<Vec<String>, MoonrakerError> {
    let objects = value
        .get("objects")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MoonrakerError::InvalidMessage("printer.objects.list sin objects".to_owned())
        })?;
    Ok(objects
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        .map(ToOwned::to_owned)
        .collect())
}

fn server_is_ready(server_info: &Value) -> bool {
    server_info
        .get("klippy_connected")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && server_info
            .get("klippy_state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("ready"))
}

fn record_result_capabilities(detail: &mut DetailSession, method: &str, result: &Value) {
    let record_names = |target: &mut BTreeSet<String>, key: &str| {
        if let Some(values) = result.get(key).and_then(Value::as_array) {
            for value in values {
                if let Some(name) = value.as_str() {
                    target.insert(name.to_owned());
                }
                if let Some(name) = value.get("name").and_then(Value::as_str) {
                    target.insert(name.to_owned());
                }
                if let Some(name) = value.get("device").and_then(Value::as_str) {
                    target.insert(name.to_owned());
                }
            }
        }
    };
    match method {
        "server.info" => record_names(&mut detail.roots, "registered_directories"),
        "server.files.roots" => {
            if let Some(roots) = result.as_array() {
                for root in roots {
                    if let Some(name) = root.get("name").and_then(Value::as_str) {
                        detail.roots.insert(name.to_owned());
                    }
                }
            } else {
                record_names(&mut detail.roots, "roots");
            }
        }
        "server.database.list" => record_names(&mut detail.namespaces, "namespaces"),
        "machine.device_power.devices" => record_names(&mut detail.devices, "devices"),
        "server.sensors.list" => record_names(&mut detail.sensors, "sensors"),
        "machine.wled.strips" => record_names(&mut detail.wled_strips, "strips"),
        "server.webcams.list" | "server.webcams.get_item" => {
            if let Some(cameras) = result.get("webcams").and_then(Value::as_array) {
                for camera in cameras {
                    record_camera_capability(detail, camera);
                }
            }
            if result.get("uid").is_some() {
                record_camera_capability(detail, result);
            }
            if let Some(camera) = result.get("webcam") {
                record_camera_capability(detail, camera);
            }
        }
        _ => {}
    }
}

fn record_camera_capability(detail: &mut DetailSession, camera: &Value) {
    let Some(uid) = camera.get("uid").and_then(Value::as_str) else {
        return;
    };
    detail.cameras.insert(uid.to_owned());
    if let Some(snapshot_url) = camera.get("snapshot_url").and_then(Value::as_str)
        && !snapshot_url.is_empty()
        && snapshot_url.len() <= 2_048
        && !snapshot_url.chars().any(char::is_control)
    {
        detail
            .camera_snapshots
            .insert(uid.to_owned(), snapshot_url.to_owned());
    }
}

fn redact(value: &Value, api_key: Option<&str>) -> Value {
    match api_key {
        Some(api_key) => redact_json(value, &[api_key]),
        None => redact_json(value, &[]),
    }
}

fn publish_detail(io: &DetailIo, detail: &mut DetailSession) {
    detail.state.revision = detail.state.revision.saturating_add(1);
    let _ = io.state.send_replace(Arc::new(DetailState {
        generation: detail.state.generation,
        revision: detail.state.revision,
        live: detail.state.live,
        objects: detail.state.objects.clone(),
        sections: detail.state.sections.clone(),
    }));
}

fn handshake_request(
    config: &DriverConfig,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, MoonrakerError> {
    let scheme = if config.connection.tls { "wss" } else { "ws" };
    let url = Url::parse(&format!(
        "{scheme}://{}:{}/websocket",
        config.connection.host, config.connection.port
    ))
    .map_err(|_| MoonrakerError::Transport("URL Moonraker inválida".to_owned()))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
    if let Some(api_key) = &config.api_key {
        request.headers_mut().insert(
            "X-Api-Key",
            HeaderValue::from_str(api_key)
                .map_err(|error| MoonrakerError::Transport(error.to_string()))?,
        );
    } else if matches!(config.connection.auth, MoonrakerAuth::ApiKey { .. }) {
        return Err(MoonrakerError::Transport(
            "API key Moonraker no disponible".to_owned(),
        ));
    }
    Ok(request)
}

fn make_file_reader(config: &DriverConfig) -> Result<FileReader, MoonrakerError> {
    let scheme = if config.connection.tls {
        "https"
    } else {
        "http"
    };
    let base = format!(
        "{scheme}://{}:{}",
        config.connection.host, config.connection.port
    );
    Url::parse(&base)
        .map_err(|_| MoonrakerError::Transport("URL HTTP Moonraker inválida".to_owned()))?;
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(api_key) = &config.api_key {
        let value = reqwest::header::HeaderValue::from_str(api_key)
            .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
        headers.insert("X-Api-Key", value);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(headers)
        .build()
        .map_err(|error| MoonrakerError::Transport(error.to_string()))?;
    Ok(FileReader { client, base })
}

async fn send_json(
    socket: &mut MoonrakerSocket,
    id: u64,
    method: &str,
    params: Option<Value>,
) -> Result<(), MoonrakerError> {
    let mut request = json!({"jsonrpc":"2.0","id":id,"method":method});
    if let Some(params) = params {
        request["params"] = params;
    }
    send_value(socket, &request).await
}

async fn send_value(socket: &mut MoonrakerSocket, request: &Value) -> Result<(), MoonrakerError> {
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(|error| MoonrakerError::Transport(error.to_string()))
}

fn message_to_json(message: Message) -> Result<Option<Value>, MoonrakerError> {
    match message {
        Message::Text(text) => {
            if text.len() > MAX_MESSAGE_BYTES {
                return Err(MoonrakerError::Transport(
                    "mensaje Moonraker supera 4 MiB".to_owned(),
                ));
            }
            serde_json::from_str(&text)
                .map(Some)
                .map_err(|error| MoonrakerError::InvalidMessage(error.to_string()))
        }
        Message::Binary(binary) => {
            if binary.len() > MAX_MESSAGE_BYTES {
                return Err(MoonrakerError::Transport(
                    "mensaje Moonraker supera 4 MiB".to_owned(),
                ));
            }
            Err(MoonrakerError::InvalidMessage(
                "Moonraker envió un mensaje binario".to_owned(),
            ))
        }
        Message::Close(_) => Err(MoonrakerError::Transport(
            "Moonraker cerró la conexión".to_owned(),
        )),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(None),
    }
}

fn validate_eventtime(eventtime: f64) -> Result<(), MoonrakerError> {
    if !eventtime.is_finite() || eventtime < 0.0 {
        return Err(MoonrakerError::InvalidMessage(
            "eventtime inválido".to_owned(),
        ));
    }
    Ok(())
}
fn rpc_error(code: Option<i64>, message: String) -> MoonrakerError {
    MoonrakerError::Transport(code.map_or(message.clone(), |code| format!("RPC {code}: {message}")))
}
fn reply_error(reply: RpcReply) -> MoonrakerError {
    match reply {
        RpcReply::Error { code, message } => rpc_error(code, message),
        RpcReply::Result(_) => MoonrakerError::InvalidMessage("respuesta inesperada".to_owned()),
    }
}

fn parse_leading_u32(s: &str) -> Option<(u32, &str)> {
    let s = s.trim_start();
    let digit_len = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len == 0 {
        return None;
    }
    let (digits, rest) = s.split_at(digit_len);
    digits.parse::<u32>().ok().map(|val| (val, rest))
}

fn extract_layers_from_message(msg: &str) -> (Option<u32>, Option<u32>) {
    let lower = msg.to_ascii_lowercase();
    let lower = lower.trim();
    if lower.is_empty() {
        return (None, None);
    }

    for prefix in &["layer", "capa"] {
        if let Some(pos) = lower.find(prefix) {
            let after = &lower[pos + prefix.len()..];
            let after = after.trim_start_matches(|c: char| {
                c == ':' || c == '#' || c.is_whitespace() || c == '['
            });
            if let Some((cur, rest)) = parse_leading_u32(after) {
                let rest = rest.trim_start();
                let sep = if rest.starts_with('/') {
                    Some(1)
                } else if rest.starts_with("of ") || rest.starts_with("de ") {
                    Some(3)
                } else {
                    None
                };
                if let Some(sep_len) = sep {
                    let after_sep = rest[sep_len..].trim_start().trim_end_matches(']');
                    if let Some((tot, _)) = parse_leading_u32(after_sep)
                        && cur <= tot
                        && tot > 0
                    {
                        return (Some(cur), Some(tot));
                    }
                }
                return (Some(cur), None);
            }
        }
    }

    for word in lower.split_whitespace() {
        let trimmed =
            word.trim_matches(|c: char| c == '[' || c == ']' || c == '(' || c == ')' || c == ':');
        if let Some((left, right)) = trimmed.split_once('/')
            && let (Ok(cur), Ok(tot)) = (left.trim().parse::<u32>(), right.trim().parse::<u32>())
            && cur <= tot
            && tot > 0
            && tot <= 50_000
        {
            return (Some(cur), Some(tot));
        }
    }

    (None, None)
}

#[allow(clippy::collapsible_if)]
fn apply_fields(
    current: &Snapshot,
    status: &Value,
    base: bool,
    active_extruder: Option<&str>,
    light_bindings: &MoonrakerLightBindings,
) -> Result<(Snapshot, bool), MoonrakerError> {
    let mut next = current.clone();
    let mut recognized = false;
    let prior_status = next.status;
    let print_stats = status.get("print_stats").and_then(Value::as_object);
    let incoming_status = print_stats
        .and_then(|object| object.get("state"))
        .map(parse_status)
        .transpose()?;
    let incoming_filename = print_stats
        .and_then(|object| object.get("filename"))
        .map(parse_optional_string)
        .transpose()?;
    if incoming_filename.as_ref().is_some_and(|value| {
        value.as_deref() != current.filename.as_deref()
            && !value.as_deref().unwrap_or_default().is_empty()
    }) || matches!(
        prior_status,
        Some(
            PrintStatus::Idle
                | PrintStatus::Completed
                | PrintStatus::Cancelled
                | PrintStatus::Error
        )
    ) && incoming_status == Some(Some(PrintStatus::Printing))
    {
        next.progress_pct = None;
        next.layer_current = None;
        next.layer_total = None;
        next.elapsed_s = None;
        next.remaining_s = None;
        next.filament_used_mm = None;
        next.velocity_mm_s = None;
        next.speed_override_pct = None;
        next.alerts = None;
    }
    if let Some(status) = incoming_status {
        next.status = status.or(Some(PrintStatus::Unknown));
        recognized = true;
    }
    if let Some(filename) = incoming_filename {
        next.filename = filename;
        recognized = true;
    }
    if let Some(duration) = print_stats
        .and_then(|object| object.get("print_duration"))
        .map(parse_optional_nonnegative)
        .transpose()?
    {
        next.elapsed_s = duration;
        recognized = true;
    }
    if let Some(filament) = print_stats
        .and_then(|object| object.get("filament_used"))
        .map(parse_optional_nonnegative)
        .transpose()?
    {
        next.filament_used_mm = filament;
        recognized = true;
    }
    let incoming_message = print_stats
        .and_then(|object| object.get("message"))
        .map(parse_optional_string)
        .transpose()?;
    let display_message = status
        .get("display_status")
        .and_then(Value::as_object)
        .and_then(|object| object.get("message"))
        .map(parse_optional_string)
        .transpose()?;

    if matches!(
        next.status,
        Some(
            PrintStatus::Idle
                | PrintStatus::Printing
                | PrintStatus::Paused
                | PrintStatus::Completed
                | PrintStatus::Cancelled
        )
    ) && (incoming_status.is_some() || base)
    {
        next.alerts = Some(Vec::new());
    } else if next.status == Some(PrintStatus::Unknown) || next.status.is_none() {
        next.alerts = None;
    } else if next.status == Some(PrintStatus::Error) {
        if let Some(msg_opt) = incoming_message.as_ref().or(display_message.as_ref()) {
            let msg = msg_opt
                .clone()
                .unwrap_or_else(|| "Fallo confirmado de trabajo".to_owned());
            let alert = PrinterAlert::new("print_error", sanitize_message(&msg, 512));
            next.alerts = Some(vec![alert]);
            recognized = true;
        } else if next.alerts.as_ref().is_none_or(|a| a.is_empty()) {
            let alert = PrinterAlert::new("print_error", "Fallo confirmado de trabajo".to_owned());
            next.alerts = Some(vec![alert]);
        }
    }
    let mut info_cleared_layers = false;
    let mut current_updated_by_info = false;
    let mut total_updated_by_info = false;
    if let Some(info) = print_stats.and_then(|object| object.get("info")) {
        if info.is_null() {
            next.layer_current = None;
            next.layer_total = None;
            info_cleared_layers = true;
            recognized = true;
        } else {
            let info = info.as_object().ok_or_else(|| {
                MoonrakerError::InvalidMessage("print_stats.info no es objeto ni null".to_owned())
            })?;
            for &k in &[
                "current_layer",
                "current",
                "layer",
                "layer_num",
                "currentLayer",
            ] {
                if let Some(layer) = info.get(k).map(parse_optional_u32).transpose()? {
                    next.layer_current = layer;
                    current_updated_by_info = true;
                    recognized = true;
                    break;
                }
            }
            for &k in &[
                "total_layer",
                "total",
                "total_layers",
                "layer_count",
                "totalLayer",
            ] {
                if let Some(layer) = info.get(k).map(parse_optional_u32).transpose()? {
                    next.layer_total = layer;
                    total_updated_by_info = true;
                    recognized = true;
                    break;
                }
            }
        }
    }
    if !info_cleared_layers {
        let mut current_updated_by_ps = false;
        let mut total_updated_by_ps = false;
        if let Some(ps) = print_stats {
            if !current_updated_by_info {
                for &k in &["current_layer", "layer"] {
                    if let Some(layer) = ps.get(k).map(parse_optional_u32).transpose()? {
                        next.layer_current = layer;
                        current_updated_by_ps = true;
                        recognized = true;
                        break;
                    }
                }
            }
            if !total_updated_by_info {
                for &k in &["total_layer", "total_layers", "layer_count"] {
                    if let Some(layer) = ps.get(k).map(parse_optional_u32).transpose()? {
                        next.layer_total = layer;
                        total_updated_by_ps = true;
                        recognized = true;
                        break;
                    }
                }
            }
        }

        let active_message = incoming_message
            .as_ref()
            .and_then(|o| o.as_deref())
            .or_else(|| display_message.as_ref().and_then(|o| o.as_deref()));
        if let Some(msg) = active_message {
            let (msg_cur, msg_tot) = extract_layers_from_message(msg);
            if !current_updated_by_info
                && !current_updated_by_ps
                && let Some(cur) = msg_cur
            {
                next.layer_current = Some(cur);
                recognized = true;
            }
            if !total_updated_by_info
                && !total_updated_by_ps
                && let Some(tot) = msg_tot
            {
                next.layer_total = Some(tot);
                recognized = true;
            }
        }
    }
    if let Some(progress) = status
        .get("virtual_sdcard")
        .and_then(Value::as_object)
        .and_then(|object| object.get("progress"))
        .or_else(|| {
            status
                .get("display_status")
                .and_then(Value::as_object)
                .and_then(|object| object.get("progress"))
        })
        .map(parse_optional_progress)
        .transpose()?
    {
        next.progress_pct = progress;
        recognized = true;
    }
    if let Some(velocity) = status
        .get("motion_report")
        .and_then(Value::as_object)
        .and_then(|object| object.get("live_velocity"))
        .map(parse_optional_nonnegative)
        .transpose()?
    {
        next.velocity_mm_s = velocity;
        recognized = true;
    }
    let nozzle_object = active_extruder.unwrap_or("extruder");
    if let Some(status_obj) = status.as_object() {
        for (key, val) in status_obj {
            if key == "extruder"
                || key.strip_prefix("extruder").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                if let Some(obj) = val.as_object() {
                    let temp = obj
                        .get("temperature")
                        .map(parse_optional_finite)
                        .transpose()?;
                    let target = obj.get("target").map(parse_optional_finite).transpose()?;
                    let id_num = if key == "extruder" {
                        1
                    } else {
                        key["extruder".len()..]
                            .parse::<u32>()
                            .unwrap_or(0)
                            .saturating_add(1)
                    };
                    let is_active = key == nozzle_object;
                    if let Some(existing) = next.nozzles.iter_mut().find(|n| n.id.0 == id_num) {
                        existing.is_active = is_active;
                        if let Some(t) = temp {
                            existing.nozzle_temp_c = t;
                        }
                        if let Some(tgt) = target {
                            existing.nozzle_target_temp_c = tgt;
                        }
                    } else {
                        next.nozzles.push(NozzleTelemetry {
                            id: ElementId(id_num),
                            is_active,
                            nozzle_temp_c: temp.flatten(),
                            nozzle_target_temp_c: target.flatten(),
                        });
                    }
                    recognized = true;
                }
            }
        }
    }

    for (role, id, binding) in [
        (LightRole::BuildArea, 1, light_bindings.build_area.as_ref()),
        (LightRole::Toolhead, 2, light_bindings.toolhead.as_ref()),
    ] {
        let Some(binding) = binding else { continue };
        let enabled = match binding {
            MoonrakerLightBinding::ToggleMacro { .. } => None,
            MoonrakerLightBinding::OutputPin { name } => {
                let object_name = format!("output_pin {name}");
                if !base
                    && status
                        .get(&object_name)
                        .is_none_or(|object| object.is_object() && object.get("value").is_none())
                {
                    continue;
                }
                status
                    .get(&object_name)
                    .and_then(Value::as_object)
                    .and_then(|object| object.get("value"))
                    .map(parse_optional_progress)
                    .transpose()?
                    .flatten()
                    .map(|value| value > 0.0)
            }
        };
        if let Some(light) = next.lights.iter_mut().find(|light| light.role == role) {
            light.enabled = enabled;
        } else {
            next.lights.push(LightTelemetry {
                id: ElementId(id),
                role,
                enabled,
            });
        }
        recognized = true;
    }
    for nozzle in &mut next.nozzles {
        let nozzle_name = if nozzle.id.0 == 1 {
            "extruder".to_owned()
        } else {
            format!("extruder{}", nozzle.id.0.saturating_sub(1))
        };
        nozzle.is_active = nozzle_name == nozzle_object;
    }

    if status.get("heater_bed").is_some_and(Value::is_null) {
        if let Some(bed) = next.beds.first_mut() {
            bed.bed_temp_c = None;
            bed.bed_target_temp_c = None;
        }
        recognized = true;
    } else if let Some(heater_bed_val) = status.get("heater_bed").and_then(Value::as_object) {
        let temperature = heater_bed_val
            .get("temperature")
            .map(parse_optional_finite)
            .transpose()?;
        let target = heater_bed_val
            .get("target")
            .map(parse_optional_finite)
            .transpose()?;
        if temperature.is_some() || target.is_some() {
            recognized = true;
            if let Some(bed) = next.beds.first_mut() {
                if let Some(value) = temperature {
                    bed.bed_temp_c = value;
                }
                if let Some(value) = target {
                    bed.bed_target_temp_c = value;
                }
            } else if temperature.flatten().is_some() || target.flatten().is_some() {
                next.beds.push(BedTelemetry {
                    id: ElementId(1),
                    bed_temp_c: temperature.flatten(),
                    bed_target_temp_c: target.flatten(),
                });
            }
        }
    }

    if status.get("fan").is_some_and(Value::is_null) {
        if let Some(fan) = next.part_cooling_fans.first_mut() {
            fan.power_pct = None;
            fan.rpm = None;
        }
        recognized = true;
    } else if let Some(fan_obj) = status.get("fan").and_then(Value::as_object) {
        let speed = fan_obj
            .get("speed")
            .map(parse_optional_progress)
            .transpose()?;
        let rpm = fan_obj
            .get("rpm")
            .map(parse_optional_nonnegative)
            .transpose()?;
        if speed.is_some() || rpm.is_some() {
            recognized = true;
            if let Some(fan) = next.part_cooling_fans.first_mut() {
                if let Some(value) = speed {
                    fan.power_pct = value;
                }
                if let Some(value) = rpm {
                    fan.rpm = value;
                }
            } else if speed.flatten().is_some() || rpm.flatten().is_some() {
                next.part_cooling_fans = vec![PartCoolingFanTelemetry {
                    id: ElementId(1),
                    power_pct: speed.flatten(),
                    rpm: rpm.flatten(),
                }];
            }
        }
    }

    if let Some(speed_factor) = status
        .get("gcode_move")
        .and_then(Value::as_object)
        .and_then(|o| o.get("speed_factor"))
        .map(parse_optional_speed_factor)
        .transpose()?
    {
        next.speed_override_pct = speed_factor;
        recognized = true;
    }

    if let Some(status_obj) = status.as_object() {
        for (key, val) in status_obj {
            if key.starts_with("filament_switch_sensor ")
                || key.starts_with("filament_motion_sensor ")
            {
                if let Some(sensor_obj) = val.as_object() {
                    if let Some(detected) = sensor_obj
                        .get("filament_detected")
                        .map(parse_optional_bool)
                        .transpose()?
                    {
                        recognized = true;
                        if let Some(sensor) = next
                            .filament_sensors
                            .iter_mut()
                            .find(|s| s.name.as_deref() == Some(key.as_str()))
                        {
                            sensor.filament_present = detected;
                        } else {
                            let next_id = next
                                .filament_sensors
                                .iter()
                                .map(|s| s.id.0)
                                .max()
                                .unwrap_or(0)
                                .saturating_add(1);
                            next.filament_sensors.push(FilamentSensorTelemetry {
                                id: ElementId(next_id),
                                filament_present: detected,
                                name: Some(key.clone()),
                            });
                        }
                    }
                } else if val.is_null() {
                    if let Some(sensor) = next
                        .filament_sensors
                        .iter_mut()
                        .find(|s| s.name.as_deref() == Some(key.as_str()))
                    {
                        sensor.filament_present = None;
                        recognized = true;
                    }
                }
            }
        }
    }

    if base && next.status.is_none() {
        next.status = Some(PrintStatus::Unknown);
    }
    next.validate().map_err(MoonrakerError::Validation)?;
    Ok((next, recognized))
}

fn parse_status(value: &Value) -> Result<Option<PrintStatus>, MoonrakerError> {
    let Some(value) = value.as_str() else {
        if value.is_null() {
            return Ok(None);
        }
        return Err(MoonrakerError::InvalidMessage(
            "print_stats.state no es texto".to_owned(),
        ));
    };
    Ok(Some(match value {
        "standby" => PrintStatus::Idle,
        "printing" => PrintStatus::Printing,
        "paused" => PrintStatus::Paused,
        "complete" => PrintStatus::Completed,
        "cancelled" => PrintStatus::Cancelled,
        "error" => PrintStatus::Error,
        _ => PrintStatus::Unknown,
    }))
}
fn parse_optional_string(value: &Value) -> Result<Option<String>, MoonrakerError> {
    if value.is_null() {
        Ok(None)
    } else {
        let text = value.as_str().ok_or_else(|| {
            MoonrakerError::InvalidMessage("nombre de archivo no es texto".to_owned())
        })?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_owned()))
        }
    }
}
fn parse_optional_progress(value: &Value) -> Result<Option<f64>, MoonrakerError> {
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_f64()
        .ok_or_else(|| MoonrakerError::InvalidMessage("progreso no es número".to_owned()))?;
    let percent = value * 100.0;
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return Err(MoonrakerError::InvalidMessage(
            "progreso fuera del rango 0–1".to_owned(),
        ));
    }
    Ok(Some(percent))
}
fn parse_optional_finite(value: &Value) -> Result<Option<f64>, MoonrakerError> {
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_f64()
        .ok_or_else(|| MoonrakerError::InvalidMessage("temperatura no es número".to_owned()))?;
    if !value.is_finite() {
        return Err(MoonrakerError::InvalidMessage(
            "temperatura no finita".to_owned(),
        ));
    }
    Ok(Some(value))
}
fn parse_optional_nonnegative(value: &Value) -> Result<Option<f64>, MoonrakerError> {
    let value = parse_optional_finite(value)?;
    if value.is_some_and(|value| value < 0.0) {
        return Err(MoonrakerError::InvalidMessage(
            "duración negativa".to_owned(),
        ));
    }
    Ok(value)
}
fn parse_optional_u32(value: &Value) -> Result<Option<u32>, MoonrakerError> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| MoonrakerError::InvalidMessage("capa inválida".to_owned()))
}
fn parse_optional_bool(value: &Value) -> Result<Option<bool>, MoonrakerError> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| MoonrakerError::InvalidMessage("valor booleano inválido".to_owned()))
}
fn parse_optional_speed_factor(value: &Value) -> Result<Option<f64>, MoonrakerError> {
    if value.is_null() {
        return Ok(None);
    }
    let value = value.as_f64().ok_or_else(|| {
        MoonrakerError::InvalidMessage("factor de velocidad no es número".to_owned())
    })?;
    if !value.is_finite() || value < 0.0 {
        return Err(MoonrakerError::InvalidMessage(
            "factor de velocidad inválido".to_owned(),
        ));
    }
    Ok(Some(value * 100.0))
}

fn publish_connection(
    state: &mut PrinterState,
    sender: &watch::Sender<PrinterState>,
    connection: LanConnection,
) -> Result<(), MoonrakerError> {
    state.set_connection(connection)?;
    sender
        .send(state.clone())
        .map_err(|error| MoonrakerError::Transport(error.to_string()))
}
fn is_credential_error(error: &MoonrakerError) -> bool {
    error.to_string().contains("401") || error.to_string().contains("403")
}
