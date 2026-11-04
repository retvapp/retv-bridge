//! «Войти в ReTV»: the window's and the tray's side of the app sign-in
//! (`retv-bridge-account`). The sign-in page opens in the browser; the window
//! shows the code as a fallback; tokens live in the OS key store and never
//! reach the window or the log.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use retv_bridge_account::Cell;
use retv_bridge_account::{
    os_store, Account, CodeRequest, Endpoints, Phase, ReTvSites, SignInEnd, Timing, Tv,
};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::i18n::Lang;
use crate::tray::SettingsStore;
use crate::{browser, lock, shared, tray};

/// The account's TVs are read again at most this often (the window asks
/// when the Android TV tab is shown).
const TVS_EVERY: Duration = Duration::from_secs(60);

pub struct AccountShared {
    pub account: Arc<Account>,
    /// «Аккаунт на»: where the next sign-in goes.
    choice: Mutex<Cell>,
    tvs_read_at: Mutex<Option<Instant>>,
}

/// A development API (debug builds only): `RETV_ACCOUNT_API` on this
/// computer, the page checked against `RETV_SITE_URL` (an allowed dev origin).
struct DevSites {
    api: String,
    site: String,
}

impl Endpoints for DevSites {
    fn api(&self, _: Cell) -> String {
        self.api.clone()
    }
    fn site(&self, _: Cell) -> String {
        self.site.clone()
    }
}

/// `value` when it is an API on this computer (`http://127.0.0.1:…/v1`).
pub fn dev_api(value: Option<&str>) -> Option<String> {
    let v = value?.trim().trim_end_matches('/');
    let local = ["http://127.0.0.1:", "http://localhost:"]
        .iter()
        .any(|p| v.starts_with(p));
    (local && !v.contains('@')).then(|| v.to_string())
}

fn endpoints() -> Box<dyn Endpoints> {
    if cfg!(debug_assertions) {
        if let Some(api) = dev_api(std::env::var("RETV_ACCOUNT_API").ok().as_deref()) {
            let site = crate::site::dev_override(std::env::var("RETV_SITE_URL").ok().as_deref())
                .map(str::to_string)
                .unwrap_or_else(|| api.trim_end_matches("/v1").to_string());
            return Box::new(DevSites { api, site });
        }
    }
    Box::new(ReTvSites)
}

/// The cell for this computer: the person's earlier pick, else ReTV Home's
/// rule (language and time zone).
pub fn first_cell(lang: Lang, saved: Option<&str>) -> Cell {
    saved.and_then(Cell::parse).unwrap_or_else(|| {
        Cell::pick(
            lang.code(),
            &iana_time_zone::get_timezone().unwrap_or_default(),
        )
    })
}

pub fn init(app: &AppHandle, lang: Lang) -> AccountShared {
    let saved = app.try_state::<SettingsStore>().and_then(|store| {
        store
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .account_cell
            .clone()
    });
    let handle = app.clone();
    let account = Account::new(
        endpoints(),
        os_store(),
        Timing::default(),
        Box::new(move |line| lock(&shared(&handle)).push_log(line)),
    );
    AccountShared {
        account: Arc::new(account),
        choice: Mutex::new(first_cell(lang, saved.as_deref())),
        tvs_read_at: Mutex::new(None),
    }
}

/// No control or format characters, at most `max` characters.
pub fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'))
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// The computer's name as the person set it («MacBook Pro Паши»): the site
/// shows it on «Разрешить» and in the list of signed-in apps.
fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    let raw = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    #[cfg(windows)]
    let raw = std::env::var("COMPUTERNAME").unwrap_or_default();
    #[cfg(not(any(target_os = "macos", windows)))]
    let raw = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let name = clean(&raw, 80);
    if name.is_empty() {
        "ReTV Bridge".into()
    } else {
        name
    }
}

/// «macOS 15», «Windows 11».
pub fn platform_line(os: &str, version: &str, edition: Option<&str>) -> String {
    let major = version.split('.').next().unwrap_or("");
    let line = match os {
        "macos" => format!("macOS {major}"),
        "windows" => edition
            .map(|e| e.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
            .filter(|e| e.starts_with("Windows"))
            .unwrap_or_else(|| "Windows".into()),
        other => format!("{other} {major}"),
    };
    clean(&line, 40)
}

fn platform() -> String {
    let info = os_info::get();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "Linux"
    };
    platform_line(os, &info.version().to_string(), info.edition())
}

