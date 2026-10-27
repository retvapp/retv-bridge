//! The Bridge's private adb server: Google's adb, on its own port (5039),
//! never the person's 5037.
//!
//! What is already on the port decides what happens:
//! - nothing → `adb -P <port> start-server`;
//! - an adb server this Bridge launch started → reused, nothing spawned;
//! - anything else (an adb from an earlier launch, someone else's adb, or a
//!   foreign program) → `adb -P <port> kill-server` once, then a fresh start.
//!   `host:version` only says "adb protocol 41", which any recent adb says, so
//!   a server we did not start in this launch is never trusted as ours.
//!   If the port is still taken after the kill, `PortTakenByOther`: a clear
//!   error, not a hang.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use retv_bridge_core::api::TransportError;
use tokio::process::Command;

use crate::host::{AdbHost, Timeouts};

/// The Bridge's adb server port.
pub const BRIDGE_ADB_PORT: u16 = 5039;

/// Turns off adb's own mDNS discovery (the Bridge runs its own, Task 10), so
/// the adb server does not open UDP 5353. Checked on r37.0.1: with it, the
/// server on 5039 holds no UDP 5353 socket.
const MDNS_OFF: (&str, &str) = ("ADB_MDNS", "0");

/// `host:version` from connect to reply. A connect that has not finished by
/// then counts as "nothing answers" (on Windows a refused loopback connect
/// itself takes about 2 s; the bind check decides "free" without waiting).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for the port to answer (or free up) after a command.
const SETTLE_TRIES: u32 = 10;
const SETTLE_STEP: Duration = Duration::from_millis(200);

/// Ports whose adb server this process (this Bridge launch) started.
static STARTED_HERE: Mutex<Option<HashSet<u16>>> = Mutex::new(None);

fn started_here(port: u16) -> bool {
    let guard = STARTED_HERE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().is_some_and(|s| s.contains(&port))
}

fn mark_started(port: u16, started: bool) {
    let mut guard = STARTED_HERE.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if started {
        set.insert(port);
    } else {
        set.remove(&port);
    }
}

#[derive(Debug)]
pub enum ServerError {
    /// Something holds the port even after one `kill-server`.
    PortTakenByOther,
    /// adb would not run, or the server never answered. The text is for
    /// local logs only (it may hold paths or adb output): see `detail`.
    StartFailed(String),
}

impl fmt::Display for ServerError {
    /// Fixed texts, safe to show or send anywhere.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::PortTakenByOther => {
                f.write_str("the adb port is taken by another program")
            }
            ServerError::StartFailed(_) => f.write_str("adb server did not start"),
        }
    }
}

impl std::error::Error for ServerError {}

impl ServerError {
    /// What went wrong, for local logs only: never send it to the site.
    pub fn detail(&self) -> Option<&str> {
        match self {
            ServerError::StartFailed(d) => Some(d),
            ServerError::PortTakenByOther => None,
        }
    }
}

/// A running adb server on `port`, started (or reused) by the Bridge. The
/// server is a daemon: it outlives this value unless `stop` is called.
#[derive(Debug)]
pub struct AdbServerProc {
    adb: PathBuf,
    port: u16,
}

impl AdbServerProc {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `adb -P <port> kill-server`.
    pub async fn stop(self) -> std::io::Result<()> {
        mark_started(self.port, false);
        let status = run(adb_command(&self.adb, self.port, &["kill-server"])).await?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "kill-server exited with {status}"
            )))
        }
    }
}

/// Starts (or reuses) the adb server on `port` (the Bridge uses 5039).
pub async fn start_adb_server(adb: &Path, port: u16) -> Result<AdbServerProc, ServerError> {
    let own = own_version(adb).await?;
    let proc = AdbServerProc {
        adb: adb.to_path_buf(),
        port,
    };

    match probe(port).await {
        Probe::Free => {}
        Probe::Adb(v) if v == own && started_here(port) => return Ok(proc),
        Probe::Adb(_) | Probe::Other => {
            // Once: an adb (ours from an earlier launch, or another one)
            // goes away; a foreign program does not.
            mark_started(port, false);
            let _ = run(adb_command(adb, port, &["kill-server"])).await;
            if !settle_free(port).await {
                return Err(ServerError::PortTakenByOther);
            }
        }
    }

    let status = run(adb_command(adb, port, &["start-server"]))
        .await
        .map_err(|e| ServerError::StartFailed(format!("start-server: {e}")))?;
    if !status.success() {
        return Err(ServerError::StartFailed(format!(
            "start-server exited with {status}"
        )));
    }
    let mut last = Probe::Free;
    for _ in 0..SETTLE_TRIES {
        last = probe(port).await;
        if !matches!(last, Probe::Free) {
            break;
        }
        tokio::time::sleep(SETTLE_STEP).await;
    }
    match last {
        Probe::Adb(v) if v == own => {
            mark_started(port, true);
            Ok(proc)
        }
        Probe::Free => Err(ServerError::StartFailed(
            "the server did not answer after start-server".into(),
        )),
        _ => Err(ServerError::PortTakenByOther),
    }
}

