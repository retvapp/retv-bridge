//! «Слежка и реклама» on a Samsung TV: Samsung's Privacy Choices with the
//! remote keys, state-safe.
//!
//! Bridge can neither open Privacy Choices (a system app: REST and sdb cannot
//! launch it) nor read the ad setting; ReTV Home on the TV can do both. So
//! Bridge leaves a request for the TV on the account
//! (`POST /v1/apps/tvs/<id>/requests`) and starts ReTV Home over sdb
//! (`was_execute`); ReTV Home takes the request from the server:
//!
//! - `privacy` (`Kind::Open`): ReTV Home reads `isLATEnabled` and, when ads
//!   are on, opens Privacy Choices at once;
//! - `privacy-check` (`Kind::Check`): ReTV Home reads the flag again and
//!   shows the result.
//!
//! The request does not ride the launch: a 2021 QE55Q67AA ignores the remote
//! channel's `ed.apps.launch` altogether (no launch, no `PAYLOAD`; seen
//! 2026-11-04).
//!
//! ReTV Home reports each answer to the account with the check id; Bridge,
//! signed in to the same account, reads it with the account's TVs. When no
//! report comes (Bridge or the TV not signed in), the person says what the TV
//! shows.
//!
//! «agree to all» toggles all three boxes, so the state is read before and
//! after each round, at most two passes (some-on → all-on → all-off), and the
//! routine stops on anything it did not expect.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const MAX_ROUNDS: u8 = 2;
/// ReTV Home starts (cold, perhaps), reads the flag and reports.
pub const OPEN_REPORT_WAIT: Duration = Duration::from_secs(20);
/// ReTV Home comes to the front, lets the flag settle (1.5 s) and reports.
pub const CHECK_REPORT_WAIT: Duration = Duration::from_secs(15);
/// After ReTV Home said it opens Privacy Choices: the screen comes up with
/// its focus on «Terms & Conditions, Privacy Policy». A report that it did
/// not open arrives within this too.
pub const SCREEN_SETTLE: Duration = Duration::from_millis(2500);

/// The path verified on camera on a 2021 QE55Q67AA (Polish labels; the same
/// layout in every language). Each key, then the wait after it.
pub fn privacy_keys() -> Vec<(&'static str, Duration)> {
    vec![
        // «Terms & Conditions, Privacy Policy» → the terms screen, focus on
        // «agree to all». The screen downloads the terms first: 6 s was too
        // short on a slow day, 9 s is safe.
        ("KEY_ENTER", Duration::from_millis(9000)),
        // «agree to all» toggles all the boxes. When it ticks them the focus
        // jumps to OK by itself; when it unticks them the focus stays on
        // «agree to all», and a second ENTER would tick them again.
        ("KEY_ENTER", Duration::from_millis(1000)),
        // So: down. From «agree to all» this reaches OK; on OK it does nothing.
        ("KEY_DOWN", Duration::from_millis(800)),
        // OK saves and goes back to Privacy Choices.
        ("KEY_ENTER", Duration::from_millis(2000)),
        // «Exit? Do you want to exit?» with the focus on Cancel.
        ("KEY_RETURN", Duration::from_millis(1500)),
        ("KEY_LEFT", Duration::from_millis(700)),
        // Exit: Smart Hub (ReTV Home's return watch brings ReTV back).
        ("KEY_ENTER", Duration::from_millis(2500)),
    ]
}

/// What Bridge asks ReTV Home for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Read the flag; when ads are on, open Privacy Choices.
    Open,
    /// Read the flag again.
    Check,
}

impl Kind {
    /// The request's `kind` on the server.
    pub fn wire(self) -> &'static str {
        match self {
            Kind::Open => "privacy",
            Kind::Check => "privacy-check",
        }
    }
}

/// Whether the request reached the account before ReTV Home was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Left for the TV: ReTV Home takes it and reports.
    Account,
    /// Not sent (Bridge or the TV not signed in): ReTV Home only starts, no
    /// report will come, the person is asked at once.
    Unsent,
}

