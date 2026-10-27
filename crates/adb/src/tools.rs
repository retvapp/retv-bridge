//! Google's platform-tools, fetched on first run and never bundled.
//!
//! The zip comes from `dl.google.com` (or is a file the person downloaded
//! themselves) and must match a size and SHA-256 compiled into the binary.
//! Only `adb` (plus its two DLLs on Windows) is unpacked, into
//! `<dir>/platform-tools-r<rev>/`, through a temp dir and a rename, so a crash
//! never leaves a half-written `adb` behind.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

/// One pinned platform-tools zip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolsPin {
    pub revision: &'static str,
    pub url: &'static str,
    /// Lowercase hex.
    pub sha256: &'static str,
    pub size: u64,
}

// Pinned 2026-10-27 from https://dl.google.com/android/repository/repository2-3.xml
// (the only `platform-tools` package listed: r37.0.1). Size and SHA-1 of each
// download match that XML; the SHA-256 values were computed from the same
// downloads. SHA-1 from the XML, for the record:
//   darwin 6ae73f4de6452dc57e62ec02b68eed92a4c21661
//   win    e03e78b1d80b396f1c3358e31251cb31740e1110
//   linux  477254aa5f903c15cf51001717bdf347fb6b53e0
//
// `lipo -info platform-tools/adb` (darwin r37.0.1): "Architectures in the fat
// file: adb are: x86_64 arm64" — universal, so Apple-silicon Macs run it
// natively (no Rosetta).

/// macOS (universal x86_64 + arm64).
pub const PIN_MAC: ToolsPin = ToolsPin {
    revision: "37.0.1",
    url: "https://dl.google.com/android/repository/platform-tools_r37.0.1-darwin.zip",
    sha256: "ee39ad5967e95c2a07f04dbcbde96b1a0c916ba376096db5d2f498b7727a5d1d",
    size: 16_110_554,
};

/// Windows (adb.exe + AdbWinApi.dll + AdbWinUsbApi.dll).
pub const PIN_WIN: ToolsPin = ToolsPin {
    revision: "37.0.1",
    url: "https://dl.google.com/android/repository/platform-tools_r37.0.1-win.zip",
    sha256: "45f4d63113e895ebde0c90f194099a4676b6ac653bd28d54314a9e022bbc1a99",
    size: 8_044_989,
};

/// Linux x86_64 (CI and development; the Bridge ships for macOS and Windows).
pub const PIN_LINUX: ToolsPin = ToolsPin {
    revision: "37.0.1",
    url: "https://dl.google.com/android/repository/platform-tools_r37.0.1-linux.zip",
    sha256: "d230f13842f60f782a8645f9c813f8f845bf36089ea7289f28c48f17979313f1",
    size: 9_054_187,
};

/// The pin for the OS this binary was built for.
pub fn host_pin() -> &'static ToolsPin {
    if cfg!(target_os = "windows") {
        &PIN_WIN
    } else if cfg!(target_os = "macos") {
        &PIN_MAC
    } else {
        &PIN_LINUX
    }
}

/// The only host the download may start on or be redirected to.
const ALLOWED_PREFIX: &str = "https://dl.google.com/";
/// No single unpacked file may be bigger than this (adb is ≈ 19 MB).
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum ToolsError {
    /// Could not reach Google, an HTTP error, a refused redirect, or a
    /// local zip that could not be read. Retry is the answer.
    Network(String),
    /// The bytes are not the pinned file (wrong hash, or more bytes than pinned).
    Sha256Mismatch,
    /// Fewer bytes than pinned: an interrupted download or a partial file.
    Truncated,
    /// The verified zip could not be unpacked or installed.
    Unpack(String),
}

impl fmt::Display for ToolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolsError::Network(m) => write!(f, "network: {m}"),
            ToolsError::Sha256Mismatch => {
                f.write_str("the archive does not match the pinned SHA-256")
            }
            ToolsError::Truncated => f.write_str("the archive is incomplete"),
            ToolsError::Unpack(m) => write!(f, "unpack: {m}"),
        }
    }
}

impl std::error::Error for ToolsError {}

/// A started HTTP GET: the declared length and the body in chunks.
#[async_trait]
pub trait HttpBody: Send {
    fn content_length(&self) -> Option<u64>;
    /// The next chunk, or `None` at the end of the body.
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, ToolsError>;
}

#[async_trait]
pub trait HttpGet: Send + Sync {
    async fn get(&self, url: &str) -> Result<Box<dyn HttpBody>, ToolsError>;
}

pub enum ToolsSource {
    Download(Arc<dyn HttpGet>),
    /// A zip the person downloaded themselves; it must match the same pin.
    LocalZip(PathBuf),
}

