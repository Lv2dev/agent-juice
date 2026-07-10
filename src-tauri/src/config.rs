use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

use crate::render::Palette;

static SETTINGS_UPDATE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_palette")]
    pub palette: Palette,
    #[serde(default = "default_warn")]
    pub warn_threshold: f32,
    #[serde(default = "default_danger")]
    pub danger_threshold: f32,
    #[serde(default = "default_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_stale")]
    pub stale_after_secs: i64,
    #[serde(default = "default_bar_mode")]
    pub bar_mode: String,
    #[serde(default = "default_limit_order")]
    pub limit_order: String,
    #[serde(default = "default_fullscreen_hide_on")]
    pub fullscreen_hide_on: bool,
    #[serde(default = "default_maximized_hide_on")]
    pub maximized_hide_on: bool,
    #[serde(default = "default_indicator_style")]
    pub indicator_style: String,
    #[serde(default = "default_ring_on")]
    pub ring_on: bool,
    #[serde(default = "default_ring_numbers_on")]
    pub ring_numbers_on: bool,
    #[serde(default = "default_ring_number_outline_on")]
    pub ring_number_outline_on: bool,
    #[serde(default = "default_ring_number_outline_width_px")]
    pub ring_number_outline_width_px: f32,
    #[serde(default = "default_ring_size_px")]
    pub ring_size_px: f32,
    #[serde(default = "default_ring_thickness_px")]
    pub ring_thickness_px: f32,
    #[serde(default = "default_ring_gap_px")]
    pub ring_gap_px: f32,
    #[serde(default = "default_ring_center_gap_px")]
    pub ring_center_gap_px: f32,
    #[serde(default = "default_ring_number_font_size_px")]
    pub ring_number_font_size_px: f32,
    #[serde(default = "default_ring_number_font_weight")]
    pub ring_number_font_weight: i32,
    #[serde(default = "default_bar_text_font_size_px")]
    pub bar_text_font_size_px: f32,
    #[serde(default = "default_bar_text_font_weight")]
    pub bar_text_font_weight: i32,
    #[serde(default = "default_autostart_on")]
    pub autostart_on: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_font_mode")]
    pub font_mode: String,
    #[serde(default = "default_taskbar_offset_ratio", skip_serializing)]
    pub taskbar_offset_ratio: f32,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub claude_taskbar_offset_ratio: f32,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub codex_taskbar_offset_ratio: f32,
    #[serde(default)]
    pub claude_taskbar_monitor_key: String,
    #[serde(default)]
    pub codex_taskbar_monitor_key: String,
    #[serde(default = "default_show_tool")]
    pub show_claude: bool,
    #[serde(default = "default_show_tool")]
    pub show_codex: bool,
    #[serde(default)]
    pub claude_usage_auto_refresh_lab_on: bool,
}

