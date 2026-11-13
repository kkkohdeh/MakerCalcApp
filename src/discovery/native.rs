use super::*;
use crate::i18n::t;
use std::{ffi::c_void, os::windows::io::AsRawSocket};
use windows_sys::Win32::{
    Foundation::{DNS_REQUEST_PENDING, ERROR_BUFFER_OVERFLOW, ERROR_CANCELLED},
    NetworkManagement::{Dns::*, IpHelper::*, Ndis::IfOperStatusUp},
    Networking::WinSock::*,
};

pub(super) fn interfaces() -> Result<Vec<NetworkInterface>, String> {
    let mut bytes = 16 * 1024_u32;
    for _ in 0..3 {
        if bytes > 256 * 1024 {
            break;
        }
        // u64 backing storage provides the alignment required by IP_ADAPTER_ADDRESSES_LH.
        let mut buffer = vec![0_u64; (bytes as usize).div_ceil(8)];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: aligned writable storage of the declared size, with no retained native pointers.
        let result = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                std::ptr::null(),
                first,
                &mut bytes,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if result != 0 {
            return Err(t!("printers.discovery.errors.interfaces_list").into_owned());
        }
        let mut entries = Vec::new();
        let mut next = first;
        // SAFETY: all list nodes/strings are owned by buffer for the duration of traversal.
        unsafe {
            while !next.is_null() && entries.len() < 64 {
                let adapter = &*next;
                next = adapter.Next;
                if adapter.OperStatus != IfOperStatusUp || adapter.IfType == 24 {
                    continue;
                }
                let mut addresses = Vec::new();
                let mut unicast = adapter.FirstUnicastAddress;
                let mut count = 0;
                while !unicast.is_null() && count < 64 {
                    count += 1;
                    let entry = &*unicast;
                    unicast = entry.Next;
                    let raw = entry.Address.lpSockaddr;
                    if raw.is_null() {
                        continue;
                    }
                    let ip = match (*raw).sa_family {
                        AF_INET
                            if entry.Address.iSockaddrLength as usize
                                >= size_of::<SOCKADDR_IN>() =>
                        {
                            Some(IpAddr::V4(Ipv4Addr::from(
                                (*(raw.cast::<SOCKADDR_IN>()))
                                    .sin_addr
                                    .S_un
                                    .S_addr
                                    .to_ne_bytes(),
                            )))
                        }
                        AF_INET6
                            if entry.Address.iSockaddrLength as usize
                                >= size_of::<SOCKADDR_IN6>() =>
                        {
                            Some(IpAddr::V6(Ipv6Addr::from(
                                (*(raw.cast::<SOCKADDR_IN6>())).sin6_addr.u.Byte,
                            )))
                        }
                        _ => None,
                    };
                    if let Some(ip) = ip
                        && !ip.is_loopback()
                        && !ip.is_unspecified()
                    {
                        addresses.push((ip, entry.OnLinkPrefixLength));
                    }
                }
                let ipv4_index = adapter.Anonymous1.Anonymous.IfIndex;
                let index = if ipv4_index == 0 {
                    adapter.Ipv6IfIndex
                } else {
                    ipv4_index
                };
                if !addresses.is_empty() && index != 0 {
                    entries.push(NetworkInterface {
                        index,
                        name: wide(adapter.FriendlyName).unwrap_or_else(|| {
                            t!("printers.discovery.network_interface").into_owned()
                        }),
                        addresses,
                    });
                }
            }
        }
        return Ok(entries);
    }
    Err(t!("printers.discovery.errors.interfaces_limit").into_owned())
}

pub(super) fn multicast_interface(
    socket: &std::net::UdpSocket,
    ip: Ipv4Addr,
) -> Result<(), String> {
    let bytes = ip.octets();
    // SAFETY: socket is live, the IPv4 address buffer has exactly the length passed to WinSock.
    let result = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            IPPROTO_IP,
            IP_MULTICAST_IF,
            bytes.as_ptr(),
            bytes.len() as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(t!("printers.discovery.errors.multicast_interface").into_owned())
    }
}

unsafe fn wide(pointer: *const u16) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    for len in 0..256 {
        // SAFETY: callers supply a native NUL-terminated UTF-16 string valid during their callback.
        if unsafe { *pointer.add(len) } == 0 {
            let value =
                String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()?;
            return (!value.chars().any(char::is_control)).then_some(value);
        }
    }
    None
}

