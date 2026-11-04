//! The tray (menu bar) icon and its menu, quitting, and "open at login".
//!
//! ```text
//! ReTV Bridge работает                 (status)
//! ТВ: MiTV-AFMU0, подключён            (status)
//! ──
//! Открыть сайт ReTV
//! Настроить ТВ Samsung…                → the window, Samsung tab
//! Код для сайта: 482 917               → copies it («Показать код для сайта» when none)
//! ──
//! Войти в ReTV…                        → the window's sign-in sheet («Выйти из аккаунта ReTV» when signed in)
//! Открыть окно Bridge
//! Журнал
//! Запускать при входе в систему ✓
//! Язык ▸ Как в системе / Русский / Українська / English
//! Проверить обновления
//! ──
//! Выйти из ReTV Bridge
//! ```

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;

use crate::i18n::{t, tf, Lang, LANG_CHOICES};
use crate::state::{
    format_code, quit_decision, set_autostart, startup_autostart, AppState, AutostartCtl,
    QuitDecision, Settings, View,
};
use crate::{account, clipboard, lock, samsung, shared, updates, window};

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-template.png");
/// How often the status lines and the code item are brought up to date.
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Menu items that change while the app runs, and the texts they show.
pub struct TrayItems {
    running: MenuItem<Wry>,
    status: MenuItem<Wry>,
    code: MenuItem<Wry>,
    account: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    updates: MenuItem<Wry>,
    shown: Mutex<TrayTexts>,
    /// Items whose text is one string: written again when the language changes.
    fixed: Vec<(MenuItem<Wry>, &'static str)>,
    lang_menu: Submenu<Wry>,
    /// «Как в системе», Русский, Українська, English (`LANG_CHOICES`).
    langs: Vec<CheckMenuItem<Wry>>,
}

/// The language menu's choice for the saved setting.
pub fn lang_choice(saved: Option<&str>) -> &'static str {
    LANG_CHOICES
        .into_iter()
        .find(|c| Some(*c) == saved)
        .unwrap_or("system")
}

/// What the changing items say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayTexts {
    pub running: String,
    pub status: String,
    pub code: String,
    pub account: String,
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

/// The site code to show in the menu, `482 917`, if one is live.
fn live_code(s: &AppState) -> Option<String> {
    s.server
        .as_ref()
        .and_then(|srv| srv.live_code())
        .map(|(c, _)| format_code(&c))
}

/// The texts of the changing items. `samsung_tv`: a Samsung TV this run can
/// talk to, shown when no Android TV is connected. `signed_in`: to ReTV.
pub fn tray_texts(s: &AppState, samsung_tv: Option<&str>, signed_in: bool) -> TrayTexts {
    let lang = s.locale;
    let running = t(
        lang,
        if s.server.is_some() {
            "title"
        } else {
            "title.off"
        },
    );
    let status = match samsung_tv {
        Some(name) if s.tvs.is_empty() => tf(lang, "tray.tvSamsung", &[("name", name)]),
        _ => s.tv_status(),
    };
    let code = match live_code(s) {
        Some(code) => tf(lang, "tray.code", &[("code", &code)]),
        None => t(lang, "tray.showCode"),
    };
    let account = t(
        lang,
        if signed_in {
            "tray.signOut"
        } else {
            "tray.signIn"
        },
    );
    TrayTexts {
        running,
        status,
        code,
        account,
    }
}

pub fn build(app: &AppHandle, lang: Lang) -> tauri::Result<()> {
    let store = app.state::<SettingsStore>();
    let (autostart_on, choice) = {
        let settings = store.settings.lock().unwrap_or_else(|e| e.into_inner());
        (
            startup_autostart(&settings, &Launcher(app)),
            lang_choice(settings.lang.as_deref()),
        )
    };

    let signed_in = account::signed_in(app);
    let texts = tray_texts(&lock(&shared(app)), None, signed_in);
    let item = |id: &str, key: &str| MenuItem::with_id(app, id, t(lang, key), true, None::<&str>);
    let running = MenuItem::with_id(app, "running", &texts.running, false, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", &texts.status, false, None::<&str>)?;
    let open = item("open", "tray.open")?;
    let samsung = item("samsung", "tray.samsung")?;
    let code = MenuItem::with_id(app, "code", &texts.code, true, None::<&str>)?;
    let account_item = MenuItem::with_id(app, "account", &texts.account, true, None::<&str>)?;
    let window_item = item("window", "tray.window")?;
    let log = item("log", "tray.log")?;
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
    let quit = item("quit", "tray.quit")?;
    // «Язык»: the window and this menu; the choice is kept in the settings.
    let langs = LANG_CHOICES
        .iter()
        .map(|c| {
            CheckMenuItem::with_id(
                app,
                format!("lang:{c}"),
                t(lang, &format!("lang.{c}")),
                true,
                *c == choice,
                None::<&str>,
            )
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let lang_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = langs
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<Wry>)
        .collect();
    let lang_menu = Submenu::with_items(app, t(lang, "lang.label"), true, &lang_refs)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(
        app,
        &[
            &running,
            &status,
            &sep()?,
            &open,
            &samsung,
            &code,
            &sep()?,
            &account_item,
            &window_item,
            &log,
            &autostart,
            &lang_menu,
            &updates,
            &sep()?,
            &quit,
        ],
    )?;
    app.manage(TrayItems {
        running,
        status,
        code,
        account: account_item,
        autostart: autostart.clone(),
        updates: updates.clone(),
        shown: Mutex::new(texts),
        fixed: vec![
            (open, "tray.open"),
            (samsung, "tray.samsung"),
            (window_item, "tray.window"),
            (log, "tray.log"),
            (quit, "tray.quit"),
        ],
        lang_menu,
        langs,
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
            "samsung" => window::show(app, View::Samsung),
            "code" => copy_code(app),
            "account" => on_account_item(app),
            "window" => window::reveal(app),
            "log" => window::show(app, View::Log),
            "autostart" => toggle_autostart(app),
            "updates" => updates::on_tray_click(app),
            "quit" => request_quit(app),
            id => {
                if let Some(code) = id.strip_prefix("lang:") {
                    window::set_language(app, code);
                }
            }
        })
        .build(app)?;

    // The code expires and TVs come and go: keep the lines current.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(REFRESH_EVERY).await;
            refresh_status(&handle);
        }
    });
    Ok(())
}

