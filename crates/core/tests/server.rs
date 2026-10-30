//! Server behaviour the wire fixtures cannot pin: the per-serial install turn
//! across sockets, aborting an install when its socket closes, the shell-facing
//! API (`events`, `active_sessions`, `live_code`, `shutdown`), and logs that
//! never carry the token or a code.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use retv_bridge_core::api::{
    ApkFetcher, BridgeAdb, BridgeDevice, Clock, CodeSource, DiscoveredTv, FetchError, ShellResult,
    TransportError,
};
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::server::{start_server, BridgeServer, ServerConfig};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, Semaphore};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const TOKEN: &str = "test-token-123"; // leakcheck:allow
const SERIAL: &str = "192.168.1.5:5555";
const APK: &[u8] = b"apk!";
const STAGING: &str = "/data/local/tmp/retv-install.apk";
const WAIT: Duration = Duration::from_secs(3);

type Ws = WebSocketStream<TcpStream>;
type Ops = Arc<Mutex<Vec<String>>>;

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

struct FixedClock;
impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        1_000_000
    }
}

struct Codes(Mutex<VecDeque<String>>);
impl CodeSource for Codes {
    fn next_code(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| "000000".into())
    }
}

/// Every connect gives a new handle to the same TV; `pm install` waits for a
/// permit from `gate` (none: it never finishes).
struct GatedAdb {
    ops: Ops,
    gate: Arc<Semaphore>,
    panic_pm: bool,
}

#[async_trait]
impl BridgeAdb for GatedAdb {
    async fn available(&self) -> bool {
        true
    }
    async fn connect(&self, address: &str) -> Result<Arc<dyn BridgeDevice>, TransportError> {
        Ok(Arc::new(GatedDevice {
            serial: address.into(),
            ops: self.ops.clone(),
            gate: self.gate.clone(),
            panic_pm: self.panic_pm,
        }))
    }
    async fn pair(&self, _: &str, _: &str) -> Result<(), TransportError> {
        Ok(())
    }
    async fn discover(&self) -> Vec<DiscoveredTv> {
        Vec::new()
    }
}

struct GatedDevice {
    serial: String,
    ops: Ops,
    gate: Arc<Semaphore>,
    panic_pm: bool,
}

#[async_trait]
impl BridgeDevice for GatedDevice {
    fn serial(&self) -> &str {
        &self.serial
    }
    fn model(&self) -> Option<String> {
        Some("TV".into())
    }
    async fn run(&self, cmd: &str) -> Result<ShellResult, TransportError> {
        self.ops.lock().unwrap().push(format!("run {cmd}"));
        let mut stdout = String::new();
        if cmd == "dumpsys package" {
            // A slow read: waits for the gate like an install does.
            self.gate.acquire().await.unwrap().forget();
        } else if cmd.starts_with("pm install") {
            assert!(!self.panic_pm, "pm install blew up");
            self.gate.acquire().await.unwrap().forget();
            self.ops.lock().unwrap().push("pm done".into());
            stdout = "Success".into();
        }
        Ok(ShellResult {
            exit_code: 0,
            stdout,
            stderr: String::new(),
        })
    }
    async fn write_file(&self, p: &str, c: &str) -> Result<(), TransportError> {
        self.write_bytes(p, c.as_bytes()).await
    }
    async fn write_bytes(&self, path: &str, _: &[u8]) -> Result<(), TransportError> {
        self.ops.lock().unwrap().push(format!("push {path}"));
        Ok(())
    }
    async fn close(&self) {
        self.ops.lock().unwrap().push("close".into());
    }
}

/// Serves `APK`, or (with `hang`) never answers and records being dropped.
struct Fetcher {
    hang: bool,
    dropped: Arc<AtomicBool>,
}

