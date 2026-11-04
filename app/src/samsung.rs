//! The «Samsung» tab: find a Samsung TV, guide Developer Mode, get the
//! Samsung certificate, install ReTV Home for Samsung, remove unused apps
//! and walk the person through the privacy setting with the TV's remote.
//!
//! One job runs at a time; its progress and errors go to the Log tab with
//! `[samsung]`. Tokens, keys and the certificate never reach the window or
//! the log. Secrets live in `<app data>/samsung/` (folder 0700, files 0600).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use retv_bridge_samsung::apps::{
    before_list, can_remove, is_retv_home, parse_applist, pre_ticked, uninstall_command,
    uninstall_result, valid_app_id, TvApp, Uninstall,
};
use retv_bridge_samsung::bundle::{bundled, has_bundle, install, read_duid, reinstall};
use retv_bridge_samsung::certs::{create, CertApi, CertStore};
use retv_bridge_samsung::privacy::{
    new_check_id, run as run_privacy, Ads, Answer, Delivery, Kind, PrivacyIo, PrivacyStep, Report,
    Stop,
};
use retv_bridge_samsung::remote::{remote_url, Remote, RemoteError, ALLOW_WAIT};
use retv_bridge_samsung::sdb::{Sdb, SdbError, SDB_PORT};
use retv_bridge_samsung::signin::{
    bind_callback, sign_in_url, wait_for_sign_in, Pages, CALLBACK_PORT,
};
use retv_bridge_samsung::ssdp::{discover, probe_hosts, subnet_hosts, SSDP_GROUP};
use retv_bridge_samsung::tv::{fetch_info, local_ipv4s, mac_ip_for, TvInfo, REST_PORT};
use retv_bridge_samsung::SamsungError;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::i18n::{t, Lang};
use crate::{browser, lock, now_ms, shared};

const SCAN_WAIT: Duration = Duration::from_secs(3);
/// The network check beside SSDP: each address of this computer's /24s on
/// the TV's REST port, this many at once, each this long.
const SWEEP_AT_ONCE: usize = 48;
const SWEEP_TIMEOUT: Duration = Duration::from_millis(350);
const SIGN_IN_WAIT: Duration = Duration::from_secs(10 * 60);
/// ReTV Home for Samsung's app id.
const RETV_APP: &str = "RetvHome00.ReTVHome";
/// A closed or refused sdb is tried once more after this: right after a removal batch the TV
/// often shuts the next call (seen 2026-11-04).
const SDB_RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct SamsungState {
    /// The running job (`scan`, `signIn`, …), if any.
    pub busy: Option<&'static str>,
    pub tvs: Vec<TvInfo>,
    pub scanned: bool,
    pub selected: Option<String>,
    /// The selected TV's DUID, once sdb answered.
    pub duid: Option<String>,
    pub has_cert: bool,
    pub installed: bool,
    /// The last install met a ReTV Home signed by another author
    /// certificate: the window offers to remove it and install again.
    pub install_conflict: bool,
    pub apps: Vec<TvApp>,
    pub ticked: Vec<String>,
    pub apps_loaded: bool,
    /// The «как было» file written this run for the selected TV.
    pub before_saved: Option<String>,
    pub remote: bool,
    pub privacy: Option<PrivacyStep>,
    /// i18n keys of the last outcome.
    pub status: Option<&'static str>,
    pub error: Option<&'static str>,
    /// Where the running install is (`prepare`, `remove`, `send`, `install`, `start`).
    pub install_stage: Option<&'static str>,
    /// The running install removes the old ReTV Home first.
    pub install_replace: bool,
    /// When Bridge should be done pressing the privacy keys (ms, wall clock).
    pub pressing_until: Option<u64>,
    /// The privacy routine waits for the person's answer here (no report came through the account).
    pub privacy_answer: Option<tokio::sync::oneshot::Sender<Answer>>,
    /// Why Bridge's request could not reach ReTV Home through the account (an i18n key), shown
    /// with the questions; None while it can.
    pub privacy_hint: Option<&'static str>,
    /// The account's Samsung TVs the person picks this one from (more than one could be it).
    pub privacy_pick: Option<Vec<PickTv>>,
    /// The person's pick: an account TV's id, or None («not in the list»).
    pub privacy_pick_answer: Option<tokio::sync::oneshot::Sender<Option<String>>>,
    /// «Отменить» (sign-in) and «Остановить» (key presses).
    pub cancel: Arc<Cancel>,
    /// What the window's automatic re-check logged last: the 5 s polls log a change once, not
    /// every round.
    pub quiet_log: QuietLog,
    /// The wizard's optional steps 5 and 6 are behind the person (done or
    /// skipped). Kept here, not in the window, which is made anew on opening.
    pub apps_passed: bool,
    pub privacy_passed: bool,
    /// Apps removed from the selected TV this run.
    pub removed: u32,
    /// App ids of the selected TV that Samsung does not let remove (`can not uninstall app`),
    /// kept on this computer beside the «как было» list.
    pub builtin: Vec<String>,
    /// Titles of the last batch's apps Samsung would not remove.
    pub refused_builtin: Vec<String>,
    /// ReTV Home is already on the TV (`vd_applist`).
    pub retv_on_tv: bool,
}

/// The lines of the window's 5 s re-check: a round that says what the last one said is not
/// logged again; when something changes, how often the old state repeated is logged first.
#[derive(Debug, Default)]
pub struct QuietLog {
    last: Option<Vec<String>>,
    repeats: u32,
}

impl QuietLog {
    /// The lines to log for this round.
    pub fn round(&mut self, lines: &[String]) -> Vec<String> {
        if self.last.as_deref() == Some(lines) {
            self.repeats += 1;
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.repeats > 0 {
            let n = self.last.as_ref().map_or(0, Vec::len);
            let what = if n == 1 { "line" } else { "lines" };
            let times = if self.repeats == 1 { "time" } else { "times" };
            out.push(format!(
                "(the same {n} {what} {} more {times})",
                self.repeats
            ));
        }
        out.extend(lines.iter().cloned());
        self.last = Some(lines.to_vec());
        self.repeats = 0;
        out
    }

    /// A check the person asked for: the next automatic round logs in full.
    pub fn reset(&mut self) {
        *self = QuietLog::default();
    }
}

/// What one removal batch did, judged by the TV's list read after it.
#[derive(Debug, Default, PartialEq)]
pub struct Removal {
    pub removed: Vec<TvApp>,
    /// Samsung would not remove these (`can not uninstall app`).
    pub builtin: Vec<TvApp>,
    pub failed: Vec<TvApp>,
}

/// An app no longer in `fresh` is removed, whatever the TV answered; one still there is
/// Samsung's (it said `can not uninstall app`) or failed. Without a fresh list the answers decide.
pub fn settle(answers: &[(TvApp, Uninstall)], fresh: Option<&[TvApp]>) -> Removal {
    let mut r = Removal::default();
    for (app, answer) in answers {
        let gone = match fresh {
            Some(list) => !list.iter().any(|a| a.app_id == app.app_id),
            None => *answer == Uninstall::Done,
        };
        let into = if gone {
            &mut r.removed
        } else if *answer == Uninstall::Builtin {
            &mut r.builtin
        } else {
            &mut r.failed
        };
        into.push(app.clone());
    }
    r
}

/// The file beside the «как было» lists with a TV's apps Samsung does not let remove.
pub fn builtin_file(duid: &str) -> String {
    format!("builtin-{duid}.json")
}

/// The ids of such a file (only plain app ids).
pub fn parse_ids(bytes: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Vec<String>>(bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|id| valid_app_id(id))
        .collect()
}

/// `old` and `new`, sorted, each once.
pub fn merge_ids(old: &[String], new: &[String]) -> Vec<String> {
    let mut all: Vec<String> = old.iter().chain(new).cloned().collect();
    all.sort();
    all.dedup();
    all
}

/// Offered for removal on this TV: removable, not protected, not one of Samsung's own.
pub fn offered(app: &TvApp, builtin: &[String]) -> bool {
    can_remove(app) && !builtin.contains(&app.app_id)
}

/// The apps ticked when the list opens.
pub fn first_ticks(apps: &[TvApp], builtin: &[String]) -> Vec<String> {
    apps.iter()
        .filter(|a| pre_ticked(a) && !builtin.contains(&a.app_id))
        .map(|a| a.app_id.clone())
        .collect()
}

/// The window's key when sdb shut the door, by what the TV's REST says (`info`; none when it
/// did not answer) and this computer's address on the TV's network (`mac`). Only a TV that
/// trusts another address is a Host PC IP problem: one that trusts this computer and still
/// refuses has Developer Mode off on its own screen, or came back from quick standby.
pub fn refused_key(info: Option<&TvInfo>, mac: Option<&str>) -> &'static str {
    let Some(tv) = info else {
        return "samsung.err.tv";
    };
    if !tv.developer_mode {
        return "samsung.err.devOff";
    }
    let Some(mac) = mac else {
        return "samsung.err.sdb";
    };
    if tv.developer_ip.as_deref() != Some(mac) {
        "samsung.err.hostIp"
    } else {
        "samsung.err.devClosed"
    }
}

/// «Дальше», «Пропустить» or going back on step 5 or 6: an error or outcome belongs to the step
/// that produced it, so it goes with the step change.
pub fn pass_step(s: &mut SamsungState, step: u8, passed: bool) {
    match step {
        5 => s.apps_passed = passed,
        6 => s.privacy_passed = passed,
        _ => return,
    }
    if s.busy.is_none() {
        s.error = None;
        s.status = None;
    }
    s.refused_builtin.clear();
}