/// Interest-based ads as ReTV Home read them (`isLATEnabled`: true = off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ads {
    Off,
    On,
    Unknown,
}

impl Ads {
    /// `latEnabled` of the report.
    pub fn of_lat(lat: Option<bool>) -> Ads {
        match lat {
            Some(true) => Ads::Off,
            Some(false) => Ads::On,
            None => Ads::Unknown,
        }
    }
}

/// ReTV Home's answer to one request, through the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub ads: Ads,
    /// Privacy Choices: `Some(false)` did not open; `None` not said (yet).
    pub opened: Option<bool>,
}

/// Why the routine stopped without a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stop {
    /// ReTV Home did not start on the TV.
    LaunchFailed,
    /// ReTV Home says Samsung's screen did not open.
    NotOpened,
    /// The keys could not go out.
    KeysFailed,
    /// The person said the TV shows something else.
    Unexpected,
    /// «Остановить».
    Person,
}

/// Where the routine is; the window shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum PrivacyStep {
    /// Before the start.
    Ready,
    /// ReTV Home was launched; Bridge waits for its answer.
    Reading {
        round: u8,
    },
    /// Bridge presses the keys on Samsung's screen.
    Pressing {
        round: u8,
    },
    /// ReTV Home was launched again to read the result.
    Checking {
        round: u8,
    },
    /// No answer through the account: «What is on the TV now?»
    AskScreen {
        round: u8,
    },
    /// No answer after the keys: «What does ReTV Home say now?»
    AskResult {
        round: u8,
    },
    Done {
        pressed: bool,
    },
    /// The person unticks the boxes themselves (state unknown, or two passes
    /// did not do it).
    Manual,
    Stopped {
        why: Stop,
    },
}

impl PrivacyStep {
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            PrivacyStep::Reading { .. }
                | PrivacyStep::Pressing { .. }
                | PrivacyStep::Checking { .. }
                | PrivacyStep::AskScreen { .. }
                | PrivacyStep::AskResult { .. }
        )
    }
}

/// What the person says the TV shows (only when ReTV Home's answer did not
/// come through the account).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    /// Samsung's privacy screen is open.
    ScreenOpen,
    /// ReTV Home says interest-based ads are off.
    Off,
    /// ReTV Home says they are on.
    On,
    /// ReTV Home could not read the setting.
    Unknown,
    /// Something else is on the TV.
    Other,
}

