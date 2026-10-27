//! Replays `conformance/validators.json` against the Rust validators.

use retv_bridge_core::validate::{
    decode_base64, group_services, is_allowed_apk_url, is_allowed_binary_path,
    is_allowed_device_path, is_lan_ipv4, is_port, split_address, MdnsService,
};
use serde_json::{json, Value};

/// Walks up from this crate to the folder that holds `conformance/`.
fn conformance_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("conformance"))
        .find(|d| d.is_dir())
        .expect("conformance/ above the crate")
}

fn fixture() -> String {
    std::fs::read_to_string(conformance_dir().join("validators.json")).unwrap()
}

/// Validators the fixture may name. `isAllowedApkUrl` vectors arrive with protocol v2.
const OPTIONAL: &[&str] = &["isAllowedApkUrl"];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The fixture gives adb's mDNS lines as `{ name, service, address }` (the TS shape); the Rust
/// discovery already has the address split, so an address that does not split never reaches it.
fn services(input: &Value) -> Vec<MdnsService> {
    input
        .as_array()
        .expect("groupServices input is an array")
        .iter()
        .filter_map(|s| {
            let (ip, port) = split_address(s["address"].as_str().unwrap_or(""))?;
            Some(MdnsService {
                instance: s["name"].as_str().unwrap().to_string(),
                service: s["service"].as_str().unwrap().to_string(),
                ip,
                port,
            })
        })
        .collect()
}

fn call(name: &str, input: &Value) -> Value {
    match name {
        "isLanIpv4" => json!(is_lan_ipv4(input)),
        "isPort" => json!(is_port(input)),
        "isAllowedDevicePath" => json!(is_allowed_device_path(input)),
        "isAllowedBinaryPath" => json!(is_allowed_binary_path(input)),
        "splitAddress" => match split_address(input.as_str().expect("splitAddress takes a string"))
        {
            Some((ip, port)) => json!({ "ip": ip, "port": port }),
            None => Value::Null,
        },
        "decodeBase64" => match decode_base64(input) {
            Some(bytes) => json!(hex(&bytes)),
            None => Value::Null,
        },
        "isAllowedApkUrl" => json!(input.as_str().is_some_and(is_allowed_apk_url)),
        "groupServices" => serde_json::to_value(group_services(&services(input))).unwrap(),
        other => panic!("validators.json names an unknown validator {other}"),
    }
}

const KNOWN: &[&str] = &[
    "isLanIpv4",
    "isPort",
    "isAllowedDevicePath",
    "isAllowedBinaryPath",
    "splitAddress",
    "decodeBase64",
    "isAllowedApkUrl",
    "groupServices",
];

fn cases() -> serde_json::Map<String, Value> {
    serde_json::from_str::<Value>(&fixture())
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn covers_exactly_the_known_validators() {
    let cases = cases();
    for name in cases.keys() {
        assert!(KNOWN.contains(&name.as_str()), "unknown validator {name}");
    }
    for name in KNOWN.iter().filter(|n| !OPTIONAL.contains(n)) {
        assert!(cases.contains_key(*name), "validators.json has no {name}");
    }
}

fn replay(name: &str) {
    let cases = cases();
    let vectors = cases[name].as_array().unwrap();
    assert!(!vectors.is_empty());
    for v in vectors {
        let (input, expected) = (&v[0], &v[1]);
        assert_eq!(&call(name, input), expected, "{name}({input})");
    }
}

#[test]
fn is_lan_ipv4_vectors() {
    replay("isLanIpv4");
}

#[test]
fn is_port_vectors() {
    replay("isPort");
}

#[test]
fn is_allowed_device_path_vectors() {
    replay("isAllowedDevicePath");
}

#[test]
fn is_allowed_binary_path_vectors() {
    replay("isAllowedBinaryPath");
}

#[test]
fn split_address_vectors() {
    replay("splitAddress");
}

#[test]
fn decode_base64_vectors() {
    replay("decodeBase64");
}

#[test]
fn group_services_vectors() {
    replay("groupServices");
}

#[test]
fn is_allowed_apk_url_vectors_when_present() {
    if cases().contains_key("isAllowedApkUrl") {
        replay("isAllowedApkUrl");
    }
}