/// `<dir>/platform-tools-r<rev>`.
fn install_dir(dir: &Path, pin: &ToolsPin) -> PathBuf {
    dir.join(format!("platform-tools-r{}", pin.revision))
}

fn is_windows_pin(pin: &ToolsPin) -> bool {
    pin.url.ends_with("-win.zip")
}

/// The files taken from `platform-tools/` in the zip; the first one is adb.
fn wanted_files(pin: &ToolsPin) -> &'static [&'static str] {
    if is_windows_pin(pin) {
        &["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll"]
    } else {
        &["adb"]
    }
}

/// The installed adb for this pin, if a complete install is there.
pub fn installed_adb(dir: &Path, pin: &ToolsPin) -> Option<PathBuf> {
    let root = install_dir(dir, pin);
    let files = wanted_files(pin);
    if files.iter().all(|f| root.join(f).is_file()) {
        Some(root.join(files[0]))
    } else {
        None
    }
}

/// Returns the path of a verified adb, downloading and unpacking it first if
/// needed. `progress(received, total)` is called while downloading.
pub async fn ensure_adb(
    dir: &Path,
    pin: &ToolsPin,
    src: ToolsSource,
    progress: impl Fn(u64, u64) + Send + Sync,
) -> Result<PathBuf, ToolsError> {
    if let Some(adb) = installed_adb(dir, pin) {
        return Ok(adb);
    }
    let zip = match src {
        ToolsSource::Download(http) => download(http.as_ref(), pin, &progress).await?,
        ToolsSource::LocalZip(path) => read_local(&path, pin).await?,
    };
    let (dir, pin) = (dir.to_path_buf(), *pin);
    tokio::task::spawn_blocking(move || {
        verify(&zip, &pin)?;
        install(&dir, &pin, &zip)
    })
    .await
    .map_err(|e| ToolsError::Unpack(e.to_string()))?
}

async fn download(
    http: &dyn HttpGet,
    pin: &ToolsPin,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<Vec<u8>, ToolsError> {
    if !pin.url.starts_with(ALLOWED_PREFIX) {
        return Err(ToolsError::Network(
            "refusing a URL outside dl.google.com".into(),
        ));
    }
    let mut body = http.get(pin.url).await?;
    if body.content_length().is_some_and(|n| n > pin.size) {
        return Err(ToolsError::Sha256Mismatch);
    }
    let mut buf = Vec::with_capacity(pin.size as usize);
    progress(0, pin.size);
    while let Some(chunk) = body.chunk().await? {
        if (buf.len() + chunk.len()) as u64 > pin.size {
            return Err(ToolsError::Sha256Mismatch);
        }
        buf.extend_from_slice(&chunk);
        progress(buf.len() as u64, pin.size);
    }
    Ok(buf)
}

async fn read_local(path: &Path, pin: &ToolsPin) -> Result<Vec<u8>, ToolsError> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| ToolsError::Network(e.kind().to_string()))?;
    // Never read more than one byte past the pinned size, whatever the file is.
    let mut buf = Vec::with_capacity(pin.size as usize);
    file.take(pin.size + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| ToolsError::Network(e.kind().to_string()))?;
    Ok(buf)
}

/// Size first, then SHA-256.
fn verify(zip: &[u8], pin: &ToolsPin) -> Result<(), ToolsError> {
    let len = zip.len() as u64;
    if len < pin.size {
        return Err(ToolsError::Truncated);
    }
    if len > pin.size {
        return Err(ToolsError::Sha256Mismatch);
    }
    let got: String = Sha256::digest(zip)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != pin.sha256.to_ascii_lowercase() {
        return Err(ToolsError::Sha256Mismatch);
    }
    Ok(())
}

/// A zip entry name that cannot escape the target dir. Any entry that fails
/// this rejects the whole archive, even if it is not one we would unpack.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.contains('\0')
        && name.split('/').all(|c| c != "..")
}

fn unpack_err(e: impl fmt::Display) -> ToolsError {
    ToolsError::Unpack(e.to_string())
}

