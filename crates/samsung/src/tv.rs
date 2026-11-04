//! A Samsung TV's own REST API (`http://TV:8001/api/v2/`, no auth): model,
//! year, Developer Mode and the Host PC IP it trusts.

use std::net::Ipv4Addr;
use std::time::Duration;

use serde_json::Value;

use crate::SamsungError;

/// The TV's REST port.
pub const REST_PORT: u16 = 8001;
const REST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TvInfo {
    /// The address we reached it on.
    pub ip: String,
    /// «[TV] Samsung Q67AA 55 TV».
    pub name: String,
    /// «QE55Q67AAUXXH».
    pub model_name: String,
    /// «21_NIKEL_UHD»: the platform code, its first two digits are the year.
    pub model_code: String,
    pub year: Option<u16>,
    pub developer_mode: bool,
    /// The Host PC IP set in Developer Mode, if any.
    pub developer_ip: Option<String>,
    pub os: String,
}

impl TvInfo {
    /// Developer Mode is on and trusts `mac` (the computer sdb comes from).
    pub fn host_ip_ok(&self, mac: &str) -> bool {
        self.developer_mode && self.developer_ip.as_deref() == Some(mac)
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Reads the `/api/v2/` answer. `ip` is the address we asked.
pub fn parse_info(ip: &str, json: &str) -> Result<TvInfo, SamsungError> {
    let v: Value = serde_json::from_str(json).map_err(|_| SamsungError::NotSamsung)?;
    let d = v
        .get("device")
        .filter(|d| d.is_object())
        .ok_or(SamsungError::NotSamsung)?;
    let model_name = text(d, "modelName");
    let model_code = text(d, "model");
    if model_name.is_empty() && model_code.is_empty() {
        return Err(SamsungError::NotSamsung);
    }
    let dev_ip = text(d, "developerIP");
    let developer_ip = (!dev_ip.is_empty() && dev_ip != "0.0.0.0").then_some(dev_ip);
    let mut name = text(d, "name");
    if name.is_empty() {
        name = text(&v, "name");
    }
    Ok(TvInfo {
        ip: ip.to_string(),
        name,
        year: year_of(&model_code),
        model_name,
        model_code,
        developer_mode: text(d, "developerMode") == "1",
        developer_ip,
        os: text(d, "OS"),
    })
}

/// «21_NIKEL_UHD» → 2021: Samsung's platform codes start with the model year.
pub fn year_of(model_code: &str) -> Option<u16> {
    let (yy, rest) = model_code.split_at_checked(2)?;
    if !rest.starts_with('_') || !yy.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(2000 + yy.parse::<u16>().ok()?)
}

fn home_network(ip: &Ipv4Addr) -> bool {
    retv_bridge_core::validate::is_lan_ipv4(&Value::String(ip.to_string()))
}

/// The Mac's address to type as the TV's Host PC IP: the one on the TV's /24,
/// else the first home-network one.
pub fn mac_ip_for(tv_ip: &str, ours: &[Ipv4Addr]) -> Option<Ipv4Addr> {
    let tv: Option<Ipv4Addr> = tv_ip.parse().ok();
    let lan: Vec<Ipv4Addr> = ours.iter().copied().filter(home_network).collect();
    if let Some(tv) = tv {
        let same = |a: &Ipv4Addr| a.octets()[..3] == tv.octets()[..3];
        if let Some(a) = lan.iter().copied().find(same) {
            return Some(a);
        }
    }
    lan.first().copied()
}

/// This computer's IPv4 addresses.
pub fn local_ipv4s() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
            _ => None,
        })
        .collect()
}

/// The client for the REST API: no proxy, `timeout` per request.
pub fn rest_client(timeout: Duration) -> Result<reqwest::Client, SamsungError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .no_proxy()
        .build()
        .map_err(|e| SamsungError::Other(e.to_string()))
}

/// GET `http://ip:port/api/v2/` (3 s).
pub async fn fetch_info(ip: &str, port: u16) -> Result<TvInfo, SamsungError> {
    fetch_info_with(&rest_client(REST_TIMEOUT)?, ip, port).await
}

/// [`fetch_info`] on a client of the caller's (its timeout).
pub async fn fetch_info_with(
    client: &reqwest::Client,
    ip: &str,
    port: u16,
) -> Result<TvInfo, SamsungError> {
    let url = format!("http://{ip}:{port}/api/v2/");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SamsungError::TvUnreachable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(SamsungError::NotSamsung);
    }
    let body = resp
        .text()
        .await
        .map_err(|e| SamsungError::TvUnreachable(e.to_string()))?;
    parse_info(ip, &body)
}
