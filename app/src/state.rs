//! The app's state: the running bridge, the adb setup, what the window and
//! the tray show, and the few settings kept on disk. No Tauri types here, so
//! all of it is unit-tested.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::RngCore;
use retv_bridge_adb::server_proc::AdbServerProc;
use retv_bridge_adb::tools::ToolsError;
use retv_bridge_adb::ServerRevive;
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::server::{BridgeServer, StartError};
use serde::{Deserialize, Serialize};

use crate::i18n::{t, tf, Lang};
use crate::updates::UpdateStatus;

const SETTINGS_FILE: &str = "settings.json";

/// Lines kept for the window's log tab.
pub const LOG_LINES: usize = 1000;
/// `retv-bridge://open` links closer together than this are handled once.
pub const DEEP_LINK_GAP: Duration = Duration::from_secs(2);

pub enum AdbSetupState {
    /// No adb yet: the window asks before downloading Google's tools.
    NeedsConsent,
    Downloading {
        done: u64,
        total: u64,
    },
    /// Starting the private adb server from the installed tools.
    Starting,
    Ready,
    /// The download (or the picked zip) failed: retry the download.
    Failed(ToolsError),
    /// The installed tools did not start: retry the start, never the download.
    StartFailed,
}

/// What «Повторить» does in the adb card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdbRetry {
    Start,
    Download,
    Nothing,
}

/// Problems the window shows, as locale keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Notice {
    PortBusy,
    LanDenied,
    PairingDisabled,
}

impl Notice {
    pub fn key(self) -> &'static str {
        match self {
            Notice::PortBusy => "port.busy",
            Notice::LanDenied => "lan.denied",
            Notice::PairingDisabled => "pairing.disabled",
        }
    }
}

/// Which tab the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    Code,
    Log,
}

pub struct AppState {
    pub server: Option<BridgeServer>,
    pub adb: AdbSetupState,
    /// The session token; never shown in the window or the log.
    pub token: String,
    pub locale: Lang,
    pub notices: Vec<Notice>,
    /// Connected TVs: serial → model, from the server's events.
    pub tvs: BTreeMap<String, String>,
    pub log: VecDeque<String>,
    pub view: View,
    /// The last code shown in a notification.
    pub notified_code: Option<String>,
    /// When this run last opened the site (the start sequence opens it once).
    pub site_opened_at: Option<Instant>,
    /// The bridge's private adb server (port 5039), once started.
    pub adb_proc: Option<AdbServerProc>,
    /// Starts that server again when it dies; stopped first when quitting.
    pub adb_revive: Option<Arc<ServerRevive>>,
    /// Bumped whenever the tray asks the window for a tab, so the window
    /// switches once and the person's own clicks then stay.
    pub view_seq: u64,
    /// The update shown in the tray.
    pub update: UpdateStatus,
    /// The last `retv-bridge://open` handled.
    pub deep_link_at: Option<Instant>,
    /// mDNS rounds in a row that failed as «Local Network denied» and found nothing (see `discovery_result`).
    pub lan_denied_rounds: u8,
}

/// Denied rounds in a row before the Local Network help shows. One is not enough: right after launch macOS can fail the
/// first multicast sends while it applies a permission that is on (the S3 run: the help showed, then the Bridge found
/// the TV by itself).
pub const LAN_DENIED_ROUNDS: u8 = 2;

impl AppState {
    pub fn new(token: String, locale: Lang) -> AppState {
        AppState {
            server: None,
            adb: AdbSetupState::NeedsConsent,
            token,
            locale,
            notices: Vec::new(),
            tvs: BTreeMap::new(),
            log: VecDeque::new(),
            view: View::Code,
            notified_code: None,
            site_opened_at: None,
            adb_proc: None,
            adb_revive: None,
            view_seq: 0,
            update: UpdateStatus::Idle,
            deep_link_at: None,
            lan_denied_rounds: 0,
        }
    }

    /// Records the result of `start_server`. A busy port is shown in the
    /// window; the app keeps running so the person can read it.
    pub fn server_started(&mut self, r: Result<BridgeServer, StartError>) {
        match r {
            Ok(server) => {
                self.clear_notice(Notice::PortBusy);
                self.server = Some(server);
            }
            Err(e) => {
                self.push_log(format!("[bridge] could not start: {e}"));
                if matches!(e, StartError::PortInUse) {
                    self.add_notice(Notice::PortBusy);
                }
            }
        }
    }