fn code_request() -> CodeRequest {
    CodeRequest {
        app: "bridge",
        name: computer_name(),
        platform: platform(),
        app_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// `ABCD2345` → `ABCD-2345`, as the site writes it.
pub fn show_code(code: &str) -> String {
    if code.len() == 8 && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

/// What the window shows (in the 700 ms poll): no tokens.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUi {
    pub phase: Phase,
    /// The code on the sheet, `ABCD-2345`.
    pub code: Option<String>,
    /// Where the code can be typed, `retv.app/go`.
    pub go: Option<String>,
    /// «Аккаунт на»: the next sign-in's site (`world`, `ru`) and host.
    pub choice: Cell,
    pub choice_host: &'static str,
    pub signed_in: bool,
    /// The live sign-in's host.
    pub host: Option<&'static str>,
    pub label: Option<String>,
    pub tier: Option<String>,
    pub pro: bool,
    pub tvs: Option<Vec<Tv>>,
    pub offline: bool,
}

impl AccountUi {
    pub fn signed_out(choice: Cell) -> AccountUi {
        AccountUi {
            phase: Phase::Idle,
            code: None,
            go: None,
            choice,
            choice_host: choice.host(),
            signed_in: false,
            host: None,
            label: None,
            tier: None,
            pro: false,
            tvs: None,
            offline: false,
        }
    }
}

pub fn ui(a: &AccountShared) -> AccountUi {
    let snap = a.account.snapshot();
    let choice = *a.choice.lock().unwrap_or_else(|e| e.into_inner());
    let (code, go) = match &snap.phase {
        Phase::Waiting {
            user_code,
            verification_uri,
            ..
        } => (
            Some(show_code(user_code)),
            Some(
                verification_uri
                    .split_once("://")
                    .map_or(verification_uri.as_str(), |(_, rest)| rest)
                    .to_string(),
            ),
        ),
        _ => (None, None),
    };
    let account = snap.account.clone();
    AccountUi {
        phase: snap.phase,
        code,
        go,
        choice,
        choice_host: choice.host(),
        signed_in: account.is_some(),
        host: snap.cell.map(Cell::host),
        label: account.as_ref().and_then(|v| v.label.clone()),
        tier: account.as_ref().map(|v| v.tier.clone()),
        pro: account.as_ref().is_some_and(|v| v.pro),
        tvs: snap.tvs,
        offline: snap.offline,
    }
}

/// The window's view of the account, or none before `init`.
pub fn ui_of(app: &AppHandle) -> Option<AccountUi> {
    app.try_state::<AccountShared>().map(|a| ui(&a))
}

pub fn signed_in(app: &AppHandle) -> bool {
    app.try_state::<AccountShared>()
        .is_some_and(|a| a.account.signed_in())
}

fn busy(phase: &Phase) -> bool {
    matches!(phase, Phase::Starting { .. } | Phase::Waiting { .. })
}

/// Starts a sign-in on the chosen site (nothing if one runs or the person
/// is signed in already).
pub fn start(app: &AppHandle) {
    let Some(a) = app.try_state::<AccountShared>() else {
        return;
    };
    if a.account.signed_in() || busy(&a.account.snapshot().phase) {
        return;
    }
    let cell = *a.choice.lock().unwrap_or_else(|e| e.into_inner());
    let account = a.account.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let req = code_request();
        let opener = handle.clone();
        let end = account
            .sign_in(cell, req, move |url| open(&opener, url))
            .await;
        if let SignInEnd::SignedIn(_) = end {
            read_tvs(&handle, true).await;
        }
        tray::refresh_status(&handle);
    });
}

fn open(app: &AppHandle, url: &str) {
    if let Err(e) = browser::open_url(app, url) {
        lock(&shared(app)).push_log(format!("[account] could not open the browser: {e}"));
    }
}

async fn read_tvs(app: &AppHandle, now: bool) {
    let Some(a) = app.try_state::<AccountShared>() else {
        return;
    };
    {
        let mut at = a.tvs_read_at.lock().unwrap_or_else(|e| e.into_inner());
        if !now && at.is_some_and(|t| t.elapsed() < TVS_EVERY) {
            return;
        }
        *at = Some(Instant::now());
    }
    let account = a.account.clone();
    let _ = account.refresh_tvs().await;
    tray::refresh_status(app);
}

/// At start: the sign-in of an earlier run, then the account and its TVs.
pub fn restore(app: &AppHandle) {
    let Some(a) = app.try_state::<AccountShared>() else {
        return;
    };
    if !a.account.restore() {
        return;
    }
    let account = a.account.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = account.refresh_account().await;
        read_tvs(&handle, true).await;
    });
}

