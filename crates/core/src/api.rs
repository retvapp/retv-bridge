//! The traits every other Rust crate builds against: the adb transport
//! (`BridgeAdb`, `BridgeDevice`), the APK download (`ApkFetcher`), and the
//! time and pairing-code sources that tests replace with fakes.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of one shell command that actually ran on the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// A TV found by mDNS. Serialized in camelCase, as on the wire.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTv {
    pub name: String,
    pub ip: String,
    pub connect_port: Option<u16>,
    pub pair_port: Option<u16>,
}

/// Every adb failure. The first seven wire codes are the TS engine's
/// `TransportErrorCode` union and must stay identical to it. `NoSpace` is a
/// failure whose text says the TV is full (protocol v2's `no-space`).
/// `Internal` is any other failure (device error text, a refused file, a
/// malformed reply): the wire answers `internal`, and the detail only goes to
/// the bridge's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportError {
    AdbServerUnreachable,
    Unauthorized,
    DeviceOffline,
    DeviceUnreachable,
    DeviceNotFound,
    PairingFailed,
    ConnectionLost,
    NoSpace,
    Internal,
}

impl TransportError {
    pub fn as_code(&self) -> &'static str {
        match self {
            TransportError::AdbServerUnreachable => "adb-server-unreachable",
            TransportError::Unauthorized => "unauthorized",
            TransportError::DeviceOffline => "device-offline",
            TransportError::DeviceUnreachable => "device-unreachable",
            TransportError::DeviceNotFound => "device-not-found",
            TransportError::PairingFailed => "pairing-failed",
            TransportError::ConnectionLost => "connection-lost",
            TransportError::NoSpace => "no-space",
            TransportError::Internal => "internal",
        }
    }

    /// One of the engine's seven transport codes (not `NoSpace` or `Internal`).
    pub fn is_transport(&self) -> bool {
        !matches!(self, TransportError::NoSpace | TransportError::Internal)
    }

    /// The code for a failure known only by its text, as the TS bridge's
    /// `errorCode` reads an error message: exactly a transport code passes
    /// through; text containing "no space left on device" (any case) is
    /// `NoSpace`; anything else is `Internal` (log the text, never send it).
    pub fn from_failure_text(text: &str) -> TransportError {
        const CODES: [TransportError; 7] = [
            TransportError::AdbServerUnreachable,
            TransportError::Unauthorized,
            TransportError::DeviceOffline,
            TransportError::DeviceUnreachable,
            TransportError::DeviceNotFound,
            TransportError::PairingFailed,
            TransportError::ConnectionLost,
        ];
        if let Some(code) = CODES.iter().find(|c| c.as_code() == text) {
            return *code;
        }
        if is_no_space(text) {
            TransportError::NoSpace
        } else {
            TransportError::Internal
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_code())
    }
}

impl std::error::Error for TransportError {}

/// Whether a failure text says the TV's storage is full.
pub fn is_no_space(text: &str) -> bool {
    text.to_ascii_lowercase()
        .contains("no space left on device")
}

/// Why an APK download failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The server answered with a status other than 200. The bridge answers
    /// any 3xx like `Redirect` (`download-failed` with no status).
    Status(u16),
    /// The server answered with a redirect (any 3xx); redirects are never followed.
    Redirect,
    /// The body is larger than `max_bytes`.
    TooLarge,
    /// DNS, TCP, TLS or a broken body.
    Network,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Status(code) => write!(f, "http status {code}"),
            FetchError::Redirect => f.write_str("redirect refused"),
            FetchError::TooLarge => f.write_str("too large"),
            FetchError::Network => f.write_str("network error"),
        }
    }
}

impl std::error::Error for FetchError {}

