//! Finding Samsung TVs: an SSDP M-SEARCH for Samsung's remote-control
//! receiver (and DIAL, which Samsung TVs answer too) from every interface of
//! this computer, then each answering address is asked on its REST API.
//! When multicast does not get through (another interface holds the default
//! route, a VPN, a router that drops it), [`probe_hosts`] over
//! [`subnet_hosts`] asks every address of this computer's /24s directly, as
//! Samsung's phone app does.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::StreamExt;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::tv::{fetch_info, fetch_info_with, local_ipv4s, rest_client, TvInfo};

/// What Samsung TVs (2016 and later) answer.
pub const SEARCH_TARGET: &str = "urn:samsung.com:device:RemoteControlReceiver:1";
/// DIAL: Samsung TVs answer it as well; other DIAL devices fail the REST check.
pub const DIAL_TARGET: &str = "urn:dial-multiscreen-org:service:dial:1";
/// The SSDP multicast group.
pub const SSDP_GROUP: SocketAddr = SocketAddr::V4(std::net::SocketAddrV4::new(
    Ipv4Addr::new(239, 255, 255, 250),
    1900,
));
/// Most /24s [`subnet_hosts`] gives (Wi-Fi, Ethernet, a VPN or two).
const MAX_SUBNETS: usize = 4;

pub fn m_search() -> String {
    m_search_for(SEARCH_TARGET)
}

/// An M-SEARCH for `st`.
pub fn m_search_for(st: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
    )
}

/// The IPv4 in an answer's LOCATION header.
pub fn location_ip(response: &str) -> Option<Ipv4Addr> {
    let value = response.split("\r\n").find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case("location").then(|| v.trim())
    })?;
    let rest = value.split_once("://").map_or(value, |(_, r)| r);
    let host = rest.split(['/', ':']).next()?;
    host.parse().ok()
}

/// A UDP socket on 0.0.0.0 whose multicast leaves on `iface` (the system's
/// choice when `None`), TTL 2, loopback on.
fn search_socket(iface: Option<Ipv4Addr>) -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_nonblocking(true)?;
    s.set_multicast_ttl_v4(2)?;
    s.set_multicast_loop_v4(true)?;
    if let Some(ip) = iface {
        s.set_multicast_if_v4(&ip)?;
    }
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
    UdpSocket::from_std(s.into())
}

/// The addresses that answer on `sock` until `deadline`.
async fn answers(sock: UdpSocket, deadline: tokio::time::Instant) -> Vec<Ipv4Addr> {
    let mut ips = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                let ip = location_ip(&text).or(match from {
                    SocketAddr::V4(v4) => Some(*v4.ip()),
                    SocketAddr::V6(_) => None,
                });
                ips.extend(ip);
            }
            Ok(Err(_)) => continue,
            Err(_) => break,
        }
    }
    ips
}

/// [`discover_via`] from every non-loopback IPv4 interface of this computer.
pub async fn discover(
    target: SocketAddr,
    wait: Duration,
    rest_port: u16,
    allow: impl Fn(&Ipv4Addr) -> bool,
) -> Vec<TvInfo> {
    discover_via(target, wait, rest_port, allow, &local_ipv4s()).await
}

/// Sends the M-SEARCHes to `target` (twice each) from a socket per
/// interface in `ifaces` plus one on the system's default, listens for
/// `wait`, then asks each address that `allow` accepts on `rest_port`. Only
/// TVs that answer as Samsung come back, sorted by address.
pub async fn discover_via(
    target: SocketAddr,
    wait: Duration,
    rest_port: u16,
    allow: impl Fn(&Ipv4Addr) -> bool,
    ifaces: &[Ipv4Addr],
) -> Vec<TvInfo> {
    let mut sockets = Vec::new();
    for iface in std::iter::once(None).chain(ifaces.iter().copied().map(Some)) {
        if let Ok(sock) = search_socket(iface) {
            sockets.push(sock);
        }
    }
    let probes = [m_search_for(SEARCH_TARGET), m_search_for(DIAL_TARGET)];
    for sock in &sockets {
        for _ in 0..2 {
            for probe in &probes {
                let _ = sock.send_to(probe.as_bytes(), target).await;
            }
        }
    }
    let deadline = tokio::time::Instant::now() + wait;
    let heard =
        futures_util::future::join_all(sockets.into_iter().map(|s| answers(s, deadline))).await;
    let ips: BTreeSet<Ipv4Addr> = heard.into_iter().flatten().filter(|ip| allow(ip)).collect();
    let probes = ips
        .into_iter()
        .map(|ip| async move { fetch_info(&ip.to_string(), rest_port).await.ok() });
    futures_util::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Every other address of the home-network /24s `ours` are on (at most
/// four of them; link-local left out).
pub fn subnet_hosts(ours: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let home = |ip: &Ipv4Addr| {
        !ip.is_link_local()
            && retv_bridge_core::validate::is_lan_ipv4(&serde_json::Value::String(ip.to_string()))
    };
    let nets: BTreeSet<[u8; 3]> = ours
        .iter()
        .filter(|ip| home(ip))
        .map(|ip| {
            let o = ip.octets();
            [o[0], o[1], o[2]]
        })
        .collect();
    nets.into_iter()
        .take(MAX_SUBNETS)
        .flat_map(|[a, b, c]| (1..=254).map(move |d| Ipv4Addr::new(a, b, c, d)))
        .filter(|ip| !ours.contains(ip))
        .collect()
}

/// Asks `hosts` on `rest_port`, at most `concurrency` at once, each within
/// `timeout`. Only TVs that answer as Samsung come back, sorted by address.
pub async fn probe_hosts(
    hosts: Vec<Ipv4Addr>,
    rest_port: u16,
    timeout: Duration,
    concurrency: usize,
) -> Vec<TvInfo> {
    let Ok(client) = rest_client(timeout) else {
        return Vec::new();
    };
    let hosts: BTreeSet<Ipv4Addr> = hosts.into_iter().collect();
    let client = &client;
    let mut found: Vec<TvInfo> = futures_util::stream::iter(hosts)
        .map(|ip| async move {
            let ip = ip.to_string();
            let ask = fetch_info_with(client, &ip, rest_port);
            tokio::time::timeout(timeout, ask).await.ok()?.ok()
        })
        .buffer_unordered(concurrency.max(1))
        .filter_map(|tv| async move { tv })
        .collect()
        .await;
    found.sort_by_key(|tv| tv.ip.parse::<Ipv4Addr>().ok());
    found
}
