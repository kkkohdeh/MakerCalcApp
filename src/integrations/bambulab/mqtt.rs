//! Local MQTT session with a Bambu Lab printer and the report it accumulates.
//!
//! The printer runs its own MQTT 3.1.1 broker on port 8883.
//! The printer serial identifies the TLS peer even when TCP dials an IP address.

use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};
use tokio_native_tls::{TlsStream, native_tls};

const USER: &str = "bblp";
const KEEP_ALIVE_S: u16 = 30;
const PING_AFTER: Duration = Duration::from_secs(15);
/// No packet for this long means the session is dead even if TCP has not noticed.
pub(super) const STALE_AFTER: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A full report is tens of KiB; anything near this bound is not a printer report.
const MAX_PACKET: usize = 1024 * 1024;
const MAX_REPORT_VALUES: usize = 8192;
const MAX_REPORT_LIST_ITEMS: usize = 256;
const CA_BUNDLE: &str = include_str!("ca.pem");

/// Asks for the full state once per session. Some models rate-limit this request,
/// so it is never repeated while the session lives.
const PUSHALL: &[u8] =
    br#"{"pushing":{"sequence_id":"0","command":"pushall","version":1,"push_target":1}}"#;
const GET_VERSION: &[u8] = br#"{"info":{"sequence_id":"0","command":"get_version"}}"#;

/// Fields that belong to one print job. They are dropped when a new job starts so the
/// new job never shows the previous one's counters while its first deltas arrive.
const JOB_FIELDS: [&str; 10] = [
    "mc_percent",
    "layer_num",
    "total_layer_num",
    "mc_remaining_time",
    "spd_mag",
    "spd_lvl",
    "print_error",
    "mc_print_error_code",
    "subtask_name",
    "gcode_file",
];

#[derive(Debug)]
pub(super) enum Error {
    Network(String),
    Tls(String),
    /// The broker refused the access code.
    Rejected,
    Protocol(&'static str),
    Stale,
}

pub(super) struct Connection {
    stream: TlsStream<TcpStream>,
    buffer: Vec<u8>,
    report_topic: String,
    request_topic: String,
    last_sent: Instant,
}

impl Connection {
    pub(super) async fn open(
        host: &str,
        port: u16,
        serial: &str,
        access_code: &str,
    ) -> Result<Self, Error> {
        let connector = connector()?;
        let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| Error::Network("timeout".into()))?
            .map_err(|error| Error::Network(error.to_string()))?;
        let stream = timeout(CONNECT_TIMEOUT, connector.connect(serial, tcp))
            .await
            .map_err(|_| Error::Tls("timeout".into()))?
            .map_err(|error| Error::Tls(error.to_string()))?;
        let now = Instant::now();
        let mut connection = Self {
            stream,
            buffer: Vec::new(),
            report_topic: format!("device/{serial}/report"),
            request_topic: format!("device/{serial}/request"),
            last_sent: now,
        };
        connection
            .write(&connect_packet(&client_id(), access_code))
            .await?;
        let ack = timeout(CONNECT_TIMEOUT, connection.read_packet())
            .await
            .map_err(|_| Error::Network("timeout".into()))??;
        match (ack.header, ack.body.as_slice()) {
            (0x20, [0, 0]) => {}
            (0x20, [0, 4 | 5]) => return Err(Error::Rejected),
            _ => return Err(Error::Protocol("connect")),
        }
        let subscribe = subscribe_packet(1, &connection.report_topic);
        connection.write(&subscribe).await?;
        connection
            .write(&publish_packet(&connection.request_topic, PUSHALL, None))
            .await?;
        connection
            .write(&publish_packet(
                &connection.request_topic,
                GET_VERSION,
                None,
            ))
            .await?;
        Ok(connection)
    }

    pub(super) async fn message(&mut self, packet: Packet) -> Result<Option<Value>, Error> {
        match packet.header >> 4 {
            3 => self.publish(&packet).await,
            9 if packet.header != 0x90 || packet.body != [0, 1, 0] => {
                Err(Error::Protocol("subscribe"))
            }
            _ => Ok(None),
        }
    }