#[derive(Clone, Deserialize)]
pub struct SettingsInput {
    #[serde(default = "default_palette_name")]
    pub palette: String,
    #[serde(default = "default_warn")]
    pub warn_threshold: f32,
    #[serde(default = "default_danger")]
    pub danger_threshold: f32,
    #[serde(default = "default_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_stale")]
    pub stale_after_secs: i64,
    #[serde(default = "default_bar_mode")]
    pub bar_mode: String,
    #[serde(default = "default_limit_order")]
    pub limit_order: String,
    #[serde(default = "default_fullscreen_hide_on")]
    pub fullscreen_hide_on: bool,
    #[serde(default = "default_maximized_hide_on")]
    pub maximized_hide_on: bool,
    #[serde(default = "default_indicator_style")]
    pub indicator_style: String,
    #[serde(default = "default_ring_on")]
    pub ring_on: bool,
    #[serde(default = "default_ring_numbers_on")]
    pub ring_numbers_on: bool,
    #[serde(default = "default_ring_number_outline_on")]
    pub ring_number_outline_on: bool,
    #[serde(default = "default_ring_number_outline_width_px")]
    pub ring_number_outline_width_px: f32,
    #[serde(default = "default_ring_size_px")]
    pub ring_size_px: f32,
    #[serde(default = "default_ring_thickness_px")]
    pub ring_thickness_px: f32,
    #[serde(default = "default_ring_gap_px")]
    pub ring_gap_px: f32,
    #[serde(default = "default_ring_center_gap_px")]
    pub ring_center_gap_px: f32,
    #[serde(default = "default_ring_number_font_size_px")]
    pub ring_number_font_size_px: f32,
    #[serde(default = "default_ring_number_font_weight")]
    pub ring_number_font_weight: i32,
    #[serde(default = "default_bar_text_font_size_px")]
    pub bar_text_font_size_px: f32,
    #[serde(default = "default_bar_text_font_weight")]
    pub bar_text_font_weight: i32,
    #[serde(default = "default_autostart_on")]
    pub autostart_on: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_font_mode")]
    pub font_mode: String,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub taskbar_offset_ratio: f32,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub claude_taskbar_offset_ratio: f32,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub codex_taskbar_offset_ratio: f32,
    #[serde(default)]
    pub claude_taskbar_monitor_key: String,
    #[serde(default)]
    pub codex_taskbar_monitor_key: String,
    #[serde(default = "default_show_tool")]
    pub show_claude: bool,
    #[serde(default = "default_show_tool")]
    pub show_codex: bool,
    #[serde(default)]
    pub claude_usage_auto_refresh_lab_on: bool,
    #[serde(default)]
    pub custom_safe: Option<String>,
    #[serde(default)]
    pub custom_warn: Option<String>,
    #[serde(default)]
    pub custom_danger: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct WrapMeta {
    managed_command: String,
    original_command: Option<String>,
}

fn default_palette_name() -> String {
    "traffic".into()
}

fn default_palette() -> Palette {
    Palette::Traffic
}

fn default_warn() -> f32 {
    70.0
}

fn default_danger() -> f32 {
    90.0
}

fn default_interval() -> u64 {
    2
}

fn default_stale() -> i64 {
    90
}

fn default_bar_mode() -> String {
    "full".into()
}

fn default_limit_order() -> String {
    "primary_first".into()
}

fn default_fullscreen_hide_on() -> bool {
    true
}

fn default_maximized_hide_on() -> bool {
    false
}

fn default_indicator_style() -> String {
    "ring".into()
}

fn default_ring_on() -> bool {
    true
}

fn default_ring_numbers_on() -> bool {
    true
}

fn default_ring_number_outline_on() -> bool {
    true
}

fn default_ring_number_outline_width_px() -> f32 {
    1.2
}

fn default_ring_size_px() -> f32 {
    36.0
}

fn default_ring_thickness_px() -> f32 {
    4.0
}

fn default_ring_gap_px() -> f32 {
    6.0
}

fn default_ring_center_gap_px() -> f32 {
    0.0
}

fn default_ring_number_font_size_px() -> f32 {
    9.0
}

fn default_ring_number_font_weight() -> i32 {
    600
}

fn default_bar_text_font_size_px() -> f32 {
    11.0
}

fn default_bar_text_font_weight() -> i32 {
    500
}

fn default_autostart_on() -> bool {
    true
}

fn default_language() -> String {
    "system".into()
}

fn default_theme() -> String {
    "system".into()
}

fn default_font_mode() -> String {
    "system".into()
}

fn default_taskbar_offset_ratio() -> f32 {
    0.5
}

fn default_show_tool() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            palette: default_palette(),
            warn_threshold: default_warn(),
            danger_threshold: default_danger(),
            poll_interval_secs: default_interval(),
            stale_after_secs: default_stale(),
            bar_mode: default_bar_mode(),
            limit_order: default_limit_order(),
            fullscreen_hide_on: default_fullscreen_hide_on(),
            maximized_hide_on: default_maximized_hide_on(),
            indicator_style: default_indicator_style(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_gap_px: default_ring_center_gap_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            autostart_on: default_autostart_on(),
            language: default_language(),
            theme: default_theme(),
            font_mode: default_font_mode(),
            taskbar_offset_ratio: default_taskbar_offset_ratio(),
            claude_taskbar_offset_ratio: default_taskbar_offset_ratio(),
            codex_taskbar_offset_ratio: default_taskbar_offset_ratio(),
            claude_taskbar_monitor_key: String::new(),
            codex_taskbar_monitor_key: String::new(),
            show_claude: default_show_tool(),
            show_codex: default_show_tool(),
            claude_usage_auto_refresh_lab_on: false,
        }
    }
}