enum Request {
    Browse(DNS_SERVICE_BROWSE_REQUEST),
    Resolve(DNS_SERVICE_RESOLVE_REQUEST),
}
struct Storage {
    cancel: DNS_SERVICE_CANCEL,
    name: Vec<u16>,
    request: Option<Request>,
}
// SAFETY: the opaque pointers are used only by Windows. Rust accesses Storage under its mutex;
// callbacks touch only State's channel/atomics. Storage never moves after registration.
unsafe impl Send for Storage {}
struct State {
    storage: Mutex<Storage>,
    events: mpsc::Sender<NativeEvent>,
    overflow: Arc<AtomicBool>,
    complete: AtomicBool,
    index: u32,
}
pub(super) struct Query {
    state: Arc<State>,
    browse: bool,
}

impl Query {
    pub(super) fn browse(
        index: u32,
        events: mpsc::Sender<NativeEvent>,
        overflow: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::start(index, "_moonraker._tcp.local.", true, events, overflow)
    }
    pub(super) fn resolve(
        index: u32,
        name: &str,
        events: mpsc::Sender<NativeEvent>,
        overflow: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::start(index, name, false, events, overflow)
    }
    fn start(
        index: u32,
        name: &str,
        browse: bool,
        events: mpsc::Sender<NativeEvent>,
        overflow: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let state = Arc::new(State {
            storage: Mutex::new(Storage {
                cancel: DNS_SERVICE_CANCEL::default(),
                name: name.encode_utf16().chain([0]).collect(),
                request: None,
            }),
            events,
            overflow,
            complete: AtomicBool::new(false),
            index,
        });
        let context = Arc::into_raw(state.clone());
        let result = {
            let Ok(mut storage) = state.storage.lock() else {
                // SAFETY: registration has not happened, so Windows cannot own the raw reference.
                unsafe {
                    drop(Arc::from_raw(context));
                }
                return Err(t!("printers.discovery.errors.dnssd_setup").into_owned());
            };
            let name = storage.name.as_ptr();
            storage.request = Some(if browse {
                Request::Browse(DNS_SERVICE_BROWSE_REQUEST {
                    Version: 1,
                    InterfaceIndex: index,
                    QueryName: name,
                    Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 {
                        pBrowseCallback: Some(browse_callback),
                    },
                    pQueryContext: context.cast_mut().cast(),
                })
            } else {
                Request::Resolve(DNS_SERVICE_RESOLVE_REQUEST {
                    Version: 1,
                    InterfaceIndex: index,
                    QueryName: name.cast_mut(),
                    pResolveCompletionCallback: Some(resolve_callback),
                    pQueryContext: context.cast_mut().cast(),
                })
            });
            let Storage {
                request, cancel, ..
            } = &mut *storage;
            // SAFETY: Arc retains request, query name, cancel handle and context through the final callback.
            unsafe {
                match request {
                    Some(Request::Browse(request)) => DnsServiceBrowse(request, cancel),
                    Some(Request::Resolve(request)) => DnsServiceResolve(request, cancel),
                    None => windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32,
                }
            }
        };
        if result != DNS_REQUEST_PENDING {
            // Windows promises no callbacks when registration fails synchronously.
            state.complete.store(true, Ordering::Release);
            unsafe {
                drop(Arc::from_raw(context));
            }
            return Err(t!("printers.discovery.errors.dnssd_unavailable").into_owned());
        }
        Ok(Self { state, browse })
    }
    pub(super) fn complete(&self) -> bool {
        self.state.complete.load(Ordering::Acquire)
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        if self.complete() {
            return;
        }
        if let Ok(storage) = self.state.storage.lock() {
            // SAFETY: the callback-owned Arc keeps this handle alive even AFTER this guard is dropped.
            // Never replace that ownership with a timer or freeing the context after Cancel returns.
            unsafe {
                if self.browse {
                    DnsServiceBrowseCancel(&storage.cancel);
                } else {
                    DnsServiceResolveCancel(&storage.cancel);
                }
            }
        }
    }
}

impl State {
    fn emit(&self, event: NativeEvent) {
        if self.events.try_send(event).is_err() {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
}

struct Records(*const DNS_RECORDW);
impl Drop for Records {
    fn drop(&mut self) {
        // SAFETY: callback transfers ownership; DnsRecordListFree is the SDK macro for this call.
        if !self.0.is_null() {
            unsafe {
                DnsFree(self.0.cast(), DnsFreeRecordList);
            }
        }
    }
}
struct Instance(*const DNS_SERVICE_INSTANCE);
impl Drop for Instance {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DnsServiceFreeInstance(self.0);
            }
        }
    }
}

