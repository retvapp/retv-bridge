//! The sign-in as Bridge lives it: signed out, asking for a code, waiting for
//! the person on the site, signed in. Tokens are refreshed one at a time
//! (the refresh token rotates); a `401` from the refresh ends the sign-in
//! here too. Nothing in this module logs a token.

use std::future::Future;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::Notify;

use crate::api::{AccountView, Api, ApiError, CodeRequest, TokenAnswer, Tv};
use crate::cell::Cell;
use crate::secret::Secret;
use crate::store::{KeyStore, Saved};

/// Where each cell's API and site are.
pub trait Endpoints: Send + Sync {
    /// `https://retv.app/v1`.
    fn api(&self, cell: Cell) -> String;
    /// `https://retv.app`: the page Bridge opens must be on it.
    fn site(&self, cell: Cell) -> String;
}

/// The real sites.
pub struct ReTvSites;

impl Endpoints for ReTvSites {
    fn api(&self, cell: Cell) -> String {
        cell.api_base()
    }
    fn site(&self, cell: Cell) -> String {
        cell.site().to_string()
    }
}

/// How long things take (tests make a second short).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// One "second" of the server's `interval` and `expiresIn`.
    pub unit: Duration,
    /// The access token is refreshed this long before it runs out.
    pub refresh_early: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            unit: Duration::from_secs(1),
            refresh_early: Duration::from_secs(60),
        }
    }
}

/// Why a sign-in attempt stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Fail {
    /// «Отклонить» on the site.
    Denied,
    /// The code ran out before the person allowed it.
    Expired,
    /// No answer from the site.
    Network,
    RateLimited,
    /// Anything else the server said, or a page not on the site.
    Server,
}

impl Fail {
    fn of(e: &ApiError) -> Fail {
        match e {
            ApiError::Denied => Fail::Denied,
            ApiError::Expired => Fail::Expired,
            ApiError::Network(_) => Fail::Network,
            ApiError::RateLimited => Fail::RateLimited,
            _ => Fail::Server,
        }
    }
}

/// What the sign-in sheet shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Phase {
    /// No attempt running (signed in or not: see `Snapshot::account`).
    #[default]
    Idle,
    Starting {
        cell: Cell,
    },
    Waiting {
        cell: Cell,
        user_code: String,
        verification_uri: String,
        verification_uri_complete: String,
        /// Wall clock, ms.
        expires_at_ms: u64,
    },
    /// The attempt just ended well (the sheet says so until it is closed).
    Done,
    Failed {
        cell: Cell,
        fail: Fail,
    },
}

/// How a `sign_in` call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInEnd {
    SignedIn(AccountView),
    Cancelled,
    Failed(Fail),
}

/// Everything the window needs, no tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub phase: Phase,
    /// The cell of the live sign-in.
    pub cell: Option<Cell>,
    pub account: Option<AccountView>,
    /// The account's TVs, once read.
    pub tvs: Option<Vec<Tv>>,
    /// The last call found no network (the sign-in is kept).
    pub offline: bool,
}

struct Held {
    cell: Cell,
    session_id: String,
    refresh: Secret,
    access: Option<(Secret, Instant)>,
    account: AccountView,
    /// Bumped on every new sign-in: an answer for an older one is dropped.
    generation: u64,
}

struct Inner {
    phase: Phase,
    attempt: u64,
    held: Option<Held>,
    generation: u64,
    tvs: Option<Vec<Tv>>,
    offline: bool,
}

pub type Log = Box<dyn Fn(String) + Send + Sync>;

