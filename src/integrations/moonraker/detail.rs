use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};
use url::Url;

use crate::shared::gcode::GcodeFacts;

pub(crate) const DETAIL_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const DETAIL_MAX_DEPTH: usize = 64;
pub(crate) const DETAIL_REQUEST_CAPACITY: usize = 8;
pub(crate) const PREVIEW_RESULT_CAPACITY: usize = 2;
pub(crate) const FILE_PREVIEW_BYTES: usize = 512 * 1024;

#[derive(Clone)]
pub(crate) struct DetailHandle {
    pub(crate) active: watch::Sender<bool>,
    pub(crate) requests: mpsc::Sender<DetailRequest>,
    pub(crate) state: watch::Receiver<Arc<DetailState>>,
}

pub(crate) struct DetailIo {
    pub(crate) active: watch::Receiver<bool>,
    pub(crate) requests: mpsc::Receiver<DetailRequest>,
    pub(crate) state: watch::Sender<Arc<DetailState>>,
    pub(crate) preview_results: mpsc::Receiver<PreviewResult>,
    pub(crate) preview_sender: mpsc::Sender<PreviewResult>,
}

pub(crate) struct PreviewResult {
    pub(crate) generation: u64,
    pub(crate) key: String,
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) offset: u64,
    pub(crate) result: Result<FilePreview, DetailError>,
    pub(crate) thumbnail: Option<bytes::Bytes>,
}

#[derive(Clone, Debug)]
pub(crate) enum DetailRequest {
    CommandCatalog {
        key: String,
    },
    Rpc {
        key: String,
        method: String,
        params: Option<Value>,
    },
    PreviewFile {
        key: String,
        root: String,
        path: String,
        offset: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SectionStatus {
    NotLoaded,
    Loading,
    Ready,
    Unsupported,
    Denied,
    Error,
    Limited,
}

#[derive(Clone)]
pub(crate) struct SectionData {
    pub(crate) status: SectionStatus,
    pub(crate) data: Option<Value>,
    /// Parsed once; shared across UI frames.
    pub(crate) gcode_facts: Option<Arc<GcodeFacts>>,
    pub(crate) thumbnail: Option<bytes::Bytes>,
    pub(crate) error: Option<String>,
}

pub(crate) struct DetailState {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) live: bool,
    pub(crate) objects: Value,
    pub(crate) sections: BTreeMap<String, SectionData>,
}

impl DetailState {
    pub(crate) fn empty(generation: u64) -> Self {
        Self {
            generation,
            revision: 0,
            live: false,
            objects: json!({}),
            sections: BTreeMap::new(),
        }
    }
}

pub(crate) fn make_detail_channels() -> (DetailHandle, DetailIo) {
    let (active_sender, active_receiver) = watch::channel(false);
    let (request_sender, request_receiver) = mpsc::channel(DETAIL_REQUEST_CAPACITY);
    let (state_sender, state_receiver) = watch::channel(Arc::new(DetailState::empty(0)));
    let (preview_sender, preview_results) = mpsc::channel(PREVIEW_RESULT_CAPACITY);
    (
        DetailHandle {
            active: active_sender,
            requests: request_sender,
            state: state_receiver,
        },
        DetailIo {
            active: active_receiver,
            requests: request_receiver,
            state: state_sender,
            preview_results,
            preview_sender,
        },
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DetailError {
    ForbiddenMethod,
    InvalidParameters,
    InvalidPayload,
    PayloadLimit,
    DepthLimit,
    InvalidPath,
    Http(String),
}

impl fmt::Display for DetailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ForbiddenMethod => "lectura Moonraker no permitida",
            Self::InvalidParameters => "parámetros de lectura inválidos",
            Self::InvalidPayload => "respuesta Moonraker inválida",
            Self::PayloadLimit => "la respuesta supera el límite local",
            Self::DepthLimit => "la respuesta supera la profundidad permitida",
            Self::InvalidPath => "ruta de archivo inválida",
            Self::Http(message) => message,
        })
    }
}