unsafe extern "system" fn browse_callback(
    status: u32,
    context: *const c_void,
    records: *const DNS_RECORDW,
) {
    let _records = Records(records);
    if context.is_null() {
        return;
    }
    // SAFETY: Windows owns a raw Arc until ERROR_CANCELLED. Each callback also holds a temporary
    // Arc so a concurrent final callback cannot free a context already being processed.
    let state = unsafe {
        Arc::increment_strong_count(context.cast::<State>());
        Arc::from_raw(context.cast::<State>())
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if status == 0 {
            let mut next = records;
            for _ in 0..256 {
                if next.is_null() {
                    break;
                }
                let record = unsafe { &*next };
                let name = unsafe {
                    match record.wType {
                        DNS_TYPE_PTR => wide(record.Data.PTR.pNameHost),
                        DNS_TYPE_SRV => wide(record.pName),
                        _ => None,
                    }
                };
                if let Some(name) = name
                    && name
                        .to_ascii_lowercase()
                        .trim_end_matches('.')
                        .ends_with("._moonraker._tcp.local")
                {
                    state.emit(NativeEvent::Service(name));
                }
                next = record.pNext;
            }
        } else if status != ERROR_CANCELLED {
            state.emit(NativeEvent::Error);
        }
    }));
    if status == ERROR_CANCELLED {
        state.complete.store(true, Ordering::Release);
        unsafe {
            drop(Arc::from_raw(context.cast::<State>()));
        }
    }
}

unsafe extern "system" fn resolve_callback(
    status: u32,
    context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    let _instance = Instance(instance);
    if context.is_null() {
        return;
    }
    // Resolve has exactly one completion callback, including cancellation. Its Arc owns all pointers.
    let state = unsafe { Arc::from_raw(context.cast::<State>()) };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if status == 0 && !instance.is_null() {
            unsafe {
                resolved(&state, &*instance);
            }
        } else if status != ERROR_CANCELLED {
            state.emit(NativeEvent::Error);
        }
    }));
    state.complete.store(true, Ordering::Release);
}

unsafe fn resolved(state: &State, instance: &DNS_SERVICE_INSTANCE) {
    let Some(host) = (unsafe { wide(instance.pszHostName) }) else {
        return;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        || instance.wPort == 0
    {
        return;
    }
    let name = unsafe { wide(instance.pszInstanceName) }.unwrap_or_else(|| "Moonraker".into());
    let mut uuid = None;
    let mut https = None;
    let mut prefix = false;
    if instance.dwPropertyCount > 64 {
        state.emit(NativeEvent::Error);
        return;
    }
    if !instance.keys.is_null() && !instance.values.is_null() {
        for offset in 0..instance.dwPropertyCount as usize {
            let key = unsafe { wide(*instance.keys.add(offset)) };
            let value = unsafe { wide(*instance.values.add(offset)) };
            if let (Some(key), Some(value)) = (key, value) {
                match key.as_str() {
                    "uuid" if value.len() <= 128 => uuid = Some(value),
                    "https_port" => https = value.parse::<u16>().ok().filter(|p| *p > 0),
                    "route_prefix" => prefix = !value.is_empty(),
                    _ => {}
                }
            }
        }
    }
    let mut addresses = Vec::with_capacity(2);
    if !instance.ip4Address.is_null() {
        addresses.push(SocketAddr::new(
            Ipv4Addr::from(unsafe { *instance.ip4Address }.to_ne_bytes()).into(),
            instance.wPort,
        ));
    }
    if !instance.ip6Address.is_null() {
        let ip = Ipv6Addr::from(unsafe { (*instance.ip6Address).IP6Byte });
        addresses.push(SocketAddr::V6(std::net::SocketAddrV6::new(
            ip,
            instance.wPort,
            0,
            if ip.is_unicast_link_local() {
                if instance.dwInterfaceIndex == 0 {
                    state.index
                } else {
                    instance.dwInterfaceIndex
                }
            } else {
                0
            },
        )));
    }
    for address in addresses {
        for port in [Some((instance.wPort, false)), https.map(|p| (p, true))]
            .into_iter()
            .flatten()
        {
            let mut address = address;
            address.set_port(port.0);
            let unsupported = prefix || matches!(address, SocketAddr::V6(ip) if ip.scope_id() != 0);
            state.emit(NativeEvent::Candidate(Candidate {
                address,
                host: host.clone(),
                protocol: Protocol::Moonraker,
                tls: port.1,
                name: name[..name.floor_char_boundary(96)].to_owned(),
                serial: None,
                uuid: uuid.clone(),
                announced: true,
                verification: if unsupported {
                    Verification::Unsupported
                } else {
                    Verification::RequiresCredential
                },
                local_address: None,
                issue: None,
            }));
        }
    }
}
