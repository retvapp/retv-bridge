//! The fake adb and APK downloader a wire fixture describes (`"adb"` in
//! `conformance/wire/*.json`; the format is in `conformance/README.md`).
//!
//! Error texts become `TransportError`s the way the real adb crate's do: an
//! exact transport code passes through, a "no space left on device" text is
//! `NoSpace`, anything else is `Internal`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use retv_bridge_core::api::{
    ApkFetcher, BridgeAdb, BridgeDevice, DiscoveredTv, FetchError, ShellResult, TransportError,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// What the fake adb saw, compared by `expectAdb` steps.
#[derive(Default, Debug)]
pub struct AdbLog {
    pub connected: Vec<String>,
    pub closed: Vec<String>,
    pub paired: Vec<String>,
    pub pushed: BTreeMap<String, String>,
    pub ran: Vec<(String, String)>,
}

impl AdbLog {
    /// The record as fixture JSON (`closed` sorted: it is a multiset).
    pub fn to_json(&self) -> Value {
        let mut closed = self.closed.clone();
        closed.sort();
        json!({
            "connected": self.connected,
            "closed": closed,
            "paired": self.paired,
            "pushed": self.pushed,
            "ran": self.ran.iter().map(|(s, c)| json!([s, c])).collect::<Vec<_>>(),
        })
    }
}

fn sha256_tag(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

const MODEL_PROBE: &str = "getprop ro.product.model";

fn fail(text: &str) -> TransportError {
    TransportError::from_failure_text(text)
}

pub struct FakeAdb {
    spec: Value,
    pub log: Arc<Mutex<AdbLog>>,
    /// How the bridge learns the model: handed over with the connection
    /// (`false`), or by its `getprop ro.product.model` fallback (`true`).
    model_via_getprop: bool,
}

impl FakeAdb {
    pub fn new(spec: &Value) -> FakeAdb {
        FakeAdb {
            spec: spec.clone(),
            log: Arc::new(Mutex::new(AdbLog::default())),
            model_via_getprop: false,
        }
    }

    /// Devices that do not hand their model over: the bridge asks with getprop.
    pub fn model_via_getprop(mut self) -> FakeAdb {
        self.model_via_getprop = true;
        self
    }

    fn devices(&self) -> Map<String, Value> {
        self.spec["devices"]
            .as_object()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl BridgeAdb for FakeAdb {
    async fn available(&self) -> bool {
        self.spec["available"].as_bool().unwrap_or(true)
    }

    async fn connect(&self, address: &str) -> Result<Arc<dyn BridgeDevice>, TransportError> {
        let Some(dev) = self.devices().get(address).cloned() else {
            return Err(TransportError::DeviceUnreachable);
        };
        if let Some(text) = dev["connectError"].as_str() {
            return Err(fail(text));
        }
        self.log.lock().unwrap().connected.push(address.to_string());
        Ok(Arc::new(FakeDevice {
            serial: address.to_string(),
            spec: dev,
            log: self.log.clone(),
            model_via_getprop: self.model_via_getprop,
        }))
    }

    async fn pair(&self, address: &str, code: &str) -> Result<(), TransportError> {
        let want = self.spec["pairCode"].as_str().unwrap_or("123456");
        if code != want {
            return Err(TransportError::PairingFailed);
        }
        self.log.lock().unwrap().paired.push(address.to_string());
        Ok(())
    }

    async fn discover(&self) -> Vec<DiscoveredTv> {
        serde_json::from_value(self.spec["tvs"].clone()).unwrap_or_default()
    }
}

pub struct FakeDevice {
    serial: String,
    spec: Value,
    log: Arc<Mutex<AdbLog>>,
    model_via_getprop: bool,
}

#[async_trait]
impl BridgeDevice for FakeDevice {
    fn serial(&self) -> &str {
        &self.serial
    }

    /// The Rust fake hands the model over with the connection (README).
    fn model(&self) -> Option<String> {
        if self.model_via_getprop {
            return None;
        }
        Some(self.spec["model"].as_str().unwrap_or("").to_string())
    }

    async fn run(&self, cmd: &str) -> Result<ShellResult, TransportError> {
        // The README's model probe: answered, not recorded in `ran`.
        if cmd == MODEL_PROBE && self.spec["shell"].get(cmd).is_none() {
            return Ok(match self.spec["model"].as_str() {
                Some(m) => ShellResult {
                    exit_code: 0,
                    stdout: format!("{m}\n"),
                    stderr: String::new(),
                },
                None => ShellResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            });
        }
        self.log
            .lock()
            .unwrap()
            .ran
            .push((self.serial.clone(), cmd.to_string()));
        match self.spec["shell"].get(cmd) {
            Some(r) if r.get("error").is_some() => Err(fail(r["error"].as_str().unwrap_or(""))),
            Some(r) => Ok(ShellResult {
                exit_code: r["exitCode"].as_i64().unwrap_or(0) as i32,
                stdout: r["stdout"].as_str().unwrap_or("").to_string(),
                stderr: r["stderr"].as_str().unwrap_or("").to_string(),
            }),
            None => Ok(ShellResult {
                exit_code: 127,
                stdout: String::new(),
                stderr: String::new(),
            }),
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), TransportError> {
        self.write_bytes(path, content.as_bytes()).await
    }

    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), TransportError> {
        if let Some(text) = self.spec["writeError"].as_str() {
            return Err(fail(text));
        }
        self.log
            .lock()
            .unwrap()
            .pushed
            .insert(path.to_string(), sha256_tag(bytes));
        Ok(())
    }

    async fn close(&self) {
        self.log.lock().unwrap().closed.push(self.serial.clone());
    }
}

/// The downloader over the fixture's `http` map. Like a client that refuses
/// redirects: a `redirect` entry fails with `Redirect`; any other status but
/// 200 (3xx included) is `Status`.
pub struct FakeFetcher {
    http: Map<String, Value>,
}

impl FakeFetcher {
    pub fn new(spec: &Value) -> FakeFetcher {
        FakeFetcher {
            http: spec["http"].as_object().cloned().unwrap_or_default(),
        }
    }
}

pub fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("fixture hex"))
        .collect()
}

#[async_trait]
impl ApkFetcher for FakeFetcher {
    async fn fetch(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        let Some(entry) = self.http.get(url) else {
            return Err(FetchError::Network);
        };
        if entry.get("redirect").is_some() {
            return Err(FetchError::Redirect);
        }
        let status = entry["status"].as_u64().unwrap_or(200) as u16;
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let unit = hex_bytes(entry["bodyHex"].as_str().unwrap_or(""));
        let repeat = entry["repeat"].as_u64().unwrap_or(1) as usize;
        // Content-Length first: never build a body over the limit.
        if (unit.len() * repeat) as u64 > max_bytes {
            return Err(FetchError::TooLarge);
        }
        Ok(unit.repeat(repeat))
    }
}
