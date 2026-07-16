use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_juice::{
    config::{Settings, SettingsInput, TaskbarTextColors, ToolColors},
    render::Palette,
};
use serde_json::Value;

fn temp_root(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("agent-juice-{name}-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn storage_revision_reads_only_file_metadata() {
    let root = temp_root("storage-revision");
    let path = root.join("settings.json");

    assert_eq!(Settings::storage_revision_at(&path), None);
    fs::write(&path, b"{}").unwrap();
    let first = Settings::storage_revision_at(&path).unwrap();
    assert_eq!(first.0, 2);

    fs::write(&path, b"{\"poll_interval_secs\":60}").unwrap();
    let second = Settings::storage_revision_at(&path).unwrap();
    assert_eq!(second.0, 25);
    assert!(second.1 >= first.1);
    assert_eq!(Settings::storage_revision_at(&root), None);
}

#[test]
fn settings_update_rejects_malformed_json_and_preserves_bytes() {
    let root = temp_root("update-malformed");
    let path = root.join("settings.json");
    let original = b"{\"poll_interval_secs\": 60";
    fs::write(&path, original).unwrap();

    assert_eq!(Settings::load_from(&path).poll_interval_secs, 60);
    assert!(Settings::try_load_from(&path).is_err());
    assert!(Settings::update_at(&path, |settings| settings.poll_interval_secs = 5).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn settings_update_rejects_non_object_json_and_preserves_bytes() {
    for (name, original) in [
        ("array", b"[1, 2, 3]".as_slice()),
        ("null", b"null".as_slice()),
    ] {
        let root = temp_root(&format!("update-{name}"));
        let path = root.join("settings.json");
        fs::write(&path, original).unwrap();

        assert!(Settings::update_at(&path, |settings| settings.poll_interval_secs = 5).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn settings_update_rejects_read_failure_without_replacing_the_path() {
    let root = temp_root("update-read-failure");
    let path = root.join("settings.json");
    fs::create_dir_all(&path).unwrap();

    assert!(Settings::try_load_from(&path).is_err());
    assert!(Settings::update_at(&path, |settings| settings.poll_interval_secs = 5).is_err());
    assert!(path.is_dir());
}

#[test]
fn strict_settings_load_uses_defaults_only_for_a_missing_file() {
    let root = temp_root("strict-load");
    let missing = root.join("missing.json");
    let loaded = Settings::try_load_from(&missing).unwrap();
    assert_eq!(
        serde_json::to_value(loaded).unwrap(),
        serde_json::to_value(Settings::default()).unwrap()
    );

    let malformed = root.join("malformed.json");
    fs::write(&malformed, b"{").unwrap();
    assert!(Settings::try_load_from(&malformed).is_err());

    let non_object = root.join("non-object.json");
    fs::write(&non_object, b"[]").unwrap();
    assert!(Settings::try_load_from(&non_object).is_err());
}

#[test]
fn settings_update_uses_defaults_only_when_the_file_is_missing() {
    let root = temp_root("update-missing");
    let path = root.join("settings.json");

    let updated = Settings::update_at(&path, |settings| settings.poll_interval_secs = 5).unwrap();

    assert_eq!(updated.poll_interval_secs, 5);
    assert_eq!(updated.taskbar_offset_ratio, 0.0);
    assert_eq!(updated.claude_taskbar_offset_ratio, 0.0);
    assert_eq!(updated.codex_taskbar_offset_ratio, 0.0);
    assert!(!updated.claude_taskbar_target_initialized);
    assert!(!updated.codex_taskbar_target_initialized);
    assert!(!updated.fullscreen_hide_on);
    assert_eq!(read_json(&path)["poll_interval_secs"], 5);
}

#[test]
fn existing_settings_without_taskbar_offsets_keep_the_legacy_center_default() {
    let root = temp_root("legacy-taskbar-default");
    let path = root.join("settings.json");
    fs::write(&path, "{}").unwrap();

    let loaded = Settings::load_from(&path);

    assert_eq!(loaded.taskbar_offset_ratio, 0.5);
    assert_eq!(loaded.claude_taskbar_offset_ratio, 0.5);
    assert_eq!(loaded.codex_taskbar_offset_ratio, 0.5);
    assert!(loaded.claude_taskbar_target_initialized);
    assert!(loaded.codex_taskbar_target_initialized);
    assert!(loaded.fullscreen_hide_on);
}

#[test]
fn legacy_explicit_zero_taskbar_position_is_not_treated_as_a_first_run() {
    let root = temp_root("legacy-explicit-zero-taskbar");
    let path = root.join("settings.json");
    fs::write(&path, r#"{"taskbar_offset_ratio":0}"#).unwrap();

    let loaded = Settings::load_from(&path);

    assert_eq!(loaded.claude_taskbar_offset_ratio, 0.0);
    assert_eq!(loaded.codex_taskbar_offset_ratio, 0.0);
    assert!(loaded.claude_taskbar_target_initialized);
    assert!(loaded.codex_taskbar_target_initialized);
}

#[test]
fn settings_update_migrates_and_clamps_valid_settings_before_mutation() {
    let root = temp_root("update-valid");
    let path = root.join("settings.json");
    fs::write(
        &path,
        r#"{"taskbar_offset_ratio":1.25,"poll_interval_secs":2,"ring_size_px":34.5,"ring_thickness_px":6.5,"ring_gap_px":8.5,"ring_center_gap_px":2.5}"#,
    )
    .unwrap();

    let updated = Settings::update_at(&path, |settings| settings.show_codex = false).unwrap();

    assert_eq!(updated.taskbar_offset_ratio, 1.0);
    assert_eq!(updated.claude_taskbar_offset_ratio, 1.0);
    assert_eq!(updated.codex_taskbar_offset_ratio, 1.0);
    assert_eq!(updated.poll_interval_secs, 60);
    assert_eq!(updated.ring_size_px, 34.5);
    assert_eq!(updated.ring_thickness_px, 6.5);
    assert_eq!(updated.ring_gap_px, 8.5);
    assert_eq!(updated.ring_center_size_px, 9.5);
    assert!(updated.indicator_track_color_auto);
    assert_eq!(updated.indicator_track_color, [0x6b, 0x72, 0x80]);
    assert_eq!(updated.indicator_track_opacity_percent, 11.0);
    assert!(!updated.show_codex);

    let saved = read_json(&path);
    assert_eq!(saved["poll_interval_secs"], 60);
    assert_eq!(saved["ring_center_size_px"], 9.5);
    assert!(saved.get("ring_center_gap_px").is_none());
    assert_eq!(saved["show_codex"], false);
}

#[test]
fn settings_roundtrip_and_legacy_defaults() {
    let root = temp_root("settings");
    let path = root.join("settings.json");
    let settings = Settings {
        palette: Palette::Cool,
        tool_colors: ToolColors {
            claude_primary: [0x11, 0x22, 0x33],
            claude_secondary: [0x44, 0x55, 0x66],
            codex_primary: [0x77, 0x88, 0x99],
            codex_secondary: [0xaa, 0xbb, 0xcc],
            warning: [0xde, 0xad, 0x01],
            danger: [0xbe, 0xef, 0x02],
            warning_on: false,
            danger_on: true,
        },
        taskbar_text_colors: TaskbarTextColors {
            claude: [0x12, 0x34, 0x56],
            claude_on: true,
            codex: [0x23, 0x45, 0x67],
            codex_on: false,
            info: [0x34, 0x56, 0x78],
            info_on: true,
            ring: [0x45, 0x67, 0x89],
            ring_on: true,
        },
        warn_threshold: 65.0,
        danger_threshold: 85.0,
        display_basis: "used".into(),
        poll_interval_secs: 3,
        stale_after_secs: 120,
        bar_mode: "quad".into(),
        full_reset_time_on: true,
        limit_order: "secondary_first".into(),
        fullscreen_hide_on: false,
        maximized_hide_on: true,
        indicator_style: "bar".into(),
        indicator_effect_style: "glow".into(),
        indicator_track_color_auto: false,
        indicator_track_color: [0x12, 0x34, 0x56],
        indicator_track_opacity_percent: 37.5,
        ring_on: false,
        ring_numbers_on: false,
        ring_number_outline_on: true,
        ring_number_outline_width_px: 1.4,
        ring_size_px: 34.5,
        ring_thickness_px: 6.5,
        ring_gap_px: 8.5,
        ring_center_size_px: 18.5,
        ring_number_font_size_px: 10.5,
        ring_number_font_weight: 650,
        bar_text_font_size_px: 12.5,
        bar_text_font_weight: 550,
        bar_content_gap_px: 3.5,
        autostart_on: false,
        update_check_on: false,
        language: "en".into(),
        theme: "light".into(),
        font_mode: "pretendard".into(),
        taskbar_offset_ratio: 0.25,
        claude_taskbar_offset_ratio: 0.15,
        codex_taskbar_offset_ratio: 0.85,
        claude_taskbar_monitor_key: "monitor:0,0,1920,1080".into(),
        codex_taskbar_monitor_key: "monitor:1920,0,2560,1440".into(),
        claude_taskbar_target_initialized: true,
        codex_taskbar_target_initialized: true,
        show_claude: false,
        show_codex: true,
        claude_account_auto_collect_on: true,
    };

    settings.save_to(&path).unwrap();
    let loaded = Settings::load_from(&path);

    assert!(matches!(loaded.palette, Palette::Cool));
    assert_eq!(loaded.tool_colors, settings.tool_colors);
    assert_eq!(loaded.taskbar_text_colors, settings.taskbar_text_colors);
    assert_eq!(loaded.warn_threshold, 65.0);
    assert_eq!(loaded.danger_threshold, 85.0);
    assert_eq!(loaded.display_basis, "used");
    assert_eq!(loaded.poll_interval_secs, 3);
    assert_eq!(loaded.stale_after_secs, 120);
    assert_eq!(loaded.bar_mode, "quad");
    assert!(loaded.full_reset_time_on);
    assert_eq!(loaded.limit_order, "secondary_first");
    assert!(!loaded.fullscreen_hide_on);
    assert!(loaded.maximized_hide_on);
    assert_eq!(loaded.indicator_style, "bar");
    assert_eq!(loaded.indicator_effect_style, "glow");
    assert!(!loaded.indicator_track_color_auto);
    assert_eq!(loaded.indicator_track_color, [0x12, 0x34, 0x56]);
    assert_eq!(loaded.indicator_track_opacity_percent, 37.5);
    assert!(!loaded.ring_on);
    assert!(!loaded.ring_numbers_on);
    assert!(loaded.ring_number_outline_on);
    assert_eq!(loaded.ring_number_outline_width_px, 1.4);
    assert_eq!(loaded.ring_size_px, 34.5);
    assert_eq!(loaded.ring_thickness_px, 6.5);
    assert_eq!(loaded.ring_gap_px, 8.5);
    assert_eq!(loaded.ring_center_size_px, 18.5);
    assert_eq!(loaded.ring_number_font_size_px, 10.5);
    assert_eq!(loaded.ring_number_font_weight, 650);
    assert_eq!(loaded.bar_text_font_size_px, 12.5);
    assert_eq!(loaded.bar_text_font_weight, 550);
    assert_eq!(loaded.bar_content_gap_px, 3.5);
    assert!(!loaded.autostart_on);
    assert!(!loaded.update_check_on);
    assert_eq!(loaded.language, "en");
    assert_eq!(loaded.theme, "light");
    assert_eq!(loaded.font_mode, "pretendard");
    assert_eq!(loaded.taskbar_offset_ratio, 0.5);
    assert_eq!(loaded.claude_taskbar_offset_ratio, 0.15);
    assert_eq!(loaded.codex_taskbar_offset_ratio, 0.85);
    assert_eq!(loaded.claude_taskbar_monitor_key, "monitor:0,0,1920,1080");
    assert_eq!(loaded.codex_taskbar_monitor_key, "monitor:1920,0,2560,1440");
    assert!(loaded.claude_taskbar_target_initialized);
    assert!(loaded.codex_taskbar_target_initialized);
    assert!(!loaded.show_claude);
    assert!(loaded.show_codex);
    assert!(loaded.claude_account_auto_collect_on);

    fs::write(
        &path,
        r#"{"palette":"Cvd","warn_threshold":60.0,"danger_threshold":92.0,"poll_interval_secs":2,"taskbar_offset_ratio":0.3,"tool_gap_px":44}"#,
    )
    .unwrap();
    let legacy = Settings::load_from(&path);

    assert!(matches!(legacy.palette, Palette::Cvd));
    assert_eq!(legacy.display_basis, "remaining");
    assert_eq!(legacy.poll_interval_secs, 60);
    assert_eq!(legacy.stale_after_secs, 90);
    assert_eq!(legacy.bar_mode, "full");
    assert!(legacy.full_reset_time_on);
    assert_eq!(legacy.limit_order, "primary_first");
    assert!(legacy.fullscreen_hide_on);
    assert!(!legacy.maximized_hide_on);
    assert_eq!(legacy.indicator_style, "ring");
    assert_eq!(legacy.indicator_effect_style, "flat");
    assert!(legacy.ring_on);
    assert!(legacy.ring_numbers_on);
    assert!(legacy.ring_number_outline_on);
    assert_eq!(legacy.ring_number_outline_width_px, 1.2);
    assert_eq!(legacy.ring_size_px, 36.0);
    assert_eq!(legacy.ring_thickness_px, 4.0);
    assert_eq!(legacy.ring_gap_px, 6.0);
    assert_eq!(legacy.ring_center_size_px, 16.0);
    assert!(legacy.update_check_on);
    assert_eq!(legacy.ring_number_font_size_px, 9.0);
    assert_eq!(legacy.ring_number_font_weight, 600);
    assert_eq!(legacy.bar_text_font_size_px, 11.0);
    assert_eq!(legacy.bar_text_font_weight, 500);
    assert_eq!(legacy.bar_content_gap_px, 14.0);
    assert!(legacy.autostart_on);
    assert_eq!(legacy.language, "system");
    assert_eq!(legacy.theme, "system");
    assert_eq!(legacy.font_mode, "system");
    assert_eq!(legacy.taskbar_offset_ratio, 0.3);
    assert_eq!(legacy.claude_taskbar_offset_ratio, 0.3);
    assert_eq!(legacy.codex_taskbar_offset_ratio, 0.3);
    assert_eq!(legacy.claude_taskbar_monitor_key, "");
    assert_eq!(legacy.codex_taskbar_monitor_key, "");
    assert!(legacy.claude_taskbar_target_initialized);
    assert!(legacy.codex_taskbar_target_initialized);
    assert!(legacy.show_claude);
    assert!(legacy.show_codex);
    assert!(legacy.claude_account_auto_collect_on);
    assert_eq!(legacy.tool_colors, ToolColors::default());

    fs::write(&path, r#"{"full_reset_time_on":false}"#).unwrap();
    let explicit_reset_off = Settings::load_from(&path);
    assert!(!explicit_reset_off.full_reset_time_on);

    fs::write(&path, r#"{"claude_usage_auto_refresh_lab_on":false}"#).unwrap();
    let migrated_claude_collection = Settings::load_from(&path);
    assert!(!migrated_claude_collection.claude_account_auto_collect_on);
    migrated_claude_collection.save_to(&path).unwrap();
    let migrated_json = read_json(&path);
    assert_eq!(migrated_json["claude_account_auto_collect_on"], false);
    assert!(migrated_json
        .get("claude_usage_auto_refresh_lab_on")
        .is_none());

    fs::write(&path, r#"{"display_basis":"used","poll_interval_secs":2}"#).unwrap();
    let explicit_modern_interval = Settings::load_from(&path);
    assert_eq!(explicit_modern_interval.display_basis, "used");
    assert_eq!(explicit_modern_interval.poll_interval_secs, 2);
}

#[test]
fn legacy_center_gap_migrates_to_the_visible_center_diameter() {
    let root = temp_root("legacy-ring-center");
    let path = root.join("settings.json");
    fs::write(
        &path,
        r#"{"ring_size_px":34.5,"ring_thickness_px":6.5,"ring_gap_px":8.5,"ring_center_gap_px":2.5}"#,
    )
    .unwrap();

    let migrated = Settings::load_from(&path);
    assert_eq!(migrated.ring_center_size_px, 9.5);
    migrated.save_to(&path).unwrap();
    let saved = read_json(&path);
    assert_eq!(saved["ring_center_size_px"], 9.5);
    assert!(saved.get("ring_center_gap_px").is_none());
}

#[test]
fn legacy_default_tool_colors_migrate_without_overwriting_custom_combinations() {
    let root = temp_root("legacy-tool-colors");
    let path = root.join("settings.json");
    fs::write(
        &path,
        r#"{"tool_colors":{"claude_primary":[183,131,58],"claude_secondary":[166,95,114],"codex_primary":[79,138,115],"codex_secondary":[79,118,166]}}"#,
    )
    .unwrap();

    let migrated = Settings::load_from(&path);
    assert_eq!(migrated.tool_colors, ToolColors::default());

    let custom = ToolColors {
        claude_primary: [0xb7, 0x83, 0x3a],
        claude_secondary: [0xa6, 0x5f, 0x72],
        codex_primary: [0x12, 0x34, 0x56],
        codex_secondary: [0x4f, 0x76, 0xa6],
        ..ToolColors::default()
    };
    fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({ "tool_colors": custom })).unwrap(),
    )
    .unwrap();

    assert_eq!(Settings::load_from(&path).tool_colors, custom);
}

#[test]
fn install_statusline_wrap_preserves_original_and_is_idempotent() {
    let root = temp_root("install");
    let home = root.join("home");
    let data_dir = root.join("data");
    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    fs::create_dir_all(&claude_dir).unwrap();
    let original_settings = r#"{"statusLine":{"type":"command","command":"claude-hud --theme dark","padding":2,"nested":{"keep":true}},"keep":true}"#;
    fs::write(&settings_path, original_settings).unwrap();

    Settings::install_statusline_wrap_at(
        &home,
        &data_dir,
        r"C:\Program Files\Juice\agentjuice-statusline.exe",
    )
    .unwrap();

    let installed = read_json(&settings_path);
    assert_eq!(installed["keep"], true);
    assert_eq!(
        installed["statusLine"]["command"],
        "\"C:/Program Files/Juice/agentjuice-statusline.exe\""
    );
    assert_eq!(
        fs::read_to_string(data_dir.join("wrap.json")).unwrap(),
        "claude-hud --theme dark"
    );
    let metadata = read_json(&data_dir.join("wrap-meta.json"));
    assert_eq!(metadata["version"], 2);
    assert_eq!(metadata["original_status_line_present"], true);
    assert_eq!(metadata["original_status_line"]["padding"], 2);
    assert_eq!(metadata["original_status_line"]["nested"]["keep"], true);
    let backup_path = settings_path.with_extension("json.aj-backup");
    assert_eq!(fs::read_to_string(&backup_path).unwrap(), original_settings);

    Settings::install_statusline_wrap_at(&home, &data_dir, r"D:\Other\agentjuice-statusline.exe")
        .unwrap();

    let second = read_json(&settings_path);
    assert_eq!(
        second["statusLine"]["command"],
        "\"D:/Other/agentjuice-statusline.exe\""
    );
    assert_eq!(
        fs::read_to_string(data_dir.join("wrap.json")).unwrap(),
        "claude-hud --theme dark"
    );
    assert_eq!(
        read_json(&data_dir.join("wrap-meta.json"))["original_status_line"]["padding"],
        2
    );
    assert_eq!(fs::read_to_string(&backup_path).unwrap(), original_settings);
}

#[test]
fn restore_statusline_restores_entire_original_subtree_or_absence() {
    let root = temp_root("restore");
    let home = root.join("home");
    let data_dir = root.join("data");
    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"claude-hud --theme dark","padding":2,"nested":{"keep":true}},"keep":true}"#,
    )
    .unwrap();

    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();
    Settings::restore_statusline_at(&home, &data_dir).unwrap();

    let restored = read_json(&settings_path);
    assert_eq!(restored["keep"], true);
    assert_eq!(restored["statusLine"]["command"], "claude-hud --theme dark");
    assert_eq!(restored["statusLine"]["padding"], 2);
    assert_eq!(restored["statusLine"]["nested"]["keep"], true);
    assert!(!data_dir.join("wrap-meta.json").exists());
    assert!(!data_dir.join("wrap.json").exists());

    fs::write(&settings_path, r#"{"keep":true}"#).unwrap();
    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();
    Settings::restore_statusline_at(&home, &data_dir).unwrap();

    let removed = read_json(&settings_path);
    assert_eq!(removed["keep"], true);
    assert!(removed.get("statusLine").is_none());

    fs::write(&settings_path, r#"{"statusLine":null,"keep":true}"#).unwrap();
    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();
    Settings::restore_statusline_at(&home, &data_dir).unwrap();
    let restored_null = read_json(&settings_path);
    assert!(restored_null.get("statusLine").is_some());
    assert!(restored_null["statusLine"].is_null());
}

#[test]
fn restore_statusline_decodes_legacy_metadata() {
    let root = temp_root("restore-legacy");
    let home = root.join("home");
    let data_dir = root.join("data");
    let settings_path = home.join(".claude").join("settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("wrap-meta.json"),
        r#"{"managed_command":"\"C:/Juice/agentjuice-statusline.exe\"","original_command":"claude-hud --theme dark"}"#,
    )
    .unwrap();
    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"\"C:/Juice/agentjuice-statusline.exe\""},"keep":true}"#,
    )
    .unwrap();

    Settings::restore_statusline_at(&home, &data_dir).unwrap();

    let restored = read_json(&settings_path);
    assert_eq!(restored["keep"], true);
    assert_eq!(restored["statusLine"]["command"], "claude-hud --theme dark");

    fs::write(
        data_dir.join("wrap-meta.json"),
        r#"{"managed_command":"\"C:/Juice/agentjuice-statusline.exe\"","original_command":null}"#,
    )
    .unwrap();
    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"\"C:/Juice/agentjuice-statusline.exe\""},"keep":true}"#,
    )
    .unwrap();
    Settings::restore_statusline_at(&home, &data_dir).unwrap();
    assert!(read_json(&settings_path).get("statusLine").is_none());
}

#[test]
fn restore_statusline_rejects_incomplete_v2_metadata_without_mutation() {
    for (name, metadata) in [
        (
            "missing-managed",
            r#"{"version":2,"managed_command":"managed","original_status_line_present":false}"#,
        ),
        (
            "missing-presence",
            r#"{"version":2,"managed_command":"managed","managed_status_line":{"type":"command","command":"managed"}}"#,
        ),
        (
            "missing-original",
            r#"{"version":2,"managed_command":"managed","managed_status_line":{"type":"command","command":"managed"},"original_status_line_present":true}"#,
        ),
    ] {
        let root = temp_root(name);
        let home = root.join("home");
        let data_dir = root.join("data");
        let settings_path = home.join(".claude").join("settings.json");
        let meta_path = data_dir.join("wrap-meta.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let settings = r#"{"statusLine":{"type":"command","command":"managed"},"keep":true}"#;
        fs::write(&settings_path, settings).unwrap();
        fs::write(&meta_path, metadata).unwrap();

        assert!(Settings::restore_statusline_at(&home, &data_dir).is_err());
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), settings);
        assert_eq!(fs::read_to_string(&meta_path).unwrap(), metadata);
    }
}

