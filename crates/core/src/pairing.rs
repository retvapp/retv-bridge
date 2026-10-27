//! Pairing the site with the bridge without a pasted token. The site asks for a
//! code (`pairStart`), the bridge shows 6 digits in its own window, the person
//! types them on the site (`pairFinish`) and gets the session token.
//!
//! A port of the TypeScript bridge's `pairing.ts`, pinned by `conformance/pairing.json`.
//! Rules (one `Pairing` per bridge run, shared by all sockets):
//! - the code is 6 digits, lives `PAIR_CODE_TTL_MS` and is single-use;
//! - it is bound to the origin that started it. While it is live, another origin
//!   gets `pair-busy` for both start and finish: its finish is never checked
//!   against the code and never counts as a miss;
//! - a start from the same origin while the code is live re-shows the same code
//!   with the same expiry and keeps the misses;
//! - the 5th miss burns the code and locks start and finish: `PAIR_LOCK_MS`,
//!   doubling with each further lock up to `PAIR_LOCK_MAX_MS`. The lock count
//!   lasts the whole run;
//! - one run issues at most `PAIR_MAX_STARTS` codes, then start is `pair-disabled`.
//!
//! Precedence. start: lock, live code (busy / re-show), disabled, new code.
//! finish: lock, no live code (expired), other origin (busy), check the code.

use std::sync::Arc;

use rand::rngs::OsRng;
use rand::Rng;
use serde::ser::{Serialize, SerializeMap, Serializer};
use subtle::ConstantTimeEq;

use crate::api::{Clock, CodeSource};
use crate::protocol::{
    PAIR_CODE_TTL_MS, PAIR_LOCK_MAX_MS, PAIR_LOCK_MS, PAIR_MAX_MISSES, PAIR_MAX_STARTS,
};

/// The answer to `pairStart`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairStart {
    /// `{ "expiresIn" }`: a code is showing; seconds until it expires.
    Code { expires_in: u64 },
    /// `{ "error": "pair-locked", "retryIn" }`
    Locked { retry_in: u64 },
    /// `{ "error": "pair-busy", "retryIn" }`
    Busy { retry_in: u64 },
    /// `{ "error": "pair-disabled" }`
    Disabled,
}

/// The answer to `pairFinish`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairFinish {
    /// `{ "token" }`: the session token.
    Paired(String),
    /// `{ "error": "bad-code", "attemptsLeft" }`
    BadCode { attempts_left: u32 },
    /// `{ "error": "pair-locked", "retryIn" }`
    Locked { retry_in: u64 },
    /// `{ "error": "pair-busy", "retryIn" }`
    Busy { retry_in: u64 },
    /// `{ "error": "pair-expired" }`
    Expired,
}

fn error_map<S: Serializer>(
    s: S,
    error: &str,
    extra: Option<(&str, u64)>,
) -> Result<S::Ok, S::Error> {
    let mut m = s.serialize_map(Some(1 + usize::from(extra.is_some())))?;
    m.serialize_entry("error", error)?;
    if let Some((k, v)) = extra {
        m.serialize_entry(k, &v)?;
    }
    m.end()
}

impl Serialize for PairStart {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PairStart::Code { expires_in } => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("expiresIn", expires_in)?;
                m.end()
            }
            PairStart::Locked { retry_in } => {
                error_map(s, "pair-locked", Some(("retryIn", *retry_in)))
            }
            PairStart::Busy { retry_in } => error_map(s, "pair-busy", Some(("retryIn", *retry_in))),
            PairStart::Disabled => error_map(s, "pair-disabled", None),
        }
    }
}

impl Serialize for PairFinish {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PairFinish::Paired(session) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("token", session)?;
                m.end()
            }
            PairFinish::BadCode { attempts_left } => error_map(
                s,
                "bad-code",
                Some(("attemptsLeft", u64::from(*attempts_left))),
            ),
            PairFinish::Locked { retry_in } => {
                error_map(s, "pair-locked", Some(("retryIn", *retry_in)))
            }
            PairFinish::Busy { retry_in } => {
                error_map(s, "pair-busy", Some(("retryIn", *retry_in)))
            }
            PairFinish::Expired => error_map(s, "pair-expired", None),
        }
    }
}

/// Codes from the operating system's CSPRNG.
pub struct OsCodeSource;

impl CodeSource for OsCodeSource {
    fn next_code(&self) -> String {
        format!("{:06}", OsRng.gen_range(0..1_000_000u32))
    }
}

fn is_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

/// Whole seconds left until `at`, at least 1 while it is in the future.
fn seconds_until(at: u64, now: u64) -> u64 {
    at.saturating_sub(now).div_ceil(1000).max(1)
}

struct Live {
    code: String,
    origin: String,
    expires_at: u64,
}

/// Shows a code to the person at the computer: `(code, expires_at_ms)`.
pub type OnCode = Box<dyn Fn(&str, u64) + Send + Sync>;
/// Fired once, when the run's `PAIR_MAX_STARTS` codes are used up.
pub type OnDisabled = Box<dyn Fn() + Send + Sync>;

