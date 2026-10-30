//! Replays `conformance/commands.json` (the `run` allow-list, protocol v3) against the Rust bridge.

use retv_bridge_core::commands::allow_command;
use serde_json::Value;

/// Walks up from this crate to the folder that holds `conformance/`.
fn conformance_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("conformance"))
        .find(|d| d.is_dir())
        .expect("conformance/ above the crate")
}

#[test]
fn commands_json() {
    let text = std::fs::read_to_string(conformance_dir().join("commands.json")).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let vectors = doc["vectors"].as_array().expect("vectors");
    assert!(
        vectors.len() > 100,
        "commands.json has {} vectors",
        vectors.len()
    );
    let mut failures = Vec::new();
    for v in vectors {
        let (input, expected) = (&v[0], &v[1]);
        // A non-string is refused, never an error (the TS function takes `unknown`).
        let got = input.as_str().and_then(allow_command);
        let want = expected.as_str().map(str::to_string);
        if got != want {
            failures.push(format!("{input}: expected {want:?}, got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
