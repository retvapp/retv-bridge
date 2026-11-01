//! ReTV Bridge, the tray app: the bridge server, Google's adb fetched on
//! first run (with consent), a small window with the pairing code and the
//! command log, and a tray menu.

pub mod adb_setup;
pub mod browser;
pub mod i18n;
pub mod signals;
pub mod site;
pub mod state;
pub mod tray;
pub mod updates;
pub mod window;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use retv_bridge_adb::server_proc::BRIDGE_ADB_PORT;
use retv_bridge_adb::{AdbHost, BoxFuture, Discoverer, HttpsApkFetcher, RealAdb, ServerRevive};
use retv_bridge_core::api::{BridgeAdb, Clock};
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::pairing::OsCodeSource;
use retv_bridge_core::protocol::BRIDGE_PORT;
use retv_bridge_core::server::{start_server, ServerConfig};
use retv_bridge_core::validate::MdnsService;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_notification::NotificationExt;

use crate::i18n::{tf, Lang};
use crate::state::{format_code, new_token, AppState, Settings, View};
use crate::tray::SettingsStore;

pub type Shared = Arc<Mutex<AppState>>;

/// The app's state, from any handle.
pub fn shared(app: &AppHandle) -> Shared {
    app.state::<Shared>().inner().clone()
}

/// Locks the state; a panic elsewhere never makes it unusable.
pub fn lock(s: &Shared) -> MutexGuard<'_, AppState> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wall-clock time in ms.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

/// mDNS discovery for `RealAdb`. Denied rounds show the Local Network help in
/// the window (macOS) only as `AppState::discovery_result` decides; a later
/// success clears it.
fn discoverer(app: AppHandle) -> Discoverer {
    Arc::new(move || -> BoxFuture<'static, Vec<MdnsService>> {
        let app = app.clone();
        Box::pin(async move {
            let r = retv_bridge_mdns::discover(
                retv_bridge_mdns::DEFAULT_TIMEOUT,
                &retv_bridge_mdns::SystemInterfaces,
            )
            .await;
            let st = shared(&app);
            match r {
                Ok(found) => {
                    lock(&st).discovery_result(false, found.len());
                    found.iter().map(MdnsService::from).collect()
                }
                Err(e) => {
                    let mut s = lock(&st);
                    s.push_log(format!("[bridge] mDNS discovery failed: {e}"));
                    let denied = cfg!(target_os = "macos")
                        && e == retv_bridge_mdns::DiscoverError::LocalNetworkDenied;
                    s.discovery_result(denied, 0);
                    Vec::new()
                }
            }
        })
    })
}

