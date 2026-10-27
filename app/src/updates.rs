//! Updates through the Tauri updater: our static `retv-bridge.json` (the
//! locale's site first), minisign signatures checked against the public key
//! in `tauri.conf.json`, a check at start and then daily, and never an
//! install while a TV session is open.
//!
//! The rules (endpoints, schedule, tray text, the busy check) are pure and
//! unit-tested; the rest drives the updater plugin.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::i18n::{t, tf, Lang};
use crate::{lock, now_ms, shared, tray};

pub const ENDPOINT_WORLD: &str = "https://retv.app/downloads/retv-bridge.json";
pub const ENDPOINT_RU: &str = "https://ru.retv.app/downloads/retv-bridge.json";

/// What `tauri.conf.json` carries until the owner puts the updater's public
/// key there (docs: apps/retv-bridge/README.md, "Updates"). With it, updates
/// stay off: nothing is checked and the menu item stays disabled.
pub const PUBKEY_PLACEHOLDER: &str = "REPLACE_WITH_THE_RETV_BRIDGE_UPDATER_PUBLIC_KEY";

/// A day between successful checks.
pub const INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
/// The first wait after a failed check; it doubles up to `INTERVAL_MS`.
pub const BACKOFF_BASE_MS: u64 = 15 * 60 * 1000;

/// The start sequence (server, adb, the site) goes first.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
/// How often the background task looks at the schedule.
const TICK: Duration = Duration::from_secs(5 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The manifests to ask, in order: the Russian site first on Russian systems,
/// else the international one. The other is the fallback.
pub fn endpoints_for(lang: Lang) -> Vec<Url> {
    let order = match lang {
        Lang::Ru => [ENDPOINT_RU, ENDPOINT_WORLD],
        Lang::En | Lang::Uk => [ENDPOINT_WORLD, ENDPOINT_RU],
    };
    order
        .iter()
        .map(|u| Url::parse(u).expect("a valid endpoint"))
        .collect()
}

/// When to check: at start, then a day after a success; after failures
/// 15 min, 30 min, 1 h … at most a day (as `UpdateSchedule` in ReTV Home).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateSchedule {
    /// The last check that ran (wall clock, ms); `None` before the first.
    pub last_check_ms: Option<u64>,
    /// Failed checks in a row.
    pub failures: u32,
}

impl UpdateSchedule {
    /// The wait after `failures` failed checks in a row.
    pub fn backoff(failures: u32) -> u64 {
        if failures == 0 {
            return 0;
        }
        let shift = (failures - 1).min(20);
        (BACKOFF_BASE_MS << shift).min(INTERVAL_MS)
    }

    fn next_at(&self, last: u64) -> u64 {
        let wait = if self.failures == 0 {
            INTERVAL_MS
        } else {
            Self::backoff(self.failures)
        };
        last.saturating_add(wait)
    }

    /// Due now, or the clock went back by more than a day.
    pub fn due(&self, now: u64) -> bool {
        match self.last_check_ms {
            None => true,
            Some(last) => {
                let next = self.next_at(last);
                now >= next || next - now > INTERVAL_MS
            }
        }
    }

    pub fn succeeded(&mut self, now: u64) {
        self.last_check_ms = Some(now);
        self.failures = 0;
    }

    pub fn failed(&mut self, now: u64) {
        self.last_check_ms = Some(now);
        self.failures = self.failures.saturating_add(1);
    }
}

/// An update never installs while a socket holds a TV: the install restarts
/// the bridge, which would cut the site's work on the TV.
pub fn can_install(active_sessions: usize) -> Result<(), &'static str> {
    if active_sessions > 0 {
        Err("update.busy")
    } else {
        Ok(())
    }
}

/// The update as the tray shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// This build has no updater key: nothing is checked.
    Off,
    /// «Проверить обновления»: a click checks now.
    Idle,
    Checking,
    /// «Доступна новая версия {v}»: a click installs.
    Available {
        version: String,
    },
    Installing,
}

/// The tray item's text and whether it can be clicked.
pub fn tray_item(status: &UpdateStatus, lang: Lang) -> (String, bool) {
    match status {
        UpdateStatus::Off => (t(lang, "tray.updates"), false),
        UpdateStatus::Idle => (t(lang, "tray.updates"), true),
        UpdateStatus::Checking => (t(lang, "tray.updateChecking"), false),
        UpdateStatus::Available { version } => {
            (tf(lang, "tray.updateAvailable", &[("v", version)]), true)
        }
        UpdateStatus::Installing => (t(lang, "tray.updateInstalling"), false),
    }
}

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Newer(String),
    Latest,
    Failed,
}

