//! A fake ReTV server for the app sign-in routes, with the real one's rules:
//! codes, polling, single-use rotating refresh tokens with a one-time grace
//! for a lost answer, reuse revoking the session, `401 revoked`.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeStatus {
    Pending,
    Approved,
    Denied,
    Consumed,
}

#[derive(Debug, Clone)]
pub struct Code {
    pub user: String,
    pub device: String,
    pub status: CodeStatus,
    pub request: Value,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub access: String,
    pub refresh: String,
    pub prev_refresh: Option<String>,
    pub grace_used: bool,
    pub revoked: bool,
}

#[derive(Default)]
pub struct State {
    pub site: String,
    pub codes: Vec<Code>,
    pub sessions: Vec<Session>,
    pub next: u64,
    /// Every request: method and path.
    pub seen: Vec<String>,
    /// Every refresh token presented, in order.
    pub refresh_seen: Vec<String>,
    /// The next refresh rotates, then the answer is lost (the socket closes).
    pub lose_next_refresh: bool,
    /// The first poll answers slow-down.
    pub slow_down_once: bool,
    /// Answers `expiresIn` for access tokens.
    pub access_ttl: u64,
    pub code_ttl: u64,
    /// `verificationUriComplete` points elsewhere.
    pub foreign_page: bool,
    pub label: String,
    pub pro: bool,
    /// `/apps/*` with a token answer this instead (e.g. 502).
    pub down: bool,
    /// Bridge requests left for a TV: its id and the body.
    pub tv_requests: Vec<(String, Value)>,
}

#[derive(Clone)]
pub struct Fake {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<State>>,
}

fn token(prefix: &str, n: u64) -> String {
    let body = format!("{n:0>43}");
    format!("{prefix}{body}")
}

impl Fake {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn base(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn site(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// «Разрешить» on the site for `user_code`.
    pub fn approve(&self, user_code: &str) {
        let mut s = self.lock();
        for c in s.codes.iter_mut().filter(|c| c.user == user_code) {
            c.status = CodeStatus::Approved;
        }
    }

    pub fn deny(&self, user_code: &str) {
        let mut s = self.lock();
        for c in s.codes.iter_mut().filter(|c| c.user == user_code) {
            c.status = CodeStatus::Denied;
        }
    }

    /// «Выйти» for the app in the site's cabinet.
    pub fn revoke_all(&self) {
        for s in self.lock().sessions.iter_mut() {
            s.revoked = true;
        }
    }

    /// The access tokens stop (as after their hour): a refresh brings new ones.
    pub fn expire_access(&self) {
        let mut s = self.lock();
        let mut n = s.next;
        for sess in s.sessions.iter_mut() {
            n += 1;
            sess.access = token("rta_", 900_000 + n);
        }
        s.next = n;
    }

    pub fn live_sessions(&self) -> usize {
        self.lock().sessions.iter().filter(|s| !s.revoked).count()
    }

    pub fn count(&self, what: &str) -> usize {
        self.lock()
            .seen
            .iter()
            .filter(|s| s.as_str() == what)
            .count()
    }
}

pub async fn start() -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(Mutex::new(State {
        site: format!("http://{addr}"),
        access_ttl: 3600,
        code_ttl: 600,
        label: "p***@example.com".into(),
        pro: true,
        ..State::default()
    }));
    let fake = Fake { addr, state };
    let f = fake.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let f = f.clone();
            tokio::spawn(async move { serve(f, sock).await });
        }
    });
    fake
}

struct Req {
    method: String,
    path: String,
    auth: Option<String>,
    body: Value,
}

async fn read(sock: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let mut len = 0usize;
    let mut auth = None; // leakcheck:allow
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "authorization" => auth = Some(v.trim().to_string()), // leakcheck:allow
                _ => {}
            }
        }
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Req {
        method,
        path,
        auth,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    })
}

/// `None`: drop the connection without an answer.
type Answer = Option<(u16, Value)>;

fn err(status: u16, code: &str) -> Answer {
    Some((status, json!({ "error": code })))
}

fn pair(s: &State, sess: &Session) -> Value {
    json!({
        "tokenType": "Bearer", // leakcheck:allow
        "accessToken": sess.access, // leakcheck:allow
        "expiresIn": s.access_ttl,
        "refreshToken": sess.refresh, // leakcheck:allow
        "scope": ["account:read", "tvs:read"],
        "sessionId": sess.id,
        "cell": "world",
        "account": { "label": s.label, "tier": if s.pro { "pro" } else { "free" }, "pro": s.pro },
    })
}

fn bearer_session<'a>(s: &'a mut State, header: &Option<String>) -> Option<&'a mut Session> {
    let at = header.as_deref()?.strip_prefix("Bearer ")?.to_string(); // leakcheck:allow
    s.sessions.iter_mut().find(|x| x.access == at && !x.revoked)
}

