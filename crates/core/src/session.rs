//! One WebSocket: its frames, its requests and the TVs it connected. A port
//! of `session()` in the TS reference bridge's `server.ts`; `PROTOCOL.md` is the
//! contract.
//!
//! - Frames are handled in order: binary or id-less → `bad-request` with
//!   `id: -1`; `hello`, `pairStart`, `pairFinish` without a token check;
//!   anything else needs the token (else `bad-token`, then close 1008).
//! - Each request's checks and its change to the socket's state (serials,
//!   upload, install slot) happen as its frame is read, in arrival order;
//!   only its I/O then runs in a task of its own (at most
//!   `MAX_REQUESTS_IN_FLIGHT` per socket), and replies go out as they finish.
//! - The socket owns the serials it connected. When it closes, its running
//!   requests are cancelled (an install still removes its staged file), then
//!   every TV it holds is closed.

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

use crate::api::{BridgeDevice, TransportError};
use crate::events::BridgeEvent;
use crate::install::{cancelled, clip_utf16, install_apk};
use crate::pairing::{PairFinish, PairStart};
use crate::protocol::{
    DEFAULT_ADB_PORT, FEATURES, MAX_COMMAND_CHARS, MAX_UPLOAD_BYTES, PROTOCOL_VERSION,
};
use crate::server::Shared;
use crate::validate::{
    decode_base64, is_allowed_apk_url, is_allowed_binary_path, is_allowed_device_path, is_lan_ipv4,
    port_value,
};

/// `run` text in the log is clipped to this many characters.
const LOG_CHARS: usize = 2000;
/// Requests one socket may have running at once (`hello` included); more
/// are answered `too-many-requests`.
pub const MAX_REQUESTS_IN_FLIGHT: usize = 32;
/// After the socket closes, running requests get this long to finish (an
/// install removes its staged file) before they are dropped.
const TASK_GRACE: Duration = Duration::from_secs(10);
/// How long a close handshake may take before the TCP connection is dropped.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// A refusal as the wire carries it: `error` and, for a few codes, `result`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WireError {
    pub code: &'static str,
    pub result: Option<Value>,
}

impl WireError {
    pub fn new(code: &'static str) -> WireError {
        WireError { code, result: None }
    }

    pub fn with(code: &'static str, result: Value) -> WireError {
        WireError {
            code,
            result: Some(result),
        }
    }
}

impl From<TransportError> for WireError {
    fn from(e: TransportError) -> WireError {
        WireError::new(e.as_code())
    }
}

fn reply_ok(id: &Value, result: Value) -> String {
    json!({ "id": id, "ok": true, "result": result }).to_string()
}

fn reply_err(id: &Value, e: &WireError) -> String {
    match &e.result {
        Some(r) => json!({ "id": id, "ok": false, "error": e.code, "result": r }),
        None => json!({ "id": id, "ok": false, "error": e.code }),
    }
    .to_string()
}

fn bad_frame() -> String {
    reply_err(&json!(-1), &WireError::new("bad-request"))
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// A string of 1..=`max` UTF-16 units, else `bad-request`.
fn str_param(v: Option<&Value>, max: usize) -> Result<&str, WireError> {
    match v.and_then(Value::as_str) {
        Some(s) if !s.is_empty() && utf16_len(s) <= max => Ok(s),
        _ => Err(WireError::new("bad-request")),
    }
}

fn clip(s: &str) -> String {
    let n = utf16_len(s);
    if n > LOG_CHARS {
        format!("{}… (+{})", clip_utf16(s, LOG_CHARS), n - LOG_CHARS)
    } else {
        s.to_string()
    }
}

/// `id`: a string of at most 64 characters or a finite number.
fn usable_id(id: Option<&Value>) -> Option<Value> {
    match id? {
        Value::String(s) if utf16_len(s) <= 64 => Some(Value::String(s.clone())),
        Value::Number(n) => Some(Value::Number(n.clone())),
        _ => None,
    }
}

/// A request field, `null` when absent.
fn field<'a>(msg: &'a Map<String, Value>, key: &str) -> &'a Value {
    msg.get(key).unwrap_or(&Value::Null)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The one `writeBytes` upload in progress on a socket.