impl Default for SettingsInput {
    fn default() -> Self {
        Self {
            palette: default_palette_name(),
            warn_threshold: default_warn(),
            danger_threshold: default_danger(),
            poll_interval_secs: default_interval(),
            stale_after_secs: default_stale(),
            bar_mode: default_bar_mode(),
            limit_order: default_limit_order(),
            fullscreen_hide_on: default_fullscreen_hide_on(),
            maximized_hide_on: default_maximized_hide_on(),
            indicator_style: default_indicator_style(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_gap_px: default_ring_center_gap_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            autostart_on: default_autostart_on(),
            language: default_language(),
            theme: default_theme(),
            font_mode: default_font_mode(),
            taskbar_offset_ratio: default_taskbar_offset_ratio(),
            claude_taskbar_offset_ratio: default_taskbar_offset_ratio(),
            codex_taskbar_offset_ratio: default_taskbar_offset_ratio(),
            claude_taskbar_monitor_key: String::new(),
            codex_taskbar_monitor_key: String::new(),
            show_claude: default_show_tool(),
            show_codex: default_show_tool(),
            claude_usage_auto_refresh_lab_on: false,
            custom_safe: None,
            custom_warn: None,
            custom_danger: None,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|dir| dir.join("agent-juice").join("settings.json"))
    }

    fn agent_dir() -> Option<PathBuf> {
        dirs::data_local_dir().map(|dir| dir.join("agent-juice"))
    }

    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |path| Self::load_from(&path))
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return Self::default();
        };

        let value = serde_json::from_str::<serde_json::Value>(&contents).ok();
        let mut settings = serde_json::from_str::<Self>(&contents).unwrap_or_default();
        settings.apply_legacy_taskbar_offset(value.as_ref());
        settings.clamp_offsets();
        settings.clamp_ring_geometry();
        settings.limit_order = normalize_limit_order(&settings.limit_order).into();
        settings.indicator_style = normalize_indicator_style(&settings.indicator_style).into();
        settings.language = normalize_language(&settings.language).into();
        settings.theme = normalize_theme(&settings.theme).into();
        settings.font_mode = normalize_font_mode(&settings.font_mode).into();
        settings
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("no settings path"))?;
        self.save_to(&path)
    }

    pub(crate) fn update(mutator: impl FnOnce(&mut Self)) -> anyhow::Result<Self> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("no settings path"))?;
        Self::update_at(&path, mutator)
    }

    pub(crate) fn update_at(path: &Path, mutator: impl FnOnce(&mut Self)) -> anyhow::Result<Self> {
        let _guard = SETTINGS_UPDATE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let mut settings = Self::load_from(path);
        mutator(&mut settings);
        settings.save_to(path)?;
        Ok(settings)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        replace_file(path, serde_json::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }

    pub fn from_ui(palette: &str, warn: f32, danger: f32, interval: u64) -> Self {
        Self::from_input(SettingsInput {
            palette: palette.into(),
            warn_threshold: warn,
            danger_threshold: danger,
            poll_interval_secs: interval,
            stale_after_secs: default_stale(),
            bar_mode: default_bar_mode(),
            limit_order: default_limit_order(),
            fullscreen_hide_on: default_fullscreen_hide_on(),
            maximized_hide_on: default_maximized_hide_on(),
            indicator_style: default_indicator_style(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_gap_px: default_ring_center_gap_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            autostart_on: default_autostart_on(),
            language: default_language(),
            theme: default_theme(),
            font_mode: default_font_mode(),
            taskbar_offset_ratio: default_taskbar_offset_ratio(),
            claude_taskbar_offset_ratio: default_taskbar_offset_ratio(),
            codex_taskbar_offset_ratio: default_taskbar_offset_ratio(),
            claude_taskbar_monitor_key: String::new(),
            codex_taskbar_monitor_key: String::new(),
            show_claude: default_show_tool(),
            show_codex: default_show_tool(),
            claude_usage_auto_refresh_lab_on: false,
            custom_safe: None,
            custom_warn: None,
            custom_danger: None,
        })
    }

    pub fn from_input(input: SettingsInput) -> Self {
        let warn = clamp_percent(input.warn_threshold);
        let danger = clamp_percent(input.danger_threshold).max(warn);
        Self {
            palette: palette_from_input(&input),
            warn_threshold: warn,
            danger_threshold: danger,
            poll_interval_secs: input.poll_interval_secs.max(1),
            stale_after_secs: input.stale_after_secs.max(1),
            bar_mode: normalize_bar_mode(&input.bar_mode).into(),
            limit_order: normalize_limit_order(&input.limit_order).into(),
            fullscreen_hide_on: input.fullscreen_hide_on,
            maximized_hide_on: input.maximized_hide_on,
            indicator_style: normalize_indicator_style(&input.indicator_style).into(),
            ring_on: input.ring_on,
            ring_numbers_on: input.ring_numbers_on,
            ring_number_outline_on: input.ring_number_outline_on,
            ring_number_outline_width_px: clamp_ring_number_outline_width(
                input.ring_number_outline_width_px,
            ),
            ring_size_px: clamp_px(input.ring_size_px, default_ring_size_px(), 20.0, 44.0),
            ring_thickness_px: clamp_ring_thickness(input.ring_thickness_px),
            ring_gap_px: clamp_ring_gap(input.ring_gap_px),
            ring_center_gap_px: clamp_ring_center_gap(input.ring_center_gap_px),
            ring_number_font_size_px: clamp_px(
                input.ring_number_font_size_px,
                default_ring_number_font_size_px(),
                6.0,
                16.0,
            ),
            ring_number_font_weight: clamp_font_weight(input.ring_number_font_weight),
            bar_text_font_size_px: clamp_px(
                input.bar_text_font_size_px,
                default_bar_text_font_size_px(),
                8.0,
                16.0,
            ),
            bar_text_font_weight: clamp_font_weight(input.bar_text_font_weight),
            autostart_on: input.autostart_on,
            language: normalize_language(&input.language).into(),
            theme: normalize_theme(&input.theme).into(),
            font_mode: normalize_font_mode(&input.font_mode).into(),
            taskbar_offset_ratio: clamp_ratio(input.taskbar_offset_ratio),
            claude_taskbar_offset_ratio: clamp_ratio(input.claude_taskbar_offset_ratio),
            codex_taskbar_offset_ratio: clamp_ratio(input.codex_taskbar_offset_ratio),
            claude_taskbar_monitor_key: input.claude_taskbar_monitor_key,
            codex_taskbar_monitor_key: input.codex_taskbar_monitor_key,
            show_claude: input.show_claude,
            show_codex: input.show_codex,
            claude_usage_auto_refresh_lab_on: input.claude_usage_auto_refresh_lab_on,
        }
    }

    fn apply_legacy_taskbar_offset(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value else {
            return;
        };
        let Some(legacy) = json_f32_field(value, "taskbar_offset_ratio").map(clamp_ratio) else {
            return;
        };

        if !json_has_field(value, "claude_taskbar_offset_ratio") {
            self.claude_taskbar_offset_ratio = legacy;
        }
        if !json_has_field(value, "codex_taskbar_offset_ratio") {
            self.codex_taskbar_offset_ratio = legacy;
        }
    }

    fn clamp_offsets(&mut self) {
        self.taskbar_offset_ratio = clamp_ratio(self.taskbar_offset_ratio);
        self.claude_taskbar_offset_ratio = clamp_ratio(self.claude_taskbar_offset_ratio);
        self.codex_taskbar_offset_ratio = clamp_ratio(self.codex_taskbar_offset_ratio);
    }

    fn clamp_ring_geometry(&mut self) {
        self.ring_size_px = clamp_px(self.ring_size_px, default_ring_size_px(), 20.0, 44.0);
        self.ring_thickness_px = clamp_ring_thickness(self.ring_thickness_px);
        self.ring_gap_px = clamp_ring_gap(self.ring_gap_px);
        self.ring_center_gap_px = clamp_ring_center_gap(self.ring_center_gap_px);
        self.ring_number_outline_width_px =
            clamp_ring_number_outline_width(self.ring_number_outline_width_px);
        self.ring_number_font_size_px = clamp_px(
            self.ring_number_font_size_px,
            default_ring_number_font_size_px(),
            6.0,
            16.0,
        );
        self.ring_number_font_weight = clamp_font_weight(self.ring_number_font_weight);
        self.bar_text_font_size_px = clamp_px(
            self.bar_text_font_size_px,
            default_bar_text_font_size_px(),
            8.0,
            16.0,
        );
        self.bar_text_font_weight = clamp_font_weight(self.bar_text_font_weight);
    }

    pub fn install_statusline_wrap(bridge_abs: &str) -> anyhow::Result<()> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let agent_dir = Self::agent_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
        Self::install_statusline_wrap_at(&home, &agent_dir, bridge_abs)
    }

    pub fn install_statusline_wrap_at(
        home: &Path,
        agent_dir: &Path,
        bridge_abs: &str,
    ) -> anyhow::Result<()> {
        let settings_path = claude_settings_path(home);
        let bridge = bridge_abs.replace('\\', "/");
        let managed_command = command_for_bridge(&bridge);
        std::fs::create_dir_all(agent_dir)?;
        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let raw = std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".into());
        let mut value = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({}));
        if !value.is_object() {
            value = serde_json::json!({});
        }

        let current = value
            .pointer("/statusLine/command")
            .and_then(|command| command.as_str())
            .unwrap_or("")
            .to_string();

        let wrap_path = agent_dir.join("wrap.json");
        let meta_path = agent_dir.join("wrap-meta.json");
        let previous_meta = read_wrap_meta(&meta_path).ok();
        let original_command = if is_agentjuice_command(&current) {
            previous_meta
                .and_then(|meta| meta.original_command)
                .or_else(|| read_nonempty(&wrap_path).ok().flatten())
        } else {
            nonempty_trimmed(&current)
        };

        if let Some(original) = &original_command {
            replace_file(&wrap_path, original.as_bytes())?;
        } else if wrap_path.exists() {
            let _ = std::fs::remove_file(&wrap_path);
        }

        if settings_path.exists() {
            std::fs::copy(
                &settings_path,
                settings_path.with_extension("json.aj-backup"),
            )?;
        }

        value["statusLine"] = serde_json::json!({
            "type": "command",
            "command": managed_command,
        });
        write_wrap_meta(
            &meta_path,
            &WrapMeta {
                managed_command,
                original_command,
            },
        )?;
        replace_file(
            &settings_path,
            serde_json::to_string_pretty(&value)?.as_bytes(),
        )?;
        Ok(())
    }

    pub fn restore_statusline() -> anyhow::Result<()> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let agent_dir = Self::agent_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
        Self::restore_statusline_at(&home, &agent_dir)
    }

    pub fn restore_statusline_at(home: &Path, agent_dir: &Path) -> anyhow::Result<()> {
        let settings_path = claude_settings_path(home);
        let meta = read_wrap_meta(&agent_dir.join("wrap-meta.json"))?;
        let raw = std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".into());
        let mut value = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({}));
        if !value.is_object() {
            value = serde_json::json!({});
        }

        let current = value
            .pointer("/statusLine/command")
            .and_then(|command| command.as_str())
            .unwrap_or("");
        if !same_command(current, &meta.managed_command) {
            anyhow::bail!("Claude statusLine is not managed by Juice");
        }

        if let Some(original) = meta.original_command.as_deref().and_then(nonempty_trimmed) {
            value["statusLine"] = serde_json::json!({
                "type": "command",
                "command": original,
            });
        } else {
            if let Some(object) = value.as_object_mut() {
                object.remove("statusLine");
            }
        }

        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        replace_file(
            &settings_path,
            serde_json::to_string_pretty(&value)?.as_bytes(),
        )?;
        Ok(())
    }
}

