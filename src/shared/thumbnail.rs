//! Bounded, provider-independent thumbnail normalization and Cloud encoding.
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use sha2::{Digest, Sha256};

pub(crate) mod color;

pub(crate) const THUMBNAIL_QUEUE_CAPACITY: usize = 4;
pub(crate) const MAX_PENDING_THUMBNAIL_WAITERS: usize = 4;

pub(crate) static GLOBAL_THUMBNAIL_SEMAPHORE: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(2);

pub(crate) static GLOBAL_THUMBNAIL_CPU_SEMAPHORE: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(2);

/// Run thumbnail CPU work in `spawn_blocking` under the specified semaphore.
/// Acquire the permit first and move it into the closure, so cancellation or timeout
/// cannot release it before blocking work finishes.
pub(crate) async fn run_blocking_thumbnail_cpu_with_semaphore<F, T>(
    semaphore: &'static tokio::sync::Semaphore,
    f: F,
) -> Result<T, ThumbnailError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = semaphore
        .acquire()
        .await
        .map_err(|_| ThumbnailError::Closed)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|_| ThumbnailError::Unavailable)
}

/// Run thumbnail CPU work with the global limit of two concurrent jobs.
pub(crate) async fn run_blocking_thumbnail_cpu<F, T>(f: F) -> Result<T, ThumbnailError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_blocking_thumbnail_cpu_with_semaphore(&GLOBAL_THUMBNAIL_CPU_SEMAPHORE, f).await
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ThumbnailResourceKey {
    pub(crate) printer_id: crate::state::LocalPrinterId,
    pub(crate) session_generation: u64,
    pub(crate) connection_epoch: u64,
    pub(crate) job_generation: u64,
    pub(crate) print_name: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct NormalizedThumbnail {
    pub(crate) key: ThumbnailResourceKey,
    pub(crate) png_bytes: Bytes,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThumbnailError {
    NotFound,
    Oversized,
    Unavailable,
    JobChanged,
    QueueFull,
    Closed,
}

impl std::fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "miniatura no encontrada"),
            Self::Oversized => write!(f, "miniatura excede el tamaño máximo permitido"),
            Self::Unavailable => write!(f, "servicio de miniaturas no disponible"),
            Self::JobChanged => write!(f, "el trabajo de impresión cambió durante la solicitud"),
            Self::QueueFull => write!(f, "cola de miniaturas saturada"),
            Self::Closed => write!(f, "canal de miniaturas cerrado"),
        }
    }
}

impl std::error::Error for ThumbnailError {}

pub(crate) struct ThumbnailRequest {
    pub(crate) key: ThumbnailResourceKey,
    pub(crate) reply:
        tokio::sync::oneshot::Sender<Result<Arc<NormalizedThumbnail>, ThumbnailError>>,
}

#[derive(Clone)]
pub(crate) struct ThumbnailHandle {
    requests: tokio::sync::mpsc::Sender<ThumbnailRequest>,
}

impl ThumbnailHandle {
    pub(crate) fn new(requests: tokio::sync::mpsc::Sender<ThumbnailRequest>) -> Self {
        Self { requests }
    }

    pub(crate) async fn request_active(
        &self,
        key: ThumbnailResourceKey,
    ) -> Result<Arc<NormalizedThumbnail>, ThumbnailError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.requests
            .try_send(ThumbnailRequest {
                key,
                reply: reply_tx,
            })
            .map_err(|err| match err {
                tokio::sync::mpsc::error::TrySendError::Full(_) => ThumbnailError::QueueFull,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => ThumbnailError::Closed,
            })?;
        reply_rx.await.map_err(|_| ThumbnailError::Closed)?
    }
}

pub(crate) struct ThumbnailIo {
    pub(crate) requests: tokio::sync::mpsc::Receiver<ThumbnailRequest>,
}

pub(crate) fn make_thumbnail_channels() -> (ThumbnailHandle, ThumbnailIo) {
    let (requests, request_io) = tokio::sync::mpsc::channel(THUMBNAIL_QUEUE_CAPACITY);
    (
        ThumbnailHandle::new(requests),
        ThumbnailIo {
            requests: request_io,
        },
    )
}

