//! Task 21a: the pairing service comes from the OS's own mDNS resolver (an
//! Android 14 TV answers it only by multicast to 5353, which the bridge
//! never binds) and is merged with what the querier finds.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use retv_bridge_mdns::{
    discover_merged, group_services, os_services, DiscoverError, Interfaces, MdnsService, OsAnswer,
    OsResolver, QuerySocket, ServiceKind, SocketFactory,
};

struct Fixed(Vec<Ipv4Addr>);
impl Interfaces for Fixed {
    fn ipv4(&self) -> Vec<Ipv4Addr> {
        self.0.clone()
    }
}

/// A querier socket that answers each query with `answer` from `from`, or
/// fails every send with `send_err`.
struct Sockets {
    answer: Option<(Vec<u8>, Ipv4Addr)>,
    send_err: Option<io::ErrorKind>,
}

struct Sock {
    answer: Option<(Vec<u8>, Ipv4Addr)>,
    send_err: Option<io::ErrorKind>,
    sent: tokio::sync::Notify,
    done: std::sync::atomic::AtomicBool,
}

impl SocketFactory for Sockets {
    fn open(&self, _iface: Ipv4Addr) -> io::Result<Box<dyn QuerySocket>> {
        Ok(Box::new(Sock {
            answer: self.answer.clone(),
            send_err: self.send_err,
            sent: tokio::sync::Notify::new(),
            done: Default::default(),
        }))
    }
}

#[async_trait]
impl QuerySocket for Sock {
    async fn send_query(&self, _packet: &[u8]) -> io::Result<()> {
        if let Some(k) = self.send_err {
            return Err(io::Error::from(k));
        }
        self.sent.notify_one();
        Ok(())
    }
    async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, Ipv4Addr)> {
        match &self.answer {
            Some((p, from)) if !self.done.swap(true, std::sync::atomic::Ordering::SeqCst) => {
                self.sent.notified().await;
                buf[..p.len()].copy_from_slice(p);
                Ok((p.len(), *from))
            }
            _ => std::future::pending().await,
        }
    }
}

#[derive(Default)]
struct FakeResolver {
    answers: Vec<OsAnswer>,
    fail: bool,
    asked: Mutex<Vec<(ServiceKind, Duration)>>,
}

#[async_trait]
impl OsResolver for FakeResolver {
    async fn browse(&self, kind: ServiceKind, timeout: Duration) -> io::Result<Vec<OsAnswer>> {
        self.asked.lock().unwrap().push((kind, timeout));
        if self.fail {
            return Err(io::Error::other("mDNSResponder not running"));
        }
        Ok(self.answers.clone())
    }
}

fn pairing(instance: &str, port: u16, addrs: &[IpAddr]) -> OsAnswer {
    OsAnswer {
        instance: instance.into(),
        service_type: "_adb-tls-pairing._tcp.".into(),
        port,
        addrs: addrs.to_vec(),
    }
}

const S3: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 232);

/// The S3's `_adb._tcp` answer, as the querier gets it.
fn adb_answer() -> Vec<u8> {
    use simple_dns::rdata::{RData, A, PTR, SRV};
    use simple_dns::{Name, Packet, ResourceRecord, CLASS};
    let mut pkt = Packet::new_reply(0);
    let inst = "adb-2b1c0f4e._adb._tcp.local";
    pkt.answers.push(ResourceRecord::new(
        Name::new_unchecked("_adb._tcp.local"),
        CLASS::IN,
        120,
        RData::PTR(PTR(Name::new_unchecked(inst))),
    ));
    pkt.answers.push(ResourceRecord::new(
        Name::new_unchecked(inst),
        CLASS::IN,
        120,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: 5555,
            target: Name::new_unchecked("Android.local"),
        }),
    ));
    pkt.additional_records.push(ResourceRecord::new(
        Name::new_unchecked("Android.local"),
        CLASS::IN,
        120,
        RData::A(A {
            address: u32::from(S3),
        }),
    ));
    pkt.build_bytes_vec().unwrap()
}

