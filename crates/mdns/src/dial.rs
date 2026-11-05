//! Fire TV by DIAL, on every OS. A Fire TV answers `_amzn-wplay._tcp` only to
//! a multicast question with a multicast answer, which only the OS resolver
//! sees (macOS). It also answers an SSDP M-SEARCH for DIAL unicast to the
//! asking port, so this works from an ephemeral port without admin rights or
//! binding 1900/5353:
//!
//! 1. M-SEARCH `urn:dial-multiscreen-org:service:dial:1` to 239.255.255.250:1900;
//! 2. GET each answer's LOCATION (the UPnP device description) on the
//!    answering address only: `<manufacturer>Amazon</manufacturer>` is a Fire TV;
//! 3. else GET `<Application-URL>FireTVRemote` (Fire TV Edition TVs carry the
//!    TV maker's name): a DIAL `<name>FireTVRemote</name>` is a Fire TV.
//!
//! Read-only: two HTTP GETs per DIAL device at most, nothing launched.

use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use async_trait::async_trait;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::parse::{MdnsService, ServiceKind};

pub const DIAL_TARGET: &str = "urn:dial-multiscreen-org:service:dial:1";
pub const SSDP_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
/// Fire TV's DIAL app for Amazon's phone remote.
pub const FIRE_TV_DIAL_APP: &str = "FireTVRemote";
/// Most DIAL devices asked over HTTP per discovery.
const MAX_DEVICES: usize = 32;
/// Most bytes read from one HTTP answer.
const MAX_HTTP: usize = 32 * 1024;
/// Time kept at the end of the window for the HTTP checks.
const FETCH_GRACE: Duration = Duration::from_millis(500);

/// Finds Fire TVs by DIAL.
#[async_trait]
pub trait DialFinder: Send + Sync {
    /// Fire TVs found within `timeout`, as `_amzn-wplay._tcp` services.
    async fn find(&self, ifaces: &[Ipv4Addr], timeout: Duration) -> Vec<MdnsService>;
}

/// No DIAL search: finds nothing.
pub struct NoDial;

#[async_trait]
impl DialFinder for NoDial {
    async fn find(&self, _ifaces: &[Ipv4Addr], _timeout: Duration) -> Vec<MdnsService> {
        Vec::new()
    }
}

/// The real search, to `target` (the SSDP group; a local address in tests).
pub struct SystemDial {
    pub target: SocketAddrV4,
}

impl Default for SystemDial {
    fn default() -> Self {
        Self { target: SSDP_GROUP }
    }
}

pub fn m_search() -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {DIAL_TARGET}\r\n\r\n"
    )
}

fn header<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.split("\r\n").skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// An `http://<ipv4>[:port]/path` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpUrl {
    pub ip: Ipv4Addr,
    pub port: u16,
    pub path: String,
}

pub fn parse_http_url(url: &str) -> Option<HttpUrl> {
    let rest = url.trim().strip_prefix("http://")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, 80),
    };
    if port == 0 || path.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    Some(HttpUrl {
        ip: host.parse().ok()?,
        port,
        path: path.to_string(),
    })
}

/// The device description URL in an M-SEARCH answer for DIAL, if it is on
/// the address that answered (never a third host).
pub fn parse_ssdp_answer(packet: &[u8], from: Ipv4Addr) -> Option<HttpUrl> {
    let text = std::str::from_utf8(packet).ok()?;
    let status = text.split("\r\n").next()?;
    if !status.starts_with("HTTP/1.") || !status.contains(" 200") {
        return None;
    }
    if !header(text, "st")?.eq_ignore_ascii_case(DIAL_TARGET) {
        return None;
    }
    let url = parse_http_url(header(text, "location")?)?;
    (url.ip == from).then_some(url)
}

/// What a UPnP device description says about the device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub friendly_name: String,
    pub manufacturer: String,
    pub model_name: String,
}

fn xml_text(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.find(&open)
        .and_then(|i| {
            let start = i + open.len();
            let end = xml[start..].find(&close)? + start;
            Some(unescape(xml[start..end].trim()))
        })
        .unwrap_or_default()
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn parse_device_description(xml: &str) -> DeviceInfo {
    DeviceInfo {
        friendly_name: xml_text(xml, "friendlyName"),
        manufacturer: xml_text(xml, "manufacturer"),
        model_name: xml_text(xml, "modelName"),
    }
}

/// Amazon's own devices that answer DIAL as TVs (Fire TV sticks, Cubes, Fire TV
/// Edition TVs built by Amazon).
pub fn is_amazon(info: &DeviceInfo) -> bool {
    info.manufacturer.trim().eq_ignore_ascii_case("amazon") || info.model_name.starts_with("AFT")
}

/// A DIAL app description for Fire TV's remote app.
pub fn is_fire_tv_app(xml: &str) -> bool {
    xml.contains("urn:dial-multiscreen-org:schemas:dial")
        && xml_text(xml, "name") == FIRE_TV_DIAL_APP
}

/// The name the Bridge shows: «<friendly name> (Fire TV)», else the model.
pub fn fire_tv_name(info: &DeviceInfo, ip: Ipv4Addr) -> String {
    let base = [&info.friendly_name, &info.model_name]
        .into_iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| ip.to_string());
    let base: String = base.chars().filter(|c| !c.is_control()).take(80).collect();
    if base.to_ascii_lowercase().contains("fire tv") {
        base
    } else {
        format!("{base} (Fire TV)")
    }
}

