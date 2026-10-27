//! The bridge's WebSocket server on 127.0.0.1. A port of
//! `startBridgeServer` in the TS reference bridge's `server.ts`; `PROTOCOL.md` is
//! the contract and `conformance/wire/*.json` pins it.
//!
//! - Binds the loopback interface only. A plain HTTP request gets 426, an
//!   upgrade with another method than GET 400, a request head over 16 KiB 431.
//! - The upgrade is refused with 403 unless `Origin` is exactly one of
//!   `ALLOWED_ORIGINS` and `Host` is `127.0.0.1:<port>` or `localhost:<port>`
//!   for the bound port (DNS-rebinding guard). Refusals never touch pairing.
//!   At most `MAX_SOCKETS` sockets are open at once; one more gets 503.
//! - Messages are capped at `MAX_MESSAGE_BYTES`; a bigger one closes the
//!   socket with 1009.
//! - One `Pairing` per server, shared by every socket. The pairing lock is
//!   never held while the configured `CodeSource` runs: codes are drawn
//!   before it is taken (`PrefetchedCodes`).
//! - Logs (as `BridgeEvent::Log`) never hold the token or a pairing code.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::api::{ApkFetcher, BridgeAdb, Clock, CodeSource};
use crate::events::BridgeEvent;
use crate::pairing::{OsCodeSource, PairFinish, PairStart, Pairing};
use crate::protocol::{ALLOWED_HOSTS, ALLOWED_ORIGINS, BRIDGE_HOST, MAX_MESSAGE_BYTES};
use crate::session::{digest_matches, run_session, token_digest, SessionState};

/// The HTTP request head must arrive within this long, and fit in `MAX_HEAD`.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 16 * 1024;
/// `shutdown` waits this long for sockets to close their TVs.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);
const EVENT_CAPACITY: usize = 1024;
/// Sockets open at once; one more is refused with 503 before the upgrade.
pub const MAX_SOCKETS: usize = 16;

pub struct ServerConfig {
    /// The session token (never logged).
    pub token: String, // leakcheck:allow
    /// 0 picks a free port (tests).
    pub port: u16,
    pub adb: Arc<dyn BridgeAdb>,
    pub fetcher: Arc<dyn ApkFetcher>,
    pub clock: Arc<dyn Clock>,
    pub codes: Arc<dyn CodeSource>,
    /// `hello`'s `version`.
    pub version: String,
}

#[derive(Debug)]
pub enum StartError {
    /// Something already listens on the port (another bridge, most likely).
    PortInUse,
    /// Any other bind failure. The text is for local logs.
    Io(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::PortInUse => f.write_str("the bridge port is already in use"),
            StartError::Io(e) => write!(f, "the bridge could not listen: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_code(c: &str) -> bool {
    c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())
}

/// The configured `CodeSource`, drawn from before the pairing lock is taken.
/// `Pairing` takes the drawn code; a malformed one (which would trip its
/// assert) is replaced by an OS-random code, so the lock can never be
/// poisoned by a bad source.
struct PrefetchedCodes {
    source: Arc<dyn CodeSource>,
    slot: Mutex<Option<String>>,
}

impl PrefetchedCodes {
    /// Draws the next code if none is waiting. Holds no lock while the source runs.
    fn fill(&self) {
        if lock(&self.slot).is_some() {
            return;
        }
        let code = self.source.next_code();
        let code = if is_code(&code) {
            code
        } else {
            OsCodeSource.next_code()
        };
        lock(&self.slot).get_or_insert(code);
    }
}

impl CodeSource for PrefetchedCodes {
    fn next_code(&self) -> String {
        lock(&self.slot)
            .take()
            .unwrap_or_else(|| OsCodeSource.next_code())
    }
}

/// Server-wide state, shared by every socket.
pub(crate) struct Shared {
    session_digest: [u8; 32],
    pub adb: Arc<dyn BridgeAdb>,
    pub fetcher: Arc<dyn ApkFetcher>,
    pub version: String,
    port: u16,
    pairing: Mutex<Pairing>,
    codes: Arc<PrefetchedCodes>,
    events: broadcast::Sender<BridgeEvent>,
    /// One install turn per serial, across sockets (the staging file is shared).
    install_turns: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    sessions: Mutex<HashMap<u64, Arc<SessionState>>>,
    sessions_changed: Notify,
    next_session: AtomicU64,
    /// Sockets open or being opened (at most `MAX_SOCKETS`).
    open_sockets: AtomicUsize,
}

impl Shared {
    pub fn log(&self, line: String) {
        let _ = self.events.send(BridgeEvent::Log(line));
    }

