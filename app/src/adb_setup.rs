//! First-run setup of Google's platform-tools and the bridge's private adb
//! server. Nothing is downloaded until the person agrees in the window.

use std::path::PathBuf;
use std::sync::Arc;

use retv_bridge_adb::server_proc::{restart_adb_server, start_adb_server, BRIDGE_ADB_PORT};
use retv_bridge_adb::tools::{ensure_adb, host_pin, installed_adb, ReqwestGet, ToolsSource};
use tauri::{AppHandle, Manager};

use crate::state::{AdbRetry, AdbSetupState, View};
use crate::{lock, shared, window};

/// Where the archive comes from.
pub enum Source {
    /// From dl.google.com, after consent.
    Download,
    /// A zip the person downloaded and picked; it must match the same pin.
    LocalZip(PathBuf),
}

/// `<app data>/platform-tools`.
pub fn tools_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("app.retv.bridge"))
        .join("platform-tools")
}

/// The download's size in whole megabytes, for the consent text.
pub fn size_mb() -> u64 {
    (host_pin().size + 500_000) / 1_000_000
}

/// The adb already installed for this build's pin.
pub fn installed(app: &AppHandle) -> Option<PathBuf> {
    installed_adb(&tools_dir(app), host_pin())
}

/// Starts fetching the tools unless that is already running or done.
pub fn begin(app: &AppHandle, src: Source) -> bool {
    {
        let st = shared(app);
        let mut s = lock(&st);
        if matches!(
            s.adb,
            AdbSetupState::Downloading { .. } | AdbSetupState::Starting | AdbSetupState::Ready
        ) {
            return false;
        }
        s.adb = AdbSetupState::Downloading {
            done: 0,
            total: host_pin().size,
        };
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let st = shared(&app);
        let source = match src {
            Source::LocalZip(p) => ToolsSource::LocalZip(p),
            Source::Download => match ReqwestGet::new() {
                Ok(h) => ToolsSource::Download(Arc::new(h)),
                Err(e) => {
                    let mut s = lock(&st);
                    s.push_log(format!("[bridge] could not set up downloads: {e}"));
                    s.adb = AdbSetupState::Failed(e);
                    return;
                }
            },
        };
        let progress_state = st.clone();
        let result = ensure_adb(&tools_dir(&app), host_pin(), source, move |done, total| {
            lock(&progress_state).adb = AdbSetupState::Downloading { done, total };
        })
        .await;
        match result {
            Ok(adb) => {
                {
                    let mut s = lock(&st);
                    s.adb = AdbSetupState::Starting;
                    s.push_log(format!(
                        "[bridge] Android platform-tools r{} ready",
                        host_pin().revision
                    ));
                }
                start(&app, adb).await;
            }
            Err(e) => {
                let mut s = lock(&st);
                s.push_log(format!("[bridge] Android platform-tools: {e}"));
                s.adb = AdbSetupState::Failed(e);
            }
        }
    });
    true
}

/// «Повторить»: starts the installed tools again after a failed start, or
/// downloads them again after a failed download.
pub fn retry(app: &AppHandle) {
    let installed = installed(app);
    let action = lock(&shared(app)).adb_retry(installed.is_some());
    match (action, installed) {
        (AdbRetry::Start, Some(path)) => {
            if lock(&shared(app)).adb_start_begin() {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { start(&app, path).await });
            }
        }
        (AdbRetry::Download, _) => {
            begin(app, Source::Download);
        }
        _ => {}
    }
}

/// The private adb server stopped answering after it was up: starts it
/// again (`ServerRevive` decides when). Not before the first start, and
/// not once the app is quitting; a server started while the app began to
/// quit is stopped again.
pub async fn revive(app: &AppHandle) -> bool {
    let st = shared(app);
    let stopping = |st: &crate::Shared| lock(st).adb_revive.as_ref().is_none_or(|r| r.is_stopped());
    if stopping(&st) || !matches!(lock(&st).adb, AdbSetupState::Ready) {
        return false;
    }
    let Some(path) = installed(app) else {
        return false;
    };
    match restart_adb_server(&path, BRIDGE_ADB_PORT).await {
        // Still there, only slow: left alone.
        Ok(None) => false,
        Ok(Some(proc)) => {
            // One lock for the check and the store: `stop_bridge` stops the
            // revive and takes `adb_proc` under the same lock, so a server
            // started here is either stored before it or stopped below.
            let leftover = {
                let mut s = lock(&st);
                if s.adb_revive.as_ref().is_none_or(|r| r.is_stopped()) {
                    Some(proc)
                } else {
                    s.push_log(format!(
                        "[bridge] the adb server had stopped: started again on 127.0.0.1:{}",
                        proc.port()
                    ));
                    s.adb_proc = Some(proc);
                    None
                }
            };
            match leftover {
                Some(proc) => {
                    let _ = proc.stop().await;
                    false
                }
                None => true,
            }
        }
        Err(e) => {
            let mut s = lock(&st);
            s.push_log(format!("[bridge] could not start adb: {e}"));
            if let Some(d) = e.detail() {
                s.push_log(format!("[bridge]   {d}"));
            }
            false
        }
    }
}

/// Starts the private adb server on 5039 (the state is already `Starting`),
/// then opens the site once. A failure shows the retry card; its detail goes
/// to the window's log.
pub async fn start(app: &AppHandle, adb: PathBuf) {
    let st = shared(app);
    match start_adb_server(&adb, BRIDGE_ADB_PORT).await {
        Ok(proc) => {
            {
                let mut s = lock(&st);
                s.adb = AdbSetupState::Ready;
                s.push_log(format!("[bridge] adb server on 127.0.0.1:{}", proc.port()));
                s.adb_proc = Some(proc);
            }
            window::open_site_once(app);
        }
        Err(e) => {
            lock(&st).adb_start_failed(
                format!("[bridge] could not start adb: {e}"),
                e.detail().map(str::to_string),
            );
            window::show(app, View::Code);
        }
    }
}
