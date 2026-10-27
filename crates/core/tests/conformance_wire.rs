//! Replays `conformance/wire/*.json` against the Rust bridge over a real
//! WebSocket, with the fake adb and downloader each fixture describes. The
//! format is in `conformance/README.md`.

mod support;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use retv_bridge_core::api::{Clock, CodeSource};
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::pairing::OsCodeSource;
use retv_bridge_core::server::{start_server, BridgeServer, ServerConfig, StartError};
use serde_json::{Map, Value};
use support::fake_adb::{hex_bytes, AdbLog, FakeAdb, FakeFetcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

const TOKEN: &str = "test-token-123"; // leakcheck:allow
/// The `expect` matcher for the session token (a fixture placeholder).
const SESSION_MATCHER: &str = "$token"; // leakcheck:allow
const T0: u64 = 1_000_000;
const WAIT: Duration = Duration::from_secs(2);

type Ws = WebSocketStream<TcpStream>;

struct FakeClock(AtomicU64);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// `pairCodes` in order, then random codes.
struct QueueCodes(Mutex<VecDeque<String>>);

impl CodeSource for QueueCodes {
    fn next_code(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| OsCodeSource.next_code())
    }
}

fn fixture_dir() -> PathBuf {
    // crates/core -> the bridge root.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.parent()
        .and_then(|p| p.parent())
        .expect("bridge root")
        .join("conformance")
        .join("wire")
}

/// Strict deep match with the `$any` / `$token` / `$code` matchers.
fn matches(
    expected: &Value,
    actual: Option<&Value>,
    code: Option<&str>,
    path: &str,
) -> Result<(), String> {
    let show = |v: Option<&Value>| v.map_or("(missing)".to_string(), |v| v.to_string());
    match expected {
        Value::String(s) if s == "$any" => {
            return actual.map(|_| ()).ok_or(format!("{path}: missing"));
        }
        Value::String(s) if s == SESSION_MATCHER => {
            return (actual.and_then(Value::as_str) == Some(TOKEN))
                .then_some(())
                .ok_or(format!("{path}: {} is not the token", show(actual)));
        }
        Value::String(s) if s == "$code" => {
            return (actual.and_then(Value::as_str) == code && code.is_some())
                .then_some(())
                .ok_or(format!(
                    "{path}: {} is not the shown code {code:?}",
                    show(actual)
                ));
        }
        _ => {}
    }
    let Some(actual) = actual else {
        return Err(format!("{path}: missing, expected {expected}"));
    };
    match (expected, actual) {
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                return Err(format!("{path}: expected {expected}, got {actual}"));
            }
            for (i, x) in e.iter().enumerate() {
                matches(x, a.get(i), code, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        (Value::Object(e), Value::Object(a)) => {
            let extra: Vec<&String> = a.keys().filter(|k| !e.contains_key(*k)).collect();
            if !extra.is_empty() {
                return Err(format!("{path}: unexpected keys {extra:?} in {actual}"));
            }
            for (k, x) in e {
                matches(x, a.get(k), code, &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        (Value::Number(e), Value::Number(a)) if e.as_f64() == a.as_f64() => Ok(()),
        (e, a) if e == a => Ok(()),
        _ => Err(format!("{path}: expected {expected}, got {actual}")),
    }
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `"$token"` / `"$code"` values and `{ "$bytes": {...} }` objects in a `send`.
fn expand(v: &Value, code: Option<&str>) -> Value {
    match v {
        Value::String(s) if s == "$token" => Value::String(TOKEN.into()), // leakcheck:allow
        Value::String(s) if s == "$code" => code.map_or(Value::Null, |c| Value::String(c.into())),
        Value::Array(a) => Value::Array(a.iter().map(|x| expand(x, code)).collect()),
        Value::Object(o) => {
            if let Some(spec) = o.get("$bytes") {
                let unit = hex_bytes(spec["hex"].as_str().unwrap());
                let repeat = spec["repeat"].as_u64().unwrap_or(1) as usize;
                return Value::String(base64(&unit.repeat(repeat)));
            }
            Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), expand(x, code)))
                    .collect::<Map<_, _>>(),
            )
        }
        other => other.clone(),
    }
}

/// Opens a socket with exactly these `Origin` / `Host` headers; the upgrade status.
async fn open(port: u16, origin: Option<&str>, host: &str) -> (u16, Option<Ws>) {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut req = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("host", HeaderValue::from_str(host).unwrap());
    if let Some(o) = origin {
        req.headers_mut()
            .insert("origin", HeaderValue::from_str(o).unwrap());
    }
    match tokio_tungstenite::client_async(req, tcp).await {
        Ok((ws, resp)) => (resp.status().as_u16(), Some(ws)),
        Err(WsError::Http(resp)) => (resp.status().as_u16(), None),
        Err(e) => panic!("upgrade failed without a status: {e}"),
    }
}

enum Next {
    Reply(Value),
    Closed(Option<u16>),
}

async fn next(ws: &mut Ws, wait: Duration) -> Option<Next> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next()).await.ok()?;
        return Some(match m {
            Some(Ok(Message::Text(t))) => Next::Reply(serde_json::from_str(&t).unwrap()),
            Some(Ok(Message::Close(f))) => Next::Closed(f.map(|f| u16::from(f.code))),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => Next::Closed(None),
        });
    }
}

