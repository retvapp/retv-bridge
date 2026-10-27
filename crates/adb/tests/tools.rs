//! Task 11: fetching, verifying and unpacking platform-tools, and running the
//! private adb server. Everything here is local: a fake `HttpGet`, zips built
//! in the test, and the fake `adb` binary (testbin/fake_adb.rs).

#[path = "support/fake_adb_bin.rs"]
mod fake_adb_bin;

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fake_adb_bin::{calls, count, fake_adb, port_is_free, test_port, wait_port_free};
use retv_bridge_adb::server_proc::{start_adb_server, ServerError};
use retv_bridge_adb::tools::{
    ensure_adb, installed_adb, HttpBody, HttpGet, ToolsError, ToolsPin, ToolsSource, PIN_LINUX,
    PIN_MAC, PIN_WIN,
};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

// ---------------------------------------------------------------- helpers

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);
    for (name, data) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A pin for a zip built in the test. `os` is `darwin`, `linux` or `win`.
fn pin_for(zip: &[u8], os: &str) -> ToolsPin {
    let url = format!("https://dl.google.com/android/repository/platform-tools_r99.0.0-{os}.zip");
    ToolsPin {
        revision: "99.0.0",
        url: Box::leak(url.into_boxed_str()),
        sha256: Box::leak(sha_hex(zip).into_boxed_str()),
        size: zip.len() as u64,
    }
}

fn mac_zip() -> Vec<u8> {
    build_zip(&[
        ("platform-tools/", b""),
        ("platform-tools/adb", b"#!adb-binary"),
        ("platform-tools/fastboot", b"fastboot"),
        ("platform-tools/NOTICE.txt", b"notice"),
        ("platform-tools/lib64/libc++.dylib", b"lib"),
    ])
}

fn win_zip() -> Vec<u8> {
    build_zip(&[
        ("platform-tools/adb.exe", b"MZ-adb"),
        ("platform-tools/AdbWinApi.dll", b"MZ-api"),
        ("platform-tools/AdbWinUsbApi.dll", b"MZ-usb"),
        ("platform-tools/fastboot.exe", b"MZ-fastboot"),
        ("platform-tools/etc1tool.exe", b"MZ-etc"),
    ])
}

/// Serves `body` in 1 KiB chunks; `cut_at` ends the stream early.
struct FakeHttp {
    body: Vec<u8>,
    cut_at: Option<usize>,
    requested: Mutex<Vec<String>>,
}

impl FakeHttp {
    fn new(body: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            body,
            cut_at: None,
            requested: Mutex::new(Vec::new()),
        })
    }
    fn truncated(body: Vec<u8>, cut_at: usize) -> Arc<Self> {
        Arc::new(Self {
            body,
            cut_at: Some(cut_at),
            requested: Mutex::new(Vec::new()),
        })
    }
}

struct FakeBody {
    data: Vec<u8>,
    pos: usize,
    declared: u64,
}

#[async_trait]
impl HttpBody for FakeBody {
    fn content_length(&self) -> Option<u64> {
        Some(self.declared)
    }
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, ToolsError> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let end = (self.pos + 1024).min(self.data.len());
        let out = self.data[self.pos..end].to_vec();
        self.pos = end;
        Ok(Some(out))
    }
}

#[async_trait]
impl HttpGet for FakeHttp {
    async fn get(&self, url: &str) -> Result<Box<dyn HttpBody>, ToolsError> {
        self.requested.lock().unwrap().push(url.to_string());
        let data = match self.cut_at {
            Some(n) => self.body[..n].to_vec(),
            None => self.body.clone(),
        };
        Ok(Box::new(FakeBody {
            data,
            pos: 0,
            declared: self.body.len() as u64,
        }))
    }
}

