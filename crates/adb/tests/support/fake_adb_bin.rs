//! The fake `adb` binary (testbin/fake_adb.rs) and the ports its server runs
//! on, shared by the server tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The fake adb in a fresh dir; returns (dir guard, adb path, log path).
///
/// A hard link where it can be: a copy is a file open for writing, and a
/// process forked meanwhile by a test on another thread inherits that until
/// it execs, so running the copy could fail with "text file busy".
pub fn fake_adb(version: u32, mode: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let adb = tmp.path().join(name);
    let built = env!("CARGO_BIN_EXE_retv-fake-adb");
    if std::fs::hard_link(built, &adb).is_err() {
        std::fs::copy(built, &adb).unwrap();
    }
    std::fs::write(
        tmp.path().join("fake.cfg"),
        format!("version={version}\nmode={mode}\n"),
    )
    .unwrap();
    let log = tmp.path().join("calls.log");
    (tmp, adb, log)
}

pub fn calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

pub fn count(c: &[String], needle: &str) -> usize {
    c.iter().filter(|l| l.contains(needle)).count()
}

pub fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Nothing listens on `port`: a connect is refused. Unlike a bind check,
/// this never holds the port itself, even for a moment (a bound socket is
/// inherited by any process another test forks meanwhile, and the server
/// test then finds its port taken).
///
/// The refusal is instant on Linux and macOS but takes about 2 s on Windows
/// (it retries the SYN after the RST; see `PROBE_TIMEOUT` in server_proc.rs),
/// so the timeout leaves room for that: with a 2 s one, a free port on Windows
/// mostly timed out instead and counted as taken.
pub fn port_refuses(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    matches!(
        std::net::TcpStream::connect_timeout(&addr, REFUSE_WITHIN),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused
    )
}

/// Longer than a refused loopback connect takes on any OS (Windows: ~2 s).
const REFUSE_WITHIN: Duration = Duration::from_secs(10);

/// Lowest test port. Ports from here to `PORT_END` are below every OS's
/// ephemeral range (Linux 32768+, macOS and Windows 49152+), so neither a
/// `bind(0)` nor an outgoing connection anywhere on the machine is ever
/// handed one. A port from `bind(0)` was: released for the fake server to
/// take, it could be taken by someone else first.
const PORT_BASE: u16 = 20_000;
const PORT_END: u16 = 32_000;

/// A port no other test process uses while this one runs. Each port is
/// claimed with an exclusive lock on `<tmp>/retv-bridge-test-ports/<port>`,
/// held until the process exits (the OS drops it even after a crash), so
/// test runs at once (several worktrees, a reviewer) never share one; a port
/// that is busy anyway is skipped.
pub fn test_port() -> u16 {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    static CLAIMS: Mutex<Vec<std::fs::File>> = Mutex::new(Vec::new());
    let dir = std::env::temp_dir().join("retv-bridge-test-ports");
    std::fs::create_dir_all(&dir).unwrap();
    let span = PORT_END - PORT_BASE;
    // Start each process somewhere else, so claims rarely meet.
    let start = (std::process::id() % 600) as u16 * 20;
    let _ = NEXT.compare_exchange(0, start + 1, Ordering::SeqCst, Ordering::SeqCst);
    for _ in 0..span {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let port = PORT_BASE + n % span;
        let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(port.to_string()))
        else {
            continue;
        };
        if file.try_lock().is_err() {
            continue; // another test process has it
        }
        if port_refuses(port) {
            CLAIMS.lock().unwrap().push(file);
            return port;
        }
    }
    panic!("no free test port in {PORT_BASE}..{PORT_END}");
}

/// Waits (up to 15 s, room for a few ~2 s probes on Windows) for `port` to be
/// free: `kill-server` returns once the server has acknowledged, which is a
/// moment before its process is gone.
pub fn wait_port_free(port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !port_refuses(port) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}