/// Whether a check runs from `current`, and what the tray shows while it
/// does. An offer already shown stays clickable while the schedule checks
/// again for a newer one.
pub fn check_starts(current: &UpdateStatus) -> Option<UpdateStatus> {
    match current {
        UpdateStatus::Idle => Some(UpdateStatus::Checking),
        UpdateStatus::Available { .. } => Some(current.clone()),
        UpdateStatus::Off | UpdateStatus::Checking | UpdateStatus::Installing => None,
    }
}

/// The status after a check (`None`: leave it). The newest version found is
/// offered; a failed check keeps an offer; an install in progress is left alone.
pub fn next_status(current: &UpdateStatus, outcome: &CheckOutcome) -> Option<UpdateStatus> {
    if matches!(current, UpdateStatus::Installing | UpdateStatus::Off) {
        return None;
    }
    match outcome {
        CheckOutcome::Newer(v) => Some(UpdateStatus::Available { version: v.clone() }),
        CheckOutcome::Latest => Some(UpdateStatus::Idle),
        CheckOutcome::Failed => match current {
            UpdateStatus::Available { .. } => None,
            _ => Some(UpdateStatus::Idle),
        },
    }
}

/// Whether `pubkey` is a real key rather than the placeholder.
pub fn pubkey_configured(pubkey: &str) -> bool {
    let k = pubkey.trim();
    !k.is_empty() && k != PUBKEY_PLACEHOLDER
}

/// `RETV_BRIDGE_UPDATE_URL` (debug builds only): one http(s) manifest URL
/// that replaces both sites, for a local test server.
#[cfg(debug_assertions)]
pub fn dev_endpoint(value: Option<&str>) -> Option<Url> {
    Url::parse(value?)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
}

pub fn user_agent() -> String {
    format!("ReTV-Bridge/{}", env!("CARGO_PKG_VERSION"))
}

/// The schedule and the update found, kept by the app.
#[derive(Default)]
pub struct Updates {
    schedule: Mutex<UpdateSchedule>,
    found: Mutex<Option<Update>>,
}

fn endpoints(lang: Lang) -> Vec<Url> {
    #[cfg(debug_assertions)]
    {
        if let Some(dev) = dev_endpoint(std::env::var("RETV_BRIDGE_UPDATE_URL").ok().as_deref()) {
            return vec![dev];
        }
    }
    endpoints_for(lang)
}

fn configured_pubkey(app: &AppHandle) -> String {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .unwrap_or_default()
        .to_string()
}

fn set_status(app: &AppHandle, status: UpdateStatus) {
    lock(&shared(app)).update = status;
    tray::refresh_updates(app);
}

fn log(app: &AppHandle, line: String) {
    lock(&shared(app)).push_log(line);
}

fn notify(app: &AppHandle, body: String) {
    let _ = app
        .notification()
        .builder()
        .title("ReTV Bridge")
        .body(body)
        .show();
}