fn entries_of(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn no_progress(_: u64, _: u64) {}

// ---------------------------------------------------------------- pins

#[test]
fn pins_point_at_google_and_look_like_sha256() {
    for pin in [&PIN_MAC, &PIN_WIN, &PIN_LINUX] {
        assert!(pin
            .url
            .starts_with("https://dl.google.com/android/repository/platform-tools_r"));
        assert!(pin.url.contains(pin.revision));
        assert_eq!(pin.sha256.len(), 64);
        assert!(pin
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(pin.size > 1_000_000);
    }
    assert!(PIN_MAC.url.ends_with("-darwin.zip"));
    assert!(PIN_WIN.url.ends_with("-win.zip"));
    assert!(PIN_LINUX.url.ends_with("-linux.zip"));
}

// ---------------------------------------------------------------- download

#[tokio::test]
async fn downloads_verifies_and_unpacks_only_adb() {
    // mac layout
    let tmp = tempfile::tempdir().unwrap();
    let zip = mac_zip();
    let pin = pin_for(&zip, "darwin");
    let http = FakeHttp::new(zip.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let adb = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(http.clone()),
        move |a, b| seen2.lock().unwrap().push((a, b)),
    )
    .await
    .unwrap();

    assert_eq!(
        http.requested.lock().unwrap().as_slice(),
        &[pin.url.to_string()]
    );
    let install = tmp.path().join("platform-tools-r99.0.0");
    assert_eq!(adb, install.join("adb"));
    assert_eq!(entries_of(tmp.path()), vec!["platform-tools-r99.0.0"]);
    assert_eq!(entries_of(&install), vec!["adb"]);
    assert_eq!(std::fs::read(&adb).unwrap(), b"#!adb-binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&adb).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
    let last = seen.lock().unwrap().last().copied();
    assert_eq!(last, Some((zip.len() as u64, zip.len() as u64)));
    assert_eq!(installed_adb(tmp.path(), &pin), Some(adb.clone()));

    // win layout
    let tmp = tempfile::tempdir().unwrap();
    let zip = win_zip();
    let pin = pin_for(&zip, "win");
    let adb = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(FakeHttp::new(zip)),
        no_progress,
    )
    .await
    .unwrap();
    let install = tmp.path().join("platform-tools-r99.0.0");
    assert_eq!(adb, install.join("adb.exe"));
    assert_eq!(
        entries_of(&install),
        vec!["AdbWinApi.dll", "AdbWinUsbApi.dll", "adb.exe"]
    );
    assert_eq!(
        std::fs::read(install.join("AdbWinUsbApi.dll")).unwrap(),
        b"MZ-usb"
    );
}

#[tokio::test]
async fn truncated_download_is_rejected_and_nothing_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = mac_zip();
    let pin = pin_for(&zip, "darwin");
    let http = FakeHttp::truncated(zip.clone(), zip.len() / 2);
    let err = ensure_adb(tmp.path(), &pin, ToolsSource::Download(http), no_progress)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolsError::Truncated), "{err:?}");
    assert!(entries_of(tmp.path()).is_empty());
    assert_eq!(installed_adb(tmp.path(), &pin), None);
}

#[tokio::test]
async fn wrong_sha_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = mac_zip();
    let mut pin = pin_for(&zip, "darwin");
    // Same size, different bytes: a tampered or proxied file.
    let mut tampered = zip.clone();
    let i = tampered.len() / 2;
    tampered[i] ^= 0xff;
    let err = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(FakeHttp::new(tampered)),
        no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolsError::Sha256Mismatch), "{err:?}");
    assert!(entries_of(tmp.path()).is_empty());

    // A bigger body than pinned is rejected too (as a different file).
    let mut longer = zip.clone();
    longer.extend_from_slice(&[0u8; 4096]);
    let err = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(FakeHttp::new(longer)),
        no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolsError::Sha256Mismatch), "{err:?}");

    // A pin whose hash is wrong never installs, even for the "right" file.
    pin.sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
    let err = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(FakeHttp::new(zip)),
        no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolsError::Sha256Mismatch), "{err:?}");
    assert!(entries_of(tmp.path()).is_empty());
}