struct Upload {
    serial: String,
    path: String,
    total: u64,
    buf: Vec<u8>,
}

/// A socket's own state, shared with its request tasks.
pub(crate) struct SessionState {
    origin: String,
    devices: Mutex<HashMap<String, Arc<dyn BridgeDevice>>>,
    upload: Mutex<Option<Upload>>,
    installing: AtomicBool,
    closed: AtomicBool,
}

impl SessionState {
    /// Whether this socket holds at least one connected serial.
    pub fn holds_devices(&self) -> bool {
        !lock(&self.devices).is_empty()
    }

    /// The serials this socket holds, with the model the connection read (if any).
    pub fn device_models(&self) -> Vec<(String, Option<String>)> {
        lock(&self.devices)
            .iter()
            .map(|(serial, conn)| (serial.clone(), conn.model()))
            .collect()
    }

    fn device(&self, serial: Option<&Value>) -> Result<Arc<dyn BridgeDevice>, WireError> {
        serial
            .and_then(Value::as_str)
            .and_then(|s| lock(&self.devices).get(s).cloned())
            .ok_or(WireError::new("unknown-serial"))
    }
}

/// Clears the socket's `installing` flag when its install ends, however it ends.
struct Installing(Arc<SessionState>);

impl Drop for Installing {
    fn drop(&mut self) {
        self.0.installing.store(false, Ordering::SeqCst);
    }
}

enum Frame {
    Reply(String),
    /// Reply, then close with 1008.
    BadToken(String),
    /// Handled in a task.
    Spawned,
}

