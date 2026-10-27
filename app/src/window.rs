//! The window: its commands for `ui/app.js`, showing and hiding it, and
//! opening the site.

use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

use crate::adb_setup::{self, Source};
use crate::i18n::{self, Lang, Strings};
use crate::state::{format_code, AdbSetupState, AppState, View};
use crate::{browser, lock, shared, site};

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
}

/// What the window polls (every 700 ms): no strings.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateView {
    view: View,
    view_seq: u64,
    adb: AdbView,
    notices: Vec<&'static str>,
    /// The live code, `482 917`, or none.
    code: Option<String>,
    tv: String,
    tv_connected: bool,
    log_len: usize,
}

pub fn ui_strings(s: &AppState) -> UiStrings {
    UiStrings {
        lang: s.locale,
        strings: i18n::strings(s.locale),
        windows: cfg!(windows),
    }
}

pub fn state_view(s: &AppState) -> StateView {
    let (done, total) = match &s.adb {
        AdbSetupState::Downloading { done, total } => (*done, *total),
        _ => (0, 0),
    };
    StateView {
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
        log_len: s.log.len(),
    }
}

#[tauri::command]
pub fn get_strings(app: AppHandle) -> UiStrings {
    ui_strings(&lock(&shared(&app)))
}

#[tauri::command]
pub fn get_state(app: AppHandle) -> StateView {
    state_view(&lock(&shared(&app)))
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
    if let Some(w) = main_window(app) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Closing the window only hides it; on Windows the first time says where
/// the bridge went.
pub fn on_close(app: &AppHandle, window: &tauri::Window) {
    let _ = window.hide();
    #[cfg(windows)]
    crate::tray::first_hide_hint(app);
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    #[test]
    fn strings_go_once_not_with_every_poll() {
        let s = AppState::new("T".into(), Lang::Ru);
        let polled = serde_json::to_value(state_view(&s)).unwrap();
        assert!(
            polled.get("strings").is_none(),
            "the 700 ms poll carries no strings"
        );
        assert_eq!(polled["adb"]["kind"], "needsConsent");
        let once = serde_json::to_value(ui_strings(&s)).unwrap();
        assert_eq!(once["lang"], "ru");
        assert_eq!(once["strings"]["title"], "ReTV Bridge работает");
        assert_eq!(once["windows"], cfg!(windows));
    }
}
