//! The pure input checks, ported from the TypeScript bridge's `protocol.ts` and
//! pinned by `conformance/validators.json`. Each takes the raw JSON value where
//! the TS version takes `unknown`: a value of the wrong type is a "no", never a panic.

use serde_json::Value;

use crate::api::DiscoveredTv;
use crate::protocol::{APK_URL_PREFIXES, DEVICE_STAGING_FILES, DEVICE_WRITE_DIR};

/// `[1-9]?\d`, `1\d\d`, `2[0-4]\d`, `25[0-5]`: 0..255 without leading zeros.
fn is_octet(part: &str) -> bool {
    let b = part.as_bytes();
    if b.is_empty() || b.len() > 3 || !b.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if b.len() > 1 && b[0] == b'0' {
        return false;
    }
    part.parse::<u16>().is_ok_and(|n| n <= 255)
}

fn lan_ipv4(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 || !parts.iter().all(|p| is_octet(p)) {
        return false;
    }
    let a: u16 = parts[0].parse().unwrap();
    let b: u16 = parts[1].parse().unwrap();
    a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 169 && b == 254)
}

/// Dotted IPv4 inside a home-network range (10/8, 172.16/12, 192.168/16, 169.254/16).
pub fn is_lan_ipv4(ip: &Value) -> bool {
    ip.as_str().is_some_and(lan_ipv4)
}

/// A JSON number whose value is an integer 1..65535, however it is spelled
/// (`5555.0` and `1e3` are integers); a string never is.
pub fn is_port(port: &Value) -> bool {
    port_value(port).is_some()
}

/// The port `is_port` accepts, as a `u16`.
pub fn port_value(port: &Value) -> Option<u16> {
    let n = port.as_number()?;
    if let Some(u) = n.as_u64() {
        return u16::try_from(u).ok().filter(|p| *p > 0);
    }
    if n.is_i64() {
        return None;
    }
    let f = n.as_f64()?;
    if f.fract() == 0.0 && (1.0..=65535.0).contains(&f) {
        Some(f as u16)
    } else {
        None
    }
}

fn device_path(path: &str) -> bool {
    if path.encode_utf16().count() > 512 || path.contains(['\0', '\r', '\n']) {
        return false;
    }
    if !path.starts_with(DEVICE_WRITE_DIR) || path.len() == DEVICE_WRITE_DIR.len() {
        return false;
    }
    // posix.normalize(path) === path and no trailing '/': no empty, `.` or `..` segment.
    path[1..]
        .split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// A normalised absolute path strictly below `DEVICE_WRITE_DIR`, at most 512
/// UTF-16 units, no NUL, CR or LF, no `//`, `.`, `..` or trailing `/`.
pub fn is_allowed_device_path(path: &Value) -> bool {
    path.as_str().is_some_and(device_path)
}

/// Where `writeBytes` may write: anywhere `writeFile` may, plus the fixed staging files.
pub fn is_allowed_binary_path(path: &Value) -> bool {
    path.as_str()
        .is_some_and(|p| device_path(p) || DEVICE_STAGING_FILES.contains(&p))
}

/// Splits `"192.168.1.5:37000"` at the last `:`; `None` unless the rest is a
/// plain decimal port 1..65535 (no sign, space, leading zero, `0x`, exponent or fraction).
pub fn split_address(address: &str) -> Option<(String, u16)> {
    let at = address.rfind(':')?;
    let digits = &address[at + 1..];
    let b = digits.as_bytes();
    if b.is_empty() || b.len() > 5 || b[0] == b'0' || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let port: u32 = digits.parse().ok()?;
    let port = u16::try_from(port).ok()?;
    Some((address[..at].to_string(), port))
}

fn b64_value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn decode_canonical(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let quads = b.len() / 4;
    for (i, q) in b.as_chunks::<4>().0.iter().enumerate() {
        let last = i + 1 == quads;
        let pad = match (q[2], q[3]) {
            (b'=', b'=') if last => 2,
            (_, b'=') if last => 1,
            _ => 0,
        };
        let mut v = [0u8; 4];
        for (k, c) in q[..4 - pad].iter().enumerate() {
            v[k] = b64_value(*c)?;
        }
        let n = (u32::from(v[0]) << 18)
            | (u32::from(v[1]) << 12)
            | (u32::from(v[2]) << 6)
            | u32::from(v[3]);
        match pad {
            0 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]),
            // Canonical: the bits a padded quad does not use must be zero (`YR==` is not).
            1 if v[2] & 0b11 == 0 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8]),
            2 if v[1] & 0b1111 == 0 => out.push((n >> 16) as u8),
            _ => return None,
        }
    }
    Some(out)
}

/// Strict canonical standard base64 (padded, no whitespace, unused trailing bits zero).
pub fn decode_base64(data: &Value) -> Option<Vec<u8>> {
    decode_canonical(data.as_str()?)
}

/// `installApk` URLs: below one of `APK_URL_PREFIXES`, the rest `[A-Za-z0-9._/-]+\.apk`,
/// and no `..`, `?` or `#` anywhere.
pub fn is_allowed_apk_url(url: &str) -> bool {
    if url.contains("..") || url.contains(['?', '#']) {
        return false;
    }
    APK_URL_PREFIXES
        .iter()
        .any(|prefix| url.strip_prefix(prefix).is_some_and(apk_path))
}

/// The part after `/downloads/`: `[A-Za-z0-9._/-]+\.apk`.
fn apk_path(rest: &str) -> bool {
    rest.len() > ".apk".len()
        && rest.ends_with(".apk")
        && rest
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b'-'))
}