    pub(super) async fn send_control(&mut self, id: u16, command: &Value) -> Result<(), Error> {
        let payload = serde_json::to_vec(command).map_err(|_| Error::Protocol("command JSON"))?;
        self.write(&publish_packet(&self.request_topic, &payload, Some(id)))
            .await
    }

    pub(super) fn ping_at(&self) -> Instant {
        self.last_sent + PING_AFTER
    }

    pub(super) async fn ping(&mut self) -> Result<(), Error> {
        self.write(&[0xC0, 0x00]).await
    }

    async fn publish(&mut self, packet: &Packet) -> Result<Option<Value>, Error> {
        let qos = (packet.header >> 1) & 0x03;
        let body = packet.body.as_slice();
        let topic_len = usize::from(u16::from_be_bytes(
            body.get(..2)
                .ok_or(Error::Protocol("publish"))?
                .try_into()
                .map_err(|_| Error::Protocol("publish"))?,
        ));
        let topic = body
            .get(2..2 + topic_len)
            .ok_or(Error::Protocol("publish"))?;
        let mut payload_start = 2 + topic_len;
        if qos > 0 {
            let id = body
                .get(payload_start..payload_start + 2)
                .ok_or(Error::Protocol("publish"))?;
            if qos == 1 {
                self.write(&[0x40, 0x02, id[0], id[1]]).await?;
            }
            payload_start += 2;
        }
        if topic != self.report_topic.as_bytes() {
            return Ok(None);
        }
        let payload = &body[payload_start..];
        serde_json::from_slice(payload)
            .map(Some)
            .map_err(|_| Error::Protocol("json"))
    }

