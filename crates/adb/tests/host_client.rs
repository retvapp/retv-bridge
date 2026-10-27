#[path = "support/fake_adb_server.rs"]
mod fake_adb_server;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fake_adb_server::{FakeAdbScript, FakeAdbServer, ShellReply};
use retv_bridge_adb::{map_adb_failure, AdbHost, PushError, RealAdb, Timeouts};
use retv_bridge_core::api::{BridgeAdb, ShellResult, TransportError};
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::validate::MdnsService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ADDR: &str = "192.0.2.10:5555";

fn connectable() -> FakeAdbScript {
    let mut script = FakeAdbScript {
        version: 41,
        ..Default::default()
    };
    script.connect.insert(ADDR.into(), Ok(ADDR.into()));
    script
}

async fn host_for(script: FakeAdbScript) -> (FakeAdbServer, AdbHost) {
    let server = FakeAdbServer::start(script).await;
    let host = AdbHost::new(server.port).with_timeouts(fast());
    (server, host)
}

fn fast() -> Timeouts {
    Timeouts {
        request: Duration::from_millis(2000),
        connect: Duration::from_millis(2000),
        ready: Duration::from_millis(600),
        idle: Duration::from_millis(300),
    }
}

type Log = Arc<Mutex<Vec<String>>>;

fn logged(host: AdbHost) -> (AdbHost, Log) {
    let log: Log = Arc::default();
    let sink = log.clone();
    (
        host.with_log(Arc::new(move |line: String| {
            sink.lock().unwrap().push(line)
        })),
        log,
    )
}

/// A server that answers every request with `reply` and then stays silent
/// with the socket open.
async fn canned_server(reply: &'static [u8]) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(reply).await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn version_reads_hex() {
    let (_server, host) = host_for(FakeAdbScript {
        version: 41,
        ..Default::default()
    })
    .await;
    assert_eq!(host.version().await, Ok(41));
}

#[tokio::test]
async fn connect_ok_returns_serial() {
    let (server, host) = host_for(connectable()).await;
    assert_eq!(host.connect(ADDR).await, Ok(ADDR.to_string()));
    let reqs = server.requests();
    assert_eq!(reqs[0], format!("host:connect:{ADDR}"));
    assert!(reqs.contains(&format!("host-serial:{ADDR}:get-state")));
}

#[tokio::test]
async fn connect_without_port_appends_default_port() {
    let mut script = connectable();
    script
        .connect
        .insert("192.0.2.10:5555".into(), Ok(ADDR.into()));
    let (server, host) = host_for(script).await;
    assert_eq!(host.connect("192.0.2.10").await, Ok(ADDR.to_string()));
    assert_eq!(server.requests()[0], format!("host:connect:{ADDR}"));
}

#[tokio::test]
async fn connect_refused_maps_device_unreachable() {
    let mut script = connectable();
    script.connect.insert(
        ADDR.into(),
        Err(format!("failed to connect to '{ADDR}': Connection refused")),
    );
    let (_server, host) = host_for(script).await;
    assert_eq!(
        host.connect(ADDR).await,
        Err(TransportError::DeviceUnreachable)
    );
}

#[tokio::test]
async fn unauthorized_maps() {
    let mut script = connectable();
    script.states.insert(ADDR.into(), "unauthorized".into());
    let (_server, host) = host_for(script).await;
    assert_eq!(host.connect(ADDR).await, Err(TransportError::Unauthorized));
    assert_eq!(
        map_adb_failure("device unauthorized.\nThis adb server's $ADB_VENDOR_KEYS is not set"),
        TransportError::Unauthorized
    );
}

#[tokio::test]
async fn connect_that_stays_offline_is_device_offline() {
    let mut script = connectable();
    script.states.insert(ADDR.into(), "offline".into());
    let (_server, host) = host_for(script).await;
    assert_eq!(host.connect(ADDR).await, Err(TransportError::DeviceOffline));
}

