//! The whole app sign-in against a fake ReTV server: code, browser page,
//! polling (slow-down, deny, expiry, cancel), tokens in the key store,
//! refresh rotation (lost answer, no reuse), revoke from the site, restore,
//! sign-out.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use retv_bridge_account::store::Saved;
use retv_bridge_account::{
    Account, ApiError, Cell, CodeRequest, Endpoints, Fail, KeyStore, MemoryStore, Phase, SignInEnd,
    Timing,
};
use support::Fake;
use zeroize::Zeroizing;

struct FakeSites {
    api: String,
    site: String,
}

impl Endpoints for FakeSites {
    fn api(&self, _: Cell) -> String {
        self.api.clone()
    }
    fn site(&self, _: Cell) -> String {
        self.site.clone()
    }
}

/// A key store the test can read too.
#[derive(Clone, Default)]
struct SharedStore(Arc<MemoryStore>);

impl KeyStore for SharedStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        self.0.load()
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        self.0.save(bytes)
    }
    fn delete(&self) -> Result<(), String> {
        self.0.delete()
    }
}

impl SharedStore {
    fn saved(&self) -> Option<Saved> {
        self.0.load().unwrap().and_then(|b| Saved::from_bytes(&b))
    }
    fn raw(&self) -> String {
        self.0
            .load()
            .unwrap()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .unwrap_or_default()
    }
}

const FAST: Timing = Timing {
    unit: Duration::from_millis(10),
    refresh_early: Duration::from_millis(0),
};

fn account(fake: &Fake, store: &SharedStore) -> (Arc<Account>, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let l = lines.clone();
    let acc = Account::new(
        Box::new(FakeSites {
            api: fake.base(),
            site: fake.site(),
        }),
        Box::new(store.clone()),
        FAST,
        Box::new(move |line| l.lock().unwrap().push(line)),
    );
    (Arc::new(acc), lines)
}

fn req() -> CodeRequest {
    CodeRequest {
        app: "bridge",
        name: "Test Mac".into(),
        platform: "macOS 15".into(),
        app_version: "1.0.1".into(),
    }
}

fn user_code_of(url: &str) -> String {
    url.rsplit_once("code=").unwrap().1.to_string()
}

/// Signs in, the person allowing it as soon as the page opens.
async fn signed_in(fake: &Fake, acc: &Account) {
    let f = fake.clone();
    let end = acc
        .sign_in(Cell::World, req(), move |url| f.approve(&user_code_of(url)))
        .await;
    assert!(matches!(end, SignInEnd::SignedIn(_)), "{end:?}");
}

fn no_token_in_logs(lines: &Arc<Mutex<Vec<String>>>) {
    for l in lines.lock().unwrap().iter() {
        assert!(
            !l.contains("rta_") && !l.contains("rtr_"),
            "a token in the log: {l}"
        );
        assert!(l.starts_with("[account] "), "{l}");
    }
}

#[tokio::test]
async fn the_whole_flow_signs_in_and_reads_the_tvs() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, lines) = account(&fake, &store);
    let opened = Arc::new(Mutex::new(None));
    let (o, f) = (opened.clone(), fake.clone());
    let end = acc
        .sign_in(Cell::World, req(), move |url| {
            *o.lock().unwrap() = Some(url.to_string());
            f.approve(&user_code_of(url));
        })
        .await;
    let SignInEnd::SignedIn(view) = end else {
        panic!("{end:?}");
    };
    assert_eq!(view.label.as_deref(), Some("p***@example.com"));
    assert!(view.pro);
    let url = opened.lock().unwrap().clone().unwrap();
    assert!(url.starts_with(&format!("{}/app-sign-in?code=", fake.site())));
    // What the app said about itself.
    let sent = fake.lock().codes[0].request.clone();
    assert_eq!(sent["app"], "bridge");
    assert_eq!(sent["name"], "Test Mac");
    assert_eq!(sent["platform"], "macOS 15");
    assert_eq!(sent["appVersion"], "1.0.1");

    let snap = acc.snapshot();
    assert_eq!(snap.phase, Phase::Done);
    assert_eq!(snap.cell, Some(Cell::World));
    // Only the refresh token is kept; the access token never leaves memory.
    let saved = store.saved().unwrap();
    assert_eq!(saved.refresh.expose(), fake.lock().sessions[0].refresh);
    assert!(!store.raw().contains("rta_"));

    let tvs = acc.refresh_tvs().await.unwrap();
    assert_eq!(tvs.len(), 2);
    assert_eq!(tvs[0].name, "Гостиная");
    assert_eq!(tvs[1].home_version.as_deref(), Some("0.2.0-tizen"));
    // An older server sends no privacy; a Samsung TV's report comes with its check id.
    assert_eq!(tvs[0].privacy, None);
    let p = tvs[1].privacy.clone().unwrap();
    assert_eq!(
        (p.lat_enabled, p.check.as_deref(), p.opened),
        (Some(false), Some("Ab12Cd34Ef56"), None)
    );
    assert_eq!(acc.snapshot().tvs.unwrap().len(), 2);
    acc.dismiss();
    assert_eq!(acc.snapshot().phase, Phase::Idle);
    no_token_in_logs(&lines);
}