impl std::error::Error for DetailError {}

pub(crate) fn build_read_request(
    id: u64,
    method: &str,
    params: Option<Value>,
) -> Result<Value, DetailError> {
    validate_read_params(method, params.as_ref())?;
    let mut request = json!({"jsonrpc":"2.0", "id":id, "method":method});
    if let Some(params) = params {
        request["params"] = params;
    }
    Ok(request)
}

fn validate_read_params(method: &str, params: Option<&Value>) -> Result<(), DetailError> {
    if method == "server.sensors.measurements" && params.is_none() {
        return Ok(());
    }
    const WITHOUT_PARAMS: &[&str] = &[
        "server.info",
        "printer.info",
        "server.websocket.id",
        "printer.gcode.help",
        "printer.objects.list",
        "server.config",
        "machine.system_info",
        "machine.proc_stats",
        "machine.sudo.info",
        "machine.peripherals.usb",
        "machine.peripherals.serial",
        "machine.peripherals.video",
        "server.history.totals",
        "server.job_queue.status",
        "server.files.roots",
        "machine.device_power.devices",
        "machine.wled.strips",
        "server.webcams.list",
        "server.extensions.list",
        "server.notifiers.list",
        "server.spoolman.status",
        "server.spoolman.get_spool_id",
        "server.analysis.status",
        "machine.td1.data",
        "server.announcements.list",
        "server.announcements.feeds",
        "access.info",
        "access.get_user",
        "access.users.list",
        "server.database.list",
    ];
    if WITHOUT_PARAMS.contains(&method) {
        return params
            .is_none()
            .then_some(())
            .ok_or(DetailError::InvalidParameters);
    }
    let Some(params) = params.and_then(Value::as_object) else {
        return Err(DetailError::ForbiddenMethod);
    };
    let valid = match method {
        "server.temperature_store" => exact_bool(params, "include_monitors"),
        "server.gcode_store" => exact_u64_range(params, "count", 1, 200),
        "printer.objects.query" => valid_object_query(params),
        "server.history.list" => {
            exact_keys(params, &["limit", "start", "order"])
                && params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .is_some_and(|v| (1..=50).contains(&v))
                && params.get("start").and_then(Value::as_u64).is_some()
                && matches!(
                    params.get("order").and_then(Value::as_str),
                    Some("desc" | "asc")
                )
        }
        "server.files.get_directory" => {
            exact_keys(params, &["path", "extended"])
                && params
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(valid_relative_path)
                && params.get("extended").and_then(Value::as_bool).is_some()
        }
        "server.files.list" => exact_safe_string(params, "root"),
        "server.files.metadata" | "server.files.thumbnails" => exact_safe_path(params, "filename"),
        "server.sensors.list" => exact_bool(params, "extended"),
        "server.sensors.info" => {
            exact_keys(params, &["sensor", "extended"])
                && params
                    .get("sensor")
                    .and_then(Value::as_str)
                    .is_some_and(valid_identifier)
                && params.get("extended").and_then(Value::as_bool).is_some()
        }
        "server.sensors.measurements" => exact_safe_string(params, "sensor"),
        "machine.device_power.get_device" => exact_safe_string(params, "device"),
        "machine.device_power.status" | "machine.wled.status" => valid_dynamic_null_map(params),
        "machine.wled.get_strip" => exact_safe_string(params, "strip"),
        "server.webcams.get_item" => exact_safe_string(params, "uid"),
        "machine.update.status" => {
            exact_keys(params, &["refresh"]) && params.get("refresh") == Some(&Value::Bool(false))
        }
        "server.database.get_item" => exact_safe_string(params, "namespace"),
        _ => false,
    };
    valid.then_some(()).ok_or(DetailError::InvalidParameters)
}

fn exact_keys(map: &Map<String, Value>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key))
}

fn exact_bool(map: &Map<String, Value>, key: &str) -> bool {
    exact_keys(map, &[key]) && map.get(key).and_then(Value::as_bool).is_some()
}