fn command_for_bridge(bridge: &str) -> String {
    format!("\"{}\"", bridge.replace('"', ""))
}

fn nonempty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_nonempty(path: &Path) -> anyhow::Result<Option<String>> {
    Ok(std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| nonempty_trimmed(&contents)))
}

fn read_wrap_meta(path: &Path) -> anyhow::Result<WrapMeta> {
    let contents = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&contents)?)
}

fn write_wrap_meta(path: &Path, meta: &WrapMeta) -> anyhow::Result<()> {
    replace_file(path, serde_json::to_string_pretty(meta)?.as_bytes())?;
    Ok(())
}

fn normalized_command_path(command: &str) -> String {
    command
        .trim()
        .trim_matches('"')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn is_agentjuice_command(command: &str) -> bool {
    let normalized = normalized_command_path(command);
    normalized.ends_with("/agentjuice-statusline.exe")
        || normalized.ends_with("/agentjuice-statusline")
}

fn same_command(left: &str, right: &str) -> bool {
    normalized_command_path(left) == normalized_command_path(right)
}

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn clamp_percent(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn normalize_bar_mode(value: &str) -> &'static str {
    match value {
        "compact" => "compact",
        "dual" => "dual",
        "quad" => "quad",
        _ => "full",
    }
}

fn normalize_limit_order(value: &str) -> &'static str {
    match value {
        "secondary_first" => "secondary_first",
        _ => "primary_first",
    }
}

