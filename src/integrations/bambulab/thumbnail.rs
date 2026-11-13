//! Thumbnail of the job being printed. MQTT does not carry it: it is the plate preview
//! inside the sliced `.gcode.3mf`, which the printer keeps on its storage and serves
//! over implicit FTPS on port 990.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;
use tokio::sync::{Semaphore, watch};

use crate::shared::thumbnail::{
    NormalizedThumbnail, ThumbnailError, ThumbnailResourceKey, normalize_thumbnail_image,
    run_blocking_thumbnail_cpu,
};
use crate::state::PrinterState;

/// `slice_info.config` and the plate preview are a few hundred KiB at most.
const MAX_ENTRY: usize = 1024 * 1024;
const SLICE_INFO: &str = "Metadata/slice_info.config";
/// A failed attempt is repeated no sooner than this. The file of a cloud print is
/// still being downloaded to the printer while the job prepares.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Sliced files held in memory at once, across every printer.
static DOWNLOADS: Semaphore = Semaphore::const_new(2);

type Image = (Bytes, u32, u32);
type Outcome = Option<(Result<Image, ThumbnailError>, Instant)>;
/// Connection epoch, job generation and job name.
type Job = (u64, u64, String);

/// One download per job. The shared actor gives each fetch a few seconds, less than a
/// large file takes, and a blocking transfer cannot be cancelled: the first request
/// starts the download and every request of that job waits on its single result.
#[derive(Clone)]
pub(super) struct Thumbnails(Arc<Source>);

struct Source {
    host: String,
    serial: String,
    access_code: String,
    /// Plate the latest report names; 0 when it names none.
    plate: AtomicU32,
    job: Mutex<Option<(Job, watch::Receiver<Outcome>)>>,
    shutdown: watch::Receiver<bool>,
    state: watch::Receiver<PrinterState>,
}

impl Thumbnails {
    pub(super) fn new(
        host: String,
        serial: &str,
        access_code: &str,
        shutdown: watch::Receiver<bool>,
        state: watch::Receiver<PrinterState>,
    ) -> Self {
        Self(Arc::new(Source {
            host,
            serial: serial.to_owned(),
            access_code: access_code.to_owned(),
            plate: AtomicU32::new(0),
            job: Mutex::new(None),
            shutdown,
            state,
        }))
    }

    /// Keeps the plate being printed: `plate_idx`, or the `Metadata/plate_N.gcode` the
    /// printer runs from a 3MF. A file with several plates lists them all in
    /// `slice_info.config`, so only the report tells which one is printing.
    pub(super) fn observe(&self, report: &Value) {
        let print = &report["print"];
        let plate = print["plate_idx"]
            .as_u64()
            .filter(|plate| *plate > 0)
            .or_else(|| {
                let (_, plate) = print["gcode_file"]
                    .as_str()?
                    .rsplit_once("Metadata/plate_")?;
                plate.strip_suffix(".gcode")?.parse().ok()
            })
            .and_then(|plate| u32::try_from(plate).ok())
            .unwrap_or(0);
        self.0.plate.store(plate, Ordering::Relaxed);
    }

    pub(super) async fn fetch(
        self,
        key: ThumbnailResourceKey,
    ) -> Result<NormalizedThumbnail, ThumbnailError> {
        let mut outcome = self.download(&key);
        let (png_bytes, width, height) = outcome
            .wait_for(Option::is_some)
            .await
            .map_err(|_| ThumbnailError::Unavailable)?
            .clone()
            .ok_or(ThumbnailError::Unavailable)?
            .0?;
        Ok(NormalizedThumbnail {
            key,
            png_bytes,
            width,
            height,
        })
    }

    fn download(&self, key: &ThumbnailResourceKey) -> watch::Receiver<Outcome> {
        let job = (
            key.connection_epoch,
            key.job_generation,
            key.print_name.clone(),
        );
        let mut current = self.0.job.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((running, outcome)) = current.as_ref()
            && *running == job
            && !matches!(&*outcome.borrow(), Some((Err(_), at)) if at.elapsed() >= RETRY_AFTER)
        {
            return outcome.clone();
        }
        let (sender, outcome) = watch::channel(None);
        *current = Some((job.clone(), outcome.clone()));
        let source = self.0.clone();
        tokio::spawn(async move {
            let result = source.read(job).await;
            sender.send_replace(Some((result, Instant::now())));
        });
        outcome
    }
}