/// «Выйти» (window or tray).
pub fn sign_out(app: &AppHandle) {
    let Some(a) = app.try_state::<AccountShared>() else {
        return;
    };
    let account = a.account.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        account.sign_out().await;
        tray::refresh_status(&handle);
    });
}

#[tauri::command]
pub fn account_sign_in(app: AppHandle) {
    start(&app);
}

#[tauri::command]
pub fn account_cancel(app: AppHandle) {
    if let Some(a) = app.try_state::<AccountShared>() {
        a.account.cancel();
    }
}

/// «Открыть сайт снова».
#[tauri::command]
pub fn account_reopen(app: AppHandle) {
    let Some(a) = app.try_state::<AccountShared>() else {
        return;
    };
    if let Phase::Waiting {
        verification_uri_complete,
        ..
    } = a.account.snapshot().phase
    {
        open(&app, &verification_uri_complete);
    }
}

#[tauri::command]
pub fn account_dismiss(app: AppHandle) {
    if let Some(a) = app.try_state::<AccountShared>() {
        a.account.dismiss();
    }
}

#[tauri::command]
pub fn account_sign_out(app: AppHandle) {
    sign_out(&app);
}

/// «Аккаунт на»: saved; a sign-in that is running starts again there.
#[tauri::command]
pub fn account_set_cell(app: AppHandle, cell: String) {
    let Some(cell) = Cell::parse(&cell) else {
        return;
    };
    let Some(a) = app.try_state::<AccountShared>() else {
        return;
    };
    if a.account.signed_in() {
        return;
    }
    *a.choice.lock().unwrap_or_else(|e| e.into_inner()) = cell;
    if let Some(store) = app.try_state::<SettingsStore>() {
        let mut settings = store.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings.account_cell = Some(cell.code().into());
        if let Err(e) = settings.save(&store.dir) {
            lock(&shared(&app)).push_log(format!("[bridge] could not save settings: {e}"));
        }
    }
    let phase = a.account.snapshot().phase;
    if busy(&phase) || matches!(phase, Phase::Failed { .. }) {
        a.account.cancel();
        start(&app);
    }
}

/// The Android TV tab is shown: the account's TVs, if not read lately.
#[tauri::command]
pub fn account_tvs(app: AppHandle) {
    if !signed_in(&app) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move { read_tvs(&handle, false).await });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_reads_like_the_site_writes_it() {
        assert_eq!(show_code("ABCD2345"), "ABCD-2345");
        assert_eq!(show_code("ABC"), "ABC");
    }

    #[test]
    fn platform_lines() {
        assert_eq!(platform_line("macos", "15.1.0", None), "macOS 15");
        assert_eq!(
            platform_line("windows", "10.0.22631", Some("Windows 11 Pro")),
            "Windows 11"
        );
        assert_eq!(platform_line("windows", "10.0.19045", None), "Windows");
        assert_eq!(platform_line("Linux", "6.8", None), "Linux 6");
    }

    #[test]
    fn names_lose_control_and_bidi_characters() {
        assert_eq!(clean("  Mac\u{202e}Book\n", 80), "MacBook");
        assert_eq!(clean("Паша\u{200b}", 80), "Паша");
        assert_eq!(clean(&"x".repeat(100), 80).len(), 80);
    }

    #[test]
    fn the_dev_api_is_only_on_this_computer() {
        assert_eq!(
            dev_api(Some("http://127.0.0.1:8787/v1/")).as_deref(),
            Some("http://127.0.0.1:8787/v1")
        );
        assert!(dev_api(Some("http://localhost:8787/v1")).is_some());
        assert_eq!(dev_api(Some("https://retv.app/v1")), None);
        assert_eq!(dev_api(Some("http://127.0.0.1.evil.example/v1")), None);
        assert_eq!(dev_api(Some("http://localhost:1@evil.example/v1")), None);
        assert_eq!(dev_api(None), None);
    }

    #[test]
    fn the_saved_pick_wins_over_the_rule() {
        assert_eq!(first_cell(Lang::En, Some("ru")), Cell::Ru);
        assert_eq!(first_cell(Lang::Ru, Some("world")), Cell::World);
        assert_eq!(first_cell(Lang::Uk, Some("junk")), Cell::World);
    }

    #[test]
    fn the_window_view_carries_no_token() {
        let ui = AccountUi::signed_out(Cell::World);
        let json = serde_json::to_string(&ui).unwrap();
        assert!(json.contains("\"phase\":{\"kind\":\"idle\"}"), "{json}");
        assert!(json.contains("\"choiceHost\":\"retv.app\""));
        assert!(!json.to_lowercase().contains("token"));
    }
}