pub(crate) async fn run_session<S>(
    mut ws: WebSocketStream<S>,
    origin: String,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state = Arc::new(SessionState {
        origin: origin.clone(),
        devices: Mutex::new(HashMap::new()),
        upload: Mutex::new(None),
        installing: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    let id = shared.register(state.clone());
    shared.log(format!("[bridge] client connected: {origin}"));

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let mut tasks: JoinSet<()> = JoinSet::new();

    // Why the bridge closes the socket: (code, reason, frame left half-read).
    let close: Option<(CloseCode, &'static str, bool)> = loop {
        tokio::select! {
            Some(text) = out_rx.recv() => {
                if ws.send(Message::text(text)).await.is_err() {
                    break None;
                }
            }
            msg = ws.next() => match msg {
                None => break None,
                Some(Ok(Message::Text(t))) => {
                    match on_text(t.as_str(), &shared, &state, &out_tx, &cancel_rx, &mut tasks) {
                        Frame::Spawned => {}
                        Frame::Reply(r) => {
                            if ws.send(Message::text(r)).await.is_err() {
                                break None;
                            }
                        }
                        Frame::BadToken(r) => {
                            shared.log(format!("[bridge] rejected message with a wrong token from {origin}"));
                            let _ = ws.send(Message::text(r)).await;
                            break Some((CloseCode::Policy, "bad token", false));
                        }
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    if ws.send(Message::text(bad_frame())).await.is_err() {
                        break None;
                    }
                }
                // Close: tungstenite answers it; the stream then ends.
                Some(Ok(_)) => {}
                Some(Err(WsError::Capacity(_))) => {
                    shared.log(format!("[bridge] message too big from {origin}: closing"));
                    break Some((CloseCode::Size, "message too big", true));
                }
                Some(Err(e)) => {
                    shared.log(format!("[bridge] socket error from {origin}: {e}"));
                    break None;
                }
            },
            _ = cancelled(&mut shutdown) => break Some((CloseCode::Away, "bridge stopping", false)),
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    state.closed.store(true, Ordering::SeqCst);

    if let Some((code, reason, half_read)) = close {
        let frame = CloseFrame {
            code,
            reason: reason.into(),
        };
        let _ = tokio::time::timeout(CLOSE_WAIT, ws.close(Some(frame))).await;
        let _ = tokio::time::timeout(CLOSE_WAIT, async {
            if half_read {
                // The rest of the oversized frame is still coming: read it
                // off raw, so the peer gets our close frame instead of a reset.
                let mut sink = [0u8; 16 * 1024];
                while matches!(ws.get_mut().read(&mut sink).await, Ok(n) if n > 0) {}
            } else {
                while let Some(Ok(_)) = ws.next().await {}
            }
        })
        .await;
    }
    drop(ws);

    // Stop what this socket still runs (installs clean up), then close its TVs.
    let _ = cancel_tx.send(true);
    let _ = tokio::time::timeout(TASK_GRACE, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    tasks.abort_all();
    *lock(&state.upload) = None;
    let devices: Vec<Arc<dyn BridgeDevice>> =
        lock(&state.devices).drain().map(|(_, d)| d).collect();
    for d in devices {
        d.close().await;
        shared.emit(BridgeEvent::TvDisconnected {
            serial: d.serial().to_string(),
        });
    }
    shared.unregister(id);
    shared.log(format!("[bridge] client disconnected: {origin}"));
}

/// What a request needs after its checks and state change, which `prepare`
/// does inline, in arrival order (as the TS bridge's synchronous part does).
enum Prepared {
    /// Answered without I/O.
    Now(Result<Value, WireError>),
    /// The I/O left to do, in its own task. `finishes_on_close`: it is not
    /// dropped when the socket closes (it stops itself, or must finish).
    Later { work: Work, finishes_on_close: bool },
}

type Work = Pin<Box<dyn Future<Output = Result<Value, WireError>> + Send>>;

fn later(work: impl Future<Output = Result<Value, WireError>> + Send + 'static) -> Prepared {
    Prepared::Later {
        work: Box::pin(work),
        finishes_on_close: false,
    }
}

/// The reply line for a finished request; failures are logged by code only.
fn reply_for(shared: &Shared, id: &Value, r: Result<Value, WireError>) -> String {
    match r {
        Ok(result) => reply_ok(id, result),
        Err(e) => {
            shared.log(format!("[bridge]   failed: {}", e.code));
            reply_err(id, &e)
        }
    }
}

fn on_text(
    text: &str,
    shared: &Arc<Shared>,
    state: &Arc<SessionState>,
    out: &mpsc::UnboundedSender<String>,
    cancel: &watch::Receiver<bool>,
    tasks: &mut JoinSet<()>,
) -> Frame {
    let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(text) else {
        return Frame::Reply(bad_frame());
    };
    let Some(id) = usable_id(msg.get("id")) else {
        return Frame::Reply(bad_frame());
    };
    let kind = msg.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "pairStart" || kind == "pairFinish" {
        return Frame::Reply(pair_reply(kind, &msg, &id, shared, &state.origin));
    }
    if kind != "hello" && !shared.token_matches(msg.get("token")) {
        return Frame::BadToken(reply_err(&id, &WireError::new("bad-token")));
    }
    while tasks.try_join_next().is_some() {}
    if tasks.len() >= MAX_REQUESTS_IN_FLIGHT {
        shared.log(format!(
            "[bridge] too many requests at once from {}",
            state.origin
        ));
        return Frame::Reply(reply_err(&id, &WireError::new("too-many-requests")));
    }
    let prepared = if kind == "hello" {
        // No token needed: the site learns whether its stored token is still good.
        let paired = shared.token_matches(msg.get("token"));
        let shared = shared.clone();
        later(async move {
            let adb = shared.adb.available().await;
            Ok(json!({
                "version": shared.version,
                "protocol": PROTOCOL_VERSION,
                "features": FEATURES,
                "adb": adb,
                "paired": paired,
            }))
        })
    } else {
        prepare(shared, state, &msg, cancel)
    };
    match prepared {
        Prepared::Now(r) => Frame::Reply(reply_for(shared, &id, r)),
        Prepared::Later {
            work,
            finishes_on_close,
        } => {
            let (shared, out, mut stop) = (shared.clone(), out.clone(), cancel.clone());
            tasks.spawn(async move {
                let work = AssertUnwindSafe(work).catch_unwind();
                let r = tokio::select! {
                    r = work => r.unwrap_or_else(|_| {
                        shared.log("[bridge]   failed: a request handler panicked".into());
                        Err(WireError::new("internal"))
                    }),
                    _ = cancelled(&mut stop), if !finishes_on_close => return,
                };
                let _ = out.send(reply_for(&shared, &id, r));
            });
            Frame::Spawned
        }
    }
}

/// `pairStart` / `pairFinish`. Logs the outcome only: never the code typed or
/// the token handed out.
fn pair_reply(
    kind: &str,
    msg: &Map<String, Value>,
    id: &Value,
    shared: &Shared,
    origin: &str,
) -> String {
    if kind == "pairStart" {
        return match shared.pair_start(origin) {
            PairStart::Code { expires_in } => {
                shared.log(format!("[bridge] pairing code shown for {origin}"));
                reply_ok(id, json!({ "expiresIn": expires_in }))
            }
            refused => {
                let e = match refused {
                    PairStart::Locked { retry_in } => {
                        WireError::with("pair-locked", json!({ "retryIn": retry_in }))
                    }
                    PairStart::Busy { retry_in } => {
                        WireError::with("pair-busy", json!({ "retryIn": retry_in }))
                    }
                    _ => WireError::new("pair-disabled"),
                };
                shared.log(format!(
                    "[bridge] pairing start refused for {origin}: {}",
                    e.code
                ));
                reply_err(id, &e)
            }
        };
    }
    let code = match msg.get("code").and_then(Value::as_str) {
        Some(c) if utf16_len(c) <= 16 => c,
        _ => return reply_err(id, &WireError::new("bad-request")),
    };
    let e = match shared.pair_finish(origin, code) {
        PairFinish::Paired(token) => {
            shared.log(format!("[bridge] site paired: {origin}"));
            return reply_ok(id, json!({ "token": token })); // leakcheck:allow
        }
        PairFinish::BadCode { attempts_left } => {
            WireError::with("bad-code", json!({ "attemptsLeft": attempts_left }))
        }
        PairFinish::Locked { retry_in } => {
            WireError::with("pair-locked", json!({ "retryIn": retry_in }))
        }
        PairFinish::Busy { retry_in } => {
            WireError::with("pair-busy", json!({ "retryIn": retry_in }))
        }
        PairFinish::Expired => WireError::new("pair-expired"),
    };
    shared.log(format!("[bridge] pairing refused for {origin}: {}", e.code));
    reply_err(id, &e)
}

/// An integer-valued JSON number in `1..=max`.
fn int_in(v: Option<&Value>, max: u64) -> Option<u64> {
    let f = v?.as_f64()?;
    (f.fract() == 0.0 && f >= 1.0 && f <= max as f64).then_some(f as u64)
}

/// A request's checks and its change to the socket's state (serials, upload,
/// install slot), done before the next frame is read; then its I/O.
fn prepare(
    shared: &Arc<Shared>,
    state: &Arc<SessionState>,
    msg: &Map<String, Value>,
    cancel: &watch::Receiver<bool>,
) -> Prepared {
    match prepare_checked(shared, state, msg, cancel) {
        Ok(p) => p,
        Err(e) => Prepared::Now(Err(e)),
    }
}

fn prepare_checked(
    shared: &Arc<Shared>,
    state: &Arc<SessionState>,
    msg: &Map<String, Value>,
    cancel: &watch::Receiver<bool>,
) -> Result<Prepared, WireError> {
    let adb = shared.adb.clone();
    Ok(
        match msg.get("type").and_then(Value::as_str).unwrap_or("") {
            "discover" => later(async move { Ok(json!({ "tvs": adb.discover().await })) }),
            "pair" => {
                if !is_lan_ipv4(field(msg, "ip")) {
                    return Err(WireError::new("ip-not-allowed"));
                }
                let port = port_value(field(msg, "port"));
                let code = msg.get("code").and_then(Value::as_str);
                let (Some(port), Some(code)) = (port, code) else {
                    return Err(WireError::new("bad-request"));
                };
                if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(WireError::new("bad-request"));
                }
                let address = format!("{}:{port}", field(msg, "ip").as_str().unwrap_or_default());
                shared.log(format!("[bridge] pair {address}"));
                let code = code.to_string();
                later(async move {
                    adb.pair(&address, &code).await?;
                    Ok(json!({}))
                })
            }
            "connect" => {
                if !is_lan_ipv4(field(msg, "ip")) {
                    return Err(WireError::new("ip-not-allowed"));
                }
                let ip = field(msg, "ip").as_str().unwrap_or_default().to_string();
                let port = match msg.get("port") {
                    None | Some(Value::Null) => None,
                    Some(p) => Some(port_value(p).ok_or(WireError::new("bad-request"))?),
                };
                let (shared, state) = (shared.clone(), state.clone());
                later(async move { connect(&shared, &state, ip, port).await })
            }
            "run" => {
                let d = state.device(msg.get("serial"))?;
                let cmd = str_param(msg.get("cmd"), MAX_COMMAND_CHARS)?.to_string();
                shared.log(format!("[bridge] run {}: {}", d.serial(), clip(&cmd)));
                let shared = shared.clone();
                later(async move {
                    let r = d.run(&cmd).await?;
                    shared.log(format!("[bridge]   exit {}", r.exit_code));
                    Ok(json!({ "exitCode": r.exit_code, "stdout": r.stdout, "stderr": r.stderr }))
                })
            }
            "writeFile" => {
                let d = state.device(msg.get("serial"))?;
                if !is_allowed_device_path(field(msg, "path")) {
                    return Err(WireError::new("path-not-allowed"));
                }
                let Some(data) = msg.get("data").and_then(Value::as_str) else {
                    return Err(WireError::new("bad-request"));
                };
                let path = field(msg, "path").as_str().unwrap_or_default().to_string();
                shared.log(format!(
                    "[bridge] writeFile {}: {path} ({} chars)",
                    d.serial(),
                    utf16_len(data)
                ));
                let data = data.to_string();
                later(async move {
                    d.write_file(&path, &data).await?;
                    Ok(json!({}))
                })
            }
            "writeBytes" => write_bytes(shared, state, msg)?,
            "installApk" => {
                let d = state.device(msg.get("serial"))?;
                let url = match msg.get("url").and_then(Value::as_str) {
                    Some(u) if is_allowed_apk_url(u) => u.to_string(),
                    _ => return Err(WireError::new("url-not-allowed")),
                };
                let sha256 = match msg.get("sha256").and_then(Value::as_str) {
                    Some(s)
                        if s.len() == 64
                            && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
                    {
                        s.to_string()
                    }
                    _ => return Err(WireError::new("bad-request")),
                };
                if state.installing.swap(true, Ordering::SeqCst) {
                    return Err(WireError::new("busy"));
                }
                let installing = Installing(state.clone());
                shared.log(format!("[bridge] installApk {}: {url}", d.serial()));
                let (shared, cancel) = (shared.clone(), cancel.clone());
                Prepared::Later {
                    // It watches the socket's close itself, to remove its staged file.
                    finishes_on_close: true,
                    work: Box::pin(async move {
                        let _installing = installing;
                        let turn = shared.install_turn(d.serial());
                        let log = |line: String| shared.log(line);
                        install_apk(
                            &d,
                            &url,
                            &sha256,
                            shared.fetcher.as_ref(),
                            turn,
                            cancel,
                            &log,
                        )
                        .await
                    }),
                }
            }
            "disconnect" => {
                let d = state.device(msg.get("serial"))?;
                lock(&state.devices).remove(d.serial());
                {
                    let mut up = lock(&state.upload);
                    if up.as_ref().is_some_and(|u| u.serial == d.serial()) {
                        *up = None;
                    }
                }
                shared.log(format!("[bridge] disconnect {}", d.serial()));
                let shared = shared.clone();
                Prepared::Later {
                    // The serial is already gone from the socket: its close must happen.
                    finishes_on_close: true,
                    work: Box::pin(async move {
                        d.close().await;
                        shared.emit(BridgeEvent::TvDisconnected {
                            serial: d.serial().to_string(),
                        });
                        Ok(json!({}))
                    }),
                }
            }
            _ => return Err(WireError::new("bad-request")),
        },
    )
}

async fn connect(
    shared: &Shared,
    state: &SessionState,
    ip: String,
    port: Option<u16>,
) -> Result<Value, WireError> {
    let adb = &shared.adb;
    let port = match port {
        Some(p) => p,
        // Wireless debugging listens on a random port the TV announces over mDNS.
        None => adb
            .discover()
            .await
            .iter()
            .find(|t| t.ip == ip)
            .and_then(|t| t.connect_port)
            .unwrap_or(DEFAULT_ADB_PORT),
    };
    let address = format!("{ip}:{port}");
    shared.log(format!("[bridge] connect {address}"));
    let conn = adb.connect(&address).await?;
    if state.closed.load(Ordering::SeqCst) {
        conn.close().await;
        return Err(WireError::new("internal"));
    }
    let serial = conn.serial().to_string();
    let previous = lock(&state.devices).insert(serial.clone(), conn.clone());
    if let Some(p) = previous {
        if !Arc::ptr_eq(&p, &conn) {
            p.close().await;
        }
    }
    let model = match conn.model() {
        Some(m) => m,
        None => match conn.run("getprop ro.product.model").await {
            Ok(r) if r.exit_code == 0 => r.stdout.trim().to_string(),
            _ => String::new(),
        },
    };
    shared.emit(BridgeEvent::TvConnected {
        serial: serial.clone(),
        model: model.clone(),
    });
    Ok(json!({ "serial": serial, "model": model }))
}

/// `{ serial, path, offset, total, data }`: base64 chunks in order; offset 0
/// starts a new upload (dropping any unfinished one). The file goes to the TV
/// in one push after the last chunk.
fn write_bytes(
    shared: &Arc<Shared>,
    state: &Arc<SessionState>,
    msg: &Map<String, Value>,
) -> Result<Prepared, WireError> {
    let d = state.device(msg.get("serial"))?;
    if !is_allowed_binary_path(field(msg, "path")) {
        return Err(WireError::new("path-not-allowed"));
    }
    let path = field(msg, "path").as_str().unwrap_or_default();
    let bytes = decode_base64(field(msg, "data"));
    let total = int_in(msg.get("total"), MAX_UPLOAD_BYTES as u64);
    let offset = msg
        .get("offset")
        .filter(|v| v.is_number())
        .and_then(Value::as_f64);
    let (Some(total), Some(offset), Some(bytes)) = (total, offset, bytes) else {
        return Err(WireError::new("bad-request"));
    };
    if bytes.is_empty() {
        return Err(WireError::new("bad-request"));
    }
    let done = {
        let mut up = lock(&state.upload);
        if offset == 0.0 {
            *up = Some(Upload {
                serial: d.serial().to_string(),
                path: path.to_string(),
                total,
                buf: Vec::new(),
            });
            shared.log(format!(
                "[bridge] writeBytes {}: {path} ({total} bytes)",
                d.serial()
            ));
        }
        let continues = up.as_ref().is_some_and(|u| {
            u.serial == d.serial()
                && u.path == path
                && u.total == total
                && u.buf.len() as f64 == offset
                && offset + bytes.len() as f64 <= total as f64
        });
        if !continues {
            *up = None;
            return Err(WireError::new("bad-request"));
        }
        let u = up.as_mut().expect("checked above");
        u.buf.extend_from_slice(&bytes);
        let received = u.buf.len() as u64;
        if received < u.total {
            return Ok(Prepared::Now(Ok(
                json!({ "received": received, "done": false }),
            )));
        }
        up.take().expect("checked above")
    };
    let shared = shared.clone();
    Ok(later(async move {
        d.write_bytes(&done.path, &done.buf).await?;
        shared.log(format!("[bridge]   wrote {} bytes", done.total));
        Ok(json!({ "received": done.total, "done": true }))
    }))
}

/// SHA-256 of a token; both sides are hashed so the compare is constant-time
/// whatever the lengths.
pub(crate) fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub(crate) fn digest_matches(expected: &[u8; 32], given: Option<&Value>) -> bool {
    match given.and_then(Value::as_str) {
        Some(t) if !t.is_empty() => bool::from(token_digest(t).ct_eq(expected)),
        _ => false,
    }
}
