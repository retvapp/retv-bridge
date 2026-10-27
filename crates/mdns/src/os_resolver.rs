//! The operating system's own mDNS resolver, for the pairing service.
//!
//! An Android 14 TV's `_adb-tls-pairing._tcp` belongs to system_server's
//! MdnsAdvertiser, which answers only by multicast to 224.0.0.251:5353. The
//! querier's ephemeral sockets never see that, and the bridge never binds
//! 5353 itself. The OS responder (mDNSResponder on macOS) does listen there,
//! so the bridge asks it instead.
//!
//! - macOS: dns_sd in libSystem (`DNSServiceBrowse` → `DNSServiceResolve` →
//!   `DNSServiceGetAddrInfo`), see `dnssd`.
//! - Windows and Linux: none yet; the site asks for the port the TV shows.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;

use crate::parse::{MdnsService, ServiceKind};

/// One service instance the OS resolver found and resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsAnswer {
    /// The instance name, unescaped (`adb-<serial>-<suffix>`).
    pub instance: String,
    /// As the OS reports it, e.g. `_adb-tls-pairing._tcp.`.
    pub service_type: String,
    /// Host byte order.
    pub port: u16,
    pub addrs: Vec<IpAddr>,
}

#[async_trait]
pub trait OsResolver: Send + Sync {
    /// Browses `kind` and resolves each instance found within `timeout`.
    async fn browse(&self, kind: ServiceKind, timeout: Duration) -> io::Result<Vec<OsAnswer>>;
}

/// No OS resolver: finds nothing.
pub struct NoResolver;

#[async_trait]
impl OsResolver for NoResolver {
    async fn browse(&self, _kind: ServiceKind, _timeout: Duration) -> io::Result<Vec<OsAnswer>> {
        Ok(Vec::new())
    }
}

/// This platform's resolver: dns_sd on macOS, none elsewhere.
pub struct SystemResolver;

#[async_trait]
impl OsResolver for SystemResolver {
    #[cfg(target_os = "macos")]
    async fn browse(&self, kind: ServiceKind, timeout: Duration) -> io::Result<Vec<OsAnswer>> {
        let regtype = service_type(kind);
        tokio::task::spawn_blocking(move || crate::dnssd::browse(regtype, timeout))
            .await
            .map_err(io::Error::other)?
    }

    #[cfg(not(target_os = "macos"))]
    async fn browse(&self, kind: ServiceKind, timeout: Duration) -> io::Result<Vec<OsAnswer>> {
        NoResolver.browse(kind, timeout).await
    }
}

/// `_adb-tls-pairing._tcp`: the service name without the domain, as the OS
/// resolvers take it.
pub fn service_type(kind: ServiceKind) -> &'static str {
    kind.service_name()
        .strip_suffix(".local")
        .unwrap_or(kind.service_name())
}

fn kind_of(reported: &str) -> Option<ServiceKind> {
    let t = reported.trim_end_matches('.').to_ascii_lowercase();
    let t = t.strip_suffix(".local").unwrap_or(&t);
    [ServiceKind::Connect, ServiceKind::Pairing, ServiceKind::Adb]
        .into_iter()
        .find(|k| service_type(*k) == t)
}

/// The OS answers as services: one per IPv4 address, in order, without
/// duplicates. Port 0, IPv6-only instances and other service types are
/// dropped. LAN filtering is left to `group_services`, as for the querier.
pub fn os_services(answers: &[OsAnswer]) -> Vec<MdnsService> {
    let mut out: Vec<MdnsService> = Vec::new();
    for a in answers {
        let Some(kind) = kind_of(&a.service_type) else {
            continue;
        };
        if a.port == 0 || a.instance.is_empty() {
            continue;
        }
        for ip in &a.addrs {
            let IpAddr::V4(ip) = ip else { continue };
            let svc = MdnsService {
                instance: a.instance.clone(),
                kind,
                ip: *ip,
                port: a.port,
            };
            if !out.contains(&svc) {
                out.push(svc);
            }
        }
    }
    out
}

/// The IPv4 address in a `struct sockaddr` as BSD lays it out
/// (`sin_len`, `sin_family`, port, address), or `None` for another family.
pub fn ipv4_from_sockaddr_bytes(sa: &[u8]) -> Option<Ipv4Addr> {
    const AF_INET: u8 = 2;
    if sa.len() < 8 || sa[1] != AF_INET {
        return None;
    }
    Some(Ipv4Addr::new(sa[4], sa[5], sa[6], sa[7]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_types_without_domain() {
        assert_eq!(service_type(ServiceKind::Pairing), "_adb-tls-pairing._tcp");
        assert_eq!(
            kind_of("_adb-tls-pairing._tcp."),
            Some(ServiceKind::Pairing)
        );
        assert_eq!(kind_of("_adb._tcp.local."), Some(ServiceKind::Adb));
        assert_eq!(kind_of("_adb-tls-connect._tcp"), Some(ServiceKind::Connect));
        assert_eq!(kind_of("_ipp._tcp."), None);
    }

    #[test]
    fn sockaddr_in_and_others() {
        // sin_len 16, AF_INET, port 36183 (network order), 192.0.2.232.
        let sa = [16, 2, 0x8d, 0x57, 192, 0, 2, 232, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            ipv4_from_sockaddr_bytes(&sa),
            Some(Ipv4Addr::new(192, 0, 2, 232))
        );
        let mut v6 = [0u8; 28];
        v6[0] = 28;
        v6[1] = 30; // AF_INET6 on macOS
        assert_eq!(ipv4_from_sockaddr_bytes(&v6), None);
        assert_eq!(ipv4_from_sockaddr_bytes(&[16, 2, 0]), None);
    }
}