#[test]
fn nsis_uninstall_hooks_restore_before_removal_and_delete_canonical_data_dir() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = read_json(&manifest_dir.join("tauri.conf.json"));
    assert_eq!(
        config["bundle"]["windows"]["nsis"]["installerHooks"],
        "./windows/hooks.nsh"
    );

    let hooks = fs::read_to_string(manifest_dir.join("windows/hooks.nsh")).unwrap();
    assert!(hooks.contains("NSIS_HOOK_PREUNINSTALL"));
    assert!(hooks.contains("--restore-owned-statusline"));
    assert!(hooks.contains(
        "IfFileExists \"$INSTDIR\\agentjuice-statusline.exe\" restore_owned_statusline restore_owned_statusline_missing_bridge"
    ));
    assert!(hooks.contains(
        "IfFileExists \"$LOCALAPPDATA\\agent-juice\\wrap-meta.json\" restore_owned_statusline_repair_required restore_owned_statusline_done"
    ));
    assert!(hooks.contains("Repair or reinstall Juice before uninstalling"));
    assert!(hooks.contains("StrCpy $0 1"));
    assert!(hooks.contains("${If} $0 <> 0"));
    assert!(hooks.contains("MessageBox MB_OK|MB_ICONSTOP"));
    assert!(hooks.contains("/SD IDOK"));
    assert!(hooks.contains("Abort"));
    assert!(hooks.contains("NSIS_HOOK_POSTUNINSTALL"));
    assert!(hooks.contains("$DeleteAppDataCheckboxState = 1"));
    assert!(hooks.contains(r#"RmDir /r "$LOCALAPPDATA\agent-juice""#));
    assert!(hooks.matches("$UpdateMode <> 1").count() >= 2);

    let preuninstall = hooks
        .split("!macro NSIS_HOOK_POSTUNINSTALL")
        .next()
        .unwrap();
    let update_guard = preuninstall.find("$UpdateMode <> 1").unwrap();
    let repair_required = preuninstall
        .find("restore_owned_statusline_repair_required:")
        .unwrap();
    let repair_abort = preuninstall[repair_required..].find("Abort").unwrap() + repair_required;
    let restore = preuninstall.find("--restore-owned-statusline").unwrap();
    let failure_gate = preuninstall.find("${If} $0 <> 0").unwrap();
    let restore_abort = preuninstall.rfind("Abort").unwrap();
    let guard_end = preuninstall.rfind("${EndIf}").unwrap();
    assert!(update_guard < repair_required);
    assert!(repair_required < repair_abort);
    assert!(repair_abort < restore);
    assert!(update_guard < restore);
    assert!(restore < failure_gate);
    assert!(failure_gate < restore_abort);
    assert!(restore_abort < guard_end);
}

#[test]
fn restore_statusline_requires_exact_managed_subtree() {
    let root = temp_root("restore-guard");
    let home = root.join("home");
    let data_dir = root.join("data");
    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    assert!(Settings::restore_statusline_at(&home, &data_dir).is_err());

    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"old"},"keep":true}"#,
    )
    .unwrap();
    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();
    let mut changed = read_json(&settings_path);
    changed["statusLine"]["userChanged"] = serde_json::json!(true);
    fs::write(&settings_path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();

    assert!(Settings::restore_statusline_at(&home, &data_dir).is_err());
    assert_eq!(read_json(&settings_path), changed);
    assert!(data_dir.join("wrap-meta.json").exists());
}