#[test]
fn os_answers_become_pairing_services() {
    let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    let got = os_services(&[
        pairing("adb-2b1c0f4e-Xy1Zab", 36183, &[v6, IpAddr::V4(S3)]),
        // Service types come with or without the domain and the final dot.
        OsAnswer {
            instance: "adb-other".into(),
            service_type: "_ADB-TLS-PAIRING._tcp.local.".into(),
            port: 40000,
            addrs: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7))],
        },
        // Dropped: no IPv4 address, port 0, a type that is not adb's.
        pairing("v6-only", 36184, &[v6]),
        pairing("no-port", 0, &[IpAddr::V4(S3)]),
        OsAnswer {
            instance: "printer".into(),
            service_type: "_ipp._tcp.".into(),
            port: 631,
            addrs: vec![IpAddr::V4(S3)],
        },
        // The same instance twice (two interfaces) is one service.
        pairing("adb-2b1c0f4e-Xy1Zab", 36183, &[IpAddr::V4(S3)]),
    ]);
    assert_eq!(
        got,
        vec![
            MdnsService {
                instance: "adb-2b1c0f4e-Xy1Zab".into(),
                kind: ServiceKind::Pairing,
                ip: S3,
                port: 36183,
            },
            MdnsService {
                instance: "adb-other".into(),
                kind: ServiceKind::Pairing,
                ip: Ipv4Addr::new(192, 168, 1, 7),
                port: 40000,
            },
        ]
    );
}

#[test]
fn every_ipv4_address_of_an_instance_counts() {
    let other = Ipv4Addr::new(10, 0, 0, 5);
    let got = os_services(&[pairing(
        "adb-x",
        36183,
        &[IpAddr::V4(S3), IpAddr::V4(other)],
    )]);
    assert_eq!(
        got.iter().map(|s| s.ip).collect::<Vec<_>>(),
        vec![S3, other]
    );
}

#[tokio::test]
async fn the_os_pairing_port_joins_the_querier_tv() {
    let resolver = FakeResolver {
        answers: vec![pairing("adb-2b1c0f4e-Xy1Zab", 36183, &[IpAddr::V4(S3)])],
        ..Default::default()
    };
    let sockets = Sockets {
        answer: Some((adb_answer(), S3)),
        send_err: None,
    };
    let timeout = Duration::from_millis(300);
    let found = discover_merged(
        timeout,
        &Fixed(vec![Ipv4Addr::new(192, 168, 1, 10)]),
        &sockets,
        &resolver,
    )
    .await
    .unwrap();
    assert_eq!(
        *resolver.asked.lock().unwrap(),
        vec![(ServiceKind::Pairing, timeout)]
    );
    // The querier's services come first, so the TV keeps its `_adb` name.
    assert_eq!(found[0].kind, ServiceKind::Adb);
    let tvs = group_services(&found);
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].name, "adb-2b1c0f4e");
    assert_eq!(tvs[0].ip, "192.168.1.232");
    assert_eq!(tvs[0].connect_port, Some(5555));
    assert_eq!(tvs[0].pair_port, Some(36183));
}

#[tokio::test]
async fn a_failing_os_resolver_leaves_the_querier_result() {
    let resolver = FakeResolver {
        fail: true,
        ..Default::default()
    };
    let sockets = Sockets {
        answer: Some((adb_answer(), S3)),
        send_err: None,
    };
    let found = discover_merged(
        Duration::from_millis(300),
        &Fixed(vec![Ipv4Addr::new(192, 168, 1, 10)]),
        &sockets,
        &resolver,
    )
    .await
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, ServiceKind::Adb);
}

#[tokio::test]
async fn os_results_stand_when_the_querier_fails() {
    let resolver = FakeResolver {
        answers: vec![pairing("adb-x", 36183, &[IpAddr::V4(S3)])],
        ..Default::default()
    };
    let denied = Sockets {
        answer: None,
        send_err: Some(io::ErrorKind::HostUnreachable),
    };
    let ifaces = Fixed(vec![Ipv4Addr::new(192, 168, 1, 10)]);
    let found = discover_merged(Duration::from_millis(300), &ifaces, &denied, &resolver)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].port, 36183);

    // Nothing from either: the querier's error stands.
    let empty = FakeResolver::default();
    assert_eq!(
        discover_merged(Duration::from_millis(300), &ifaces, &denied, &empty).await,
        Err(DiscoverError::LocalNetworkDenied)
    );
}

#[tokio::test]
async fn os_results_without_interfaces() {
    // No IPv4 interface for the querier: the OS resolver is still asked.
    let resolver = Arc::new(FakeResolver {
        answers: vec![pairing("adb-x", 36183, &[IpAddr::V4(S3)])],
        ..Default::default()
    });
    let sockets = Sockets {
        answer: None,
        send_err: None,
    };
    let found = discover_merged(
        Duration::from_millis(100),
        &Fixed(vec![]),
        &sockets,
        resolver.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(found.len(), 1);
}
