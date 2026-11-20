//! Bounded TCP sessions for manual motor control.
use super::{ControlError, ControlResult};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

async fn connect(host: &str, port: u16) -> Result<TcpStream, ControlResult> {
    tokio::time::timeout(Duration::from_secs(2), TcpStream::connect((host, port)))
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(ControlResult::Rejected(ControlError::SendFailed))
}

struct Reply {
    body: String,
    result: Result<(), ControlResult>,
}

impl Reply {
    fn acquired_control(&self) -> bool {
        self.body
            .split_inclusive('\n')
            .any(|line| line.ends_with('\n') && line.trim().starts_with("Control Success "))
            && !self.body.contains("Control Failed")
    }

    fn complete(self) -> Result<String, ControlResult> {
        self.result.map(|()| self.body)
    }
}

async fn exchange(stream: &mut TcpStream, command: &str) -> Reply {
    let mut data = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        stream
            .write_all(format!("~{command}\r\n").as_bytes())
            .await
            .ok()?;
        let mut chunk = [0; 512];
        loop {
            let count = stream.read(&mut chunk).await.ok()?;
            if count == 0 || data.len() + count > 8192 {
                return None;
            }
            data.extend_from_slice(&chunk[..count]);
            let text = match std::str::from_utf8(&data) {
                Ok(text) => text,
                Err(error) if error.error_len().is_none() => continue,
                Err(_) => return None,
            };
            if text
                .lines()
                .any(|line| line.starts_with("Error:") || line.contains("Control Failed"))
            {
                return Some(Err(ControlResult::Rejected(ControlError::PrinterRejected)));
            }
            if text.ends_with('\n') && text.lines().any(|line| line.trim() == "ok") {
                return Some(Ok(()));
            }
        }
    })
    .await;
    Reply {
        body: String::from_utf8_lossy(&data).into_owned(),
        result: result.ok().flatten().unwrap_or(Err(ControlResult::Unknown)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::integrations::flashforge) enum Model {
    Ad5x,
    Adventurer5m,
    Unsupported,
}

pub(in crate::integrations::flashforge) async fn probe(host: &str) -> Model {
    probe_at(host, 8899).await
}

async fn probe_at(host: &str, port: u16) -> Model {
    let Ok(mut stream) = connect(host, port).await else {
        return Model::Unsupported;
    };
    let Ok(reply) = exchange(&mut stream, "M115").await.complete() else {
        return Model::Unsupported;
    };
    reply
        .lines()
        .find_map(|line| {
            let name = line.strip_prefix("Machine Type:")?.trim();
            match name.to_ascii_lowercase().as_str() {
                "flashforge ad5x" => Some(Model::Ad5x),
                "flashforge adventurer 5m" | "flashforge adventurer 5m pro" => {
                    Some(Model::Adventurer5m)
                }
                _ => None,
            }
        })
        .unwrap_or(Model::Unsupported)
}

pub(in crate::integrations::flashforge) async fn release(
    host: &str,
    authorize: impl Fn() -> Result<(), ControlError>,
) -> ControlResult {
    release_at(host, 8899, authorize).await
}

async fn release_at(
    host: &str,
    port: u16,
    authorize: impl Fn() -> Result<(), ControlError>,
) -> ControlResult {
    run_idle_at(host, port, "M18", "CMD M18 Received.", authorize).await
}

pub(in crate::integrations::flashforge) async fn extrude(
    host: &str,
    length_mm: f64,
    speed_mm_s: f64,
    authorize: impl Fn() -> Result<(), ControlError>,
) -> ControlResult {
    extrude_at(host, 8899, length_mm, speed_mm_s, authorize).await
}

async fn extrude_at(
    host: &str,
    port: u16,
    length_mm: f64,
    speed_mm_s: f64,
    authorize: impl Fn() -> Result<(), ControlError>,
) -> ControlResult {
    // The FlashForge server resets E before an E-only G1.
    let feedrate = speed_mm_s * 60.0;
    let command = format!("G1 E{length_mm} F{feedrate}");
    run_idle_at(host, port, &command, "CMD G1 Received.", authorize).await
}

async fn run_idle_at(
    host: &str,
    port: u16,
    command: &str,
    acknowledgement: &str,
    authorize: impl Fn() -> Result<(), ControlError>,
) -> ControlResult {
    let mut stream = match connect(host, port).await {
        Ok(stream) => stream,
        Err(result) => return result,
    };
    let acquired = exchange(&mut stream, "M601 S1").await;
    if !acquired.acquired_control() {
        return acquired
            .result
            .err()
            .unwrap_or(ControlResult::Rejected(ControlError::PrinterRejected));
    }
    let result = async {
        acquired.complete()?;
        // Recheck physical rest after acquiring control.
        let status = exchange(&mut stream, "M119").await.complete()?;
        let field = |prefix| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .map(str::trim)
        };
        if field("MachineStatus:") != Some("READY") || field("MoveMode:") != Some("READY") {
            return Err(ControlResult::Rejected(ControlError::InvalidState));
        }
        authorize().map_err(ControlResult::Rejected)?;
        let reply = exchange(&mut stream, command).await.complete()?;
        if !reply.contains(acknowledgement) {
            return Err(ControlResult::Unknown);
        }
        Ok(ControlResult::Accepted)
    }
    .await;
    // An ambiguous acknowledgement never triggers another move.
    let cleanup = exchange(&mut stream, "M602").await.complete();
    match result {
        Err(result) => result,
        Ok(result) if cleanup.is_ok() => result,
        Ok(_) => ControlResult::Unknown,
    }
}
