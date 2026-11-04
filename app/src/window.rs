//! The window: its commands for `ui/app.js`, showing and hiding it, and
//! opening the site.

use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

use crate::account::{self, AccountUi};
use crate::adb_setup::{self, Source};
use crate::i18n::{self, Lang, Strings};
use crate::state::{format_code, start_view, AdbSetupState, AppState, View, TV_KINDS};
use crate::tray::SettingsStore;
use crate::{browser, lock, shared, site, tray};

pub const MAIN: &str = "main";
const TERMS_URL: &str = "https://developer.android.com/studio/terms";
/// A deep link right after the start sequence opened the site does not open it twice.
const REOPEN_AFTER: Duration = Duration::from_secs(3);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdbView {
    kind: &'static str,
    done: u64,
    total: u64,
    size_mb: u64,
}

/// What the window reads once: its language and strings.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiStrings {
    lang: Lang,
    strings: &'static Strings,
    windows: bool,
    /// The language menu's choice: `system`, `ru`, `uk` or `en`.
    lang_choice: &'static str,
}

/// What the window polls (every 700 ms): no strings.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateView {
    /// The language now: the window reads its strings again when it changed (the tray's
    /// «Язык» menu).
    lang: Lang,
    view: View,
    view_seq: u64,
    adb: AdbView,
    notices: Vec<&'static str>,
    /// The live code, `482 917`, or none.
    code: Option<String>,
    tv: String,
    tv_connected: bool,
    /// The connected Android TVs' models.
    tv_models: Vec<String>,
    /// The bridge server runs (not: its port was taken).
    running: bool,
    /// The first run's answer, none until the person picks.
    tv_kind: Option<String>,
    log_len: usize,
    /// «Войти в ReTV»: the sign-in sheet, the header's account, its TVs.
    account: Option<AccountUi>,
}

pub fn ui_strings(s: &AppState, lang_choice: &'static str) -> UiStrings {
    UiStrings {
        lang: s.locale,
        strings: i18n::strings(s.locale),
        windows: cfg!(windows),
        lang_choice,
    }
}

fn saved_lang_choice(app: &AppHandle) -> &'static str {
    let saved = app.try_state::<SettingsStore>().and_then(|store| {
        store
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lang
            .clone()
    });
    tray::lang_choice(saved.as_deref())
}

/// The language menu (window or tray): kept in the settings, the window and the tray follow.
pub fn set_language(app: &AppHandle, choice: &str) {
    if !i18n::valid_choice(choice) {
        return;
    }
    if let Some(store) = app.try_state::<SettingsStore>() {
        let mut settings = store.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings.lang = (choice != "system").then(|| choice.to_string());
        if let Err(e) = settings.save(&store.dir) {
            lock(&shared(app)).push_log(format!("[bridge] could not save settings: {e}"));
        }
    }
    let lang = i18n::pick(Some(choice), Lang::detect());
    {
        let st = shared(app);
        let mut s = lock(&st);
        s.locale = lang;
        s.push_log(format!("[bridge] language: {choice} ({})", lang.code()));
    }
    // The window sees the new `lang` in its next poll and reads its strings again.
    tray::relabel(app, tray::lang_choice(Some(choice)));
}

#[tauri::command]
pub fn set_lang(app: AppHandle, choice: String) -> UiStrings {
    set_language(&app, &choice);
    get_strings(app)
}

pub fn state_view(s: &AppState, tv_kind: Option<String>, account: Option<AccountUi>) -> StateView {
    let (done, total) = match &s.adb {
        AdbSetupState::Downloading { done, total } => (*done, *total),
        _ => (0, 0),
    };
    StateView {
        lang: s.locale,
        view: s.view,
        view_seq: s.view_seq,
        adb: AdbView {
            kind: s.adb_kind(),
            done,
            total,
            size_mb: adb_setup::size_mb(),
        },
        notices: s.notice_keys(),
        code: s
            .server
            .as_ref()
            .and_then(|srv| srv.live_code())
            .map(|(c, _)| format_code(&c)),
        tv: s.tv_status(),
        tv_connected: !s.tvs.is_empty(),
        tv_models: s.tvs.values().cloned().collect(),
        running: s.server.is_some(),
        tv_kind,
        log_len: s.log.len(),
        account,
    }
}

