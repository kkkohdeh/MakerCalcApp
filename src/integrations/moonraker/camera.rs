use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use url::Url;

pub(crate) const CAMERA_REQUEST_CAPACITY: usize = 2;
pub(crate) const CAMERA_MAX_BYTES: usize = 4 * 1024 * 1024;
const CAMERA_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub(crate) struct CameraHandle {
    pub(crate) requests: mpsc::Sender<CameraRequest>,
    pub(crate) state: watch::Receiver<Arc<CameraState>>,
}

pub(crate) struct CameraIo {
    pub(crate) requests: mpsc::Receiver<CameraRequest>,
    pub(crate) state: watch::Sender<Arc<CameraState>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CameraRequest {
    Load { uid: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CameraStatus {
    Idle,
    Loading,
    Ready,
    Unsupported,
    Denied,
    Error,
    Limited,
}

#[derive(Clone)]
pub(crate) struct CameraState {
    pub(crate) uid: Option<String>,
    pub(crate) status: CameraStatus,
    pub(crate) frame: Option<Bytes>,
    pub(crate) received_at: Option<Instant>,
    pub(crate) message: Option<String>,
}

impl CameraState {
    pub(crate) fn empty() -> Self {
        Self {
            uid: None,
            status: CameraStatus::Idle,
            frame: None,
            received_at: None,
            message: None,
        }
    }

    pub(crate) fn loading(uid: String, previous: &Self) -> Self {
        let same_camera = previous.uid.as_deref() == Some(uid.as_str());
        Self {
            uid: Some(uid),
            status: CameraStatus::Loading,
            frame: same_camera.then(|| previous.frame.clone()).flatten(),
            received_at: same_camera.then_some(previous.received_at).flatten(),
            message: None,
        }
    }
}

pub(crate) fn make_camera_channels() -> (CameraHandle, CameraIo) {
    let (requests, request_io) = mpsc::channel(CAMERA_REQUEST_CAPACITY);
    let (state_io, state) = watch::channel(Arc::new(CameraState::empty()));
    (
        CameraHandle { requests, state },
        CameraIo {
            requests: request_io,
            state: state_io,
        },
    )
}

pub(crate) fn same_origin_snapshot_url(base: &str, raw_url: &str) -> Result<Url, CameraStatus> {
    let base = Url::parse(base).map_err(|_| CameraStatus::Error)?;
    let candidate = base.join(raw_url).map_err(|_| CameraStatus::Unsupported)?;
    let same_origin = candidate.scheme() == base.scheme()
        && candidate.host_str() == base.host_str()
        && candidate.port_or_known_default() == base.port_or_known_default();
    if !same_origin
        || !matches!(candidate.scheme(), "http" | "https")
        || !candidate.username().is_empty()
        || candidate.password().is_some()
    {
        return Err(CameraStatus::Unsupported);
    }
    Ok(candidate)
}

pub(crate) async fn fetch_snapshot(
    client: reqwest::Client,
    endpoint: Url,
) -> Result<Bytes, (CameraStatus, String)> {
    tokio::time::timeout(
        CAMERA_DOWNLOAD_TIMEOUT,
        fetch_snapshot_body(client, endpoint),
    )
    .await
    .map_err(|_| {
        (
            CameraStatus::Error,
            "No se pudo obtener el snapshot de cámara".to_owned(),
        )
    })?
}

async fn fetch_snapshot_body(
    client: reqwest::Client,
    endpoint: Url,
) -> Result<Bytes, (CameraStatus, String)> {
    let response = client.get(endpoint).send().await.map_err(|_| {
        (
            CameraStatus::Error,
            "No se pudo obtener el snapshot de cámara".to_owned(),
        )
    })?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err((
            CameraStatus::Denied,
            "Moonraker no autorizó el snapshot de cámara".to_owned(),
        ));
    }
    if !response.status().is_success() {
        return Err((
            CameraStatus::Error,
            format!("La cámara respondió {}", response.status()),
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !(content_type.starts_with("image/jpeg") || content_type.starts_with("image/png")) {
        return Err((
            CameraStatus::Unsupported,
            "La cámara no expone un snapshot JPEG o PNG".to_owned(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > CAMERA_MAX_BYTES as u64)
    {
        return Err((CameraStatus::Limited, "El snapshot supera 4 MiB".to_owned()));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            (
                CameraStatus::Error,
                "La cámara cerró la descarga del snapshot".to_owned(),
            )
        })?;
        if bytes.len().saturating_add(chunk.len()) > CAMERA_MAX_BYTES {
            return Err((CameraStatus::Limited, "El snapshot supera 4 MiB".to_owned()));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err((
            CameraStatus::Error,
            "La cámara respondió una imagen vacía".to_owned(),
        ));
    }
    crate::shared::thumbnail::validate_camera_frame(&bytes).map_err(|status| {
        if status == crate::shared::workspace::ReadStatus::Limited {
            (
                CameraStatus::Limited,
                "El snapshot supera 1920 × 1080".to_owned(),
            )
        } else {
            (
                CameraStatus::Error,
                "El snapshot de cámara está dañado".to_owned(),
            )
        }
    })?;
    Ok(Bytes::from(bytes))
}