#[test]
fn external_claude_settings_fail_closed_on_invalid_or_unreadable_roots() {
    for (name, contents) in [
        ("malformed", "{"),
        ("array", "[]"),
        ("string", r#""value""#),
    ] {
        let root = temp_root(name);
        let home = root.join("home");
        let data_dir = root.join("data");
        let settings_path = home.join(".claude").join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        fs::write(&settings_path, contents).unwrap();

        assert!(Settings::install_statusline_wrap_at(
            &home,
            &data_dir,
            r"C:\Juice\agentjuice-statusline.exe"
        )
        .is_err());
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), contents);
        assert!(!data_dir.exists());
    }

    let root = temp_root("read-failure");
    let home = root.join("home");
    let data_dir = root.join("data");
    let settings_path = home.join(".claude").join("settings.json");
    fs::create_dir_all(&settings_path).unwrap();
    assert!(Settings::install_statusline_wrap_at(
        &home,
        &data_dir,
        r"C:\Juice\agentjuice-statusline.exe"
    )
    .is_err());
    assert!(settings_path.is_dir());
    assert!(!data_dir.exists());
}

#[test]
fn restore_statusline_fails_closed_when_settings_becomes_malformed() {
    let root = temp_root("restore-malformed");
    let home = root.join("home");
    let data_dir = root.join("data");
    let settings_path = home.join(".claude").join("settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(&settings_path, r#"{"statusLine":{"command":"old"}}"#).unwrap();
    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();
    fs::write(&settings_path, "{").unwrap();

    assert!(Settings::restore_statusline_at(&home, &data_dir).is_err());
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), "{");
    assert!(data_dir.join("wrap-meta.json").exists());
}