    pub fn add_notice(&mut self, n: Notice) {
        if let Err(i) = self.notices.binary_search(&n) {
            self.notices.insert(i, n);
        }
    }

    pub fn clear_notice(&mut self, n: Notice) {
        self.notices.retain(|x| *x != n);
    }

    /// One mDNS round: `denied` when it failed as «Local Network denied» (macOS), `found` the TVs it found. The help
    /// shows only after LAN_DENIED_ROUNDS denied rounds in a row that found nothing; any round that works, or finds a TV,
    /// clears it.
    pub fn discovery_result(&mut self, denied: bool, found: usize) {
        if !denied || found > 0 {
            self.lan_denied_rounds = 0;
            self.clear_notice(Notice::LanDenied);
            return;
        }
        self.lan_denied_rounds = self.lan_denied_rounds.saturating_add(1);
        if self.lan_denied_rounds >= LAN_DENIED_ROUNDS {
            self.add_notice(Notice::LanDenied);
        }
    }

    pub fn notice_keys(&self) -> Vec<&'static str> {
        self.notices.iter().map(|n| n.key()).collect()
    }

    pub fn push_log(&mut self, line: String) {
        if cfg!(debug_assertions) {
            // Development builds also print the log to the terminal, like the CLI.
            eprintln!("{line}");
        }
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// Applies a server event. Returns a code to announce in a notification
    /// when the site has just asked for a new one.
    pub fn on_event(&mut self, ev: &BridgeEvent) -> Option<String> {
        match ev {
            BridgeEvent::Log(line) => self.push_log(line.clone()),
            BridgeEvent::PairCode { code, .. } => {
                if self.notified_code.as_deref() != Some(code.as_str()) {
                    self.notified_code = Some(code.clone());
                    return Some(code.clone());
                }
            }
            BridgeEvent::PairingDisabled => self.add_notice(Notice::PairingDisabled),
            BridgeEvent::TvConnected { serial, model } => {
                // A TV reached over the network (`ip:port`) proves Local Network access: no help.
                if serial.contains(':') {
                    self.discovery_result(false, 1);
                }
                self.tvs.insert(serial.clone(), model.clone());
            }
            BridgeEvent::TvDisconnected { serial } => {
                self.tvs.remove(serial);
            }
        }
        None
    }

    /// The tray's status line.
    pub fn tv_status(&self) -> String {
        if self.tvs.is_empty() {
            return t(self.locale, "tray.tvNone");
        }
        let models: Vec<&str> = self.tvs.values().map(String::as_str).collect();
        tf(self.locale, "tray.tv", &[("model", &models.join(", "))])
    }

    /// Asks the window to show `view` (the tray's "show code" / "log").
    pub fn request_view(&mut self, view: View) {
        self.view = view;
        self.view_seq += 1;
    }

    /// The adb card the window shows.
    pub fn adb_kind(&self) -> &'static str {
        match self.adb {
            AdbSetupState::NeedsConsent => "needsConsent",
            AdbSetupState::Downloading { .. } => "downloading",
            AdbSetupState::Starting => "starting",
            AdbSetupState::Ready => "ready",
            AdbSetupState::Failed(_) => "failed",
            AdbSetupState::StartFailed => "startFailed",
        }
    }

    /// Marks the adb server as starting; false while a download, a start or
    /// a running server already holds it.
    pub fn adb_start_begin(&mut self) -> bool {
        if matches!(
            self.adb,
            AdbSetupState::Downloading { .. } | AdbSetupState::Starting | AdbSetupState::Ready
        ) {
            return false;
        }
        self.adb = AdbSetupState::Starting;
        true
    }

    /// The installed tools did not start: logged with the detail, and the
    /// window offers a retry of the start.
    pub fn adb_start_failed(&mut self, line: String, detail: Option<String>) {
        self.push_log(line);
        if let Some(d) = detail {
            self.push_log(format!("[bridge]   {d}"));
        }
        self.adb = AdbSetupState::StartFailed;
    }

    /// What «Повторить» does now; `installed`: the tools for this build's pin are present.
    pub fn adb_retry(&self, installed: bool) -> AdbRetry {
        match self.adb {
            AdbSetupState::StartFailed if installed => AdbRetry::Start,
            AdbSetupState::Downloading { .. } | AdbSetupState::Starting | AdbSetupState::Ready => {
                AdbRetry::Nothing
            }
            _ => AdbRetry::Download,
        }
    }

    /// Whether a `retv-bridge://open` arriving at `now` is handled (at most one per `DEEP_LINK_GAP`).
    pub fn deep_link_due(&mut self, now: Instant) -> bool {
        if self
            .deep_link_at
            .is_some_and(|last| now.saturating_duration_since(last) < DEEP_LINK_GAP)
        {
            return false;
        }
        self.deep_link_at = Some(now);
        true
    }

    /// Replaces the TV list with the server's own (after missed events). Known
    /// models stay; an unknown one shows the serial.
    pub fn resync_tvs(&mut self, live: BTreeMap<String, Option<String>>) {
        let old = std::mem::take(&mut self.tvs);
        for (serial, model) in live {
            let name = model
                .filter(|m| !m.is_empty())
                .or_else(|| old.get(&serial).cloned())
                .unwrap_or_else(|| serial.clone());
            self.tvs.insert(serial, name);
        }
    }

    /// Sockets holding a TV right now.
    pub fn active_sessions(&self) -> usize {
        self.server.as_ref().map_or(0, |s| s.active_sessions())
    }
}

