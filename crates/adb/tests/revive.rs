//! Task 21a: the bridge's private adb server is started again when it has
//! died (`adb-server-unreachable`), at most once per gap, never once
//! stopped, and the failed operation is retried once.

#[path = "support/fake_adb_bin.rs"]
mod fake_adb_bin;
#[path = "support/fake_adb_server.rs"]
mod fake_adb_server;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fake_adb_bin::{calls, count, fake_adb, test_port, wait_port_free};
use fake_adb_server::{FakeAdbScript, FakeAdbServer, ShellReply};
use retv_bridge_adb::server_proc::{restart_adb_server, start_adb_server};
use retv_bridge_adb::{AdbHost, BoxFuture, Discoverer, RealAdb, Restarter, ServerRevive};
use retv_bridge_core::api::{BridgeAdb, TransportError};

fn no_discovery() -> Discoverer {
    Arc::new(|| -> BoxFuture<'static, _> { Box::pin(async { Vec::new() }) })
}

/// Restarts the fake adb's server on `port` the way the app does.
fn restarter(adb: &Path, port: u16) -> Restarter {
    let adb: PathBuf = adb.to_path_buf();
    Arc::new(move || -> BoxFuture<'static, bool> {
        let adb = adb.clone();
        Box::pin(async move { start_adb_server(&adb, port).await.is_ok() })
    })
}

