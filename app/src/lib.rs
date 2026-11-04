//! ReTV Bridge, the tray app: the bridge server, Google's adb fetched on
//! first run (with consent), a small window with the pairing code and the
//! command log, a tab that sets up Samsung TVs, and a tray menu.

pub mod account;
mod activate;
pub mod adb_setup;
pub mod browser;
pub mod clipboard;
pub mod i18n;
pub mod samsung;
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
use crate::state::{format_code, new_token, start_view, AppState, Settings, View};
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
                    let fire_tvs = retv_bridge_mdns::group_services(&found)
                        .into_iter()
                        .filter(|tv| tv.fire_tv)
                        .map(|tv| tv.ip)
                        .collect();
                    let mut s = lock(&st);
                    s.discovery_result(false, found.len());
                    s.fire_tv_result(fire_tvs);
                    drop(s);
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
        dev_origins: retv_bridge_core::protocol::DEV_BUILD,
        unpaired_timeout: retv_bridge_core::server::UNPAIRED_SOCKET,
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
        // First run, or the tools are gone: the window, on the tab the
        // person's TV needs (a Samsung owner is never asked for adb).
        None => window::show(&app, start_view(window::tv_kind(&app).as_deref())),
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
        .manage(samsung::SamsungShared::default())
        .invoke_handler(tauri::generate_handler![
            window::get_state,
            window::get_strings,
            window::set_lang,
            window::accept_adb_download,
            window::pick_adb_zip,
            window::retry_adb,
            window::open_site,
            window::show_log,
            window::open_terms,
            window::set_view,
            window::set_tv_kind,
            window::restart_bridge,
            samsung::samsung_view,
            samsung::samsung_pass,
            samsung::samsung_forget,
            samsung::samsung_sign_in_cancel,
            samsung::samsung_sign_in_reopen,
            samsung::samsung_open_account,
            samsung::samsung_privacy_stop,
            samsung::samsung_scan,
            samsung::samsung_add_ip,
            samsung::samsung_select,
            samsung::samsung_refresh,
            samsung::samsung_sign_in,
            samsung::samsung_install,
            samsung::samsung_reinstall,
            samsung::samsung_install_cancel,
            samsung::samsung_keep_installed,
            samsung::samsung_apps,
            samsung::samsung_tick,
            samsung::samsung_remove,
            samsung::samsung_privacy_start,
            samsung::samsung_remote_pair,
            samsung::samsung_privacy,
            samsung::samsung_privacy_pick,
            account::account_sign_in,
            account::account_cancel,
            account::account_reopen,
            account::account_dismiss,
            account::account_sign_out,
            account::account_set_cell,
            account::account_tvs,
        ])
        .on_window_event(|w, event| {
            // Closing destroys the window (its WebView's memory goes too);
            // Bridge stays in the tray and makes it again when asked.
            if let WindowEvent::CloseRequested { .. } = event {
                window::on_close(w.app_handle());
            }
        })
        .setup(move |app| {
            // A normal app on macOS: an icon in the Dock and in Cmd+Tab, so people can see that Bridge is running
            // and find it (owner, 2026-11-04). The tray icon stays; clicking the Dock icon opens the window.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("app.retv.bridge"));
            let settings = Settings::load(&config_dir);
            // The language the person picked in Bridge, else the system's preferred languages.
            let lang = i18n::pick(settings.lang.as_deref(), lang);
            lock(&shared(app.handle())).locale = lang;
            app.manage(SettingsStore {
                settings: Mutex::new(settings),
                dir: config_dir,
            });
            // «Войти в ReTV»: the sign-in of an earlier run comes back from the key store.
            app.manage(account::init(app.handle(), lang));
            account::restore(app.handle());

            let handle = app.handle().clone();
            #[cfg(any(windows, target_os = "linux"))]
            let _ = app.deep_link().register_all();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    window::on_deep_link(&handle, url.as_str());
                }
            });

            tray::build(app.handle(), lang)?;
            activate::reveal_on_activate(app.handle());
            updates::init(app.handle());
            signals::quit_on_termination(app.handle());
            tauri::async_runtime::spawn(start(app.handle().clone()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the ReTV Bridge app");

    // Closing the window destroys it (`on_window_event`); the last window
    // going does not end the app (no exit code: `prevent_exit`). It ends
    // through «Выйти» (`app.exit(0)`), an update or the system (log out).
    // However it ends, the private adb server goes too.
    app.run(|app, event| match event {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => api.prevent_exit(),
        RunEvent::Exit => stop_adb_on_exit(app),
        // A click on the Dock icon (or Cmd+Tab to Bridge with no window) opens the window.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => window::reveal(app),
        _ => {}
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
