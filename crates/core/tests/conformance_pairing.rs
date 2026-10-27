//! Replays `conformance/pairing.json` against `Pairing` with a fake clock and a queue of codes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use retv_bridge_core::api::{Clock, CodeSource};
use retv_bridge_core::pairing::Pairing;
use serde_json::Value;

/// Walks up from this crate to the folder that holds `conformance/`.
fn conformance_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|d| d.join("conformance"))
        .find(|d| d.is_dir())
        .expect("conformance/ above the crate")
}

fn fixture() -> String {
    std::fs::read_to_string(conformance_dir().join("pairing.json")).unwrap()
}

/// Any fixed start; results must not depend on it.
const T0: u64 = 1_000_000;

struct FakeClock(AtomicU64);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct QueueCodes(Mutex<VecDeque<String>>);

impl CodeSource for QueueCodes {
    fn next_code(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .expect("fixture error: the scenario ran out of codes")
    }
}

#[test]
fn pairing_fixture_scenarios() {
    let scenarios: Value = serde_json::from_str(&fixture()).unwrap();
    let scenarios = scenarios.as_array().unwrap();
    assert!(!scenarios.is_empty());

    for sc in scenarios {
        let name = sc["name"].as_str().unwrap();
        let clock = Arc::new(FakeClock(AtomicU64::new(T0)));
        let codes: VecDeque<String> = sc["codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        let shown: Arc<Mutex<Vec<(String, u64)>>> = Arc::default();
        let disabled = Arc::new(AtomicUsize::new(0));

        let shown_cb = shown.clone();
        let disabled_cb = disabled.clone();
        let mut pairing = Pairing::new(
            sc["token"].as_str().unwrap().to_string(),
            clock.clone(),
            Arc::new(QueueCodes(Mutex::new(codes))),
            Box::new(move |code: &str, expires_at: u64| {
                shown_cb
                    .lock()
                    .unwrap()
                    .push((code.to_string(), expires_at))
            }),
            Box::new(move || {
                disabled_cb.fetch_add(1, Ordering::SeqCst);
            }),
        );

        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let now = T0 + step["at"].as_u64().unwrap();
            clock.0.store(now, Ordering::SeqCst);
            shown.lock().unwrap().clear();
            let origin = step["origin"].as_str().unwrap();
            let got = match step["op"].as_str().unwrap() {
                "start" => serde_json::to_value(pairing.start(origin)).unwrap(),
                "finish" => {
                    let code = step["code"].as_str().unwrap();
                    serde_json::to_value(pairing.finish(origin, code)).unwrap()
                }
                op => panic!("unknown op {op}"),
            };
            assert_eq!(got, step["expect"], "{name}: step {i}");

            let shown_now: Vec<String> = shown
                .lock()
                .unwrap()
                .iter()
                .map(|(c, _)| c.clone())
                .collect();
            match step.get("shown").and_then(Value::as_str) {
                Some(code) => assert_eq!(shown_now, [code], "{name}: step {i} shown"),
                None => assert!(
                    shown_now.is_empty(),
                    "{name}: step {i} showed {shown_now:?}"
                ),
            }
            if let Some(code) = step.get("shown").and_then(Value::as_str) {
                let live = pairing.live_code().expect("a shown code is live");
                assert_eq!(live.0, code, "{name}: step {i} live code");
                let expires_in = got["expiresIn"].as_u64().unwrap();
                assert!(
                    live.1 > now && live.1 - now <= expires_in * 1000,
                    "{name}: step {i} expiry"
                );
            }
        }

        let want = sc.get("disabledCalls").and_then(Value::as_u64).unwrap_or(0);
        assert_eq!(
            disabled.load(Ordering::SeqCst) as u64,
            want,
            "{name}: disabledCalls"
        );
    }
}
