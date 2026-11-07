//! A bounded frame from the printer's MJPEG stream; endpoints stay private to this adapter.
use super::*;
use crate::shared::workspace::{CameraIo, CameraRequest, CameraState};

const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

fn available(detail: &DetailState, state: &PrinterState, id: ResourceId) -> bool {
    state.connection == LanConnection::Connected
        && state.telemetry_ready
        && detail
            .cameras
            .value
            .iter()
            .flatten()
            .any(|camera| camera.id == id && camera.enabled == Some(true))
}

pub(super) async fn run(
    client: reqwest::Client,
    host: String,
    mut io: CameraIo,
    mut details: watch::Receiver<Arc<DetailState>>,
    mut telemetry: watch::Receiver<PrinterState>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut identity = None;
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() {break;},
            changed = telemetry.changed() => {
                if changed.is_err() {break;}
                let state = telemetry.borrow_and_update();
                if identity.is_some_and(|(_,epoch)| epoch != state.connection_epoch) || state.connection != LanConnection::Connected {
                    identity = None;
                    io.state.send_replace(Arc::new(CameraState::empty()));
                }
            }
            changed = details.changed() => {
                if changed.is_err() {break;}
                let detail = details.borrow_and_update();
                if identity.is_some_and(|(id,_)| !available(&detail,&telemetry.borrow(),id)) {
                    identity = None;
                    io.state.send_replace(Arc::new(CameraState::empty()));
                }
            }
            request = io.requests.recv() => {
                let Some(CameraRequest::Load {id}) = request else {break;};
                let epoch = telemetry.borrow().connection_epoch;
                let generation = details.borrow().generation;
                if !available(&details.borrow(),&telemetry.borrow(),id) {
                    io.state.send_replace(Arc::new(CameraState {status:ReadStatus::Unsupported,..CameraState::empty()}));
                    continue;
                }
                identity = Some((id,epoch));
                io.state.send_replace(Arc::new(CameraState {status:ReadStatus::Loading,..CameraState::empty()}));
                let endpoint = format!("http://{host}:8080/?action=stream");
                let frame = tokio::time::timeout(Duration::from_secs(4),fetch_frame(&client,&endpoint));
                tokio::pin!(frame);
                let result = loop {
                    tokio::select! {
                        biased;
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {return;}
                        }
                        changed = telemetry.changed() => {
                            if changed.is_err() {return;}
                            if telemetry.borrow().connection_epoch != epoch || telemetry.borrow().connection != LanConnection::Connected {break None;}
                        }
                        changed = details.changed() => {
                            if changed.is_err() {return;}
                            if details.borrow().generation != generation || !available(&details.borrow(),&telemetry.borrow(),id) {break None;}
                        }
                        result = &mut frame => break Some(result.unwrap_or(Err(ReadStatus::Error))),
                    }
                };
                let state = match result {
                    Some(Ok(frame)) => CameraState {status:ReadStatus::Ready,frame:Some(frame),received_at:Some(Instant::now()),message:None},
                    Some(Err(status)) => CameraState {status,..CameraState::empty()},
                    None => {identity=None;CameraState::empty()},
                };
                io.state.send_replace(Arc::new(state));
            }
        }
    }
    io.state.send_replace(Arc::new(CameraState::empty()));
}

pub(super) async fn fetch_frame(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<Bytes, ReadStatus> {
    let response = client
        .get(endpoint)
        .send()
        .await
        .map_err(|_| ReadStatus::Error)?;
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err(ReadStatus::Denied);
    }
    if !response.status().is_success() {
        return Err(ReadStatus::Error);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ReadStatus::Error)?;
        if body.len().saturating_add(chunk.len()) > MAX_FRAME_BYTES {
            return Err(ReadStatus::Limited);
        }
        body.extend_from_slice(&chunk);
        if let Some(start) = body.windows(2).position(|bytes| bytes == [0xff, 0xd8])
            && let Some(end) = body[start + 2..]
                .windows(2)
                .position(|bytes| bytes == [0xff, 0xd9])
        {
            let bytes = Bytes::copy_from_slice(&body[start..start + end + 4]);
            let image = bytes.clone();
            crate::shared::thumbnail::run_blocking_thumbnail_cpu(move || {
                crate::shared::thumbnail::validate_camera_frame(&image)
            })
            .await
            .map_err(|_| ReadStatus::Error)??;
            return Ok(bytes);
        }
    }
    Err(ReadStatus::Error)
}