fn normalize_indicator_style(value: &str) -> &'static str {
    match value {
        "bar" => "bar",
        _ => "ring",
    }
}

fn normalize_theme(value: &str) -> &'static str {
    match value {
        "light" => "light",
        "dark" => "dark",
        _ => "system",
    }
}

fn normalize_language(value: &str) -> &'static str {
    match value {
        "ko" => "ko",
        "en" => "en",
        _ => "system",
    }
}

fn normalize_font_mode(value: &str) -> &'static str {
    match value {
        "pretendard" => "pretendard",
        _ => "system",
    }
}

fn clamp_ratio(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        default_taskbar_offset_ratio()
    }
}

fn clamp_px(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn clamp_font_weight(value: i32) -> i32 {
    value.clamp(100, 900)
}

fn clamp_ring_thickness(value: f32) -> f32 {
    clamp_px(value, default_ring_thickness_px(), 1.0, 10.0)
}

fn clamp_ring_gap(value: f32) -> f32 {
    clamp_px(value, default_ring_gap_px(), 2.0, 14.0)
}

fn clamp_ring_center_gap(value: f32) -> f32 {
    clamp_px(value, default_ring_center_gap_px(), 0.0, 8.0)
}

fn clamp_ring_number_outline_width(value: f32) -> f32 {
    clamp_px(value, default_ring_number_outline_width_px(), 0.0, 4.0)
}

fn json_has_field(value: &serde_json::Value, key: &str) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.contains_key(key))
}