/// A stop request for the running job: a flag (checked between steps) and a
/// wake-up for a job that is waiting.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Cancel {
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    pub fn fire(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_fired(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolves once `fire` was called (also if before this call).
    pub async fn fired(&self) {
        loop {
            let wake = self.notify.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if self.is_fired() {
                return;
            }
            wake.await;
        }
    }
}

/// The install stage a progress line of `bundle::install` starts.
pub fn install_stage(progress: &str) -> Option<&'static str> {
    [
        ("connecting", "prepare"),
        ("signing", "prepare"),
        ("removing", "remove"),
        ("sending", "send"),
        ("installing", "install"),
        ("starting", "start"),
    ]
    .into_iter()
    .find(|(prefix, _)| progress.starts_with(prefix))
    .map(|(_, stage)| stage)
}

/// How long `keys` take, for the window's countdown.
pub fn pressing_ms(keys: &[(&str, Duration)]) -> u64 {
    keys.iter()
        .map(|(_, wait)| wait.as_millis() as u64 + KEY_SEND_MS)
        .sum()
}

/// A rough time to send one key over the remote channel.
const KEY_SEND_MS: u64 = 150;

pub type SamsungShared = Arc<Mutex<SamsungState>>;

fn sam(app: &AppHandle) -> SamsungShared {
    app.state::<SamsungShared>().inner().clone()
}

fn guard(s: &SamsungShared) -> MutexGuard<'_, SamsungState> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TvRow {
    ip: String,
    name: String,
    model: String,
    year: Option<u16>,
    dev_mode: bool,
    dev_ip: Option<String>,
    host_ok: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRow {
    app_id: String,
    title: String,
    use_count: Option<u32>,
    ticked: bool,
    removable: bool,
    /// Samsung's own: shown greyed, never offered.
    builtin: bool,
}

/// What the window polls for the tab.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamsungView {
    busy: Option<&'static str>,
    scanned: bool,
    tvs: Vec<TvRow>,
    selected: Option<String>,
    mac_ip: Option<String>,
    duid: Option<String>,
    cert: bool,
    wgt: bool,
    installed: bool,
    install_conflict: bool,
    apps_loaded: bool,
    apps: Vec<AppRow>,
    before_saved: bool,
    remote: bool,
    privacy: Option<PrivacyStep>,
    status: Option<&'static str>,
    error: Option<&'static str>,
    install_stage: Option<&'static str>,
    install_replace: bool,
    /// The countdown while Bridge presses the privacy keys.
    pressing_left_ms: Option<u64>,
    privacy_hint: Option<&'static str>,
    privacy_pick: Option<Vec<PickTv>>,
    apps_passed: bool,
    privacy_passed: bool,
    removed: u32,
    refused_builtin: Vec<String>,
    retv_on_tv: bool,
}

pub fn view(s: &SamsungState, ours: &[Ipv4Addr], wgt: bool) -> SamsungView {
    view_at(s, ours, wgt, now_ms())
}

pub fn view_at(s: &SamsungState, ours: &[Ipv4Addr], wgt: bool, now: u64) -> SamsungView {
    let mac_ip = s
        .selected
        .as_deref()
        .and_then(|ip| mac_ip_for(ip, ours))
        .map(|a| a.to_string());
    let tvs = s
        .tvs
        .iter()
        .map(|tv| TvRow {
            ip: tv.ip.clone(),
            name: tv.name.clone(),
            model: tv.model_name.clone(),
            year: tv.year,
            dev_mode: tv.developer_mode,
            dev_ip: tv.developer_ip.clone(),
            host_ok: mac_ip_for(&tv.ip, ours).is_some_and(|m| tv.host_ip_ok(&m.to_string())),
        })
        .collect();
    let row = |a: &TvApp, builtin: bool| AppRow {
        app_id: a.app_id.clone(),
        title: a.title.clone(),
        use_count: a.use_count,
        ticked: !builtin && s.ticked.contains(&a.app_id),
        removable: !builtin,
        builtin,
    };
    // Those that can go first, then Samsung's own, greyed.
    let apps = s
        .apps
        .iter()
        .filter(|a| offered(a, &s.builtin))
        .map(|a| row(a, false))
        .chain(
            s.apps
                .iter()
                .filter(|a| can_remove(a) && s.builtin.contains(&a.app_id))
                .map(|a| row(a, true)),
        )
        .collect();
    SamsungView {
        busy: s.busy,
        scanned: s.scanned,
        tvs,
        selected: s.selected.clone(),
        mac_ip,
        duid: s.duid.clone(),
        cert: s.has_cert,
        wgt,
        installed: s.installed,
        install_conflict: s.install_conflict,
        apps_loaded: s.apps_loaded,
        apps,
        before_saved: s.before_saved.is_some(),
        remote: s.remote,
        privacy: s.privacy,
        status: s.status,
        error: s.error,
        install_stage: (s.busy == Some("install"))
            .then_some(s.install_stage)
            .flatten(),
        install_replace: s.install_replace,
        pressing_left_ms: s
            .pressing_until
            .filter(|_| s.busy == Some("privacy"))
            .map(|until| until.saturating_sub(now)),
        privacy_hint: s.privacy_hint.filter(|_| s.busy == Some("privacy")),
        privacy_pick: s.privacy_pick.clone().filter(|_| s.busy == Some("privacy")),
        apps_passed: s.apps_passed,
        privacy_passed: s.privacy_passed,
        removed: s.removed,
        refused_builtin: s.refused_builtin.clone(),
        retv_on_tv: s.retv_on_tv,
    }
}

/// `ready_tv_name` of the app's Samsung state.
pub fn ready_tv(app: &AppHandle) -> Option<String> {
    app.try_state::<SamsungShared>()
        .and_then(|st| ready_tv_name(&guard(st.inner())))
}

/// The tray's line for the Samsung TV this run set up: its name, once Bridge
/// can talk to it.
pub fn ready_tv_name(s: &SamsungState) -> Option<String> {
    s.duid.as_ref()?;
    let ip = s.selected.as_deref()?;
    let tv = s.tvs.iter().find(|t| t.ip == ip)?;
    Some(if tv.name.is_empty() {
        tv.model_name.clone()
    } else {
        tv.name.clone()
    })
}

/// The apps the person asked to remove, only if every one may be removed.
pub fn to_remove(apps: &[TvApp], ids: &[String], builtin: &[String]) -> Option<Vec<TvApp>> {
    let mut out = Vec::new();
    for id in ids {
        let app = apps.iter().find(|a| &a.app_id == id)?;
        if !offered(app, builtin) {
            return None;
        }
        out.push(app.clone());
    }
    (!out.is_empty()).then_some(out)
}

/// The window's key for an error; the details go to the log.
pub fn error_key(e: &SamsungError) -> &'static str {
    match e {
        SamsungError::Sdb(SdbError::Down(_)) => "samsung.err.devOff",
        // Which door is shut only the TV's REST can tell: `refused_key`.
        SamsungError::Sdb(SdbError::Refused) => "samsung.err.sdb",
        SamsungError::Sdb(_) => "samsung.err.sdb",
        SamsungError::TooManyRequests => "samsung.err.tooMany",
        SamsungError::Samsung(_) => "samsung.err.samsung",
        SamsungError::WrongCertificate => "samsung.err.wrongCert",
        SamsungError::InstallFailed(_) => "samsung.err.install",
        SamsungError::OtherAuthor => "samsung.err.otherAuthor",
        SamsungError::TvUnreachable(_) | SamsungError::NotSamsung => "samsung.err.tv",
        SamsungError::CallbackPort(_) => "samsung.err.port",
        SamsungError::Other(_) => "samsung.err.other",
    }
}

fn remote_error_key(e: &RemoteError) -> &'static str {
    match e {
        RemoteError::Denied => "samsung.err.remoteDenied",
        RemoteError::Timeout => "samsung.err.remoteTimeout",
        RemoteError::Down(_) | RemoteError::BadKey => "samsung.err.remoteDown",
    }
}

fn is_home_ip(ip: &str) -> bool {
    retv_bridge_core::validate::is_lan_ipv4(&serde_json::Value::String(ip.to_string()))
}

fn log(app: &AppHandle, line: String) {
    lock(&shared(app)).push_log(format!("[samsung] {line}"));
}

fn store(app: &AppHandle) -> CertStore {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("app.retv.bridge"))
        .join("samsung");
    CertStore::new(dir)
}

fn lang(app: &AppHandle) -> Lang {
    lock(&shared(app)).locale
}

fn sdb_addr(ip: &str) -> Option<SocketAddr> {
    let ip: Ipv4Addr = ip.parse().ok()?;
    Some(SocketAddr::from((ip, SDB_PORT)))
}

fn remote_pairing_file(ip: &str) -> String {
    format!("remote-{}.pairing", ip.replace('.', "-"))
}

/// Marks `job` running unless another one is; false when busy.
fn begin(app: &AppHandle, job: &'static str) -> bool {
    let st = sam(app);
    let mut s = guard(&st);
    if s.busy.is_some() {
        return false;
    }
    s.busy = Some(job);
    s.error = None;
    s.status = None;
    s.cancel.reset();
    true
}

/// The lines of one automatic re-check: written when the round differs from the last one, so
/// a TV that stays off is logged once (`QuietLog`).
fn log_quiet_round(app: &AppHandle, lines: &[String]) {
    let out = guard(&sam(app)).quiet_log.round(lines);
    for line in out {
        log(app, line);
    }
}

/// The TV's REST and this computer's address on its network, to name a refused sdb.
async fn refused_reason(ip: &str) -> &'static str {
    let info = fetch_info(ip, REST_PORT).await.ok();
    let mac = mac_ip_for(ip, &local_ipv4s()).map(|m| m.to_string());
    refused_key(info.as_ref(), mac.as_deref())
}

/// Logs `e` and gives the window's key; a refused sdb is named by the TV's REST.
async fn fail_tv(app: &AppHandle, ip: &str, what: &str, e: &SamsungError) -> &'static str {
    log(app, format!("{what}: {e}"));
    if matches!(e, SamsungError::Sdb(SdbError::Refused)) {
        let key = refused_reason(ip).await;
        log(app, format!("sdb refused; the TV's REST says: {key}"));
        return key;
    }
    error_key(e)
}

