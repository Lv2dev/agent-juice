use agent_juice::update::{
    is_release_url_allowed, is_update_available, is_updater_asset_url_allowed, load_state_from,
    prepare_notification_at, record_update_check_at, release_info_for_version,
    update_check_is_due_at, update_package_size_is_allowed, updater_asset_url_for_version,
    UpdateState, MAX_UPDATE_PACKAGE_BYTES,
};
use chrono::{Duration, TimeZone, Utc};
use std::{
    fs,
    path::PathBuf,
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

#[test]
fn release_info_accepts_only_stable_three_part_semver() {
    let release = release_info_for_version("v0.1.3").unwrap();
    assert_eq!(release.version, "0.1.3");
    assert_eq!(
        release.url,
        "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3"
    );

    assert!(release_info_for_version("0.1.3-beta.1").is_err());
    assert!(release_info_for_version("0.1").is_err());
    assert!(release_info_for_version("0.01.3").is_err());
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
fn updater_asset_url_is_bound_to_the_exact_repository_version_and_filename() {
    let expected =
        "https://github.com/Lv2dev/agent-juice/releases/download/v0.1.11/Juice_0.1.11_x64-setup.exe";
    assert_eq!(updater_asset_url_for_version("0.1.11").unwrap(), expected);
    assert!(is_updater_asset_url_allowed(expected, "0.1.11"));
    assert!(!is_updater_asset_url_allowed(
        "https://example.com/Juice_0.1.11_x64-setup.exe",
        "0.1.11"
    ));
    assert!(!is_updater_asset_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases/download/v0.1.10/Juice_0.1.10_x64-setup.exe",
        "0.1.11"
    ));
    assert!(!is_updater_asset_url_allowed(
        "https://github.com/Lv2dev/agent-juice/releases/download/v0.1.11/Juice_0.1.10_x64-setup.exe",
        "0.1.11"
    ));
}

#[test]
fn updater_download_limit_rejects_declared_or_streamed_oversize_packages() {
    assert!(update_package_size_is_allowed(
        MAX_UPDATE_PACKAGE_BYTES,
        Some(MAX_UPDATE_PACKAGE_BYTES)
    ));
    assert!(!update_package_size_is_allowed(
        1,
        Some(MAX_UPDATE_PACKAGE_BYTES + 1)
    ));
    assert!(!update_package_size_is_allowed(
        MAX_UPDATE_PACKAGE_BYTES + 1,
        None
    ));
}

#[cfg(windows)]
#[test]
fn verified_installer_rejects_non_executable_bytes_before_writing() {
    assert!(
        agent_juice::update::prepare_verified_installer(b"not an executable", "0.1.11").is_err()
    );
}

#[test]
fn automatic_checks_throttle_for_24_hours_and_manual_checks_bypass_cache() {
    let path = temp_path("throttle");
    let now = Utc.with_ymd_and_hms(2026, 7, 10, 3, 0, 0).unwrap();
    assert!(update_check_is_due_at(&path, false, now));
    let first = record_update_check_at(&path, "0.1.2", Some("0.1.3"), now).unwrap();
    assert_eq!(first.status, "update_available");
    assert!(first.checked_now);

    assert!(!update_check_is_due_at(
        &path,
        false,
        now + Duration::hours(23)
    ));
    assert!(update_check_is_due_at(
        &path,
        true,
        now + Duration::hours(23)
    ));
    let forced =
        record_update_check_at(&path, "0.1.2", Some("0.1.4"), now + Duration::hours(23)).unwrap();
    assert_eq!(forced.latest_version.as_deref(), Some("0.1.4"));
    assert!(forced.checked_now);
}

#[test]
fn successful_current_check_replaces_a_stale_available_release() {
    let path = temp_path("current");
    let now = Utc.with_ymd_and_hms(2026, 7, 10, 3, 0, 0).unwrap();
    record_update_check_at(&path, "0.1.2", Some("0.1.3"), now).unwrap();

    let current = record_update_check_at(&path, "0.1.3", None, now + Duration::hours(24)).unwrap();
    assert_eq!(current.status, "current");
    assert_eq!(current.latest_version.as_deref(), Some("0.1.3"));
    assert_eq!(
        current.release_url.as_deref(),
        Some("https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.3")
    );
}

#[test]
fn notification_is_persisted_only_after_successful_commit() {
    let path = temp_path("notification");
    assert_eq!(load_state_from(&path).last_notified_version, None);

    let failed_display = prepare_notification_at(&path, "0.1.3").unwrap().unwrap();
    assert!(prepare_notification_at(&path, "0.1.3").unwrap().is_none());
    drop(failed_display);

    let successful_display = prepare_notification_at(&path, "0.1.3").unwrap().unwrap();
    assert!(successful_display.commit().unwrap());
    assert!(prepare_notification_at(&path, "0.1.3").unwrap().is_none());
    assert!(prepare_notification_at(&path, "0.1.2").unwrap().is_none());
    let newer_display = prepare_notification_at(&path, "0.1.4").unwrap().unwrap();
    assert!(newer_display.commit().unwrap());
    assert_eq!(
        load_state_from(&path).last_notified_version.as_deref(),
        Some("0.1.4")
    );
}

#[test]
fn update_state_temp_file_is_synced_before_atomic_replace() {
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/update.rs"))
            .unwrap();
    let state_save = &source[source.find("fn save_state_to_with").unwrap()..];
    let write = state_save.find("file.write_all(&contents)").unwrap();
    let sync = state_save.find("file.sync_all()").unwrap();
    let replace = state_save.find("match replace(path, &temp)").unwrap();

    assert!(write < sync);
    assert!(sync < replace);
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
    assert!(update_check_is_due_at(&path, false, now));
}
