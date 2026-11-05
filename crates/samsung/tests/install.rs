mod support;

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use retv_bridge_samsung::bundle::{ids, install, other_author, read_duid, reinstall, shipped};
use retv_bridge_samsung::certs::SamsungCert;
use retv_bridge_samsung::csr::{key_pem, new_key};
use retv_bridge_samsung::sdb::PUSH_PATH;
use retv_bridge_samsung::signer::WgtFile;
use retv_bridge_samsung::SamsungError;
use support::fake_sdbd::{FakeSdbd, Mode};

const CONFIG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<widget xmlns:tizen="http://tizen.org/ns/widgets" xmlns="http://www.w3.org/ns/widgets" id="https://retv.app/tizen" version="0.2.0">
    <tizen:application id="RetvHome00.ReTVHome" package="RetvHome00" required_version="3.0"/>
</widget>"#;
const CERT: &str = "-----BEGIN CERTIFICATE-----\nQUFBQQ==\n-----END CERTIFICATE-----\n";

fn cert(duid: &str) -> SamsungCert {
    SamsungCert {
        duid: duid.into(),
        author_key_pem: key_pem(&new_key()).to_string(),
        author_chain: vec![CERT.into(), CERT.into()],
        distributor_key_pem: key_pem(&new_key()).to_string(),
        distributor_chain: vec![CERT.into(), CERT.into()],
        profile_xml: String::new(),
        created_ms: 0,
    }
}

fn files() -> Vec<WgtFile> {
    vec![
        WgtFile::new("config.xml", CONFIG.as_bytes().to_vec()),
        WgtFile::new("index.html", b"<html></html>".to_vec()),
    ]
}

fn tv(install_out: &str) -> HashMap<String, String> {
    HashMap::from([
        ("0 getduid".to_string(), "2DCNLQ722RJQQ\n".to_string()),
        (
            format!("0 vd_appinstall RetvHome00 {PUSH_PATH}"),
            install_out.to_string(),
        ),
        (
            "0 was_execute RetvHome00.ReTVHome".to_string(),
            "launched\n".to_string(),
        ),
    ])
}

#[test]
fn bundle_skips_dev_and_service_sources() {
    for ok in [
        "config.xml",
        "main.js",
        "logic.js",
        "folders.js",
        "dns.js",
        "icon.png",
        "service/dist/index.js",
        "service/dist/client.js",
    ] {
        assert!(shipped(ok), "{ok}");
    }
    for no in [
        "dev.js",
        "service/src/sdb.js",
        "service/build.js",
        "service/test/fake-tv.js",
        "service/PROTOCOL.md",
        ".DS_Store",
        "author-signature.xml",
        "signature1.xml",
    ] {
        assert!(!shipped(no), "{no}");
    }
}

#[test]
fn ids_from_config() {
    assert_eq!(
        ids(CONFIG),
        Some(("RetvHome00".to_string(), "RetvHome00.ReTVHome".to_string()))
    );
    assert_eq!(ids("<widget/>"), None);
}