/// sdb to the TV at `ip`, tried once more after `SDB_RETRY_AFTER` if the first try fails: a
/// transient close right after a batch is not a Host PC IP problem.
async fn sdb_connect(app: &AppHandle, ip: &str) -> Result<Sdb, &'static str> {
    let addr = sdb_addr(ip).ok_or("samsung.err.other")?;
    match Sdb::connect(addr).await {
        Ok(sdb) => return Ok(sdb),
        Err(e) => log(app, format!("sdb: {e}; trying again in 1 s")),
    }
    tokio::time::sleep(SDB_RETRY_AFTER).await;
    match Sdb::connect(addr).await {
        Ok(sdb) => Ok(sdb),
        Err(e) => Err(fail_tv(app, ip, "sdb", &e.into()).await),
    }
}

fn finish(app: &AppHandle, result: Result<&'static str, &'static str>) {
    let st = sam(app);
    let mut s = guard(&st);
    s.busy = None;
    match result {
        Ok(status) => s.status = Some(status),
        Err(key) => s.error = Some(key),
    }
}

fn fail(app: &AppHandle, what: &str, e: &SamsungError) -> &'static str {
    log(app, format!("{what}: {e}"));
    error_key(e)
}

fn selected(app: &AppHandle) -> Option<String> {
    guard(&sam(app)).selected.clone()
}

/// Spawns `job` as the one running job.
fn run<F>(app: &AppHandle, name: &'static str, job: F)
where
    F: std::future::Future<Output = Result<&'static str, &'static str>> + Send + 'static,
{
    if !begin(app, name) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = job.await;
        finish(&handle, result);
    });
}

/// After the TV changed (selected, Developer Mode turned on): reads its REST
/// info again and, if sdb answers, its DUID and whether we hold its
/// certificate.
async fn refresh_tv(app: &AppHandle, ip: &str) -> Result<&'static str, &'static str> {
    refresh_tv_with(app, ip, false).await
}

/// `refresh_tv`; `quiet` (the window's 5 s re-check) logs only changes.
async fn refresh_tv_with(
    app: &AppHandle,
    ip: &str,
    quiet: bool,
) -> Result<&'static str, &'static str> {
    if !quiet {
        guard(&sam(app)).quiet_log.reset();
        return check_tv(app, ip, &|line| log(app, line)).await;
    }
    // The automatic re-check: the round's lines are logged only when they changed.
    let lines = Mutex::new(Vec::new());
    let result = check_tv(app, ip, &|line| {
        lines.lock().unwrap_or_else(|e| e.into_inner()).push(line)
    })
    .await;
    let lines = lines.into_inner().unwrap_or_else(|e| e.into_inner());
    log_quiet_round(app, &lines);
    result
}

/// `refresh_tv`'s work; `say` takes its log lines.
async fn check_tv(
    app: &AppHandle,
    ip: &str,
    say: &(dyn Fn(String) + Sync),
) -> Result<&'static str, &'static str> {
    let fail = |what: &str, e: &SamsungError| {
        say(format!("{what}: {e}"));
        error_key(e)
    };
    let info = fetch_info(ip, REST_PORT)
        .await
        .map_err(|e| fail("TV info", &e))?;
    let mac = mac_ip_for(ip, &local_ipv4s()).map(|m| m.to_string());
    let host_ok = mac.as_deref().is_some_and(|m| info.host_ip_ok(m));
    {
        let st = sam(app);
        let mut s = guard(&st);
        if let Some(t) = s.tvs.iter_mut().find(|t| t.ip == ip) {
            *t = info.clone();
        } else {
            s.tvs.push(info.clone());
        }
    }
    say(format!(
        "{} {} ({}), Developer Mode {}",
        info.model_name,
        ip,
        info.year.map_or("?".into(), |y| y.to_string()),
        if info.developer_mode { "on" } else { "off" }
    ));
    if !info.developer_mode {
        return Ok("samsung.status.devOff");
    }
    if !host_ok {
        return Ok("samsung.status.hostIp");
    }
    let addr = sdb_addr(ip).ok_or("samsung.err.other")?;
    let duid = read_duid(addr).await.map_err(|e| {
        let key = fail("sdb", &e);
        if matches!(e, SamsungError::Sdb(SdbError::Refused)) {
            // REST says on and this computer is trusted, yet sdb is shut.
            refused_key(Some(&info), mac.as_deref())
        } else {
            key
        }
    })?;
    let has_cert = store(app).load(&duid).is_some();
    // Is ReTV Home already there? Best effort: a TV that will not list its apps is asked again
    // on step 5.
    let retv_on_tv = match list_apps(ip).await {
        Ok(apps) => apps.iter().any(is_retv_home),
        Err(e) => {
            say(format!("vd_applist: {e}"));
            false
        }
    };
    let builtin = store(app)
        .read_private(&builtin_file(&duid))
        .map(|b| parse_ids(&b))
        .unwrap_or_default();
    let st = sam(app);
    let mut s = guard(&st);
    s.duid = Some(duid);
    s.has_cert = has_cert;
    s.retv_on_tv = retv_on_tv;
    s.builtin = builtin;
    Ok(if has_cert {
        "samsung.status.ready"
    } else {
        "samsung.status.needCert"
    })
}

fn select_tv(app: &AppHandle, ip: &str) {
    let st = sam(app);
    let mut s = guard(&st);
    if s.selected.as_deref() != Some(ip) {
        s.selected = Some(ip.to_string());
        s.duid = None;
        s.has_cert = false;
        s.installed = false;
        s.install_conflict = false;
        s.apps.clear();
        s.ticked.clear();
        s.apps_loaded = false;
        s.before_saved = None;
        s.remote = false;
        s.privacy = None;
        s.apps_passed = false;
        s.privacy_passed = false;
        s.removed = 0;
        s.builtin.clear();
        s.refused_builtin.clear();
        s.retv_on_tv = false;
    }
}

/// «Дальше» / «Пропустить» on the optional steps (5 apps, 6 ads), or going
/// back to one (`passed` false).
#[tauri::command]
pub fn samsung_pass(app: AppHandle, step: u8, passed: bool) {
    pass_step(&mut guard(&sam(&app)), step, passed);
}

/// «Настроить другой ТВ»: back to the search, nothing selected.
#[tauri::command]
pub fn samsung_forget(app: AppHandle) {
    let st = sam(&app);
    let mut s = guard(&st);
    if s.busy.is_some() {
        return;
    }
    *s = SamsungState {
        cancel: s.cancel.clone(),
        ..SamsungState::default()
    };
}

#[tauri::command]
pub fn samsung_view(app: AppHandle) -> SamsungView {
    view(&guard(&sam(&app)), &local_ipv4s(), has_bundle())
}

#[tauri::command]
pub fn samsung_scan(app: AppHandle) {
    let handle = app.clone();
    run(&app, "scan", async move {
        log(
            &handle,
            "looking for Samsung TVs (SSDP, and port 8001 on this computer's network)".into(),
        );
        let hosts = subnet_hosts(&local_ipv4s());
        let (heard, swept) = tokio::join!(
            discover(SSDP_GROUP, SCAN_WAIT, REST_PORT, |ip| {
                is_home_ip(&ip.to_string())
            }),
            probe_hosts(hosts, REST_PORT, SWEEP_TIMEOUT, SWEEP_AT_ONCE),
        );
        log(
            &handle,
            format!(
                "SSDP: {} Samsung TV(s), network check: {}",
                heard.len(),
                swept.len()
            ),
        );
        let found = merge_found(heard, swept);
        log(&handle, format!("found {} Samsung TV(s)", found.len()));
        let only = (found.len() == 1).then(|| found[0].ip.clone());
        {
            let st = sam(&handle);
            let mut s = guard(&st);
            s.tvs = found;
            s.scanned = true;
        }
        match only {
            Some(ip) => {
                select_tv(&handle, &ip);
                refresh_tv(&handle, &ip).await
            }
            None => Ok("samsung.status.scanned"),
        }
    });
}

/// The TVs SSDP heard, then those only the network check found; one per
/// address, sorted.
pub fn merge_found(heard: Vec<TvInfo>, swept: Vec<TvInfo>) -> Vec<TvInfo> {
    let mut out: Vec<TvInfo> = Vec::new();
    for tv in heard.into_iter().chain(swept) {
        if !out.iter().any(|t| t.ip == tv.ip) {
            out.push(tv);
        }
    }
    out.sort_by_key(|tv| tv.ip.parse::<Ipv4Addr>().ok());
    out
}

/// A TV by its address, when the search does not find it.
#[tauri::command]
pub fn samsung_add_ip(app: AppHandle, ip: String) {
    let ip = ip.trim().to_string();
    if !is_home_ip(&ip) {
        guard(&sam(&app)).error = Some("samsung.err.badIp");
        return;
    }
    let handle = app.clone();
    run(&app, "refresh", async move {
        select_tv(&handle, &ip);
        refresh_tv(&handle, &ip).await
    });
}

#[tauri::command]
pub fn samsung_select(app: AppHandle, ip: String) {
    if !guard(&sam(&app)).tvs.iter().any(|t| t.ip == ip) {
        return;
    }
    let handle = app.clone();
    run(&app, "refresh", async move {
        select_tv(&handle, &ip);
        refresh_tv(&handle, &ip).await
    });
}

/// Reads the TV again: «Проверить сейчас», or (`auto`) the window's re-check
/// every 5 s while it waits for the TV, which logs only changes.
#[tauri::command]
pub fn samsung_refresh(app: AppHandle, auto: Option<bool>) {
    let Some(ip) = selected(&app) else {
        return;
    };
    let quiet = auto.unwrap_or(false);
    let handle = app.clone();
    run(&app, "refresh", async move {
        refresh_tv_with(&handle, &ip, quiet).await
    });
}

