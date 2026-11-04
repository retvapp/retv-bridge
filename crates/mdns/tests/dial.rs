//! Fire TV by SSDP/DIAL (the Windows and Linux path). Fixtures are the live
//! answers of the AFTMA08C15 stick and a Google TV (conformance/mdns/dial).

use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use retv_bridge_mdns::{
    dial_m_search, discover_all, fire_tv_name, group_services, is_amazon, is_fire_tv_app,
    parse_device_description, parse_http_answer, parse_http_url, parse_ssdp_answer, DialFinder,
    HttpUrl, Interfaces, MdnsService, NoResolver, QuerySocket, ServiceKind, SocketFactory,
    SystemDial,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixture(name: &str) -> Vec<u8> {
    let dir: PathBuf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("conformance"))
        .find(|d| d.is_dir())
        .expect("conformance/ above the crate")
        .join("mdns/dial");
    std::fs::read(dir.join(name)).expect("fixture")
}

const STICK: Ipv4Addr = Ipv4Addr::new(192, 168, 88, 218);
const GTV: Ipv4Addr = Ipv4Addr::new(192, 168, 88, 232);

#[test]
fn the_search_asks_for_dial() {
    let m = dial_m_search();
    assert!(m.starts_with("M-SEARCH * HTTP/1.1\r\n"));
    assert!(m.contains("ST: urn:dial-multiscreen-org:service:dial:1\r\n"));
    assert!(m.contains("MAN: \"ssdp:discover\"\r\n"));
    assert!(m.ends_with("\r\n\r\n"));
}

#[test]
fn ssdp_answers_give_the_description_on_the_answering_host() {
    let stick = parse_ssdp_answer(&fixture("aftma08c15-ssdp.txt"), STICK).unwrap();
    assert_eq!(
        stick,
        HttpUrl {
            ip: STICK,
            port: 60000,
            path: "/dd.xml".into()
        }
    );
    let gtv = parse_ssdp_answer(&fixture("googletv-ssdp.txt"), GTV).unwrap();
    assert_eq!(gtv.port, 8008);
    // A LOCATION on another host is never followed.
    assert_eq!(
        parse_ssdp_answer(&fixture("aftma08c15-ssdp.txt"), GTV),
        None
    );
    // Another search target, a notify, garbage.
    let other = String::from_utf8(fixture("aftma08c15-ssdp.txt"))
        .unwrap()
        .replace(
            "urn:dial-multiscreen-org:service:dial:1\r\nUSN",
            "upnp:rootdevice\r\nUSN",
        );
    assert_eq!(parse_ssdp_answer(other.as_bytes(), STICK), None);
    assert_eq!(
        parse_ssdp_answer(b"NOTIFY * HTTP/1.1\r\nST: urn:dial-multiscreen-org:service:dial:1\r\nLOCATION: http://192.168.88.218:1/\r\n\r\n", STICK),
        None
    );
    assert_eq!(parse_ssdp_answer(&[0xff, 0, 1], STICK), None);
}

#[test]
fn urls() {
    assert_eq!(
        parse_http_url("http://192.168.88.218:8009/apps/"),
        Some(HttpUrl {
            ip: STICK,
            port: 8009,
            path: "/apps/".into()
        })
    );
    assert_eq!(parse_http_url("http://192.168.88.218").unwrap().port, 80);
    assert_eq!(parse_http_url("https://192.168.88.218/"), None);
    assert_eq!(parse_http_url("http://tv.local:8009/"), None);
    assert_eq!(parse_http_url("http://192.168.88.218:0/"), None);
    assert_eq!(parse_http_url("http://192.168.88.218/a b"), None);
}

#[test]
fn the_stick_describes_itself_as_amazon() {
    let dd = parse_http_answer(&fixture("aftma08c15-dd.http")).unwrap();
    assert_eq!(dd.status, 200);
    assert!(dd
        .head
        .contains("Application-URL: http://192.168.88.218:8009/apps/"));
    let info = parse_device_description(&dd.body);
    assert_eq!(info.manufacturer, "Amazon");
    assert_eq!(info.model_name, "AFTMA08C15");
    assert_eq!(info.friendly_name, "label's Fire TV Stick");
    assert!(is_amazon(&info));
    assert_eq!(fire_tv_name(&info, STICK), "label's Fire TV Stick");

    let app = parse_http_answer(&fixture("aftma08c15-firetvremote.http")).unwrap();
    assert_eq!(app.status, 200);
    assert!(is_fire_tv_app(&app.body));
}

#[test]
fn a_google_tv_is_not_a_fire_tv() {
    let dd = parse_http_answer(&fixture("googletv-dd.http")).unwrap();
    let info = parse_device_description(&dd.body);
    assert_eq!(info.manufacturer, "Xiaomi");
    assert!(!is_amazon(&info));
    assert!(!is_fire_tv_app(&dd.body));
    assert!(!is_fire_tv_app(
        "<service xmlns=\"urn:dial-multiscreen-org:schemas:dial\"><name>YouTube</name></service>"
    ));
}

#[test]
fn names() {
    let mut info = parse_device_description(
        "<root><device><friendlyName>Living room</friendlyName><modelName>AFTKA</modelName></device></root>",
    );
    assert!(is_amazon(&info));
    assert_eq!(fire_tv_name(&info, STICK), "Living room (Fire TV)");
    info.friendly_name.clear();
    assert_eq!(fire_tv_name(&info, STICK), "AFTKA (Fire TV)");
    info.model_name.clear();
    assert_eq!(fire_tv_name(&info, STICK), "192.168.88.218 (Fire TV)");
    let escaped = parse_device_description("<friendlyName>Tom &amp; Ann</friendlyName>");
    assert_eq!(escaped.friendly_name, "Tom & Ann");
}