    pub fn emit(&self, event: BridgeEvent) {
        let _ = self.events.send(event);
    }

    pub fn token_matches(&self, given: Option<&Value>) -> bool {
        digest_matches(&self.session_digest, given)
    }

    pub fn pair_start(&self, origin: &str) -> PairStart {
        self.codes.fill();
        lock(&self.pairing).start(origin)
    }

    pub fn pair_finish(&self, origin: &str, code: &str) -> PairFinish {
        lock(&self.pairing).finish(origin, code)
    }

    pub fn install_turn(&self, serial: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut turns = lock(&self.install_turns);
        // Forget turns nobody holds or waits for.
        turns.retain(|_, t| Arc::strong_count(t) > 1);
        turns.entry(serial.to_string()).or_default().clone()
    }

    pub fn register(&self, state: Arc<SessionState>) -> u64 {
        let id = self.next_session.fetch_add(1, Ordering::SeqCst);
        lock(&self.sessions).insert(id, state);
        id
    }

    pub fn unregister(&self, id: u64) {
        lock(&self.sessions).remove(&id);
        self.sessions_changed.notify_waiters();
    }

    fn host_allowed(&self, host: &str) -> bool {
        ALLOWED_HOSTS.iter().any(|h| {
            host.strip_prefix(h).and_then(|r| r.strip_prefix(':')) == Some(&self.port.to_string())
        })
    }
}

/// A running bridge.
pub struct BridgeServer {
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    accept: Option<JoinHandle<()>>,
}

impl BridgeServer {
    pub fn port(&self) -> u16 {
        self.shared.port
    }

    pub fn events(&self) -> broadcast::Receiver<BridgeEvent> {
        self.shared.events.subscribe()
    }

    /// Sockets holding at least one connected serial.
    pub fn active_sessions(&self) -> usize {
        lock(&self.shared.sessions)
            .values()
            .filter(|s| s.holds_devices())
            .count()
    }

    /// Every connected TV: serial → the model its connection read, if any. The
    /// app rebuilds its tray line from this after it missed events.
    pub fn connected_tvs(&self) -> BTreeMap<String, Option<String>> {
        let sessions: Vec<Arc<SessionState>> =
            lock(&self.shared.sessions).values().cloned().collect();
        sessions.iter().flat_map(|s| s.device_models()).collect()
    }

    /// The pairing code showing now and when it expires (ms), if one is live.
    pub fn live_code(&self) -> Option<(String, u64)> {
        lock(&self.shared.pairing).live_code()
    }

    /// Stops listening, closes every socket (1001) and the TVs they hold.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(a) = self.accept.take() {
            a.abort();
        }
        let shared = self.shared.clone();
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, async {
            loop {
                let changed = shared.sessions_changed.notified();
                if lock(&shared.sessions).is_empty() {
                    break;
                }
                changed.await;
            }
        })
        .await;
        shared.log("[bridge] stopped".into());
    }
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(a) = self.accept.take() {
            a.abort();
        }
    }
}

