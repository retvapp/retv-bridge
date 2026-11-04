//! The ReTV server's app routes (`/v1/apps/*`): the device code, its poll,
//! the rotating refresh, the account, its TVs and sign-out.
//!
//! Request bodies that carry a token are sent from zeroing buffers, and the
//! answers are read into one; the `Authorization` header is marked sensitive.
//! Errors name the route only, never a token.

use std::time::Duration;

use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secret::Secret;

/// The answers are small; anything bigger is not the ReTV server.
const MAX_BODY: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeRequest {
    pub app: &'static str,
    pub name: String,
    pub platform: String,
    pub app_version: String,
}

/// `POST /v1/apps/codes`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnswer {
    pub user_code: String,
    pub device_code: Secret,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
    pub cell: String,
}

/// The account as an app may show it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub label: Option<String>,
    pub tier: String,
    pub pro: bool,
}

/// `POST /v1/apps/token` and `POST /v1/apps/refresh`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenAnswer {
    pub access_token: Secret, // leakcheck:allow
    pub expires_in: u64,
    pub refresh_token: Secret, // leakcheck:allow
    #[serde(default)]
    pub scope: Vec<String>,
    pub session_id: String,
    pub cell: String,
    pub account: AccountView,
}

/// `GET /v1/apps/me`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub session_id: String,
    pub cell: String,
    pub account: AccountView,
}

/// One TV of the account (`GET /v1/apps/tvs`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Tv {
    pub id: String,
    pub name: String,
    pub model: String,
    pub kind_line: String,
    pub home_version: Option<String>,
    pub helper_version: Option<String>,
    pub helper_seen: Option<String>,
    /// A Samsung TV's last ad-tracking report (`POST /v1/tv/privacy`).
    #[serde(default)]
    pub privacy: Option<TvPrivacy>,
}

/// What ReTV Home for Samsung last read: `lat_enabled` true when
/// interest-based ads are off, `None` when the TV would not say; `check` the
/// id of Bridge's request it answers; `opened` Privacy Choices opened for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TvPrivacy {
    pub lat_enabled: Option<bool>,
    pub at: String,
    pub check: Option<String>,
    pub opened: Option<bool>,
}

#[derive(Deserialize)]
struct Tvs {
    items: Vec<Tv>,
}

/// What can go wrong talking to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The person has not answered on the site yet.
    Pending,
    /// Polled too fast: wait longer.
    SlowDown,
    /// «Отклонить» on the site, or the account is gone.
    Denied,
    /// The code ran out (or was used).
    Expired,
    /// The sign-in is over (revoked, signed out, refresh reused, unknown token).
    Revoked,
    /// The token works but lacks the scope.
    Forbidden,
    RateLimited,
    /// No answer (no network, timeout, TLS); the request may or may not have arrived.
    Network(String),
    /// Any other answer: its status and error code.
    Server(u16, String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Pending => f.write_str("authorization pending"),
            ApiError::SlowDown => f.write_str("slow down"),
            ApiError::Denied => f.write_str("access denied"),
            ApiError::Expired => f.write_str("code expired"),
            ApiError::Revoked => f.write_str("signed out by the server"),
            ApiError::Forbidden => f.write_str("forbidden"),
            ApiError::RateLimited => f.write_str("rate limited"),
            ApiError::Network(e) => write!(f, "no answer: {e}"),
            ApiError::Server(s, code) => write!(f, "HTTP {s} {code}"),
        }
    }
}

/// The client of one cell's API (`https://retv.app/v1`).
#[derive(Clone)]
pub struct Api {
    base: String,
    client: reqwest::Client,
}

/// A request body that is zeroed when reqwest drops it.
struct ZeroBody(Zeroizing<Vec<u8>>);