async fn reply(ws: &mut Ws, where_: &str) -> Value {
    match next(ws, WAIT).await {
        Some(Next::Reply(v)) => v,
        Some(Next::Closed(c)) => panic!("{where_}: socket closed ({c:?}) before replying"),
        None => panic!("{where_}: timed out waiting for a reply"),
    }
}

async fn closed(ws: &mut Ws, where_: &str) -> Option<u16> {
    match next(ws, WAIT).await {
        Some(Next::Closed(c)) => c,
        Some(Next::Reply(v)) => panic!("{where_}: unread reply {v} while waiting for the close"),
        None => panic!("{where_}: timed out waiting for the socket to close"),
    }
}

async fn http_get(port: u16, path: &str) -> u16 {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(WAIT, s.read_to_end(&mut buf))
        .await
        .expect("http reply timed out")
        .unwrap();
    let text = String::from_utf8_lossy(&buf);
    text.split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .expect("http status line")
}

fn adb_mismatch(expected: &Value, log: &AdbLog) -> Option<String> {
    let got = log.to_json();
    for (key, want) in expected.as_object().unwrap() {
        let Some(have) = got.get(key) else {
            return Some(format!("expectAdb: unknown key {key}"));
        };
        let want = if key == "closed" {
            let mut w: Vec<Value> = want.as_array().unwrap().clone();
            w.sort_by_key(|v| v.to_string());
            Value::Array(w)
        } else {
            want.clone()
        };
        if let Err(m) = matches(&want, Some(have), None, key) {
            return Some(m);
        }
    }
    None
}

/// The last code the bridge showed (its `PairCode` events).
fn drain_codes(events: &mut broadcast::Receiver<BridgeEvent>, code: &mut Option<String>) {
    while let Ok(e) = events.try_recv() {
        if let BridgeEvent::PairCode { code: c, .. } = e {
            *code = Some(c);
        }
    }
}