/// «Войти в ReTV…» opens the window's sign-in sheet; «Выйти из аккаунта
/// ReTV» signs out.
fn on_account_item(app: &AppHandle) {
    if account::signed_in(app) {
        account::sign_out(app);
    } else {
        window::show(app, View::Code);
        account::start(app);
    }
}

/// «Код для сайта: 482 917»: copies the code (a notification says so); with
/// no code, or no clipboard, shows it in the window.
fn copy_code(app: &AppHandle) {
    let (code, lang) = {
        let st = shared(app);
        let s = lock(&st);
        (live_code(&s), s.locale)
    };
    let Some(code) = code else {
        window::show(app, View::Code);
        return;
    };
    match clipboard::copy(&code.replace(' ', "")) {
        Ok(()) => {
            let _ = app
                .notification()
                .builder()
                .title("ReTV Bridge")
                .body(tf(lang, "notify.copied", &[("code", &code)]))
                .show();
        }
        Err(e) => {
            lock(&shared(app)).push_log(format!("[bridge] could not copy the code: {e}"));
            window::show(app, View::Code);
        }
    }
}

/// Puts the status lines and the code item in the menu up to date (only the
/// ones that changed, so an open menu does not flicker).
pub fn refresh_status(app: &AppHandle) {
    let samsung_tv = samsung::ready_tv(app);
    let signed_in = account::signed_in(app);
    let texts = tray_texts(&lock(&shared(app)), samsung_tv.as_deref(), signed_in);
    let Some(items) = app.try_state::<TrayItems>() else {
        return;
    };
    let mut shown = items.shown.lock().unwrap_or_else(|e| e.into_inner());
    if shown.running != texts.running {
        let _ = items.running.set_text(&texts.running);
    }
    if shown.status != texts.status {
        let _ = items.status.set_text(&texts.status);
    }
    if shown.code != texts.code {
        let _ = items.code.set_text(&texts.code);
    }
    if shown.account != texts.account {
        let _ = items.account.set_text(&texts.account);
    }
    *shown = texts;
}

/// After a language change: every item in the new language, `choice` ticked.
pub fn relabel(app: &AppHandle, choice: &str) {
    let lang = lock(&shared(app)).locale;
    if let Some(items) = app.try_state::<TrayItems>() {
        for (item, key) in &items.fixed {
            let _ = item.set_text(t(lang, key));
        }
        let _ = items.autostart.set_text(t(lang, "tray.autostart"));
        let _ = items.lang_menu.set_text(t(lang, "lang.label"));
        for (item, c) in items.langs.iter().zip(LANG_CHOICES) {
            let _ = item.set_text(t(lang, &format!("lang.{c}")));
            let _ = item.set_checked(c == choice);
        }
        // The changing lines are written again on the next refresh.
        *items.shown.lock().unwrap_or_else(|e| e.into_inner()) = TrayTexts::default();
    }
    refresh_status(app);
    refresh_updates(app);
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

/// «Выйти из ReTV Bridge»: asks first while a TV is held.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_lines_follow_the_state() {
        let mut s = AppState::new("T".into(), Lang::Ru);
        let x = tray_texts(&s, None, false);
        assert_eq!(x.running, "ReTV Bridge не работает");
        assert_eq!(x.account, "Войти в ReTV…");
        assert_eq!(tray_texts(&s, None, true).account, "Выйти из аккаунта ReTV");
        assert_eq!(x.status, "ТВ не подключён");
        assert_eq!(x.code, "Показать код для сайта");
        let x = tray_texts(&s, Some("Гостиная"), false);
        assert_eq!(x.status, "ТВ: Гостиная (Samsung), готов");
        s.tvs.insert("a:5555".into(), "MiTV-AFMU0".into());
        let x = tray_texts(&s, Some("Гостиная"), false);
        assert_eq!(x.status, "ТВ: MiTV-AFMU0, подключён");
        let en = tray_texts(&AppState::new("T".into(), Lang::En), None, false);
        assert_eq!(en.running, "ReTV Bridge is not running");
    }

    #[test]
    fn tray_items_read_plainly() {
        assert_eq!(t(Lang::Ru, "tray.quit"), "Выйти из ReTV Bridge");
        assert_eq!(t(Lang::Ru, "tray.log"), "Журнал");
        assert_eq!(t(Lang::Ru, "tray.samsung"), "Настроить ТВ Samsung…");
        assert_eq!(
            tf(Lang::Ru, "tray.code", &[("code", "482 917")]),
            "Код для сайта: 482 917"
        );
        assert_eq!(t(Lang::Uk, "tray.window"), "Відкрити вікно Bridge");
    }
}
