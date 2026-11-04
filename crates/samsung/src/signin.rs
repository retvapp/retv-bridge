//! Signing in to Samsung for the certificate. The person signs in on
//! Samsung's own page in their default browser (Bridge never sees the
//! password); Samsung posts the result to `http://localhost:4794/signin/callback`,
//! which only Bridge listens on, and only for the sign-in.
//!
//! The callback listener must own both loopback addresses (a program already
//! on the port could take the sign-in), takes only a POST from an https
//! samsung.com page, and closes after the first such callback.
//!
//! The access token is kept in memory only, zeroed on drop, never logged and
//! never shown (`Debug` redacts it); the request bytes and parsed fields that
//! carried it are zeroed too.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

use crate::SamsungError;

pub const CALLBACK_PORT: u16 = 4794;
const MAX_REQUEST: usize = 64 * 1024;

/// Samsung's sign-in for Tizen certificates (the client id Samsung's own
/// Tizen tools use), the way `tools/samsung/cert.js` does it.
pub fn sign_in_url() -> String {
    format!(
        "https://account.samsung.com/mobile/account/check.do?serviceID=v285zxnl3h&actionID=StartOAuth2&accessToken=Y&redirect_uri=http://localhost:{CALLBACK_PORT}/signin/callback"
    )
}

pub struct SignIn {
    pub access: Zeroizing<String>,
    pub user_id: String,
    /// The account's email, when Samsung sends it.
    pub email: Option<String>,
}

impl fmt::Debug for SignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignIn")
            .field("access", &"<redacted>")
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .finish()
    }
}

/// The pages the browser shows after the callback.
pub struct Pages {
    pub done: String,
    pub retry: String,
}

/// The OS says this address family or address does not exist here (no IPv6).
fn address_unavailable(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::AddrNotAvailable {
        return true;
    }
    // EAFNOSUPPORT: macOS 47, Linux 97, Windows WSAEAFNOSUPPORT 10047.
    let afnosupport = if cfg!(target_os = "macos") {
        47
    } else if cfg!(windows) {
        10047
    } else {
        97
    };
    e.raw_os_error() == Some(afnosupport)
}

/// Binds the callback on 127.0.0.1 and ::1. Any failure but «this computer
/// has no IPv6 loopback» stops the sign-in: another program on the port
/// would get Samsung's answer.
pub async fn bind_callback(port: u16) -> Result<Vec<TcpListener>, SamsungError> {
    let mut listeners = Vec::new();
    match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => listeners.push(l),
        Err(e) => return Err(SamsungError::CallbackPort(e.to_string())),
    }
    match TcpListener::bind(("::1", port)).await {
        Ok(l) => listeners.push(l),
        Err(e) if address_unavailable(&e) => {}
        Err(e) => return Err(SamsungError::CallbackPort(e.to_string())),
    }
    Ok(listeners)
}

/// An `Origin` (or `Referer`) on an https samsung.com host.
pub fn samsung_origin(value: &str) -> bool {
    let Some(rest) = value.trim().strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(h, _)| h)
        .to_ascii_lowercase();
    host == "samsung.com" || host.ends_with(".samsung.com")
}

/// `application/x-www-form-urlencoded` decoding.
pub fn form_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    // No copy on the usual (UTF-8) path; a lossy copy zeroes the original.
    String::from_utf8(out).unwrap_or_else(|e| {
        let mut bytes = e.into_bytes();
        let s = String::from_utf8_lossy(&bytes).into_owned();
        bytes.zeroize();
        s
    })
}

type Fields = HashMap<String, Zeroizing<String>>;

fn pairs(s: &str, into: &mut Fields) {
    for part in s.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        into.insert(form_decode(k), Zeroizing::new(form_decode(v)));
    }
}

fn json_into(s: &str, into: &mut Fields) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(s) else {
        return false;
    };
    for (k, v) in map {
        let text = match v {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => continue,
        };
        into.insert(k, Zeroizing::new(text));
    }
    true
}

fn pick(fields: &Fields, names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| {
        fields
            .get(*n)
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.as_str().to_string())
    })
}

/// Reads the callback: query, form body or JSON body; Samsung's `code` holds
/// JSON, sometimes URL-encoded once more. Every parsed value is zeroed when
/// this returns.
pub fn parse_callback(query: &str, body: &str) -> Option<SignIn> {
    let mut fields = Fields::new();
    pairs(query, &mut fields);
    if !json_into(body, &mut fields) {
        pairs(body, &mut fields);
    }
    if let Some(code) = fields.get("code") {
        let mut candidate = Zeroizing::new(code.as_str().to_string());
        for _ in 0..3 {
            if json_into(&candidate, &mut fields) {
                break;
            }
            candidate = Zeroizing::new(form_decode(&candidate));
        }
    }
    let access = Zeroizing::new(pick(&fields, &["access_token", "accessToken", "token"])?);
    let user_id = pick(
        &fields,
        &["userId", "user_id", "userID", "api_server_url_userid"],
    )?;
    let email = pick(&fields, &["inputEmailID", "email", "login_id"]);
    Some(SignIn {
        access,
        user_id,
        email,
    })
}