pub(crate) async fn run_thumbnail_actor<F, Fut>(
    mut io: ThumbnailIo,
    mut state: tokio::sync::watch::Receiver<crate::state::PrinterState>,
    fetch_fn: F,
) where
    F: Fn(ThumbnailResourceKey) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<NormalizedThumbnail, ThumbnailError>> + Send + 'static,
{
    type WaiterSender =
        tokio::sync::oneshot::Sender<Result<Arc<NormalizedThumbnail>, ThumbnailError>>;

    let mut cached: Option<Arc<NormalizedThumbnail>> = None;
    let mut waiters: Vec<(ThumbnailResourceKey, WaiterSender)> = Vec::new();
    let mut active_fetch: Option<(ThumbnailResourceKey, tokio::task::AbortHandle)> = None;
    let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::channel::<(
        ThumbnailResourceKey,
        Result<NormalizedThumbnail, ThumbnailError>,
    )>(MAX_PENDING_THUMBNAIL_WAITERS.max(4));

    loop {
        tokio::select! {
            biased;
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                let current = state.borrow_and_update();
                if let Some(c) = &cached
                    && (c.key.connection_epoch != current.connection_epoch
                        || c.key.job_generation != current.job_generation)
                {
                    cached = None;
                }
                if let Some((active_key, abort_handle)) = &active_fetch
                    && (active_key.connection_epoch != current.connection_epoch
                        || active_key.job_generation != current.job_generation)
                {
                    abort_handle.abort();
                    active_fetch = None;
                }
                let mut valid = Vec::new();
                for (key, tx) in waiters {
                    if key.connection_epoch != current.connection_epoch
                        || key.job_generation != current.job_generation
                    {
                        let _ = tx.send(Err(ThumbnailError::JobChanged));
                    } else if !tx.is_closed() {
                        valid.push((key, tx));
                    }
                }
                waiters = valid;
            }
            done = fetch_rx.recv() => {
                let Some((key, result)) = done else {
                    continue;
                };
                if let Some((active_key, _)) = &active_fetch
                    && *active_key == key
                {
                    active_fetch = None;
                }

                let current = state.borrow();
                let is_stale = key.connection_epoch != current.connection_epoch
                    || key.job_generation != current.job_generation;

                if is_stale {
                    let mut still_waiting = Vec::new();
                    for (k, tx) in waiters {
                        if k == key {
                            let _ = tx.send(Err(ThumbnailError::JobChanged));
                        } else {
                            still_waiting.push((k, tx));
                        }
                    }
                    waiters = still_waiting;
                } else {
                    match result {
                        Ok(thumb) => {
                            let arc_thumb = Arc::new(thumb);
                            cached = Some(arc_thumb.clone());
                            let mut still_waiting = Vec::new();
                            for (k, tx) in waiters {
                                if k == key {
                                    let _ = tx.send(Ok(arc_thumb.clone()));
                                } else {
                                    still_waiting.push((k, tx));
                                }
                            }
                            waiters = still_waiting;
                        }
                        Err(err) => {
                            let mut still_waiting = Vec::new();
                            for (k, tx) in waiters {
                                if k == key {
                                    let _ = tx.send(Err(err));
                                } else {
                                    still_waiting.push((k, tx));
                                }
                            }
                            waiters = still_waiting;
                        }
                    }
                }
            }
            req = io.requests.recv() => {
                let Some(req) = req else {
                    break;
                };
                waiters.retain(|(_, tx)| !tx.is_closed());

                if req.key.print_name.is_empty() {
                    let _ = req.reply.send(Err(ThumbnailError::NotFound));
                    continue;
                }

                let current = state.borrow();
                if let Some(c) = &cached {
                    if c.key.connection_epoch != current.connection_epoch
                        || c.key.job_generation != current.job_generation
                    {
                        cached = None;
                    } else if c.key == req.key {
                        let _ = req.reply.send(Ok(c.clone()));
                        continue;
                    }
                }

                if current.connection_epoch != req.key.connection_epoch
                    || current.job_generation != req.key.job_generation
                {
                    let _ = req.reply.send(Err(ThumbnailError::JobChanged));
                    continue;
                }

                if waiters.len() >= MAX_PENDING_THUMBNAIL_WAITERS {
                    let _ = req.reply.send(Err(ThumbnailError::QueueFull));
                    continue;
                }

                if let Some((active_key, abort_handle)) = active_fetch.take() {
                    if active_key != req.key {
                        abort_handle.abort();
                    } else {
                        active_fetch = Some((active_key, abort_handle));
                    }
                }

                let is_different_key = waiters.iter().any(|(k, _)| *k != req.key);
                if is_different_key {
                    let mut same = Vec::new();
                    for (k, tx) in waiters {
                        if k == req.key {
                            same.push((k, tx));
                        } else {
                            let _ = tx.send(Err(ThumbnailError::JobChanged));
                        }
                    }
                    waiters = same;
                    cached = None;
                }

                let is_already_fetching =
                    active_fetch.as_ref().is_some_and(|(k, _)| *k == req.key);
                waiters.push((req.key.clone(), req.reply));

                if !is_already_fetching {
                    let fetch_tx = fetch_tx.clone();
                    let key = req.key;
                    let fetch_key = key.clone();
                    let fetch_impl = fetch_fn.clone();
                    let handle = tokio::spawn(async move {
                        let Ok(_permit) = GLOBAL_THUMBNAIL_SEMAPHORE.acquire().await else {
                            let _ = fetch_tx.send((fetch_key, Err(ThumbnailError::Closed))).await;
                            return;
                        };
                        let res = tokio::time::timeout(
                            std::time::Duration::from_secs(4),
                            fetch_impl(fetch_key.clone()),
                        )
                        .await;
                        let result = match res {
                            Ok(inner) => inner,
                            Err(_) => Err(ThumbnailError::Unavailable),
                        };
                        let _ = fetch_tx.send((fetch_key, result)).await;
                    });
                    active_fetch = Some((key, handle.abort_handle()));
                }
            }
        }
    }

    if let Some((_, abort_handle)) = active_fetch {
        abort_handle.abort();
    }
    for (_, tx) in waiters {
        let _ = tx.send(Err(ThumbnailError::Closed));
    }
}

