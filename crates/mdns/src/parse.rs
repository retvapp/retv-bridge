//! Parsing of mDNS answers into ADB services. Pure: no sockets, no clock.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use retv_bridge_core::api::DiscoveredTv;
use simple_dns::rdata::RData;
use simple_dns::Packet;

/// The three service types adb advertises.
pub const SERVICE_CONNECT: &str = "_adb-tls-connect._tcp.local";
pub const SERVICE_PAIRING: &str = "_adb-tls-pairing._tcp.local";
pub const SERVICE_ADB: &str = "_adb._tcp.local";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceKind {
    /// `_adb-tls-connect._tcp`: Wireless debugging, connect port.
    Connect,
    /// `_adb-tls-pairing._tcp`: Wireless debugging, pairing port.
    Pairing,
    /// `_adb._tcp`: plain adb over TCP.
    Adb,
}

impl ServiceKind {
    pub fn service_name(self) -> &'static str {
        match self {
            ServiceKind::Connect => SERVICE_CONNECT,
            ServiceKind::Pairing => SERVICE_PAIRING,
            ServiceKind::Adb => SERVICE_ADB,
        }
    }
}

/// One announced service instance, resolved to an address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MdnsService {
    pub instance: String,
    pub kind: ServiceKind,
    pub ip: Ipv4Addr,
    pub port: u16,
}

fn split_instance(fqdn: &str) -> Option<(String, ServiceKind)> {
    let name = fqdn.trim_end_matches('.');
    let lower = name.to_ascii_lowercase();
    for kind in [ServiceKind::Connect, ServiceKind::Pairing, ServiceKind::Adb] {
        let suffix = format!(".{}", kind.service_name());
        if lower.ends_with(&suffix) && name.len() > suffix.len() {
            return Some((name[..name.len() - suffix.len()].to_string(), kind));
        }
    }
    None
}

fn host_key(name: &impl ToString) -> String {
    name.to_string().trim_end_matches('.').to_ascii_lowercase()
}

/// Resolve every adb service in one mDNS packet. The address comes from the
/// A record of the SRV target, else from `from` (the packet's source).
/// Malformed packets give an empty list. No LAN filtering happens here; see
/// [`group_services`].
pub fn parse_response(packet: &[u8], from: Ipv4Addr) -> Vec<MdnsService> {
    if !validate_packet(packet) {
        return Vec::new();
    }
    let Ok(pkt) = Packet::parse(packet) else {
        return Vec::new();
    };
    let mut srv: Vec<(String, u16, String)> = Vec::new();
    let mut addrs: BTreeMap<String, Ipv4Addr> = BTreeMap::new();
    for rr in pkt
        .answers
        .iter()
        .chain(&pkt.additional_records)
        .chain(&pkt.name_servers)
    {
        if rr.ttl == 0 {
            continue; // goodbye
        }
        match &rr.rdata {
            RData::SRV(s) => srv.push((rr.name.to_string(), s.port, host_key(&s.target))),
            RData::A(a) => {
                addrs
                    .entry(host_key(&rr.name))
                    .or_insert(Ipv4Addr::from(a.address));
            }
            _ => {}
        }
    }
    // The SRV record carries the port, so it drives the result; the PTR that
    // points at it adds nothing we need.
    let mut out = Vec::new();
    for (name, port, target) in srv {
        let Some((instance, kind)) = split_instance(&name) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let ip = addrs.get(&target).copied().unwrap_or(from);
        let svc = MdnsService {
            instance,
            kind,
            ip,
            port,
        };
        if !out.contains(&svc) {
            out.push(svc);
        }
    }
    out
}

impl From<&MdnsService> for retv_bridge_core::validate::MdnsService {
    fn from(s: &MdnsService) -> Self {
        Self {
            instance: s.instance.clone(),
            service: s.kind.service_name().to_string(),
            ip: s.ip.to_string(),
            port: s.port,
        }
    }
}

/// One entry per LAN address, by the single rule set in
/// `retv_bridge_core::validate::group_services` (a port of the TS bridge's
/// `groupServices`): non-LAN addresses and port 0 are dropped, the first
/// connect/`_adb` port wins, the last pairing port wins.
pub fn group_services(services: &[MdnsService]) -> Vec<DiscoveredTv> {
    let core: Vec<_> = services.iter().map(Into::into).collect();
    retv_bridge_core::validate::group_services(&core)
}

/// Cheap structural check run before `simple-dns` sees the bytes: counts and
/// lengths must fit the packet, and every compression pointer must point
/// strictly backwards, each one before the previous one's target, so no name
/// can loop. Rejects what it cannot prove finite.
fn validate_packet(p: &[u8]) -> bool {
    fn name(p: &[u8], mut pos: usize) -> Option<usize> {
        // Returns the offset just after the name as written at `pos`.
        let mut end: Option<usize> = None;
        let mut limit = usize::MAX; // next pointer must target below this
        for _ in 0..512 {
            let len = *p.get(pos)? as usize;
            match len & 0xC0 {
                0x00 => {
                    if len == 0 {
                        return Some(end.unwrap_or(pos + 1));
                    }
                    pos = pos.checked_add(1 + len)?;
                    if pos > p.len() {
                        return None;
                    }
                }
                0xC0 => {
                    let target = ((len & 0x3F) << 8) | *p.get(pos + 1)? as usize;
                    if target >= pos || target >= limit {
                        return None;
                    }
                    end.get_or_insert(pos + 2);
                    limit = target;
                    pos = target;
                }
                _ => return None,
            }
        }
        None
    }

    if p.len() < 12 {
        return false;
    }
    let count = |i: usize| u16::from_be_bytes([p[i], p[i + 1]]) as usize;
    let (qd, an, ns, ar) = (count(4), count(6), count(8), count(10));
    // Each entry needs at least 5 bytes (root name + type + class).
    if (qd + an + ns + ar).saturating_mul(5) > p.len() {
        return false;
    }
    let mut pos = 12;
    for _ in 0..qd {
        let Some(e) = name(p, pos) else { return false };
        pos = e + 4;
        if pos > p.len() {
            return false;
        }
    }
    for _ in 0..an + ns + ar {
        let Some(e) = name(p, pos) else { return false };
        if e + 10 > p.len() {
            return false;
        }
        let rtype = u16::from_be_bytes([p[e], p[e + 1]]);
        let rdlen = u16::from_be_bytes([p[e + 8], p[e + 9]]) as usize;
        let rd = e + 10;
        let Some(next) = rd.checked_add(rdlen) else {
            return false;
        };
        if next > p.len() {
            return false;
        }
        let ok = match rtype {
            12 => name(&p[..next], rd).is_some_and(|x| x == next),
            33 => rdlen >= 7 && name(&p[..next], rd + 6).is_some_and(|x| x == next),
            1 => rdlen == 4,
            _ => true,
        };
        if !ok {
            return false;
        }
        pos = next;
    }
    true
}