#[test]
fn settings_input_normalizes_task10_fields_and_custom_palette() {
    let input = SettingsInput {
        palette: "custom".into(),
        warn_threshold: 72.0,
        danger_threshold: 91.0,
        display_basis: "used".into(),
        poll_interval_secs: 5,
        stale_after_secs: 80,
        bar_mode: "quad".into(),
        full_reset_time_on: true,
        limit_order: "secondary_first".into(),
        fullscreen_hide_on: false,
        maximized_hide_on: true,
        indicator_style: "bar".into(),
        indicator_effect_style: "depth".into(),
        indicator_track_color_auto: false,
        indicator_track_color: Some("#123456".into()),
        indicator_track_opacity_percent: 37.5,
        ring_on: false,
        ring_numbers_on: false,
        ring_number_outline_on: true,
        ring_number_outline_width_px: 1.4,
        ring_size_px: 34.5,
        ring_thickness_px: 6.5,
        ring_gap_px: 8.5,
        ring_center_size_px: 18.5,
        ring_number_font_size_px: 10.5,
        ring_number_font_weight: 650,
        bar_text_font_size_px: 12.5,
        bar_text_font_weight: 550,
        bar_content_gap_px: 3.5,
        autostart_on: false,
        update_check_on: false,
        language: "ko".into(),
        theme: "dark".into(),
        font_mode: "pretendard".into(),
        taskbar_offset_ratio: 1.25,
        claude_taskbar_offset_ratio: -0.25,
        codex_taskbar_offset_ratio: 1.25,
        claude_taskbar_monitor_key: "monitor:0,0,1920,1080".into(),
        codex_taskbar_monitor_key: "monitor:1920,0,2560,1440".into(),
        show_claude: false,
        show_codex: true,
        claude_account_auto_collect_on: true,
        mono_color: Some("#345678".into()),
        custom_safe: Some("#112233".into()),
        custom_warn: Some("#445566".into()),
        custom_danger: Some("#778899".into()),
        claude_primary_color: Some("#102030".into()),
        claude_secondary_color: Some("#405060".into()),
        codex_primary_color: Some("#708090".into()),
        codex_secondary_color: Some("#a0b0c0".into()),
        tool_warning_color: Some("#b0c0d0".into()),
        tool_danger_color: Some("#d0c0b0".into()),
        tool_warning_color_on: false,
        tool_danger_color_on: true,
        claude_text_color: Some("#112244".into()),
        claude_text_color_on: true,
        codex_text_color: Some("#335577".into()),
        codex_text_color_on: false,
        info_text_color: Some("#446688".into()),
        info_text_color_on: true,
        ring_text_color: Some("#557799".into()),
        ring_text_color_on: true,
    };

    let settings = Settings::from_input(input);

    assert!(matches!(
        settings.palette,
        Palette::Custom([0x11, 0x22, 0x33], [0x44, 0x55, 0x66], [0x77, 0x88, 0x99])
    ));
    assert_eq!(
        settings.tool_colors,
        ToolColors {
            claude_primary: [0x10, 0x20, 0x30],
            claude_secondary: [0x40, 0x50, 0x60],
            codex_primary: [0x70, 0x80, 0x90],
            codex_secondary: [0xa0, 0xb0, 0xc0],
            warning: [0xb0, 0xc0, 0xd0],
            danger: [0xd0, 0xc0, 0xb0],
            warning_on: false,
            danger_on: true,
        }
    );
    assert_eq!(
        settings.taskbar_text_colors,
        TaskbarTextColors {
            claude: [0x11, 0x22, 0x44],
            claude_on: true,
            codex: [0x33, 0x55, 0x77],
            codex_on: false,
            info: [0x44, 0x66, 0x88],
            info_on: true,
            ring: [0x55, 0x77, 0x99],
            ring_on: true,
        }
    );
    assert_eq!(settings.warn_threshold, 72.0);
    assert_eq!(settings.danger_threshold, 91.0);
    assert_eq!(settings.display_basis, "used");
    assert_eq!(settings.poll_interval_secs, 5);
    assert_eq!(settings.stale_after_secs, 80);
    assert_eq!(settings.bar_mode, "quad");
    assert!(settings.full_reset_time_on);
    assert_eq!(settings.limit_order, "secondary_first");
    assert!(!settings.fullscreen_hide_on);
    assert!(settings.maximized_hide_on);
    assert_eq!(settings.indicator_style, "bar");
    assert_eq!(settings.indicator_effect_style, "depth");
    assert!(!settings.indicator_track_color_auto);
    assert_eq!(settings.indicator_track_color, [0x12, 0x34, 0x56]);
    assert_eq!(settings.indicator_track_opacity_percent, 37.5);
    assert!(!settings.ring_on);
    assert!(!settings.ring_numbers_on);
    assert!(settings.ring_number_outline_on);
    assert_eq!(settings.ring_number_outline_width_px, 1.4);
    assert_eq!(settings.ring_size_px, 34.5);
    assert_eq!(settings.ring_thickness_px, 6.5);
    assert_eq!(settings.ring_gap_px, 8.5);
    assert_eq!(settings.ring_center_size_px, 18.5);
    assert_eq!(settings.ring_number_font_size_px, 10.5);
    assert_eq!(settings.ring_number_font_weight, 650);
    assert_eq!(settings.bar_text_font_size_px, 12.5);
    assert_eq!(settings.bar_text_font_weight, 550);
    assert_eq!(settings.bar_content_gap_px, 3.5);
    assert!(!settings.autostart_on);
    assert!(!settings.update_check_on);
    assert_eq!(settings.language, "ko");
    assert_eq!(settings.theme, "dark");
    assert_eq!(settings.font_mode, "pretendard");
    assert_eq!(settings.taskbar_offset_ratio, 1.0);
    assert_eq!(settings.claude_taskbar_offset_ratio, 0.0);
    assert_eq!(settings.codex_taskbar_offset_ratio, 1.0);
    assert_eq!(settings.claude_taskbar_monitor_key, "monitor:0,0,1920,1080");
    assert_eq!(
        settings.codex_taskbar_monitor_key,
        "monitor:1920,0,2560,1440"
    );
    assert!(!settings.show_claude);
    assert!(settings.show_codex);
    assert!(settings.claude_account_auto_collect_on);
}