/// Binds `127.0.0.1:<port>` and serves the bridge protocol.
pub async fn start_server(cfg: ServerConfig) -> Result<BridgeServer, StartError> {
    let listener = TcpListener::bind((BRIDGE_HOST, cfg.port))
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AddrInUse => StartError::PortInUse,
            _ => StartError::Io(e.to_string()),
        })?;
    let port = listener
        .local_addr()
        .map_err(|e| StartError::Io(e.to_string()))?
        .port();

    let (events, _) = broadcast::channel(EVENT_CAPACITY);
    let codes = Arc::new(PrefetchedCodes {
        source: cfg.codes,
        slot: Mutex::new(None),
    });
    let (on_code, on_disabled) = (events.clone(), events.clone());
    let pairing = Pairing::new(
        cfg.token.clone(),
        cfg.clock,
        codes.clone(),
        Box::new(move |code: &str, expires_at_ms: u64| {
            let _ = on_code.send(BridgeEvent::PairCode {
                code: code.to_string(),
                expires_at_ms,
            });
        }),
        Box::new(move || {
            let _ = on_disabled.send(BridgeEvent::PairingDisabled);
        }),
    );
    let shared = Arc::new(Shared {
        session_digest: token_digest(&cfg.token),
        adb: cfg.adb,
        fetcher: cfg.fetcher,
        version: cfg.version,
        port,
        pairing: Mutex::new(pairing),
        codes,
        events,
        install_turns: Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        sessions_changed: Notify::new(),
        next_session: AtomicU64::new(1),
        open_sockets: AtomicUsize::new(0),
    });
    let (shutdown, shutdown_rx) = watch::channel(false);
    let accept = tokio::spawn(accept_loop(listener, shared.clone(), shutdown_rx));
    Ok(BridgeServer {
        shared,
        shutdown,
        accept: Some(accept),
    })
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>, shutdown: watch::Receiver<bool>) {
    loop {
        match listener.accept().await {
            Ok((tcp, _)) => {
                tokio::spawn(serve_connection(tcp, shared.clone(), shutdown.clone()));
            }
            Err(e) => {
                shared.log(format!("[bridge] accept failed: {e}"));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

enum Head {
    Complete(Vec<u8>),
    /// Over `MAX_HEAD` without the blank line.
    TooBig,
    /// The connection ended or failed first.
    Gone,
}

/// Reads the request head (up to the blank line), without reading past it.
async fn read_head(tcp: &mut TcpStream) -> Head {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    // Byte by byte: the head is small, and nothing after it may be consumed.
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Head::TooBig;
        }
        match tcp.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => return Head::Gone,
        }
    }
    Head::Complete(head)
}

/// What the head asks for.
enum Ask {
    /// A WebSocket upgrade with GET: handed to the handshake.
    Upgrade,
    /// An upgrade with another method: 400.
    BadUpgrade,
    /// Anything else: 426.
    Plain,
}

fn ask(head: &[u8]) -> Ask {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    if !matches!(req.parse(head), Ok(httparse::Status::Complete(_))) {
        return Ask::Plain;
    }
    let upgrade = req.headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case("upgrade")
            && std::str::from_utf8(h.value).is_ok_and(|v| {
                v.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("websocket"))
            })
    });
    match (upgrade, req.method) {
        (false, _) => Ask::Plain,
        (true, Some("GET")) => Ask::Upgrade,
        (true, _) => Ask::BadUpgrade,
    }
}

/// The one value of header `name`, if it appears exactly once.
fn single_header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    let mut all = req.headers().get_all(name).iter();
    let first = all.next()?;
    if all.next().is_some() {
        return None;
    }
    first.to_str().ok()
}

fn printable(s: Option<&str>) -> String {
    match s {
        None => "(none)".into(),
        Some(s) => format!("{:?}", s.chars().take(100).collect::<String>()),
    }
}

/// Answers a refused request and closes. What the client still sends is read
/// off (briefly), so it gets the answer rather than a reset.
async fn refuse_plain(mut tcp: TcpStream, status: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = tcp.write_all(reply.as_bytes()).await;
    let _ = tcp.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        let mut sink = [0u8; 16 * 1024];
        while matches!(tcp.read(&mut sink).await, Ok(n) if n > 0) {}
    })
    .await;
}

/// One of the `MAX_SOCKETS` socket slots, given back when dropped.
struct SocketSlot(Arc<Shared>);

impl SocketSlot {
    fn take(shared: &Arc<Shared>) -> Option<SocketSlot> {
        shared
            .open_sockets
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_SOCKETS).then_some(n + 1)
            })
            .ok()
            .map(|_| SocketSlot(shared.clone()))
    }
}