#[tokio::test]
async fn connect_that_never_appears_is_device_not_found() {
    let mut script = connectable();
    // "connected" but the server never lists it.
    script
        .connect
        .insert(ADDR.into(), Err(format!("connected to {ADDR}")));
    let (_server, host) = host_for(script).await;
    assert_eq!(
        host.connect(ADDR).await,
        Err(TransportError::DeviceNotFound)
    );
}

#[tokio::test]
async fn pair_wrong_code_maps_pairing_failed() {
    let mut script = connectable();
    script
        .pair
        .insert(("123456".into(), "192.0.2.10:37000".into()), Ok(()));
    let (server, host) = host_for(script).await;
    assert_eq!(host.pair("192.0.2.10:37000", "123456").await, Ok(()));
    assert_eq!(
        host.pair("192.0.2.10:37000", "000000").await,
        Err(TransportError::PairingFailed)
    );
    assert!(server
        .requests()
        .contains(&"host:pair:000000:192.0.2.10:37000".to_string()));
}

#[tokio::test]
async fn disconnect_sends_host_disconnect() {
    let (server, host) = host_for(connectable()).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(host.disconnect(ADDR).await, Ok(()));
    assert!(server
        .requests()
        .contains(&format!("host:disconnect:{ADDR}")));
}

#[tokio::test]
async fn shell_v2_splits_streams_and_exit_code() {
    let mut script = connectable();
    script.shell.insert(
        "ls /nope; echo hi".into(),
        ShellReply::new("hello world\n", "ls: /nope: No such file\n", 1),
    );
    let (server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    let res = host.shell(ADDR, "ls /nope; echo hi").await;
    assert_eq!(
        res,
        Ok(ShellResult {
            exit_code: 1,
            stdout: "hello world\n".into(),
            stderr: "ls: /nope: No such file\n".into(),
        })
    );
    let reqs = server.requests();
    assert!(reqs.contains(&format!("host:transport:{ADDR}")));
    assert!(reqs.contains(&"shell,v2,raw:ls /nope; echo hi".to_string()));
}

#[tokio::test]
async fn shell_on_unknown_serial_is_device_not_found() {
    let (_server, host) = host_for(connectable()).await;
    assert_eq!(
        host.shell("192.0.2.99:5555", "true").await,
        Err(TransportError::DeviceNotFound)
    );
}

#[tokio::test]
async fn push_chunks_over_64k_and_reports_fail() {
    let (server, host) = host_for(connectable()).await;
    host.connect(ADDR).await.unwrap();
    let bytes: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
    host.push(ADDR, "/sdcard/Download/a.apk", &bytes, 0o644)
        .await
        .unwrap();
    let pushed = server.pushed();
    assert_eq!(pushed.len(), 1);
    let file = &pushed[0];
    assert_eq!(file.path, "/sdcard/Download/a.apk");
    assert_eq!(file.mode & 0o777, 0o644);
    assert_eq!(file.chunks, vec![65536, 65536, 150_000 - 2 * 65536]);
    assert_eq!(file.data, bytes);
    assert!(file.mtime > 0);

    let mut script = connectable();
    script.sync_fail = Some("couldn't create file: Read-only file system".into());
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(
        host.push(ADDR, "/system/x", b"x", 0o644).await,
        Err(PushError::Failed(
            "couldn't create file: Read-only file system".into()
        ))
    );
}

#[tokio::test]
async fn server_down_is_adb_server_unreachable() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let host = AdbHost::new(port);
    assert_eq!(
        host.version().await,
        Err(TransportError::AdbServerUnreachable)
    );
    assert_eq!(
        host.connect(ADDR).await,
        Err(TransportError::AdbServerUnreachable)
    );
    assert_eq!(
        host.pair(ADDR, "123456").await,
        Err(TransportError::AdbServerUnreachable)
    );
    assert_eq!(
        host.shell(ADDR, "true").await,
        Err(TransportError::AdbServerUnreachable)
    );
}

