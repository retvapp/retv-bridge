mod common;

use std::net::Ipv4Addr;
use std::path::PathBuf;

use common::{response, unhex, Announce};
use retv_bridge_mdns::{group_services, parse_response, MdnsService, ServiceKind};

fn kind(s: &str) -> ServiceKind {
    match s {
        "connect" => ServiceKind::Connect,
        "pairing" => ServiceKind::Pairing,
        "adb" => ServiceKind::Adb,
        other => panic!("unknown kind {other}"),
    }
}

fn fixtures_dir() -> PathBuf {
    // walk up from this crate to the folder that holds conformance/
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("conformance"))
        .find(|d| d.is_dir())
        .expect("conformance/ above the crate")
        .join("mdns")
}

/// Prints the hex of the synthetic announcement used by the fixture file.
#[test]
#[ignore]
fn print_synthetic_connect_hex() {
    let pkt = response(&[Announce {
        service: "_adb-tls-connect._tcp.local",
        instance: "adb-0000000000000000-AbCdEf",
        port: 37123,
        host: "Android-1.local",
        ip: Some("192.0.2.32".parse().unwrap()),
    }]);
    println!("{}", common::hex(&pkt));
}

#[test]
fn parses_synthetic_connect_fixture() {
    let path = fixtures_dir().join("synthetic-connect.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let got = parse_response(
        &unhex(v["packetHex"].as_str().unwrap()),
        v["from"].as_str().unwrap().parse().unwrap(),
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].kind, ServiceKind::Connect);
    assert_eq!(got[0].port, 37123);
    assert_eq!(got[0].ip, "192.0.2.32".parse::<Ipv4Addr>().unwrap());
}

#[test]
fn every_conformance_fixture_parses_to_its_expectation() {
    let mut count = 0;
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let from: Ipv4Addr = v["from"].as_str().unwrap().parse().unwrap();
        let packet = unhex(v["packetHex"].as_str().unwrap());
        let mut want: Vec<MdnsService> = v["expect"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| MdnsService {
                instance: e["instance"].as_str().unwrap().to_string(),
                kind: kind(e["kind"].as_str().unwrap()),
                ip: e["ip"].as_str().unwrap().parse().unwrap(),
                port: e["port"].as_u64().unwrap() as u16,
            })
            .collect();
        let mut got = parse_response(&packet, from);
        got.sort();
        want.sort();
        assert_eq!(got, want, "{path:?}");
        count += 1;
    }
    assert!(count >= 1, "no fixtures found");
}

#[test]
fn falls_back_to_packet_source_without_an_a_record() {
    let pkt = response(&[Announce {
        service: "_adb-tls-connect._tcp.local",
        instance: "adb-ABC",
        port: 37123,
        host: "tv.local",
        ip: None,
    }]);
    let got = parse_response(&pkt, "192.0.2.50".parse().unwrap());
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].ip, "192.0.2.50".parse::<Ipv4Addr>().unwrap());
    assert_eq!(got[0].port, 37123);
    assert_eq!(got[0].kind, ServiceKind::Connect);
}

#[test]
fn garbage_gives_nothing() {
    assert!(parse_response(&[1, 2, 3], "192.0.2.1".parse().unwrap()).is_empty());
}

#[test]
fn drops_non_lan_answers() {
    let pkt = response(&[Announce {
        service: "_adb-tls-connect._tcp.local",
        instance: "adb-CGNAT",
        port: 4000,
        host: "tv.local",
        ip: Some("100.64.0.5".parse().unwrap()),
    }]);
    let services = parse_response(&pkt, "192.0.2.1".parse().unwrap());
    assert_eq!(services.len(), 1);
    assert!(group_services(&services).is_empty());
}

#[test]
fn plain_adb_service_is_the_connect_port_when_no_tls_connect_is_announced() {
    let pkt = response(&[Announce {
        service: "_adb._tcp.local",
        instance: "adb-XYZ",
        port: 5555,
        host: "tv.local",
        ip: Some("192.168.1.9".parse().unwrap()),
    }]);
    let tvs = group_services(&parse_response(&pkt, "192.168.1.9".parse().unwrap()));
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].connect_port, Some(5555));
    assert_eq!(tvs[0].pair_port, None);
}

#[test]
fn groups_connect_and_pairing_per_ip() {
    let pkt = response(&[
        Announce {
            service: "_adb-tls-pairing._tcp.local",
            instance: "adb-ABC",
            port: 40111,
            host: "tv.local",
            ip: Some("192.168.1.7".parse().unwrap()),
        },
        Announce {
            service: "_adb-tls-connect._tcp.local",
            instance: "adb-ABC",
            port: 37123,
            host: "tv.local",
            ip: Some("192.168.1.7".parse().unwrap()),
        },
    ]);
    let tvs = group_services(&parse_response(&pkt, "192.168.1.7".parse().unwrap()));
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].name, "adb-ABC");
    assert_eq!(tvs[0].ip, "192.168.1.7");
    assert_eq!(tvs[0].connect_port, Some(37123));
    assert_eq!(tvs[0].pair_port, Some(40111));
}