fn exact_u64_range(map: &Map<String, Value>, key: &str, minimum: u64, maximum: u64) -> bool {
    exact_keys(map, &[key])
        && map
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|value| (minimum..=maximum).contains(&value))
}

fn exact_safe_string(map: &Map<String, Value>, key: &str) -> bool {
    exact_keys(map, &[key])
        && map
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(valid_identifier)
}

fn exact_safe_path(map: &Map<String, Value>, key: &str) -> bool {
    exact_keys(map, &[key])
        && map
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(valid_relative_path)
}

fn valid_object_query(map: &Map<String, Value>) -> bool {
    if !exact_keys(map, &["objects"]) {
        return false;
    }
    let Some(objects) = map.get("objects").and_then(Value::as_object) else {
        return false;
    };
    !objects.is_empty()
        && objects.iter().all(|(name, fields)| {
            valid_identifier(name)
                && (fields.is_null()
                    || fields
                        .as_array()
                        .is_some_and(|values| values.iter().all(Value::is_string)))
        })
}

fn valid_dynamic_null_map(map: &Map<String, Value>) -> bool {
    !map.is_empty()
        && map
            .iter()
            .all(|(name, value)| valid_identifier(name) && value.is_null())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && !value.contains(['\\', '/', '?', '#'])
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && !value.contains('\\')
        && value.split('/').all(valid_path_segment)
}

fn valid_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !matches!(segment, "." | "..")
        && !segment.chars().any(char::is_control)
        && !segment.contains(':')
}

pub(crate) fn subscription_params(objects: &[String], expanded: bool) -> Value {
    let mut requested = Map::new();
    if expanded {
        for object in objects {
            if object != "configfile" && valid_identifier(object) {
                requested.insert(object.clone(), Value::Null);
            }
        }
    } else {
        let available = |name: &str| objects.iter().any(|object| object == name);
        if available("print_stats") {
            requested.insert(
                "print_stats".to_owned(),
                json!([
                    "state",
                    "filename",
                    "print_duration",
                    "total_duration",
                    "filament_used",
                    "info",
                    "message"
                ]),
            );
        }
        if available("virtual_sdcard") {
            requested.insert("virtual_sdcard".to_owned(), json!(["progress"]));
        }
        if available("display_status") {
            requested.insert("display_status".to_owned(), json!(["message", "progress"]));
        }
        for object in objects {
            if object == "extruder"
                || object.strip_prefix("extruder").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                requested.insert(object.clone(), json!(["temperature", "target"]));
            }
        }
        if available("heater_bed") {
            requested.insert("heater_bed".to_owned(), json!(["temperature", "target"]));
        }
        if available("toolhead") {
            requested.insert("toolhead".to_owned(), json!(["extruder"]));
        }
        if available("motion_report") {
            requested.insert("motion_report".to_owned(), json!(["live_velocity"]));
        }
        if available("fan") {
            requested.insert("fan".to_owned(), json!(["speed", "rpm"]));
        }
        if available("gcode_move") {
            requested.insert(
                "gcode_move".to_owned(),
                json!(["speed_factor", "gcode_position"]),
            );
        }
        for object in objects {
            if object.starts_with("output_pin ") {
                requested.insert(object.clone(), json!(["value"]));
            }
        }
        for object in objects {
            if object.starts_with("filament_switch_sensor ")
                || object.starts_with("filament_motion_sensor ")
            {
                requested.insert(object.clone(), json!(["filament_detected"]));
            }
        }
        if available("heaters") {
            requested.insert(
                "heaters".to_owned(),
                json!(["available_heaters", "available_sensors"]),
            );
        }
    }
    json!({"objects": requested})
}

pub(crate) struct DetailCache {
    generation: u64,
    eventtime: Option<f64>,
    max_bytes: usize,
    objects: Value,
}

impl DetailCache {
    pub(crate) fn new(generation: u64, max_bytes: usize) -> Self {
        Self {
            generation,
            eventtime: None,
            max_bytes: max_bytes.min(DETAIL_MAX_BYTES),
            objects: json!({}),
        }
    }