struct OnDrop(Arc<AtomicBool>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ApkFetcher for Fetcher {
    async fn fetch(&self, _: &str, _: u64) -> Result<Vec<u8>, FetchError> {
        if self.hang {
            let _guard = OnDrop(self.dropped.clone());
            std::future::pending::<()>().await;
        }
        Ok(APK.to_vec())
    }
}

struct Setup {
    server: BridgeServer,
    ops: Ops,
    gate: Arc<Semaphore>,
    fetch_dropped: Arc<AtomicBool>,
}

async fn setup(hang_fetch: bool) -> Setup {
    setup_with(hang_fetch, false).await
}

async fn setup_with(hang_fetch: bool, panic_pm: bool) -> Setup {
    let ops: Ops = Arc::default();
    let gate = Arc::new(Semaphore::new(0));
    let fetch_dropped = Arc::new(AtomicBool::new(false));
    let server = start_server(ServerConfig {
        token: TOKEN.into(), // leakcheck:allow
        port: 0,
        adb: Arc::new(GatedAdb {
            ops: ops.clone(),
            gate: gate.clone(),
            panic_pm,
        }),
        fetcher: Arc::new(Fetcher {
            hang: hang_fetch,
            dropped: fetch_dropped.clone(),
        }),
        clock: Arc::new(FixedClock),
        codes: Arc::new(Codes(Mutex::new(VecDeque::from(["482917".to_string()])))),
        version: "test".into(),
    })
    .await
    .unwrap();
    Setup {
        server,
        ops,
        gate,
        fetch_dropped,
    }
}

async fn open(port: u16) -> Ws {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut req = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://retv.app"));
    tokio_tungstenite::client_async(req, tcp).await.unwrap().0
}

/// The request with the session token added, unless it carries one.
fn authed(mut v: Value) -> Value {
    if let Some(m) = v.as_object_mut() {
        m.entry("token").or_insert(Value::from(TOKEN));
    }
    v
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(authed(v).to_string())).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("reply timed out")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(Message::Close(f))) => panic!("closed: {f:?}"),
            Some(Ok(_)) => continue,
            other => panic!("socket ended: {other:?}"),
        }
    }
}

async fn connect(ws: &mut Ws, id: u64) {
    send(
        ws,
        json!({"id": id, "type": "connect", "ip": "192.168.1.5", "port": 5555}),
    )
    .await;
    assert_eq!(recv(ws).await["ok"], json!(true));
}

fn install(id: u64) -> Value {
    json!({"id": id, "type": "installApk", "serial": SERIAL,
           "url": "https://retv.app/downloads/x.apk", "sha256": sha256_hex(APK)})
}