#[tauri::command]
pub fn get_strings(app: AppHandle) -> UiStrings {
    let choice = saved_lang_choice(&app);
    ui_strings(&lock(&shared(&app)), choice)
}

#[tauri::command]
pub fn get_state(app: AppHandle) -> StateView {
    let kind = tv_kind(&app);
    // Before the state lock: the account's own lock is never taken inside it.
    let account = account::ui_of(&app);
    state_view(&lock(&shared(&app)), kind, account)
}

#[tauri::command]
pub fn accept_adb_download(app: AppHandle) {
    adb_setup::begin(&app, Source::Download);
}

#[tauri::command]
pub fn retry_adb(app: AppHandle) {
    adb_setup::retry(&app);
}

#[tauri::command]
pub fn pick_adb_zip(app: AppHandle) {
    let handle = app.clone();
    app.dialog()
        .file()
        .add_filter("platform-tools", &["zip"])
        .pick_file(move |picked| {
            if let Some(path) = picked.and_then(|p| p.into_path().ok()) {
                adb_setup::begin(&handle, Source::LocalZip(path));
            }
        });
}

#[tauri::command]
pub fn open_site(app: AppHandle) {
    open_site_now(&app);
}

/// The command log, oldest first.
#[tauri::command]
pub fn show_log(app: AppHandle) -> Vec<String> {
    let st = shared(&app);
    let s = lock(&st);
    s.log.iter().cloned().collect()
}

/// «Условия Google»: always this one page, in the browser.
#[tauri::command]
pub fn open_terms(app: AppHandle) {
    if let Err(e) = browser::open_url(&app, TERMS_URL) {
        lock(&shared(&app)).push_log(format!("[bridge] could not open the browser: {e}"));
    }
}

/// The site for this run: the locale's site, or in debug builds an allowed
/// `RETV_SITE_URL` (the local dev server).
pub fn site_url(lang: Lang) -> &'static str {
    if cfg!(debug_assertions) {
        if let Some(dev) = site::dev_override(std::env::var("RETV_SITE_URL").ok().as_deref()) {
            return dev;
        }
    }
    site::site_for(lang.code())
}

/// Opens the site with the token, now.
pub fn open_site_now(app: &AppHandle) {
    let st = shared(app);
    let link = {
        let mut s = lock(&st);
        if s.server.is_none() {
            // No bridge to talk to (the port is busy): the window says why.
            drop(s);
            show(app, View::Code);
            return;
        }
        s.site_opened_at = Some(Instant::now());
        let site = site_url(s.locale);
        s.push_log(format!("[bridge] opening {site}"));
        site::connect_link(site, &s.token)
    };
    if let Err(e) = browser::open_url(app, &link) {
        lock(&st).push_log(format!("[bridge] could not open the browser: {e}"));
        show(app, View::Code);
    }
}

/// The start sequence's single opening of the site.
pub fn open_site_once(app: &AppHandle) {
    if lock(&shared(app)).site_opened_at.is_none() {
        open_site_now(app);
    }
}

/// `retv-bridge://open`: shows the window and opens the site, unless the start
/// sequence has just done so. A burst of links (a double click on the site's
/// button) is handled once.
pub fn on_deep_link(app: &AppHandle, url: &str) {
    let Some(rest) = url.strip_prefix("retv-bridge://") else {
        return;
    };
    if rest.trim_end_matches('/') != "open" {
        return;
    }
    if !lock(&shared(app)).deep_link_due(Instant::now()) {
        return;
    }
    lock(&shared(app)).push_log("[bridge] retv-bridge://open".into());
    show(app, View::Code);
    let recent = lock(&shared(app))
        .site_opened_at
        .is_some_and(|t| t.elapsed() < REOPEN_AFTER);
    let adb_ready = matches!(lock(&shared(app)).adb, AdbSetupState::Ready);
    if !recent && adb_ready {
        open_site_now(app);
    }
}

pub fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(MAIN)
}

/// Shows and focuses the window on `view`.
pub fn show(app: &AppHandle, view: View) {
    lock(&shared(app)).request_view(view);
    reveal(app);
}