fn pages(lang: Lang) -> Pages {
    let page = |key: &str| {
        format!(
            "<!doctype html><meta charset=\"utf-8\"><title>ReTV Bridge</title><body style=\"font:16px -apple-system,sans-serif;margin:40px\"><h2>{}</h2></body>",
            t(lang, key)
        )
    };
    Pages {
        done: page("samsung.signin.done"),
        retry: page("samsung.signin.retry"),
    }
}

#[tauri::command]
pub fn samsung_sign_in(app: AppHandle) {
    let Some(duid) = guard(&sam(&app)).duid.clone() else {
        return;
    };
    let handle = app.clone();
    run(&app, "signIn", async move {
        // localhost may be ::1 or 127.0.0.1 in the browser: both must be ours,
        // or another program could take Samsung's answer.
        let listeners = bind_callback(CALLBACK_PORT)
            .await
            .map_err(|e| fail(&handle, "sign-in", &e))?;
        log(&handle, "opening Samsung sign-in in the browser".into());
        if let Err(e) = browser::open_url(&handle, &sign_in_url()) {
            log(&handle, format!("could not open the browser: {e}"));
            return Err("samsung.err.other");
        }
        let cancel = guard(&sam(&handle)).cancel.clone();
        let wait = wait_for_sign_in(listeners, SIGN_IN_WAIT, pages(lang(&handle)));
        let signed = tokio::select! {
            r = wait => r.map_err(|e| fail(&handle, "sign-in", &e))?,
            // «Отменить»: dropping the wait closes the listeners.
            _ = cancel.fired() => {
                log(&handle, "sign-in cancelled".into());
                return Ok("samsung.status.cancelled");
            }
        };
        log(
            &handle,
            "signed in; asking Samsung for the certificates".into(),
        );
        let cert = create(&CertApi::samsung(), &signed, &duid, now_ms())
            .await
            .map_err(|e| fail(&handle, "certificate", &e))?;
        drop(signed);
        store(&handle).save(&cert).map_err(|e| {
            log(&handle, format!("could not save the certificate: {e}"));
            "samsung.err.other"
        })?;
        log(&handle, format!("certificate saved for DUID {duid}"));
        guard(&sam(&handle)).has_cert = true;
        Ok("samsung.status.certDone")
    });
}

/// «Отменить» while Bridge waits for the sign-in in the browser.
#[tauri::command]
pub fn samsung_sign_in_cancel(app: AppHandle) {
    let st = sam(&app);
    let s = guard(&st);
    if s.busy == Some("signIn") {
        s.cancel.fire();
    }
}

/// «Открыть страницу снова»: the same Samsung page, only while Bridge waits
/// for it (the listener is already up).
#[tauri::command]
pub fn samsung_sign_in_reopen(app: AppHandle) {
    if guard(&sam(&app)).busy != Some("signIn") {
        return;
    }
    if let Err(e) = browser::open_url(&app, &sign_in_url()) {
        log(&app, format!("could not open the browser: {e}"));
    }
}

/// «Нет аккаунта Samsung? Создать бесплатно»: Samsung's account page.
#[tauri::command]
pub fn samsung_open_account(app: AppHandle) {
    if let Err(e) = browser::open_url(&app, SAMSUNG_ACCOUNT_URL) {
        log(&app, format!("could not open the browser: {e}"));
    }
}

const SAMSUNG_ACCOUNT_URL: &str = "https://account.samsung.com/";

#[tauri::command]
pub fn samsung_install(app: AppHandle) {
    start_install(app, false);
}

/// After [`SamsungError::OtherAuthor`] and the person's confirmation:
/// removes the ReTV Home on the TV (its settings and sign-in there are lost)
/// and installs this one.
#[tauri::command]
pub fn samsung_reinstall(app: AppHandle) {
    if guard(&sam(&app)).install_conflict {
        start_install(app, true);
    }
}

/// «Дальше» on «ReTV Home уже стоит»: the one on the TV stays, the wizard goes on.
#[tauri::command]
pub fn samsung_keep_installed(app: AppHandle) {
    keep_installed(&mut guard(&sam(&app)));
}

pub fn keep_installed(s: &mut SamsungState) {
    if s.busy.is_none() && s.retv_on_tv {
        s.installed = true;
        s.error = None;
        s.status = None;
    }
}

/// The person kept the ReTV Home already on the TV.
#[tauri::command]
pub fn samsung_install_cancel(app: AppHandle) {
    let st = sam(&app);
    let mut s = guard(&st);
    if s.busy.is_none() {
        s.install_conflict = false;
        s.error = None;
    }
}

fn start_install(app: AppHandle, replace: bool) {
    let (ip, duid) = {
        let st = sam(&app);
        let s = guard(&st);
        (s.selected.clone(), s.duid.clone())
    };
    let (Some(ip), Some(duid)) = (ip, duid) else {
        return;
    };
    let handle = app.clone();
    run(&app, "install", async move {
        {
            let st = sam(&handle);
            let mut s = guard(&st);
            s.install_conflict = false;
            s.install_stage = Some("prepare");
            s.install_replace = replace;
        }
        let Some(files) = bundled() else {
            log(&handle, "this build has no ReTV Home for Samsung".into());
            return Err("samsung.err.noWgt");
        };
        let Some(cert) = store(&handle).load(&duid) else {
            return Err("samsung.err.wrongCert");
        };
        let addr = sdb_addr(&ip).ok_or("samsung.err.other")?;
        let progress_handle = handle.clone();
        let progress = move |step: &str| {
            if let Some(stage) = install_stage(step) {
                guard(&sam(&progress_handle)).install_stage = Some(stage);
            }
            log(&progress_handle, step.to_string())
        };
        let result = if replace {
            reinstall(addr, files, &cert, &progress).await
        } else {
            install(addr, files, &cert, &progress).await
        };
        if let Err(e) = result {
            if e == SamsungError::OtherAuthor {
                guard(&sam(&handle)).install_conflict = true;
            }
            return Err(fail_tv(&handle, &ip, "install", &e).await);
        }
        log(&handle, "ReTV Home is installed and started".into());
        let st = sam(&handle);
        let mut s = guard(&st);
        s.installed = true;
        s.retv_on_tv = true;
        drop(s);
        Ok("samsung.status.installed")
    });
}

/// `0 vd_applist` once, no retry, no log.
async fn list_apps(ip: &str) -> Result<Vec<TvApp>, SamsungError> {
    let addr = sdb_addr(ip).ok_or_else(|| SamsungError::Other("not an IPv4 address".into()))?;
    let mut sdb = Sdb::connect(addr).await?;
    Ok(parse_applist(&sdb.shell("0 vd_applist").await?))
}

/// The TV's app list; a closed sdb is tried once more after `SDB_RETRY_AFTER`.
async fn load_apps(app: &AppHandle, ip: &str) -> Result<Vec<TvApp>, &'static str> {
    let mut sdb = sdb_connect(app, ip).await?;
    let out = match sdb.shell("0 vd_applist").await {
        Ok(out) => out,
        Err(e) => {
            log(app, format!("vd_applist: {e}; trying again in 1 s"));
            drop(sdb);
            tokio::time::sleep(SDB_RETRY_AFTER).await;
            let mut sdb = sdb_connect(app, ip).await?;
            match sdb.shell("0 vd_applist").await {
                Ok(out) => out,
                Err(e) => return Err(fail_tv(app, ip, "vd_applist", &e.into()).await),
            }
        }
    };
    Ok(parse_applist(&out))
}

#[tauri::command]
pub fn samsung_apps(app: AppHandle) {
    let Some(ip) = selected(&app) else {
        return;
    };
    let handle = app.clone();
    run(&app, "apps", async move {
        let apps = load_apps(&handle, &ip).await?;
        log(&handle, format!("{} apps on the TV", apps.len()));
        let st = sam(&handle);
        let mut s = guard(&st);
        s.ticked = first_ticks(&apps, &s.builtin);
        s.retv_on_tv = apps.iter().any(is_retv_home);
        s.apps = apps;
        s.apps_loaded = true;
        Ok("samsung.status.appsLoaded")
    });
}

#[tauri::command]
pub fn samsung_tick(app: AppHandle, app_id: String, on: bool) {
    let st = sam(&app);
    let mut s = guard(&st);
    if !s
        .apps
        .iter()
        .any(|a| a.app_id == app_id && offered(a, &s.builtin))
    {
        return;
    }
    s.ticked.retain(|id| id != &app_id);
    if on {
        s.ticked.push(app_id);
    }
}