/// One connected TV.
#[async_trait]
pub trait BridgeDevice: Send + Sync {
    fn serial(&self) -> &str;
    /// `ro.product.model`, trimmed (`""` when unknown), when the connection
    /// already read it. `None`: the bridge asks with `getprop ro.product.model`.
    fn model(&self) -> Option<String> {
        None
    }
    async fn run(&self, cmd: &str) -> Result<ShellResult, TransportError>;
    async fn write_file(&self, path: &str, content: &str) -> Result<(), TransportError>;
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), TransportError>;
    async fn close(&self);
}

/// The adb host the Bridge drives.
#[async_trait]
pub trait BridgeAdb: Send + Sync {
    async fn available(&self) -> bool;
    async fn connect(&self, address: &str) -> Result<Arc<dyn BridgeDevice>, TransportError>;
    async fn pair(&self, address: &str, code: &str) -> Result<(), TransportError>;
    async fn discover(&self) -> Vec<DiscoveredTv>;
}

/// Downloads an APK for `installApk`: one GET, redirects refused, at most
/// `max_bytes` (by `Content-Length` and while reading: else `TooLarge`).
#[async_trait]
pub trait ApkFetcher: Send + Sync {
    async fn fetch(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError>;
}

/// Wall-clock milliseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Source of site pairing codes.
pub trait CodeSource: Send + Sync {
    fn next_code(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire codes are the TS engine's `TransportErrorCode` union, in order.
    #[test]
    fn transport_codes_match_engine() {
        let variants = [
            TransportError::AdbServerUnreachable,
            TransportError::Unauthorized,
            TransportError::DeviceOffline,
            TransportError::DeviceUnreachable,
            TransportError::DeviceNotFound,
            TransportError::PairingFailed,
            TransportError::ConnectionLost,
        ];
        let codes: Vec<&str> = variants.iter().map(|e| e.as_code()).collect();
        assert_eq!(
            codes,
            [
                "adb-server-unreachable",
                "unauthorized",
                "device-offline",
                "device-unreachable",
                "device-not-found",
                "pairing-failed",
                "connection-lost",
            ]
        );
    }

    #[test]
    fn failure_texts_map_like_the_ts_bridge() {
        let f = TransportError::from_failure_text;
        assert_eq!(f("unauthorized"), TransportError::Unauthorized);
        assert_eq!(f("connection-lost"), TransportError::ConnectionLost);
        assert_eq!(f("internal"), TransportError::Internal);
        assert_eq!(
            f("unauthorized: the adb key was refused"),
            TransportError::Internal
        );
        assert_eq!(f("remote No Space Left On Device"), TransportError::NoSpace);
        assert_eq!(
            f("sh: /data/x: Permission denied"),
            TransportError::Internal
        );
        assert_eq!(TransportError::NoSpace.as_code(), "no-space");
        assert!(!TransportError::NoSpace.is_transport());
    }

    /// `Internal` is the protocol's `internal`, not an engine transport code.
    #[test]
    fn internal_is_not_a_transport_code() {
        assert_eq!(TransportError::Internal.as_code(), "internal");
        assert_eq!(TransportError::Internal.to_string(), "internal");
        assert!(!TransportError::Internal.is_transport());
        assert!(TransportError::ConnectionLost.is_transport());
        assert!(TransportError::AdbServerUnreachable.is_transport());
    }

    #[test]
    fn transport_error_displays_its_code() {
        assert_eq!(
            TransportError::DeviceNotFound.to_string(),
            "device-not-found"
        );
    }

    #[test]
    fn discovered_tv_is_camel_case_on_the_wire() {
        let tv = DiscoveredTv {
            name: "Mi TV Stick".into(),
            ip: "192.168.1.20".into(),
            connect_port: Some(5555),
            pair_port: None,
        };
        let json = serde_json::to_value(&tv).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "Mi TV Stick",
                "ip": "192.168.1.20",
                "connectPort": 5555,
                "pairPort": null,
            })
        );
        let back: DiscoveredTv = serde_json::from_value(json).unwrap();
        assert_eq!(back, tv);
    }
}
