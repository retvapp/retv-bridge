//! The privacy routine with a fake TV, account and person.

use std::collections::VecDeque;
use std::time::Duration;

use retv_bridge_samsung::privacy::{
    new_check_id, privacy_keys, run, Ads, Answer, Delivery, Kind, PrivacyIo, PrivacyStep, Report,
    Stop, CHECK_REPORT_WAIT, OPEN_REPORT_WAIT, SCREEN_SETTLE,
};

#[derive(Default)]
struct Fake {
    /// What each `report` call answers, in order.
    reports: VecDeque<Option<Report>>,
    /// What the person answers, in order.
    answers: VecDeque<Option<Answer>>,
    launch_fails: bool,
    /// The request could not be left on the account (not signed in).
    unsent: bool,
    keys_fail: bool,
    /// The person presses «Остановить» while the keys go.
    stop_in_keys: bool,
    stopped: bool,
    n: u32,
    events: Vec<String>,
}

impl PrivacyIo for Fake {
    async fn launch(&mut self, kind: Kind, check: &str) -> Result<Delivery, String> {
        self.events.push(format!("launch {} {check}", kind.wire()));
        if self.launch_fails {
            Err("refused".into())
        } else if self.unsent {
            Ok(Delivery::Unsent)
        } else {
            Ok(Delivery::Account)
        }
    }
    async fn report(&mut self, check: &str, wait: Duration) -> Option<Report> {
        self.events
            .push(format!("report {check} {}", wait.as_millis()));
        self.reports.pop_front().expect("an unexpected report read")
    }
    async fn keys(&mut self, keys: &[(&'static str, Duration)]) -> Result<(), String> {
        self.events.push(format!("keys {}", keys.len()));
        if self.stop_in_keys {
            self.stopped = true;
        }
        if self.keys_fail {
            Err("down".into())
        } else {
            Ok(())
        }
    }
    async fn ask(&mut self, step: PrivacyStep) -> Option<Answer> {
        self.events.push(format!("ask {step:?}"));
        self.answers.pop_front().expect("an unexpected question")
    }
    async fn sleep(&mut self, d: Duration) {
        self.events.push(format!("sleep {}", d.as_millis()));
    }
    fn show(&mut self, step: PrivacyStep) {
        self.events.push(format!("show {step:?}"));
    }
    fn check_id(&mut self) -> String {
        self.n += 1;
        format!("Check{:04}", self.n)
    }
    fn stopped(&self) -> bool {
        self.stopped
    }
    fn log(&mut self, _: String) {}
}

const fn rep(ads: Ads, opened: Option<bool>) -> Option<Report> {
    Some(Report { ads, opened })
}

fn fake(reports: Vec<Option<Report>>, answers: Vec<Option<Answer>>) -> Fake {
    Fake {
        reports: reports.into(),
        answers: answers.into(),
        ..Fake::default()
    }
}

fn keys_sent(f: &Fake) -> usize {
    f.events.iter().filter(|e| e.starts_with("keys")).count()
}

const OPEN: u128 = OPEN_REPORT_WAIT.as_millis();
const CHECK: u128 = CHECK_REPORT_WAIT.as_millis();

#[tokio::test]
async fn ads_on_one_pass_turns_them_off() {
    let mut f = fake(
        vec![rep(Ads::On, None), rep(Ads::On, None), rep(Ads::Off, None)],
        vec![],
    );
    assert_eq!(run(&mut f).await, PrivacyStep::Done { pressed: true });
    assert_eq!(
        f.events,
        [
            "show Reading { round: 1 }".to_string(),
            "launch privacy Check0001".into(),
            format!("report Check0001 {OPEN}"),
            format!("sleep {}", SCREEN_SETTLE.as_millis()),
            // Read once more: ReTV Home may say Samsung's screen did not open after all.
            "report Check0001 0".into(),
            "show Pressing { round: 1 }".into(),
            "keys 7".into(),
            "show Checking { round: 1 }".into(),
            "launch privacy-check Check0002".into(),
            format!("report Check0002 {CHECK}"),
        ]
    );
}

#[tokio::test]
async fn already_off_or_unknown_presses_nothing() {
    let mut f = fake(vec![rep(Ads::Off, Some(false))], vec![]);
    assert_eq!(run(&mut f).await, PrivacyStep::Done { pressed: false });
    assert_eq!(keys_sent(&f), 0);
    let mut f = fake(vec![rep(Ads::Unknown, Some(false))], vec![]);
    assert_eq!(run(&mut f).await, PrivacyStep::Manual);
    assert_eq!(keys_sent(&f), 0);
}

#[tokio::test]
async fn some_on_goes_all_on_then_all_off_in_two_passes() {
    let mut f = fake(
        vec![
            rep(Ads::On, None),
            None,
            rep(Ads::On, None),
            rep(Ads::On, None),
            None,
            rep(Ads::Off, None),
        ],
        vec![],
    );
    assert_eq!(run(&mut f).await, PrivacyStep::Done { pressed: true });
    assert_eq!(keys_sent(&f), 2);
    assert!(f.events.contains(&"launch privacy Check0003".to_string()));
}

#[tokio::test]
async fn never_more_than_two_passes() {
    let mut f = fake(
        vec![
            rep(Ads::On, None),
            None,
            rep(Ads::On, None),
            rep(Ads::On, None),
            None,
            rep(Ads::On, None),
        ],
        vec![],
    );
    assert_eq!(run(&mut f).await, PrivacyStep::Manual);
    assert_eq!(keys_sent(&f), 2);
    // Unknown after the keys: no third guess either.
    let mut f = fake(
        vec![rep(Ads::On, None), None, rep(Ads::Unknown, None)],
        vec![],
    );
    assert_eq!(run(&mut f).await, PrivacyStep::Manual);
    assert_eq!(keys_sent(&f), 1);
}

#[tokio::test]
async fn stops_on_anything_unexpected() {
    // Samsung's screen did not open: at once, or said a moment later.
    let mut f = fake(vec![rep(Ads::On, Some(false))], vec![]);
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped {
            why: Stop::NotOpened
        }
    );
    let mut f = fake(vec![rep(Ads::On, None), rep(Ads::On, Some(false))], vec![]);
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped {
            why: Stop::NotOpened
        }
    );
    assert_eq!(keys_sent(&f), 0);
    // ReTV Home did not start.
    let mut f = fake(vec![], vec![]);
    f.launch_fails = true;
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped {
            why: Stop::LaunchFailed
        }
    );
    // The keys broke off.
    let mut f = fake(vec![rep(Ads::On, None), None], vec![]);
    f.keys_fail = true;
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped {
            why: Stop::KeysFailed
        }
    );
    // «Остановить» during the keys: no check launch, no second round.
    let mut f = fake(vec![rep(Ads::On, None), None], vec![]);
    f.stop_in_keys = true;
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped { why: Stop::Person }
    );
    assert!(!f.events.iter().any(|e| e.contains("privacy-check")));
}