impl AsRef<[u8]> for ZeroBody {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

fn json_body<T: Serialize>(value: &T) -> bytes::Bytes {
    let mut buf = Zeroizing::new(Vec::with_capacity(128));
    // Serializing plain structs into a Vec cannot fail.
    let _ = serde_json::to_writer(&mut *buf, value);
    bytes::Bytes::from_owner(ZeroBody(buf))
}

/// The error code of an error answer (`{"error":"slow-down"}`).
fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// The error an answer means.
pub fn classify(status: u16, body: &[u8]) -> ApiError {
    let code = error_code(body);
    match (status, code.as_str()) {
        (400, "authorization-pending") => ApiError::Pending,
        (400, "slow-down") => ApiError::SlowDown,
        (400, "access-denied") => ApiError::Denied,
        (400, "expired-token") => ApiError::Expired,
        (401, _) => ApiError::Revoked,
        (403, _) => ApiError::Forbidden,
        (429, _) => ApiError::RateLimited,
        _ => ApiError::Server(status, code),
    }
}

impl Api {
    /// `base`: `https://retv.app/v1` (no trailing slash).
    pub fn new(base: String) -> Api {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ReTV-Bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client");
        Api { base, client }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn send(
        &self,
        route: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        let mut resp = req
            .send()
            .await
            .map_err(|e| ApiError::Network(format!("{route}: {}", e.without_url())))?;
        let status = resp.status().as_u16();
        let mut body = Zeroizing::new(Vec::new());
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ApiError::Network(format!("{route}: {}", e.without_url())))?
        {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(ApiError::Server(status, "too-big".into()));
            }
            body.extend_from_slice(&chunk);
        }
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(classify(status, &body))
        }
    }

    fn parse<T: for<'de> Deserialize<'de>>(route: &str, body: &[u8]) -> Result<T, ApiError> {
        serde_json::from_slice(body)
            .map_err(|_| ApiError::Server(200, format!("{route}: bad JSON")))
    }

    fn post_json<T: Serialize>(&self, path: &str, value: &T) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header(CONTENT_TYPE, "application/json")
            .body(json_body(value))
    }

    fn bearer(access: &Secret) -> HeaderValue {
        let line = Zeroizing::new(format!("Bearer {}", access.expose())); // leakcheck:allow
        let mut v =
            HeaderValue::from_str(&line).unwrap_or_else(|_| HeaderValue::from_static("Bearer -")); // leakcheck:allow
        v.set_sensitive(true);
        v
    }

    /// `POST /apps/codes`: a new code for this computer.
    pub async fn codes(&self, req: &CodeRequest) -> Result<CodeAnswer, ApiError> {
        let body = self
            .send("codes", self.post_json("/apps/codes", req))
            .await?;
        Self::parse("codes", &body)
    }

    /// `POST /apps/token`. Once the person allowed it, the pair comes back (once).
    pub async fn token(&self, device_code: &Secret) -> Result<TokenAnswer, ApiError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            device_code: &'a Secret,
        }
        let req = self.post_json("/apps/token", &Body { device_code });
        let body = self.send("token", req).await?;
        Self::parse("token", &body)
    }

    /// `POST /apps/refresh`: a new pair; the refresh token sent stops working.
    pub async fn refresh(&self, refresh: &Secret) -> Result<TokenAnswer, ApiError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            refresh_token: &'a Secret, // leakcheck:allow
        }
        let req = self.post_json(
            "/apps/refresh",
            &Body {
                refresh_token: refresh, // leakcheck:allow
            },
        ); // leakcheck:allow
        let body = self.send("refresh", req).await?;
        Self::parse("refresh", &body)
    }

    pub async fn me(&self, access: &Secret) -> Result<Me, ApiError> {
        let req = self
            .client
            .get(format!("{}/apps/me", self.base))
            .header(AUTHORIZATION, Self::bearer(access));
        let body = self.send("me", req).await?;
        Self::parse("me", &body)
    }

    pub async fn tvs(&self, access: &Secret) -> Result<Vec<Tv>, ApiError> {
        let req = self
            .client
            .get(format!("{}/apps/tvs", self.base))
            .header(AUTHORIZATION, Self::bearer(access));
        let body = self.send("tvs", req).await?;
        Ok(Self::parse::<Tvs>("tvs", &body)?.items)
    }

    /// `POST /apps/tvs/<tv_id>/requests` (scope tv-help): leaves `kind` with
    /// `check` for the account's TV, which takes it from the server. An id
    /// that is not one (letters, digits, `-`) is refused here as the server
    /// would (404).
    pub async fn tv_request(
        &self,
        access: &Secret,
        tv_id: &str,
        kind: &str,
        check: &str,
    ) -> Result<(), ApiError> {
        if tv_id.is_empty()
            || tv_id.len() > 64
            || !tv_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(ApiError::Server(404, "device-not-found".into()));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            kind: &'a str,
            check: &'a str,
        }
        let req = self
            .post_json(
                &format!("/apps/tvs/{tv_id}/requests"),
                &Body { kind, check },
            )
            .header(AUTHORIZATION, Self::bearer(access));
        self.send("tv-request", req).await.map(|_| ())
    }

    pub async fn sign_out(&self, access: &Secret) -> Result<(), ApiError> {
        let req = self
            .client
            .post(format!("{}/apps/sign-out", self.base))
            .header(AUTHORIZATION, Self::bearer(access));
        self.send("sign-out", req).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_map_to_errors() {
        let e = |s, b: &str| classify(s, b.as_bytes());
        assert_eq!(
            e(400, r#"{"error":"authorization-pending"}"#),
            ApiError::Pending
        );
        assert_eq!(e(400, r#"{"error":"slow-down"}"#), ApiError::SlowDown);
        assert_eq!(e(400, r#"{"error":"access-denied"}"#), ApiError::Denied);
        assert_eq!(e(400, r#"{"error":"expired-token"}"#), ApiError::Expired);
        assert_eq!(e(401, r#"{"error":"revoked"}"#), ApiError::Revoked);
        assert_eq!(e(403, "{}"), ApiError::Forbidden);
        assert_eq!(e(429, ""), ApiError::RateLimited);
        assert_eq!(
            e(400, r#"{"error":"validation-failed"}"#),
            ApiError::Server(400, "validation-failed".into())
        );
        assert_eq!(e(502, "<html>"), ApiError::Server(502, String::new()));
    }

    #[test]
    fn the_bearer_header_is_sensitive() {
        let v = Api::bearer(&Secret::new("rta_x".into()));
        assert!(v.is_sensitive());
        assert_eq!(format!("{v:?}"), "Sensitive");
    }
}