/// The site pairing state machine.
pub struct Pairing {
    session: String,
    clock: Arc<dyn Clock>,
    codes: Arc<dyn CodeSource>,
    on_code: OnCode,
    on_disabled: OnDisabled,
    live: Option<Live>,
    misses: u32,
    locked_until: u64,
    locks: u32,
    starts: u32,
    disabled_reported: bool,
}

impl Pairing {
    pub fn new(
        token: String, // leakcheck:allow
        clock: Arc<dyn Clock>,
        codes: Arc<dyn CodeSource>,
        on_code: OnCode,
        on_disabled: OnDisabled,
    ) -> Self {
        Pairing {
            session: token,
            clock,
            codes,
            on_code,
            on_disabled,
            live: None,
            misses: 0,
            locked_until: 0,
            locks: 0,
            starts: 0,
            disabled_reported: false,
        }
    }

    /// Drops an expired code; returns the lock still in force, if any (in seconds).
    fn settle(&mut self, t: u64) -> Option<u64> {
        if self.live.as_ref().is_some_and(|l| t >= l.expires_at) {
            self.live = None;
            self.misses = 0;
        }
        (t < self.locked_until).then(|| seconds_until(self.locked_until, t))
    }

    pub fn start(&mut self, origin: &str) -> PairStart {
        let t = self.clock.now_ms();
        if let Some(retry_in) = self.settle(t) {
            return PairStart::Locked { retry_in };
        }
        if let Some(live) = &self.live {
            if origin != live.origin {
                return PairStart::Busy {
                    retry_in: seconds_until(live.expires_at, t),
                };
            }
            (self.on_code)(&live.code, live.expires_at);
            return PairStart::Code {
                expires_in: seconds_until(live.expires_at, t),
            };
        }
        if self.starts >= PAIR_MAX_STARTS {
            if !self.disabled_reported {
                self.disabled_reported = true;
                (self.on_disabled)();
            }
            return PairStart::Disabled;
        }
        let code = self.codes.next_code();
        assert!(
            is_code(&code),
            "pairing: code source returned a non 6-digit code"
        );
        self.starts += 1;
        self.misses = 0;
        let expires_at = t + PAIR_CODE_TTL_MS;
        (self.on_code)(&code, expires_at);
        self.live = Some(Live {
            code,
            origin: origin.to_string(),
            expires_at,
        });
        PairStart::Code {
            expires_in: seconds_until(expires_at, t),
        }
    }

    pub fn finish(&mut self, origin: &str, code: &str) -> PairFinish {
        let t = self.clock.now_ms();
        if let Some(retry_in) = self.settle(t) {
            return PairFinish::Locked { retry_in };
        }
        let Some(live) = &self.live else {
            return PairFinish::Expired;
        };
        if origin != live.origin {
            return PairFinish::Busy {
                retry_in: seconds_until(live.expires_at, t),
            };
        }
        if is_code(code) && bool::from(live.code.as_bytes().ct_eq(code.as_bytes())) {
            self.live = None;
            self.misses = 0;
            return PairFinish::Paired(self.session.clone());
        }
        self.misses += 1;
        if self.misses >= PAIR_MAX_MISSES {
            self.live = None;
            self.misses = 0;
            self.locks += 1;
            let lock = PAIR_LOCK_MS
                .saturating_mul(1u64 << (self.locks - 1).min(32))
                .min(PAIR_LOCK_MAX_MS);
            self.locked_until = t + lock;
            return PairFinish::Locked {
                retry_in: seconds_until(self.locked_until, t),
            };
        }
        PairFinish::BadCode {
            attempts_left: PAIR_MAX_MISSES - self.misses,
        }
    }

    /// The code showing now and when it expires (ms), if one is live.
    pub fn live_code(&self) -> Option<(String, u64)> {
        let t = self.clock.now_ms();
        self.live
            .as_ref()
            .filter(|l| t < l.expires_at)
            .map(|l| (l.code.clone(), l.expires_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_codes_are_six_digits() {
        for _ in 0..200 {
            assert!(is_code(&OsCodeSource.next_code()));
        }
    }

    #[test]
    fn results_serialize_like_the_ts_objects() {
        let v = |x: &dyn erased::Ser| x.json();
        assert_eq!(
            v(&PairStart::Code { expires_in: 120 }),
            r#"{"expiresIn":120}"#
        );
        assert_eq!(v(&PairStart::Disabled), r#"{"error":"pair-disabled"}"#);
        assert_eq!(v(&PairFinish::Paired("T".into())), r#"{"token":"T"}"#); // leakcheck:allow: fake token
        assert_eq!(
            v(&PairFinish::BadCode { attempts_left: 4 }),
            r#"{"error":"bad-code","attemptsLeft":4}"#
        );
        assert_eq!(
            v(&PairFinish::Busy { retry_in: 9 }),
            r#"{"error":"pair-busy","retryIn":9}"#
        );
    }

    mod erased {
        pub trait Ser {
            fn json(&self) -> String;
        }
        impl<T: serde::Serialize> Ser for T {
            fn json(&self) -> String {
                serde_json::to_string(self).unwrap()
            }
        }
    }
}