/// `482917` → `482 917`.
pub fn format_code(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

/// A fresh session token made of 15 bytes from the OS RNG, base64url, no padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 15];
    OsRng.fill_bytes(&mut bytes);
    base64url(&bytes)
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..=c.len() {
            out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitDecision {
    Quit,
    /// Ask first, with this locale key.
    Confirm(&'static str),
}

/// Quitting while a TV is held asks first.
pub fn quit_decision(active_sessions: usize) -> QuitDecision {
    if active_sessions > 0 {
        QuitDecision::Confirm("quit.confirm")
    } else {
        QuitDecision::Quit
    }
}

/// The settings kept in `settings.json` in the app's config folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// «Запускать при входе в систему»; off until the person turns it on.
    pub autostart: bool,
    /// Windows: the "still running in the notification area" balloon was shown.
    pub close_hint_shown: bool,
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        std::fs::read(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, dir.join(SETTINGS_FILE))
    }
}

/// Turning "open at login" on and off (the autostart plugin in the app).
pub trait AutostartCtl {
    fn enable(&self) -> Result<(), String>;
    fn disable(&self) -> Result<(), String>;
}

/// The check mark the tray starts with. The launcher is never touched at
/// start: only the person's click turns it on.
pub fn startup_autostart(settings: &Settings, _ctl: &dyn AutostartCtl) -> bool {
    settings.autostart
}

/// The menu toggle: applies `on`, then saves it.
pub fn set_autostart(
    settings: &mut Settings,
    dir: &Path,
    ctl: &dyn AutostartCtl,
    on: bool,
) -> Result<(), String> {
    if on {
        ctl.enable()?;
    } else {
        ctl.disable()?;
    }
    settings.autostart = on;
    settings.save(dir).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    fn state() -> AppState {
        AppState::new("T".into(), Lang::Ru)
    }

    #[test]
    fn port_in_use_shows_message() {
        let mut s = state();
        s.server_started(Err(StartError::PortInUse));
        assert!(s.server.is_none());
        assert_eq!(s.notice_keys(), vec!["port.busy"]);
        assert_eq!(
            t(Lang::Ru, Notice::PortBusy.key()).split(' ').next(),
            Some("Порт")
        );
        // The app keeps running: the adb setup and the log still work.
        assert!(matches!(s.adb, AdbSetupState::NeedsConsent));
        s.push_log("still here".into());
        assert_eq!(s.log.len(), 2, "the start failure is logged too");
        // Other bind failures are only logged, with their detail.
        let mut s = state();
        s.server_started(Err(StartError::Io("boom".into())));
        assert!(s.notice_keys().is_empty());
        assert!(s.log.back().unwrap().contains("boom"));
    }

    #[test]
    fn local_network_help_only_after_denied_rounds_and_never_once_a_tv_is_found() {
        let mut s = state();
        // The first denied round right after launch shows nothing.
        s.discovery_result(true, 0);
        assert!(s.notice_keys().is_empty());
        s.discovery_result(true, 0);
        assert_eq!(s.notice_keys(), vec!["lan.denied"]);
        // A round that works clears it and starts the count again.
        s.discovery_result(false, 0);
        assert!(s.notice_keys().is_empty());
        s.discovery_result(true, 0);
        assert!(s.notice_keys().is_empty());
        // A denied querier whose other path found the TV is no denial.
        s.discovery_result(true, 1);
        s.discovery_result(true, 0);
        assert!(s.notice_keys().is_empty());
        // A network TV connecting clears the help.
        s.discovery_result(true, 0);
        assert_eq!(s.notice_keys(), vec!["lan.denied"]);
        s.on_event(&BridgeEvent::TvConnected {
            serial: "192.168.88.232:5555".into(),
            model: "MDZ-32-AA".into(),
        });
        assert!(s.notice_keys().is_empty());
    }

    #[test]
    fn notices_are_unique_and_clearable() {
        let mut s = state();
        s.add_notice(Notice::LanDenied);
        s.add_notice(Notice::LanDenied);
        s.add_notice(Notice::PortBusy);
        assert_eq!(s.notice_keys(), vec!["port.busy", "lan.denied"]);
        s.clear_notice(Notice::LanDenied);
        assert_eq!(s.notice_keys(), vec!["port.busy"]);
    }

    #[test]
    fn quit_with_active_session_asks() {
        assert_eq!(quit_decision(0), QuitDecision::Quit);
        assert_eq!(quit_decision(1), QuitDecision::Confirm("quit.confirm"));
        assert_eq!(quit_decision(3), QuitDecision::Confirm("quit.confirm"));
        assert_eq!(t(Lang::Ru, "quit.confirm"), "Идёт работа с ТВ. Выйти?");
        assert_eq!(state().active_sessions(), 0);
    }

    struct Recorder(RefCell<Vec<&'static str>>);
    impl AutostartCtl for Recorder {
        fn enable(&self) -> Result<(), String> {
            self.0.borrow_mut().push("enable");
            Ok(())
        }
        fn disable(&self) -> Result<(), String> {
            self.0.borrow_mut().push("disable");
            Ok(())
        }
    }

    #[test]
    fn autostart_defaults_off() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(dir.path());
        assert!(!settings.autostart);
        let ctl = Recorder(RefCell::new(Vec::new()));
        assert!(!startup_autostart(&settings, &ctl));
        // Even a saved "on" never re-enables the launcher at start.
        let on = Settings {
            autostart: true,
            ..Settings::default()
        };
        assert!(startup_autostart(&on, &ctl));
        assert!(
            ctl.0.borrow().is_empty(),
            "the launcher was touched at start"
        );
    }

    #[test]
    fn autostart_toggle_applies_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = Recorder(RefCell::new(Vec::new()));
        let mut s = Settings::load(dir.path());
        set_autostart(&mut s, dir.path(), &ctl, true).unwrap();
        assert!(Settings::load(dir.path()).autostart);
        set_autostart(&mut s, dir.path(), &ctl, false).unwrap();
        assert!(!Settings::load(dir.path()).autostart);
        assert_eq!(*ctl.0.borrow(), vec!["enable", "disable"]);
        // A corrupt file reads as the defaults.
        std::fs::write(dir.path().join("settings.json"), "{oops").unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
    }

    #[test]
    fn events_drive_the_tray_status_log_and_notifications() {
        let mut s = state();
        assert_eq!(s.tv_status(), "ТВ не подключён");
        s.on_event(&BridgeEvent::TvConnected {
            serial: "192.0.2.233:5555".into(),
            model: "MiTV-AFMU0".into(),
        });
        assert_eq!(s.tv_status(), "ТВ: MiTV-AFMU0, подключён");
        s.on_event(&BridgeEvent::TvDisconnected {
            serial: "192.0.2.233:5555".into(),
        });
        assert_eq!(s.tv_status(), "ТВ не подключён");

        let code = |c: &str| BridgeEvent::PairCode {
            code: c.into(),
            expires_at_ms: 1,
        };
        assert_eq!(s.on_event(&code("482917")), Some("482917".into()));
        assert_eq!(
            s.on_event(&code("482917")),
            None,
            "the same code is announced once"
        );
        assert_eq!(s.on_event(&code("111222")), Some("111222".into()));

        s.on_event(&BridgeEvent::PairingDisabled);
        assert_eq!(s.notice_keys(), vec!["pairing.disabled"]);

        for i in 0..(LOG_LINES + 5) {
            s.on_event(&BridgeEvent::Log(format!("line {i}")));
        }
        assert_eq!(s.log.len(), LOG_LINES);
        assert_eq!(s.log.back().unwrap(), &format!("line {}", LOG_LINES + 4));
    }

    #[test]
    fn installed_adb_start_failure_offers_retry_not_download() {
        let mut s = state();
        assert_eq!(s.adb_kind(), "needsConsent");
        assert!(s.adb_start_begin());
        assert_eq!(s.adb_kind(), "starting", "never the consent card");
        assert!(!s.adb_start_begin(), "one start at a time");
        assert_eq!(s.adb_retry(true), AdbRetry::Nothing);

        s.adb_start_failed(
            "[bridge] could not start adb: boom".into(),
            Some("exit 1".into()),
        );
        assert_eq!(s.adb_kind(), "startFailed");
        assert!(s.notice_keys().is_empty(), "the card says it, not a notice");
        assert_eq!(s.log.back().unwrap(), "[bridge]   exit 1");
        assert_eq!(s.adb_retry(true), AdbRetry::Start);
        assert_eq!(
            s.adb_retry(false),
            AdbRetry::Download,
            "the tools went away: fetch them again"
        );
        assert!(s.adb_start_begin(), "a retry starts again");

        // A failed download still retries the download.
        s.adb = AdbSetupState::Failed(ToolsError::Truncated);
        assert_eq!(s.adb_kind(), "failed");
        assert_eq!(s.adb_retry(true), AdbRetry::Download);
        for busy in [
            AdbSetupState::Downloading { done: 0, total: 1 },
            AdbSetupState::Ready,
        ] {
            s.adb = busy;
            assert_eq!(s.adb_retry(true), AdbRetry::Nothing);
            assert!(!s.adb_start_begin());
        }
    }

    #[test]
    fn repeated_deep_links_are_throttled() {
        let mut s = state();
        let t0 = Instant::now();
        assert!(s.deep_link_due(t0));
        assert!(!s.deep_link_due(t0 + Duration::from_millis(300)));
        assert!(!s.deep_link_due(t0 + Duration::from_millis(1999)));
        assert!(s.deep_link_due(t0 + Duration::from_secs(2)));
        assert!(!s.deep_link_due(t0 + Duration::from_millis(3000)));
    }

    #[test]
    fn lagged_events_resync_the_tvs() {
        let mut s = state();
        s.tvs.insert("a:5555".into(), "MiTV-AFMU0".into());
        s.tvs.insert("b:5555".into(), "Gone".into());
        let live = BTreeMap::from([
            ("a:5555".to_string(), None),
            ("c:5555".to_string(), Some("Box S3".to_string())),
            ("d:5555".to_string(), None),
        ]);
        s.resync_tvs(live);
        let tvs: Vec<(&str, &str)> = s
            .tvs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            tvs,
            vec![
                ("a:5555", "MiTV-AFMU0"),
                ("c:5555", "Box S3"),
                ("d:5555", "d:5555")
            ]
        );
        s.resync_tvs(BTreeMap::new());
        assert_eq!(s.tv_status(), "ТВ не подключён");
    }

    #[test]
    fn codes_and_tokens() {
        assert_eq!(format_code("482917"), "482 917");
        assert_eq!(format_code("12"), "12");
        let tok = new_token();
        assert_eq!(tok.len(), 20);
        assert!(tok
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(tok, new_token());
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
    }
}