    // Reads are cancellation-safe; the actor sends ACKs and pings outside its select.
    pub(super) async fn read_packet(&mut self) -> Result<Packet, Error> {
        loop {
            if let Some(packet) = take_packet(&mut self.buffer)? {
                return Ok(packet);
            }
            let mut chunk = [0_u8; 16 * 1024];
            let read = self
                .stream
                .read(&mut chunk)
                .await
                .map_err(|error| Error::Network(error.to_string()))?;
            if read == 0 {
                return Err(Error::Network("closed".into()));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        timeout(CONNECT_TIMEOUT, self.stream.write_all(bytes))
            .await
            .map_err(|_| Error::Network("write timeout".into()))?
            .map_err(|error| Error::Network(error.to_string()))?;
        self.last_sent = Instant::now();
        Ok(())
    }
}

/// Latest known printer state: full reports replace, deltas merge into it.
#[derive(Clone, Default)]
pub(super) struct Report {
    value: Value,
}

impl Report {
    pub(super) fn value(&self) -> &Value {
        &self.value
    }

    pub(super) fn apply(&mut self, message: Value) -> Result<bool, Error> {
        check_report_size(&message)?;
        let Value::Object(message) = message else {
            return Ok(false);
        };
        if !self.value.is_object() {
            self.value = Value::Object(Map::new());
        }
        let mut status = false;
        for (key, incoming) in message {
            match key.as_str() {
                "print"
                    if incoming["command"] == "push_status"
                        && incoming.as_object().is_some_and(|fields| {
                            fields.keys().any(|key| {
                                !matches!(key.as_str(), "command" | "msg" | "sequence_id")
                            })
                        }) =>
                {
                    if super::integer(&incoming["msg"]) == Some(0) {
                        self.value["print"] = incoming;
                    } else {
                        self.reset_if_new_job(&incoming);
                        merge(&mut self.value["print"], incoming);
                    }
                    prune_disconnected_ams(&mut self.value["print"]);
                    clear_absent_spool_data(&mut self.value["print"]);
                    status = true;
                }
                "info" if incoming["command"] == "get_version" => {
                    self.value["info"] = incoming;
                }
                _ => {}
            }
        }
        check_report_size(&self.value)?;
        Ok(status)
    }

    fn reset_if_new_job(&mut self, incoming: &Value) {
        let print = &self.value["print"];
        let renamed = incoming["subtask_name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .is_some_and(|name| print["subtask_name"].as_str() != Some(name));
        let was_done = matches!(
            print["gcode_state"].as_str(),
            Some("IDLE" | "FINISH" | "FAILED")
        );
        let starts = matches!(
            incoming["gcode_state"].as_str(),
            Some("PREPARE" | "RUNNING")
        );
        if !(renamed || (was_done && starts)) {
            return;
        }
        if let Some(print) = self.value["print"].as_object_mut() {
            for key in JOB_FIELDS {
                print.remove(key);
            }
        }
    }
}

fn check_report_size(value: &Value) -> Result<(), Error> {
    fn visit(value: &Value, values: &mut usize, bytes: &mut usize) -> bool {
        let Some(left) = values.checked_sub(1) else {
            return false;
        };
        *values = left;
        match value {
            Value::Object(fields) => fields
                .iter()
                .all(|(key, value)| charge(bytes, key.len()) && visit(value, values, bytes)),
            Value::Array(items) => {
                items.len() <= MAX_REPORT_LIST_ITEMS
                    && items.iter().all(|item| visit(item, values, bytes))
            }
            Value::String(value) => charge(bytes, value.len()),
            _ => true,
        }
    }
    fn charge(bytes: &mut usize, cost: usize) -> bool {
        let Some(left) = bytes.checked_sub(cost) else {
            return false;
        };
        *bytes = left;
        true
    }
    let mut values = MAX_REPORT_VALUES;
    let mut bytes = MAX_PACKET;
    visit(value, &mut values, &mut bytes)
        .then_some(())
        .ok_or(Error::Protocol("report size"))
}

fn prune_disconnected_ams(print: &mut Value) {
    let ams = &mut print["ams"];
    let Some(bits) = super::hex(&ams["ams_exist_bits"]) else {
        return;
    };
    if let Some(units) = ams["ams"].as_array_mut() {
        units.retain(|unit| {
            if bits == 0 {
                return false;
            }
            let bit = match super::integer(&unit["id"]) {
                Some(id @ 0..=3) => id,
                Some(id @ 128..=135) => id - 124,
                _ => return true,
            };
            bits & (1 << bit) != 0
        });
    }
}

const SPOOL_FIELDS: [&str; 3] = ["remain", "nozzle_temp_min", "nozzle_temp_max"];

fn clear_absent_spool_data(print: &mut Value) {
    let Some(bits) = super::hex(&print["ams"]["tray_exist_bits"]) else {
        return;
    };
    let Some(units) = print["ams"]["ams"].as_array_mut() else {
        return;
    };
    for unit in units {
        let Some(id) = super::integer(&unit["id"]) else {
            continue;
        };
        let Some(trays) = unit["tray"].as_array_mut() else {
            continue;
        };
        for tray in trays {
            if let Some((_, bit)) =
                super::integer(&tray["id"]).and_then(|slot| super::station_slot(id, slot))
                && bits & (1 << bit) == 0
                && let Some(fields) = tray.as_object_mut()
            {
                for key in SPOOL_FIELDS {
                    fields.remove(key);
                }
            }
        }
    }
}

/// Objects merge field by field. Lists of objects that carry an `id` (or a light
/// `node`) merge item by item, because deltas may carry only the items that changed;
/// any other list replaces the previous one.
fn merge(target: &mut Value, incoming: Value) {
    match (target, incoming) {
        (Value::Object(target), Value::Object(incoming)) => {
            if ["tag_uid", "tray_uuid"].into_iter().any(|key| {
                incoming
                    .get(key)
                    .is_some_and(|value| target.get(key) != Some(value))
            }) {
                for key in SPOOL_FIELDS {
                    target.remove(key);
                }
            }
            for (key, value) in incoming {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (Value::Array(target), Value::Array(incoming))
            if keyed(target) && keyed(&incoming) && !incoming.is_empty() =>
        {
            for item in incoming {
                match target
                    .iter_mut()
                    .find(|existing| key(existing) == key(&item))
                {
                    Some(existing) => merge(existing, item),
                    None => target.push(item),
                }
            }
        }
        (target, incoming) => *target = incoming,
    }
}

fn keyed(items: &[Value]) -> bool {
    items.iter().all(|item| key(item).is_some())
}

fn key(item: &Value) -> Option<&Value> {
    item.get("id").or_else(|| item.get("node"))
}

pub(super) struct Packet {
    header: u8,
    body: Vec<u8>,
}

/// Removes one complete packet from the front of `buffer`, if it holds one.
fn take_packet(buffer: &mut Vec<u8>) -> Result<Option<Packet>, Error> {
    let Some(&header) = buffer.first() else {
        return Ok(None);
    };
    let mut length = 0_usize;
    let mut position = 1;
    loop {
        let Some(&byte) = buffer.get(position) else {
            return Ok(None);
        };
        length |= usize::from(byte & 0x7F) << (7 * (position - 1));
        position += 1;
        if byte & 0x80 == 0 {
            break;
        }
        if position > 4 {
            return Err(Error::Protocol("length"));
        }
    }
    if length > MAX_PACKET {
        return Err(Error::Protocol("size"));
    }
    if buffer.len() < position + length {
        return Ok(None);
    }
    let body = buffer[position..position + length].to_vec();
    buffer.drain(..position + length);
    Ok(Some(Packet { header, body }))
}

fn connect_packet(client_id: &str, access_code: &str) -> Vec<u8> {
    let mut body = Vec::new();
    put_str(&mut body, "MQTT");
    // Protocol level 4 (3.1.1); user name, password and clean session.
    body.extend_from_slice(&[4, 0xC2]);
    body.extend_from_slice(&KEEP_ALIVE_S.to_be_bytes());
    put_str(&mut body, client_id);
    put_str(&mut body, USER);
    put_str(&mut body, access_code);
    frame(0x10, &body)
}

fn subscribe_packet(id: u16, topic: &str) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    put_str(&mut body, topic);
    body.push(0);
    frame(0x82, &body)
}

fn publish_packet(topic: &str, payload: &[u8], packet_id: Option<u16>) -> Vec<u8> {
    let mut body = Vec::new();
    put_str(&mut body, topic);
    if let Some(id) = packet_id {
        body.extend_from_slice(&id.to_be_bytes());
    }
    body.extend_from_slice(payload);
    frame(if packet_id.is_some() { 0x32 } else { 0x30 }, &body)
}

fn frame(header: u8, body: &[u8]) -> Vec<u8> {
    let mut packet = vec![header];
    let mut length = body.len();
    loop {
        let mut byte = (length % 128) as u8;
        length /= 128;
        if length > 0 {
            byte |= 0x80;
        }
        packet.push(byte);
        if length == 0 {
            break;
        }
    }
    packet.extend_from_slice(body);
    packet
}

/// MQTT strings carry a 16-bit length. Every caller passes a bounded value: fixed
/// topics, the validated serial and access code.
fn put_str(out: &mut Vec<u8>, value: &str) {
    let length = u16::try_from(value.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&value.as_bytes()[..usize::from(length)]);
}

/// The broker drops an older session that reuses a client id, so two app instances
/// on the same printer must not share one.
fn client_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!("makercalc_{}_{nanos}", std::process::id())
}

fn connector() -> Result<tokio_native_tls::TlsConnector, Error> {
    let mut builder = native_tls::TlsConnector::builder();
    for block in CA_BUNDLE.split_inclusive("-----END CERTIFICATE-----") {
        if !block.contains("-----BEGIN CERTIFICATE-----") {
            continue;
        }
        let certificate = native_tls::Certificate::from_pem(block.trim().as_bytes())
            .map_err(|error| Error::Tls(error.to_string()))?;
        builder.add_root_certificate(certificate);
    }
    // Some firmware never answers a TLS 1.3 ClientHello.
    builder
        .disable_built_in_roots(true)
        .max_protocol_version(Some(native_tls::Protocol::Tlsv12));
    builder
        .build()
        .map(Into::into)
        .map_err(|error| Error::Tls(error.to_string()))
}