/// Shows and focuses the window on the tab it last had («Открыть окно
/// Bridge»). The window exists only while it is open: it is made here and
/// destroyed on close, so the WebView's memory is freed while Bridge sits in
/// the tray. Everything it shows lives in Rust, so a new one is up at once.
pub fn reveal(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = main_window(&handle) {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            return;
        }
        let built = WebviewWindowBuilder::new(&handle, MAIN, WebviewUrl::App("index.html".into()))
            .title("ReTV Bridge")
            .inner_size(WIDTH, HEIGHT)
            .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
            .resizable(true)
            .maximizable(false)
            .center()
            .focused(true)
            .build();
        match built {
            Ok(w) => {
                let _ = w.set_focus();
            }
            Err(e) => {
                lock(&shared(&handle)).push_log(format!("[bridge] could not open the window: {e}"));
            }
        }
    });
}

/// The window: 460 × 680, resizable down to 420 × 600.
pub const WIDTH: f64 = 460.0;
pub const HEIGHT: f64 = 680.0;
pub const MIN_WIDTH: f64 = 420.0;
pub const MIN_HEIGHT: f64 = 600.0;

/// Closing the window destroys it (Bridge stays in the tray); on Windows the
/// first time says where the bridge went.
pub fn on_close(app: &AppHandle) {
    #[cfg(windows)]
    crate::tray::first_hide_hint(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// The window's own tab clicks: kept here, so a window made again opens on
/// the same tab. No `view_seq` bump: that is for the tray's requests.
#[tauri::command]
pub fn set_view(app: AppHandle, view: String) {
    let v = match view.as_str() {
        "code" => View::Code,
        "samsung" => View::Samsung,
        "log" => View::Log,
        _ => return,
    };
    lock(&shared(&app)).view = v;
}

/// The first run's «Какой у вас телевизор?»: saved; Samsung and «Не знаю» go
/// to the Samsung tab.
#[tauri::command]
pub fn set_tv_kind(app: AppHandle, kind: String) {
    if !TV_KINDS.contains(&kind.as_str()) {
        return;
    }
    {
        let store = app.state::<SettingsStore>();
        let mut settings = store.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings.tv_kind = Some(kind.clone());
        if let Err(e) = settings.save(&store.dir) {
            lock(&shared(&app)).push_log(format!("[bridge] could not save settings: {e}"));
        }
    }
    lock(&shared(&app)).request_view(start_view(Some(&kind)));
}

/// «Перезапустить Bridge» when its port was taken.
#[tauri::command]
pub fn restart_bridge(app: AppHandle) {
    if lock(&shared(&app)).server.is_some() {
        return;
    }
    lock(&shared(&app)).push_log("[bridge] restarting (the person asked)".into());
    app.restart();
}

pub fn tv_kind(app: &AppHandle) -> Option<String> {
    app.try_state::<SettingsStore>().and_then(|store| {
        store
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tv_kind
            .clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    #[test]
    fn strings_go_once_not_with_every_poll() {
        let s = AppState::new("T".into(), Lang::Ru);
        let polled = serde_json::to_value(state_view(&s, None, None)).unwrap();
        assert!(
            polled.get("strings").is_none(),
            "the 700 ms poll carries no strings"
        );
        assert_eq!(polled["adb"]["kind"], "needsConsent");
        assert!(polled["tvKind"].is_null());
        assert_eq!(polled["running"], false);
        assert_eq!(polled["view"], "code");
        let polled = serde_json::to_value(state_view(&s, Some("samsung".into()), None)).unwrap();
        assert_eq!(polled["tvKind"], "samsung");
        // The poll carries the language, so a change in the tray reaches an open window.
        assert_eq!(polled["lang"], "ru");
        let once = serde_json::to_value(ui_strings(&s, "ru")).unwrap();
        assert_eq!(once["lang"], "ru");
        assert_eq!(once["langChoice"], "ru");
        assert_eq!(once["strings"]["title"], "ReTV Bridge работает");
        assert_eq!(once["windows"], cfg!(windows));
        assert_eq!(tray::lang_choice(None), "system");
        assert_eq!(tray::lang_choice(Some("uk")), "uk");
        assert_eq!(tray::lang_choice(Some("de")), "system");
    }
}
