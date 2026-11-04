mod support;

use std::net::Ipv4Addr;

use retv_bridge_samsung::tv::{fetch_info, mac_ip_for, parse_info, year_of};
use support::{fixture, http_server};

#[test]
fn parses_the_q67aa_answer() {
    let info = parse_info("192.168.88.219", &fixture("api_v2.json")).unwrap();
    assert_eq!(info.ip, "192.168.88.219");
    assert_eq!(info.name, "[TV] Samsung Q67AA 55 TV");
    assert_eq!(info.model_name, "QE55Q67AAUXXH");
    assert_eq!(info.model_code, "21_NIKEL_UHD");
    assert_eq!(info.year, Some(2021));
    assert!(info.developer_mode);
    assert_eq!(info.developer_ip.as_deref(), Some("192.168.88.234"));
    assert!(info.host_ip_ok("192.168.88.234"));
    assert!(!info.host_ip_ok("192.168.88.10"));
}

#[test]
fn developer_ip_zero_is_none() {
    for ip in ["0.0.0.0", "", "  "] {
        let json = fixture("api_v2.json")
            .replace("\"192.168.88.234\"", &format!("\"{ip}\""))
            .replace("\"developerMode\": \"1\"", "\"developerMode\": \"0\"");
        let info = parse_info("192.168.88.219", &json).unwrap();
        assert_eq!(info.developer_ip, None, "{ip:?}");
        assert!(!info.developer_mode);
        assert!(!info.host_ip_ok("192.168.88.234"));
    }
}

#[test]
fn not_samsung_is_refused() {
    assert!(parse_info("192.168.1.2", "{\"hello\":1}").is_err());
    assert!(parse_info("192.168.1.2", "<html>").is_err());
}

#[test]
fn year_from_the_model_code() {
    assert_eq!(year_of("21_NIKEL_UHD"), Some(2021));
    assert_eq!(year_of("18_KANTM2_UHD"), Some(2018));
    assert_eq!(year_of("NIKEL"), None);
    assert_eq!(year_of(""), None);
}

#[test]
fn mac_ip_prefers_the_tvs_subnet() {
    let ours = [
        Ipv4Addr::new(10, 8, 0, 2),
        Ipv4Addr::new(192, 168, 88, 234),
        Ipv4Addr::new(127, 0, 0, 1),
    ];
    assert_eq!(
        mac_ip_for("192.168.88.219", &ours),
        Some(Ipv4Addr::new(192, 168, 88, 234))
    );
    // No address in the TV's /24: the first home-network one.
    assert_eq!(
        mac_ip_for("192.168.1.5", &ours),
        Some(Ipv4Addr::new(10, 8, 0, 2))
    );
    assert_eq!(mac_ip_for("192.168.1.5", &[Ipv4Addr::LOCALHOST]), None);
}

#[tokio::test]
async fn fetch_info_reads_a_fake_tv() {
    let body = fixture("api_v2.json");
    let addr = http_server(move |req| {
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/api/v2/");
        (200, body.clone())
    })
    .await;
    let info = fetch_info("127.0.0.1", addr.port()).await.unwrap();
    assert_eq!(info.ip, "127.0.0.1");
    assert_eq!(info.model_name, "QE55Q67AAUXXH");
}

#[tokio::test]
async fn fetch_info_unreachable() {
    // A port nothing listens on.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    assert!(fetch_info("127.0.0.1", port).await.is_err());
}