#[tokio::test]
async fn download_refuses_a_url_off_dl_google_com() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = mac_zip();
    let mut pin = pin_for(&zip, "darwin");
    pin.url = "https://example.com/android/repository/platform-tools_r99.0.0-darwin.zip";
    let http = FakeHttp::new(zip);
    let err = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(http.clone()),
        no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolsError::Network(_)), "{err:?}");
    assert!(http.requested.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------- manual zip

#[tokio::test]
async fn manual_zip_with_pinned_sha_installs() {
    let tmp = tempfile::tempdir().unwrap();
    let dl = tempfile::tempdir().unwrap();
    let zip = mac_zip();
    let pin = pin_for(&zip, "darwin");

    // Not the pinned file → rejected.
    let wrong = dl.path().join("wrong.zip");
    let mut other = zip.clone();
    let i = other.len() - 10;
    other[i] ^= 1;
    std::fs::write(&wrong, &other).unwrap();
    let err = ensure_adb(tmp.path(), &pin, ToolsSource::LocalZip(wrong), no_progress)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolsError::Sha256Mismatch), "{err:?}");

    // A half-downloaded file → truncated.
    let half = dl.path().join("half.zip");
    std::fs::write(&half, &zip[..zip.len() / 3]).unwrap();
    let err = ensure_adb(tmp.path(), &pin, ToolsSource::LocalZip(half), no_progress)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolsError::Truncated), "{err:?}");
    assert!(entries_of(tmp.path()).is_empty());

    // The pinned file → installed.
    let good = dl.path().join("platform-tools_r99.0.0-darwin.zip");
    std::fs::write(&good, &zip).unwrap();
    let adb = ensure_adb(tmp.path(), &pin, ToolsSource::LocalZip(good), no_progress)
        .await
        .unwrap();
    assert_eq!(adb, tmp.path().join("platform-tools-r99.0.0").join("adb"));
    assert_eq!(installed_adb(tmp.path(), &pin), Some(adb));
}

// ---------------------------------------------------------------- atomicity

#[tokio::test]
async fn install_is_atomic() {
    let tmp = tempfile::tempdir().unwrap();

    // 1. A crash mid-unpack leaves only a temp dir: that is not an install.
    //    A fresh one may belong to a run still going, so it is left alone
    //    (dirs older than an hour are cleared: unit test in tools.rs).
    let zip = mac_zip();
    let pin = pin_for(&zip, "darwin");
    let stale = tmp.path().join("platform-tools-r99.0.0.tmp-123abc");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("adb"), b"half").unwrap();
    assert_eq!(installed_adb(tmp.path(), &pin), None);

    // 2. A failure after the first file is written (bad CRC on the second
    //    allow-listed entry) leaves no install and no temp dir of its own.
    let win = win_zip();
    let mut corrupt = win.clone();
    let pos = corrupt
        .windows(6)
        .position(|w| w == b"MZ-api")
        .expect("stored entry data");
    corrupt[pos + 3] = b'X';
    let wpin = pin_for(&corrupt, "win");
    let err = ensure_adb(
        tmp.path(),
        &wpin,
        ToolsSource::Download(FakeHttp::new(corrupt)),
        no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolsError::Unpack(_)), "{err:?}");
    assert_eq!(installed_adb(tmp.path(), &wpin), None);
    assert_eq!(
        entries_of(tmp.path()),
        vec!["platform-tools-r99.0.0.tmp-123abc"]
    );

    // 3. A good run installs by rename, and leaves no temp dir behind.
    let adb = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(FakeHttp::new(zip)),
        no_progress,
    )
    .await
    .unwrap();
    assert_eq!(
        entries_of(tmp.path()),
        vec![
            "platform-tools-r99.0.0",
            "platform-tools-r99.0.0.tmp-123abc"
        ]
    );
    assert_eq!(std::fs::read(adb).unwrap(), b"#!adb-binary");

    // 4. An existing install is reused without a download.
    let http = FakeHttp::new(Vec::new());
    ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(http.clone()),
        no_progress,
    )
    .await
    .unwrap();
    assert!(http.requested.lock().unwrap().is_empty());
}

#[tokio::test]
async fn zip_slip_entry_is_rejected() {
    for evil in [
        "../platform-tools/adb",
        concat!("platform-tools/..", "/../adb"),
        "/platform-tools/adb",
        "platform-tools\\..\\..\\adb",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let zip = build_zip(&[("platform-tools/adb", b"ok"), (evil, b"evil")]);
        let pin = pin_for(&zip, "darwin");
        let err = ensure_adb(
            tmp.path(),
            &pin,
            ToolsSource::Download(FakeHttp::new(zip)),
            no_progress,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolsError::Unpack(_)), "{evil}: {err:?}");
        assert!(entries_of(tmp.path()).is_empty(), "{evil}");
        assert!(!tmp.path().parent().unwrap().join("adb").exists());
    }

    // A zip without adb at all is rejected too.
    let tmp = tempfile::tempdir().unwrap();
    let zip = build_zip(&[("platform-tools/fastboot", b"fb")]);
    let pin = pin_for(&zip, "darwin");
    let err = ensure_adb(
        tmp.path(),
        &pin,
        ToolsSource::Download(FakeHttp::new(zip)),
        no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolsError::Unpack(_)), "{err:?}");
}

