mod common;

use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{response, Announce};
use retv_bridge_mdns::{
    build_query, discover_with, DiscoverError, Interfaces, QuerySocket, ServiceKind, SocketFactory,
    UdpQuerySocket,
};
use simple_dns::{Packet, QTYPE, TYPE};

struct Fixed(Vec<Ipv4Addr>);
impl Interfaces for Fixed {
    fn ipv4(&self) -> Vec<Ipv4Addr> {
        self.0.clone()
    }
}

#[derive(Default)]
struct Log {
    opened: Vec<Ipv4Addr>,
    sent: Vec<Ipv4Addr>,
}

struct Fake {
    log: Arc<Mutex<Log>>,
    send_err: Option<io::ErrorKind>,
    /// Answer every query with this packet from this source.
    answer: Option<(Vec<u8>, Ipv4Addr)>,
    /// Extra packets delivered after the first answer.
    more: Vec<Vec<u8>>,
}

struct FakeSocket {
    iface: Ipv4Addr,
    log: Arc<Mutex<Log>>,
    send_err: Option<io::ErrorKind>,
    answer: Option<(Vec<u8>, Ipv4Addr)>,
    more: Vec<Vec<u8>>,
    next: std::sync::atomic::AtomicUsize,
    sent: tokio::sync::Notify,
}

impl SocketFactory for Fake {
    fn open(&self, iface: Ipv4Addr) -> io::Result<Box<dyn QuerySocket>> {
        self.log.lock().unwrap().opened.push(iface);
        Ok(Box::new(FakeSocket {
            iface,
            log: self.log.clone(),
            send_err: self.send_err,
            answer: self.answer.clone(),
            more: self.more.clone(),
            next: Default::default(),
            sent: tokio::sync::Notify::new(),
        }))
    }
}

#[async_trait]
impl QuerySocket for FakeSocket {
    async fn send_query(&self, _packet: &[u8]) -> io::Result<()> {
        self.log.lock().unwrap().sent.push(self.iface);
        if let Some(k) = self.send_err {
            return Err(io::Error::from(k));
        }
        self.sent.notify_one();
        Ok(())
    }
    async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, Ipv4Addr)> {
        match &self.answer {
            Some((p, from)) => {
                let i = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if i == 0 {
                    self.sent.notified().await;
                    buf[..p.len()].copy_from_slice(p);
                    return Ok((p.len(), *from));
                }
                match self.more.get(i - 1) {
                    Some(m) => {
                        buf[..m.len()].copy_from_slice(m);
                        Ok((m.len(), *from))
                    }
                    None => std::future::pending().await,
                }
            }
            None => std::future::pending().await,
        }
    }
}

#[test]
fn query_sets_qu_bit_and_asks_three_services() {
    let bytes = build_query();
    let pkt = Packet::parse(&bytes).unwrap();
    let names: Vec<String> = pkt.questions.iter().map(|q| q.qname.to_string()).collect();
    assert_eq!(
        names,
        [
            "_adb-tls-connect._tcp.local",
            "_adb-tls-pairing._tcp.local",
            "_adb._tcp.local"
        ]
    );
    assert!(pkt
        .questions
        .iter()
        .all(|q| q.qtype == QTYPE::TYPE(TYPE::PTR) && q.unicast_response));
    // Raw wire check: every question ends with class 0x8001 (IN + QU).
    assert_eq!(
        bytes.windows(4).filter(|w| w == &[0, 12, 0x80, 1]).count(),
        3
    );
}

#[tokio::test]
async fn queries_every_ipv4_interface() {
    let log = Arc::new(Mutex::new(Log::default()));
    let f = Fake {
        log: log.clone(),
        send_err: None,
        answer: None,
        more: vec![],
    };
    let a: Ipv4Addr = "192.0.2.10".parse().unwrap();
    let b: Ipv4Addr = "198.51.100.10".parse().unwrap();
    let out = discover_with(Duration::from_millis(400), &Fixed(vec![a, b]), &f)
        .await
        .unwrap();
    assert!(out.is_empty());
    let log = log.lock().unwrap();
    assert_eq!(log.opened, vec![a, b]);
    // twice per socket
    assert_eq!(log.sent.iter().filter(|x| **x == a).count(), 2);
    assert_eq!(log.sent.iter().filter(|x| **x == b).count(), 2);
}

#[tokio::test]
async fn collects_unicast_answers() {
    let pkt = response(&[Announce {
        service: "_adb-tls-connect._tcp.local",
        instance: "adb-ABC",
        port: 37123,
        host: "tv.local",
        ip: Some("192.0.2.7".parse().unwrap()),
    }]);
    let f = Fake {
        log: Default::default(),
        send_err: None,
        answer: Some((pkt, "192.0.2.7".parse().unwrap())),
        more: vec![],
    };
    let out = discover_with(
        Duration::from_millis(400),
        &Fixed(vec!["192.0.2.10".parse().unwrap()]),
        &f,
    )
    .await
    .unwrap();
    assert_eq!(out.len(), 1, "deduplicated across both rounds");
    assert_eq!(out[0].kind, ServiceKind::Connect);
    assert_eq!(out[0].port, 37123);
}

#[tokio::test]
async fn send_ehostunreach_everywhere_is_local_network_denied() {
    let f = Fake {
        log: Default::default(),
        send_err: Some(io::ErrorKind::HostUnreachable),
        answer: None,
        more: vec![],
    };
    let r = discover_with(
        Duration::from_millis(400),
        &Fixed(vec![
            "192.0.2.10".parse().unwrap(),
            "198.51.100.10".parse().unwrap(),
        ]),
        &f,
    )
    .await;
    assert_eq!(r, Err(DiscoverError::LocalNetworkDenied));
}

#[tokio::test]
async fn real_socket_binds_an_ephemeral_port_on_the_interface() {
    let s = UdpQuerySocket::bind(Ipv4Addr::LOCALHOST).unwrap();
    let addr = s.local_addr().unwrap();
    assert_eq!(addr.ip(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_ne!(addr.port(), 5353);
    assert_ne!(addr.port(), 0);
}

#[tokio::test]
async fn real_socket_sets_ip_multicast_if_to_its_interface() {
    let s = UdpQuerySocket::bind(Ipv4Addr::LOCALHOST).unwrap();
    assert_eq!(s.multicast_if().unwrap(), Ipv4Addr::LOCALHOST);
}

#[tokio::test]
async fn distinct_services_are_capped() {
    let hosts: Vec<(String, String)> = (0..300)
        .map(|i| (format!("adb-{i}"), format!("h{i}.local")))
        .collect();
    let packets: Vec<Vec<u8>> = hosts
        .chunks(30)
        .map(|chunk| {
            let items: Vec<Announce> = chunk
                .iter()
                .map(|(inst, host)| Announce {
                    service: "_adb-tls-connect._tcp.local",
                    instance: inst,
                    port: 4000,
                    host,
                    ip: Some("192.168.1.7".parse().unwrap()),
                })
                .collect();
            response(&items)
        })
        .collect();
    let f = Fake {
        log: Default::default(),
        send_err: None,
        answer: Some((packets[0].clone(), "192.168.1.7".parse().unwrap())),
        more: packets[1..].to_vec(),
    };
    let out = discover_with(
        Duration::from_millis(400),
        &Fixed(vec!["192.168.1.10".parse().unwrap()]),
        &f,
    )
    .await
    .unwrap();
    assert_eq!(out.len(), retv_bridge_mdns::MAX_SERVICES);
}