/// One HTTP answer: status, headers (raw), body.
pub struct HttpAnswer {
    pub status: u16,
    pub head: String,
    pub body: String,
}

pub fn parse_http_answer(raw: &[u8]) -> Option<HttpAnswer> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    Some(HttpAnswer {
        status,
        head: head.to_string(),
        body: body.to_string(),
    })
}

async fn http_get(url: &HttpUrl) -> io::Result<HttpAnswer> {
    let mut s = TcpStream::connect(SocketAddrV4::new(url.ip, url.port)).await?;
    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        url.path, url.ip, url.port
    );
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < MAX_HTTP {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    parse_http_answer(&buf).ok_or_else(|| io::Error::other("bad HTTP answer"))
}

/// Steps 2 and 3 for one DIAL device.
async fn check_device(location: HttpUrl) -> Option<MdnsService> {
    let dd = http_get(&location).await.ok()?;
    if dd.status != 200 {
        return None;
    }
    let info = parse_device_description(&dd.body);
    let app_url = header(&dd.head, "application-url")
        .and_then(parse_http_url)
        .filter(|u| u.ip == location.ip);
    let fire = if is_amazon(&info) {
        true
    } else if let Some(app) = &app_url {
        let mut url = app.clone();
        if !url.path.ends_with('/') {
            url.path.push('/');
        }
        url.path.push_str(FIRE_TV_DIAL_APP);
        matches!(http_get(&url).await, Ok(a) if a.status == 200 && is_fire_tv_app(&a.body))
    } else {
        false
    };
    fire.then(|| MdnsService {
        instance: fire_tv_name(&info, location.ip),
        kind: ServiceKind::FireTv,
        ip: location.ip,
        port: app_url.map_or(location.port, |u| u.port),
    })
}

fn search_socket(iface: Option<Ipv4Addr>) -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_nonblocking(true)?;
    s.set_multicast_ttl_v4(2)?;
    if let Some(ip) = iface {
        s.set_multicast_if_v4(&ip)?;
    }
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
    UdpSocket::from_std(s.into())
}

#[async_trait]
impl DialFinder for SystemDial {
    async fn find(&self, ifaces: &[Ipv4Addr], timeout: Duration) -> Vec<MdnsService> {
        let start = Instant::now();
        let deadline = start + timeout;
        let listen_until = start + timeout.saturating_sub(FETCH_GRACE).max(timeout / 2);
        let mut sockets = Vec::new();
        for iface in std::iter::once(None).chain(ifaces.iter().copied().map(Some)) {
            if let Ok(s) = search_socket(iface) {
                sockets.push(s);
            }
        }
        let probe = m_search();
        for s in &sockets {
            for _ in 0..2 {
                let _ = s.send_to(probe.as_bytes(), self.target).await;
            }
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel::<HttpUrl>(MAX_DEVICES);
        let mut readers = JoinSet::new();
        for s in sockets {
            let tx = tx.clone();
            readers.spawn(async move {
                let mut buf = [0u8; 2048];
                loop {
                    // An error is about one datagram, so the search goes on: Windows fails the read of an
                    // answer longer than the buffer (WSAEMSGSIZE) where Unix cuts it.
                    let Ok((n, from)) = s.recv_from(&mut buf).await else {
                        continue;
                    };
                    let SocketAddr::V4(from) = from else { continue };
                    if let Some(url) = parse_ssdp_answer(&buf[..n], *from.ip()) {
                        let _ = tx.try_send(url);
                    }
                }
            });
        }
        drop(tx);

        let mut asked: BTreeSet<Ipv4Addr> = BTreeSet::new();
        let mut checks = JoinSet::new();
        loop {
            tokio::select! {
                got = rx.recv() => match got {
                    Some(url) => {
                        if asked.len() < MAX_DEVICES && asked.insert(url.ip) {
                            checks.spawn(tokio::time::timeout_at(deadline, check_device(url)));
                        }
                    }
                    None => break,
                },
                _ = tokio::time::sleep_until(listen_until) => break,
            }
        }
        readers.abort_all();
        let mut found = Vec::new();
        while let Some(done) = checks.join_next().await {
            if let Ok(Ok(Some(svc))) = done {
                if !found.contains(&svc) {
                    found.push(svc);
                }
            }
        }
        found.sort();
        found
    }
}
