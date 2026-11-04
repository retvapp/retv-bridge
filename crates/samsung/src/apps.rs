//! The TV's app list (`0 vd_applist`) and the cleanup rules: what is offered
//! for removal, what is pre-ticked, what is never touched.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TvApp {
    /// `app_id`: what `vd_appuninstall` takes.
    pub app_id: String,
    pub title: String,
    pub tizen_id: String,
    /// `None` when the TV gave no count: such an app is never pre-ticked.
    pub use_count: Option<u32>,
    pub removable: bool,
}

/// Drops ANSI colour codes (`ESC [ … m`).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `--------------app_title    =Netflix-------------` → (`app_title`, `Netflix`).
fn field(line: &str) -> Option<(String, String)> {
    let line = strip_ansi(line);
    let (k, v) = line.split_once('=')?;
    let key = k.trim_matches(|c: char| c == '-' || c.is_whitespace());
    let value = v.trim_end().trim_end_matches('-').trim();
    (!key.is_empty()).then(|| (key.to_string(), value.to_string()))
}

/// Every record of `vd_applist`; a record starts at `app_id`.
pub fn parse_applist(out: &str) -> Vec<TvApp> {
    let mut apps = Vec::new();
    let mut cur: Option<TvApp> = None;
    for line in out.lines() {
        let Some((k, v)) = field(line) else {
            continue;
        };
        if k == "app_id" {
            apps.extend(cur.take());
            cur = Some(TvApp {
                app_id: v,
                title: String::new(),
                tizen_id: String::new(),
                use_count: None,
                removable: false,
            });
            continue;
        }
        let Some(app) = cur.as_mut() else {
            continue;
        };
        match k.as_str() {
            "app_title" => app.title = v,
            "app_tizen_id" => app.tizen_id = v,
            "use_count" => app.use_count = v.parse().ok(),
            "is_removable" => app.removable = v == "1",
            _ => {}
        }
    }
    apps.extend(cur);
    // An id that could carry more than an id never reaches a command line.
    apps.retain(|a| valid_app_id(&a.app_id));
    apps
}

/// Apps ReTV never lists and never removes: ReTV itself and TizenBrew.
pub fn is_protected(app: &TvApp) -> bool {
    let title = app.title.to_lowercase();
    let id = app.tizen_id.to_lowercase();
    let app_id = app.app_id.to_lowercase();
    title.contains("tizenbrew")
        || id.contains("tizenbrew")
        || app_id == "retvhome00"
        || id.starts_with("retvhome00.")
        || app_id == "xvvl3s1bvh"
        || id.starts_with("xvvl3s1bvh.")
}

/// Apps people surely use even when Smart Hub counted no launches (the TV opens some of them by
/// itself): Netflix, YouTube, the photo gallery. Listed for removal, never pre-ticked.
pub fn kept_unticked(app: &TvApp) -> bool {
    let title = app.title.to_lowercase();
    let id = app.tizen_id.to_lowercase();
    ["netflix", "youtube", "gallery", "галере"]
        .iter()
        .any(|w| title.contains(w) || id.contains(w))
}

/// Offered for removal at all.
pub fn can_remove(app: &TvApp) -> bool {
    app.removable && !is_protected(app) && valid_app_id(&app.app_id)
}

/// `[A-Za-z0-9._-]{1,64}`: the shape of Smart Hub app ids (`3201907018807`,
/// `RetvHome00`).
pub fn valid_app_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The removal command for `app_id`, only for a valid id.
pub fn uninstall_command(app_id: &str) -> Option<String> {
    valid_app_id(app_id).then(|| format!("0 vd_appuninstall {app_id}"))
}

/// Ticked when the list opens: removable, never opened (an app used even once is the person's
/// call), and not one of the apps kept unticked.
pub fn pre_ticked(app: &TvApp) -> bool {
    app.use_count == Some(0) && can_remove(app) && !kept_unticked(app)
}

/// The «как было» list: `{ at, apps: [{ appId, title, tizenId, useCount }] }`.
pub fn before_list(apps: &[TvApp], at_ms: u64) -> String {
    let list: Vec<serde_json::Value> = apps
        .iter()
        .map(|a| {
            serde_json::json!({
                "appId": a.app_id,
                "title": a.title,
                "tizenId": a.tizen_id,
                "useCount": a.use_count,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "at": at_ms, "apps": list }))
        .unwrap_or_default()
}

/// `vd_appuninstall` ends a removal with `app_id[...] uninstall completed`; anything else (a failure, or no answer
/// because the TV closed sdb) is not a removal. Seen 2026-11-04: an empty answer was taken for success.
pub fn uninstall_ok(out: &str) -> bool {
    let l = out.to_lowercase();
    l.contains("uninstall completed") && !l.contains("fail")
}

/// How the TV answered one `vd_appuninstall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uninstall {
    /// `… uninstall completed`.
    Done,
    /// `can not uninstall app`, at once: an app Samsung put on the TV (e-Manual, Rakuten TV),
    /// even when `vd_applist` calls it removable. Seen 2026-11-04 on a QE55Q67AA.
    Builtin,
    /// Anything else: a failure, or no answer because the TV closed sdb.
    Failed,
}