#[tokio::test]
async fn slow_down_waits_longer_and_still_signs_in() {
    let fake = support::start().await;
    fake.lock().slow_down_once = true;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    assert!(fake.count("POST /v1/apps/token") >= 2);
}

#[tokio::test]
async fn deny_on_the_site_fails_the_attempt() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    let f = fake.clone();
    let end = acc
        .sign_in(Cell::World, req(), move |url| f.deny(&user_code_of(url)))
        .await;
    assert_eq!(end, SignInEnd::Failed(Fail::Denied));
    assert_eq!(
        acc.snapshot().phase,
        Phase::Failed {
            cell: Cell::World,
            fail: Fail::Denied
        }
    );
    assert!(!acc.signed_in());
    assert!(store.saved().is_none());
}

#[tokio::test]
async fn a_code_nobody_answers_runs_out() {
    let fake = support::start().await;
    fake.lock().code_ttl = 4;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    let end = acc.sign_in(Cell::World, req(), |_| {}).await;
    assert_eq!(end, SignInEnd::Failed(Fail::Expired));
}

#[tokio::test]
async fn a_page_off_the_site_is_never_opened() {
    let fake = support::start().await;
    fake.lock().foreign_page = true;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    let opened = Arc::new(Mutex::new(false));
    let o = opened.clone();
    let end = acc
        .sign_in(Cell::World, req(), move |_| *o.lock().unwrap() = true)
        .await;
    assert_eq!(end, SignInEnd::Failed(Fail::Server));
    assert!(!*opened.lock().unwrap());
}

#[tokio::test]
async fn cancel_stops_waiting_and_a_late_allow_is_ended() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    let a = acc.clone();
    let run = tokio::spawn(async move { a.sign_in(Cell::World, req(), |_| {}).await });
    let user = loop {
        if let Phase::Waiting { user_code, .. } = acc.snapshot().phase {
            break user_code;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    acc.cancel();
    assert_eq!(run.await.unwrap(), SignInEnd::Cancelled);
    assert_eq!(acc.snapshot().phase, Phase::Idle);
    fake.approve(&user);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!acc.signed_in());
    assert!(store.saved().is_none());
}

#[tokio::test]
async fn refresh_rotates_and_never_reuses_a_refresh_token() {
    let fake = support::start().await;
    fake.lock().access_ttl = 0; // every call needs a new access token
    let store = SharedStore::default();
    let (acc, lines) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    let first = store.saved().unwrap().refresh;
    acc.refresh_tvs().await.unwrap();
    acc.refresh_account().await.unwrap();
    acc.refresh_tvs().await.unwrap();
    let seen = fake.lock().refresh_seen.clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0], first.expose());
    let mut uniq = seen.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), seen.len(), "a refresh token was sent twice");
    // The key store follows every rotation.
    assert_eq!(
        store.saved().unwrap().refresh.expose(),
        fake.lock().sessions[0].refresh
    );
    assert_eq!(fake.live_sessions(), 1);
    no_token_in_logs(&lines);
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    fake.expire_access();
    assert_eq!(acc.refresh_tvs().await.unwrap().len(), 2);
    assert_eq!(fake.lock().refresh_seen.len(), 1);
    assert!(acc.signed_in());
}

#[tokio::test]
async fn parallel_calls_refresh_one_at_a_time() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    fake.expire_access();
    let (a, b) = tokio::join!(acc.refresh_tvs(), acc.refresh_account());
    assert!(a.is_ok() && b.is_ok(), "{a:?} {b:?}");
    let seen = fake.lock().refresh_seen.clone();
    let mut uniq = seen.clone();
    uniq.dedup();
    assert_eq!(uniq.len(), seen.len(), "a refresh token was sent twice");
    assert_eq!(fake.live_sessions(), 1);
}

