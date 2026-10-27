//! `BridgeAdb` / `BridgeDevice` on top of `AdbHost`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use retv_bridge_core::api::{BridgeAdb, BridgeDevice, DiscoveredTv, ShellResult, TransportError};
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::validate::{group_services, MdnsService};

use crate::host::AdbHost;
use crate::revive::{retry_once, retry_once_after, ServerRevive};
use crate::sync::PushError;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Browses mDNS for adb services (`mdns::discover` in the app); `RealAdb`
/// groups them into TVs.
pub type Discoverer = Arc<dyn Fn() -> BoxFuture<'static, Vec<MdnsService>> + Send + Sync>;

/// Receives the host's `Log` lines. (`TvConnected` / `TvDisconnected` come
/// from the bridge server, which knows which socket holds the TV.)
pub type EventSink = Arc<dyn Fn(BridgeEvent) + Send + Sync>;

const FILE_MODE: u32 = 0o644;

pub struct RealAdb {
    pub host: AdbHost,
    pub discover: Discoverer,
    revive: Option<Arc<ServerRevive>>,
}

impl RealAdb {
    pub fn new(host: AdbHost, discover: Discoverer) -> RealAdb {
        RealAdb {
            host,
            discover,
            revive: None,
        }
    }

    /// Starts the adb server again when a call finds it gone, and retries
    /// that call once (see `ServerRevive`).
    pub fn with_revive(mut self, revive: Arc<ServerRevive>) -> RealAdb {
        self.revive = Some(revive);
        self
    }

    /// Sends the host's log lines to `sink` as `Log`.
    pub fn with_events(mut self, sink: EventSink) -> RealAdb {
        self.host = self
            .host
            .with_log(Arc::new(move |line| sink(BridgeEvent::Log(line))));
        self
    }
}

#[async_trait]
impl BridgeAdb for RealAdb {
    async fn available(&self) -> bool {
        retry_once(self.revive.as_deref(), || self.host.version())
            .await
            .is_ok()
    }

    async fn connect(&self, address: &str) -> Result<Arc<dyn BridgeDevice>, TransportError> {
        let serial = retry_once(self.revive.as_deref(), || self.host.connect(address)).await?;
        if let Ok(features) = self.host.features(&serial).await {
            if !features.iter().any(|f| f == "shell_v2") {
                self.host.log(format!(
                    "{serial} does not support shell protocol v2 (Android 6 or older); \
                     commands on it will fail"
                ));
            }
        }
        // The model only labels the TV; a failure to read it is not fatal.
        let model = match self.host.shell(&serial, "getprop ro.product.model").await {
            Ok(r) if r.exit_code == 0 => r.stdout.trim().to_string(),
            _ => String::new(),
        };
        Ok(Arc::new(RealDevice {
            host: self.host.clone(),
            revive: self.revive.clone(),
            serial,
            model,
            closed: AtomicBool::new(false),
        }))
    }

    async fn pair(&self, address: &str, code: &str) -> Result<(), TransportError> {
        retry_once(self.revive.as_deref(), || self.host.pair(address, code)).await
    }

    async fn discover(&self) -> Vec<DiscoveredTv> {
        group_services(&(self.discover)().await)
    }
}

/// One connected TV. Requests open their own server socket, so `close` only
/// stops this handle; it does not `adb disconnect` the TV, which other adb
/// users on the machine may share.
pub struct RealDevice {
    host: AdbHost,
    revive: Option<Arc<ServerRevive>>,
    serial: String,
    model: String,
    closed: AtomicBool,
}

impl RealDevice {
    /// `ro.product.model`, trimmed, or `""` when it could not be read.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// After the adb server was started again: it has forgotten every
    /// `adb connect` (its mDNS auto-connect is off), so a network TV is
    /// connected again before the command is retried. A failure here is left
    /// to the retried command to report.
    async fn reconnect(&self) {
        if is_network_serial(&self.serial) {
            let _ = self.host.connect(&self.serial).await;
        }
    }

    fn check_open(&self) -> Result<(), TransportError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(TransportError::ConnectionLost)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl BridgeDevice for RealDevice {
    fn serial(&self) -> &str {
        &self.serial
    }

    fn model(&self) -> Option<String> {
        Some(self.model.clone())
    }

    async fn run(&self, cmd: &str) -> Result<ShellResult, TransportError> {
        self.check_open()?;
        retry_once_after(
            self.revive.as_deref(),
            || self.host.shell(&self.serial, cmd),
            || self.reconnect(),
        )
        .await
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), TransportError> {
        self.write_bytes(path, content.as_bytes()).await
    }

    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), TransportError> {
        self.check_open()?;
        // The adb server's own failures retry; the device's answer stays as it is.
        let pushed = retry_once_after(
            self.revive.as_deref(),
            || async {
                match self.host.push(&self.serial, path, bytes, FILE_MODE).await {
                    Err(PushError::Transport(e)) => Err(e),
                    other => Ok(other),
                }
            },
            || self.reconnect(),
        )
        .await?;
        match pushed {
            Ok(()) => Ok(()),
            Err(PushError::Transport(e)) => Err(e),
            Err(PushError::Failed(msg)) => {
                let detail = format!("{}: writing {path} failed: {msg}", self.serial);
                if retv_bridge_core::api::is_no_space(&msg) {
                    self.host.log(detail);
                    Err(TransportError::NoSpace)
                } else {
                    Err(self.host.internal(detail))
                }
            }
        }
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// `ip:port`, the serial of a TV connected over the network (not USB).
fn is_network_serial(serial: &str) -> bool {
    serial.rsplit_once(':').is_some_and(|(host, port)| {
        host.parse::<std::net::Ipv4Addr>().is_ok() && port.parse::<u16>().is_ok_and(|p| p > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::is_network_serial;

    #[test]
    fn network_serials_are_ip_and_port() {
        assert!(is_network_serial("192.0.2.232:5555"));
        assert!(!is_network_serial("192.0.2.232"));
        assert!(!is_network_serial("R58M123ABC"));
        assert!(!is_network_serial("adb-123-abc._adb-tls-connect._tcp"));
        assert!(!is_network_serial("192.0.2.232:0"));
    }
}