    pub(crate) fn replace_base(
        &mut self,
        generation: u64,
        eventtime: f64,
        objects: &Value,
    ) -> Result<bool, DetailError> {
        if generation != self.generation {
            return Ok(false);
        }
        validate_eventtime(eventtime)?;
        validate_objects(objects, false)?;
        validate_payload(objects, self.max_bytes)?;
        if self.eventtime.is_some_and(|previous| eventtime < previous) {
            return Ok(false);
        }
        self.objects = objects.clone();
        self.eventtime = Some(eventtime);
        Ok(true)
    }

    pub(crate) fn apply_delta(
        &mut self,
        generation: u64,
        eventtime: f64,
        delta: &Value,
    ) -> Result<bool, DetailError> {
        if generation != self.generation {
            return Ok(false);
        }
        validate_eventtime(eventtime)?;
        validate_objects(delta, true)?;
        if self.eventtime.is_some_and(|previous| eventtime < previous) {
            return Ok(false);
        }
        let mut next = self.objects.clone();
        let target = next.as_object_mut().ok_or(DetailError::InvalidPayload)?;
        for (object_name, fields) in delta.as_object().ok_or(DetailError::InvalidPayload)? {
            if fields.is_null() {
                target.remove(object_name);
                continue;
            }
            let target_fields = target
                .entry(object_name.clone())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or(DetailError::InvalidPayload)?;
            for (field_name, value) in fields.as_object().ok_or(DetailError::InvalidPayload)? {
                target_fields.insert(field_name.clone(), value.clone());
            }
        }
        validate_payload(&next, self.max_bytes)?;
        self.objects = next;
        self.eventtime = Some(eventtime);
        Ok(true)
    }

    pub(crate) fn objects(&self) -> &Value {
        &self.objects
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "La interfaz de contrato local se ejerce desde las pruebas incluidas, no desde el binario release."
        )
    )]
    pub(crate) fn eventtime(&self) -> Option<f64> {
        self.eventtime
    }

    pub(crate) fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.eventtime = None;
        self.objects = json!({});
    }
}

fn validate_eventtime(eventtime: f64) -> Result<(), DetailError> {
    (eventtime.is_finite() && eventtime >= 0.0)
        .then_some(())
        .ok_or(DetailError::InvalidPayload)
}

fn validate_objects(value: &Value, allow_null: bool) -> Result<(), DetailError> {
    value
        .as_object()
        .filter(|objects| {
            objects
                .values()
                .all(|field| field.is_object() || (allow_null && field.is_null()))
        })
        .map(|_| ())
        .ok_or(DetailError::InvalidPayload)
}

fn validate_payload(value: &Value, max_bytes: usize) -> Result<(), DetailError> {
    if json_depth(value) > DETAIL_MAX_DEPTH {
        return Err(DetailError::DepthLimit);
    }
    if serde_json::to_vec(value)
        .map_err(|_| DetailError::InvalidPayload)?
        .len()
        > max_bytes
    {
        return Err(DetailError::PayloadLimit);
    }
    Ok(())
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

pub(crate) fn redact_json(value: &Value, known_secrets: &[&str]) -> Value {
    redact_value(value, known_secrets, None)
}

fn redact_value(value: &Value, known_secrets: &[&str], key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let safe = if sensitive_key(key) {
                        Value::String("[REDACTED]".to_owned())
                    } else {
                        redact_value(value, known_secrets, Some(key))
                    };
                    (key.clone(), safe)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| redact_value(value, known_secrets, key))
                .collect(),
        ),
        Value::String(text) => Value::String(redact_text(text, known_secrets, key)),
        _ => value.clone(),
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "password",
        "passwd",
        "passphrase",
        "apikey",
        "token",
        "accesstoken",
        "refreshtoken",
        "authorization",
        "privatekey",
        "secret",
        "credential",
    ]
    .iter()
    .any(|word| normalized.contains(word))
}