pub struct Account {
    endpoints: Box<dyn Endpoints>,
    store: Box<dyn KeyStore>,
    timing: Timing,
    log: Log,
    inner: Mutex<Inner>,
    /// One refresh at a time: the refresh token is single-use.
    refreshing: tokio::sync::Mutex<()>,
    /// Key store writes take turns (`persist`, `forget`).
    saving: Mutex<()>,
    wake: Notify,
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Account {
    pub fn new(
        endpoints: Box<dyn Endpoints>,
        store: Box<dyn KeyStore>,
        timing: Timing,
        log: Log,
    ) -> Account {
        Account {
            endpoints,
            store,
            timing,
            log,
            inner: Mutex::new(Inner {
                phase: Phase::Idle,
                attempt: 0,
                held: None,
                generation: 0,
                tvs: None,
                offline: false,
            }),
            refreshing: tokio::sync::Mutex::new(()),
            saving: Mutex::new(()),
            wake: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn api(&self, cell: Cell) -> Api {
        Api::new(self.endpoints.api(cell))
    }

    fn log(&self, line: String) {
        (self.log)(format!("[account] {line}"));
    }

    pub fn snapshot(&self) -> Snapshot {
        let s = self.lock();
        Snapshot {
            phase: s.phase.clone(),
            cell: s.held.as_ref().map(|h| h.cell),
            account: s.held.as_ref().map(|h| h.account.clone()),
            tvs: s.tvs.clone(),
            offline: s.offline,
        }
    }

    pub fn signed_in(&self) -> bool {
        self.lock().held.is_some()
    }

    /// The sign-in saved by an earlier run, if any: signed in at once (the
    /// header shows the saved label); the first call refreshes the tokens.
    pub fn restore(&self) -> bool {
        let bytes = match self.store.load() {
            Ok(Some(b)) => b,
            Ok(None) => return false,
            Err(e) => {
                self.log(format!("could not read the saved sign-in: {e}"));
                return false;
            }
        };
        let Some(saved) = Saved::from_bytes(&bytes) else {
            self.log("the saved sign-in is unreadable: signed out".into());
            let _ = self.store.delete();
            return false;
        };
        let mut s = self.lock();
        s.generation += 1;
        s.held = Some(Held {
            cell: saved.cell,
            session_id: saved.session_id,
            refresh: saved.refresh,
            access: None,
            account: saved.account,
            generation: s.generation,
        });
        drop(s);
        self.log(format!("signed in ({})", saved.cell.host()));
        true
    }

    /// Writes the live sign-in to the key store. Writers take turns and each
    /// writes what is live when its turn comes, so an older refresh token can
    /// never land after a newer one. Never called with `inner` locked.
    fn persist(&self) {
        let _turn = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        let saved = {
            let s = self.lock();
            let Some(h) = s.held.as_ref() else {
                return;
            };
            Saved {
                v: 1,
                cell: h.cell,
                session_id: h.session_id.clone(),
                refresh: h.refresh.clone(),
                account: h.account.clone(),
            }
        };
        if let Err(e) = self.store.save(&saved.to_bytes()) {
            self.log(format!(
                "could not save the sign-in (it lasts until Bridge quits): {e}"
            ));
        }
    }

    fn adopt(&self, cell: Cell, t: TokenAnswer) -> AccountView {
        {
            let mut s = self.lock();
            s.generation += 1;
            s.held = Some(Held {
                cell,
                session_id: t.session_id,
                refresh: t.refresh_token,
                access: Some((
                    t.access_token,
                    Instant::now() + self.timing.unit * t.expires_in as u32,
                )),
                account: t.account.clone(),
                generation: s.generation,
            });
            s.tvs = None;
            s.offline = false;
        }
        self.persist();
        t.account
    }

    fn current(&self, attempt: u64) -> bool {
        self.lock().attempt == attempt
    }

    fn fail(&self, attempt: u64, cell: Cell, fail: Fail) -> SignInEnd {
        let mut s = self.lock();
        if s.attempt != attempt {
            return SignInEnd::Cancelled;
        }
        s.phase = Phase::Failed { cell, fail };
        SignInEnd::Failed(fail)
    }

    /// Sleeps `d`, or less if the attempt is cancelled.
    async fn nap(&self, d: Duration) {
        let _ = tokio::time::timeout(d, self.wake.notified()).await;
    }

    /// The whole sign-in on `cell`: a code, `opened` (Bridge opens the page in
    /// the browser), then polling until the person answers on the site, the
    /// code runs out or `cancel` is called.
    pub async fn sign_in(
        &self,
        cell: Cell,
        req: CodeRequest,
        opened: impl FnOnce(&str),
    ) -> SignInEnd {
        let attempt = {
            let mut s = self.lock();
            s.attempt += 1;
            s.phase = Phase::Starting { cell };
            s.attempt
        };
        let api = self.api(cell);
        self.log(format!("asking {} for a sign-in code", cell.host()));
        let code = match api.codes(&req).await {
            Ok(c) => c,
            Err(e) => {
                self.log(format!("no code: {e}"));
                return self.fail(attempt, cell, Fail::of(&e));
            }
        };
        let site = format!("{}/", self.endpoints.site(cell).trim_end_matches('/'));
        if !code.verification_uri_complete.starts_with(&site)
            || !code.verification_uri.starts_with(&site)
        {
            self.log("the sign-in page is not on the ReTV site: not opened".into());
            return self.fail(attempt, cell, Fail::Server);
        }
        let deadline = Instant::now() + self.timing.unit * code.expires_in as u32;
        {
            let mut s = self.lock();
            if s.attempt != attempt {
                return SignInEnd::Cancelled;
            }
            s.phase = Phase::Waiting {
                cell,
                user_code: code.user_code.clone(),
                verification_uri: code.verification_uri.clone(),
                verification_uri_complete: code.verification_uri_complete.clone(),
                expires_at_ms: wall_ms() + code.expires_in * 1000,
            };
        }
        opened(&code.verification_uri_complete);
        let mut interval = code.interval.max(1);
        loop {
            self.nap(self.timing.unit * interval as u32).await;
            if !self.current(attempt) {
                return SignInEnd::Cancelled;
            }
            if Instant::now() >= deadline {
                self.log("the code ran out".into());
                return self.fail(attempt, cell, Fail::Expired);
            }
            match api.token(&code.device_code).await {
                Ok(t) => {
                    if !self.current(attempt) {
                        // Allowed on the site after «Отменить»: end that session.
                        let _ = api.sign_out(&t.access_token).await;
                        return SignInEnd::Cancelled;
                    }
                    let account = self.adopt(cell, t);
                    self.lock().phase = Phase::Done;
                    self.log(format!("signed in ({})", cell.host()));
                    return SignInEnd::SignedIn(account);
                }
                Err(ApiError::Pending) => {}
                Err(ApiError::SlowDown) => interval += 5,
                Err(ApiError::RateLimited) => interval = (interval * 2).min(60),
                // The network may come back before the code runs out.
                Err(ApiError::Network(_)) => {}
                Err(e) => {
                    self.log(format!("sign-in stopped: {e}"));
                    return self.fail(attempt, cell, Fail::of(&e));
                }
            }
        }
    }

    /// «Отменить»: stops the running attempt.
    pub fn cancel(&self) {
        let mut s = self.lock();
        s.attempt += 1;
        s.phase = Phase::Idle;
        drop(s);
        self.wake.notify_waiters();
    }

    /// Closes the sheet after success or a failure.
    pub fn dismiss(&self) {
        let mut s = self.lock();
        if matches!(s.phase, Phase::Done | Phase::Failed { .. }) {
            s.phase = Phase::Idle;
        }
    }

    /// Ends the sign-in here (the server already did, or the person asked).
    fn forget(&self, generation: u64) {
        let _turn = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut s = self.lock();
            if s.held.as_ref().map(|h| h.generation) != Some(generation) {
                return;
            }
            s.held = None;
            s.tvs = None;
            s.offline = false;
        }
        if let Err(e) = self.store.delete() {
            self.log(format!("could not remove the saved sign-in: {e}"));
        }
    }

    /// A live access token, refreshed first when it is (nearly) out.
    async fn access(&self, force: bool) -> Result<(Api, Secret, u64), ApiError> {
        let _one = self.refreshing.lock().await;
        let (cell, refresh, generation) = {
            let s = self.lock();
            let Some(h) = s.held.as_ref() else {
                return Err(ApiError::Revoked);
            };
            if let Some((at, until)) = &h.access {
                if !force && *until > Instant::now() + self.timing.refresh_early {
                    return Ok((self.api(h.cell), at.clone(), h.generation));
                }
            }
            (h.cell, h.refresh.clone(), h.generation)
        };
        let api = self.api(cell);
        match api.refresh(&refresh).await {
            Ok(t) => {
                let mut s = self.lock();
                let Some(h) = s.held.as_mut().filter(|h| h.generation == generation) else {
                    return Err(ApiError::Revoked);
                };
                let access = t.access_token.clone();
                h.refresh = t.refresh_token;
                h.access = Some((
                    t.access_token,
                    Instant::now() + self.timing.unit * t.expires_in as u32,
                ));
                h.account = t.account;
                h.session_id = t.session_id;
                s.offline = false;
                drop(s);
                // Saved before anything uses the new pair: the old refresh token is spent.
                self.persist();
                Ok((api, access, generation))
            }
            Err(ApiError::Revoked) => {
                self.log("the sign-in was ended on the site: signed out".into());
                self.forget(generation);
                Err(ApiError::Revoked)
            }
            Err(e) => {
                if matches!(e, ApiError::Network(_)) {
                    self.lock().offline = true;
                }
                // The refresh token is kept: a lost answer is covered by the server's grace.
                self.log(format!("could not refresh the sign-in: {e}"));
                Err(e)
            }
        }
    }

    /// Runs `call` with an access token; a `401` refreshes once and retries,
    /// and a second `401` ends the sign-in.
    async fn with_access<T, F, Fut>(&self, call: F) -> Result<T, ApiError>
    where
        F: Fn(Api, Secret) -> Fut,
        Fut: Future<Output = Result<T, ApiError>>,
    {
        let (api, at, _) = self.access(false).await?;
        match call(api, at).await {
            Err(ApiError::Revoked) => {
                let (api, at, generation) = self.access(true).await?;
                let r = call(api, at).await;
                if matches!(r, Err(ApiError::Revoked)) {
                    self.forget(generation);
                }
                r
            }
            Err(ApiError::Network(e)) => {
                self.lock().offline = true;
                Err(ApiError::Network(e))
            }
            r => {
                if r.is_ok() {
                    self.lock().offline = false;
                }
                r
            }
        }
    }

    /// Reads the account (label, plan) again.
    pub async fn refresh_account(&self) -> Result<AccountView, ApiError> {
        let me = self
            .with_access(|api, at| async move { api.me(&at).await })
            .await?;
        let changed = match self.lock().held.as_mut() {
            Some(h) if h.account != me.account => {
                h.account = me.account.clone();
                true
            }
            _ => false,
        };
        if changed {
            self.persist();
        }
        Ok(me.account)
    }

    /// Reads the account's TVs.
    pub async fn refresh_tvs(&self) -> Result<Vec<Tv>, ApiError> {
        let tvs = self
            .with_access(|api, at| async move { api.tvs(&at).await })
            .await?;
        let mut s = self.lock();
        if s.held.is_some() {
            s.tvs = Some(tvs.clone());
        }
        Ok(tvs)
    }

    /// Leaves a request (`privacy`, `privacy-check`) with `check` for the
    /// account's TV `tv_id`; ReTV Home on it takes it from the server.
    pub async fn request_tv(&self, tv_id: &str, kind: &str, check: &str) -> Result<(), ApiError> {
        self.with_access(|api, at| async move { api.tv_request(&at, tv_id, kind, check).await })
            .await
    }

    /// «Выйти»: signed out here at once, then on the server (best effort).
    pub async fn sign_out(&self) {
        let live = self.lock().held.as_ref().map(|h| h.generation);
        let Some(generation) = live else {
            return;
        };
        // A token for the server's sign-out, refreshed if need be.
        let access = self.access(false).await;
        self.forget(generation);
        self.log("signed out".into());
        if let Ok((api, at, _)) = access {
            if let Err(e) = api.sign_out(&at).await {
                self.log(format!("the site did not hear the sign-out: {e}"));
            }
        }
    }
}