pub fn uninstall_result(out: &str) -> Uninstall {
    if uninstall_ok(out) {
        Uninstall::Done
    } else if out.to_lowercase().contains("can not uninstall") {
        Uninstall::Builtin
    } else {
        Uninstall::Failed
    }
}

/// ReTV Home for Samsung, as `vd_applist` lists it.
pub fn is_retv_home(app: &TvApp) -> bool {
    app.app_id.eq_ignore_ascii_case("RetvHome00")
        || app.tizen_id.to_lowercase().starts_with("retvhome00.")
}

#[cfg(test)]
mod uninstall_answer_tests {
    use super::*;

    #[test]
    fn a_preloaded_app_is_told_apart_from_a_failure() {
        assert_eq!(
            uninstall_result(
                "uninstall 20212100002\ncan not uninstall app\nspend time for wascmd is [3]ms\n"
            ),
            Uninstall::Builtin
        );
        assert_eq!(
            uninstall_result("app_id[1] uninstall start\napp_id[1] uninstall completed\n"),
            Uninstall::Done
        );
        assert_eq!(uninstall_result(""), Uninstall::Failed);
        assert_eq!(
            uninstall_result("app_id[1] uninstall failed[-1]\n"),
            Uninstall::Failed
        );
    }

    #[test]
    fn netflix_youtube_gallery_are_offered_never_ticked_retv_and_tizenbrew_never_offered() {
        let app = |id: &str, title: &str, tizen: &str, uses: Option<u32>| TvApp {
            app_id: id.into(),
            title: title.into(),
            tizen_id: tizen.into(),
            use_count: uses,
            removable: true,
        };
        for a in [
            app("3201907018807", "Netflix", "org.tizen.netflix-app", Some(0)),
            app(
                "111299001912",
                "YouTube",
                "9Ur5IzDKqV.TizenYouTube",
                Some(0),
            ),
            app(
                "3201710015037",
                "Галерея",
                "com.samsung.tv.gallery",
                Some(0),
            ),
        ] {
            assert!(can_remove(&a), "{} is offered", a.title);
            assert!(!pre_ticked(&a), "{} is never pre-ticked", a.title);
        }
        for a in [
            app("RetvHome00", "ReTV Home", "RetvHome00.ReTVHome", Some(0)),
            app(
                "xvvl3S1bvH",
                "TizenBrewNextGeneration",
                "xvvl3S1bvH.TizenBrewStandalone",
                Some(0),
            ),
        ] {
            assert!(!can_remove(&a), "{} is never offered", a.title);
            assert!(!pre_ticked(&a));
        }
        assert!(pre_ticked(&app(
            "1",
            "Viaplay",
            "niYSnzL6h1.Viaplay",
            Some(0)
        )));
        for n in [1, 3, 4, 50] {
            let a = app("1", "Viaplay", "niYSnzL6h1.Viaplay", Some(n));
            assert!(!pre_ticked(&a), "{n} uses");
        }
        assert!(!pre_ticked(&app(
            "1",
            "Viaplay",
            "niYSnzL6h1.Viaplay",
            None
        )));
    }

    #[test]
    fn retv_home_is_found_in_the_list() {
        let app = |id: &str, tizen: &str| TvApp {
            app_id: id.into(),
            title: "ReTV Home".into(),
            tizen_id: tizen.into(),
            use_count: Some(2),
            removable: true,
        };
        assert!(is_retv_home(&app("RetvHome00", "RetvHome00.ReTVHome")));
        assert!(is_retv_home(&app("x", "RetvHome00.ReTVHome")));
        assert!(!is_retv_home(&app("3201907018807", "Netflix.app")));
    }

    #[test]
    fn only_a_completed_removal_counts() {
        assert!(uninstall_ok("uninstall 3202009021713\napp_id[3202009021713] uninstall start\napp_id[3202009021713] uninstall completed\nspend time for wascmd is [1367]ms\n"));
        assert!(!uninstall_ok(""));
        assert!(!uninstall_ok("app_id[1] uninstall start\n"));
        assert!(!uninstall_ok("app_id[1] uninstall failed[-1]\n"));
    }
}