impl Source {
    /// A new job, a reconnection or the driver stopping ends the transfer. Otherwise
    /// it keeps a download slot, and the runtime waits for it when the application exits.
    fn wanted(&self, job: &Job) -> bool {
        let state = self.state.borrow();
        !*self.shutdown.borrow() && state.connection_epoch == job.0 && state.job_generation == job.1
    }

    async fn read(self: Arc<Self>, job: Job) -> Result<Image, ThumbnailError> {
        let _permit = DOWNLOADS
            .acquire()
            .await
            .map_err(|_| ThumbnailError::Closed)?;
        if !self.wanted(&job) {
            return Err(ThumbnailError::JobChanged);
        }
        let plate = match self.plate.load(Ordering::Relaxed) {
            0 => None,
            plate => Some(plate),
        };
        let archive = tokio::task::spawn_blocking(move || {
            let cancelled = || !self.wanted(&job);
            download(
                &self.host,
                &self.serial,
                &self.access_code,
                &job.2,
                &cancelled,
            )
        })
        .await
        .map_err(|_| ThumbnailError::Unavailable)??;
        run_blocking_thumbnail_cpu(move || {
            plate_image(&archive, plate).and_then(|png| normalize_thumbnail_image(&png))
        })
        .await?
        .ok_or(ThumbnailError::NotFound)
    }
}

#[cfg(windows)]
use ftps::download;

/// The data connection must resume the control connection's TLS session; the
/// application only does that through SChannel.
#[cfg(not(windows))]
fn download(
    _host: &str,
    _serial: &str,
    _access_code: &str,
    _name: &str,
    _cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, ThumbnailError> {
    Err(ThumbnailError::Unavailable)
}

/// The preview of the reported plate, or of the one `slice_info.config` names.
fn plate_image(archive: &[u8], reported: Option<u32>) -> Option<Vec<u8>> {
    let image = |plate: u32| entry(archive, &format!("Metadata/plate_{plate}.png"));
    reported.and_then(image).or_else(|| {
        let slice_info = String::from_utf8(entry(archive, SLICE_INFO)?).ok()?;
        image(plate_index(&slice_info)?)
    })
}

/// `slice_info.config` names the sliced plate as `<metadata key="index" value="N"/>`.
fn plate_index(slice_info: &str) -> Option<u32> {
    slice_info.split("<metadata").skip(1).find_map(|tag| {
        let tag = &tag[..tag.find('>')?];
        if !tag.contains(r#"key="index""#) {
            return None;
        }
        tag.split_once(r#"value=""#)?
            .1
            .split_once('"')?
            .0
            .parse()
            .ok()
    })
}

/// One member of a ZIP archive, found through the central directory: members written
/// as a stream leave their sizes out of the local header.
fn entry(archive: &[u8], name: &str) -> Option<Vec<u8>> {
    // The end record is the last 22 bytes plus its comment; a signature inside the
    // comment is told apart by the comment length.
    let last = archive.len().checked_sub(22)?;
    let end = (last.saturating_sub(usize::from(u16::MAX))..=last)
        .rev()
        .find(|&at| {
            archive[at..].starts_with(b"PK\x05\x06")
                && u16_at(archive, at + 20) == Some(archive.len() - at - 22)
        })?;
    let mut at = u32_at(archive, end + 16)?;
    for _ in 0..u16_at(archive, end + 10)? {
        if !archive.get(at..)?.starts_with(b"PK\x01\x02") {
            return None;
        }
        let name_len = u16_at(archive, at + 28)?;
        if archive.get(at + 46..at + 46 + name_len)? != name.as_bytes() {
            at += 46 + name_len + u16_at(archive, at + 30)? + u16_at(archive, at + 32)?;
            continue;
        }
        let compressed = u32_at(archive, at + 20)?;
        let size = u32_at(archive, at + 24)?;
        let local = u32_at(archive, at + 42)?;
        if size > MAX_ENTRY || !archive.get(local..)?.starts_with(b"PK\x03\x04") {
            return None;
        }
        let start = local + 30 + u16_at(archive, local + 26)? + u16_at(archive, local + 28)?;
        let data = archive.get(start..start.checked_add(compressed)?)?;
        return match u16_at(archive, at + 10)? {
            0 if compressed == size => Some(data.to_vec()),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, MAX_ENTRY)
                .ok()
                .filter(|member| member.len() == size),
            _ => None,
        };
    }
    None
}

fn u16_at(bytes: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}

fn u32_at(bytes: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)).ok()
}

