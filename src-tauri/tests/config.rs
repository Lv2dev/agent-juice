use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_juice::{
    config::{Settings, SettingsInput, ToolColors},
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
        },
        warn_threshold: 65.0,
        danger_threshold: 85.0,
        display_basis: "used".into(),
        poll_interval_secs: 3,
        stale_after_secs: 120,
        bar_mode: "quad".into(),
        limit_order: "secondary_first".into(),
        fullscreen_hide_on: false,
        maximized_hide_on: true,
        indicator_style: "bar".into(),
        indicator_effect_style: "glow".into(),
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
        show_claude: false,
        show_codex: true,
        claude_account_auto_collect_on: true,
    };

    settings.save_to(&path).unwrap();
    let loaded = Settings::load_from(&path);

    assert!(matches!(loaded.palette, Palette::Cool));
    assert_eq!(loaded.tool_colors, settings.tool_colors);
    assert_eq!(loaded.warn_threshold, 65.0);
    assert_eq!(loaded.danger_threshold, 85.0);
    assert_eq!(loaded.display_basis, "used");
    assert_eq!(loaded.poll_interval_secs, 3);
    assert_eq!(loaded.stale_after_secs, 120);
    assert_eq!(loaded.bar_mode, "quad");
    assert_eq!(loaded.limit_order, "secondary_first");
    assert!(!loaded.fullscreen_hide_on);
    assert!(loaded.maximized_hide_on);
    assert_eq!(loaded.indicator_style, "bar");
    assert_eq!(loaded.indicator_effect_style, "glow");
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
    assert!(legacy.autostart_on);
    assert_eq!(legacy.language, "system");
    assert_eq!(legacy.theme, "system");
    assert_eq!(legacy.font_mode, "system");
    assert_eq!(legacy.taskbar_offset_ratio, 0.3);
    assert_eq!(legacy.claude_taskbar_offset_ratio, 0.3);
    assert_eq!(legacy.codex_taskbar_offset_ratio, 0.3);
    assert_eq!(legacy.claude_taskbar_monitor_key, "");
    assert_eq!(legacy.codex_taskbar_monitor_key, "");
    assert!(legacy.show_claude);
    assert!(legacy.show_codex);
    assert!(legacy.claude_account_auto_collect_on);
    assert_eq!(legacy.tool_colors, ToolColors::default());

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
    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"claude-hud --theme dark"},"keep":true}"#,
    )
    .unwrap();

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
    assert!(settings_path.with_extension("json.aj-backup").exists());

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
}

#[test]
fn restore_statusline_restores_only_statusline() {
    let root = temp_root("restore");
    let home = root.join("home");
    let data_dir = root.join("data");
    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("wrap.json"), "claude-hud --theme dark").unwrap();
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

    let removed = read_json(&settings_path);
    assert_eq!(removed["keep"], true);
    assert!(removed.get("statusLine").is_none());
}

#[test]
fn restore_statusline_refuses_missing_metadata_or_unmanaged_current_command() {
    let root = temp_root("restore-guard");
    let home = root.join("home");
    let data_dir = root.join("data");
    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"claude-hud --theme dark"},"keep":true}"#,
    )
    .unwrap();

    assert!(Settings::restore_statusline_at(&home, &data_dir).is_err());

    fs::write(
        data_dir.join("wrap-meta.json"),
        r#"{"managed_command":"\"C:/Juice/agentjuice-statusline.exe\"","original_command":"old"}"#,
    )
    .unwrap();

    assert!(Settings::restore_statusline_at(&home, &data_dir).is_err());
    assert_eq!(
        read_json(&settings_path)["statusLine"]["command"],
        "claude-hud --theme dark"
    );
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
        limit_order: "secondary_first".into(),
        fullscreen_hide_on: false,
        maximized_hide_on: true,
        indicator_style: "bar".into(),
        indicator_effect_style: "depth".into(),
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
        }
    );
    assert_eq!(settings.warn_threshold, 72.0);
    assert_eq!(settings.danger_threshold, 91.0);
    assert_eq!(settings.display_basis, "used");
    assert_eq!(settings.poll_interval_secs, 5);
    assert_eq!(settings.stale_after_secs, 80);
    assert_eq!(settings.bar_mode, "quad");
    assert_eq!(settings.limit_order, "secondary_first");
    assert!(!settings.fullscreen_hide_on);
    assert!(settings.maximized_hide_on);
    assert_eq!(settings.indicator_style, "bar");
    assert_eq!(settings.indicator_effect_style, "depth");
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
        taskbar_offset_ratio: -0.5,
        claude_taskbar_offset_ratio: -0.5,
        codex_taskbar_offset_ratio: 2.0,
        ..SettingsInput::default()
    });

    assert_eq!(settings.theme, "system");
    assert_eq!(settings.display_basis, "remaining");
    assert_eq!(settings.language, "system");
    assert_eq!(settings.font_mode, "system");
    assert!(settings.fullscreen_hide_on);
    assert!(!settings.maximized_hide_on);
    assert_eq!(settings.indicator_style, "ring");
    assert_eq!(settings.indicator_effect_style, "flat");
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
    assert_eq!(settings.taskbar_offset_ratio, 0.0);
    assert_eq!(settings.claude_taskbar_offset_ratio, 0.0);
    assert_eq!(settings.codex_taskbar_offset_ratio, 1.0);
    assert!(settings.show_claude);
    assert!(settings.show_codex);
    assert!(settings.claude_account_auto_collect_on);
}