#[tokio::test]
async fn eof_mid_shell_is_connection_lost() {
    let mut script = connectable();
    script.shell.insert(
        "logcat".into(),
        ShellReply {
            stdout: "partial".into(),
            ..Default::default()
        },
    );
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(
        host.shell(ADDR, "logcat").await,
        Err(TransportError::ConnectionLost)
    );
}

#[test]
fn failure_messages_map_like_the_engine() {
    use TransportError::*;
    let cases = [
        ("device unauthorized.", Unauthorized),
        ("device offline", DeviceOffline),
        (
            "failed to connect to '192.0.2.10:5555': Operation timed out",
            DeviceUnreachable,
        ),
        (
            "cannot connect to 192.0.2.10:5555: Connection refused",
            DeviceUnreachable,
        ),
        ("connect: No route to host", DeviceUnreachable),
        ("device '192.0.2.10:5555' not found", DeviceNotFound),
        ("something else entirely", Internal),
    ];
    for (msg, want) in cases {
        assert_eq!(map_adb_failure(msg), want, "{msg}");
    }
}

#[tokio::test]
async fn real_adb_connects_reads_model_and_writes_files() {
    let mut script = connectable();
    script.shell.insert(
        "getprop ro.product.model".into(),
        ShellReply::new("MiTV-AESP0\n", "", 0),
    );
    script
        .shell
        .insert("echo ok".into(), ShellReply::new("ok\n", "", 0));
    let (server, host) = host_for(script).await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let adb = RealAdb::new(
        host,
        Arc::new(|| {
            Box::pin(async {
                vec![
                    MdnsService {
                        instance: "adb-TEST-abc".into(),
                        service: "_adb-tls-connect._tcp".into(),
                        ip: "10.0.0.20".into(),
                        port: 5555,
                    },
                    MdnsService {
                        instance: "adb-TEST-abc".into(),
                        service: "_adb-tls-pairing._tcp".into(),
                        ip: "10.0.0.20".into(),
                        port: 37000,
                    },
                    // Not a home-network address: group_services drops it.
                    MdnsService {
                        instance: "adb-OTHER-def".into(),
                        service: "_adb-tls-connect._tcp".into(),
                        ip: "192.0.2.10".into(),
                        port: 5555,
                    },
                ]
            })
        }),
    )
    .with_events(Arc::new(move |e| sink.lock().unwrap().push(e)));

    assert!(adb.available().await);
    let tvs = adb.discover().await;
    assert_eq!(tvs.len(), 1, "grouped into one TV: {tvs:?}");
    assert_eq!(tvs[0].ip, "10.0.0.20");
    assert_eq!(tvs[0].connect_port, Some(5555));
    assert_eq!(tvs[0].pair_port, Some(37000));
    let dev = adb.connect(ADDR).await.unwrap();
    assert_eq!(dev.serial(), ADDR);
    // The model comes with the connection; TvConnected is the server's to send.
    assert_eq!(dev.model(), Some("MiTV-AESP0".to_string()));
    let not_logs: Vec<BridgeEvent> = events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| !matches!(e, BridgeEvent::Log(_)))
        .cloned()
        .collect();
    assert_eq!(not_logs, vec![]);
    assert_eq!(dev.run("echo ok").await.unwrap().stdout, "ok\n");
    dev.write_file("/sdcard/retv/a.txt", "héllo").await.unwrap();
    let pushed = server.pushed();
    assert_eq!(pushed[0].path, "/sdcard/retv/a.txt");
    assert_eq!(pushed[0].data, "héllo".as_bytes());
    dev.close().await;
    assert_eq!(
        dev.run("echo ok").await,
        Err(TransportError::ConnectionLost)
    );
}