/// Removes the ticked apps, after saving the «как было» list once.
#[tauri::command]
pub fn samsung_remove(app: AppHandle) {
    let (ip, duid, chosen, apps, saved) = {
        let st = sam(&app);
        let s = guard(&st);
        (
            s.selected.clone(),
            s.duid.clone(),
            to_remove(&s.apps, &s.ticked, &s.builtin),
            s.apps.clone(),
            s.before_saved.is_some(),
        )
    };
    let (Some(ip), Some(chosen)) = (ip, chosen) else {
        guard(&sam(&app)).error = Some("samsung.err.nothingTicked");
        return;
    };
    let handle = app.clone();
    run(&app, "remove", async move {
        if !saved {
            let name = format!(
                "before-{}-{}.json",
                duid.as_deref().unwrap_or("tv"),
                now_ms()
            );
            store(&handle)
                .write_private(&name, before_list(&apps, now_ms()).as_bytes())
                .map_err(|e| {
                    log(&handle, format!("could not save the «как было» list: {e}"));
                    "samsung.err.other"
                })?;
            log(&handle, format!("«как было» list saved: {name}"));
            guard(&sam(&handle)).before_saved = Some(name);
        }
        guard(&sam(&handle)).refused_builtin.clear();
        let mut sdb = sdb_connect(&handle, &ip).await?;
        let mut answers = Vec::new();
        for a in &chosen {
            let Some(cmd) = uninstall_command(&a.app_id) else {
                log(&handle, format!("skipped {}: unexpected app id", a.title));
                answers.push((a.clone(), Uninstall::Failed));
                continue;
            };
            let mut out = sdb.shell(&cmd).await;
            if let Err(e) = &out {
                // The TV closed sdb in the batch: once more on a new connection.
                log(&handle, format!("{}: {e}; trying again in 1 s", a.title));
                tokio::time::sleep(SDB_RETRY_AFTER).await;
                if let Ok(again) = Sdb::connect(sdb_addr(&ip).ok_or("samsung.err.other")?).await {
                    sdb = again;
                    out = sdb.shell(&cmd).await;
                }
            }
            let answer = match out {
                Ok(out) => {
                    let r = uninstall_result(&out);
                    log(&handle, format!("{}: {:?}: {}", a.title, r, out.trim()));
                    r
                }
                Err(e) => {
                    log(&handle, format!("could not remove {}: {e}", a.title));
                    Uninstall::Failed
                }
            };
            answers.push((a.clone(), answer));
        }
        drop(sdb);
        // Each removal is checked against the list the TV gives now.
        let fresh = load_apps(&handle, &ip).await.ok();
        if fresh.is_none() {
            log(
                &handle,
                "could not read the app list after removing; going by the TV's answers".into(),
            );
        }
        let r = settle(&answers, fresh.as_deref());
        let titles = |list: &[TvApp]| list.iter().map(|a| a.title.clone()).collect::<Vec<_>>();
        log(
            &handle,
            format!(
                "removed {}; Samsung's own, not removable: {}; not removed: {}",
                r.removed.len(),
                titles(&r.builtin).join(", "),
                titles(&r.failed).join(", ")
            ),
        );
        let st = sam(&handle);
        let mut s = guard(&st);
        if !r.builtin.is_empty() {
            let ids: Vec<String> = r.builtin.iter().map(|a| a.app_id.clone()).collect();
            s.builtin = merge_ids(&s.builtin, &ids);
            let file = builtin_file(duid.as_deref().unwrap_or("tv"));
            let json = serde_json::to_vec(&s.builtin).unwrap_or_default();
            if let Err(e) = store(&handle).write_private(&file, &json) {
                log(&handle, format!("could not save {file}: {e}"));
            }
        }
        s.ticked.clear();
        match fresh {
            Some(apps) => s.apps = apps,
            None => s
                .apps
                .retain(|a| !r.removed.iter().any(|x| x.app_id == a.app_id)),
        }
        s.removed += r.removed.len() as u32;
        s.refused_builtin = titles(&r.builtin);
        Ok(if r.failed.is_empty() {
            "samsung.status.removed"
        } else {
            "samsung.status.removedSome"
        })
    });
}

/// The log line for how opening the remote ended.
pub fn remote_outcome(result: Result<(), &RemoteError>) -> String {
    match result {
        Ok(()) => "remote connected".into(),
        Err(e @ RemoteError::Denied) => format!("remote denied: {e}"),
        Err(e @ RemoteError::Timeout) => format!("remote timed out: {e}"),
        Err(e) => format!("remote failed: {e}"),
    }
}

async fn open_remote(app: &AppHandle, ip: &str, wait: Duration) -> Result<Remote, &'static str> {
    let st = store(app);
    let file = remote_pairing_file(ip);
    let saved = st
        .read_private(&file)
        .and_then(|b| String::from_utf8(b).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    match Remote::connect(&remote_url(ip, saved.as_deref()), true, wait).await {
        Ok((remote, issued)) => {
            log(app, remote_outcome(Ok(())));
            if let Some(fresh) = issued.filter(|n| Some(n) != saved.as_ref()) {
                if let Err(e) = st.write_private(&file, fresh.as_bytes()) {
                    log(app, format!("could not save the remote pairing ({e})"));
                }
            }
            Ok(remote)
        }
        Err(e) => {
            log(app, remote_outcome(Err(&e)));
            if e == RemoteError::Denied {
                let _ = st.write_private(&file, b"");
            }
            Err(remote_error_key(&e))
        }
    }
}

/// Opens the remote channel once: the TV asks «Allow» the first time.
#[tauri::command]
pub fn samsung_remote_pair(app: AppHandle) {
    let Some(ip) = selected(&app) else {
        return;
    };
    let handle = app.clone();
    run(&app, "remote", async move {
        log(
            &handle,
            "connecting the remote: press Allow on the TV".into(),
        );
        let remote = open_remote(&handle, &ip, ALLOW_WAIT).await?;
        remote.close().await;
        let st = sam(&handle);
        let mut s = guard(&st);
        s.remote = true;
        s.privacy.get_or_insert(PrivacyStep::Ready);
        Ok("samsung.status.remoteOk")
    });
}

/// How often Bridge reads the account's TVs while it waits for ReTV Home's report.
const REPORT_POLL: Duration = Duration::from_millis(1500);

/// The report for `check` among the account's TVs.
pub fn report_for(tvs: &[retv_bridge_account::Tv], check: &str) -> Option<Report> {
    tvs.iter()
        .filter_map(|t| t.privacy.as_ref())
        .find(|p| p.check.as_deref() == Some(check))
        .map(|p| Report {
            ads: Ads::of_lat(p.lat_enabled),
            opened: p.opened,
        })
}

/// One of the account's Samsung TVs, for the person to pick from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PickTv {
    pub id: String,
    pub name: String,
    pub model: String,
}

/// Which of the account's TVs is the Samsung TV at hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TvMatch {
    One(String),
    /// No Samsung TV of the account is it: ReTV Home on it is not signed in.
    Absent,
    /// The person picks.
    Pick(Vec<PickTv>),
}

/// A model name as letters and digits, upper case: `QE55Q67AA` and the REST's `QE55Q67AAUXXH`
/// meet as prefixes.
fn model_key(m: &str) -> String {
    m.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The account's TV that is the Samsung TV with REST model name `model` (`QE55Q67AAUXXH`;
/// None when unknown). Only Samsung TVs (ReTV Home `…-tizen`) count. One of the same model is it;
/// several, or a single Samsung of another model, the person picks (a report from the wrong TV
/// would be read as this one's); none signed in: `Absent`. The account does not know a TV's DUID.
pub fn match_tv(tvs: &[retv_bridge_account::Tv], model: Option<&str>) -> TvMatch {
    let samsung: Vec<&retv_bridge_account::Tv> = tvs
        .iter()
        .filter(|t| {
            t.home_version
                .as_deref()
                .is_some_and(|v| v.ends_with("-tizen"))
        })
        .collect();
    let pick = |list: &[&retv_bridge_account::Tv]| {
        TvMatch::Pick(
            list.iter()
                .map(|t| PickTv {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    model: t.model.clone(),
                })
                .collect(),
        )
    };
    if samsung.is_empty() {
        return TvMatch::Absent;
    }
    let key = model.map(model_key).filter(|k| k.len() >= 6);
    let Some(key) = key else {
        return match samsung.as_slice() {
            [one] => TvMatch::One(one.id.clone()),
            many => pick(many),
        };
    };
    let same: Vec<&retv_bridge_account::Tv> = samsung
        .iter()
        .copied()
        .filter(|t| {
            let k = model_key(&t.model);
            k.len() >= 6 && (key.starts_with(&k) || k.starts_with(&key))
        })
        .collect();
    match same.as_slice() {
        [one] => TvMatch::One(one.id.clone()),
        [] => pick(&samsung),
        many => pick(many),
    }
}

/// The routine's hands in the app: the account (requests and reports), sdb (starting ReTV
/// Home), the TV's remote channel (keys), the window.
struct AppIo {
    app: AppHandle,
    ip: String,
    /// The REST model name of the TV (`QE55Q67AAUXXH`), to find it among the account's TVs.
    model: Option<String>,
    cancel: Arc<Cancel>,
    /// The last error, for the window.
    error: Option<&'static str>,
    /// The account's TV this one is, once known this run (`Some(None)`: not signed in there).
    tv: Option<Option<String>>,
}

impl AppIo {
    fn set(&self, f: impl FnOnce(&mut SamsungState)) {
        f(&mut guard(&sam(&self.app)));
    }

    /// Waits `d` unless the person stops first.
    async fn pause(&self, d: Duration) {
        tokio::select! {
            _ = tokio::time::sleep(d) => {}
            _ = self.cancel.fired() => {}
        }
    }
}

impl AppIo {
    fn account(&self) -> Option<Arc<retv_bridge_account::Account>> {
        Some(
            self.app
                .try_state::<crate::account::AccountShared>()?
                .account
                .clone(),
        )
    }

    /// The account's TV this one is: matched by model, else picked by the person; None when
    /// ReTV Home on it is not signed in (or the account's TVs could not be read).
    async fn account_tv(&mut self, account: &retv_bridge_account::Account) -> Option<String> {
        if let Some(known) = &self.tv {
            return known.clone();
        }
        let tvs = match account.refresh_tvs().await {
            Ok(tvs) => tvs,
            Err(e) => {
                log(
                    &self.app,
                    format!("privacy: could not read the account's TVs ({e})"),
                );
                return None;
            }
        };
        let found = match match_tv(&tvs, self.model.as_deref()) {
            TvMatch::One(id) => Some(id),
            TvMatch::Absent => None,
            TvMatch::Pick(list) => {
                log(
                    &self.app,
                    format!("privacy: {} TVs could be this one; asking", list.len()),
                );
                self.pick(list).await
            }
        };
        if found.is_none() {
            log(
                &self.app,
                "privacy: this TV is not signed in to ReTV Home".into(),
            );
        }
        self.tv = Some(found.clone());
        found
    }

    async fn pick(&mut self, list: Vec<PickTv>) -> Option<String> {
        let ids: Vec<String> = list.iter().map(|t| t.id.clone()).collect();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.set(|s| {
            s.privacy_pick = Some(list);
            s.privacy_pick_answer = Some(tx);
        });
        let picked = tokio::select! {
            a = rx => a.ok().flatten(),
            _ = self.cancel.fired() => None,
        };
        self.set(|s| {
            s.privacy_pick = None;
            s.privacy_pick_answer = None;
        });
        // Only one of those offered.
        picked.filter(|id| ids.contains(id))
    }

    /// Leaves the request on the account for this TV; why not, as the window's hint.
    async fn send_request(&mut self, kind: Kind, check: &str) -> Result<(), &'static str> {
        let Some(account) = self.account().filter(|a| a.signed_in()) else {
            return Err("samsung.privacy.accountHint");
        };
        let Some(tv) = self.account_tv(&account).await else {
            return Err("samsung.privacy.tvSignIn");
        };
        match account.request_tv(&tv, kind.wire(), check).await {
            Ok(()) => Ok(()),
            Err(retv_bridge_account::ApiError::Server(404, _)) => {
                // Signed out on the TV since (or removed from the account).
                log(
                    &self.app,
                    "privacy: the account no longer has this TV".into(),
                );
                self.tv = Some(None);
                Err("samsung.privacy.tvSignIn")
            }
            Err(e) => {
                log(
                    &self.app,
                    format!("privacy: could not leave the request ({e})"),
                );
                Err("samsung.privacy.accountHint")
            }
        }
    }

    /// ReTV Home to the front over sdb (`was_execute`): the remote channel's launch does nothing
    /// on a 2021 TV.
    async fn start_retv(&mut self) -> Result<(), String> {
        let mut sdb = sdb_connect(&self.app, &self.ip).await.map_err(|key| {
            self.error = Some(key);
            key.to_string()
        })?;
        match sdb.shell(&format!("0 was_execute {RETV_APP}")).await {
            Ok(_) => Ok(()),
            Err(e) => {
                self.error = Some("samsung.err.retvNotOpened");
                Err(format!("was_execute: {e}"))
            }
        }
    }
}

