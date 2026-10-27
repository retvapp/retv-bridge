//! mDNS discovery of adb TVs. Queries from ephemeral ports (never 5353) with
//! the unicast-response bit set, one socket per IPv4 interface. The pairing
//! service, which Android 14 answers only by multicast, comes from the OS's
//! own mDNS resolver where there is one (`os_resolver`).

#[cfg(target_os = "macos")]
mod dnssd;
mod os_resolver;
mod parse;
mod query;

use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::Instant;

pub use os_resolver::{
    ipv4_from_sockaddr_bytes, os_services, service_type, NoResolver, OsAnswer, OsResolver,
    SystemResolver,
};
pub use parse::{
    group_services, parse_response, MdnsService, ServiceKind, SERVICE_ADB, SERVICE_CONNECT,
    SERVICE_PAIRING,
};
pub use query::{build_query, QuerySocket, SocketFactory, SystemSockets, UdpQuerySocket};

/// Default collection window.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Parsed packets waiting for the collector.
const CHANNEL_CAPACITY: usize = 64;
/// Upper bound on distinct services kept per discovery.
pub const MAX_SERVICES: usize = 256;
/// The query goes out twice, this far apart.
pub const RESEND_AFTER: Duration = Duration::from_millis(250);

pub trait Interfaces: Send + Sync {
    fn ipv4(&self) -> Vec<Ipv4Addr>;
}

/// Up, non-loopback IPv4 interface addresses.
pub struct SystemInterfaces;

impl Interfaces for SystemInterfaces {
    fn ipv4(&self) -> Vec<Ipv4Addr> {
        let mut out: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| !i.is_loopback() && i.is_oper_up())
            .filter_map(|i| match i.ip() {
                std::net::IpAddr::V4(v4) => Some(v4),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoverError {
    /// Every send failed with "host/network unreachable": macOS Local Network
    /// permission is denied.
    LocalNetworkDenied,
    /// Every interface failed for another reason.
    Io(String),
}

impl std::fmt::Display for DiscoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoverError::LocalNetworkDenied => write!(f, "local network access denied"),
            DiscoverError::Io(e) => write!(f, "mDNS I/O error: {e}"),
        }
    }
}

impl std::error::Error for DiscoverError {}

fn is_unreachable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable
    )
}

/// Query every interface and collect answers for `timeout`.
///
/// Returns the raw services: nothing is LAN-filtered or merged, so pass them
/// through [`group_services`] to get `DiscoveredTv`s.
///
/// `LocalNetworkDenied` means every send failed with host/network
/// unreachable. That is what macOS gives when Local Network access is denied,
/// but a machine whose only up interface is a VPN or other unroutable link
/// fails the same way, so the UI help (Task 14) should word it as "check
/// Local Network permission and that you are on your home network".
pub async fn discover(
    timeout: Duration,
    ifaces: &dyn Interfaces,
) -> Result<Vec<MdnsService>, DiscoverError> {
    discover_merged(timeout, ifaces, &SystemSockets, &SystemResolver).await
}

/// The querier (every adb service) and the OS resolver (the pairing
/// service) at once, for `timeout` each. The querier's services come first,
/// then the resolver's new ones, so [`group_services`] keeps its rules (a
/// TV is named by its first service). A resolver failure only loses its
/// part; a querier failure stands only when the resolver found nothing.
pub async fn discover_merged(
    timeout: Duration,
    ifaces: &dyn Interfaces,
    factory: &dyn SocketFactory,
    resolver: &dyn OsResolver,
) -> Result<Vec<MdnsService>, DiscoverError> {
    let (queried, resolved) = tokio::join!(
        discover_with(timeout, ifaces, factory),
        resolver.browse(ServiceKind::Pairing, timeout)
    );
    let mut extra = resolved.map(|a| os_services(&a)).unwrap_or_default();
    extra.sort();
    let mut found = match queried {
        Ok(found) => found,
        Err(e) if extra.is_empty() => return Err(e),
        Err(_) => Vec::new(),
    };
    for svc in extra {
        if found.len() >= MAX_SERVICES {
            break;
        }
        if !found.contains(&svc) {
            found.push(svc);
        }
    }
    Ok(found)
}

pub async fn discover_with(
    timeout: Duration,
    ifaces: &dyn Interfaces,
    factory: &dyn SocketFactory,
) -> Result<Vec<MdnsService>, DiscoverError> {
    let addrs = ifaces.ipv4();
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    let mut sockets: Vec<Arc<dyn QuerySocket>> = Vec::new();
    let mut last_err: Option<io::Error> = None;
    for a in addrs {
        match factory.open(a) {
            Ok(s) => sockets.push(Arc::from(s)),
            Err(e) => last_err = Some(e),
        }
    }
    if sockets.is_empty() {
        return Err(match last_err {
            Some(e) => DiscoverError::Io(e.to_string()),
            None => DiscoverError::Io("no socket".into()),
        });
    }

    // Readers parse their own packets (so the select loop below never does
    // CPU work) and hand results over a bounded channel; when it is full the
    // newest results are dropped.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<MdnsService>>(CHANNEL_CAPACITY);
    let mut readers = JoinSet::new();
    for s in &sockets {
        let s = Arc::clone(s);
        let tx = tx.clone();
        readers.spawn(async move {
            let mut buf = vec![0u8; 9000];
            while let Ok((n, from)) = s.recv(&mut buf).await {
                let services = parse_response(&buf[..n], from);
                if !services.is_empty() {
                    let _ = tx.try_send(services);
                }
                tokio::task::yield_now().await;
            }
        });
    }
    drop(tx);

    let query = build_query();
    let start = Instant::now();
    let deadline = start + timeout;
    let mut rounds_left = 2;
    let mut next_send = start;
    let (mut attempts, mut unreachable, mut other_fail) = (0usize, 0usize, 0usize);
    let mut found: Vec<MdnsService> = Vec::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next_send), if rounds_left > 0 && next_send < deadline => {
                rounds_left -= 1;
                next_send += RESEND_AFTER;
                for s in &sockets {
                    attempts += 1;
                    if let Err(e) = s.send_query(&query).await {
                        if is_unreachable(&e) { unreachable += 1 } else { other_fail += 1 }
                    }
                }
            }
            got = rx.recv() => match got {
                Some(services) => {
                    for svc in services {
                        if found.len() >= MAX_SERVICES { break; }
                        if !found.contains(&svc) { found.push(svc); }
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    readers.abort_all();

    if found.is_empty() && attempts > 0 {
        if unreachable == attempts {
            return Err(DiscoverError::LocalNetworkDenied);
        }
        if unreachable + other_fail == attempts {
            return Err(DiscoverError::Io("every send failed".into()));
        }
    }
    found.sort();
    Ok(found)
}
