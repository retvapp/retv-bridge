//! `installApk` (protocol v2): the bridge downloads an APK from the ReTV site
//! itself and installs it on a TV. A port of the TS reference bridge's `install.ts`.
//!
//! Steps, each one a refusal code on failure (`PROTOCOL.md`, "installApk"):
//! 1. download with redirects refused, within `INSTALL_DOWNLOAD_TIMEOUT`
//!    (`download-failed`; `result.status` for an HTTP status other than 200
//!    that is not a 3xx);
//! 2. at most `MAX_INSTALL_BYTES` (`too-large`);
//! 3. SHA-256 equal to the pinned lowercase hex (`sha256-mismatch`);
//! 4. stage at `INSTALL_STAGING_FILE`, then `pm install -r --user current`
//!    (`install-failed` with pm's output unless its stdout has `Success`).
//!    Without `--user` pm installs for every user, a Google TV kids profile
//!    (a separate Android user) included;
//! 5. once staging began, always `rm -f` the staged file.
//!
//! Steps 4–5 hold the TV's install turn (one per serial, across sockets: the
//! staging file is shared). The install stops when `cancel` fires (its socket
//! closed); if staging had begun, the file is still removed.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Mutex, OwnedMutexGuard};

use crate::api::{ApkFetcher, BridgeDevice, FetchError};
use crate::protocol::{INSTALL_STAGING_FILE, MAX_INSTALL_BYTES};
use crate::session::WireError;

/// Longest a download may take, body included.
pub const INSTALL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// pm's output in a reply is clipped to this many characters (UTF-16 units, as in TS).
pub const INSTALL_OUTPUT_CHARS: usize = 1000;
/// The cleanup `rm -f` gets this long, so a dead TV cannot hold the turn.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolves once `cancel` holds `true` (or its sender is gone).
pub(crate) async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    let _ = cancel.wait_for(|c| *c).await;
}

/// `s` cut to at most `max` UTF-16 units, never inside a character.
pub(crate) fn clip_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

fn download_error(e: FetchError) -> WireError {
    match e {
        FetchError::TooLarge => WireError::new("too-large"),
        // Any 3xx is a redirect, refused: no status in the reply.
        FetchError::Status(s) if !(300..=399).contains(&s) => {
            WireError::with("download-failed", json!({ "status": s }))
        }
        FetchError::Status(_) | FetchError::Redirect | FetchError::Network => {
            WireError::new("download-failed")
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Downloads, checks and installs the APK on `device`; `{ output }` or a
/// wire error. `turn` is this serial's install lock.
pub(crate) async fn install_apk(
    device: &Arc<dyn BridgeDevice>,
    url: &str,
    sha256: &str,
    fetcher: &dyn ApkFetcher,
    turn: Arc<Mutex<()>>,
    mut cancel: watch::Receiver<bool>,
    log: &(dyn Fn(String) + Send + Sync),
) -> Result<Value, WireError> {
    let aborted = |log: &(dyn Fn(String) + Send + Sync)| {
        log("[bridge]   installApk aborted: the site closed the connection".into());
        WireError::new("internal")
    };
    let fetched = tokio::select! {
        r = tokio::time::timeout(INSTALL_DOWNLOAD_TIMEOUT, fetcher.fetch(url, MAX_INSTALL_BYTES)) => r,
        _ = cancelled(&mut cancel) => return Err(aborted(log)),
    };
    let bytes = match fetched {
        Err(_) => {
            log("[bridge]   download timed out".into());
            return Err(WireError::new("download-failed"));
        }
        Ok(Err(e)) => {
            log(format!("[bridge]   download failed: {e}"));
            return Err(download_error(e));
        }
        Ok(Ok(b)) => b,
    };
    if bytes.len() as u64 > MAX_INSTALL_BYTES {
        return Err(WireError::new("too-large"));
    }
    if sha256_hex(&bytes) != sha256 {
        return Err(WireError::new("sha256-mismatch"));
    }
    log(format!(
        "[bridge]   downloaded {} bytes, sha256 ok",
        bytes.len()
    ));

    let turn = tokio::select! {
        g = turn.lock_owned() => g,
        _ = cancelled(&mut cancel) => return Err(aborted(log)),
    };
    // From here on the staged file is removed however this ends: normally
    // below, or by the guard if this future panics or is dropped.
    let cleanup = Cleanup(Some((device.clone(), turn)));
    let result = tokio::select! {
        r = stage_and_install(device.as_ref(), &bytes, log) => r,
        _ = cancelled(&mut cancel) => Err(aborted(log)),
    };
    cleanup.run().await;
    result
}

/// Removes the staged file, then gives the TV's install turn back. Dropped
/// without `run` (a panic, an aborted task), it does so in a task of its own.
struct Cleanup(Option<(Arc<dyn BridgeDevice>, OwnedMutexGuard<()>)>);

async fn remove_staged(device: Arc<dyn BridgeDevice>, turn: OwnedMutexGuard<()>) {
    let rm = format!("rm -f {INSTALL_STAGING_FILE}");
    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, device.run(&rm)).await;
    drop(turn);
}

impl Cleanup {
    async fn run(mut self) {
        if let Some((device, turn)) = self.0.take() {
            remove_staged(device, turn).await;
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some((device, turn)) = self.0.take() {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(remove_staged(device, turn));
            }
        }
    }
}

async fn stage_and_install(
    device: &dyn BridgeDevice,
    bytes: &[u8],
    log: &(dyn Fn(String) + Send + Sync),
) -> Result<Value, WireError> {
    device
        .write_bytes(INSTALL_STAGING_FILE, bytes)
        .await
        .map_err(WireError::from)?;
    let r = device
        .run(&format!(
            "pm install -r --user current {INSTALL_STAGING_FILE}"
        ))
        .await
        .map_err(WireError::from)?;
    let all = format!("{}{}", r.stdout, r.stderr);
    let output = clip_utf16(all.trim(), INSTALL_OUTPUT_CHARS).to_string();
    log(format!("[bridge]   pm install: {output}"));
    if !r.stdout.contains("Success") {
        return Err(WireError::with(
            "install-failed",
            json!({ "output": output }),
        ));
    }
    Ok(json!({ "output": output }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_xx_never_reports_a_status() {
        for s in [300, 301, 302, 304, 307, 308, 399] {
            let e = download_error(FetchError::Status(s));
            assert_eq!((e.code, e.result), ("download-failed", None), "{s}");
        }
        let e = download_error(FetchError::Status(404));
        assert_eq!(e.result, Some(json!({ "status": 404 })));
        let e = download_error(FetchError::Status(204));
        assert_eq!(e.result, Some(json!({ "status": 204 })));
        assert_eq!(download_error(FetchError::Redirect).result, None);
        assert_eq!(download_error(FetchError::TooLarge).code, "too-large");
    }

    #[test]
    fn output_is_clipped_in_utf16_units() {
        assert_eq!(clip_utf16("abc", 2), "ab");
        assert_eq!(clip_utf16("a😀b", 2), "a");
        assert_eq!(clip_utf16("a😀b", 3), "a😀");
        assert_eq!(clip_utf16("ab", 5), "ab");
    }
}