/// Starts the background checks (or turns updates off without a key).
pub fn init(app: &AppHandle) {
    app.manage(Updates::default());
    if !pubkey_configured(&configured_pubkey(app)) {
        log(
            app,
            "[bridge] updates are off: this build has no updater key".into(),
        );
        set_status(app, UpdateStatus::Off);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_AFTER).await;
        loop {
            let due = app
                .state::<Updates>()
                .schedule
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .due(now_ms());
            if due {
                check(&app, false).await;
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// One check. `manual`: the person clicked, so the result is announced.
/// While an update is offered the schedule keeps checking, and a newer one
/// replaces the offer.
pub async fn check(app: &AppHandle, manual: bool) {
    let lang = {
        let st = shared(app);
        let mut s = lock(&st);
        let Some(during) = check_starts(&s.update) else {
            return;
        };
        s.update = during;
        s.locale
    };
    tray::refresh_updates(app);

    let result = async {
        app.updater_builder()
            .endpoints(endpoints(lang))?
            .header("User-Agent", user_agent())?
            .timeout(REQUEST_TIMEOUT)
            .build()?
            .check()
            .await
    }
    .await;

    let now = now_ms();
    let updates = app.state::<Updates>();
    let (outcome, update) = {
        let mut schedule = updates.schedule.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(Some(update)) => {
                schedule.succeeded(now);
                (CheckOutcome::Newer(update.version.clone()), Some(update))
            }
            Ok(None) => {
                schedule.succeeded(now);
                (CheckOutcome::Latest, None)
            }
            Err(e) => {
                schedule.failed(now);
                log(app, format!("[bridge] update check failed: {e}"));
                (CheckOutcome::Failed, None)
            }
        }
    };

    let applied = {
        let st = shared(app);
        let mut s = lock(&st);
        match next_status(&s.update, &outcome) {
            Some(next) => {
                // Under the state lock, so an install that starts now takes
                // the update that matches the tray.
                *updates.found.lock().unwrap_or_else(|e| e.into_inner()) = update;
                s.update = next;
                true
            }
            None => false,
        }
    };
    tray::refresh_updates(app);
    if !applied {
        return;
    }
    match &outcome {
        CheckOutcome::Newer(version) => {
            log(app, format!("[bridge] update {version} is available"));
            if manual {
                notify(app, tf(lang, "tray.updateAvailable", &[("v", version)]));
            }
            // Debug builds: `RETV_BRIDGE_UPDATE_AUTOINSTALL=1` clicks the tray
            // item for the tester (the same path, busy check included).
            #[cfg(debug_assertions)]
            {
                if std::env::var("RETV_BRIDGE_UPDATE_AUTOINSTALL").as_deref() == Ok("1") {
                    on_tray_click(app);
                }
            }
        }
        CheckOutcome::Latest => {
            log(
                app,
                format!(
                    "[bridge] updates: {} is the latest",
                    env!("CARGO_PKG_VERSION")
                ),
            );
            if manual {
                notify(app, t(lang, "update.latest"));
            }
        }
        CheckOutcome::Failed => {
            if manual {
                notify(app, t(lang, "update.failed"));
            }
        }
    }
}

/// The tray item: checks now, or installs the update it shows.
pub fn on_tray_click(app: &AppHandle) {
    let status = lock(&shared(app)).update.clone();
    match status {
        UpdateStatus::Idle => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { check(&app, true).await });
        }
        UpdateStatus::Available { .. } => request_install(app),
        UpdateStatus::Off | UpdateStatus::Checking | UpdateStatus::Installing => {}
    }
}

/// «Сначала завершите настройку ТВ» while a TV is held; else the install.
fn request_install(app: &AppHandle) {
    if !install_allowed(app) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { install(app).await });
}

/// `can_install` now; a refusal is shown in a dialog.
fn install_allowed(app: &AppHandle) -> bool {
    let (active, lang) = {
        let st = shared(app);
        let s = lock(&st);
        (s.active_sessions(), s.locale)
    };
    match can_install(active) {
        Ok(()) => true,
        Err(key) => {
            app.dialog()
                .message(t(lang, key))
                .title("ReTV Bridge")
                .show(|_| {});
            false
        }
    }
}