/// Starts the server again after a call found it unreachable, but only when
/// it is really gone: the port can be bound, or a connect to it is refused.
/// A server that only answered slowly (busy) is left alone, `Ok(None)`:
/// `start_adb_server` would otherwise kill it as "not answering".
pub async fn restart_adb_server(
    adb: &Path,
    port: u16,
) -> Result<Option<AdbServerProc>, ServerError> {
    if !server_gone(port).await {
        return Ok(None);
    }
    start_adb_server(adb, port).await.map(Some)
}

/// Nothing listens on 127.0.0.1:`port`.
async fn server_gone(port: u16) -> bool {
    if can_bind(port) {
        return true;
    }
    // A refused connect can take a moment on Windows; a connect that is
    // accepted, or neither in time, means something is still there.
    matches!(
        tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        )
        .await,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused
    )
}

/// `adb -P <port> ...` with the private port and mDNS off, and no console
/// window on Windows.
///
/// All stdio is null: `start-server` leaves a daemon behind, and a daemon
/// that inherited a pipe of ours would keep it open, so waiting for that
/// pipe's end could hang. Only the exit status is used.
fn adb_command(adb: &Path, port: u16, args: &[&str]) -> Command {
    let mut c = Command::new(adb);
    c.arg("-P")
        .arg(port.to_string())
        .args(args)
        .env("ANDROID_ADB_SERVER_PORT", port.to_string())
        .env(MDNS_OFF.0, MDNS_OFF.1)
        // These would point the client somewhere else than our port.
        .env_remove("ADB_SERVER_SOCKET")
        .env_remove("ANDROID_SERIAL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

async fn run(mut c: Command) -> std::io::Result<std::process::ExitStatus> {
    match tokio::time::timeout(COMMAND_TIMEOUT, c.status()).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "adb timed out",
        )),
    }
}

/// The adb server protocol version of this adb binary: the `41` in
/// "Android Debug Bridge version 1.0.41". It is what `host:version` answers.
/// `adb version` starts no daemon, so reading its stdout pipe is safe.
async fn own_version(adb: &Path) -> Result<u32, ServerError> {
    let mut c = Command::new(adb);
    c.arg("version")
        .env(MDNS_OFF.0, MDNS_OFF.1)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = match tokio::time::timeout(COMMAND_TIMEOUT, c.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(ServerError::StartFailed(format!("adb version: {e}"))),
        Err(_) => return Err(ServerError::StartFailed("adb version timed out".into())),
    };
    parse_version(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| ServerError::StartFailed("could not read the adb version".into()))
}

fn parse_version(text: &str) -> Option<u32> {
    let rest = &text[text.find("version 1.0.")? + "version 1.0.".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[derive(Debug)]
enum Probe {
    /// Nothing holds the port.
    Free,
    /// An adb server answered `host:version`.
    Adb(u32),
    /// The port is held by something that did not answer as adb.
    Other,
}

/// Whether 127.0.0.1:`port` can be bound right now. This is the "free"
/// test: it is immediate on every OS (a refused connect is not, on Windows).
fn can_bind(port: u16) -> bool {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
}

/// What holds 127.0.0.1:`port`: nothing, an adb server, or something else.
/// The question itself is `AdbHost::version` (`host:version`).
async fn probe(port: u16) -> Probe {
    let bindable = can_bind(port);
    // Even when bindable, ask: on some systems a listener on 0.0.0.0 does
    // not stop a bind to 127.0.0.1 but still takes the connect.
    let host = AdbHost::new(port)
        .with_timeouts(Timeouts {
            request: PROBE_TIMEOUT,
            ..Timeouts::default()
        })
        // A foreign listener's garbage is expected here, not worth a log line.
        .with_log(Arc::new(|_| {}));
    match host.version().await {
        Ok(v) => Probe::Adb(v),
        // Refused, failed or timed out: nothing answers.
        Err(TransportError::AdbServerUnreachable) if bindable => Probe::Free,
        Err(_) => Probe::Other,
    }
}

/// Polls until the port is free, for up to SETTLE_TRIES steps.
async fn settle_free(port: u16) -> bool {
    for _ in 0..SETTLE_TRIES {
        if matches!(probe(port).await, Probe::Free) {
            return true;
        }
        tokio::time::sleep(SETTLE_STEP).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adb_version_output() {
        let out =
            "Android Debug Bridge version 1.0.41\nVersion 37.0.1-14523553\nInstalled as /x/adb\n";
        assert_eq!(parse_version(out), Some(41));
        assert_eq!(parse_version("garbage"), None);
    }

    #[tokio::test]
    async fn bound_port_that_never_answers_is_other_not_free() {
        // Bound and listening, never accepted or answered.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(!can_bind(port));
        assert!(matches!(probe(port).await, Probe::Other));
        drop(l);
        assert!(matches!(probe(port).await, Probe::Free));
    }
}