// ---------------------------------------------------------------- server
//
// These run on every OS: the fake adb is a small Rust binary of this crate
// (testbin/fake_adb.rs), copied into a temp dir with a `fake.cfg` beside it.

/// Listens on an ephemeral port; each connection gets `answer` and is closed.
async fn listener(answer: &'static [u8]) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(answer).await;
                let _ = s.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test]
async fn free_port_starts_the_server_with_private_env() {
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let proc = start_adb_server(&adb, port).await.unwrap();
    assert_eq!(proc.port(), port);
    let c = calls(&log);
    let start = format!("-P {port} start-server port={port} mdns=0");
    assert_eq!(c.iter().filter(|l| **l == start).count(), 1, "{c:?}");
    assert_eq!(count(&c, "kill-server"), 0, "{c:?}");
    assert!(!port_is_free(port));

    proc.stop().await.unwrap();
    let c = calls(&log);
    let kill = format!("-P {port} kill-server port={port} mdns=0");
    assert_eq!(c.iter().filter(|l| **l == kill).count(), 1, "{c:?}");
    assert!(wait_port_free(port));
}

#[tokio::test]
async fn reuses_own_adb_on_5039() {
    // Started by this Bridge launch → a second start reuses it, no spawn.
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let first = start_adb_server(&adb, port).await.unwrap();
    let again = start_adb_server(&adb, port).await.unwrap();
    let c = calls(&log);
    assert_eq!(count(&c, "start-server"), 1, "{c:?}");
    assert_eq!(count(&c, "kill-server"), 0, "{c:?}");
    drop(first);
    again.stop().await.unwrap();
}

#[tokio::test]
async fn server_left_by_an_earlier_launch_is_restarted_once() {
    // host:version cannot tell our adb from another adb of the same
    // version, so a server found at launch is killed once and restarted.
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = test_port();
    let status = std::process::Command::new(&adb)
        .args(["-P", &port.to_string(), "start-server"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!port_is_free(port));
    let before = calls(&log).len();

    let proc = start_adb_server(&adb, port).await.unwrap();
    let c = calls(&log)[before..].to_vec();
    let kill = format!("-P {port} kill-server port={port} mdns=0");
    let start = format!("-P {port} start-server port={port} mdns=0");
    let k = c.iter().position(|l| *l == kill).expect("kill-server");
    let s = c.iter().position(|l| *l == start).expect("start-server");
    assert!(k < s, "{c:?}");
    assert_eq!(count(&c, "kill-server"), 1, "{c:?}");
    proc.stop().await.unwrap();
}

#[tokio::test]
async fn foreign_listener_on_5039_reports_error() {
    let (_dir, adb, log) = fake_adb(41, "serve");
    let port = listener(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
    let err = start_adb_server(&adb, port).await.unwrap_err();
    assert!(matches!(err, ServerError::PortTakenByOther), "{err:?}");
    let c = calls(&log);
    assert_eq!(count(&c, "kill-server"), 1, "{c:?}");
    assert_eq!(count(&c, "start-server"), 0, "{c:?}");
}

#[tokio::test]
async fn start_that_never_answers_is_start_failed_with_a_fixed_message() {
    // adb exits 0 but nothing ever listens → StartFailed, not a hang.
    let (dir, adb, log) = fake_adb(41, "noop");
    let port = test_port();
    let err = start_adb_server(&adb, port).await.unwrap_err();
    assert!(matches!(err, ServerError::StartFailed(_)), "{err:?}");
    assert_eq!(count(&calls(&log), "start-server"), 1);
    // The public text never carries local details (paths, adb output).
    assert_eq!(err.to_string(), "adb server did not start");
    assert!(err.detail().is_some());

    // adb that cannot run at all: the path stays out of the public text.
    let missing = dir.path().join("no-such-adb");
    let err = start_adb_server(&missing, port).await.unwrap_err();
    assert_eq!(err.to_string(), "adb server did not start");
    assert!(!err.to_string().contains("no-such-adb"));
}