/// Downloads (the signature is checked against the compiled-in key), stops
/// the bridge and its adb server, installs, and relaunches.
async fn install(app: AppHandle) {
    let found = app
        .state::<Updates>()
        .found
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(update) = found else { return };
    let version = update.version.clone();
    let lang = lock(&shared(&app)).locale;
    set_status(&app, UpdateStatus::Installing);
    log(&app, format!("[bridge] downloading update {version}"));

    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(b) => b,
        Err(e) => {
            log(&app, format!("[bridge] update {version} failed: {e}"));
            notify(&app, t(lang, "update.installFailed"));
            set_status(&app, UpdateStatus::Available { version });
            return;
        }
    };
    // A TV session may have started during the download.
    if !install_allowed(&app) {
        set_status(&app, UpdateStatus::Available { version });
        return;
    }

    log(&app, format!("[bridge] installing update {version}"));
    tray::stop_bridge(&app).await;
    if let Err(e) = update.install(&bytes) {
        // The relaunch below then starts this version again.
        log(&app, format!("[bridge] update {version} failed: {e}"));
        notify(&app, t(lang, "update.installFailed"));
    }
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::t;

    const MIN: u64 = 60 * 1000;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;

    #[test]
    fn ru_locale_checks_ru_site_first() {
        let s = |l| -> Vec<String> { endpoints_for(l).iter().map(|u| u.to_string()).collect() };
        assert_eq!(
            s(Lang::Ru),
            vec![
                "https://ru.retv.app/downloads/retv-bridge.json",
                "https://retv.app/downloads/retv-bridge.json"
            ]
        );
        for lang in [Lang::En, Lang::Uk] {
            assert_eq!(
                s(lang),
                vec![
                    "https://retv.app/downloads/retv-bridge.json",
                    "https://ru.retv.app/downloads/retv-bridge.json"
                ]
            );
        }
    }

    #[test]
    fn schedule_24h_and_backoff() {
        let t0 = 1_800_000_000_000;
        let mut s = UpdateSchedule::default();
        assert!(s.due(t0), "the first check runs at start");
        s.succeeded(t0);
        assert!(!s.due(t0 + DAY - 1));
        assert!(s.due(t0 + DAY));
        // A clock that went back by more than a day checks now.
        assert!(s.due(t0 - DAY - 1));

        // Failures back off 15 min, 30 min, 1 h … up to a day.
        let waits = [
            15 * MIN,
            30 * MIN,
            HOUR,
            2 * HOUR,
            4 * HOUR,
            8 * HOUR,
            16 * HOUR,
            DAY,
            DAY,
            DAY,
        ];
        let mut now = t0 + DAY;
        for (i, wait) in waits.iter().enumerate() {
            s.failed(now);
            assert_eq!(s.failures, i as u32 + 1);
            assert!(!s.due(now + wait - 1), "failure {} waits {wait} ms", i + 1);
            assert!(s.due(now + wait), "failure {} waits {wait} ms", i + 1);
            now += wait;
        }
        for _ in 0..100 {
            s.failed(now);
        }
        assert!(
            s.due(now + DAY) && !s.due(now + DAY - 1),
            "never more than a day"
        );
        // A success resets the back-off.
        s.succeeded(now);
        assert_eq!(s.failures, 0);
        assert!(!s.due(now + 15 * MIN));
        assert!(s.due(now + DAY));
    }

    #[test]
    fn install_refused_during_session() {
        assert_eq!(can_install(0), Ok(()));
        assert_eq!(can_install(1), Err("update.busy"));
        assert_eq!(can_install(2), Err("update.busy"));
        assert_eq!(t(Lang::Ru, "update.busy"), "Сначала завершите настройку ТВ");
    }

    #[test]
    fn tray_item_follows_the_update() {
        use UpdateStatus::*;
        let check = "Проверить обновления".to_string();
        assert_eq!(tray_item(&Idle, Lang::Ru), (check.clone(), true));
        assert_eq!(tray_item(&Off, Lang::Ru), (check, false));
        assert!(!tray_item(&Checking, Lang::Ru).1);
        assert!(!tray_item(&Installing, Lang::Ru).1);
        assert_eq!(
            tray_item(
                &Available {
                    version: "1.0.1".into()
                },
                Lang::Ru
            ),
            ("Доступна новая версия 1.0.1".to_string(), true)
        );
        assert_eq!(
            tray_item(
                &Available {
                    version: "1.0.1".into()
                },
                Lang::En
            )
            .0,
            "New version 1.0.1 available"
        );
    }

    #[test]
    fn the_key_placeholder_turns_updates_off() {
        assert!(!pubkey_configured(PUBKEY_PLACEHOLDER));
        assert!(!pubkey_configured(""));
        assert!(!pubkey_configured("  "));
        assert!(pubkey_configured(
            "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk="
        ));
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let committed = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        assert!(
            committed == PUBKEY_PLACEHOLDER || pubkey_configured(committed),
            "tauri.conf.json carries the placeholder or the owner's public key"
        );
    }

    /// hello, the User-Agent and the «latest» log say CARGO_PKG_VERSION; Tauri and the updater say tauri.conf.json.
    /// scripts/release-version.sh --set writes both, and this keeps them from drifting.
    #[test]
    fn cargo_version_is_the_tauri_conf_version() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            conf["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "run scripts/release-version.sh --set <version>"
        );
    }

    #[test]
    fn tauri_conf_wires_the_updater() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["bundle"]["createUpdaterArtifacts"], true);
        assert_eq!(
            conf["bundle"]["windows"]["nsis"]["installMode"],
            "currentUser"
        );
        let u = &conf["plugins"]["updater"];
        assert_eq!(
            u["endpoints"],
            serde_json::json!([
                "https://retv.app/downloads/retv-bridge.json",
                "https://ru.retv.app/downloads/retv-bridge.json"
            ])
        );
        assert_eq!(u["windows"]["installMode"], "passive");
    }

    #[test]
    fn available_keeps_checking_and_offers_newest() {
        use UpdateStatus::*;
        let avail = |v: &str| Available { version: v.into() };
        // Which checks run, and what the tray shows meanwhile.
        assert_eq!(check_starts(&Idle), Some(Checking));
        assert_eq!(
            check_starts(&avail("1.0.1")),
            Some(avail("1.0.1")),
            "the offer stays clickable while the schedule checks again"
        );
        for busy in [Off, Checking, Installing] {
            assert_eq!(check_starts(&busy), None);
        }
        // What a result does.
        let newer = |v: &str| CheckOutcome::Newer(v.into());
        assert_eq!(
            next_status(&Checking, &newer("1.0.1")),
            Some(avail("1.0.1"))
        );
        assert_eq!(
            next_status(&avail("1.0.1"), &newer("1.0.2")),
            Some(avail("1.0.2")),
            "the newest version is offered"
        );
        assert_eq!(next_status(&Checking, &CheckOutcome::Latest), Some(Idle));
        assert_eq!(
            next_status(&avail("1.0.1"), &CheckOutcome::Latest),
            Some(Idle)
        );
        assert_eq!(next_status(&Checking, &CheckOutcome::Failed), Some(Idle));
        assert_eq!(
            next_status(&avail("1.0.1"), &CheckOutcome::Failed),
            None,
            "a failed check keeps the offer"
        );
        for outcome in [newer("1.0.2"), CheckOutcome::Latest, CheckOutcome::Failed] {
            assert_eq!(
                next_status(&Installing, &outcome),
                None,
                "an install in progress is left alone"
            );
        }
    }

    #[test]
    fn updater_requires_signed_version() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["plugins"]["updater"]["requireSignedVersion"], true);
    }

    /// The release gate: `RETV_RELEASE=1 cargo test -p retv-bridge-app release_has_updater_key`
    /// fails while tauri.conf.json carries `PUBKEY_PLACEHOLDER` (README, "Updates").
    #[test]
    fn release_has_updater_key() {
        if std::env::var("RETV_RELEASE").as_deref() != Ok("1") {
            return;
        }
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let key = conf["plugins"]["updater"]["pubkey"]
            .as_str()
            .unwrap_or_default();
        assert!(
            pubkey_configured(key),
            "release build: tauri.conf.json still has the updater key placeholder"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn dev_endpoint_and_user_agent() {
        assert_eq!(
            dev_endpoint(Some("http://127.0.0.1:8765/retv-bridge.json"))
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8765/retv-bridge.json"
        );
        assert!(dev_endpoint(Some("file:///tmp/x.json")).is_none());
        assert!(dev_endpoint(Some("not a url")).is_none());
        assert!(dev_endpoint(None).is_none());
        assert_eq!(
            user_agent(),
            format!("ReTV-Bridge/{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn manifest_fixture_parses() {
        let text = include_str!("../tests/fixtures/retv-bridge-world.json");
        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_str(text).expect("Tauri reads our manifest");
        assert_eq!(release.version.to_string(), "1.0.0");
        assert!(release.version.pre.is_empty());
        assert_eq!(
            release.notes.as_deref(),
            Some("Faster TV search\nFixed pairing")
        );
        assert!(release.pub_date.is_some());
        for target in ["darwin-aarch64", "darwin-x86_64"] {
            assert_eq!(
                release.download_url(target).unwrap().as_str(),
                "https://retv.app/downloads/ReTV-Bridge-1.0.0-mac-universal.app.tar.gz"
            );
            assert!(!release.signature(target).unwrap().is_empty());
        }
        assert_eq!(
            release.download_url("windows-x86_64").unwrap().as_str(),
            "https://retv.app/downloads/ReTV-Bridge-1.0.0-win-x64-setup.exe"
        );
        assert!(release.download_url("linux-x86_64").is_err());
    }
}