/// The routine's hands: the TV, the account, the person, the clock.
pub trait PrivacyIo {
    /// Leaves the request `kind` with `check` for the TV (if it can), then
    /// starts ReTV Home; Err when ReTV Home did not start.
    fn launch(
        &mut self,
        kind: Kind,
        check: &str,
    ) -> impl Future<Output = Result<Delivery, String>> + Send;
    /// ReTV Home's report for `check`, waiting up to `wait` (zero: read
    /// once); `None` when none came (or Bridge is not signed in).
    fn report(
        &mut self,
        check: &str,
        wait: Duration,
    ) -> impl Future<Output = Option<Report>> + Send;
    /// The keys in order, each with its wait; returns early (Ok) when the
    /// person stops.
    fn keys(
        &mut self,
        keys: &[(&'static str, Duration)],
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Shows `step` and waits for the person's answer; `None` when stopped.
    fn ask(&mut self, step: PrivacyStep) -> impl Future<Output = Option<Answer>> + Send;
    fn sleep(&mut self, d: Duration) -> impl Future<Output = ()> + Send;
    fn show(&mut self, step: PrivacyStep);
    /// A fresh check id (8..32 letters and digits).
    fn check_id(&mut self) -> String;
    fn stopped(&self) -> bool;
    fn log(&mut self, line: String);
}

/// A fresh check id: 16 random letters and digits.
pub fn new_check_id() -> String {
    use rand::Rng;
    const ABC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut r = rand::thread_rng();
    (0..16)
        .map(|_| ABC[r.gen_range(0..ABC.len())] as char)
        .collect()
}

fn ads_word(a: Ads) -> &'static str {
    match a {
        Ads::Off => "off",
        Ads::On => "on",
        Ads::Unknown => "unknown",
    }
}

/// Runs the routine to its end (`Done`, `Manual` or `Stopped`).
pub async fn run<I: PrivacyIo>(io: &mut I) -> PrivacyStep {
    use PrivacyStep::*;
    for round in 1..=MAX_ROUNDS {
        // Before: ReTV Home reads the flag and opens Privacy Choices if ads are on.
        io.show(Reading { round });
        let check = io.check_id();
        let sent = match io.launch(Kind::Open, &check).await {
            Ok(d) => d,
            Err(e) => {
                io.log(format!("privacy: ReTV Home did not start ({e})"));
                return Stopped {
                    why: Stop::LaunchFailed,
                };
            }
        };
        let first = match sent {
            Delivery::Account => io.report(&check, OPEN_REPORT_WAIT).await,
            Delivery::Unsent => None,
        };
        match first {
            Some(r) => {
                io.log(format!(
                    "privacy: round {round}, ReTV Home says ads are {}",
                    ads_word(r.ads)
                ));
                match r.ads {
                    Ads::Off => return Done { pressed: round > 1 },
                    Ads::Unknown => return Manual,
                    Ads::On if r.opened == Some(false) => {
                        return Stopped {
                            why: Stop::NotOpened,
                        }
                    }
                    Ads::On => {
                        io.sleep(SCREEN_SETTLE).await;
                        // A later report may say Samsung's screen did not open after all.
                        let again = io.report(&check, Duration::ZERO).await;
                        if again.is_some_and(|r| r.opened == Some(false)) {
                            return Stopped {
                                why: Stop::NotOpened,
                            };
                        }
                    }
                }
            }
            None => {
                io.log(format!(
                    "privacy: round {round}, no answer through the account; asking"
                ));
                match io.ask(AskScreen { round }).await {
                    Some(Answer::ScreenOpen) => {}
                    Some(Answer::Off) => return Done { pressed: round > 1 },
                    Some(Answer::Unknown) => return Manual,
                    Some(Answer::On) => {
                        return Stopped {
                            why: Stop::NotOpened,
                        }
                    }
                    Some(Answer::Other) => {
                        return Stopped {
                            why: Stop::Unexpected,
                        }
                    }
                    None => return Stopped { why: Stop::Person },
                }
            }
        }
        if io.stopped() {
            return Stopped { why: Stop::Person };
        }

        io.show(Pressing { round });
        if let Err(e) = io.keys(&privacy_keys()).await {
            io.log(format!("privacy: keys: {e}"));
            return Stopped {
                why: Stop::KeysFailed,
            };
        }
        if io.stopped() {
            io.log("privacy: stopped by the person".into());
            return Stopped { why: Stop::Person };
        }

        // After: ReTV Home reads the flag again.
        io.show(Checking { round });
        let check = io.check_id();
        let sent = match io.launch(Kind::Check, &check).await {
            Ok(d) => d,
            Err(e) => {
                io.log(format!("privacy: ReTV Home did not start ({e})"));
                return Stopped {
                    why: Stop::LaunchFailed,
                };
            }
        };
        let read = match sent {
            Delivery::Account => io.report(&check, CHECK_REPORT_WAIT).await,
            Delivery::Unsent => None,
        };
        let after = match read {
            Some(r) => r.ads,
            None => match io.ask(AskResult { round }).await {
                Some(Answer::Off) => Ads::Off,
                Some(Answer::On) => Ads::On,
                Some(Answer::Unknown) => Ads::Unknown,
                Some(Answer::ScreenOpen | Answer::Other) => {
                    return Stopped {
                        why: Stop::Unexpected,
                    }
                }
                None => return Stopped { why: Stop::Person },
            },
        };
        io.log(format!(
            "privacy: after round {round}, ads are {}",
            ads_word(after)
        ));
        match after {
            Ads::Off => return Done { pressed: true },
            Ads::Unknown => return Manual,
            Ads::On => continue,
        }
    }
    Manual
}