#[test]
fn compressed_packets_still_parse() {
    let items = [
        Announce {
            service: "_adb-tls-pairing._tcp.local",
            instance: "adb-ABC",
            port: 40111,
            host: "tv.local",
            ip: Some("192.168.1.7".parse().unwrap()),
        },
        Announce {
            service: "_adb-tls-connect._tcp.local",
            instance: "adb-ABC",
            port: 37123,
            host: "tv.local",
            ip: Some("192.168.1.7".parse().unwrap()),
        },
    ];
    let plain = response(&items);
    let packed = common::response_compressed(&items);
    assert!(packed.len() < plain.len(), "really compressed");
    assert_eq!(
        parse_response(&packed, "192.168.1.7".parse().unwrap()).len(),
        2
    );
}

// ---- hostile packets: each must return promptly with nothing ----

fn header(qd: u16, an: u16, ns: u16, ar: u16) -> Vec<u8> {
    let mut h = vec![0, 0, 0x84, 0];
    for c in [qd, an, ns, ar] {
        h.extend_from_slice(&c.to_be_bytes());
    }
    h
}

fn parse_promptly(pkt: Vec<u8>) -> Vec<MdnsService> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(parse_response(&pkt, "192.168.1.1".parse().unwrap()));
    });
    rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("parse_response did not return")
}

#[test]
fn self_pointer_returns_nothing() {
    let mut p = header(1, 0, 0, 0);
    p.extend_from_slice(&[0xC0, 0x0C, 0, 12, 0, 1]);
    assert!(parse_promptly(p).is_empty());
}

#[test]
fn self_pointer_in_answer_name_and_srv_target_returns_nothing() {
    // answer: name = self pointer at 12, type SRV, rdata = prio, weight, port, target = self pointer
    let mut p = header(0, 1, 0, 0);
    p.extend_from_slice(&[0xC0, 0x0C, 0, 33, 0, 1, 0, 0, 0, 120, 0, 8]);
    p.extend_from_slice(&[0, 0, 0, 0, 0x0F, 0xA0, 0xC0, 0x0C]);
    assert!(parse_promptly(p).is_empty());
    // valid name, SRV target pointing at itself
    let mut q = header(0, 1, 0, 0);
    q.extend_from_slice(&[1, b'a', 0, 0, 33, 0, 1, 0, 0, 0, 120, 0, 8]);
    let target_off = (q.len() + 6) as u8;
    q.extend_from_slice(&[0, 0, 0, 0, 0x0F, 0xA0, 0xC0, target_off]);
    assert!(parse_promptly(q).is_empty());
}

#[test]
fn two_pointer_cycle_returns_nothing() {
    let mut p = header(1, 0, 0, 0);
    p.extend_from_slice(&[0xC0, 0x0E, 0xC0, 0x0C, 0, 12, 0, 1]);
    assert!(parse_promptly(p).is_empty());
}

#[test]
fn label_then_pointer_back_to_itself_returns_nothing() {
    let mut p = header(1, 0, 0, 0);
    p.extend_from_slice(&[1, b'a', 0xC0, 0x0C, 0, 12, 0, 1]);
    assert!(parse_promptly(p).is_empty());
}

#[test]
fn huge_counts_with_short_body_return_nothing() {
    let mut p = header(65535, 65535, 65535, 65535);
    p.extend_from_slice(&[0, 0, 12, 0, 1]);
    assert!(parse_promptly(p).is_empty());
}

#[test]
fn truncated_rdata_returns_nothing() {
    let good = response(&[Announce {
        service: "_adb-tls-connect._tcp.local",
        instance: "adb-ABC",
        port: 37123,
        host: "tv.local",
        ip: Some("192.168.1.7".parse().unwrap()),
    }]);
    assert_eq!(parse_promptly(good.clone()).len(), 1);
    for cut in [good.len() - 1, good.len() - 3, good.len() / 2, 13] {
        assert!(parse_promptly(good[..cut].to_vec()).is_empty(), "cut {cut}");
    }
}

#[test]
fn ttl_zero_goodbye_is_dropped() {
    use simple_dns::rdata::{RData, SRV};
    use simple_dns::{Name, Packet, ResourceRecord, CLASS};
    let mut pkt = Packet::new_reply(0);
    pkt.answers.push(ResourceRecord::new(
        Name::new_unchecked("adb-ABC._adb-tls-connect._tcp.local"),
        CLASS::IN,
        0,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: 4000,
            target: Name::new_unchecked("tv.local"),
        }),
    ));
    let bytes = pkt.build_bytes_vec().unwrap();
    assert!(parse_response(&bytes, "192.168.1.7".parse().unwrap()).is_empty());
}
