use agent_juice::update::{
    check_for_update_at, claim_notification_at, is_release_url_allowed, is_update_available,
    load_state_from, parse_latest_release, UpdateState,
};
use chrono::{Duration, TimeZone, Utc};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_path(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "agent-juice-update-{name}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root.join("update-state.json")
}

fn release_json(version: &str) -> String {
    format!(
        r#"{{"tag_name":"v{version}","html_url":"https://github.com/Lv2dev/agent-juice/releases/tag/v{version}","draft":false,"prerelease":false}}"#
    )
}

#[test]
fn latest_release_parser_accepts_only_stable_allowlisted_semver() {
    let release = parse_latest_release(&release_json("0.1.3")).unwrap();
    assert_eq!(release.version, "0.1.3");
    assert_eq!(
        release.url,
        "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3"
    );

    assert!(parse_latest_release(
        r#"{"tag_name":"v0.1.3","html_url":"https://evil.example/releases/tag/v0.1.3"}"#
    )
    .is_err());
    assert!(parse_latest_release(
        r#"{"tag_name":"v0.1.3-beta.1","html_url":"https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3-beta.1"}"#
    )
    .is_err());
    assert!(parse_latest_release(
        r#"{"tag_name":"v0.1.3","html_url":"https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3","prerelease":true}"#
    )
    .is_err());
}

#[test]
fn semantic_version_comparison_is_numeric_and_strict() {
    assert!(is_update_available("0.1.2", "0.1.10").unwrap());
    assert!(!is_update_available("0.2.0", "0.1.99").unwrap());
    assert!(!is_update_available("v1.0.0", "1.0.0").unwrap());
    assert!(is_update_available("1.0", "1.0.1").is_err());
    assert!(is_update_available("1.0.0", "1.00.1").is_err());
}

#[test]
fn release_url_allowlist_rejects_redirect_and_path_tricks() {
    assert!(is_release_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases"
    ));
    assert!(is_release_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases/latest"
    ));
    assert!(is_release_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3"
    ));
    assert!(!is_release_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3/../../evil"
    ));
    assert!(!is_release_url_allowed(
        "https://github.com.evil.example/Lv2dev/agent-juice/releases/tag/v0.1.3"
    ));
    assert!(!is_release_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3?next=evil"
    ));
}

#[test]
fn automatic_checks_throttle_for_24_hours_and_manual_checks_bypass_cache() {
    let path = temp_path("throttle");
    let now = Utc.with_ymd_and_hms(2026, 7, 10, 3, 0, 0).unwrap();
    let fetches = AtomicUsize::new(0);
    let first = check_for_update_at(&path, "0.1.2", false, now, || {
        fetches.fetch_add(1, Ordering::SeqCst);
        Ok(release_json("0.1.3"))
    })
    .unwrap();
    assert_eq!(first.status, "update_available");
    assert!(first.checked_now);

    let cached = check_for_update_at(&path, "0.1.2", false, now + Duration::hours(23), || {
        fetches.fetch_add(1, Ordering::SeqCst);
        Ok(release_json("9.9.9"))
    })
    .unwrap();
    assert_eq!(cached.latest_version.as_deref(), Some("0.1.3"));
    assert!(!cached.checked_now);
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    let forced = check_for_update_at(&path, "0.1.2", true, now + Duration::hours(23), || {
        fetches.fetch_add(1, Ordering::SeqCst);
        Ok(release_json("0.1.4"))
    })
    .unwrap();
    assert_eq!(forced.latest_version.as_deref(), Some("0.1.4"));
    assert!(forced.checked_now);
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
}

#[test]
fn notification_claim_is_persisted_once_per_version() {
    let path = temp_path("notification");
    assert!(claim_notification_at(&path, "0.1.3").unwrap());
    assert!(!claim_notification_at(&path, "0.1.3").unwrap());
    assert!(claim_notification_at(&path, "0.1.4").unwrap());
    assert_eq!(
        load_state_from(&path).last_notified_version.as_deref(),
        Some("0.1.4")
    );
}

#[test]
fn malformed_or_future_state_does_not_suppress_a_check() {
    let path = temp_path("state");
    fs::write(&path, b"not json").unwrap();
    assert_eq!(load_state_from(&path), UpdateState::default());

    let now = Utc.with_ymd_and_hms(2026, 7, 10, 3, 0, 0).unwrap();
    fs::write(
        &path,
        serde_json::to_vec(&UpdateState {
            last_checked_at: Some((now + Duration::days(1)).to_rfc3339()),
            ..UpdateState::default()
        })
        .unwrap(),
    )
    .unwrap();
    let result =
        check_for_update_at(&path, "0.1.2", false, now, || Ok(release_json("0.1.2"))).unwrap();
    assert!(result.checked_now);
    assert_eq!(result.status, "current");
}