#[tokio::test]
async fn install_happy_path_on_fake_sdbd() {
    let fake = FakeSdbd::start(
        tv("installing[9]\ninstalling[100]\nspend time for pkgcmd is [900]ms\ninstall completed\n"),
        Mode::Normal,
    )
    .await;
    let steps: Arc<Mutex<Vec<String>>> = Arc::default();
    let steps2 = steps.clone();
    let app = install(
        fake.addr,
        files(),
        &cert("2DCNLQ722RJQQ"),
        &move |s: &str| steps2.lock().unwrap().push(s.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(app, "RetvHome00.ReTVHome");
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(
        seen.services,
        [
            "shell:0 getduid".to_string(),
            "sync:".to_string(),
            format!("shell:0 vd_appinstall RetvHome00 {PUSH_PATH}"),
            "shell:0 was_execute RetvHome00.ReTVHome".to_string(),
        ]
    );
    assert_eq!(seen.pushed[0].0, format!("{PUSH_PATH},{}", 0o100644));
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(seen.pushed[0].1.clone())).unwrap();
    let mut sig = String::new();
    z.by_name("signature1.xml")
        .unwrap()
        .read_to_string(&mut sig)
        .unwrap();
    assert!(sig.contains("URI=\"author-signature.xml\""));
    assert!(z.by_name("config.xml").is_ok());
    assert!(!steps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn install_refuses_other_duid() {
    let fake = FakeSdbd::start(tv("install completed"), Mode::Normal).await;
    let err = install(fake.addr, files(), &cert("OTHERTV0001"), &|_: &str| {})
        .await
        .unwrap_err();
    assert_eq!(err, SamsungError::WrongCertificate);
    assert!(
        fake.seen.lock().unwrap().pushed.is_empty(),
        "nothing pushed"
    );
}

#[tokio::test]
async fn install_failed_reports_tail() {
    let fake = FakeSdbd::start(
        tv("installing[9]\napp_id[RetvHome00] install failed[118, -12]\nspend time for pkgcmd is [90]ms\n"),
        Mode::Normal,
    )
    .await;
    let err = install(fake.addr, files(), &cert("2DCNLQ722RJQQ"), &|_: &str| {})
        .await
        .unwrap_err();
    match err {
        SamsungError::InstallFailed(tail) => {
            assert!(tail.contains("install failed[118, -12]"), "{tail}");
            assert!(!tail.contains("installing["), "{tail}");
        }
        other => panic!("{other:?}"),
    }
    let services = fake.seen.lock().unwrap().services.clone();
    assert!(!services.iter().any(|s| s.contains("was_execute")));
}

const OTHER_SIGNER_OUT: &str = "installing[9]\napp_id[RetvHome00] install failed[118, -11], reason: Author certificate not match :\nspend time for pkgcmd is [120]ms\n";

#[test]
fn other_author_is_read_from_the_installer() {
    assert!(other_author(OTHER_SIGNER_OUT));
    assert!(other_author(
        "app_id[RetvHome00] install failed[118, -11]\n"
    ));
    assert!(!other_author(
        "app_id[RetvHome00] install failed[118, -12]\n"
    ));
    assert!(!other_author("install failed[118, -110]"));
    assert!(!other_author("install completed"));
}

#[tokio::test]
async fn install_over_another_author_says_so() {
    let fake = FakeSdbd::start(tv(OTHER_SIGNER_OUT), Mode::Normal).await;
    let err = install(fake.addr, files(), &cert("2DCNLQ722RJQQ"), &|_: &str| {})
        .await
        .unwrap_err();
    assert_eq!(err, SamsungError::OtherAuthor);
    let services = fake.seen.lock().unwrap().services.clone();
    assert!(
        !services.iter().any(|s| s.contains("vd_appuninstall")),
        "a plain install never removes the old app: {services:?}"
    );
}

#[tokio::test]
async fn reinstall_removes_the_old_app_then_installs() {
    let mut outputs = tv("installing[100]\ninstall completed\n");
    outputs.insert(
        "0 vd_appuninstall RetvHome00".into(),
        "app_id[RetvHome00] uninstall start\napp_id[RetvHome00] uninstall completed\n".into(),
    );
    let fake = FakeSdbd::start(outputs, Mode::Normal).await;
    let app = reinstall(fake.addr, files(), &cert("2DCNLQ722RJQQ"), &|_: &str| {})
        .await
        .unwrap();
    assert_eq!(app, "RetvHome00.ReTVHome");
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(
        seen.services,
        [
            "shell:0 getduid".to_string(),
            "shell:0 vd_appuninstall RetvHome00".to_string(),
            "sync:".to_string(),
            format!("shell:0 vd_appinstall RetvHome00 {PUSH_PATH}"),
            "shell:0 was_execute RetvHome00.ReTVHome".to_string(),
        ]
    );
}

#[tokio::test]
async fn reinstall_never_removes_with_the_wrong_certificate() {
    let fake = FakeSdbd::start(tv("install completed"), Mode::Normal).await;
    let err = reinstall(fake.addr, files(), &cert("OTHERTV0001"), &|_: &str| {})
        .await
        .unwrap_err();
    assert_eq!(err, SamsungError::WrongCertificate);
    let services = fake.seen.lock().unwrap().services.clone();
    assert_eq!(services, ["shell:0 getduid".to_string()]);
}

#[tokio::test]
async fn reinstall_stops_when_the_old_app_stays() {
    let mut outputs = tv("install completed");
    outputs.insert(
        "0 vd_appuninstall RetvHome00".into(),
        "uninstall failed[1, -1]\n".into(),
    );
    let fake = FakeSdbd::start(outputs, Mode::Normal).await;
    let err = reinstall(fake.addr, files(), &cert("2DCNLQ722RJQQ"), &|_: &str| {})
        .await
        .unwrap_err();
    assert!(matches!(err, SamsungError::InstallFailed(_)), "{err:?}");
    let seen = fake.seen.lock().unwrap().clone();
    assert!(seen.pushed.is_empty(), "nothing pushed");
}

#[tokio::test]
async fn duid_is_read_and_checked() {
    let fake = FakeSdbd::start(tv(""), Mode::Normal).await;
    assert_eq!(read_duid(fake.addr).await.unwrap(), "2DCNLQ722RJQQ");
    let fake = FakeSdbd::start(
        HashMap::from([("0 getduid".to_string(), "sh: not found".to_string())]),
        Mode::Normal,
    )
    .await;
    assert!(read_duid(fake.addr).await.is_err());
}

#[test]
fn bundled_files_when_built() {
    // In a checkout with service/dist built the bundle has config.xml and no dev.js.
    if let Some(files) = retv_bridge_samsung::bundle::bundled() {
        let paths: Vec<String> = files.iter().map(|f| f.path()).collect();
        assert!(paths.contains(&"config.xml".to_string()));
        assert!(paths.contains(&"service/dist/index.js".to_string()));
        assert!(!paths.contains(&"dev.js".to_string()));
    }
}