/// `adb -P <port> kill-server` from outside, as another program would.
fn kill(adb: &Path, port: u16) {
    let status = std::process::Command::new(adb)
        .args(["-P", &port.to_string(), "kill-server"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(wait_port_free(port));
}

#[tokio::test]
async fn a_dead_server_is_started_again_and_the_call_retried() {
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let first = start_adb_server(&adb, port).await.unwrap();
    let revive = Arc::new(ServerRevive::new(restarter(&adb, port)));
    let bridge = RealAdb::new(AdbHost::new(port), no_discovery()).with_revive(revive.clone());
    assert!(bridge.available().await, "{:?}", calls(&log));

    kill(&adb, port);
    assert!(bridge.available().await, "restarted and asked again");
    assert_eq!(count(&calls(&log), "start-server"), 2);

    // Within the gap: no second restart; the existing "adb is down" answer.
    kill(&adb, port);
    assert!(!bridge.available().await);
    assert_eq!(
        bridge.connect("192.0.2.10:5555").await.err(),
        Some(TransportError::AdbServerUnreachable)
    );
    assert_eq!(count(&calls(&log), "start-server"), 2);
    drop(first);
}

#[tokio::test]
async fn after_the_gap_it_may_restart_again() {
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let proc = start_adb_server(&adb, port).await.unwrap();
    let revive =
        Arc::new(ServerRevive::new(restarter(&adb, port)).with_gap(Duration::from_millis(400)));
    let bridge = RealAdb::new(AdbHost::new(port), no_discovery()).with_revive(revive);

    kill(&adb, port);
    assert!(bridge.available().await, "{:?}", calls(&log));
    tokio::time::sleep(Duration::from_millis(500)).await;
    kill(&adb, port);
    assert!(bridge.available().await, "{:?}", calls(&log));
    assert_eq!(count(&calls(&log), "start-server"), 3);
    proc.stop().await.unwrap();
}

#[tokio::test]
async fn never_restarts_once_stopped() {
    // `stop` comes first when the app quits or installs an update.
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let _proc = start_adb_server(&adb, port).await.unwrap();
    let revive = Arc::new(ServerRevive::new(restarter(&adb, port)));
    let bridge = RealAdb::new(AdbHost::new(port), no_discovery()).with_revive(revive.clone());

    revive.stop();
    kill(&adb, port);
    assert!(!bridge.available().await);
    assert_eq!(
        bridge.pair("192.0.2.10:37000", "123456").await,
        Err(TransportError::AdbServerUnreachable)
    );
    assert_eq!(count(&calls(&log), "start-server"), 1);
}

#[tokio::test]
async fn pair_is_retried_on_the_new_server() {
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let proc = start_adb_server(&adb, port).await.unwrap();
    let revive = Arc::new(ServerRevive::new(restarter(&adb, port)));
    let bridge = RealAdb::new(AdbHost::new(port), no_discovery()).with_revive(revive);

    kill(&adb, port);
    // The fake server refuses `host:pair`, so the retry answers pairing-failed.
    assert_eq!(
        bridge.pair("192.0.2.10:37000", "123456").await,
        Err(TransportError::PairingFailed)
    );
    assert_eq!(count(&calls(&log), "start-server"), 2);
    proc.stop().await.unwrap();
}

const ADDR: &str = "192.0.2.10:5555";

fn script() -> FakeAdbScript {
    let mut script = FakeAdbScript {
        version: 41,
        ..Default::default()
    };
    // A server knows the TV only after `host:connect`: a restarted one has
    // forgotten it, as adb with ADB_MDNS=0 does.
    script.connect.insert(ADDR.into(), Ok(ADDR.into()));
    script
        .shell
        .insert("echo hi".into(), ShellReply::new("hi\n", "", 0));
    script
}

#[tokio::test]
async fn device_commands_are_retried_after_a_restart() {
    let port = test_port();
    let server = FakeAdbServer::start_on(port, script()).await;
    let restarts = Arc::new(AtomicUsize::new(0));
    let replacement: Arc<tokio::sync::Mutex<Option<FakeAdbServer>>> = Arc::default();
    let restart: Restarter = {
        let (restarts, replacement) = (restarts.clone(), replacement.clone());
        Arc::new(move || -> BoxFuture<'static, bool> {
            let (restarts, replacement) = (restarts.clone(), replacement.clone());
            Box::pin(async move {
                restarts.fetch_add(1, Ordering::SeqCst);
                *replacement.lock().await = Some(FakeAdbServer::start_on(port, script()).await);
                true
            })
        })
    };
    let bridge = RealAdb::new(AdbHost::new(port), no_discovery())
        .with_revive(Arc::new(ServerRevive::new(restart)));
    let tv = bridge.connect(ADDR).await.unwrap();

    server.stop().await;
    // Reconnected to the new server, then retried.
    let r = tv.run("echo hi").await.unwrap();
    assert_eq!(r.stdout, "hi\n");
    assert_eq!(restarts.load(Ordering::SeqCst), 1);

    replacement.lock().await.as_ref().unwrap().stop().await;
    assert_eq!(
        tv.write_bytes("/sdcard/x", b"x").await,
        Err(TransportError::AdbServerUnreachable),
        "one restart per gap"
    );
    assert_eq!(restarts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_restart_that_fails_reports_adb_down() {
    let port = test_port();
    let restarts = Arc::new(AtomicUsize::new(0));
    let restart: Restarter = {
        let restarts = restarts.clone();
        Arc::new(move || -> BoxFuture<'static, bool> {
            restarts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { false })
        })
    };
    let bridge = RealAdb::new(AdbHost::new(port), no_discovery())
        .with_revive(Arc::new(ServerRevive::new(restart)));
    assert_eq!(
        bridge.connect(ADDR).await.err(),
        Some(TransportError::AdbServerUnreachable)
    );
    assert!(!bridge.available().await);
    assert_eq!(restarts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_busy_server_is_left_alone() {
    // Something holds the port and accepts, but does not answer in time: a
    // busy adb is not a dead one, so no kill-server and no start.
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let busy = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    assert!(matches!(restart_adb_server(&adb, port).await, Ok(None)));
    assert_eq!(count(&calls(&log), "kill-server"), 0);
    assert_eq!(count(&calls(&log), "start-server"), 0);
    drop(busy);
    // The probe's connection to the busy listener is torn down with it.
    assert!(wait_port_free(port));

    // Gone (nothing on the port): started again.
    let proc = restart_adb_server(&adb, port)
        .await
        .unwrap()
        .expect("started");
    assert_eq!(count(&calls(&log), "start-server"), 1);
    proc.stop().await.unwrap();
}
