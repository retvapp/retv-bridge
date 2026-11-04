// tungstenite's handshake callback returns its large error type.
#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use retv_bridge_samsung::remote::{remote_url, Remote, RemoteError};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

#[test]
fn url_encodes_name_and_token() {
    assert_eq!(
        remote_url("192.168.88.219", None),
        "wss://192.168.88.219:8002/api/v2/channels/samsung.remote.control?name=UmVUViBCcmlkZ2U="
    );
    assert_eq!(
        remote_url("192.168.88.219", Some("12345678")),
        "wss://192.168.88.219:8002/api/v2/channels/samsung.remote.control?name=UmVUViBCcmlkZ2U=&token=12345678"
    );
}

/// A fake TV remote channel: answers the first connect with `first`, records
/// the request path and every frame.
async fn fake_tv(
    first: Option<&'static str>,
) -> (String, Arc<Mutex<Vec<String>>>, Arc<Mutex<String>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/api/v2/channels/samsung.remote.control?name=x",
        l.local_addr().unwrap()
    );
    let frames: Arc<Mutex<Vec<String>>> = Arc::default();
    let path: Arc<Mutex<String>> = Arc::default();
    let (f2, p2) = (frames.clone(), path.clone());
    tokio::spawn(async move {
        let (sock, _) = l.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_hdr_async(sock, |req: &Request, resp: Response| {
            *p2.lock().unwrap() = req.uri().to_string();
            Ok(resp)
        })
        .await
        .unwrap();
        let (mut tx, mut rx) = ws.split();
        if let Some(first) = first {
            tx.send(Message::text(first)).await.unwrap();
        }
        while let Some(Ok(m)) = rx.next().await {
            if let Message::Text(t) = m {
                f2.lock().unwrap().push(t.to_string());
            }
        }
    });
    (url, frames, path)
}

#[tokio::test]
async fn connect_gets_token_and_sends_keys() {
    let (url, frames, _) = fake_tv(Some(
        r#"{"data":{"clients":[],"id":"x","token":"98765432"},"event":"ms.channel.connect"}"#,
    ))
    .await;
    let (mut remote, token) = Remote::connect(&url, false, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(token.as_deref(), Some("98765432"));
    remote.key("KEY_ENTER").await.unwrap();
    remote.key("DOWN").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let got = frames.lock().unwrap().clone();
    assert_eq!(got.len(), 2);
    let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
    assert_eq!(v["method"], "ms.remote.control");
    assert_eq!(v["params"]["Cmd"], "Click");
    assert_eq!(v["params"]["DataOfCmd"], "KEY_ENTER");
    assert_eq!(v["params"]["Option"], "false");
    assert_eq!(v["params"]["TypeOfRemote"], "SendRemoteKey");
    let v: serde_json::Value = serde_json::from_str(&got[1]).unwrap();
    assert_eq!(v["params"]["DataOfCmd"], "KEY_DOWN");
}

#[tokio::test]
async fn refuses_other_keys() {
    let (url, frames, _) = fake_tv(Some(r#"{"data":{},"event":"ms.channel.connect"}"#)).await;
    let (mut remote, token) = Remote::connect(&url, false, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(token, None);
    for bad in ["KEY_POWER", "KEY_HOME", "KEY_0", "", "KEY_ENTER\"}"] {
        assert_eq!(
            remote.key(bad).await.unwrap_err(),
            RemoteError::BadKey,
            "{bad}"
        );
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(frames.lock().unwrap().is_empty());
}

#[tokio::test]
async fn denied_and_timeout() {
    let (url, _, _) = fake_tv(Some(r#"{"event":"ms.channel.unauthorized"}"#)).await;
    assert_eq!(
        Remote::connect(&url, false, Duration::from_secs(2))
            .await
            .err()
            .unwrap(),
        RemoteError::Denied
    );
    let (url, _, _) = fake_tv(Some(r#"{"event":"ms.channel.timeOut"}"#)).await;
    assert_eq!(
        Remote::connect(&url, false, Duration::from_secs(2))
            .await
            .err()
            .unwrap(),
        RemoteError::Timeout
    );
    // The Allow prompt never answered.
    let (url, _, _) = fake_tv(None).await;
    assert_eq!(
        Remote::connect(&url, false, Duration::from_millis(200))
            .await
            .err()
            .unwrap(),
        RemoteError::Timeout
    );
}