/// One request; its query and body (which carry the token) are zeroed on
/// drop.
struct Request {
    method: String,
    path: String,
    origin: Option<String>,
    referer: Option<String>,
    query: Zeroizing<String>,
    body: Zeroizing<String>,
}

async fn read_request(sock: &mut TcpStream) -> Option<Request> {
    let mut buf = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0u8; 4096]);
    let head_end = loop {
        let n = sock.read(&mut chunk[..]).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_REQUEST {
            return None;
        }
    };
    let head = Zeroizing::new(String::from_utf8_lossy(&buf[..head_end]).into_owned());
    let mut first = head.lines().next()?.split(' ');
    let method = first.next()?.to_string();
    let target = Zeroizing::new(first.next()?.to_string());
    let header = |name: &str| {
        head.lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    let len: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .min(MAX_REQUEST);
    let origin = header("origin");
    let referer = header("referer");
    let mut body = Zeroizing::new(buf[head_end..].to_vec());
    while body.len() < len {
        let n = sock.read(&mut chunk[..]).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Request {
        method,
        path: path.to_string(),
        origin,
        referer,
        query: Zeroizing::new(query.to_string()),
        body: Zeroizing::new(String::from_utf8_lossy(&body).into_owned()),
    })
}

impl Request {
    /// Refuses a callback that names another site; accepts one from samsung.com or one whose browser hid where it
    /// came from. Samsung's sign-in ends in a cross-site redirect chain, after which Chrome sends `Origin: null` and
    /// no Referer (seen 2026-11-03 on the owner's Mac: the strict check answered 403 to the real Samsung callback).
    /// The port is ours alone (`bind_callback` refuses a taken port), so the remaining risk is a page that forges a
    /// sign-in during the wait; it can only make the certificate come from another Samsung account.
    fn sent_by_samsung(&self) -> bool {
        let hidden = |v: Option<&str>| {
            v.map(str::trim)
                .filter(|v| !v.is_empty() && *v != "null")
                .is_none()
        };
        match self
            .origin
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty() && *o != "null")
        {
            Some(origin) => samsung_origin(origin),
            None => {
                hidden(self.referer.as_deref())
                    || self.referer.as_deref().is_some_and(samsung_origin)
            }
        }
    }
}

async fn respond(sock: &mut TcpStream, status: &str, html: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncache-control: no-store\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{html}",
        html.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// Serves the callback on `listeners` (from `bind_callback`) until the first
/// POST to `/signin/callback` from an https samsung.com page, or `timeout`.
/// Anything else is refused (404, 405, 403) and the wait goes on; after that
/// one callback the listeners close, with the sign-in or an error.
///
/// Dropping the returned future (the person cancelled the sign-in) closes
/// the listeners at once too, so the next sign-in can bind the port again.
pub async fn wait_for_sign_in(
    listeners: Vec<TcpListener>,
    timeout: Duration,
    pages: Pages,
) -> Result<SignIn, SamsungError> {
    let (tx, mut rx) = mpsc::channel::<TcpStream>(8);
    let mut accepts = AbortOnDrop(Vec::new());
    for l in listeners {
        let tx = tx.clone();
        accepts.0.push(tokio::spawn(async move {
            while let Ok((sock, _)) = l.accept().await {
                if tx.send(sock).await.is_err() {
                    return;
                }
            }
        }));
    }
    drop(tx);
    let work = async {
        while let Some(mut sock) = rx.recv().await {
            let Ok(Some(req)) =
                tokio::time::timeout(Duration::from_secs(10), read_request(&mut sock)).await
            else {
                continue;
            };
            if req.path != "/signin/callback" {
                respond(&mut sock, "404 Not Found", "").await;
                continue;
            }
            if req.method != "POST" {
                respond(&mut sock, "405 Method Not Allowed", "").await;
                continue;
            }
            if !req.sent_by_samsung() {
                respond(&mut sock, "403 Forbidden", "").await;
                continue;
            }
            return match parse_callback(&req.query, &req.body) {
                Some(s) => {
                    respond(&mut sock, "200 OK", &pages.done).await;
                    Ok(s)
                }
                None => {
                    respond(&mut sock, "200 OK", &pages.retry).await;
                    Err(SamsungError::Samsung(
                        "the sign-in came back without a token".into(),
                    ))
                }
            };
        }
        Err(SamsungError::Other("the sign-in listener stopped".into()))
    };
    let result = tokio::time::timeout(timeout, work)
        .await
        .unwrap_or_else(|_| Err(SamsungError::Other("the Samsung sign-in timed out".into())));
    drop(accepts);
    result
}

/// The listeners' accept tasks: aborted (closing the listeners) however the
/// wait ends, a callback, the timeout or a cancel that drops the future.
struct AbortOnDrop(Vec<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for a in &self.0 {
            a.abort();
        }
    }
}