#[tokio::test]
async fn real_adb_model_is_empty_when_getprop_fails() {
    let (_server, host) = host_for(connectable()).await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let adb = RealAdb::new(host, Arc::new(|| Box::pin(async { Vec::new() })))
        .with_events(Arc::new(move |e| sink.lock().unwrap().push(e)));
    let dev = adb.connect(ADDR).await.unwrap();
    assert_eq!(dev.serial(), ADDR);
    assert_eq!(dev.model(), Some(String::new()));
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, BridgeEvent::TvConnected { .. })));
}

#[tokio::test]
async fn connect_waits_through_transient_get_state_fails() {
    let mut script = connectable();
    script.state_fails.insert(
        ADDR.into(),
        vec![
            "device still connecting".into(),
            "device still authorizing".into(),
            "device still connecting".into(),
        ],
    );
    let (server, host) = host_for(script).await;
    assert_eq!(host.connect(ADDR).await, Ok(ADDR.to_string()));
    let polls = server
        .requests()
        .iter()
        .filter(|r| r.ends_with(":get-state"))
        .count();
    assert_eq!(polls, 4);
}

#[tokio::test]
async fn connect_still_authorizing_at_deadline_is_device_not_found() {
    let mut script = connectable();
    script
        .state_fails
        .insert(ADDR.into(), vec!["device still authorizing".into(); 50]);
    let (_server, host) = host_for(script).await;
    assert_eq!(
        host.connect(ADDR).await,
        Err(TransportError::DeviceNotFound)
    );
}

#[tokio::test]
async fn silent_server_times_out() {
    let port = canned_server(b"").await;
    let host = AdbHost::new(port).with_timeouts(Timeouts {
        request: Duration::from_millis(300),
        connect: Duration::from_millis(300),
        ..fast()
    });
    assert_eq!(
        host.version().await,
        Err(TransportError::AdbServerUnreachable)
    );
    assert_eq!(
        host.connect(ADDR).await,
        Err(TransportError::ConnectionLost)
    );
    assert_eq!(
        host.shell(ADDR, "true").await,
        Err(TransportError::ConnectionLost)
    );
    assert_eq!(
        host.push(ADDR, "/sdcard/a", b"x", 0o644).await,
        Err(PushError::Transport(TransportError::ConnectionLost))
    );
}

#[tokio::test]
async fn unparsable_version_is_internal() {
    let port = canned_server(b"OKAY0004zzzz").await;
    let (host, log) = logged(AdbHost::new(port).with_timeouts(fast()));
    assert_eq!(host.version().await, Err(TransportError::Internal));
    assert!(log.lock().unwrap().iter().any(|l| l.contains("zzzz")));
}

#[tokio::test]
async fn unrecognised_fail_text_is_internal_and_logged() {
    let port = canned_server(b"FAIL0013weird adb complaint").await;
    let (host, log) = logged(AdbHost::new(port).with_timeouts(fast()));
    assert_eq!(
        host.shell(ADDR, "true").await,
        Err(TransportError::Internal)
    );
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("weird adb complaint")));
}

#[tokio::test]
async fn over_long_request_is_internal() {
    let (_server, host) = host_for(connectable()).await;
    host.connect(ADDR).await.unwrap();
    let cmd = "x".repeat(70_000);
    assert_eq!(host.shell(ADDR, &cmd).await, Err(TransportError::Internal));
}

#[tokio::test]
async fn silent_shell_hits_inactivity_timeout() {
    let mut script = connectable();
    script.shell.insert(
        "sleep 999".into(),
        ShellReply {
            stdout: "started\n".into(),
            hang: true,
            ..Default::default()
        },
    );
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(
        host.shell(ADDR, "sleep 999").await,
        Err(TransportError::ConnectionLost)
    );
}

#[tokio::test]
async fn shell_reassembles_multi_packet_output_delivered_byte_by_byte() {
    let big: String = (0..100_000u32)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let mut script = connectable();
    script.shell.insert(
        "cat big".into(),
        ShellReply {
            stdout: big.clone(),
            stderr: "warn\n".into(),
            exit: Some(0),
            trickle: true,
            ..Default::default()
        },
    );
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    let res = host.shell(ADDR, "cat big").await.unwrap();
    assert_eq!(res.exit_code, 0);
    assert_eq!(res.stderr, "warn\n");
    assert_eq!(res.stdout, big);
}

