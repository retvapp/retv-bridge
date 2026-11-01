//! Smart-socket framing and the `host:` requests.
//!
//! A request is 4 hex digits of payload length, then the payload. The server
//! answers `OKAY` or `FAIL` followed by a length-prefixed message.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use retv_bridge_core::api::{ShellResult, TransportError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::errors::{map_adb_failure, server_connect_error, stream_error};
use crate::sync::PushError;

const DEFAULT_PORT: &str = "5555";
const STATE_POLL: Duration = Duration::from_millis(200);
const MAX_REQUEST: usize = 0xffff;

/// Receives log lines: details that never go on the wire (adb error text,
/// malformed replies), and notices such as a device without shell v2.
pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;

/// How long the client waits on the adb server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// One host request (`host:version`, `get-state`, opening a device
    /// service) from opening the socket to the reply. Default 10 s.
    pub request: Duration,
    /// `host:connect` and `host:pair`, which wait on the network. Default 30 s.
    pub connect: Duration,
    /// After `host:connect`, until the device is `device`. Default 10 s.
    pub ready: Duration,
    /// Shell output and sync transfers: the longest gap with no bytes moving.
    /// There is no total cap. Default 60 s.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            request: Duration::from_secs(10),
            connect: Duration::from_secs(30),
            ready: Duration::from_secs(10),
            idle: Duration::from_secs(60),
        }
    }
}

/// The adb server on 127.0.0.1:`port`.
#[derive(Clone)]
pub struct AdbHost {
    port: u16,
    pub(crate) timeouts: Timeouts,
    log: Option<LogSink>,
}

impl AdbHost {
    pub fn new(port: u16) -> AdbHost {
        AdbHost {
            port,
            timeouts: Timeouts::default(),
            log: None,
        }
    }

    pub fn with_timeouts(mut self, timeouts: Timeouts) -> AdbHost {
        self.timeouts = timeouts;
        self
    }

    /// Where log lines go; without a sink they go to stderr.
    pub fn with_log(mut self, sink: LogSink) -> AdbHost {
        self.log = Some(sink);
        self
    }

    pub(crate) fn log(&self, line: String) {
        match &self.log {
            Some(sink) => sink(line),
            None => eprintln!("[adb] {line}"),
        }
    }

    /// A failure with no transport code: logs `detail`, answers `Internal`.
    pub(crate) fn internal(&self, detail: String) -> TransportError {
        self.log(detail);
        TransportError::Internal
    }

    /// A `FAIL` text as its transport code; unrecognised text is logged.
    fn fail_text(&self, context: &str, msg: &str) -> TransportError {
        match map_adb_failure(msg) {
            TransportError::Internal => self.internal(fail_detail(context, msg)),
            code => code,
        }
    }

    /// The server's protocol version (`host:version`). A server that does not
    /// answer in time counts as not running.
    pub async fn version(&self) -> Result<u32, TransportError> {
        let reply = match timeout(self.timeouts.request, self.query_raw("host:version")).await {
            Err(_) => return Err(TransportError::AdbServerUnreachable),
            Ok(r) => r?,
        };
        let reply = reply.map_err(|msg| self.fail_text("host:version", &msg))?;
        u32::from_str_radix(reply.trim(), 16)
            .map_err(|_| self.internal(format!("adb server sent an unparsable version {reply:?}")))
    }