#[derive(Clone)]
pub(crate) struct CloudThumbnail {
    pub sha256: String,
    pub webp_base64: String,
}

fn thumbnail_decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(16 * 1024 * 1024);
    limits
}

/// Encodes an already-normalized PNG for the bounded Cloud thumbnail message.
pub(crate) fn thumbnail_for_cloud(png: &[u8], max_bytes: usize) -> Option<CloudThumbnail> {
    if png.is_empty() || png.len() > 1024 * 1024 || max_bytes == 0 {
        return None;
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png);
    reader.limits(thumbnail_decode_limits());
    let image = reader.decode().ok()?;
    let image = if image.width() > 128 || image.height() > 128 {
        image.thumbnail(128, 128)
    } else {
        image
    }
    .into_rgba8();
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .encode(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    if webp.len() > max_bytes.min(40_960) {
        return None;
    }
    Some(CloudThumbnail {
        sha256: format!("{:x}", Sha256::digest(&webp)),
        webp_base64: base64::engine::general_purpose::STANDARD.encode(webp),
    })
}

pub(crate) fn embedded_thumbnail_image(content: &str) -> Option<(Bytes, u32, u32)> {
    let mut lines = content.lines().take(16_384);
    let mut best: Option<(u32, Bytes, u32, u32)> = None;
    while let Some(line) = lines.next() {
        let trimmed_line = line.trim();
        let Some(header) = trimmed_line
            .strip_prefix("; thumbnail begin ")
            .or_else(|| trimmed_line.strip_prefix("; thumbnail_PNG begin "))
            .or_else(|| trimmed_line.strip_prefix("; thumbnail_JPG begin "))
        else {
            continue;
        };
        let mut parts = header.split_ascii_whitespace();
        let Some((width, height)) = parts.next().and_then(|s| s.split_once('x')) else {
            continue;
        };
        let (Ok(width), Ok(height), Some(encoded_size)) = (
            width.parse::<u32>(),
            height.parse::<u32>(),
            parts.next().and_then(|s| s.parse::<usize>().ok()),
        ) else {
            continue;
        };
        if width == 0 || height == 0 || width > 1024 || height > 1024 || encoded_size > 512 * 1024 {
            continue;
        }
        let mut encoded = String::with_capacity(encoded_size);
        let mut complete = false;
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed.eq_ignore_ascii_case("; thumbnail end")
                || trimmed.eq_ignore_ascii_case("; thumbnail_PNG end")
                || trimmed.eq_ignore_ascii_case("; thumbnail_JPG end")
            {
                complete = true;
                break;
            }
            if trimmed.is_empty() || trimmed == ";" {
                continue;
            }
            let Some(payload) = trimmed.strip_prefix(';') else {
                break;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            if encoded.len().saturating_add(payload.len()) > encoded_size {
                break;
            }
            encoded.push_str(payload);
        }
        if !complete || encoded.len() != encoded_size {
            continue;
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            continue;
        };
        if let Some((normalized, w, h)) = normalize_thumbnail_image(&bytes) {
            let area = width.saturating_mul(height);
            if best
                .as_ref()
                .is_none_or(|(best_area, _, _, _)| area > *best_area)
            {
                best = Some((area, normalized, w, h));
            }
        }
    }
    best.map(|(_, bytes, w, h)| (bytes, w, h))
}

pub(crate) fn embedded_thumbnail(content: &str) -> Option<Bytes> {
    embedded_thumbnail_image(content).map(|(bytes, _, _)| bytes)
}

/// Converts near-black background to transparent alpha in-place.
pub(crate) fn make_black_background_transparent(img: &mut image::RgbaImage) {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return;
    }
    let corners = [
        img.get_pixel(0, 0),
        img.get_pixel(w.saturating_sub(1), 0),
        img.get_pixel(0, h.saturating_sub(1)),
        img.get_pixel(w.saturating_sub(1), h.saturating_sub(1)),
    ];
    let dark_corners = corners
        .iter()
        .filter(|p| p[0] <= 32 && p[1] <= 32 && p[2] <= 32)
        .count();
    if dark_corners < 3 {
        return;
    }

    for pixel in img.pixels_mut() {
        let [r, g, b, ref mut a] = pixel.0;
        let max_val = r.max(g).max(b);
        if max_val <= 10 {
            *a = 0;
        } else if max_val < 26 {
            let factor = (max_val - 10) as f32 / 16.0;
            *a = ((*a as f32) * factor) as u8;
        }
    }
}