fn redact_text(text: &str, known_secrets: &[&str], key: Option<&str>) -> String {
    let mut safe = text.to_owned();
    for secret in known_secrets.iter().filter(|secret| !secret.is_empty()) {
        safe = safe.replace(secret, "[REDACTED]");
    }
    if key.is_some_and(|key| key.eq_ignore_ascii_case("url") && !safe.starts_with("http")) {
        return "[REDACTED URL]".to_owned();
    }
    if let Ok(mut url) = Url::parse(&safe) {
        if !url.username().is_empty() || url.password().is_some() {
            let _ = url.set_username("");
            let _ = url.set_password(None);
        }
        let query = url.query().map(ToOwned::to_owned);
        if query.as_deref().is_some_and(|query| {
            query.split('&').any(|pair| {
                pair.split_once('=')
                    .is_some_and(|(name, _)| sensitive_key(name))
            })
        }) {
            url.set_query(None);
        }
        return url.to_string();
    }
    safe
}

pub(crate) fn file_url(base: &str, root: &str, relative: &str) -> Result<Url, DetailError> {
    if !valid_identifier(root) || !valid_relative_path(relative) {
        return Err(DetailError::InvalidPath);
    }
    let mut url = Url::parse(base).map_err(|_| DetailError::InvalidPath)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(DetailError::InvalidPath);
    }
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| DetailError::InvalidPath)?;
    segments.clear();
    segments.push("server");
    segments.push("files");
    segments.push(root);
    for segment in relative.split('/') {
        segments.push(segment);
    }
    drop(segments);
    Ok(url)
}

#[derive(Debug)]
pub(crate) struct FilePreview {
    pub(crate) content_type: Option<String>,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) next_offset: Option<u64>,
    pub(crate) text: Option<String>,
    pub(crate) analysis_tail: Option<String>,
    pub(crate) binary: bool,
}

pub(crate) async fn fetch_gcode_preview(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    known_secrets: &[&str],
) -> Result<FilePreview, DetailError> {
    let mut preview = fetch_file_preview(client, base, "gcodes", path, 0, known_secrets).await?;
    if preview.binary {
        return Ok(preview);
    }
    let Some(total_bytes) = preview.total_bytes else {
        return Ok(preview);
    };
    let tail_offset = total_bytes.saturating_sub(FILE_PREVIEW_BYTES as u64);
    if tail_offset == 0 {
        return Ok(preview);
    }
    let tail = fetch_file_preview(client, base, "gcodes", path, tail_offset, known_secrets).await?;
    if !tail.binary {
        preview.analysis_tail = tail.text;
    }
    Ok(preview)
}

