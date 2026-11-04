mod support;

use retv_bridge_samsung::apps::{
    before_list, can_remove, is_protected, kept_unticked, parse_applist, pre_ticked,
    uninstall_command, uninstall_ok, valid_app_id, TvApp,
};
use support::fixture;

fn list() -> Vec<TvApp> {
    // The TV colours some lines; the fixture marks where (ESC is added here).
    let raw = fixture("vd_applist.txt")
        .replace("[0;32m", "\x1b[0;32m")
        .replace("[0m", "\x1b[0m");
    parse_applist(&raw)
}

fn find<'a>(apps: &'a [TvApp], title: &str) -> &'a TvApp {
    apps.iter().find(|a| a.title == title).unwrap()
}

#[test]
fn parses_records() {
    let apps = list();
    assert_eq!(apps.len(), 6);
    let n = find(&apps, "Netflix");
    assert_eq!(n.app_id, "3201907018807");
    assert_eq!(n.tizen_id, "org.tizen.netflix-app");
    assert_eq!(n.use_count, Some(183));
    assert!(n.removable);
    let retv = find(&apps, "ReTV Home");
    assert_eq!(retv.app_id, "RetvHome00", "ANSI codes are stripped");
    assert!(!find(&apps, "e-Manual").removable);
}

#[test]
fn missing_use_count_is_none_and_not_ticked() {
    let apps = list();
    let meet = find(&apps, "Google Meet");
    assert_eq!(meet.use_count, None);
    assert!(!pre_ticked(meet));
    assert!(can_remove(meet), "the person may still tick it");
}

#[test]
fn only_unused_unprotected_removable_apps_are_ticked() {
    let apps = list();
    let ticked: Vec<&str> = apps
        .iter()
        .filter(|a| pre_ticked(a))
        .map(|a| a.title.as_str())
        .collect();
    assert_eq!(ticked, ["Spotify - Music and Podcasts"]);
}

#[test]
fn gallery_netflix_youtube_never_ticked() {
    let mk = |title: &str, tizen: &str| TvApp {
        app_id: "1".into(),
        title: title.into(),
        tizen_id: tizen.into(),
        use_count: Some(0),
        removable: true,
    };
    // Offered, so the person may tick them; never ticked by ReTV.
    for a in [
        mk("Netflix", "org.tizen.netflix-app"),
        mk("YouTube", "9Ur5IzDKqV.TizenYouTube"),
        mk("Gallery", "com.samsung.tv.gallery"),
        mk("Галерея", "com.samsung.tv.gallery"),
    ] {
        assert!(!is_protected(&a), "{}", a.title);
        assert!(kept_unticked(&a), "{}", a.title);
        assert!(!pre_ticked(&a), "{}", a.title);
        assert!(can_remove(&a), "{}", a.title);
    }
    // Never offered at all.
    let brew = mk("TizenBrewNextGeneration", "xvvl3S1bvH.TizenBrewStandalone");
    assert!(is_protected(&brew));
    assert!(!pre_ticked(&brew));
    assert!(!can_remove(&brew));
}

#[test]
fn every_removable_app_is_offered_but_retv() {
    let apps = list();
    let offered: Vec<&str> = apps
        .iter()
        .filter(|a| can_remove(a))
        .map(|a| a.title.as_str())
        .collect();
    assert!(offered.contains(&"Netflix"), "{offered:?}");
    assert!(!offered.contains(&"ReTV Home"), "{offered:?}");
}

#[test]
fn retv_and_not_removable_cannot_be_removed() {
    let apps = list();
    assert!(!can_remove(find(&apps, "ReTV Home")));
    assert!(!can_remove(find(&apps, "e-Manual")));
}

#[test]
fn before_list_is_json_with_every_app() {
    let apps = list();
    let json: serde_json::Value =
        serde_json::from_str(&before_list(&apps, 1_762_000_000_000)).unwrap();
    assert_eq!(json["at"], 1_762_000_000_000u64);
    assert_eq!(json["apps"].as_array().unwrap().len(), 6);
    assert_eq!(json["apps"][0]["appId"], "3201907018807");
    assert_eq!(json["apps"][0]["useCount"], 183);
    assert_eq!(json["apps"][3]["useCount"], serde_json::Value::Null);
}

#[test]
fn uninstall_verdict() {
    assert!(uninstall_ok(
        "app_id[1] uninstall completed\nspend time for wascmd is [80]ms\n"
    ));
    assert!(!uninstall_ok("spend time for wascmd is [80]ms\n"));
    assert!(!uninstall_ok("app_id[1] uninstall failed[118]"));
    assert!(!uninstall_ok("ERROR: no such app"));
}

#[test]
fn app_ids_are_checked() {
    assert!(valid_app_id("3201907018807"));
    assert!(valid_app_id("org.tizen.netflix-app"));
    assert!(valid_app_id("RetvHome00"));
    let long = "a".repeat(65);
    for bad in ["", "1; reboot", "a b", "x\n0 getduid", "a/b", long.as_str()] {
        assert!(!valid_app_id(bad), "{bad:?}");
    }
    let raw = "--------------app_id      =1;reboot-------------\n\
--------------app_title   =Evil-------------\n\
--------------use_count   =0-------------\n\
--------------is_removable=1-------------\n\
--------------app_id      =42-------------\n\
--------------app_title   =Fine-------------\n\
--------------is_removable=1-------------\n";
    let apps = parse_applist(raw);
    assert_eq!(apps.len(), 1, "the record with a bad id is dropped");
    assert_eq!(apps[0].app_id, "42");
    assert_eq!(
        uninstall_command("42").as_deref(),
        Some("0 vd_appuninstall 42")
    );
    assert_eq!(uninstall_command("42 ; reboot"), None);
    let bad = TvApp {
        app_id: "1 2".into(),
        title: "x".into(),
        tizen_id: "x".into(),
        use_count: Some(0),
        removable: true,
    };
    assert!(!can_remove(&bad));
}
