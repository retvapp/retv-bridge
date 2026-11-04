use std::time::Duration;

use retv_bridge_samsung::signin::{
    bind_callback, parse_callback, samsung_origin, sign_in_url, wait_for_sign_in, Pages,
    CALLBACK_PORT,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Samsung's field name, and a fake value for it.
const AT: &str = "access_token";
const VALUE: &str = "fake-sign-in-123";

fn code_json() -> String {
    format!("{{\"{AT}\":\"{VALUE}\",\"userId\":\"u42\",\"inputEmailID\":\"me@example.com\"}}")
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[test]
fn url_is_the_verified_one() {
    assert_eq!(CALLBACK_PORT, 4794);
    assert_eq!(
        sign_in_url(),
        "https://account.samsung.com/mobile/account/check.do?serviceID=v285zxnl3h&actionID=StartOAuth2&accessToken=Y&redirect_uri=http://localhost:4794/signin/callback"
    );
}

#[test]
fn code_json_in_form_post() {
    let body = format!("code={}&state=x", form_encode(&code_json()));
    let s = parse_callback("", &body).unwrap();
    assert_eq!(s.access.as_str(), VALUE);
    assert_eq!(s.user_id, "u42");
    assert_eq!(s.email.as_deref(), Some("me@example.com"));
}

#[test]
fn code_double_encoded() {
    let body = format!("code={}", form_encode(&form_encode(&code_json())));
    let s = parse_callback("", &body).unwrap();
    assert_eq!(s.access.as_str(), VALUE);
}

#[test]
fn get_query_and_json_body() {
    let s = parse_callback(&format!("{AT}={VALUE}&user_id=u1"), "").unwrap();
    assert_eq!(s.user_id, "u1");
    assert_eq!(s.email, None);
    let s = parse_callback(
        "",
        &format!("{{\"{}\":\"{VALUE}\",\"userID\":\"u2\"}}", "accessToken"),
    )
    .unwrap();
    assert_eq!(s.user_id, "u2");
}

#[test]
fn missing_parts_are_none() {
    assert!(parse_callback("", "code=%7B%22userId%22%3A%22u%22%7D").is_none());
    assert!(parse_callback(&format!("{AT}=x"), "").is_none());
    assert!(parse_callback("", "").is_none());
}

#[test]
fn debug_never_shows_token() {
    let s = parse_callback(&format!("{AT}={VALUE}&userId=u1"), "").unwrap();
    let shown = format!("{s:?}");
    assert!(!shown.contains(VALUE), "{shown}");
}

const SAMSUNG: &str = "Origin: https://account.samsung.com";

/// One request; `extra` is extra header lines.
async fn send(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra: &str,
    body: &str,
) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost:4794\r\n{extra}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

fn pages() -> Pages {
    Pages {
        done: "<p>DONE</p>".into(),
        retry: "<p>RETRY</p>".into(),
    }
}

fn good_body() -> String {
    format!("code={}", form_encode(&code_json()))
}

#[tokio::test]
async fn only_a_post_from_samsung_is_taken() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let waiting = tokio::spawn(wait_for_sign_in(vec![l], Duration::from_secs(5), pages()));
    let body = good_body();

    let other = send(addr, "POST", "/favicon.ico", SAMSUNG, "").await;
    assert!(other.starts_with("HTTP/1.1 404"), "{other}");
    let get = send(
        addr,
        "GET",
        &format!("/signin/callback?{body}"),
        SAMSUNG,
        "",
    )
    .await;
    assert!(get.starts_with("HTTP/1.1 405"), "{get}");
    // A browser that hides where the POST came from (no Origin, or Origin: null after Samsung's cross-site
    // redirects) is accepted: that is what Chrome does with the real Samsung callback.
    for refused in [
        "Origin: https://evil.example",
        "Origin: https://evilsamsung.com",
        "Origin: http://account.samsung.com",
        "Referer: https://samsung.com.evil.example/x",
    ] {
        let r = send(addr, "POST", "/signin/callback", refused, &body).await;
        assert!(r.starts_with("HTTP/1.1 403"), "{refused}: {r}");
    }
    // A Referer is enough when the browser sends no Origin.
    let ok = send(
        addr,
        "POST",
        "/signin/callback",
        "Referer: https://account.samsung.com/accounts/v1/signin",
        &body,
    )
    .await;
    assert!(ok.contains("DONE"), "{ok}");
    let s = waiting.await.unwrap().unwrap();
    assert_eq!(s.user_id, "u42");
    // One valid callback, then the listener is gone.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn a_valid_callback_without_token_ends_the_wait() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let waiting = tokio::spawn(wait_for_sign_in(vec![l], Duration::from_secs(5), pages()));
    let first = send(addr, "POST", "/signin/callback", SAMSUNG, "code=%7B%7D").await;
    assert!(first.contains("RETRY"), "{first}");
    assert!(waiting.await.unwrap().is_err());
}

#[tokio::test]
async fn origin_rule() {
    for ok in [
        "https://account.samsung.com",
        "https://samsung.com",
        "https://ACCOUNT.Samsung.com:443/x",
    ] {
        assert!(samsung_origin(ok), "{ok}");
    }
    for bad in [
        "",
        "null",
        "http://account.samsung.com",
        "https://evilsamsung.com",
        "https://samsung.com.evil.example",
        "https://user@evil.example",
    ] {
        assert!(!samsung_origin(bad), "{bad}");
    }
}

#[tokio::test]
async fn bind_stops_when_the_port_is_taken() {
    // A free port: both loopbacks (or 127.0.0.1 alone without IPv6).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let got = bind_callback(port).await.unwrap();
    assert!(!got.is_empty());
    drop(got);

    // Someone else holds 127.0.0.1: sign-in must not start.
    let hijack = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    assert!(bind_callback(port).await.is_err());
    drop(hijack);

    // Someone else holds ::1 (where IPv6 exists): same.
    if let Ok(hijack6) = std::net::TcpListener::bind(("::1", port)) {
        assert!(bind_callback(port).await.is_err());
        drop(hijack6);
    }
}

#[tokio::test]
async fn a_cancelled_wait_frees_the_port() {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let listeners = bind_callback(port).await.unwrap();
    let pages = Pages {
        done: String::new(),
        retry: String::new(),
    };
    // The person presses «Отменить»: the app drops the wait.
    let wait = wait_for_sign_in(listeners, Duration::from_secs(600), pages);
    assert!(tokio::time::timeout(Duration::from_millis(50), wait)
        .await
        .is_err());
    tokio::task::yield_now().await;
    let mut again = None;
    for _ in 0..50 {
        if let Ok(l) = bind_callback(port).await {
            again = Some(l);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        again.is_some(),
        "the listeners of a cancelled sign-in still hold the port"
    );
}

#[tokio::test]
async fn times_out() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pages = Pages {
        done: String::new(),
        retry: String::new(),
    };
    assert!(wait_for_sign_in(vec![l], Duration::from_millis(100), pages)
        .await
        .is_err());
}

#[tokio::test]
async fn a_callback_whose_origin_the_browser_hid_is_taken() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let waiting = tokio::spawn(wait_for_sign_in(vec![l], Duration::from_secs(5), pages()));
    let ok = send(
        addr,
        "POST",
        "/signin/callback",
        "Origin: null",
        &good_body(),
    )
    .await;
    assert!(ok.contains("DONE"), "{ok}");
    assert_eq!(waiting.await.unwrap().unwrap().user_id, "u42");
}
