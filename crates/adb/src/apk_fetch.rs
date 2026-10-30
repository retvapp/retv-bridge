//! The real `installApk` downloader: one HTTPS GET with the OS's TLS, no
//! redirects (any 3xx is `FetchError::Redirect`), at most `max_bytes` by
//! `Content-Length` and while reading. Only URLs `is_installable_apk_url`
//! accepts are fetched (a debug build also takes a dev site over plain HTTP). The bridge itself bounds the whole download in time.

use std::time::Duration;

use async_trait::async_trait;
use retv_bridge_core::api::{ApkFetcher, FetchError};
use retv_bridge_core::validate::{is_installable_apk_url, DEV_APK_URLS};

pub struct HttpsApkFetcher {
    client: reqwest::Client,
    /// Off only in tests, which serve plain HTTP on loopback.
    check_url: bool,
}

fn client(https_only: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("ReTV-Bridge/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

impl HttpsApkFetcher {
    /// The error text is for local logs.
    pub fn new() -> Result<Self, String> {
        Ok(HttpsApkFetcher {
            client: client(!DEV_APK_URLS)?,
            check_url: true,
        })
    }
}

#[async_trait]
impl ApkFetcher for HttpsApkFetcher {
    async fn fetch(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        if self.check_url && !is_installable_apk_url(url) {
            return Err(FetchError::Network);
        }
        let mut resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| FetchError::Network)?;
        let status = resp.status().as_u16();
        if (300..=399).contains(&status) {
            return Err(FetchError::Redirect);
        }
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(FetchError::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| FetchError::Network)? {
            if (body.len() + chunk.len()) as u64 > max_bytes {
                return Err(FetchError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves `reply` (a raw HTTP response) to every request; counts requests.
    async fn serve(reply: Vec<u8>) -> (String, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                h.fetch_add(1, Ordering::SeqCst);
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    let _ = s.write_all(&reply).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        (base, hits)
    }

    fn plain() -> HttpsApkFetcher {
        HttpsApkFetcher {
            client: client(false).unwrap(),
            check_url: false,
        }
    }

    async fn get(reply: &str, max: u64) -> (Result<Vec<u8>, FetchError>, usize) {
        let (base, hits) = serve(reply.as_bytes().to_vec()).await;
        let r = plain().fetch(&format!("{base}/downloads/x.apk"), max).await;
        (r, hits.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn reads_a_200_body() {
        let (r, _) = get("HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd", 10).await;
        assert_eq!(r, Ok(b"abcd".to_vec()));
    }

    #[tokio::test]
    async fn never_follows_a_redirect() {
        let (r, hits) = get(
            "HTTP/1.1 302 Found\r\nLocation: /downloads/y.apk\r\nContent-Length: 0\r\n\r\n",
            10,
        )
        .await;
        assert_eq!(r, Err(FetchError::Redirect));
        assert_eq!(hits, 1);
        let (r, _) = get(
            "HTTP/1.1 300 Multiple Choices\r\nContent-Length: 0\r\n\r\n",
            10,
        )
        .await;
        assert_eq!(r, Err(FetchError::Redirect));
    }

    #[tokio::test]
    async fn another_status_is_reported() {
        let (r, _) = get("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n", 10).await;
        assert_eq!(r, Err(FetchError::Status(404)));
    }

    #[tokio::test]
    async fn too_large_by_length_or_while_reading() {
        let (r, _) = get(
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n01234567890",
            10,
        )
        .await;
        assert_eq!(r, Err(FetchError::TooLarge));
        // No Content-Length: the body ends with the connection.
        let (r, _) = get(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n01234567890",
            10,
        )
        .await;
        assert_eq!(r, Err(FetchError::TooLarge));
        let (r, _) = get(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789",
            10,
        )
        .await;
        assert_eq!(r.map(|b| b.len()), Ok(10));
    }

    #[tokio::test]
    async fn only_allowed_urls_are_fetched() {
        let f = HttpsApkFetcher::new().unwrap();
        assert_eq!(
            f.fetch("https://evil.example/downloads/x.apk", 10).await,
            Err(FetchError::Network)
        );
        assert_eq!(
            f.fetch("http://retv.app/downloads/x.apk", 10).await,
            Err(FetchError::Network)
        );
    }
}