impl Drop for SocketSlot {
    fn drop(&mut self) {
        self.0.open_sockets.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn serve_connection(
    mut tcp: TcpStream,
    shared: Arc<Shared>,
    shutdown: watch::Receiver<bool>,
) {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut tcp)).await {
        Ok(Head::Complete(h)) => h,
        Ok(Head::TooBig) => {
            return refuse_plain(tcp, "431 Request Header Fields Too Large", "").await;
        }
        Ok(Head::Gone) | Err(_) => return,
    };
    match ask(&head) {
        Ask::Upgrade => {}
        Ask::BadUpgrade => return refuse_plain(tcp, "400 Bad Request", "").await,
        Ask::Plain => {
            let mut tcp = tcp;
            let _ = tcp
                .write_all(
                    b"HTTP/1.1 426 Upgrade Required\r\nConnection: close\r\nUpgrade: websocket\r\n\
                      Content-Type: text/plain; charset=utf-8\r\nContent-Length: 28\r\n\r\n\
                      ReTV Bridge: WebSocket only\n",
                )
                .await;
            let _ = tcp.shutdown().await;
            return;
        }
    }

    let mut origin: Option<String> = None;
    let mut slot: Option<SocketSlot> = None;
    // The error type is tungstenite's own.
    #[allow(clippy::result_large_err)]
    let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let o = single_header(req, "origin");
        let h = single_header(req, "host");
        let ok = o.is_some_and(|o| ALLOWED_ORIGINS.contains(&o))
            && h.is_some_and(|h| shared.host_allowed(h));
        let refuse = |status: StatusCode| {
            let mut r = ErrorResponse::new(None);
            *r.status_mut() = status;
            Err(r)
        };
        if !ok {
            shared.log(format!(
                "[bridge] rejected connection: origin {}, host {}",
                printable(o),
                printable(h)
            ));
            return refuse(StatusCode::FORBIDDEN);
        }
        // Refusals above never take a slot; past here, a full bridge says so.
        let Some(s) = SocketSlot::take(&shared) else {
            shared.log(format!(
                "[bridge] rejected connection: {MAX_SOCKETS} sockets already open"
            ));
            return refuse(StatusCode::SERVICE_UNAVAILABLE);
        };
        slot = Some(s);
        origin = o.map(str::to_string);
        Ok(resp)
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let stream = Replay {
        head,
        pos: 0,
        inner: tcp,
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(config)).await
    else {
        return;
    };
    let (Some(origin), Some(_slot)) = (origin, slot) else {
        return;
    };
    run_session(ws, origin, shared, shutdown).await;
}

/// The TCP stream with the already-read request head put back in front.
struct Replay {
    head: Vec<u8>,
    pos: usize,
    inner: TcpStream,
}

impl AsyncRead for Replay {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.pos < self.head.len() {
            let n = (self.head.len() - self.pos).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.head[start..start + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Replay {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BadSource;
    impl CodeSource for BadSource {
        fn next_code(&self) -> String {
            "nope".into()
        }
    }

    #[test]
    fn a_bad_code_source_never_reaches_pairing() {
        let codes = PrefetchedCodes {
            source: Arc::new(BadSource),
            slot: Mutex::new(None),
        };
        codes.fill();
        assert!(is_code(&codes.next_code()));
        // Nothing drawn: still a valid code.
        assert!(is_code(&codes.next_code()));
    }

    #[test]
    fn upgrade_detection() {
        let a = |h: &[u8]| match ask(h) {
            Ask::Upgrade => "upgrade",
            Ask::BadUpgrade => "bad",
            Ask::Plain => "plain",
        };
        assert_eq!(
            a(b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\n\r\n"),
            "upgrade"
        );
        assert_eq!(
            a(b"PUT / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n"),
            "bad"
        );
        assert_eq!(a(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"), "plain");
        assert_eq!(a(b"garbage\r\n\r\n"), "plain");
    }
}