async fn run_fixture(file: &str, model_via_getprop: bool) {
    let sc: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_dir().join(file)).unwrap()).unwrap();
    let adb_spec = sc.get("adb").cloned().unwrap_or(Value::Null);
    let adb = FakeAdb::new(&adb_spec);
    let adb = Arc::new(if model_via_getprop {
        adb.model_via_getprop()
    } else {
        adb
    });
    let log = adb.log.clone();
    let clock = Arc::new(FakeClock(AtomicU64::new(T0)));
    let codes: VecDeque<String> = sc["pairCodes"]
        .as_array()
        .map(|a| a.iter().map(|c| c.as_str().unwrap().to_string()).collect())
        .unwrap_or_default();
    let server: BridgeServer = start_server(ServerConfig {
        token: TOKEN.into(), // leakcheck:allow
        port: 0,
        adb,
        fetcher: Arc::new(FakeFetcher::new(&adb_spec)),
        clock: clock.clone(),
        codes: Arc::new(QueueCodes(Mutex::new(codes))),
        version: "test".into(),
    })
    .await
    .unwrap_or_else(|e| panic!("{file}: start failed: {e}"));
    let port = server.port();
    let mut events = server.events();
    let mut code: Option<String> = None;
    let mut sockets: HashMap<u64, Ws> = HashMap::new();

    for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
        let where_ = format!(
            "{file} step {i}: {}",
            step.to_string().chars().take(160).collect::<String>()
        );
        let n = step["socket"].as_u64().unwrap_or(0);
        if let Some(o) = step.get("open") {
            let host = o["host"]
                .as_str()
                .unwrap_or("$host")
                .replace("$host", &format!("127.0.0.1:{port}"))
                .replace("$port", &port.to_string());
            let (status, ws) = open(port, o["origin"].as_str(), &host).await;
            assert_eq!(
                Some(u64::from(status)),
                step["expectStatus"].as_u64(),
                "{where_}"
            );
            if let Some(ws) = ws {
                sockets.insert(n, ws);
            }
        } else if let Some(path) = step.get("httpGet") {
            let status = http_get(port, path.as_str().unwrap()).await;
            assert_eq!(
                Some(u64::from(status)),
                step["expectStatus"].as_u64(),
                "{where_}"
            );
        } else if step.get("send").is_some()
            || step.get("sendRaw").is_some()
            || step.get("sendBinary").is_some()
        {
            let ws = sockets
                .get_mut(&n)
                .unwrap_or_else(|| panic!("{where_}: socket {n} is not open"));
            drain_codes(&mut events, &mut code);
            let msg = if let Some(s) = step.get("send") {
                Message::text(expand(s, code.as_deref()).to_string())
            } else if let Some(raw) = step.get("sendRaw") {
                Message::text(raw.as_str().unwrap().to_string())
            } else {
                Message::binary(hex_bytes(step["sendBinary"].as_str().unwrap()))
            };
            ws.send(msg).await.unwrap();
            let got = reply(ws, &where_).await;
            drain_codes(&mut events, &mut code);
            if let Err(m) = matches(&step["expect"], Some(&got), code.as_deref(), "$") {
                panic!("{where_}\n  {m}\n  got {got}");
            }
        } else if let Some(size) = step.get("sendOversize") {
            let ws = sockets
                .get_mut(&n)
                .unwrap_or_else(|| panic!("{where_}: socket {n} is not open"));
            let text = "x".repeat(size.as_u64().unwrap() as usize);
            // Over the limit the bridge may close before the whole frame is written.
            let _ = ws.send(Message::text(text)).await;
            if let Some(expect) = step.get("expect") {
                let got = reply(ws, &where_).await;
                if let Err(m) = matches(expect, Some(&got), code.as_deref(), "$") {
                    panic!("{where_}\n  {m}");
                }
            } else {
                let c = closed(ws, &where_).await;
                if let Some(want) = step["closeCode"].as_u64() {
                    assert_eq!(c.map(u64::from), Some(want), "{where_}");
                }
            }
        } else if step.get("expectClose").is_some() {
            let ws = sockets
                .get_mut(&n)
                .unwrap_or_else(|| panic!("{where_}: socket {n} is not open"));
            let c = closed(ws, &where_).await;
            if let Some(want) = step["closeCode"].as_u64() {
                assert_eq!(c.map(u64::from), Some(want), "{where_}");
            }
        } else if let Some(ms) = step.get("advanceMs") {
            clock.0.fetch_add(ms.as_u64().unwrap(), Ordering::SeqCst);
        } else if step.get("close").is_some() {
            let mut ws = sockets
                .remove(&n)
                .unwrap_or_else(|| panic!("{where_}: socket {n} is not open"));
            ws.close(None).await.unwrap();
            while let Some(Next::Reply(_)) = next(&mut ws, WAIT).await {}
        } else if let Some(want) = step.get("expectAdb") {
            let deadline = tokio::time::Instant::now() + WAIT;
            loop {
                let m = adb_mismatch(want, &log.lock().unwrap());
                match m {
                    None => break,
                    Some(m) if tokio::time::Instant::now() > deadline => panic!("{where_}\n  {m}"),
                    Some(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        } else {
            panic!("{where_}: unknown step");
        }
    }

    for (n, ws) in sockets.iter_mut() {
        if let Some(Next::Reply(v)) = next(ws, Duration::from_millis(100)).await {
            panic!("{file}: unread reply on socket {n}: {v}");
        }
    }
    drop(sockets);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_fixtures() {
    let mut files: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| f.ends_with(".json"))
        .collect();
    files.sort();
    assert!(files.len() >= 12, "too few fixtures: {files:?}");
    for f in &files {
        let t = std::time::Instant::now();
        run_fixture(f, false).await;
        eprintln!("{f}: {:?}", t.elapsed());
    }
}

/// The fake hands the model over with the connection; once, it does not, so
/// the bridge's `getprop ro.product.model` fallback runs (a model, and none).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_fixture_with_the_getprop_model_fallback() {
    run_fixture("connect-run.json", true).await;
}

#[tokio::test]
async fn start_server_reports_port_in_use() {
    // Stands in for something already on 5577.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let adb_spec = Value::Null;
    let r = start_server(ServerConfig {
        token: TOKEN.into(), // leakcheck:allow
        port,
        adb: Arc::new(FakeAdb::new(&adb_spec)),
        fetcher: Arc::new(FakeFetcher::new(&adb_spec)),
        clock: Arc::new(FakeClock(AtomicU64::new(T0))),
        codes: Arc::new(QueueCodes(Mutex::new(VecDeque::new()))),
        version: "test".into(),
    })
    .await;
    assert!(matches!(r, Err(StartError::PortInUse)), "{:?}", r.err());
}