fn json_f32_field(value: &serde_json::Value, key: &str) -> Option<f32> {
    value.get(key)?.as_f64().map(|number| number as f32)
}

fn palette_from_input(input: &SettingsInput) -> Palette {
    match input.palette.to_ascii_lowercase().as_str() {
        "cvd" => Palette::Cvd,
        "cool" => Palette::Cool,
        "custom" => match (
            parse_hex_rgb(input.custom_safe.as_deref()),
            parse_hex_rgb(input.custom_warn.as_deref()),
            parse_hex_rgb(input.custom_danger.as_deref()),
        ) {
            (Some(safe), Some(warn), Some(danger)) => Palette::Custom(safe, warn, danger),
            _ => Palette::Traffic,
        },
        _ => Palette::Traffic,
    }
}

fn parse_hex_rgb(value: Option<&str>) -> Option<[u8; 3]> {
    let value = value?.trim().strip_prefix('#').unwrap_or(value?.trim());
    if value.len() != 6 {
        return None;
    }

    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some([red, green, blue])
}

fn replace_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent-juice");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(
        ".{file_name}.{}.{}.aj-tmp",
        std::process::id(),
        sequence
    ));
    std::fs::write(&tmp, contents)?;
    let result = if path.exists() {
        replace_existing_file(path, &tmp)
    } else {
        std::fs::rename(&tmp, path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

#[cfg(windows)]
fn replace_existing_file(path: &Path, tmp: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS},
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let target = wide(path);
    let replacement = wide(tmp);
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .map_err(|err| std::io::Error::other(err.to_string()))
    }
}

#[cfg(not(windows))]
fn replace_existing_file(path: &Path, tmp: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, path)
}
