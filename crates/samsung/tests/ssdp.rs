mod support;

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use retv_bridge_samsung::ssdp::{
    discover, discover_via, location_ip, m_search, m_search_for, probe_hosts, subnet_hosts,
    DIAL_TARGET, SEARCH_TARGET,
};
use support::{fixture, http_server};

#[test]
fn m_search_has_man_and_st() {
    let m = m_search();
    assert!(m.starts_with("M-SEARCH * HTTP/1.1\r\n"));
    assert!(m.contains("\r\nMAN: \"ssdp:discover\"\r\n"));
    assert!(m.contains(&format!("\r\nST: {SEARCH_TARGET}\r\n")));
    assert!(m.ends_with("\r\n\r\n"));
}

#[test]
fn location_ip_reads_any_case() {
    let r = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nlocation: http://192.168.88.219:7678/nservice/\r\nST: x\r\n\r\n";
    assert_eq!(location_ip(r), Some(Ipv4Addr::new(192, 168, 88, 219)));
    let r = "HTTP/1.1 200 OK\r\nLOCATION:http://10.0.0.7/dmr.xml\r\n\r\n";
    assert_eq!(location_ip(r), Some(Ipv4Addr::new(10, 0, 0, 7)));
}

#[test]
fn location_ip_needs_an_ipv4() {
    assert_eq!(
        location_ip("HTTP/1.1 200 OK\r\nLOCATION: http://tv.local/x\r\n\r\n"),
        None
    );
    assert_eq!(location_ip("HTTP/1.1 200 OK\r\n\r\n"), None);
}

#[tokio::test]
async fn discover_dedupes_and_probes() {
    let body = fixture("api_v2.json");
    let rest = http_server(move |_| (200, body.clone())).await;
    let fake = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = fake.local_addr().unwrap();
    let rest_port = rest.port();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = fake.recv_from(&mut buf).await else {
                return;
            };
            assert!(String::from_utf8_lossy(&buf[..n]).contains(SEARCH_TARGET));
            let answer = format!(
                "HTTP/1.1 200 OK\r\nST: {SEARCH_TARGET}\r\nLOCATION: http://127.0.0.1:{rest_port}/smp_2_\r\n\r\n"
            );
            // Twice, as TVs do.
            fake.send_to(answer.as_bytes(), from).await.unwrap();
            fake.send_to(answer.as_bytes(), from).await.unwrap();
        }
    });
    let tvs = discover(target, Duration::from_millis(400), rest_port, |_| true).await;
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].ip, "127.0.0.1");
    assert_eq!(tvs[0].model_name, "QE55Q67AAUXXH");
    // The app's filter: home network only, so loopback is dropped.
    let none = discover(target, Duration::from_millis(300), rest_port, |ip| {
        !ip.is_loopback()
    })
    .await;
    assert!(none.is_empty());
}

#[test]
fn m_search_for_other_targets() {
    assert!(m_search_for(DIAL_TARGET).contains(&format!("\r\nST: {DIAL_TARGET}\r\n")));
    assert_eq!(m_search(), m_search_for(SEARCH_TARGET));
}

/// Every interface gets its own socket (multicast leaves on that interface),
/// and both search targets are asked.
#[tokio::test]
async fn discover_asks_from_every_interface_with_both_targets() {
    let body = fixture("api_v2.json");
    let rest = http_server(move |_| (200, body.clone())).await;
    let rest_port = rest.port();
    let fake = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = fake.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = fake.recv_from(&mut buf).await else {
                return;
            };
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            seen2.lock().unwrap().push((from, text));
            let answer = format!(
                "HTTP/1.1 200 OK\r\nST: {SEARCH_TARGET}\r\nLOCATION: http://127.0.0.1:{rest_port}/smp_2_\r\n\r\n"
            );
            let _ = fake.send_to(answer.as_bytes(), from).await;
        }
    });
    let tvs = discover_via(
        target,
        Duration::from_millis(400),
        rest_port,
        |_| true,
        &[Ipv4Addr::LOCALHOST],
    )
    .await;
    assert_eq!(tvs.len(), 1);
    let seen = seen.lock().unwrap().clone();
    let sources: BTreeSet<_> = seen.iter().map(|(from, _)| *from).collect();
    assert!(
        sources.len() >= 2,
        "one socket per interface plus the default: {sources:?}"
    );
    assert!(seen.iter().any(|(_, t)| t.contains(SEARCH_TARGET)));
    assert!(seen.iter().any(|(_, t)| t.contains(DIAL_TARGET)));
}

#[test]
fn subnet_hosts_are_the_home_24s_without_us() {
    let ours = [
        Ipv4Addr::new(192, 168, 88, 234),
        Ipv4Addr::new(192, 168, 88, 240),
        Ipv4Addr::new(10, 0, 0, 5),
        Ipv4Addr::new(8, 8, 8, 8),
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(169, 254, 3, 4),
    ];
    let hosts = subnet_hosts(&ours);
    assert_eq!(
        hosts.len(),
        2 * 254 - 3,
        "two /24s, our own three addresses left out"
    );
    assert!(hosts.contains(&Ipv4Addr::new(192, 168, 88, 219)));
    assert!(hosts.contains(&Ipv4Addr::new(10, 0, 0, 1)));
    assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 88, 234)));
    assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 88, 0)));
    assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 88, 255)));
    assert!(!hosts.iter().any(|h| h.octets()[0] == 8 || h.is_loopback()));
    assert!(subnet_hosts(&[]).is_empty());
}

#[tokio::test]
async fn probe_hosts_finds_the_tv_and_stays_bounded() {
    let body = fixture("api_v2.json");
    let rest = http_server(move |_| (200, body.clone())).await;
    let tvs = probe_hosts(
        vec![Ipv4Addr::LOCALHOST],
        rest.port(),
        Duration::from_millis(350),
        48,
    )
    .await;
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].model_name, "QE55Q67AAUXXH");
    // A black-hole address (TEST-NET-1) a hundred times: the timeout and the
    // concurrency keep it near 3 x 350 ms.
    let start = Instant::now();
    let none = probe_hosts(
        vec![Ipv4Addr::new(192, 0, 2, 1); 100],
        rest.port(),
        Duration::from_millis(350),
        48,
    )
    .await;
    assert!(none.is_empty());
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}