fn v2_packet(id: u8, len: u32, data: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[tokio::test]
async fn oversized_shell_packet_is_internal() {
    let mut script = connectable();
    script.shell.insert(
        "huge".into(),
        ShellReply {
            raw: Some(v2_packet(1, 2 * 1024 * 1024, b"")),
            ..Default::default()
        },
    );
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(
        host.shell(ADDR, "huge").await,
        Err(TransportError::Internal)
    );
}

#[tokio::test]
async fn shell_output_over_cap_is_internal() {
    let chunk = vec![b'x'; 1024 * 1024];
    let mut raw = Vec::new();
    for _ in 0..17 {
        raw.extend(v2_packet(1, chunk.len() as u32, &chunk));
    }
    raw.extend(v2_packet(3, 1, &[0]));
    let mut script = connectable();
    script.shell.insert(
        "flood".into(),
        ShellReply {
            raw: Some(raw),
            ..Default::default()
        },
    );
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(
        host.shell(ADDR, "flood").await,
        Err(TransportError::Internal)
    );
}

#[tokio::test]
async fn oversized_sync_fail_message_is_internal() {
    let mut script = connectable();
    script.sync_fail = Some("e".repeat(70 * 1024));
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    assert_eq!(
        host.push(ADDR, "/sdcard/a", b"x", 0o644).await,
        Err(PushError::Transport(TransportError::Internal))
    );
}

#[tokio::test]
async fn early_sync_fail_during_large_push_is_reported() {
    let mut script = connectable();
    script.sync_fail_early = Some("couldn't create file: Permission denied".into());
    let (_server, host) = host_for(script).await;
    host.connect(ADDR).await.unwrap();
    let bytes = vec![0u8; 32 * 1024 * 1024];
    assert_eq!(
        host.push(ADDR, "/data/x", &bytes, 0o644).await,
        Err(PushError::Failed(
            "couldn't create file: Permission denied".into()
        ))
    );
}

#[tokio::test]
async fn device_refusing_a_file_is_internal_and_logged() {
    let mut script = connectable();
    script.sync_fail = Some("couldn't create file: Read-only file system".into());
    let (_server, host) = host_for(script).await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let adb = RealAdb::new(host, Arc::new(|| Box::pin(async { Vec::new() })))
        .with_events(Arc::new(move |e| sink.lock().unwrap().push(e)));
    let dev = adb.connect(ADDR).await.unwrap();
    assert_eq!(
        dev.write_file("/system/x", "x").await,
        Err(TransportError::Internal)
    );
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, BridgeEvent::Log(l) if l.contains("Read-only file system"))));
}

#[tokio::test]
async fn device_without_shell_v2_is_logged() {
    let mut script = connectable();
    script.features = Some("cmd,stat_v2".into());
    let (_server, host) = host_for(script).await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let adb = RealAdb::new(host, Arc::new(|| Box::pin(async { Vec::new() })))
        .with_events(Arc::new(move |e| sink.lock().unwrap().push(e)));
    let _ = adb.connect(ADDR).await;
    assert!(events.lock().unwrap().iter().any(
        |e| matches!(e, BridgeEvent::Log(l) if l.contains("shell protocol v2") && l.contains(ADDR))
    ));
}

/// Manual: `ANDROID_ADB_SERVER_PORT=5039 adb start-server`, then
/// `ANDROID_ADB_SERVER_PORT=5039 cargo test -p retv-bridge-adb -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_adb_version() {
    let port: u16 = std::env::var("ANDROID_ADB_SERVER_PORT")
        .expect("set ANDROID_ADB_SERVER_PORT")
        .parse()
        .unwrap();
    let version = AdbHost::new(port).version().await.unwrap();
    assert!(version >= 39, "adb server protocol version {version}");
}