/// Implicit FTPS as the printers speak it: TLS from the first byte, protected data
/// connections in passive mode only, and every data connection resuming the control
/// connection's TLS session, or the printer refuses it with 522.
#[cfg(windows)]
mod ftps {
    use std::io::{self, BufRead, Read, Write};
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    use std::time::{Duration, Instant};

    use schannel::cert_context::CertContext;
    use schannel::cert_store::{CertAdd, CertStore, Memory};
    use schannel::schannel_cred::{Direction, Protocol, SchannelCred};
    use schannel::tls_stream::{Builder, HandshakeError, TlsStream};

    use super::super::mqtt::{CA_BUNDLE, USER};
    use crate::shared::thumbnail::ThumbnailError;

    const PORT: u16 = 990;
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// A sliced file carries the whole G-code; larger ones are not fetched for a preview.
    const MAX_ARCHIVE: usize = 64 * 1024 * 1024;
    /// Bounds a printer that sends the file slower than the socket timeout notices.
    const MAX_TRANSFER: Duration = Duration::from_secs(120);
    const MAX_LINE: u64 = 1024;
    const MAX_REPLY_LINES: usize = 32;

    /// The sliced file of the job `name`. `cancelled` is polled during the transfer.
    pub(super) fn download(
        host: &str,
        serial: &str,
        access_code: &str,
        name: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, ThumbnailError> {
        // The name goes into FTP commands, where a line break would end them early.
        if name.chars().any(char::is_control) {
            return Err(ThumbnailError::NotFound);
        }
        let mut session = Session::open(host, serial, access_code).map_err(unavailable)?;
        for path in candidates(name) {
            if let Some(archive) = session.retrieve(&path, cancelled)? {
                return Ok(archive);
            }
        }
        Err(ThumbnailError::NotFound)
    }

    /// `/cache/` holds what the printer downloads for cloud prints and `/` what is sent
    /// over LAN; with both under one name, the cloud copy wins. The job name may already
    /// end in `.gcode` from `name.gcode.3mf`.
    fn candidates(name: &str) -> Vec<String> {
        let files = if name.ends_with(".3mf") {
            vec![name.to_owned()]
        } else {
            vec![format!("{name}.gcode.3mf"), format!("{name}.3mf")]
        };
        ["/cache/", "/"]
            .iter()
            .flat_map(|dir| files.iter().map(move |file| format!("{dir}{file}")))
            .collect()
    }

    fn unavailable(_: io::Error) -> ThumbnailError {
        ThumbnailError::Unavailable
    }

    fn tcp(address: SocketAddr) -> io::Result<TcpStream> {
        let stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        Ok(stream)
    }

    /// SChannel resumes a session only under the credential that opened it, so every
    /// connection of a session shares this one.
    struct Tls {
        credential: SchannelCred,
        roots: CertStore,
        serial: String,
    }

    impl Tls {
        fn new(serial: &str) -> io::Result<Self> {
            let mut roots = Memory::new()?.into_store();
            for block in CA_BUNDLE
                .split_inclusive("-----END CERTIFICATE-----")
                .filter(|block| block.contains("-----BEGIN CERTIFICATE-----"))
            {
                roots.add_cert(
                    &CertContext::from_pem(block.trim())?,
                    CertAdd::ReplaceExisting,
                )?;
            }
            // Some firmware never answers a TLS 1.3 ClientHello.
            let credential = SchannelCred::builder()
                .enabled_protocols(&[Protocol::Tls12])
                .acquire(Direction::Outbound)?;
            Ok(Self {
                credential,
                roots,
                serial: serial.to_owned(),
            })
        }

        /// Same trust as the MQTT connection: the printer serial, issued under one of
        /// the bundled Bambu Lab roots and never a system root.
        fn wrap(&self, stream: TcpStream) -> io::Result<TlsStream<TcpStream>> {
            let roots = self.roots.clone();
            Builder::new()
                .domain(&self.serial)
                .cert_store(self.roots.clone())
                .verify_callback(move |validation| {
                    validation.result()?;
                    let chain = validation
                        .chain()
                        .ok_or_else(|| io::Error::other("no certificate chain"))?;
                    if chain
                        .certificates()
                        .any(|cert| roots.certs().any(|root| root == cert))
                    {
                        Ok(())
                    } else {
                        Err(io::Error::other("certificate outside the Bambu Lab roots"))
                    }
                })
                .connect(self.credential.clone(), stream)
                .map_err(|error| match error {
                    HandshakeError::Failure(error) => error,
                    HandshakeError::Interrupted(_) => io::ErrorKind::TimedOut.into(),
                })
        }
    }

    struct Session {
        control: TlsStream<TcpStream>,
        address: SocketAddr,
        tls: Tls,
    }

    impl Session {
        fn open(host: &str, serial: &str, access_code: &str) -> io::Result<Self> {
            let address = (host, PORT)
                .to_socket_addrs()?
                .next()
                .ok_or(io::ErrorKind::NotFound)?;
            let tls = Tls::new(serial)?;
            let control = tls.wrap(tcp(address)?)?;
            let mut session = Self {
                control,
                address,
                tls,
            };
            session.expect(None, 220)?;
            session.expect(Some(&format!("USER {USER}")), 331)?;
            session.expect(Some(&format!("PASS {access_code}")), 230)?;
            for command in ["TYPE I", "PBSZ 0", "PROT P"] {
                session.expect(Some(command), 200)?;
            }
            Ok(session)
        }

        /// The file at `path`, or `None` when the printer has no such file.
        fn retrieve(
            &mut self,
            path: &str,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Option<Vec<u8>>, ThumbnailError> {
            let (code, reply) = self
                .command(Some(&format!("SIZE {path}")))
                .map_err(unavailable)?;
            match code {
                213 => {}
                550 => return Ok(None),
                _ => return Err(ThumbnailError::Unavailable),
            }
            let size: usize = reply[4..]
                .trim()
                .parse()
                .map_err(|_| ThumbnailError::Unavailable)?;
            if size > MAX_ARCHIVE {
                return Err(ThumbnailError::Oversized);
            }
            self.transfer(path, size, cancelled)
                .map(Some)
                .map_err(unavailable)
        }

        fn transfer(
            &mut self,
            path: &str,
            size: usize,
            cancelled: &dyn Fn() -> bool,
        ) -> io::Result<Vec<u8>> {
            let reply = self.expect(Some("PASV"), 227)?;
            let port = pasv_port(&reply).ok_or_else(|| io::Error::other("no data port"))?;
            // The address in the reply is often 0.0.0.0: the port is on the control host.
            let data = tcp(SocketAddr::new(self.address.ip(), port))?;
            let (code, _) = self.command(Some(&format!("RETR {path}")))?;
            if code != 150 && code != 125 {
                return Err(io::Error::other(format!("RETR answered {code}")));
            }
            // The printer starts TLS on the data connection only after its 150.
            let mut data = self.tls.wrap(data)?;
            let started = Instant::now();
            let mut archive = Vec::with_capacity(size);
            let mut chunk = vec![0; 64 * 1024];
            // Reading exactly `size` bytes does not depend on how the printer closes, and
            // the session ends with the file, so the closing reply is not awaited.
            while archive.len() < size {
                if cancelled() || started.elapsed() > MAX_TRANSFER {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let wanted = chunk.len().min(size - archive.len());
                match data.read(&mut chunk[..wanted])? {
                    0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                    read => archive.extend_from_slice(&chunk[..read]),
                }
            }
            Ok(archive)
        }

        fn expect(&mut self, line: Option<&str>, code: u16) -> io::Result<String> {
            let (answered, reply) = self.command(line)?;
            if answered == code {
                return Ok(reply);
            }
            // The reply text is left out: it can echo the command, and PASS carries the
            // access code.
            Err(io::Error::other(format!("answered {answered}, not {code}")))
        }

        /// Sends `line`, if any, and reads the reply's code and final line.
        fn command(&mut self, line: Option<&str>) -> io::Result<(u16, String)> {
            if let Some(line) = line {
                self.control.write_all(format!("{line}\r\n").as_bytes())?;
                self.control.flush()?;
            }
            for _ in 0..MAX_REPLY_LINES {
                let mut reply = String::new();
                if (&mut self.control).take(MAX_LINE).read_line(&mut reply)? == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                // A reply ends on the line that starts with its code and a space.
                if reply.as_bytes().get(3) == Some(&b' ')
                    && let Some(code) = reply.get(..3).and_then(|code| code.parse().ok())
                {
                    return Ok((code, reply));
                }
            }
            Err(io::Error::other("reply too long"))
        }
    }

    fn pasv_port(reply: &str) -> Option<u16> {
        let fields = reply.split_once('(')?.1.split_once(')')?.0;
        let fields = fields
            .split(',')
            .map(|field| field.trim().parse().ok())
            .collect::<Option<Vec<u8>>>()?;
        let [_, _, _, _, high, low] = fields[..] else {
            return None;
        };
        Some(u16::from_be_bytes([high, low]))
    }
}