async fn until(what: &str, ok: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn ops(s: &Setup) -> Vec<String> {
    s.ops.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installs_to_one_tv_take_turns_across_sockets() {
    let s = setup(false).await;
    let (mut a, mut b) = (open(s.server.port()).await, open(s.server.port()).await);
    connect(&mut a, 1).await;
    connect(&mut b, 1).await;

    send(&mut a, install(2)).await;
    until("a's pm install", || {
        ops(&s).contains(
            &"run pm install -r --user current /data/local/tmp/retv-install.apk".to_string(),
        )
    })
    .await;
    send(&mut b, install(2)).await;
    // b downloads, then waits for the TV's turn: it must not stage over a's file.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        ops(&s).iter().filter(|o| o.starts_with("push")).count(),
        1,
        "{:?}",
        ops(&s)
    );

    s.gate.add_permits(2);
    assert_eq!(recv(&mut a).await["result"], json!({"output": "Success"}));
    assert_eq!(recv(&mut b).await["result"], json!({"output": "Success"}));
    let pm = "run pm install -r --user current /data/local/tmp/retv-install.apk".to_string();
    let rm = format!("run rm -f {STAGING}");
    let push = format!("push {STAGING}");
    let want = vec![
        push.clone(),
        pm.clone(),
        "pm done".into(),
        rm.clone(),
        push,
        pm,
        "pm done".into(),
        rm,
    ];
    assert_eq!(ops(&s), want);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_install_on_one_socket_is_busy() {
    let s = setup(false).await;
    let mut a = open(s.server.port()).await;
    connect(&mut a, 1).await;
    send(&mut a, install(2)).await;
    until("pm install", || {
        ops(&s).iter().any(|o| o.starts_with("run pm"))
    })
    .await;
    send(&mut a, install(3)).await;
    assert_eq!(
        recv(&mut a).await,
        json!({"id": 3, "ok": false, "error": "busy"})
    );
    s.gate.add_permits(1);
    assert_eq!(recv(&mut a).await["id"], json!(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_socket_aborts_a_download() {
    let s = setup(true).await;
    let mut a = open(s.server.port()).await;
    connect(&mut a, 1).await;
    send(&mut a, install(2)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    a.close(None).await.unwrap();
    until("the download to stop", || {
        s.fetch_dropped.load(Ordering::SeqCst)
    })
    .await;
    until("the TV to close", || ops(&s).contains(&"close".to_string())).await;
    // Nothing reached the TV.
    assert_eq!(ops(&s), vec!["close".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_socket_aborts_an_install_and_removes_the_staged_file() {
    let s = setup(false).await;
    let mut a = open(s.server.port()).await;
    connect(&mut a, 1).await;
    send(&mut a, install(2)).await;
    until("pm install", || {
        ops(&s).iter().any(|o| o.starts_with("run pm"))
    })
    .await;
    a.close(None).await.unwrap();
    until("the TV to close", || ops(&s).contains(&"close".to_string())).await;
    // pm never finished; the staged file was removed before the TV closed.
    let o = ops(&s);
    assert_eq!(
        &o[o.len() - 2..],
        &[format!("run rm -f {STAGING}"), "close".into()],
        "{o:?}"
    );
    assert!(!o.contains(&"pm done".to_string()));

    // The TV's turn is free again for another socket.
    let mut b = open(s.server.port()).await;
    connect(&mut b, 1).await;
    s.gate.add_permits(1);
    send(&mut b, install(2)).await;
    assert_eq!(recv(&mut b).await["result"], json!({"output": "Success"}));
}

fn drain(rx: &mut broadcast::Receiver<BridgeEvent>) -> Vec<BridgeEvent> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_api_events_sessions_codes_and_shutdown() {
    let s = setup(false).await;
    let mut events = s.server.events();
    let mut a = open(s.server.port()).await;
    assert_eq!(s.server.active_sessions(), 0);
    assert_eq!(s.server.live_code(), None);

    send(&mut a, json!({"id": 1, "type": "pairStart"})).await;
    assert_eq!(recv(&mut a).await["result"], json!({"expiresIn": 120}));
    assert_eq!(
        s.server.live_code(),
        Some(("482917".to_string(), 1_120_000))
    );
    send(
        &mut a,
        json!({"id": 2, "type": "pairFinish", "code": "482917"}),
    )
    .await;
    assert_eq!(recv(&mut a).await["result"]["token"], json!(TOKEN));
    assert_eq!(s.server.live_code(), None);

    assert!(s.server.connected_tvs().is_empty());
    connect(&mut a, 3).await;
    assert_eq!(s.server.active_sessions(), 1);
    // The app rebuilds its tray from this when it missed events.
    assert_eq!(
        s.server.connected_tvs(),
        std::collections::BTreeMap::from([(SERIAL.to_string(), Some("TV".to_string()))])
    );
    send(
        &mut a,
        json!({"id": 4, "type": "run", "serial": SERIAL, "cmd": "getprop sys.boot_completed"}),
    )
    .await;
    recv(&mut a).await;
    let _idle = open(s.server.port()).await;
    assert_eq!(
        s.server.active_sessions(),
        1,
        "a socket without TVs is not active"
    );

    let port = s.server.port();
    s.server.shutdown().await;
    match tokio::time::timeout(WAIT, a.next()).await.unwrap() {
        Some(Ok(Message::Close(Some(f)))) => assert_eq!(u16::from(f.code), 1001),
        other => panic!("expected a 1001 close, got {other:?}"),
    }
    assert!(s.ops.lock().unwrap().contains(&"close".to_string()));
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "still listening"
    );

    let evs = drain(&mut events);
    assert!(evs.contains(&BridgeEvent::PairCode {
        code: "482917".into(),
        expires_at_ms: 1_120_000
    }));
    assert!(evs.contains(&BridgeEvent::TvConnected {
        serial: SERIAL.into(),
        model: "TV".into()
    }));
    assert!(evs.contains(&BridgeEvent::TvDisconnected {
        serial: SERIAL.into()
    }));
    let logs: Vec<&String> = evs
        .iter()
        .filter_map(|e| match e {
            BridgeEvent::Log(l) => Some(l),
            _ => None,
        })
        .collect();
    assert!(
        logs.iter()
            .any(|l| l.contains("run 192.168.1.5:5555: getprop sys.boot_completed")),
        "{logs:?}"
    );
    for l in &logs {
        assert!(
            !l.contains(TOKEN) && !l.contains("482917"),
            "secret in log: {l}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_token_is_logged_without_the_token() {
    let s = setup(false).await;
    let mut events = s.server.events();
    let mut a = open(s.server.port()).await;
    send(
        &mut a,
        json!({"id": 1, "token": "guess-123", "type": "run", "serial": SERIAL, "cmd": "x"}), // leakcheck:allow
    )
    .await;
    assert_eq!(recv(&mut a).await["error"], json!("bad-token"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    for e in drain(&mut events) {
        if let BridgeEvent::Log(l) = e {
            assert!(!l.contains("guess-123") && !l.contains(TOKEN), "{l}");
        }
    }
}

async fn send_all(ws: &mut Ws, msgs: Vec<Value>) {
    for m in msgs {
        ws.feed(Message::text(authed(m).to_string())).await.unwrap();
    }
    ws.flush().await.unwrap();
}

/// `n` replies, by id.
async fn replies(ws: &mut Ws, n: usize) -> std::collections::BTreeMap<u64, Value> {
    let mut out = std::collections::BTreeMap::new();
    for _ in 0..n {
        let r = recv(ws).await;
        out.insert(r["id"].as_u64().unwrap(), r);
    }
    out
}

fn chunk(id: u64, offset: u64, data: &str) -> Value {
    json!({"id": id, "type": "writeBytes", "serial": SERIAL,
           "path": "/sdcard/ReTV/a.bin", "offset": offset, "total": 12, "data": data})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_requests_keep_arrival_order() {
    let s = setup(true).await;
    for round in 0..20 {
        let mut a = open(s.server.port()).await;
        connect(&mut a, 1).await;
        // Chunks sent back to back, without waiting for replies.
        send_all(
            &mut a,
            vec![
                chunk(2, 0, "YWJjZA=="),
                chunk(3, 4, "ZWZnaA=="),
                chunk(4, 8, "aWprbA=="),
            ],
        )
        .await;
        let r = replies(&mut a, 3).await;
        assert_eq!(
            r[&2]["result"],
            json!({"received": 4, "done": false}),
            "round {round}: {r:?}"
        );
        assert_eq!(
            r[&3]["result"],
            json!({"received": 8, "done": false}),
            "round {round}: {r:?}"
        );
        assert_eq!(
            r[&4]["result"],
            json!({"received": 12, "done": true}),
            "round {round}: {r:?}"
        );

        // Two installs back to back: the second is busy, always.
        send_all(&mut a, vec![install(5), install(6)]).await;
        assert_eq!(
            recv(&mut a).await,
            json!({"id": 6, "ok": false, "error": "busy"}),
            "round {round}"
        );

        // disconnect, then run at once: the run no longer sees the serial.
        send_all(
            &mut a,
            vec![
                json!({"id": 7, "type": "disconnect", "serial": SERIAL}),
                json!({"id": 8, "type": "run", "serial": SERIAL, "cmd": "getprop"}),
            ],
        )
        .await;
        let r = replies(&mut a, 2).await;
        assert_eq!(r[&7]["ok"], json!(true), "round {round}: {r:?}");
        assert_eq!(
            r[&8],
            json!({"id": 8, "ok": false, "error": "unknown-serial"}),
            "round {round}"
        );
        a.close(None).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_runs_at_most_32_requests_at_once() {
    let s = setup(false).await;
    let mut a = open(s.server.port()).await;
    connect(&mut a, 1).await;
    // These never finish until the gate opens.
    let blocked: Vec<Value> = (100..132)
        .map(|id| json!({"id": id, "type": "run", "serial": SERIAL, "cmd": "dumpsys package"}))
        .collect();
    send_all(&mut a, blocked).await;
    until("32 commands running", || ops(&s).len() == 32).await;
    send(&mut a, json!({"id": 200, "type": "hello"})).await;
    assert_eq!(
        recv(&mut a).await,
        json!({"id": 200, "ok": false, "error": "too-many-requests"})
    );
    send(
        &mut a,
        json!({"id": 201, "type": "run", "serial": SERIAL, "cmd": "getprop"}),
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        json!({"id": 201, "ok": false, "error": "too-many-requests"})
    );
    s.gate.add_permits(32);
    let r = replies(&mut a, 32).await;
    assert!(r.values().all(|v| v["ok"] == json!(true)), "{r:?}");
    send(
        &mut a,
        json!({"id": 202, "type": "run", "serial": SERIAL, "cmd": "getprop"}),
    )
    .await;
    assert_eq!(recv(&mut a).await["ok"], json!(true));
}

async fn try_open(port: u16) -> Result<Ws, u16> {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut req = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://retv.app"));
    match tokio_tungstenite::client_async(req, tcp).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("upgrade failed: {e}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_most_16_sockets_at_once() {
    let s = setup(false).await;
    let port = s.server.port();
    let mut sockets = Vec::new();
    for _ in 0..16 {
        sockets.push(open(port).await);
    }
    assert_eq!(try_open(port).await.err(), Some(503));
    let mut first = sockets.remove(0);
    first.close(None).await.unwrap();
    while let Some(Ok(_)) = first.next().await {}
    let deadline = tokio::time::Instant::now() + WAIT;
    while try_open(port).await.is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the freed slot never reopened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn raw_status(port: u16, request: &[u8]) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let _ = s.write_all(request).await;
    let mut buf = vec![0u8; 256];
    let n = tokio::time::timeout(WAIT, s.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    text.split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status in {text:?}"))
}

#[tokio::test]
async fn oversized_heads_and_non_get_upgrades_are_refused() {
    let s = setup(false).await;
    let port = s.server.port();
    let big = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(17 * 1024)
    );
    assert_eq!(raw_status(port, big.as_bytes()).await, 431);
    let post = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://retv.app\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    assert_eq!(raw_status(port, post.as_bytes()).await, 400);
    let plain = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    assert_eq!(raw_status(port, plain.as_bytes()).await, 426);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_during_the_install_still_removes_the_staged_file() {
    let s = setup_with(false, true).await;
    let mut a = open(s.server.port()).await;
    connect(&mut a, 1).await;
    send(&mut a, install(2)).await;
    assert_eq!(
        recv(&mut a).await,
        json!({"id": 2, "ok": false, "error": "internal"})
    );
    until("the cleanup", || {
        ops(&s).contains(&format!("run rm -f {STAGING}"))
    })
    .await;
    // The socket's install slot is free again.
    send(&mut a, install(3)).await;
    assert_eq!(recv(&mut a).await["error"], json!("internal"));
}