#[tokio::test]
async fn a_lost_refresh_answer_is_retried_with_the_same_token() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    fake.expire_access();
    fake.lock().lose_next_refresh = true;
    let r = acc.refresh_tvs().await;
    assert!(matches!(r, Err(ApiError::Network(_))), "{r:?}");
    assert!(acc.signed_in(), "a network loss keeps the sign-in");
    assert!(acc.snapshot().offline);
    // The server rotated, but Bridge never got the answer: the old token works once more.
    assert_eq!(acc.refresh_tvs().await.unwrap().len(), 2);
    let seen = fake.lock().refresh_seen.clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1]);
    assert!(!acc.snapshot().offline);
    assert_eq!(fake.live_sessions(), 1);
    assert_eq!(
        store.saved().unwrap().refresh.expose(),
        fake.lock().sessions[0].refresh
    );
}

#[tokio::test]
async fn revoked_on_the_site_signs_out_here() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    acc.refresh_tvs().await.unwrap();
    fake.revoke_all();
    assert_eq!(acc.refresh_tvs().await, Err(ApiError::Revoked));
    assert!(!acc.signed_in());
    let snap = acc.snapshot();
    assert!(snap.account.is_none() && snap.tvs.is_none());
    assert!(store.saved().is_none(), "the key store entry is removed");
}

#[tokio::test]
async fn a_later_run_restores_the_sign_in_from_the_key_store() {
    let fake = support::start().await;
    let store = SharedStore::default();
    {
        let (acc, _) = account(&fake, &store);
        signed_in(&fake, &acc).await;
    }
    let (acc, _) = account(&fake, &store);
    assert!(acc.restore());
    assert!(acc.signed_in());
    assert_eq!(
        acc.snapshot().account.unwrap().label.as_deref(),
        Some("p***@example.com")
    );
    // No access token yet: the first call refreshes.
    fake.lock().pro = false;
    let view = acc.refresh_account().await.unwrap();
    assert!(!view.pro);
    assert_eq!(fake.lock().refresh_seen.len(), 1);
    assert!(
        !store.saved().unwrap().account.pro,
        "the saved view follows"
    );
}

#[tokio::test]
async fn an_unreadable_entry_is_dropped() {
    let fake = support::start().await;
    let store = SharedStore::default();
    store.save(b"{not json").unwrap();
    let (acc, _) = account(&fake, &store);
    assert!(!acc.restore());
    assert!(store.0.load().unwrap().is_none());
}

#[tokio::test]
async fn sign_out_ends_the_session_on_the_server_too() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    assert_eq!(fake.live_sessions(), 1);
    acc.sign_out().await;
    assert!(!acc.signed_in());
    assert!(store.saved().is_none());
    assert_eq!(fake.live_sessions(), 0);
    assert_eq!(fake.count("POST /v1/apps/sign-out"), 1);
}

#[tokio::test]
async fn sign_out_without_network_still_signs_out_here() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, _) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    fake.expire_access();
    fake.lock().lose_next_refresh = true;
    acc.sign_out().await;
    assert!(!acc.signed_in());
    assert!(store.saved().is_none());
}

#[tokio::test]
async fn no_network_at_the_start_says_so() {
    // A port nothing listens on.
    let sock = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    let acc = Account::new(
        Box::new(FakeSites {
            api: format!("http://{addr}/v1"),
            site: format!("http://{addr}"),
        }),
        Box::new(MemoryStore::default()),
        FAST,
        Box::new(|_| {}),
    );
    assert_eq!(
        acc.sign_in(Cell::Ru, req(), |_| {}).await,
        SignInEnd::Failed(Fail::Network)
    );
    assert_eq!(
        acc.snapshot().phase,
        Phase::Failed {
            cell: Cell::Ru,
            fail: Fail::Network
        }
    );
}

#[tokio::test]
async fn a_request_for_one_of_the_accounts_tvs_goes_to_the_server() {
    let fake = support::start().await;
    let store = SharedStore::default();
    let (acc, lines) = account(&fake, &store);
    signed_in(&fake, &acc).await;
    acc.request_tv("d2", "privacy", "Ab12Cd34Ef56")
        .await
        .unwrap();
    let sent = fake.lock().tv_requests.clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "d2");
    assert_eq!(
        sent[0].1,
        serde_json::json!({ "kind": "privacy", "check": "Ab12Cd34Ef56" })
    );
    // Another account's TV (or one gone): the server's 404.
    assert_eq!(
        acc.request_tv("d9", "privacy-check", "Zz99Zz99").await,
        Err(ApiError::Server(404, "device-not-found".into()))
    );
    // Nothing but an id goes into the path.
    assert!(matches!(
        acc.request_tv("../me", "privacy", "Ab12Cd34Ef56").await,
        Err(ApiError::Server(404, _))
    ));
    assert_eq!(fake.lock().tv_requests.len(), 1);
    assert!(!fake.lock().seen.iter().any(|l| l.contains("..")));
    no_token_in_logs(&lines);
}