#[test]
fn legacy_settings_keep_all_taskbar_text_colors_automatic() {
    let root = temp_root("legacy-text-colors");
    let path = root.join("settings.json");
    fs::write(&path, r#"{"palette":"Traffic","show_claude":true}"#).unwrap();

    let loaded = Settings::load_from(&path);

    assert_eq!(loaded.taskbar_text_colors, TaskbarTextColors::default());
    assert!(!loaded.taskbar_text_colors.claude_on);
    assert!(!loaded.taskbar_text_colors.codex_on);
    assert!(!loaded.taskbar_text_colors.info_on);
    assert!(!loaded.taskbar_text_colors.ring_on);
}

#[test]
fn settings_input_supports_extended_and_monochrome_palettes() {
    for (name, expected) in [
        ("signal", Palette::Signal),
        ("ocean", Palette::Ocean),
        ("forest", Palette::Forest),
        ("sunset", Palette::Sunset),
    ] {
        let settings = Settings::from_input(SettingsInput {
            palette: name.into(),
            ..SettingsInput::default()
        });
        assert!(std::mem::discriminant(&settings.palette) == std::mem::discriminant(&expected));
    }

    let mono = Settings::from_input(SettingsInput {
        palette: "mono".into(),
        mono_color: Some("#345678".into()),
        ..SettingsInput::default()
    });
    assert!(matches!(mono.palette, Palette::Mono([0x34, 0x56, 0x78])));
}

#[test]
fn settings_input_defaults_theme_to_system_and_clamps_tool_taskbar_offsets() {
    let settings = Settings::from_input(SettingsInput {
        theme: "unknown".into(),
        display_basis: "unexpected".into(),
        language: "unknown".into(),
        font_mode: "unknown".into(),
        indicator_style: "unexpected".into(),
        indicator_effect_style: "unexpected".into(),
        indicator_track_opacity_percent: 999.0,
        limit_order: "unexpected".into(),
        ring_size_px: 99.0,
        ring_thickness_px: 99.0,
        ring_gap_px: 1.0,
        ring_center_size_px: 99.0,
        ring_number_outline_width_px: 99.0,
        ring_number_font_size_px: 99.0,
        ring_number_font_weight: 999,
        bar_text_font_size_px: -1.0,
        bar_text_font_weight: 999,
        bar_content_gap_px: 99.0,
        taskbar_offset_ratio: -0.5,
        claude_taskbar_offset_ratio: -0.5,
        codex_taskbar_offset_ratio: 2.0,
        ..SettingsInput::default()
    });

    assert_eq!(settings.theme, "system");
    assert_eq!(settings.display_basis, "remaining");
    assert_eq!(settings.language, "system");
    assert_eq!(settings.font_mode, "system");
    assert!(!settings.fullscreen_hide_on);
    assert!(!settings.maximized_hide_on);
    assert_eq!(settings.indicator_style, "ring");
    assert_eq!(settings.indicator_effect_style, "flat");
    assert!(settings.indicator_track_color_auto);
    assert_eq!(settings.indicator_track_color, [0x6b, 0x72, 0x80]);
    assert_eq!(settings.indicator_track_opacity_percent, 100.0);
    assert_eq!(settings.limit_order, "primary_first");
    assert!(settings.ring_numbers_on);
    assert!(settings.ring_number_outline_on);
    assert_eq!(settings.ring_size_px, 44.0);
    assert_eq!(settings.ring_thickness_px, 10.0);
    assert_eq!(settings.ring_gap_px, 2.0);
    assert_eq!(settings.ring_center_size_px, 32.0);
    assert_eq!(settings.ring_number_outline_width_px, 4.0);
    assert_eq!(settings.ring_number_font_size_px, 16.0);
    assert_eq!(settings.ring_number_font_weight, 900);
    assert_eq!(settings.bar_text_font_size_px, 8.0);
    assert_eq!(settings.bar_text_font_weight, 900);
    assert_eq!(settings.bar_content_gap_px, 24.0);
    assert_eq!(settings.taskbar_offset_ratio, 0.0);
    assert_eq!(settings.claude_taskbar_offset_ratio, 0.0);
    assert_eq!(settings.codex_taskbar_offset_ratio, 1.0);
    assert!(settings.show_claude);
    assert!(settings.show_codex);
    assert!(settings.claude_account_auto_collect_on);
}
