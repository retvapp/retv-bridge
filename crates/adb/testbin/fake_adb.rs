//! Test-only stand-in for Google's `adb`, used by `tests/tools.rs` on every
//! OS (a shell script would not run on Windows CI). Never shipped: the app
//! bundles only its own binary.
//!
//! The tests copy it into a temp dir as `adb`/`adb.exe` and write `fake.cfg`
//! beside it (`version=<n>`, `mode=serve|noop`). Every call is appended to
//! `calls.log` beside it as `<args> port=<ANDROID_ADB_SERVER_PORT> mdns=<ADB_MDNS>`.
//!
//! - `version` prints "Android Debug Bridge version 1.0.<n>";
//! - `-P <port> start-server`: in `serve` mode starts a detached copy of itself
//!   that listens on 127.0.0.1:<port> and answers `host:version` / `host:kill`
//!   like an adb server, and waits for it; in `noop` mode just exits 0;
//! - `-P <port> kill-server` sends `host:kill` to the port, as adb does.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    let exe = std::env::current_exe().expect("exe");
    let dir = exe.parent().expect("dir").to_path_buf();
    let cfg = std::fs::read_to_string(dir.join("fake.cfg")).unwrap_or_default();
    let get = |k: &str| {
        cfg.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string))
    };
    let version: u32 = get("version").and_then(|v| v.parse().ok()).unwrap_or(41);
    let serve_mode = get("mode").as_deref() != Some("noop");

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        let port: u16 = args[1].parse().expect("port");
        serve(port, version);
        return;
    }

    let line = format!(
        "{} port={} mdns={}\n",
        args.join(" "),
        std::env::var("ANDROID_ADB_SERVER_PORT").unwrap_or_default(),
        std::env::var("ADB_MDNS").unwrap_or_default()
    );
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("calls.log"))
        .expect("log");
    log.write_all(line.as_bytes()).expect("log write");
    drop(log);

    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["version"] => {
            println!("Android Debug Bridge version 1.0.{version}");
            println!("Version 99.0.0-fake");
        }
        ["-P", port, "start-server"] => {
            let port: u16 = port.parse().expect("port");
            if serve_mode {
                // A daemon, like adb's server: deliberately not waited for.
                #[allow(clippy::zombie_processes)]
                Command::new(&exe)
                    .args(["serve", &port.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn server");
                let deadline = Instant::now() + Duration::from_secs(5);
                while TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err() {
                    if Instant::now() > deadline {
                        std::process::exit(1);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        ["-P", port, "kill-server"] => {
            let port: u16 = port.parse().expect("port");
            if let Ok(mut s) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = s.write_all(b"0009host:kill");
                let mut buf = [0u8; 16];
                let _ = s.read(&mut buf);
            }
        }
        _ => std::process::exit(2),
    }
}

fn serve(port: u16, version: u32) {
    let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else {
        std::process::exit(1);
    };
    // Never outlive a test run by much.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
    });
    // One thread per connection, like adb: a client that connects and says
    // nothing never holds up the next one.
    for conn in listener.incoming() {
        let Ok(s) = conn else { continue };
        std::thread::spawn(move || answer(s, version));
    }
}

fn answer(mut s: TcpStream, version: u32) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let mut len = [0u8; 4];
    if s.read_exact(&mut len).is_err() {
        return;
    }
    let n = std::str::from_utf8(&len)
        .ok()
        .and_then(|h| usize::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    let mut req = vec![0u8; n];
    if s.read_exact(&mut req).is_err() {
        return;
    }
    match req.as_slice() {
        b"host:version" => {
            let _ = s.write_all(format!("OKAY0004{version:04x}").as_bytes());
        }
        b"host:kill" => {
            let _ = s.write_all(b"OKAY");
            drop(s);
            std::process::exit(0);
        }
        _ => {
            let _ = s.write_all(b"FAIL0007unknown");
        }
    }
}
