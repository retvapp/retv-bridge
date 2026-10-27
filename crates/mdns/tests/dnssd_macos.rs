//! macOS only: a `_adb-tls-pairing._tcp` service registered with
//! `dns-sd -R` is found through mDNSResponder, with its port. Skipped when
//! `dns-sd` is missing.
#![cfg(target_os = "macos")]

use std::process::{Command, Stdio};
use std::time::Duration;

use retv_bridge_mdns::{os_services, OsResolver, ServiceKind, SystemResolver};

struct Registered(std::process::Child);

impl Drop for Registered {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn registered_pairing_service_is_found_with_its_port() {
    let dns_sd = std::path::Path::new("/usr/bin/dns-sd");
    if !dns_sd.exists() {
        eprintln!("skipped: no dns-sd");
        return;
    }
    let instance = format!("retv-test-{}", std::process::id());
    let port = 20_000 + (std::process::id() % 10_000) as u16;
    let _reg = Registered(
        Command::new(dns_sd)
            .args([
                "-R",
                &instance,
                "_adb-tls-pairing._tcp",
                "local",
                &port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("dns-sd -R"),
    );

    // The registration takes a moment; each browse waits up to 1.5 s.
    let mut found = Vec::new();
    for _ in 0..5 {
        let answers = SystemResolver
            .browse(ServiceKind::Pairing, Duration::from_millis(1500))
            .await
            .expect("browse");
        found = os_services(&answers)
            .into_iter()
            .filter(|s| s.instance == instance)
            .collect();
        if !found.is_empty() {
            break;
        }
    }
    assert!(!found.is_empty(), "{instance} not found");
    for s in &found {
        assert_eq!(s.kind, ServiceKind::Pairing);
        assert_eq!(s.port, port);
        assert!(!s.ip.is_unspecified());
    }
}