impl PrivacyIo for AppIo {
    async fn launch(&mut self, kind: Kind, check: &str) -> Result<Delivery, String> {
        let sent = self.send_request(kind, check).await;
        let hint = sent.err();
        self.set(|s| s.privacy_hint = hint);
        self.start_retv().await?;
        let how = if hint.is_none() {
            "with the request on the account"
        } else {
            "without a request"
        };
        log(
            &self.app,
            format!("privacy: ReTV Home started ({}, {how})", kind.wire()),
        );
        Ok(if hint.is_none() {
            Delivery::Account
        } else {
            Delivery::Unsent
        })
    }

    async fn report(&mut self, check: &str, wait: Duration) -> Option<Report> {
        let account = self.account()?;
        if !account.signed_in() {
            return None;
        }
        let until = tokio::time::Instant::now() + wait;
        loop {
            match account.refresh_tvs().await {
                Ok(tvs) => {
                    if let Some(r) = report_for(&tvs, check) {
                        return Some(r);
                    }
                }
                Err(e) => log(
                    &self.app,
                    format!("privacy: could not read the account's TVs ({e})"),
                ),
            }
            if self.cancel.is_fired() || tokio::time::Instant::now() + REPORT_POLL > until {
                return None;
            }
            self.pause(REPORT_POLL).await;
        }
    }