    /// `adb connect <addr>`, then waits until the device is `device`.
    /// Returns the serial (`ip:port`; adb appends port 5555 when absent).
    pub async fn connect(&self, addr: &str) -> Result<String, TransportError> {
        let serial = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("{addr}:{DEFAULT_PORT}")
        };
        let req = format!("host:connect:{serial}");
        let reply = self.query(&req, self.timeouts.connect).await?;
        let lower = reply.to_ascii_lowercase();
        if !(lower.starts_with("connected to") || lower.starts_with("already connected to")) {
            // The key is not accepted yet: the TV shows the authorization prompt.
            if lower.contains("failed to authenticate") {
                return Err(TransportError::Unauthorized);
            }
            // Like the engine, a refused connect is the device's network.
            return Err(match map_adb_failure(&reply) {
                TransportError::Internal => {
                    self.log(format!("{req}: {reply:?}"));
                    TransportError::DeviceUnreachable
                }
                other => other,
            });
        }
        self.wait_for_device(&serial).await?;
        Ok(serial)
    }

    // Right after `host:connect` the transport still handshakes: `get-state`
    // answers FAIL "device '<s>' not found", "device still connecting" or
    // "device still authorizing", or the states `offline` / `connecting` /
    // `authorizing`. All of these are polled through until `ready` runs out.
    async fn wait_for_device(&self, serial: &str) -> Result<(), TransportError> {
        let deadline = Instant::now() + self.timeouts.ready;
        let req = format!("host-serial:{serial}:get-state");
        let mut seen_offline = false;
        loop {
            let reply = match timeout(self.timeouts.request, self.query_raw(&req)).await {
                Err(_) => return Err(TransportError::ConnectionLost),
                Ok(r) => r?,
            };
            match reply {
                Ok(state) => match state.trim() {
                    "device" => return Ok(()),
                    "unauthorized" => return Err(TransportError::Unauthorized),
                    "offline" => seen_offline = true,
                    _ => {}
                },
                Err(msg) => {
                    let lower = msg.to_ascii_lowercase();
                    if lower.contains("unauthorized") {
                        return Err(TransportError::Unauthorized);
                    } else if lower.contains("offline") {
                        seen_offline = true;
                    } else if !(lower.contains("not found")
                        || lower.contains("still connecting")
                        || lower.contains("still authorizing"))
                    {
                        return Err(self.fail_text(&req, &msg));
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(if seen_offline {
                    TransportError::DeviceOffline
                } else {
                    TransportError::DeviceNotFound
                });
            }
            tokio::time::sleep(STATE_POLL).await;
        }
    }

    /// `adb pair <addr> <code>`. Any refusal is `PairingFailed`.
    pub async fn pair(&self, addr: &str, code: &str) -> Result<(), TransportError> {
        let req = format!("host:pair:{code}:{addr}");
        match self.query(&req, self.timeouts.connect).await {
            Ok(reply) if reply.starts_with("Successfully paired") => Ok(()),
            Err(TransportError::AdbServerUnreachable) => Err(TransportError::AdbServerUnreachable),
            _ => Err(TransportError::PairingFailed),
        }
    }

    /// `adb disconnect <addr>`.
    pub async fn disconnect(&self, addr: &str) -> Result<(), TransportError> {
        self.query(&format!("host:disconnect:{addr}"), self.timeouts.request)
            .await
            .map(|_| ())
    }

    /// The device's adbd features (`shell_v2`, `cmd`, ...).
    pub async fn features(&self, serial: &str) -> Result<Vec<String>, TransportError> {
        let reply = self
            .query(
                &format!("host-serial:{serial}:features"),
                self.timeouts.request,
            )
            .await?;
        Ok(reply.trim().split(',').map(str::to_string).collect())
    }

    /// Runs `cmd` on the device with shell protocol v2 (no PTY).
    pub async fn shell(&self, serial: &str, cmd: &str) -> Result<ShellResult, TransportError> {
        let sock = self
            .open_service(serial, &format!("shell,v2,raw:{cmd}"))
            .await?;
        crate::shell_v2::run(self, sock).await
    }

    /// Writes `bytes` to `path` on the device through the sync service.
    pub async fn push(
        &self,
        serial: &str,
        path: &str,
        bytes: &[u8],
        mode: u32,
    ) -> Result<(), PushError> {
        let sock = self.open_service(serial, "sync:").await?;
        crate::sync::send(self, sock, path, bytes, mode).await
    }

    /// A socket switched to the device (`host:transport:<serial>`) with
    /// `service` opened on it, within the request timeout.
    async fn open_service(&self, serial: &str, service: &str) -> Result<TcpStream, TransportError> {
        if service.len() > MAX_REQUEST {
            return Err(self.internal(format!(
                "request of {} bytes is over adb's {MAX_REQUEST}-byte limit",
                service.len()
            )));
        }
        let open = async {
            let mut sock = self.open().await?;
            for req in [format!("host:transport:{serial}"), service.to_string()] {
                if let Err(msg) = request(&mut sock, &req).await? {
                    return Err(self.fail_text(&req, &msg));
                }
            }
            Ok(sock)
        };
        timeout(self.timeouts.request, open)
            .await
            .unwrap_or(Err(TransportError::ConnectionLost))
    }

    async fn open(&self) -> Result<TcpStream, TransportError> {
        let sock = TcpStream::connect(("127.0.0.1", self.port))
            .await
            .map_err(server_connect_error)?;
        let _ = sock.set_nodelay(true);
        Ok(sock)
    }

    /// One request whose `OKAY` carries a length-prefixed reply, within
    /// `limit`; a timeout is `ConnectionLost`.
    async fn query(&self, req: &str, limit: Duration) -> Result<String, TransportError> {
        match timeout(limit, self.query_raw(req)).await {
            Err(_) => Err(TransportError::ConnectionLost),
            Ok(r) => r?.map_err(|msg| self.fail_text(req, &msg)),
        }
    }

    /// The reply, or `Err(text)` for `FAIL text`; the outer error is the
    /// socket's.
    async fn query_raw(&self, req: &str) -> Result<Result<String, String>, TransportError> {
        if req.len() > MAX_REQUEST {
            return Err(self.internal(format!(
                "request of {} bytes is over adb's {MAX_REQUEST}-byte limit",
                req.len()
            )));
        }
        let mut sock = self.open().await?;
        match request(&mut sock, req).await? {
            Ok(()) => Ok(Ok(read_framed(&mut sock).await?)),
            Err(msg) => Ok(Err(msg)),
        }
    }
}

/// Sends `req` (at most `MAX_REQUEST` bytes) and reads the status: `Ok(Ok)`
/// for `OKAY`, `Ok(Err(text))` for `FAIL text`.
async fn request(sock: &mut TcpStream, req: &str) -> Result<Result<(), String>, TransportError> {
    let mut out = format!("{:04x}", req.len()).into_bytes();
    out.extend_from_slice(req.as_bytes());
    sock.write_all(&out).await.map_err(stream_error)?;
    let mut status = [0u8; 4];
    sock.read_exact(&mut status).await.map_err(stream_error)?;
    match &status {
        b"OKAY" => Ok(Ok(())),
        b"FAIL" => Ok(Err(read_framed(sock).await?)),
        _ => Err(TransportError::ConnectionLost),
    }
}

async fn read_framed(sock: &mut TcpStream) -> Result<String, TransportError> {
    let mut len = [0u8; 4];
    sock.read_exact(&mut len).await.map_err(stream_error)?;
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or(TransportError::ConnectionLost)?;
    let mut buf = vec![0u8; len];
    sock.read_exact(&mut buf).await.map_err(stream_error)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// `read_exact` that fails with `TimedOut` after `idle` without a byte.
pub(crate) async fn read_exact_idle(
    sock: &mut TcpStream,
    buf: &mut [u8],
    idle: Duration,
) -> io::Result<()> {
    let mut n = 0;
    while n < buf.len() {
        match timeout(idle, sock.read(&mut buf[n..])).await {
            Err(_) => return Err(io::ErrorKind::TimedOut.into()),
            Ok(Ok(0)) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(Ok(k)) => n += k,
            Ok(Err(e)) => return Err(e),
        }
    }
    Ok(())
}

/// `write_all` that fails with `TimedOut` after `idle` without progress.
pub(crate) async fn write_all_idle(
    sock: &mut TcpStream,
    buf: &[u8],
    idle: Duration,
) -> io::Result<()> {
    let mut n = 0;
    while n < buf.len() {
        match timeout(idle, sock.write(&buf[n..])).await {
            Err(_) => return Err(io::ErrorKind::TimedOut.into()),
            Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(Ok(k)) => n += k,
            Ok(Err(e)) => return Err(e),
        }
    }
    Ok(())
}

/// The log line of an unrecognised `FAIL`: the request (e.g. `shell,v2,raw:<line>`) and adb's text, with ReTV Home's
/// one-time sign-in code hidden (`--es code <hidden>`), so no bridge log ever holds it.
fn fail_detail(context: &str, msg: &str) -> String {
    use retv_bridge_core::commands::redact_secrets;
    format!(
        "{}: adb said {:?}",
        redact_secrets(context),
        redact_secrets(msg)
    )
}

#[cfg(test)]
mod fail_detail_tests {
    use super::fail_detail;

    #[test]
    fn hides_the_sign_in_code() {
        let code = "Ab0_-".repeat(8) + "xyz";
        let req = format!("shell,v2,raw:am broadcast --user 0 -n app.retv.home/.data.SiteSignInReceiver -a app.retv.home.action.SITE_SIGN_IN -f 32 --es cell world --es code {code}");
        let line = fail_detail(&req, &format!("closed: --es code {code}"));
        assert!(!line.contains(&code), "{line}");
        assert!(
            line.contains("--es cell world --es code <hidden>: adb said"),
            "{line}"
        );
        assert_eq!(
            fail_detail("host:version", "nope"),
            "host:version: adb said \"nope\""
        );
    }
}