/// Validates image bounds, decodes BMP/PNG/JPEG, makes solid black background
/// transparent, and re-encodes to normalized PNG bytes with width and height.
pub(crate) fn normalize_thumbnail_image(bytes: &[u8]) -> Option<(Bytes, u32, u32)> {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let format = reader.format()?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Bmp
    ) {
        return None;
    }
    reader.limits(thumbnail_decode_limits());
    let dyn_img = reader.decode().ok()?;
    let (width, height) = (dyn_img.width(), dyn_img.height());
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba = dyn_img.into_rgba8();
    make_black_background_transparent(&mut rgba);

    let mut png_buf = Cursor::new(Vec::new());
    rgba.write_to(&mut png_buf, image::ImageFormat::Png).ok()?;
    Some((Bytes::from(png_buf.into_inner()), width, height))
}

/// The same frame budget applies to every camera integration.
pub(crate) fn validate_camera_frame(
    bytes: &[u8],
) -> Result<(), crate::shared::workspace::ReadStatus> {
    use crate::shared::workspace::ReadStatus;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(ReadStatus::Limited);
    }
    let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ReadStatus::Error)?
        .into_dimensions()
        .map_err(|_| ReadStatus::Error)?;
    if width == 0 || height == 0 {
        return Err(ReadStatus::Error);
    }
    if width > 1920 || height > 1080 {
        return Err(ReadStatus::Limited);
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn normalize_thumbnail_png(bytes: &[u8]) -> Option<Bytes> {
    normalize_thumbnail_image(bytes).map(|(bytes, _, _)| bytes)
}