#[tokio::test]
async fn a_request_that_could_not_be_left_asks_the_person_at_once() {
    // Not signed in (Bridge or the TV): ReTV Home only starts; no report is waited for.
    let mut f = fake(vec![], vec![Some(Answer::ScreenOpen), Some(Answer::Off)]);
    f.unsent = true;
    assert_eq!(run(&mut f).await, PrivacyStep::Done { pressed: true });
    assert!(!f.events.iter().any(|e| e.starts_with("report")));
    assert_eq!(
        f.events,
        [
            "show Reading { round: 1 }",
            "launch privacy Check0001",
            "ask AskScreen { round: 1 }",
            "show Pressing { round: 1 }",
            "keys 7",
            "show Checking { round: 1 }",
            "launch privacy-check Check0002",
            "ask AskResult { round: 1 }",
        ]
    );
    assert_eq!(
        (Kind::Open.wire(), Kind::Check.wire()),
        ("privacy", "privacy-check")
    );
}

#[tokio::test]
async fn without_the_account_the_person_says_what_the_tv_shows() {
    let mut f = fake(
        vec![None, None],
        vec![Some(Answer::ScreenOpen), Some(Answer::Off)],
    );
    assert_eq!(run(&mut f).await, PrivacyStep::Done { pressed: true });
    assert!(f.events.contains(&"ask AskScreen { round: 1 }".to_string()));
    assert!(f.events.contains(&"ask AskResult { round: 1 }".to_string()));
    assert_eq!(keys_sent(&f), 1);
    // No settle wait when the person already sees the screen.
    assert!(!f.events.iter().any(|e| e.starts_with("sleep")));

    for (answer, end) in [
        (Answer::Off, PrivacyStep::Done { pressed: false }),
        (Answer::Unknown, PrivacyStep::Manual),
        (
            Answer::On,
            PrivacyStep::Stopped {
                why: Stop::NotOpened,
            },
        ),
        (
            Answer::Other,
            PrivacyStep::Stopped {
                why: Stop::Unexpected,
            },
        ),
    ] {
        let mut f = fake(vec![None], vec![Some(answer)]);
        assert_eq!(run(&mut f).await, end, "{answer:?}");
        assert_eq!(keys_sent(&f), 0);
    }
    // Samsung's screen still open after the keys is not a result.
    let mut f = fake(
        vec![None, None],
        vec![Some(Answer::ScreenOpen), Some(Answer::ScreenOpen)],
    );
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped {
            why: Stop::Unexpected
        }
    );
    // Stopped while asked.
    let mut f = fake(vec![None], vec![None]);
    assert_eq!(
        run(&mut f).await,
        PrivacyStep::Stopped { why: Stop::Person }
    );
}

#[test]
fn the_key_path_is_the_one_verified_on_camera() {
    let keys: Vec<(&str, u64)> = privacy_keys()
        .iter()
        .map(|(k, d)| (*k, d.as_millis() as u64))
        .collect();
    assert_eq!(
        keys,
        [
            ("KEY_ENTER", 9000),
            ("KEY_ENTER", 1000),
            ("KEY_DOWN", 800),
            ("KEY_ENTER", 2000),
            ("KEY_RETURN", 1500),
            ("KEY_LEFT", 700),
            ("KEY_ENTER", 2500),
        ]
    );
}

#[test]
fn check_ids_are_fresh_letters_and_digits() {
    let a = new_check_id();
    assert_eq!(a.len(), 16);
    assert!(a.bytes().all(|b| b.is_ascii_alphanumeric()));
    assert_ne!(a, new_check_id());
}

#[test]
fn steps_serialise_for_the_window() {
    let j = |s: PrivacyStep| serde_json::to_value(s).unwrap();
    assert_eq!(
        j(PrivacyStep::Ready),
        serde_json::json!({ "step": "ready" })
    );
    assert_eq!(
        j(PrivacyStep::AskScreen { round: 2 }),
        serde_json::json!({ "step": "askScreen", "round": 2 })
    );
    assert_eq!(
        j(PrivacyStep::Stopped {
            why: Stop::NotOpened
        }),
        serde_json::json!({ "step": "stopped", "why": "notOpened" })
    );
    assert_eq!(
        serde_json::from_str::<Answer>("\"screenOpen\"").unwrap(),
        Answer::ScreenOpen
    );
}
