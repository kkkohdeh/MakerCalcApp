//! Temporary LAN discovery; candidates never become telemetry or persisted state.

use futures_util::StreamExt;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{TcpSocket, UdpSocket},
    runtime::Handle,
    sync::{Semaphore, mpsc, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

use crate::config::Protocol;
use crate::i18n::t;

const MAX_CANDIDATES: usize = 256;
const MAX_BODY: usize = 64 * 1024;
const PARALLELISM: usize = 16;

#[derive(Clone)]
pub(crate) struct NetworkInterface {
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) addresses: Vec<(IpAddr, u8)>,
}

fn is_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn ipv4_mask(prefix: u8) -> Option<u32> {
    (1..=32)
        .contains(&prefix)
        .then(|| u32::MAX << (32 - prefix))
}

fn same_subnet(local: IpAddr, prefix: u8, target: IpAddr) -> bool {
    match (local, target) {
        (IpAddr::V4(local), IpAddr::V4(target)) => {
            let Some(mask) = ipv4_mask(prefix) else {
                return false;
            };
            let value = u32::from(target);
            (value & mask) == (u32::from(local) & mask)
                && (prefix > 30 || (value != (value & mask) && value != (value | !mask)))
        }
        (IpAddr::V6(local), IpAddr::V6(target)) if (1..=128).contains(&prefix) => {
            let mask = u128::MAX << (128 - prefix);
            (u128::from(target) & mask) == (u128::from(local) & mask)
        }
        _ => false,
    }
}

impl NetworkInterface {
    fn allows(&self, address: IpAddr) -> bool {
        self.addresses.iter().any(|&(local, prefix)| {
            is_lan_ip(local) && is_lan_ip(address) && same_subnet(local, prefix, address)
        })
    }

    pub(crate) fn default_range(&self) -> String {
        self.addresses
            .iter()
            .find_map(|&(ip, prefix)| {
                let IpAddr::V4(ip) = ip else {
                    return None;
                };
                if !(24..=30).contains(&prefix) {
                    return None;
                }
                if !ip.is_private() && !ip.is_link_local() {
                    return None;
                }
                let mask = ipv4_mask(prefix)?;
                let first = (u32::from(ip) & mask) + 1;
                let last = (u32::from(ip) | !mask) - 1;
                Some(format!(
                    "{} - {}",
                    Ipv4Addr::from(first),
                    Ipv4Addr::from(last)
                ))
            })
            .unwrap_or_default()
    }