/// The bridge's port: 5577, or in debug builds `RETV_BRIDGE_PORT` (so the app
/// can run beside another bridge during development).
fn bridge_port() -> u16 {
    if cfg!(debug_assertions) {
        if let Some(p) = std::env::var("RETV_BRIDGE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|p| *p > 0)
        {
            return p;
        }
    }
    BRIDGE_PORT
}

/// Start sequence step 2 onwards: the server, its event loop, then adb.
async fn start(app: AppHandle) {
    let st = shared(&app);
    let revive = {
        let app = app.clone();
        Arc::new(ServerRevive::new(Arc::new(
            move || -> BoxFuture<'static, bool> {
                let app = app.clone();
                Box::pin(async move { adb_setup::revive(&app).await })
            },
        )))
    };
    lock(&st).adb_revive = Some(revive.clone());
    let adb: Arc<dyn BridgeAdb> = Arc::new(
        RealAdb::new(AdbHost::new(BRIDGE_ADB_PORT), discoverer(app.clone())).with_revive(revive),
    );
    let fetcher = match HttpsApkFetcher::new() {
        Ok(f) => Arc::new(f),
        Err(e) => {
            lock(&st).push_log(format!("[bridge] could not set up downloads: {e}"));
            window::show(&app, View::Log);
            return;
        }
    };
    let session_key = lock(&st).token.clone();
    let result = start_server(ServerConfig {
        token: session_key, // leakcheck:allow
        port: bridge_port(),
        adb,
        fetcher,
        clock: Arc::new(SystemClock),
        codes: Arc::new(OsCodeSource),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
    .await;
    let events = result.as_ref().ok().map(|s| s.events());
    let port = result.as_ref().ok().map(|s| s.port());
    lock(&st).server_started(result);
    let Some(mut events) = events else {
        window::show(&app, View::Code);
        return;
    };
    lock(&st).push_log(format!(
        "ReTV Bridge {}: ws://127.0.0.1:{}",
        env!("CARGO_PKG_VERSION"),
        port.unwrap_or_default()
    ));

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let ev = match events.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Missed events may include TVs coming and going: the
                    // tray line is rebuilt from the server itself.
                    {
                        let st = shared(&handle);
                        let mut s = lock(&st);
                        s.push_log(format!("[bridge] ({n} log lines skipped)"));
                        if let Some(live) = s.server.as_ref().map(|srv| srv.connected_tvs()) {
                            s.resync_tvs(live);
                        }
                    }
                    tray::refresh_status(&handle);
                    continue;
                }
                Err(_) => break,
            };
            let st = shared(&handle);
            let (announce, lang) = {
                let mut s = lock(&st);
                (s.on_event(&ev), s.locale)
            };
            if cfg!(debug_assertions) {
                // Development builds also print to the terminal, like the CLI.
                match &ev {
                    BridgeEvent::Log(_) => {} // echoed by `push_log`
                    BridgeEvent::PairCode { code, .. } => eprintln!("[dev] site code {code}"),
                    other => eprintln!("[dev] {other:?}"),
                }
            }
            if let Some(code) = announce {
                notify_code(&handle, lang, &code);
            }
            if matches!(
                ev,
                BridgeEvent::TvConnected { .. } | BridgeEvent::TvDisconnected { .. }
            ) {
                tray::refresh_status(&handle);
            }
        }
    });

    // Step 3: adb if it is installed, else the consent window.
    match adb_setup::installed(&app) {
        Some(path) => {
            if lock(&st).adb_start_begin() {
                adb_setup::start(&app, path).await;
            }
        }
        None => window::show(&app, View::Code),
    }
}

/// «Код для сайта: 482 917».
fn notify_code(app: &AppHandle, lang: Lang, code: &str) {
    let _ = app
        .notification()
        .builder()
        .title("ReTV Bridge")
        .body(tf(lang, "notify.code", &[("code", &format_code(code))]))
        .show();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let lang = Lang::detect();
    // Step 1: the session token.
    let state: Shared = Arc::new(Mutex::new(AppState::new(new_token(), lang)));

    let app = tauri::Builder::default()
        // single-instance first: a second launch focuses this one.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            lock(&shared(app)).push_log("[bridge] second launch: showing this one".into());
            window::show(app, View::Code);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            window::get_state,
            window::get_strings,
            window::accept_adb_download,
            window::pick_adb_zip,
            window::retry_adb,
            window::open_site,
            window::show_log,
            window::open_terms,
        ])
        .on_window_event(|w, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                window::on_close(w.app_handle(), w);
            }
        })
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("app.retv.bridge"));
            app.manage(SettingsStore {
                settings: Mutex::new(Settings::load(&config_dir)),
                dir: config_dir,
            });

            let handle = app.handle().clone();
            #[cfg(any(windows, target_os = "linux"))]
            let _ = app.deep_link().register_all();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    window::on_deep_link(&handle, url.as_str());
                }
            });

            tray::build(app.handle(), lang)?;
            updates::init(app.handle());
            signals::quit_on_termination(app.handle());
            tauri::async_runtime::spawn(start(app.handle().clone()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the ReTV Bridge app");

    // Closing the window only hides it (`on_window_event`), so the app never
    // runs out of windows; it ends through «Выйти», an update or the system
    // (log out). However it ends, the private adb server goes too.
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            stop_adb_on_exit(app);
        }
    });
}

/// Best effort `adb -P 5039 kill-server` when the app ends without «Выйти»
/// (which stops it itself), e.g. at log out.
fn stop_adb_on_exit(app: &AppHandle) {
    let adb = {
        let st = shared(app);
        let mut s = lock(&st);
        if let Some(revive) = &s.adb_revive {
            revive.stop();
        }
        s.adb_proc.take()
    };
    let Some(adb) = adb else {
        return;
    };
    let _ = tauri::async_runtime::block_on(async {
        tokio::time::timeout(Duration::from_secs(3), adb.stop()).await
    });
}