pub(crate) async fn fetch_file_preview(
    client: &reqwest::Client,
    base: &str,
    root: &str,
    path: &str,
    offset: u64,
    known_secrets: &[&str],
) -> Result<FilePreview, DetailError> {
    if protected_file(path) {
        return Err(DetailError::InvalidPath);
    }
    let url = file_url(base, root, path)?;
    let end = offset
        .saturating_add(FILE_PREVIEW_BYTES as u64)
        .saturating_sub(1);
    let (content_type, range_end, total_bytes, bytes) =
        tokio::time::timeout(Duration::from_secs(4), async {
            let response = client
                .get(url)
                .header(RANGE, format!("bytes={offset}-{end}"))
                .send()
                .await
                .map_err(|error| DetailError::Http(sanitize_http_error(&error.to_string())))?;
            let status = response.status();
            if status.is_redirection() {
                return Err(DetailError::Http(
                    "Moonraker redirigió el contenido; se bloqueó por seguridad".to_owned(),
                ));
            }
            if offset > 0 && status == reqwest::StatusCode::OK {
                return Err(DetailError::Http(
                    "Rango no soportado por esta impresora".to_owned(),
                ));
            }
            if status != reqwest::StatusCode::PARTIAL_CONTENT && status != reqwest::StatusCode::OK {
                return Err(DetailError::Http(format!(
                    "Moonraker devolvió HTTP {}",
                    status.as_u16()
                )));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            let range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            let (range_end, total_bytes) = if status == reqwest::StatusCode::PARTIAL_CONTENT {
                parse_content_range(range.as_deref(), offset)?
            } else {
                (None, response.content_length())
            };
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::with_capacity(FILE_PREVIEW_BYTES);
            while let Some(chunk) = stream.next().await {
                let chunk = chunk
                    .map_err(|error| DetailError::Http(sanitize_http_error(&error.to_string())))?;
                if bytes.len().saturating_add(chunk.len()) > FILE_PREVIEW_BYTES {
                    return Err(DetailError::PayloadLimit);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, DetailError>((content_type, range_end, total_bytes, bytes))
        })
        .await
        .map_err(|_| DetailError::Http("Moonraker tardó demasiado en responder".to_owned()))??;
    let binary = !is_text_content(content_type.as_deref(), &bytes);
    let text = (!binary)
        .then(|| String::from_utf8_lossy(&bytes).to_string())
        .map(|text| redact_text_preview(&text, known_secrets));
    let next_offset = range_end
        .and_then(|end| end.checked_add(1))
        .filter(|next| total_bytes.is_none_or(|total| *next < total));
    Ok(FilePreview {
        content_type,
        total_bytes,
        next_offset,
        text,
        analysis_tail: None,
        binary,
    })
}

fn parse_content_range(
    value: Option<&str>,
    expected_start: u64,
) -> Result<(Option<u64>, Option<u64>), DetailError> {
    let value = value.ok_or_else(|| DetailError::Http("Content-Range ausente".to_owned()))?;
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| DetailError::Http("Content-Range inválido".to_owned()))?;
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| DetailError::Http("Content-Range inválido".to_owned()))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| DetailError::Http("Content-Range inválido".to_owned()))?;
    let start = start
        .parse::<u64>()
        .map_err(|_| DetailError::Http("Content-Range inválido".to_owned()))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| DetailError::Http("Content-Range inválido".to_owned()))?;
    if start != expected_start
        || end < start
        || end.saturating_sub(start).saturating_add(1) > FILE_PREVIEW_BYTES as u64
    {
        return Err(DetailError::Http("Content-Range incoherente".to_owned()));
    }
    let total = if total == "*" {
        None
    } else {
        Some(
            total
                .parse::<u64>()
                .map_err(|_| DetailError::Http("Content-Range inválido".to_owned()))?,
        )
    };
    if total.is_some_and(|total| end >= total) {
        return Err(DetailError::Http("Content-Range incoherente".to_owned()));
    }
    Ok((Some(end), total))
}

fn protected_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        ".key",
        ".pem",
        ".pfx",
        ".p12",
        ".kdbx",
        "authorized_keys",
        "id_rsa",
        "id_ed25519",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

fn is_text_content(content_type: Option<&str>, bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return false;
    }
    content_type.is_none_or(|type_name| {
        let type_name = type_name.to_ascii_lowercase();
        type_name.starts_with("text/")
            || type_name.contains("json")
            || type_name.contains("yaml")
            || type_name.contains("gcode")
            || type_name == "application/octet-stream"
    })
}

pub(crate) fn redact_text_preview(text: &str, known_secrets: &[&str]) -> String {
    let mut output = String::with_capacity(text.len());
    let mut inside_pem = false;
    for line in text.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("-----begin ") && lower.contains(" private key-----") {
            inside_pem = true;
        }
        let safe = if inside_pem || sensitive_assignment(&lower) {
            "[REDACTED]\n".to_owned()
        } else {
            redact_text(line, known_secrets, None)
        };
        output.push_str(&safe);
        if lower.contains("-----end ") && lower.contains(" private key-----") {
            inside_pem = false;
        }
    }
    output
}

fn sensitive_assignment(lower: &str) -> bool {
    lower
        .split_once(['=', ':'])
        .is_some_and(|(name, _)| sensitive_key(name))
}

fn sanitize_http_error(error: &str) -> String {
    error
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}