    fn source(&self, target: IpAddr) -> Option<IpAddr> {
        self.addresses
            .iter()
            .find(|&&(local, prefix)| {
                // Loopback-to-loopback supports local test batches; real adapters filter loopback.
                ((is_lan_ip(local) && is_lan_ip(target))
                    || (local.is_loopback() && target.is_loopback()))
                    && same_subnet(local, prefix, target)
            })
            .map(|(ip, _)| *ip)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verification {
    Compatible,
    RequiresCredential,
    Unverified,
    Certificate,
    Timeout,
    Unsupported,
}

#[derive(Clone)]
pub(crate) struct Candidate {
    pub(crate) address: SocketAddr,
    pub(crate) host: String,
    pub(crate) protocol: Protocol,
    pub(crate) tls: bool,
    pub(crate) name: String,
    pub(crate) serial: Option<String>,
    pub(crate) uuid: Option<String>,
    pub(crate) announced: bool,
    pub(crate) verification: Verification,
    pub(crate) local_address: Option<IpAddr>,
    pub(crate) issue: Option<String>,
}

impl Candidate {
    pub(crate) fn matches(&self, host: &str, port: u16, protocol: Protocol, tls: bool) -> bool {
        self.protocol == protocol
            && self.tls == tls
            && self.address.port() == port
            && (self
                .host
                .eq_ignore_ascii_case(host.trim().trim_end_matches('.'))
                || host.trim().trim_matches(['[', ']']).parse::<IpAddr>().ok()
                    == Some(self.address.ip()))
    }

    pub(crate) fn caption(&self) -> String {
        match self.verification {
            Verification::Compatible => t!("printers.discovery.status.compatible"),
            Verification::RequiresCredential => t!("printers.discovery.status.requires_credential"),
            Verification::Unverified => t!("printers.discovery.status.unverified"),
            Verification::Certificate => t!("printers.discovery.status.certificate"),
            Verification::Timeout => t!("printers.discovery.status.timeout"),
            Verification::Unsupported => t!("printers.discovery.status.unsupported"),
        }
        .into_owned()
    }
}

fn parse_flashforge(data: &[u8], address: SocketAddr) -> Option<Candidate> {
    if !matches!(data.len(), 276 | 280) {
        return None;
    }
    let number = |offset| u16::from_be_bytes([data[offset], data[offset + 1]]);
    if number(0x86) != 0x2b71 || number(0x8c) != 0x5a02 || number(0x8e) == 0 {
        return None;
    }
    let text = |slice: &[u8]| -> Option<String> {
        let end = slice.iter().position(|&b| b == 0)?;
        let value = std::str::from_utf8(&slice[..end]).ok()?.trim();
        (!value.chars().any(char::is_control)).then(|| value.to_owned())
    };
    let name = text(&data[..0x80])?;
    let serial = text(&data[0x92..0x92 + 32]).filter(|s| !s.is_empty() && s.is_ascii());
    Some(Candidate {
        address: SocketAddr::new(address.ip(), number(0x8e)),
        host: address.ip().to_string(),
        protocol: Protocol::Flashforge,
        tls: false,
        name: name[..name.floor_char_boundary(96)].to_owned(),
        serial,
        uuid: None,
        announced: true,
        verification: if matches!(number(0x88), 0x23 | 0x24 | 0x26 | 0x28 | 0x29) {
            Verification::RequiresCredential
        } else {
            Verification::Unsupported
        },
        local_address: None,
        issue: None,
    })
}

pub(crate) fn valid_moonraker(body: &[u8]) -> bool {
    if body.len() > MAX_BODY {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let result = &value["result"];
    result["klippy_connected"].is_boolean()
        && matches!(
            result["klippy_state"].as_str(),
            Some("ready" | "startup" | "shutdown" | "error" | "disconnected")
        )
        && result["moonraker_version"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && result["components"]
            .as_array()
            .is_some_and(|a| a.iter().all(|v| v.is_string()))
        && result["api_version"]
            .as_array()
            .is_some_and(|a| a.len() == 3 && a.iter().all(|v| v.as_u64().is_some()))
}

fn parse_range(interface: &NetworkInterface, value: &str) -> Result<(u32, u32), String> {
    let (first, last) = value
        .split_once('-')
        .ok_or_else(|| t!("printers.discovery.errors.range_format").into_owned())?;
    let first: Ipv4Addr = first
        .trim()
        .parse()
        .map_err(|_| t!("printers.discovery.errors.invalid_start_ip").into_owned())?;
    let last: Ipv4Addr = last
        .trim()
        .parse()
        .map_err(|_| t!("printers.discovery.errors.invalid_end_ip").into_owned())?;
    let (start, end) = (u32::from(first), u32::from(last));
    if start > end
        || end - start >= 256
        || !interface.allows(first.into())
        || !interface.allows(last.into())
    {
        return Err(t!("printers.discovery.errors.range_scope").into_owned());
    }
    Ok((start, end))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchEnd {
    Finished,
    Cancelled,
}

#[derive(Clone, Default)]
pub(crate) struct DiscoveryView {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) errors: Vec<String>,
    pub(crate) running: bool,
    pub(crate) end: Option<SearchEnd>,
    pub(crate) checked: usize,
    pub(crate) total: usize,
    pub(crate) retried: bool,
    pending: Vec<Candidate>,
}

impl DiscoveryView {
    pub(crate) fn stop(&mut self) {
        if self.running {
            self.running = false;
            self.end = Some(SearchEnd::Cancelled);
        }
    }

    pub(crate) fn can_retry(&self) -> bool {
        !self.running && !self.retried && !self.pending.is_empty()
    }
    fn error(&mut self, value: impl Into<String>) {
        if self.errors.len() < 8 {
            self.errors.push(value.into());
        }
    }
    fn insert(&mut self, candidate: Candidate) {
        let mut candidate = candidate;
        if let Some(index) = self.candidates.iter().position(|old| {
            old.address == candidate.address
                && old.protocol == candidate.protocol
                && old.tls == candidate.tls
        }) {
            let old = self.candidates.remove(index);
            if old.announced && !candidate.announced {
                candidate.host = old.host;
                candidate.name = old.name;
                candidate.serial = old.serial;
                candidate.uuid = old.uuid;
                candidate.announced = true;
                if candidate.protocol == Protocol::Flashforge {
                    candidate.verification = old.verification;
                }
            }
        }
        // Cross-address deduplication needs BOTH UUID evidence and a verified API on both endpoints.
        if candidate.verification == Verification::Compatible
            && candidate.uuid.as_ref().is_some_and(|uuid| !uuid.is_empty())
            && let Some(old) = self.candidates.iter_mut().find(|old| {
                old.verification == Verification::Compatible
                    && old.uuid == candidate.uuid
                    && old.protocol == candidate.protocol
                    && old.address.port() == candidate.address.port()
                    && old.tls == candidate.tls
            })
        {
            if candidate.address.is_ipv4() {
                *old = candidate;
            }
            return;
        }
        if let Some(old) = self.candidates.iter_mut().find(|old| {
            old.address == candidate.address
                && old.protocol == candidate.protocol
                && old.tls == candidate.tls
        }) {
            *old = candidate;
        } else if self.candidates.len() < MAX_CANDIDATES {
            self.candidates.push(candidate);
        } else {
            let limit_error = t!(
                "printers.discovery.errors.candidate_limit",
                count = MAX_CANDIDATES
            )
            .into_owned();
            if !self.errors.iter().any(|e| e == &limit_error) {
                self.error(limit_error);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct DiscoveryHandle {
    runtime: Handle,
    shutdown: watch::Receiver<bool>,
    single: Arc<Semaphore>,
}

pub(crate) struct DiscoveryQuery {
    pub(crate) view: watch::Receiver<Arc<DiscoveryView>>,
    cancel: watch::Sender<bool>,
}

impl Drop for DiscoveryQuery {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

pub(crate) enum Search {
    Announcements,
    Range(String),
    Retry,
}

impl DiscoveryHandle {
    pub(crate) fn new(runtime: &Handle, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            runtime: runtime.clone(),
            shutdown,
            single: Arc::new(Semaphore::new(1)),
        }
    }

    pub(crate) async fn interfaces(&self) -> Result<Vec<NetworkInterface>, String> {
        self.runtime
            .spawn_blocking(native::interfaces)
            .await
            .map_err(|_| t!("printers.discovery.errors.interfaces_query").into_owned())?
    }

    pub(crate) fn start(
        &self,
        interface: NetworkInterface,
        search: Search,
        mut state: DiscoveryView,
    ) -> DiscoveryQuery {
        state.running = true;
        state.end = None;
        state.checked = 0;
        state.total = 0;
        let (sender, view) = watch::channel(Arc::new(state.clone()));
        let (cancel, mut cancelled) = watch::channel(false);
        let mut shutdown = self.shutdown.clone();
        let single = self.single.clone();
        self.runtime.spawn(async move {
            let end = tokio::select! {
                biased;
                _ = stopped(&mut cancelled) => SearchEnd::Cancelled,
                _ = stopped(&mut shutdown) => SearchEnd::Cancelled,
                _ = async {
                    let Ok(_permit) = single.acquire().await else { return; };
                    search_network(interface, search, &mut state, &sender).await;
                } => SearchEnd::Finished,
            };
            state.running = false;
            state.end = Some(end);
            sender.send_replace(Arc::new(state));
        });
        DiscoveryQuery { view, cancel }
    }
}

async fn stopped(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() || receiver.changed().await.is_err() {
            return;
        }
    }
}

async fn search_network(
    interface: NetworkInterface,
    search: Search,
    state: &mut DiscoveryView,
    sender: &watch::Sender<Arc<DiscoveryView>>,
) {
    if !interface
        .addresses
        .iter()
        .any(|&(ip, prefix)| is_lan_ip(ip) && prefix > 0)
    {
        state.error(t!("printers.discovery.errors.no_private_lan"));
        return;
    }
    let retry = matches!(search, Search::Retry);
    let budget = Duration::from_secs(if retry { 180 } else { 60 });
    let deadline = Instant::now() + budget;
    let targets: Box<dyn Iterator<Item = Candidate> + Send> = match search {
        Search::Announcements => {
            state.retried = false;
            state.errors.clear();
            announcements(&interface, state, sender).await;
            state.total = state
                .candidates
                .iter()
                .filter(|c| {
                    c.protocol == Protocol::Moonraker && c.verification != Verification::Unsupported
                })
                .count();
            Box::new(
                state
                    .candidates
                    .iter()
                    .filter(|c| {
                        c.protocol == Protocol::Moonraker
                            && c.verification != Verification::Unsupported
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter(),
            )
        }
        Search::Range(range) => {
            let (first, last) = match parse_range(&interface, &range) {
                Ok(range) => range,
                Err(error) => {
                    state.error(error);
                    return;
                }
            };
            let scope = interface.clone();
            state.retried = false;
            state.errors.clear();
            state.total = (first..=last)
                .filter(|&ip| interface.allows(Ipv4Addr::from(ip).into()))
                .count()
                * 3;
            Box::new(
                (first..=last)
                    .filter(move |&ip| scope.allows(Ipv4Addr::from(ip).into()))
                    .flat_map(|ip| {
                        [7125, 7130, 8898].into_iter().map(move |port| {
                            let address = SocketAddr::new(Ipv4Addr::from(ip).into(), port);
                            Candidate {
                                address,
                                host: address.ip().to_string(),
                                protocol: if port == 8898 {
                                    Protocol::Flashforge
                                } else {
                                    Protocol::Moonraker
                                },
                                tls: port == 7130,
                                name: t!("printers.discovery.unverified_service").into_owned(),
                                serial: None,
                                uuid: None,
                                announced: false,
                                verification: Verification::Unverified,
                                local_address: None,
                                issue: None,
                            }
                        })
                    }),
            )
        }
        Search::Retry => {
            if state.retried || state.pending.is_empty() {
                return;
            }
            state.retried = true;
            state.total = state.pending.len();
            Box::new(std::mem::take(&mut state.pending).into_iter())
        }
    };
    check_batch(&interface, targets, retry, deadline, state, sender).await;
}

async fn check_batch(
    interface: &NetworkInterface,
    mut targets: Box<dyn Iterator<Item = Candidate> + Send>,
    retry: bool,
    deadline: Instant,
    state: &mut DiscoveryView,
    sender: &watch::Sender<Arc<DiscoveryView>>,
) {
    // Clients share connection policy, but never the telemetry client's credentials or pool.
    let clients = interface
        .addresses
        .iter()
        .filter_map(|&(ip, _)| make_client(ip, None, retry).ok())
        .collect::<Vec<_>>();
    if clients.is_empty() {
        state.error(t!("printers.discovery.errors.client_setup"));
        return;
    }
    state.checked = 0;
    state.pending.clear();
    let mut tasks = JoinSet::new();
    let mut active = Vec::new();
    let mut exhausted = false;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    loop {
        while tasks.len() < PARALLELISM && !exhausted {
            if let Some(mut candidate) = targets.next() {
                candidate.local_address = interface.source(candidate.address.ip());
                active.push(candidate.clone());
                let client = clients
                    .iter()
                    .find(|(ip, _)| Some(*ip) == candidate.local_address)
                    .map(|(_, c)| c.clone());
                tasks.spawn(check_candidate(candidate, client, retry));
            } else {
                exhausted = true;
            }
        }
        if tasks.is_empty() {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                // Pending tasks keep their inputs separately below; never lose an unstarted target.
                state.pending.extend(targets);
                state.pending.append(&mut active);
                state.error(t!("printers.discovery.errors.timeout_pending"));
                break;
            }
            _ = ticker.tick() => { sender.send_replace(Arc::new(state.clone())); }
            result = tasks.join_next() => {
                state.checked += 1;
                match result {
                    Some(Ok((candidate, reachable))) => {
                        active.retain(|old: &Candidate| !(old.address == candidate.address && old.tls == candidate.tls && old.protocol == candidate.protocol));
                        if candidate.verification == Verification::Timeout { state.pending.push(candidate.clone()); }
                        if reachable { state.insert(candidate); }
                    }
                    _ => state.error(t!("printers.discovery.errors.probe_failed")),
                }
            }
        }
    }
    // JoinSet owns sockets/futures: cancellation closes them. No detached per-IP jobs.
    tasks.abort_all();
}

fn make_client(
    local: IpAddr,
    pin: Option<(&str, SocketAddr)>,
    retry: bool,
) -> Result<(IpAddr, reqwest::Client), reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .local_address(local)
        .connect_timeout(Duration::from_millis(if retry { 2000 } else { 800 }))
        .timeout(Duration::from_secs(if retry { 3 } else { 2 }));
    if let Some((host, address)) = pin {
        builder = builder.resolve(&host.to_ascii_lowercase(), address);
    }
    builder.build().map(|client| (local, client))
}

pub(crate) fn verification_client(candidate: &Candidate) -> Result<reqwest::Client, String> {
    let local = candidate
        .local_address
        .ok_or_else(|| t!("printers.discovery.errors.missing_source_interface").into_owned())?;
    make_client(local, Some((&candidate.host, candidate.address)), true)
        .map(|(_, c)| c)
        .map_err(|_| t!("printers.discovery.errors.tls_client_setup").into_owned())
}

pub(crate) fn endpoint_url(candidate: &Candidate, path: &str) -> Result<url::Url, String> {
    let mut url = url::Url::parse(if candidate.tls {
        "https://localhost"
    } else {
        "http://localhost"
    })
    .map_err(|_| t!("printers.discovery.errors.invalid_address").into_owned())?;
    if let Ok(ip) = candidate.host.trim_matches(['[', ']']).parse::<IpAddr>() {
        url.set_ip_host(ip)
            .map_err(|_| t!("printers.discovery.errors.invalid_ip").into_owned())?;
    } else {
        url.set_host(Some(&candidate.host))
            .map_err(|_| t!("printers.discovery.errors.invalid_host").into_owned())?;
    }
    url.set_port(Some(candidate.address.port()))
        .map_err(|_| t!("printers.discovery.errors.invalid_port").into_owned())?;
    match url.host() {
        Some(url::Host::Ipv4(ip)) if candidate.address.ip() != IpAddr::V4(ip) => {
            return Err(t!("printers.discovery.errors.host_ip_mismatch").into_owned());
        }
        Some(url::Host::Ipv6(ip)) if candidate.address.ip() != IpAddr::V6(ip) => {
            return Err(t!("printers.discovery.errors.host_ip_mismatch").into_owned());
        }
        _ => {}
    }
    url.set_path(path);
    Ok(url)
}

pub(crate) async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(t!("printers.discovery.errors.response_too_large").into_owned());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| t!("printers.discovery.errors.response_read").into_owned())?;
        if body.len() + chunk.len() > MAX_BODY {
            return Err(t!("printers.discovery.errors.response_too_large").into_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn check_candidate(
    mut candidate: Candidate,
    client: Option<reqwest::Client>,
    retry: bool,
) -> (Candidate, bool) {
    if candidate.protocol == Protocol::Flashforge {
        let Ok(socket) = TcpSocket::new_v4() else {
            candidate.verification = Verification::Timeout;
            return (candidate, false);
        };
        let bound = candidate
            .local_address
            .is_some_and(|ip| socket.bind(SocketAddr::new(ip, 0)).is_ok());
        let reachable = bound
            && matches!(
                timeout(
                    Duration::from_millis(if retry { 2000 } else { 800 }),
                    socket.connect(candidate.address)
                )
                .await,
                Ok(Ok(_))
            );
        candidate.verification = if reachable {
            Verification::Unverified
        } else {
            Verification::Timeout
        };
        return (candidate, reachable);
    }
    let client = if candidate.announced {
        candidate.local_address.and_then(|ip| {
            make_client(ip, Some((&candidate.host, candidate.address)), retry)
                .ok()
                .map(|(_, c)| c)
        })
    } else {
        client
    };
    let Some(client) = client else {
        candidate.verification = Verification::Timeout;
        return (candidate, false);
    };
    let Ok(url) = endpoint_url(&candidate, "/server/info") else {
        candidate.verification = Verification::Unsupported;
        return (candidate, true);
    };
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            candidate.verification = if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                if candidate.announced {
                    Verification::RequiresCredential
                } else {
                    Verification::Unverified
                }
            } else if status.is_success() {
                match bounded_body(response).await {
                    Ok(body) if valid_moonraker(&body) => {
                        if !candidate.announced {
                            candidate.name = "Moonraker / Klipper".into();
                        }
                        Verification::Compatible
                    }
                    Ok(_) => Verification::Unverified,
                    Err(_) => {
                        candidate.issue = Some(
                            t!("printers.discovery.errors.unreadable_or_too_large").into_owned(),
                        );
                        Verification::Unverified
                    }
                }
            } else {
                Verification::Unverified
            };
            (candidate, true)
        }
        Err(error) => {
            // Error strings are examined only in memory; never logged (may contain a URL).
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            let mut certificate = false;
            while let Some(error) = source {
                let text = error.to_string().to_ascii_lowercase();
                certificate |= text.contains("certificate")
                    || text.contains("certificado")
                    || text.contains("800b0109")
                    || text.contains("800b010f")
                    || text.contains("80090325")
                    || text.contains("80090322")
                    || text.contains("sec_e_untrusted_root")
                    || text.contains("sec_e_wrong_principal");
                source = error.source();
            }
            candidate.verification = if certificate {
                Verification::Certificate
            } else {
                Verification::Timeout
            };
            let reachable = certificate || candidate.announced;
            (candidate, reachable)
        }
    }
}

async fn announcements(
    interface: &NetworkInterface,
    state: &mut DiscoveryView,
    sender: &watch::Sender<Arc<DiscoveryView>>,
) {
    let (events, mut receiver) = mpsc::channel(128);
    let overflow = Arc::new(AtomicBool::new(false));
    let browse = native::Query::browse(interface.index, events.clone(), overflow.clone());
    if let Err(error) = &browse {
        state.error(error.clone());
    }
    let mut resolves = Vec::new();
    let mut names = BTreeSet::new();
    let mut sockets = JoinSet::new();
    for &(ip, prefix) in &interface.addresses {
        if let IpAddr::V4(ip) = ip
            && (1..=32).contains(&prefix)
            && (ip.is_private() || ip.is_link_local())
        {
            sockets.spawn(flashforge_udp(ip, prefix, events.clone()));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = ticker.tick() => { sender.send_replace(Arc::new(state.clone())); }
            result = sockets.join_next(), if !sockets.is_empty() => {
                if let Some(Ok(Err(error))) = result { state.error(error); }
            }
            event = receiver.recv() => match event {
                Some(NativeEvent::Service(name)) => {
                    resolves.retain(|q: &native::Query| !q.complete());
                    if names.len() < MAX_CANDIDATES && !names.contains(&name) {
                        if resolves.len() < PARALLELISM {
                            names.insert(name.clone());
                            match native::Query::resolve(interface.index, &name, events.clone(), overflow.clone()) {
                                Ok(query) => resolves.push(query), Err(error) => state.error(error),
                            }
                        } else { state.error(t!("printers.discovery.errors.announcements_pending")); }
                    }
                }
                Some(NativeEvent::Candidate(mut candidate)) => {
                    if interface.allows(candidate.address.ip()) {
                        candidate.local_address = interface.source(candidate.address.ip());
                        state.insert(candidate);
                    }
                }
                Some(NativeEvent::Error) => state.error(t!("printers.discovery.errors.dnssd_resolve")),
                None => break,
            }
        }
    }
    if overflow.load(Ordering::Relaxed) {
        state.error(t!("printers.discovery.errors.too_many_announcements"));
    }
    // Dropping the guards cancels native queries, but Windows owns their context until the final callback.
    drop(browse);
    drop(resolves);
}

enum NativeEvent {
    Service(String),
    Candidate(Candidate),
    Error,
}

#[cfg(windows)]
mod native;

async fn flashforge_udp(
    ip: Ipv4Addr,
    prefix: u8,
    events: mpsc::Sender<NativeEvent>,
) -> Result<(), String> {
    let Some(mask) = ipv4_mask(prefix) else {
        return Ok(());
    };
    let socket = std::net::UdpSocket::bind((ip, 0))
        .map_err(|_| t!("printers.discovery.errors.udp_bind").into_owned())?;
    socket
        .set_nonblocking(true)
        .map_err(|_| t!("printers.discovery.errors.udp_setup").into_owned())?;
    socket
        .set_broadcast(true)
        .map_err(|_| t!("printers.discovery.errors.udp_broadcast").into_owned())?;
    socket
        .set_multicast_ttl_v4(1)
        .map_err(|_| t!("printers.discovery.errors.udp_multicast_ttl").into_owned())?;
    native::multicast_interface(&socket, ip)?;
    let socket = UdpSocket::from_std(socket)
        .map_err(|_| t!("printers.discovery.errors.udp_setup").into_owned())?;
    let broadcast = Ipv4Addr::from(u32::from(ip) | !mask);
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut round = tokio::time::interval(Duration::from_secs(1));
    let mut rounds = 0;
    // One extra byte distinguishes a 2 KiB datagram from a truncated oversized packet.
    let mut buffer = [0; 2049];
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Ok(()),
            _ = round.tick(), if rounds < 2 => {
                rounds += 1;
                let broadcast_result = socket.send_to(&[], (broadcast, 48899)).await;
                let multicast_result = socket.send_to(&[], (Ipv4Addr::new(225, 0, 0, 9), 19000)).await;
                if broadcast_result.is_err() && multicast_result.is_err() {
                    return Err(t!("printers.discovery.errors.udp_send").into_owned());
                }
            }
            received = socket.recv_from(&mut buffer) => {
                let (size, address) =
                    received.map_err(|_| t!("printers.discovery.errors.udp_recv").into_owned())?;
                if size <= 2048 && let Some(candidate) = parse_flashforge(&buffer[..size], address)
                    && events.try_send(NativeEvent::Candidate(candidate)).is_err() {
                    return Err(t!("printers.discovery.errors.udp_overflow").into_owned());
                }
            }
        }
    }
}