/// A one-shot HTTP server on 127.0.0.1: answers each path from `routes`, else 404.
async fn http_server(routes: Vec<(&'static str, Vec<u8>)>) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_else(|| b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec());
                let _ = s.write_all(&body).await;
            });
        }
    });
    port
}

/// An SSDP responder on 127.0.0.1 that answers every M-SEARCH for DIAL with `location`.
async fn ssdp_responder(location: String) -> SocketAddrV4 {
    let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = match s.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = s.recv_from(&mut buf).await {
            let q = String::from_utf8_lossy(&buf[..n]);
            if q.starts_with("M-SEARCH") && q.contains("urn:dial-multiscreen-org:service:dial:1") {
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nLOCATION: {location}\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n"
                );
                let _ = s.send_to(answer.as_bytes(), from).await;
            }
        }
    });
    addr
}

#[tokio::test]
async fn the_search_finds_the_stick_over_the_loopback() {
    // The fixture's Application-URL names 192.168.88.218, not the answering
    // host, so it is not followed and the port is the description's.
    let server = http_server(vec![("/dd.xml", fixture("aftma08c15-dd.http"))]).await;
    let target = ssdp_responder(format!("http://127.0.0.1:{server}/dd.xml")).await;
    let found = SystemDial { target }
        .find(&[], Duration::from_millis(800))
        .await;
    assert_eq!(
        found,
        vec![MdnsService {
            instance: "label's Fire TV Stick".into(),
            kind: ServiceKind::FireTv,
            ip: Ipv4Addr::LOCALHOST,
            port: server,
        }]
    );
}

#[tokio::test]
async fn a_fire_tv_edition_tv_is_found_by_its_remote_app() {
    // A TV maker's description, but the FireTVRemote DIAL app answers.
    let app = fixture("aftma08c15-firetvremote.http");
    let app_port = http_server(vec![("/apps/FireTVRemote", app)]).await;
    let dd = format!(
        "HTTP/1.1 200 OK\r\nApplication-URL: http://127.0.0.1:{app_port}/apps/\r\n\r\n<root><device><friendlyName>Bedroom</friendlyName><manufacturer>Toshiba</manufacturer><modelName>55C350</modelName></device></root>"
    );
    let dd_port = http_server(vec![("/dd.xml", dd.into_bytes())]).await;
    let target = ssdp_responder(format!("http://127.0.0.1:{dd_port}/dd.xml")).await;
    let found = SystemDial { target }
        .find(&[], Duration::from_millis(800))
        .await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].instance, "Bedroom (Fire TV)");
    assert_eq!(found[0].port, app_port);
}

#[tokio::test]
async fn a_google_tv_over_dial_is_left_out() {
    let dd = fixture("googletv-dd.http");
    let dd_port = http_server(vec![("/ssdp/device-desc.xml", dd)]).await;
    let target = ssdp_responder(format!("http://127.0.0.1:{dd_port}/ssdp/device-desc.xml")).await;
    let found = SystemDial { target }
        .find(&[], Duration::from_millis(600))
        .await;
    assert!(found.is_empty());
}

struct Fixed(Vec<Ipv4Addr>);
impl Interfaces for Fixed {
    fn ipv4(&self) -> Vec<Ipv4Addr> {
        self.0.clone()
    }
}

/// A querier whose sockets never hear anything.
struct Silent;
struct SilentSock;
impl SocketFactory for Silent {
    fn open(&self, _iface: Ipv4Addr) -> std::io::Result<Box<dyn QuerySocket>> {
        Ok(Box::new(SilentSock))
    }
}
#[async_trait]
impl QuerySocket for SilentSock {
    async fn send_query(&self, _packet: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    async fn recv(&self, _buf: &mut [u8]) -> std::io::Result<(usize, Ipv4Addr)> {
        std::future::pending().await
    }
}

struct FakeDial(Vec<MdnsService>);
#[async_trait]
impl DialFinder for FakeDial {
    async fn find(&self, ifaces: &[Ipv4Addr], _timeout: Duration) -> Vec<MdnsService> {
        assert_eq!(ifaces, [Ipv4Addr::new(192, 168, 88, 10)]);
        self.0.clone()
    }
}

#[tokio::test]
async fn without_an_os_resolver_the_dial_fire_tv_connects_on_5555() {
    // Windows and Linux: no OS resolver, the stick comes from DIAL alone.
    let dial = FakeDial(vec![MdnsService {
        instance: "label's Fire TV Stick".into(),
        kind: ServiceKind::FireTv,
        ip: STICK,
        port: 8009,
    }]);
    let found = discover_all(
        Duration::from_millis(100),
        &Fixed(vec![Ipv4Addr::new(192, 168, 88, 10)]),
        &Silent,
        &NoResolver,
        &dial,
    )
    .await
    .unwrap();
    let tvs = group_services(&found);
    assert_eq!(tvs.len(), 1);
    assert!(tvs[0].fire_tv);
    assert_eq!(tvs[0].name, "label's Fire TV Stick");
    assert_eq!(tvs[0].ip, "192.168.88.218");
    assert_eq!(tvs[0].connect_port, Some(5555));
    assert_eq!(tvs[0].pair_port, None);
}
