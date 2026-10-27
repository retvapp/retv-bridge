//! The tray (menu bar) icon and its menu, quitting, and "open at login".

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::i18n::{t, Lang};
use crate::state::{
    quit_decision, set_autostart, startup_autostart, AutostartCtl, QuitDecision, Settings, View,
};
use crate::{lock, shared, updates, window};

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-template.png");

/// Menu items that change while the app runs.
pub struct TrayItems {
    status: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    updates: MenuItem<Wry>,
}

/// The settings and the folder they live in.
pub struct SettingsStore {
    pub dir: PathBuf,
    pub settings: Mutex<Settings>,
}

struct Launcher<'a>(&'a AppHandle);

impl AutostartCtl for Launcher<'_> {
    fn enable(&self) -> Result<(), String> {
        self.0.autolaunch().enable().map_err(|e| e.to_string())
    }
    fn disable(&self) -> Result<(), String> {
        self.0.autolaunch().disable().map_err(|e| e.to_string())
    }
}

pub fn build(app: &AppHandle, lang: Lang) -> tauri::Result<()> {
    let store = app.state::<SettingsStore>();
    let autostart_on = startup_autostart(
        &store.settings.lock().unwrap_or_else(|e| e.into_inner()),
        &Launcher(app),
    );

    let open = MenuItem::with_id(app, "open", t(lang, "tray.open"), true, None::<&str>)?;
    let code = MenuItem::with_id(app, "code", t(lang, "tray.showCode"), true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", t(lang, "tray.tvNone"), false, None::<&str>)?;
    let log = MenuItem::with_id(app, "log", t(lang, "tray.log"), true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        t(lang, "tray.autostart"),
        true,
        autostart_on,
        None::<&str>,
    )?;
    // «Проверить обновления», or «Доступна новая версия {v}» (updates.rs).
    let updates = MenuItem::with_id(app, "updates", t(lang, "tray.updates"), false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", t(lang, "tray.quit"), true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &code,
            &sep()?,
            &status,
            &sep()?,
            &log,
            &autostart,
            &updates,
            &sep()?,
            &quit,
        ],
    )?;
    app.manage(TrayItems {
        status,
        autostart: autostart.clone(),
        updates: updates.clone(),
    });
    refresh_updates(app);

    TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(TRAY_ICON)?)
        .icon_as_template(true)
        .tooltip("ReTV Bridge")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => window::open_site_now(app),
            "code" => window::show(app, View::Code),
            "log" => window::show(app, View::Log),
            "autostart" => toggle_autostart(app),
            "updates" => updates::on_tray_click(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Puts the TV line in the menu up to date.
pub fn refresh_status(app: &AppHandle) {
    let text = lock(&shared(app)).tv_status();
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.status.set_text(text);
    }
}

/// Puts the update item up to date.
pub fn refresh_updates(app: &AppHandle) {
    let (text, enabled) = {
        let st = shared(app);
        let s = lock(&st);
        updates::tray_item(&s.update, s.locale)
    };
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.updates.set_text(text);
        let _ = items.updates.set_enabled(enabled);
    }
}

fn toggle_autostart(app: &AppHandle) {
    let store = app.state::<SettingsStore>();
    let mut settings = store.settings.lock().unwrap_or_else(|e| e.into_inner());
    let want = !settings.autostart;
    let result = set_autostart(&mut settings, &store.dir, &Launcher(app), want);
    if let Err(e) = &result {
        lock(&shared(app)).push_log(format!("[bridge] open at login: {e}"));
    }
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.autostart.set_checked(settings.autostart);
    }
}

/// «Выйти»: asks first while a TV is held.
pub fn request_quit(app: &AppHandle) {
    let (active, lang) = {
        let st = shared(app);
        let s = lock(&st);
        (s.active_sessions(), s.locale)
    };
    match quit_decision(active) {
        QuitDecision::Quit => quit(app.clone()),
        QuitDecision::Confirm(key) => {
            let handle = app.clone();
            app.dialog()
                .message(t(lang, key))
                .title("ReTV Bridge")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    t(lang, "quit.yes"),
                    t(lang, "quit.cancel"),
                ))
                .show(move |yes| {
                    if yes {
                        quit(handle);
                    }
                });
        }
    }
}

/// Stops the bridge (closing its TVs), then the private adb server, then exits.
pub fn quit(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        stop_bridge(&app).await;
        app.exit(0);
    });
}

/// How long each step of `stop_bridge` may take: quitting never hangs.
const STOP_STEP: Duration = Duration::from_secs(3);

/// Stops the bridge (closing its TVs) and the private adb server.
pub async fn stop_bridge(app: &AppHandle) {
    let (server, adb) = {
        let st = shared(app);
        let mut s = lock(&st);
        // First, so nothing starts the adb server again while it goes.
        if let Some(revive) = &s.adb_revive {
            revive.stop();
        }
        (s.server.take(), s.adb_proc.take())
    };
    if let Some(server) = server {
        let _ = tokio::time::timeout(STOP_STEP, server.shutdown()).await;
    }
    if let Some(adb) = adb {
        let _ = tokio::time::timeout(STOP_STEP, adb.stop()).await;
    }
}

/// Windows: the first time the window is closed, one balloon says the bridge
/// is still in the notification area.
#[cfg(windows)]
pub fn first_hide_hint(app: &AppHandle) {
    use tauri_plugin_notification::NotificationExt;
    let store = app.state::<SettingsStore>();
    let mut settings = store.settings.lock().unwrap_or_else(|e| e.into_inner());
    if settings.close_hint_shown {
        return;
    }
    settings.close_hint_shown = true;
    let _ = settings.save(&store.dir);
    let lang = lock(&shared(app)).locale;
    let _ = app
        .notification()
        .builder()
        .title("ReTV Bridge")
        .body(t(lang, "notify.hidden"))
        .show();
}