fn answer(f: &Fake, r: &Req) -> Answer {
    let mut s = f.lock();
    s.seen.push(format!("{} {}", r.method, r.path));
    match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/apps/codes") => {
            if r.body["app"] != "bridge" || r.body["name"].as_str().unwrap_or("").is_empty() {
                return err(400, "validation-failed");
            }
            s.next += 1;
            let n = s.next;
            let user = format!("ABCD{n:04}");
            let device = format!("{n:0>43}");
            s.codes.push(Code {
                user: user.clone(),
                device: device.clone(),
                status: CodeStatus::Pending,
                request: r.body.clone(),
            });
            let site = if s.foreign_page {
                "https://evil.example".to_string()
            } else {
                s.site.clone()
            };
            Some((
                200,
                json!({
                    "userCode": user,
                    "deviceCode": device,
                    "verificationUri": format!("{}/go", s.site),
                    "verificationUriComplete": format!("{site}/app-sign-in?code={user}"),
                    "expiresIn": s.code_ttl,
                    "interval": 1,
                    "cell": "world",
                }),
            ))
        }
        ("POST", "/v1/apps/token") => {
            if s.slow_down_once {
                s.slow_down_once = false;
                return err(400, "slow-down");
            }
            let dc = r.body["deviceCode"].as_str().unwrap_or("").to_string();
            let Some(i) = s.codes.iter().position(|c| c.device == dc) else {
                return err(400, "expired-token");
            };
            match s.codes[i].status {
                CodeStatus::Pending => err(400, "authorization-pending"),
                CodeStatus::Denied => err(400, "access-denied"),
                CodeStatus::Consumed => err(400, "expired-token"),
                CodeStatus::Approved => {
                    s.codes[i].status = CodeStatus::Consumed;
                    s.next += 1;
                    let n = s.next;
                    let sess = Session {
                        id: format!("sess{n}"),
                        access: token("rta_", n),
                        refresh: token("rtr_", n),
                        prev_refresh: None,
                        grace_used: false,
                        revoked: false,
                    };
                    let body = pair(&s, &sess);
                    s.sessions.push(sess);
                    Some((200, body))
                }
            }
        }
        ("POST", "/v1/apps/refresh") => {
            let rt = r.body["refreshToken"].as_str().unwrap_or("").to_string(); // leakcheck:allow
            s.refresh_seen.push(rt.clone());
            s.next += 1;
            let n = s.next;
            let lose = std::mem::take(&mut s.lose_next_refresh);
            let Some(i) = s
                .sessions
                .iter()
                .position(|x| x.refresh == rt || x.prev_refresh.as_deref() == Some(&rt))
            else {
                return err(401, "revoked");
            };
            if s.sessions[i].revoked {
                return err(401, "revoked");
            }
            let sess = &mut s.sessions[i];
            if sess.refresh == rt {
                sess.prev_refresh = Some(rt);
                sess.grace_used = false;
            } else if !sess.grace_used {
                // The answer to the last refresh was lost: once more.
                sess.grace_used = true;
            } else {
                sess.revoked = true;
                return err(401, "revoked");
            }
            sess.access = token("rta_", n);
            sess.refresh = token("rtr_", n);
            let sess = sess.clone();
            if lose {
                return None;
            }
            Some((200, pair(&s, &sess)))
        }
        ("GET", "/v1/apps/me") => {
            if s.down {
                return err(502, "");
            }
            let (label, pro) = (s.label.clone(), s.pro);
            let Some(sess) = bearer_session(&mut s, &r.auth) else {
                return err(401, "revoked");
            };
            Some((
                200,
                json!({
                    "sessionId": sess.id, "app": "bridge", "cell": "world", "scope": ["account:read", "tvs:read"],
                    "account": { "label": label, "tier": if pro { "pro" } else { "free" }, "pro": pro },
                }),
            ))
        }
        ("GET", "/v1/apps/tvs") => {
            if bearer_session(&mut s, &r.auth).is_none() {
                return err(401, "revoked");
            }
            Some((
                200,
                json!({ "items": [
                    { "id": "d1", "name": "Гостиная", "model": "MiTV-AFMU0", "kindLine": "Android TV 11",
                      "homeVersion": "1.4.0", "helperVersion": "1.1.0", "helperSeen": "2026-11-03T10:00:00Z" },
                    { "id": "d2", "name": "Кухня", "model": "QE55Q60B", "kindLine": "Samsung (Tizen)",
                      "homeVersion": "0.2.0-tizen", "helperVersion": null, "helperSeen": null,
                      "privacy": { "latEnabled": false, "at": "2026-11-04T10:00:00.000Z", "check": "Ab12Cd34Ef56", "opened": null } },
                ] }),
            ))
        }
        ("POST", path) if path.starts_with("/v1/apps/tvs/") && path.ends_with("/requests") => {
            if bearer_session(&mut s, &r.auth).is_none() {
                return err(401, "revoked");
            }
            let tv = path["/v1/apps/tvs/".len()..path.len() - "/requests".len()].to_string();
            if tv != "d2" {
                return err(404, "device-not-found");
            }
            s.tv_requests.push((tv, r.body.clone()));
            Some((
                200,
                json!({ "id": "r1", "expiresAt": "2026-11-04T10:02:00.000Z" }),
            ))
        }
        ("POST", "/v1/apps/sign-out") => {
            let Some(sess) = bearer_session(&mut s, &r.auth) else {
                return err(401, "revoked");
            };
            sess.revoked = true;
            Some((204, Value::Null))
        }
        _ => err(404, "not-found"),
    }
}

async fn serve(f: Fake, mut sock: TcpStream) {
    while let Some(req) = read(&mut sock).await {
        let Some((status, body)) = answer(&f, &req) else {
            return;
        };
        let text = if body.is_null() {
            String::new()
        } else {
            body.to_string()
        };
        let head = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            text.len()
        );
        if sock.write_all(head.as_bytes()).await.is_err()
            || sock.write_all(text.as_bytes()).await.is_err()
        {
            return;
        }
    }
}
