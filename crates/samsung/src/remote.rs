//! The TV's remote keys from the Mac: Samsung's remote-control channel
//! (`wss://TV:8002/api/v2/channels/samsung.remote.control`). The TV asks
//! once («Allow»), then gives a token we keep. It never answers a client on
//! the TV itself, which is why the privacy keys come from the Mac.
//!
//! Only navigation keys go out. Launching ReTV Home with a request over
//! this channel (`ed.apps.launch`, `DEEP_LINK` or `NATIVE_LAUNCH` with a
//! `metaTag`) was tried and dropped: a 2021 QE55Q67AA does nothing with it,
//! for ReTV Home or even YouTube (2026-11-04). Bridge now leaves the request
//! on the account and starts ReTV Home over sdb (see `privacy`).

use std::fmt;
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub const REMOTE_PORT: u16 = 8002;
/// The name the TV shows in its Allow prompt and device list.
pub const REMOTE_NAME: &str = "ReTV Bridge";
/// How long the person has to press Allow on the TV.
pub const ALLOW_WAIT: Duration = Duration::from_secs(30);

/// Only navigation keys: nothing that powers off, changes source or volume.
const KEYS: &[&str] = &[
    "KEY_UP",
    "KEY_DOWN",
    "KEY_LEFT",
    "KEY_RIGHT",
    "KEY_ENTER",
    "KEY_RETURN",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// The person pressed «Deny», or the TV refuses this name.
    Denied,
    /// No answer to the Allow prompt (or no answer at all).
    Timeout,
    /// The channel could not be opened or broke.
    Down(String),
    /// A key outside the navigation keys.
    BadKey,
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoteError::Denied => write!(f, "the TV refused the remote (Deny)"),
            RemoteError::Timeout => write!(f, "the TV did not allow the remote in time"),
            RemoteError::Down(e) => write!(f, "remote channel: {e}"),
            RemoteError::BadKey => write!(f, "key not allowed"),
        }
    }
}

impl std::error::Error for RemoteError {}

pub fn remote_url(ip: &str, token: Option<&str>) -> String {
    let name = base64::engine::general_purpose::STANDARD.encode(REMOTE_NAME);
    let mut url =
        format!("wss://{ip}:{REMOTE_PORT}/api/v2/channels/samsung.remote.control?name={name}");
    if let Some(t) = token.filter(|t| t.bytes().all(|b| b.is_ascii_alphanumeric())) {
        // The TV takes its pairing back in the `token` query parameter.
        url.push_str(&format!("&{}={t}", "token"));
    }
    url
}

/// `KEY_ENTER` or `ENTER` → `KEY_ENTER`, if allowed.
fn normalise(key: &str) -> Option<&'static str> {
    let full = if key.starts_with("KEY_") {
        key.to_string()
    } else {
        format!("KEY_{key}")
    };
    KEYS.iter().copied().find(|k| *k == full)
}

pub struct Remote {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Remote {
    /// Opens the channel and waits (up to `wait`) for `ms.channel.connect`.
    /// Returns the token the TV issued, if it sent one. `insecure_tls`
    /// accepts the TV's self-signed certificate (the real TV needs it).
    pub async fn connect(
        url: &str,
        insecure_tls: bool,
        wait: Duration,
    ) -> Result<(Remote, Option<String>), RemoteError> {
        let connector = if insecure_tls {
            let tls = native_tls::TlsConnector::builder()
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true)
                .build()
                .map_err(|e| RemoteError::Down(e.to_string()))?;
            Some(tokio_tungstenite::Connector::NativeTls(tls))
        } else {
            None
        };
        let open = tokio_tungstenite::connect_async_tls_with_config(url, None, false, connector);
        let (mut ws, _) = tokio::time::timeout(Duration::from_secs(10), open)
            .await
            .map_err(|_| RemoteError::Timeout)?
            .map_err(|e| RemoteError::Down(e.to_string()))?;
        let answer = async {
            while let Some(msg) = ws.next().await {
                let msg = msg.map_err(|e| RemoteError::Down(e.to_string()))?;
                let Message::Text(text) = msg else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                match v.get("event").and_then(|e| e.as_str()) {
                    Some("ms.channel.connect") => {
                        let issued = v
                            .pointer("/data/token")
                            .and_then(|t| {
                                t.as_str()
                                    .map(str::to_string)
                                    .or_else(|| Some(t.to_string()))
                            })
                            .filter(|t| {
                                !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric())
                            });
                        return Ok(issued);
                    }
                    Some("ms.channel.unauthorized") => return Err(RemoteError::Denied),
                    Some("ms.channel.timeOut") => return Err(RemoteError::Timeout),
                    _ => continue,
                }
            }
            Err(RemoteError::Down("closed".into()))
        };
        let issued = tokio::time::timeout(wait, answer)
            .await
            .map_err(|_| RemoteError::Timeout)??;
        Ok((Remote { ws }, issued))
    }

    /// One click of a navigation key.
    pub async fn key(&mut self, key: &str) -> Result<(), RemoteError> {
        let key = normalise(key).ok_or(RemoteError::BadKey)?;
        let msg = serde_json::json!({
            "method": "ms.remote.control",
            "params": {
                "Cmd": "Click",
                "DataOfCmd": key,
                "Option": "false",
                "TypeOfRemote": "SendRemoteKey",
            }
        });
        self.ws
            .send(Message::text(msg.to_string()))
            .await
            .map_err(|e| RemoteError::Down(e.to_string()))
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}