fn install(dir: &Path, pin: &ToolsPin, zip: &[u8]) -> Result<PathBuf, ToolsError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(unpack_err)?;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).map_err(unpack_err)?;
        let name = entry.name().map_err(unpack_err)?;
        if !is_safe_name(&name) || entry.enclosed_name().is_none() {
            return Err(ToolsError::Unpack(format!("unsafe entry name: {name:?}")));
        }
    }

    std::fs::create_dir_all(dir).map_err(unpack_err)?;
    let final_dir = install_dir(dir, pin);
    let prefix = format!("platform-tools-r{}.tmp-", pin.revision);
    remove_stale_temp_dirs(dir, &prefix, STALE_TEMP_AGE);

    let tmp = dir.join(format!("{prefix}{}", unique_suffix()));
    let result = unpack_into(&mut archive, pin, &tmp).and_then(|()| {
        if final_dir.exists() {
            // Incomplete (installed_adb said no): replace it.
            std::fs::remove_dir_all(&final_dir).map_err(unpack_err)?;
        }
        std::fs::rename(&tmp, &final_dir).map_err(unpack_err)
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    Ok(final_dir.join(wanted_files(pin)[0]))
}

fn unpack_into(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    pin: &ToolsPin,
    tmp: &Path,
) -> Result<(), ToolsError> {
    std::fs::create_dir(tmp).map_err(unpack_err)?;
    for name in wanted_files(pin) {
        let full = format!("platform-tools/{name}");
        let mut entry = archive
            .by_name(&full)
            .map_err(|_| ToolsError::Unpack(format!("missing {full}")))?;
        if !entry.is_file() || entry.is_symlink() {
            return Err(ToolsError::Unpack(format!("{full} is not a regular file")));
        }
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(ToolsError::Unpack(format!("{full} is too big")));
        }
        let path = tmp.join(name);
        let mut out = std::fs::File::create(&path).map_err(unpack_err)?;
        // `take` bounds what a lying header can make us write; reading to
        // the end makes the zip crate check the CRC.
        let mut limited = (&mut entry).take(MAX_ENTRY_BYTES + 1);
        let n = std::io::copy(&mut limited, &mut out).map_err(unpack_err)?;
        if n > MAX_ENTRY_BYTES {
            return Err(ToolsError::Unpack(format!("{full} is too big")));
        }
        out.flush().map_err(unpack_err)?;
        out.sync_all().map_err(unpack_err)?;
        #[cfg(unix)]
        if *name == wanted_files(pin)[0] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(unpack_err)?;
        }
    }
    Ok(())
}

/// Temp dirs older than this are leftovers of a crashed install.
const STALE_TEMP_AGE: Duration = Duration::from_secs(3600);

/// Removes temp dirs left by crashed installs. A younger one may belong to an
/// install still running (a second Bridge window), so it is left alone.
fn remove_stale_temp_dirs(dir: &Path, prefix: &str, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{:x}{:x}{:x}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

// ---------------------------------------------------------------- reqwest

/// The real downloader: the OS's TLS (native-tls: Security.framework on
/// macOS, SChannel on Windows, OpenSSL on Linux dev/CI), HTTPS only, and at
/// most 3 redirects, all within dl.google.com. TLS only keeps the download
/// private; integrity rests on the pinned size and SHA-256.
pub struct ReqwestGet {
    client: reqwest::Client,
}

impl ReqwestGet {
    pub fn new() -> Result<Self, ToolsError> {
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            let ok_host = attempt.url().scheme() == "https"
                && attempt.url().host_str() == Some("dl.google.com");
            if attempt.previous().len() > 3 {
                attempt.error("too many redirects")
            } else if ok_host {
                attempt.follow()
            } else {
                attempt.error("redirect outside dl.google.com")
            }
        });
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(policy)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("ReTV-Bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ToolsError::Network(e.to_string()))?;
        Ok(Self { client })
    }
}

struct ReqwestBody(reqwest::Response);

#[async_trait]
impl HttpBody for ReqwestBody {
    fn content_length(&self) -> Option<u64> {
        self.0.content_length()
    }
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, ToolsError> {
        self.0
            .chunk()
            .await
            .map(|c| c.map(|b| b.to_vec()))
            .map_err(|e| ToolsError::Network(e.to_string()))
    }
}

#[async_trait]
impl HttpGet for ReqwestGet {
    async fn get(&self, url: &str) -> Result<Box<dyn HttpBody>, ToolsError> {
        if !url.starts_with(ALLOWED_PREFIX) {
            return Err(ToolsError::Network(
                "refusing a URL outside dl.google.com".into(),
            ));
        }
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolsError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ToolsError::Network(format!(
                "HTTP {}",
                resp.status().as_u16()
            )));
        }
        Ok(Box::new(ReqwestBody(resp)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_old_temp_dirs_are_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        let ours = tmp.path().join("platform-tools-r1.tmp-abc");
        let other = tmp.path().join("something-else");
        std::fs::create_dir(&ours).unwrap();
        std::fs::create_dir(&other).unwrap();

        // Fresh: may belong to a run still unpacking → kept.
        remove_stale_temp_dirs(tmp.path(), "platform-tools-r1.tmp-", STALE_TEMP_AGE);
        assert!(ours.exists());

        // Older than the limit → cleared; unrelated dirs never.
        remove_stale_temp_dirs(tmp.path(), "platform-tools-r1.tmp-", Duration::ZERO);
        assert!(!ours.exists());
        assert!(other.exists());
    }
}