/// A dev site's APK (scripts/dev-stack.sh serves ReTV Home from the site itself):
/// `http://` + `localhost`, `127.0.0.1` or a home-network IPv4, `:<port>` (1..65535, plain
/// digits), then `/downloads/` and the same path rule as `is_allowed_apk_url`. Taken only
/// when `DEV_APK_URLS` is on (debug builds).
pub fn is_dev_apk_url(url: &str) -> bool {
    if url.contains("..") || url.contains(['?', '#']) {
        return false;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some(slash) = rest.find('/') else {
        return false;
    };
    let (authority, path) = rest.split_at(slash);
    let Some((host, _port)) = split_address(authority) else {
        return false;
    };
    (host == "localhost" || host == "127.0.0.1" || lan_ipv4(&host))
        && path.strip_prefix("/downloads/").is_some_and(apk_path)
}

/// Whether this build lets `installApk` download from a dev site (`is_dev_apk_url`): debug
/// builds only, never a release build.
#[cfg(debug_assertions)]
pub const DEV_APK_URLS: bool = true;
/// Whether this build lets `installApk` download from a dev site (`is_dev_apk_url`): debug
/// builds only, never a release build.
#[cfg(not(debug_assertions))]
pub const DEV_APK_URLS: bool = false;

/// Where this build's `installApk` downloads from: the ReTV site, and in a debug build also a dev site.
pub fn is_installable_apk_url(url: &str) -> bool {
    is_allowed_apk_url(url) || (DEV_APK_URLS && is_dev_apk_url(url))
}

/// One adb service seen over mDNS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdnsService {
    /// The DNS-SD instance name (`adb-<serial>-<random>`).
    pub instance: String,
    /// `_adb-tls-connect._tcp`, `_adb-tls-pairing._tcp` or `_adb._tcp`.
    pub service: String,
    /// Dotted IPv4 from the A record.
    pub ip: String,
    pub port: u16,
}

/// Groups mDNS services by IP: one entry per TV in a home-network range, in
/// first-seen order. A port of `groupServices` in the TypeScript bridge's `adb.ts`.
pub fn group_services(services: &[MdnsService]) -> Vec<DiscoveredTv> {
    let mut tvs: Vec<DiscoveredTv> = Vec::new();
    for s in services {
        if s.port == 0 || !lan_ipv4(&s.ip) {
            continue;
        }
        let idx = match tvs.iter().position(|tv| tv.ip == s.ip) {
            Some(i) => i,
            None => {
                tvs.push(DiscoveredTv {
                    name: s.instance.clone(),
                    ip: s.ip.clone(),
                    connect_port: None,
                    pair_port: None,
                });
                tvs.len() - 1
            }
        };
        let tv = &mut tvs[idx];
        if s.service.starts_with("_adb-tls-pairing.") {
            tv.pair_port = Some(s.port);
        } else if s.service.starts_with("_adb-tls-connect.") || s.service.starts_with("_adb.") {
            tv.connect_port.get_or_insert(s.port);
        }
    }
    tvs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn apk_url_rules() {
        assert!(is_allowed_apk_url(
            "https://retv.app/downloads/retv-home-1.2.apk"
        ));
        assert!(is_allowed_apk_url(
            "https://ru.retv.app/downloads/a/b_c-1.apk"
        ));
        assert!(!is_allowed_apk_url("https://retv.app/downloads/.apk"));
        assert!(!is_allowed_apk_url("https://retv.app/downloads/x.apk?v=1"));
        assert!(!is_allowed_apk_url("https://retv.app/downloads/x.apk#f"));
        assert!(!is_allowed_apk_url("https://retv.app/downloads/../x.apk"));
        assert!(!is_allowed_apk_url("https://retv.app/downloads/x.zip"));
        assert!(!is_allowed_apk_url("https://retv.app/downloads/a b.apk"));
        assert!(!is_allowed_apk_url("http://retv.app/downloads/x.apk"));
        assert!(!is_allowed_apk_url(
            "https://retv.app.evil.example/downloads/x.apk"
        ));
        assert!(!is_allowed_apk_url("https://retv.app/other/x.apk"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_debug_build_installs_from_a_dev_site() {
        assert!(is_installable_apk_url(
            "http://localhost:5180/downloads/retv-home.apk"
        ));
        assert!(is_installable_apk_url("https://retv.app/downloads/x.apk"));
        assert!(!is_installable_apk_url(
            "http://evil.example:5180/downloads/x.apk"
        ));
    }

    // Runs under `cargo test --release`.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_release_build_installs_only_from_the_retv_site() {
        assert!(!is_installable_apk_url(
            "http://localhost:5180/downloads/retv-home.apk"
        ));
        assert!(!is_installable_apk_url(
            "http://192.168.1.20:5180/downloads/x.apk"
        ));
        assert!(is_installable_apk_url("https://retv.app/downloads/x.apk"));
    }

    #[test]
    fn port_value_reads_integral_floats() {
        assert_eq!(port_value(&json!(5555.0)), Some(5555));
        assert_eq!(port_value(&json!(-0.0)), None);
        assert_eq!(port_value(&json!(-5)), None);
    }

    #[test]
    fn group_services_drops_port_zero() {
        let s = MdnsService {
            instance: "x".into(),
            service: "_adb-tls-connect._tcp".into(),
            ip: "192.168.1.7".into(),
            port: 0,
        };
        assert!(group_services(&[s]).is_empty());
    }
}