    async fn keys(&mut self, keys: &[(&'static str, Duration)]) -> Result<(), String> {
        let mut remote = open_remote(&self.app, &self.ip, Duration::from_secs(10))
            .await
            .map_err(|key| {
                self.error = Some(key);
                key.to_string()
            })?;
        let total = pressing_ms(keys);
        self.set(|s| s.pressing_until = Some(now_ms() + total));
        let mut result = Ok(());
        for (key, wait) in keys {
            // «Остановить»: no further key goes out.
            if self.cancel.is_fired() {
                break;
            }
            if let Err(e) = remote.key(key).await {
                log(&self.app, format!("remote: {e}"));
                self.error = Some(remote_error_key(&e));
                result = Err(e.to_string());
                break;
            }
            self.pause(*wait).await;
        }
        remote.close().await;
        self.set(|s| s.pressing_until = None);
        result
    }

    async fn ask(&mut self, step: PrivacyStep) -> Option<Answer> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.set(|s| {
            s.privacy = Some(step);
            s.privacy_answer = Some(tx);
        });
        let answer = tokio::select! {
            a = rx => a.ok(),
            _ = self.cancel.fired() => None,
        };
        self.set(|s| s.privacy_answer = None);
        answer
    }

    async fn sleep(&mut self, d: Duration) {
        self.pause(d).await;
    }

    fn show(&mut self, step: PrivacyStep) {
        self.set(|s| s.privacy = Some(step));
    }

    fn check_id(&mut self) -> String {
        new_check_id()
    }

    fn stopped(&self) -> bool {
        self.cancel.is_fired()
    }

    fn log(&mut self, line: String) {
        log(&self.app, line);
    }
}

/// What the window is told when the routine ends at `end`.
pub fn privacy_outcome(
    end: PrivacyStep,
    error: Option<&'static str>,
) -> Result<&'static str, &'static str> {
    match end {
        PrivacyStep::Stopped { why: Stop::Person } => Ok("samsung.status.stopped"),
        PrivacyStep::Stopped {
            why: Stop::LaunchFailed | Stop::KeysFailed,
        } => Err(error.unwrap_or("samsung.err.remoteDown")),
        PrivacyStep::Done { pressed: true } => Ok("samsung.status.keysSent"),
        _ => Ok("samsung.status.privacyChecked"),
    }
}

/// «Выключить рекламу»: Bridge leaves a request on the account and starts ReTV Home, which reads
/// the setting and opens Samsung's Privacy Choices; Bridge presses the keys and has ReTV Home
/// read the result (`privacy`).
#[tauri::command]
pub fn samsung_privacy_start(app: AppHandle) {
    let (ip, model, cancel) = {
        let st = sam(&app);
        let mut s = guard(&st);
        if s.busy.is_some() || !s.remote {
            return;
        }
        s.privacy_hint = None;
        let model = s
            .selected
            .as_deref()
            .and_then(|ip| s.tvs.iter().find(|t| t.ip == ip))
            .map(|t| t.model_name.clone())
            .filter(|m| !m.is_empty());
        (s.selected.clone(), model, s.cancel.clone())
    };
    let Some(ip) = ip else {
        return;
    };
    let handle = app.clone();
    run(&app, "privacy", async move {
        log(&handle, "privacy: starting".into());
        let mut io = AppIo {
            app: handle.clone(),
            ip,
            model,
            cancel,
            error: None,
            tv: None,
        };
        let end = run_privacy(&mut io).await;
        let st = sam(&handle);
        let mut s = guard(&st);
        s.privacy = Some(end);
        s.privacy_answer = None;
        s.privacy_pick = None;
        s.privacy_pick_answer = None;
        s.pressing_until = None;
        privacy_outcome(end, io.error)
    });
}

/// The person's answer when ReTV Home's report did not come through the account.
#[tauri::command]
pub fn samsung_privacy(app: AppHandle, answer: Answer) {
    if let Some(tx) = guard(&sam(&app)).privacy_answer.take() {
        let _ = tx.send(answer);
    }
}

/// Which of the account's TVs this one is (None: «not in the list»).
#[tauri::command]
pub fn samsung_privacy_pick(app: AppHandle, tv: Option<String>) {
    if let Some(tx) = guard(&sam(&app)).privacy_pick_answer.take() {
        let _ = tx.send(tv);
    }
}

/// «Остановить» while the privacy routine runs.
#[tauri::command]
pub fn samsung_privacy_stop(app: AppHandle) {
    let st = sam(&app);
    let s = guard(&st);
    if s.busy == Some("privacy") {
        s.cancel.fire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retv_bridge_samsung::privacy::privacy_keys;

    #[test]
    fn install_progress_maps_to_stages() {
        assert_eq!(install_stage("connecting to the TV (sdb)"), Some("prepare"));
        assert_eq!(install_stage("signing ReTV Home"), Some("prepare"));
        assert_eq!(
            install_stage("removing the ReTV Home already on the TV"),
            Some("remove")
        );
        assert_eq!(install_stage("sending the package (812 KB)"), Some("send"));
        assert_eq!(install_stage("installing"), Some("install"));
        assert_eq!(install_stage("starting ReTV Home"), Some("start"));
        assert_eq!(install_stage("something else"), None);
    }

    #[test]
    fn countdown_and_stage_show_only_while_their_job_runs() {
        let mut s = SamsungState {
            busy: Some("privacy"),
            pressing_until: Some(10_000),
            install_stage: Some("send"),
            ..Default::default()
        };
        let v = serde_json::to_value(view_at(&s, &[], true, 3_000)).unwrap();
        assert_eq!(v["pressingLeftMs"], 7_000);
        assert!(v["installStage"].is_null());
        s.busy = Some("install");
        let v = serde_json::to_value(view_at(&s, &[], true, 3_000)).unwrap();
        assert!(v["pressingLeftMs"].is_null());
        assert_eq!(v["installStage"], "send");
        let ms = pressing_ms(&privacy_keys());
        assert!((17_000..19_000).contains(&ms), "{ms}");
    }

    #[tokio::test]
    async fn cancel_wakes_a_waiter_and_a_late_one() {
        let c = Arc::new(Cancel::default());
        let waiter = {
            let c = c.clone();
            tokio::spawn(async move { c.fired().await })
        };
        tokio::task::yield_now().await;
        c.fire();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        // Fired before anyone waits: still seen.
        tokio::time::timeout(Duration::from_secs(1), c.fired())
            .await
            .unwrap();
        c.reset();
        assert!(!c.is_fired());
    }

    #[test]
    fn the_tray_names_a_ready_samsung_tv_only() {
        let tv = TvInfo {
            ip: "192.168.1.47".into(),
            name: "Гостиная".into(),
            model_name: "QE55Q60B".into(),
            model_code: "22_X".into(),
            year: Some(2022),
            developer_mode: true,
            developer_ip: Some("192.168.1.34".into()),
            os: "Tizen".into(),
        };
        let mut s = SamsungState {
            tvs: vec![tv],
            selected: Some("192.168.1.47".into()),
            ..Default::default()
        };
        assert_eq!(ready_tv_name(&s), None);
        s.duid = Some("X".into());
        assert_eq!(ready_tv_name(&s).as_deref(), Some("Гостиная"));
    }

    fn app(id: &str, title: &str, uses: Option<u32>, removable: bool) -> TvApp {
        TvApp {
            app_id: id.into(),
            title: title.into(),
            tizen_id: format!("x.{title}"),
            use_count: uses,
            removable,
        }
    }

    #[test]
    fn view_has_no_secrets_and_hides_protected_apps() {
        let s = SamsungState {
            apps: vec![
                app("1", "TizenBrew", Some(0), true),
                app("2", "Spotify", Some(0), true),
                app("3", "e-Manual", Some(0), false),
                app("4", "Netflix", Some(0), true),
            ],
            ticked: vec!["2".into()],
            selected: Some("192.168.88.219".into()),
            duid: Some("2DCNLQ722RJQQ".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(view(&s, &[Ipv4Addr::new(192, 168, 88, 234)], true)).unwrap();
        assert_eq!(v["macIp"], "192.168.88.234");
        let apps = v["apps"].as_array().unwrap();
        assert_eq!(
            apps.len(),
            2,
            "only removable, unprotected apps are offered"
        );
        assert_eq!(apps[0]["title"], "Spotify");
        assert_eq!(apps[0]["ticked"], true);
        assert_eq!(apps[1]["title"], "Netflix");
        assert_eq!(apps[1]["ticked"], false);
        assert_eq!(
            first_ticks(&s.apps, &[]),
            vec!["2".to_string()],
            "Netflix is offered, never pre-ticked"
        );
        let text = v.to_string();
        for secret in ["token", "PRIVATE", "password"] {
            assert!(
                !text.to_lowercase().contains(&secret.to_lowercase()),
                "{secret} in {text}"
            );
        }
    }

    #[test]
    fn found_tvs_are_merged_by_address() {
        let tv = |ip: &str, name: &str| TvInfo {
            ip: ip.into(),
            name: name.into(),
            model_name: "QE55Q67AAUXXH".into(),
            model_code: "21_NIKEL_UHD".into(),
            year: Some(2021),
            developer_mode: false,
            developer_ip: None,
            os: "Tizen".into(),
        };
        let found = merge_found(
            vec![tv("192.168.88.219", "ssdp")],
            vec![tv("192.168.88.30", "sweep"), tv("192.168.88.219", "sweep")],
        );
        let ips: Vec<&str> = found.iter().map(|t| t.ip.as_str()).collect();
        assert_eq!(ips, ["192.168.88.30", "192.168.88.219"]);
        assert_eq!(found[1].name, "ssdp");
    }

    #[test]
    fn remote_outcome_is_logged_plainly() {
        assert_eq!(remote_outcome(Ok(())), "remote connected");
        assert_eq!(
            remote_outcome(Err(&RemoteError::Denied)),
            "remote denied: the TV refused the remote (Deny)"
        );
        assert_eq!(
            remote_outcome(Err(&RemoteError::Timeout)),
            "remote timed out: the TV did not allow the remote in time"
        );
        assert_eq!(
            remote_outcome(Err(&RemoteError::Down("refused".into()))),
            "remote failed: remote channel: refused"
        );
    }

    #[test]
    fn other_author_offers_reinstall() {
        assert_eq!(
            error_key(&SamsungError::OtherAuthor),
            "samsung.err.otherAuthor"
        );
        let s = SamsungState {
            install_conflict: true,
            ..Default::default()
        };
        let v = serde_json::to_value(view(&s, &[], true)).unwrap();
        assert_eq!(v["installConflict"], true);
        let v = serde_json::to_value(view(&SamsungState::default(), &[], true)).unwrap();
        assert_eq!(v["installConflict"], false);
    }

    #[test]
    fn remove_rejects_protected_and_unknown_ids() {
        let apps = vec![
            app("1", "TizenBrew", Some(0), true),
            app("2", "Spotify", Some(0), true),
            app("3", "e-Manual", Some(0), false),
            app("4", "Netflix", Some(9), true),
        ];
        assert_eq!(to_remove(&apps, &["2".into()], &[]).unwrap().len(), 1);
        assert!(to_remove(&apps, &["1".into()], &[]).is_none());
        // Netflix is the person's to remove when they tick it.
        assert_eq!(to_remove(&apps, &["4".into()], &[]).unwrap().len(), 1);
        assert!(to_remove(&apps, &["2".into(), "3".into()], &[]).is_none());
        assert!(to_remove(&apps, &["9".into()], &[]).is_none());
        // An id that is not a plain app id is refused even if listed.
        let odd = vec![app("1;reboot", "Odd", Some(0), true)];
        assert!(to_remove(&odd, &["1;reboot".into()], &[]).is_none());
        assert!(to_remove(&apps, &[], &[]).is_none());
    }

    #[test]
    fn a_batch_is_judged_by_the_list_read_after_it() {
        use retv_bridge_samsung::apps::Uninstall as U;
        let gone = app("1", "Spotify", Some(0), true);
        let manual = app("20212100002", "e-Manual", Some(0), true);
        let rakuten = app("3201511006428", "Rakuten TV", Some(0), true);
        let stuck = app("4", "Art Store", Some(0), true);
        // The TV said «completed» for Art Store, yet it is still listed: not removed.
        let answers = vec![
            (gone.clone(), U::Done),
            (manual.clone(), U::Builtin),
            (rakuten.clone(), U::Builtin),
            (stuck.clone(), U::Done),
        ];
        let fresh = vec![manual.clone(), rakuten.clone(), stuck.clone()];
        let r = settle(&answers, Some(&fresh));
        assert_eq!(r.removed, vec![gone.clone()]);
        assert_eq!(r.builtin, vec![manual.clone(), rakuten.clone()]);
        assert_eq!(r.failed, vec![stuck.clone()]);
        // A failed answer for an app the list no longer has: it is gone.
        let r = settle(&[(gone.clone(), U::Failed)], Some(&[]));
        assert_eq!(r.removed, vec![gone.clone()]);
        // No fresh list (the TV closed sdb): the answers decide.
        let r = settle(&answers, None);
        assert_eq!(r.removed, vec![gone, stuck]);
        assert_eq!(r.builtin.len(), 2);
        assert!(r.failed.is_empty());
    }

    #[test]
    fn builtin_apps_are_kept_per_tv_and_shown_greyed() {
        assert_eq!(builtin_file("2DCNLQ722RJQQ"), "builtin-2DCNLQ722RJQQ.json");
        assert_eq!(
            merge_ids(&["b".into(), "a".into()], &["a".into(), "c".into()]),
            vec!["a".to_string(), "b".into(), "c".into()]
        );
        assert_eq!(
            parse_ids(br#"["20212100002","x;y"]"#),
            vec!["20212100002".to_string()]
        );
        assert!(parse_ids(b"junk").is_empty());

        let s = SamsungState {
            apps: vec![
                app("20212100002", "e-Manual", Some(0), true),
                app("2", "Spotify", Some(0), true),
            ],
            builtin: vec!["20212100002".into()],
            ticked: vec!["2".into()],
            refused_builtin: vec!["e-Manual".into(), "Rakuten TV".into()],
            ..Default::default()
        };
        let v = serde_json::to_value(view(&s, &[], true)).unwrap();
        let apps = v["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0]["title"], "Spotify");
        assert_eq!(apps[0]["builtin"], false);
        assert_eq!(apps[1]["title"], "e-Manual");
        assert_eq!(apps[1]["builtin"], true);
        assert_eq!(apps[1]["removable"], false);
        assert_eq!(apps[1]["ticked"], false);
        assert_eq!(
            v["refusedBuiltin"],
            serde_json::json!(["e-Manual", "Rakuten TV"])
        );
        // Never removed, never pre-ticked.
        assert!(to_remove(&s.apps, &["20212100002".into()], &s.builtin).is_none());
        assert_eq!(first_ticks(&s.apps, &s.builtin), vec!["2".to_string()]);
    }

    #[test]
    fn a_refused_sdb_names_host_pc_ip_only_when_the_tv_says_so() {
        let tv = |on: bool, ip: Option<&str>| TvInfo {
            ip: "192.168.88.219".into(),
            name: "TV".into(),
            model_name: "QE55Q67AAUXXH".into(),
            model_code: "21_NIKEL_UHD".into(),
            year: Some(2021),
            developer_mode: on,
            developer_ip: ip.map(Into::into),
            os: "Tizen".into(),
        };
        let mac = Some("192.168.88.234");
        let other = tv(true, Some("192.168.88.20"));
        assert_eq!(refused_key(Some(&other), mac), "samsung.err.hostIp");
        assert_eq!(
            refused_key(Some(&tv(true, None)), mac),
            "samsung.err.hostIp"
        );
        // REST says on and trusts this computer, yet sdb is shut: the TV's own switch is off or it
        // came back from quick standby.
        let ours = tv(true, Some("192.168.88.234"));
        assert_eq!(refused_key(Some(&ours), mac), "samsung.err.devClosed");
        assert_eq!(
            refused_key(Some(&tv(false, None)), mac),
            "samsung.err.devOff"
        );
        assert_eq!(refused_key(None, mac), "samsung.err.tv");
        assert_eq!(refused_key(Some(&ours), None), "samsung.err.sdb");
        // Without the TV's word a refusal is no Host PC IP problem.
        assert_ne!(
            error_key(&SamsungError::Sdb(SdbError::Refused)),
            "samsung.err.hostIp"
        );
    }

    #[test]
    fn the_wait_logs_a_round_only_when_it_changed() {
        let mut q = QuietLog::default();
        let round = |a: &str| vec![a.to_string(), "sdb: the TV closed sdb".to_string()];
        assert_eq!(q.round(&round("QE55 on")), round("QE55 on"));
        assert!(q.round(&round("QE55 on")).is_empty());
        assert!(q.round(&round("QE55 on")).is_empty());
        let next = q.round(&round("QE55 off"));
        assert_eq!(
            next,
            vec![
                "(the same 2 lines 2 more times)".to_string(),
                "QE55 off".into(),
                "sdb: the TV closed sdb".into()
            ]
        );
        q.reset();
        assert_eq!(q.round(&round("QE55 off")), round("QE55 off"));
    }

    #[test]
    fn retv_home_on_the_tv_is_offered_as_an_update() {
        let s = SamsungState {
            retv_on_tv: true,
            ..Default::default()
        };
        let v = serde_json::to_value(view(&s, &[], true)).unwrap();
        assert_eq!(v["retvOnTv"], true);
        let mut s = s;
        keep_installed(&mut s);
        assert!(s.installed);
        let mut none = SamsungState::default();
        keep_installed(&mut none);
        assert!(!none.installed, "«Дальше» only when ReTV Home is on the TV");
    }

    #[test]
    fn a_step_error_goes_with_the_step() {
        // Seen 2026-11-04: after «Пропустить» on step 5, step 6 still showed step 5's error.
        let mut s = SamsungState {
            error: Some("samsung.err.hostIp"),
            status: Some("samsung.status.removedSome"),
            refused_builtin: vec!["e-Manual".into()],
            ..Default::default()
        };
        pass_step(&mut s, 5, true);
        assert!(s.apps_passed);
        assert_eq!(s.error, None);
        assert_eq!(s.status, None);
        assert!(s.refused_builtin.is_empty());
        s.error = Some("samsung.err.remoteDown");
        pass_step(&mut s, 5, false);
        assert!(!s.apps_passed);
        assert_eq!(s.error, None);
    }

    #[test]
    fn every_error_key_has_strings() {
        use retv_bridge_samsung::sdb::SdbError as E;
        let errors = [
            SamsungError::Sdb(E::Down("x".into())),
            SamsungError::Sdb(E::Refused),
            SamsungError::Sdb(E::Timeout),
            SamsungError::TooManyRequests,
            SamsungError::Samsung("x".into()),
            SamsungError::WrongCertificate,
            SamsungError::InstallFailed("x".into()),
            SamsungError::OtherAuthor,
            SamsungError::NotSamsung,
            SamsungError::CallbackPort("x".into()),
            SamsungError::Other("x".into()),
        ];
        let mut keys: Vec<&str> = errors.iter().map(error_key).collect();
        keys.extend([
            remote_error_key(&RemoteError::Denied),
            remote_error_key(&RemoteError::Timeout),
            remote_error_key(&RemoteError::BadKey),
            "samsung.err.retvNotOpened",
            "samsung.privacy.tvSignIn",
            "samsung.privacy.openYourself",
            "samsung.privacy.pickTitle",
            "samsung.privacy.pickLead",
            "samsung.privacy.pickNone",
            "samsung.privacy.pickNone.sub",
            "samsung.err.badIp",
            "samsung.err.port",
            "samsung.err.noWgt",
            "samsung.err.nothingTicked",
            "samsung.err.devClosed",
            "samsung.apps.builtin",
            "samsung.apps.builtinRefused",
            "samsung.apps.builtinSub",
            "samsung.install.already",
            "samsung.install.update",
            "samsung.install.conflict",
            "samsung.install.replace",
            "samsung.install.keep",
        ]);
        // The outcomes the window shows.
        keys.extend([
            "samsung.status.devOff",
            "samsung.status.hostIp",
            "samsung.status.ready",
            "samsung.status.needCert",
            "samsung.status.scanned",
            "samsung.status.certDone",
            "samsung.status.installed",
            "samsung.status.appsLoaded",
            "samsung.status.removed",
            "samsung.status.removedSome",
            "samsung.status.privacyChecked",
            "samsung.status.remoteOk",
            "samsung.status.keysSent",
            "samsung.status.cancelled",
            "samsung.status.stopped",
        ]);
        for key in keys {
            for lang in [Lang::Ru, Lang::En, Lang::Uk] {
                assert_ne!(t(lang, key), key, "{lang:?} misses {key}");
            }
        }
    }

    fn tv_with(
        check: Option<&str>,
        lat: Option<bool>,
        opened: Option<bool>,
    ) -> retv_bridge_account::Tv {
        retv_bridge_account::Tv {
            id: "d".into(),
            name: "Кухня".into(),
            model: "QE55Q67AA".into(),
            kind_line: "Samsung (Tizen)".into(),
            home_version: Some("0.2.0-tizen".into()),
            helper_version: None,
            helper_seen: None,
            privacy: Some(retv_bridge_account::TvPrivacy {
                lat_enabled: lat,
                at: "2026-11-04T10:00:00.000Z".into(),
                check: check.map(str::to_string),
                opened,
            }),
        }
    }

    #[test]
    fn only_the_report_for_this_request_counts() {
        let mut other = tv_with(Some("Other1234"), Some(true), None);
        other.id = "d0".into();
        let mut none = tv_with(None, Some(false), None);
        none.privacy = None;
        let tvs = vec![
            none,
            other,
            tv_with(Some("Mine1234"), Some(false), Some(false)),
        ];
        assert_eq!(
            report_for(&tvs, "Mine1234"),
            Some(Report {
                ads: Ads::On,
                opened: Some(false)
            })
        );
        assert_eq!(report_for(&tvs, "Other1234").map(|r| r.ads), Some(Ads::Off));
        // A report ReTV Home made on its own (no check) is never Bridge's answer.
        assert_eq!(
            report_for(&[tv_with(None, Some(true), None)], "Mine1234"),
            None
        );
        assert_eq!(report_for(&tvs, "Gone1234"), None);
        assert_eq!(
            report_for(&[tv_with(Some("Mine1234"), None, None)], "Mine1234").map(|r| r.ads),
            Some(Ads::Unknown)
        );
    }

    #[test]
    fn the_samsung_at_hand_is_found_among_the_accounts_tvs() {
        let tv = |id: &str, model: &str, home: Option<&str>| {
            let mut t = tv_with(None, None, None);
            t.id = id.into();
            t.name = format!("TV {id}");
            t.model = model.into();
            t.home_version = home.map(str::to_string);
            t.privacy = None;
            t
        };
        let tizen = Some("0.2.0-tizen");
        let android = tv("a", "MiTV-AFMU0", Some("1.4.0"));
        // The REST's model name carries the region; ReTV Home's does not.
        let kitchen = tv("k", "QE55Q67AA", tizen);
        assert_eq!(
            match_tv(&[android.clone(), kitchen.clone()], Some("QE55Q67AAUXXH")),
            TvMatch::One("k".into())
        );
        // No Samsung signed in (an Android TV does not count).
        assert_eq!(
            match_tv(std::slice::from_ref(&android), Some("QE55Q67AAUXXH")),
            TvMatch::Absent
        );
        assert_eq!(match_tv(&[], None), TvMatch::Absent);
        // A Samsung of another model may be another TV: the person says.
        let other = tv("o", "UE43AU7100", tizen);
        let TvMatch::Pick(list) = match_tv(std::slice::from_ref(&other), Some("QE55Q67AAUXXH"))
        else {
            panic!()
        };
        assert_eq!(
            list,
            [PickTv {
                id: "o".into(),
                name: "TV o".into(),
                model: "UE43AU7100".into()
            }]
        );
        // Two of the same model: the person says which.
        let twin = tv("t", "QE55Q67AA", tizen);
        let TvMatch::Pick(list) = match_tv(
            &[kitchen.clone(), twin, other.clone()],
            Some("QE55Q67AAUXXH"),
        ) else {
            panic!()
        };
        assert_eq!(
            list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["k", "t"]
        );
        // The model unknown here: a single Samsung is it, several are picked from.
        assert_eq!(
            match_tv(std::slice::from_ref(&kitchen), None),
            TvMatch::One("k".into())
        );
        assert!(matches!(
            match_tv(&[kitchen, other], Some("")),
            TvMatch::Pick(l) if l.len() == 2
        ));
    }

    #[test]
    fn the_end_of_the_routine_is_told_plainly() {
        assert_eq!(
            privacy_outcome(PrivacyStep::Done { pressed: true }, None),
            Ok("samsung.status.keysSent")
        );
        assert_eq!(
            privacy_outcome(PrivacyStep::Stopped { why: Stop::Person }, None),
            Ok("samsung.status.stopped")
        );
        // A remote problem shows as that problem.
        assert_eq!(
            privacy_outcome(
                PrivacyStep::Stopped {
                    why: Stop::LaunchFailed
                },
                Some("samsung.err.remoteDenied")
            ),
            Err("samsung.err.remoteDenied")
        );
        assert_eq!(
            privacy_outcome(
                PrivacyStep::Stopped {
                    why: Stop::KeysFailed
                },
                None
            ),
            Err("samsung.err.remoteDown")
        );
        for end in [
            PrivacyStep::Done { pressed: false },
            PrivacyStep::Manual,
            PrivacyStep::Stopped {
                why: Stop::NotOpened,
            },
        ] {
            assert_eq!(
                privacy_outcome(end, None),
                Ok("samsung.status.privacyChecked")
            );
        }
    }
}
