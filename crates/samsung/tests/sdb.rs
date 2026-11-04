mod support;

use std::collections::HashMap;

use retv_bridge_samsung::sdb::{packet, Sdb, SdbError, PUSH_PATH};
use support::fake_sdbd::{FakeSdbd, Mode};

fn outputs() -> HashMap<String, String> {
    HashMap::from([
        ("0 getduid".to_string(), "2DCNLQ722RJQQ\n".to_string()),
        (
            "0 vd_applist".to_string(),
            "get app list...\napps count: 0\n".to_string(),
        ),
    ])
}

#[test]
fn packet_header_checksum_and_magic() {
    let p = packet(*b"OPEN", 1, 0, b"ab\0");
    assert_eq!(&p[0..4], b"OPEN");
    assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(p[16..20].try_into().unwrap()), 97 + 98);
    assert_eq!(
        u32::from_le_bytes(p[20..24].try_into().unwrap()),
        u32::from_le_bytes(*b"OPEN") ^ 0xffff_ffff
    );
    assert_eq!(&p[24..], b"ab\0");
}

#[tokio::test]
async fn shell_collects_writes_until_close() {
    let fake = FakeSdbd::start(outputs(), Mode::Normal).await;
    let mut sdb = Sdb::connect(fake.addr).await.unwrap();
    assert_eq!(sdb.shell("0 getduid").await.unwrap(), "2DCNLQ722RJQQ\n");
    // A second service on the same connection.
    assert!(sdb
        .shell("0 vd_applist")
        .await
        .unwrap()
        .contains("apps count: 0"));
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.services, ["shell:0 getduid", "shell:0 vd_applist"]);
}

#[tokio::test]
async fn push_sends_send_data_done() {
    let fake = FakeSdbd::start(outputs(), Mode::Normal).await;
    let mut sdb = Sdb::connect(fake.addr).await.unwrap();
    let data: Vec<u8> = (0..100 * 1024).map(|i| (i % 251) as u8).collect();
    sdb.push(&data, PUSH_PATH, 0o100644).await.unwrap();
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.pushed.len(), 1);
    assert_eq!(seen.pushed[0].0, format!("{PUSH_PATH},{}", 0o100644));
    assert_eq!(seen.pushed[0].1, data);
    // The connection still works afterwards.
    assert_eq!(
        sdb.shell("0 getduid").await.unwrap().trim(),
        "2DCNLQ722RJQQ"
    );
}

#[tokio::test]
async fn push_fail_reports_message() {
    let fake = FakeSdbd::start(outputs(), Mode::PushFails("No space left".into())).await;
    let mut sdb = Sdb::connect(fake.addr).await.unwrap();
    let err = sdb.push(b"x", "/tmp/x", 0o100644).await.unwrap_err();
    assert_eq!(err, SdbError::PushFailed("No space left".into()));
}

#[tokio::test]
async fn close_after_done_without_status_is_a_failure() {
    let fake = FakeSdbd::start(outputs(), Mode::CloseAfterDone).await;
    let mut sdb = Sdb::connect(fake.addr).await.unwrap();
    let err = sdb.push(b"x", PUSH_PATH, 0o100644).await.unwrap_err();
    assert!(matches!(err, SdbError::PushFailed(_)), "{err:?}");
}

#[tokio::test]
async fn closed_after_cnxn_is_refused() {
    let fake = FakeSdbd::start(outputs(), Mode::CloseAfterCnxn).await;
    let err = Sdb::connect(fake.addr).await.err().unwrap();
    assert_eq!(err, SdbError::Refused);
}

#[tokio::test]
async fn nothing_listening_is_down() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    assert!(matches!(
        Sdb::connect(addr).await.err().unwrap(),
        SdbError::Down(_)
    ));
}
