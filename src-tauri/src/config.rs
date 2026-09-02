use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::SystemTime,
};

use crate::{paths, render::Palette};

static SETTINGS_UPDATE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolColors {
    pub claude_primary: [u8; 3],
    pub claude_secondary: [u8; 3],
    pub codex_primary: [u8; 3],
    pub codex_secondary: [u8; 3],
    pub grok_primary: [u8; 3],
    pub grok_secondary: [u8; 3],
    pub cursor_primary: [u8; 3],
    pub cursor_secondary: [u8; 3],
    pub warning: [u8; 3],
    pub danger: [u8; 3],
    pub warning_on: bool,
    pub danger_on: bool,
}

const V0_1_14_CURSOR_PRIMARY: [u8; 3] = [0x3b, 0x82, 0xf6];
const V0_1_14_CURSOR_SECONDARY: [u8; 3] = [0x06, 0xb6, 0xd4];
const V0_1_19_CURSOR_PRIMARY: [u8; 3] = [0x72, 0x71, 0x6d];
const V0_1_19_CURSOR_SECONDARY: [u8; 3] = [0x08, 0x91, 0xb2];
const DEFAULT_CURSOR_PRIMARY: [u8; 3] = [0x85, 0x84, 0x7f];
const DEFAULT_CURSOR_SECONDARY: [u8; 3] = [0x08, 0x91, 0xb2];

const LEGACY_DEFAULT_TOOL_COLORS: ToolColors = ToolColors {
    claude_primary: [0xb7, 0x83, 0x3a],
    claude_secondary: [0xa6, 0x5f, 0x72],
    codex_primary: [0x4f, 0x8a, 0x73],
    codex_secondary: [0x4f, 0x76, 0xa6],
    grok_primary: [0xd9, 0x57, 0x8b],
    grok_secondary: [0x8a, 0x6f, 0xd1],
    cursor_primary: DEFAULT_CURSOR_PRIMARY,
    cursor_secondary: DEFAULT_CURSOR_SECONDARY,
    warning: [0xf5, 0x9e, 0x0b],
    danger: [0xef, 0x44, 0x44],
    warning_on: true,
    danger_on: true,
};

impl Default for ToolColors {
    fn default() -> Self {
        Self {
            claude_primary: [0xd7, 0x9a, 0x32],
            claude_secondary: [0xd3, 0x6b, 0x86],
            codex_primary: [0x2f, 0xac, 0x7d],
            codex_secondary: [0x4d, 0x86, 0xd6],
            grok_primary: [0xd9, 0x57, 0x8b],
            grok_secondary: [0x8a, 0x6f, 0xd1],
            cursor_primary: DEFAULT_CURSOR_PRIMARY,
            cursor_secondary: DEFAULT_CURSOR_SECONDARY,
            warning: [0xf5, 0x9e, 0x0b],
            danger: [0xef, 0x44, 0x44],
            warning_on: true,
            danger_on: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskbarTextColors {
    pub claude: [u8; 3],
    pub claude_on: bool,
    pub codex: [u8; 3],
    pub codex_on: bool,
    pub grok: [u8; 3],
    pub grok_on: bool,
    pub cursor: [u8; 3],
    pub cursor_on: bool,
    pub info: [u8; 3],
    pub info_on: bool,
    pub ring: [u8; 3],
    pub ring_on: bool,
}

impl Default for TaskbarTextColors {
    fn default() -> Self {
        Self {
            claude: [0xd7, 0x9a, 0x32],
            claude_on: false,
            codex: [0x2f, 0xac, 0x7d],
            codex_on: false,
            grok: [0xd9, 0x57, 0x8b],
            grok_on: false,
            cursor: DEFAULT_CURSOR_PRIMARY,
            cursor_on: false,
            info: [0x6b, 0x72, 0x80],
            info_on: false,
            ring: [0x6b, 0x72, 0x80],
            ring_on: false,
        }
    }
}

const MAX_TASKBAR_LAYOUT_PROFILES: usize = 16;
const MAX_TASKBAR_PROFILE_MONITORS: usize = 16;
const MAX_TASKBAR_MONITOR_KEY_LEN: usize = 2048;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskbarPlacement {
    pub monitor_key: String,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub offset_ratio: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskbarPresentationProfile {
    pub bar_mode: String,
    pub full_reset_time_on: bool,
    pub limit_order: String,
    pub indicator_style: String,
    pub indicator_effect_style: String,
    pub ring_on: bool,
    pub ring_numbers_on: bool,
    pub ring_number_outline_on: bool,
    pub ring_number_outline_width_px: f32,
    pub ring_size_px: f32,
    pub ring_thickness_px: f32,
    pub ring_gap_px: f32,
    pub ring_center_size_px: f32,
    pub ring_number_font_size_px: f32,
    pub ring_number_font_weight: i32,
    pub bar_text_font_size_px: f32,
    pub bar_text_font_weight: i32,
    pub bar_content_gap_px: f32,
}

impl Default for TaskbarPresentationProfile {
    fn default() -> Self {
        Self {
            bar_mode: default_bar_mode(),
            full_reset_time_on: default_full_reset_time_on(),
            limit_order: default_limit_order(),
            indicator_style: default_indicator_style(),
            indicator_effect_style: default_indicator_effect_style(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_size_px: default_ring_center_size_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            bar_content_gap_px: default_bar_content_gap_px(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskbarAppearanceProfile {
    pub palette: Palette,
    pub tool_colors: ToolColors,
    pub taskbar_text_colors: TaskbarTextColors,
    pub indicator_track_color_auto: bool,
    pub indicator_track_color: [u8; 3],
    pub indicator_track_opacity_percent: f32,
}

impl Default for TaskbarAppearanceProfile {
    fn default() -> Self {
        Self {
            palette: default_palette(),
            tool_colors: ToolColors::default(),
            taskbar_text_colors: TaskbarTextColors::default(),
            indicator_track_color_auto: default_indicator_track_color_auto(),
            indicator_track_color: default_indicator_track_color(),
            indicator_track_opacity_percent: default_indicator_track_opacity_percent(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskbarLayoutProfile {
    #[serde(default)]
    pub monitor_keys: Vec<String>,
    #[serde(default)]
    pub claude: Option<TaskbarPlacement>,
    #[serde(default)]
    pub codex: Option<TaskbarPlacement>,
    #[serde(default)]
    pub grok: Option<TaskbarPlacement>,
    #[serde(default)]
    pub cursor: Option<TaskbarPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<TaskbarPresentationProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<TaskbarAppearanceProfile>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_palette")]
    pub palette: Palette,
    #[serde(default)]
    pub tool_colors: ToolColors,
    #[serde(default)]
    pub taskbar_text_colors: TaskbarTextColors,
    #[serde(default = "default_warn")]
    pub warn_threshold: f32,
    #[serde(default = "default_danger")]
    pub danger_threshold: f32,
    #[serde(default = "default_display_basis")]
    pub display_basis: String,
    #[serde(default = "default_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_stale")]
    pub stale_after_secs: i64,
    #[serde(default = "default_activity_weeks")]
    pub activity_weeks: u16,
    #[serde(default = "default_activity_scale_mode")]
    pub activity_scale_mode: String,
    #[serde(default = "default_activity_tokens_per_level")]
    pub activity_tokens_per_level: u64,
    #[serde(default = "default_bar_mode")]
    pub bar_mode: String,
    #[serde(default = "default_full_reset_time_on")]
    pub full_reset_time_on: bool,
    #[serde(default = "default_limit_order")]
    pub limit_order: String,
    #[serde(default = "legacy_fullscreen_hide_on")]
    pub fullscreen_hide_on: bool,
    #[serde(default = "default_maximized_hide_on")]
    pub maximized_hide_on: bool,
    #[serde(default = "default_taskbar_avoid_overlap_on")]
    pub taskbar_avoid_overlap_on: bool,
    #[serde(default)]
    pub taskbar_bars_paused: bool,
    #[serde(default = "default_taskbar_layout_memory_on")]
    pub taskbar_layout_memory_on: bool,
    #[serde(default = "default_taskbar_profile_presentation_on")]
    pub taskbar_profile_presentation_on: bool,
    #[serde(default = "default_taskbar_profile_colors_on")]
    pub taskbar_profile_colors_on: bool,
    #[serde(default)]
    pub taskbar_layout_profiles: Vec<TaskbarLayoutProfile>,
    #[serde(default)]
    pub taskbar_layout_memory_initialized: bool,
    #[serde(default = "default_indicator_style")]
    pub indicator_style: String,
    #[serde(default = "default_indicator_effect_style")]
    pub indicator_effect_style: String,
    #[serde(default = "default_indicator_track_color_auto")]
    pub indicator_track_color_auto: bool,
    #[serde(default = "default_indicator_track_color")]
    pub indicator_track_color: [u8; 3],
    #[serde(default = "default_indicator_track_opacity_percent")]
    pub indicator_track_opacity_percent: f32,
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
    #[serde(default = "default_ring_center_size_px")]
    pub ring_center_size_px: f32,
    #[serde(default = "default_ring_number_font_size_px")]
    pub ring_number_font_size_px: f32,
    #[serde(default = "default_ring_number_font_weight")]
    pub ring_number_font_weight: i32,
    #[serde(default = "default_bar_text_font_size_px")]
    pub bar_text_font_size_px: f32,
    #[serde(default = "default_bar_text_font_weight")]
    pub bar_text_font_weight: i32,
    #[serde(default = "default_bar_content_gap_px")]
    pub bar_content_gap_px: f32,
    #[serde(default = "default_autostart_on")]
    pub autostart_on: bool,
    #[serde(default = "default_update_check_on")]
    pub update_check_on: bool,
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
    #[serde(default = "default_taskbar_offset_ratio")]
    pub grok_taskbar_offset_ratio: f32,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub cursor_taskbar_offset_ratio: f32,
    #[serde(default)]
    pub claude_taskbar_monitor_key: String,
    #[serde(default)]
    pub codex_taskbar_monitor_key: String,
    #[serde(default)]
    pub grok_taskbar_monitor_key: String,
    #[serde(default)]
    pub cursor_taskbar_monitor_key: String,
    #[serde(default)]
    pub claude_taskbar_target_initialized: bool,
    #[serde(default)]
    pub codex_taskbar_target_initialized: bool,
    #[serde(default)]
    pub grok_taskbar_target_initialized: bool,
    #[serde(default)]
    pub cursor_taskbar_target_initialized: bool,
    #[serde(default = "default_show_tool")]
    pub show_claude: bool,
    #[serde(default = "default_show_tool")]
    pub show_codex: bool,
    #[serde(default)]
    pub show_grok: bool,
    #[serde(default)]
    pub show_cursor: bool,
    #[serde(
        default = "default_claude_account_auto_collect_on",
        alias = "claude_usage_auto_refresh_lab_on"
    )]
    pub claude_account_auto_collect_on: bool,
}

impl TaskbarPresentationProfile {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            bar_mode: settings.bar_mode.clone(),
            full_reset_time_on: settings.full_reset_time_on,
            limit_order: settings.limit_order.clone(),
            indicator_style: settings.indicator_style.clone(),
            indicator_effect_style: settings.indicator_effect_style.clone(),
            ring_on: settings.ring_on,
            ring_numbers_on: settings.ring_numbers_on,
            ring_number_outline_on: settings.ring_number_outline_on,
            ring_number_outline_width_px: settings.ring_number_outline_width_px,
            ring_size_px: settings.ring_size_px,
            ring_thickness_px: settings.ring_thickness_px,
            ring_gap_px: settings.ring_gap_px,
            ring_center_size_px: settings.ring_center_size_px,
            ring_number_font_size_px: settings.ring_number_font_size_px,
            ring_number_font_weight: settings.ring_number_font_weight,
            bar_text_font_size_px: settings.bar_text_font_size_px,
            bar_text_font_weight: settings.bar_text_font_weight,
            bar_content_gap_px: settings.bar_content_gap_px,
        }
    }

    pub fn apply_to(&self, settings: &mut Settings) {
        settings.bar_mode = self.bar_mode.clone();
        settings.full_reset_time_on = self.full_reset_time_on;
        settings.limit_order = self.limit_order.clone();
        settings.indicator_style = self.indicator_style.clone();
        settings.indicator_effect_style = self.indicator_effect_style.clone();
        settings.ring_on = self.ring_on;
        settings.ring_numbers_on = self.ring_numbers_on;
        settings.ring_number_outline_on = self.ring_number_outline_on;
        settings.ring_number_outline_width_px = self.ring_number_outline_width_px;
        settings.ring_size_px = self.ring_size_px;
        settings.ring_thickness_px = self.ring_thickness_px;
        settings.ring_gap_px = self.ring_gap_px;
        settings.ring_center_size_px = self.ring_center_size_px;
        settings.ring_number_font_size_px = self.ring_number_font_size_px;
        settings.ring_number_font_weight = self.ring_number_font_weight;
        settings.bar_text_font_size_px = self.bar_text_font_size_px;
        settings.bar_text_font_weight = self.bar_text_font_weight;
        settings.bar_content_gap_px = self.bar_content_gap_px;
    }
}

impl TaskbarAppearanceProfile {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            palette: settings.palette,
            tool_colors: settings.tool_colors,
            taskbar_text_colors: settings.taskbar_text_colors,
            indicator_track_color_auto: settings.indicator_track_color_auto,
            indicator_track_color: settings.indicator_track_color,
            indicator_track_opacity_percent: settings.indicator_track_opacity_percent,
        }
    }

    pub fn apply_to(&self, settings: &mut Settings) {
        settings.palette = self.palette;
        settings.tool_colors = self.tool_colors;
        settings.taskbar_text_colors = self.taskbar_text_colors;
        settings.indicator_track_color_auto = self.indicator_track_color_auto;
        settings.indicator_track_color = self.indicator_track_color;
        settings.indicator_track_opacity_percent = self.indicator_track_opacity_percent;
    }
}

#[derive(Clone, Deserialize)]
pub struct SettingsInput {
    #[serde(default = "default_palette_name")]
    pub palette: String,
    #[serde(default = "default_warn")]
    pub warn_threshold: f32,
    #[serde(default = "default_danger")]
    pub danger_threshold: f32,
    #[serde(default = "default_display_basis")]
    pub display_basis: String,
    #[serde(default = "default_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_stale")]
    pub stale_after_secs: i64,
    #[serde(default = "default_activity_weeks")]
    pub activity_weeks: u16,
    #[serde(default = "default_activity_scale_mode")]
    pub activity_scale_mode: String,
    #[serde(default = "default_activity_tokens_per_level")]
    pub activity_tokens_per_level: u64,
    #[serde(default = "default_bar_mode")]
    pub bar_mode: String,
    #[serde(default = "default_full_reset_time_on")]
    pub full_reset_time_on: bool,
    #[serde(default = "default_limit_order")]
    pub limit_order: String,
    #[serde(default = "default_fullscreen_hide_on")]
    pub fullscreen_hide_on: bool,
    #[serde(default = "default_maximized_hide_on")]
    pub maximized_hide_on: bool,
    #[serde(default = "default_taskbar_avoid_overlap_on")]
    pub taskbar_avoid_overlap_on: bool,
    #[serde(default = "default_taskbar_layout_memory_on")]
    pub taskbar_layout_memory_on: bool,
    #[serde(default = "default_taskbar_profile_presentation_on")]
    pub taskbar_profile_presentation_on: bool,
    #[serde(default = "default_taskbar_profile_colors_on")]
    pub taskbar_profile_colors_on: bool,
    #[serde(default = "default_indicator_style")]
    pub indicator_style: String,
    #[serde(default = "default_indicator_effect_style")]
    pub indicator_effect_style: String,
    #[serde(default = "default_indicator_track_color_auto")]
    pub indicator_track_color_auto: bool,
    #[serde(default)]
    pub indicator_track_color: Option<String>,
    #[serde(default = "default_indicator_track_opacity_percent")]
    pub indicator_track_opacity_percent: f32,
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
    #[serde(default = "default_ring_center_size_px")]
    pub ring_center_size_px: f32,
    #[serde(default = "default_ring_number_font_size_px")]
    pub ring_number_font_size_px: f32,
    #[serde(default = "default_ring_number_font_weight")]
    pub ring_number_font_weight: i32,
    #[serde(default = "default_bar_text_font_size_px")]
    pub bar_text_font_size_px: f32,
    #[serde(default = "default_bar_text_font_weight")]
    pub bar_text_font_weight: i32,
    #[serde(default = "default_bar_content_gap_px")]
    pub bar_content_gap_px: f32,
    #[serde(default = "default_autostart_on")]
    pub autostart_on: bool,
    #[serde(default = "default_update_check_on")]
    pub update_check_on: bool,
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
    #[serde(default = "default_taskbar_offset_ratio")]
    pub grok_taskbar_offset_ratio: f32,
    #[serde(default = "default_taskbar_offset_ratio")]
    pub cursor_taskbar_offset_ratio: f32,
    #[serde(default)]
    pub claude_taskbar_monitor_key: String,
    #[serde(default)]
    pub codex_taskbar_monitor_key: String,
    #[serde(default)]
    pub grok_taskbar_monitor_key: String,
    #[serde(default)]
    pub cursor_taskbar_monitor_key: String,
    #[serde(default = "default_show_tool")]
    pub show_claude: bool,
    #[serde(default = "default_show_tool")]
    pub show_codex: bool,
    #[serde(default)]
    pub show_grok: bool,
    #[serde(default)]
    pub show_cursor: bool,
    #[serde(
        default = "default_claude_account_auto_collect_on",
        alias = "claude_usage_auto_refresh_lab_on"
    )]
    pub claude_account_auto_collect_on: bool,
    #[serde(default)]
    pub mono_color: Option<String>,
    #[serde(default)]
    pub custom_safe: Option<String>,
    #[serde(default)]
    pub custom_warn: Option<String>,
    #[serde(default)]
    pub custom_danger: Option<String>,
    #[serde(default)]
    pub claude_primary_color: Option<String>,
    #[serde(default)]
    pub claude_secondary_color: Option<String>,
    #[serde(default)]
    pub codex_primary_color: Option<String>,
    #[serde(default)]
    pub codex_secondary_color: Option<String>,
    #[serde(default)]
    pub grok_primary_color: Option<String>,
    #[serde(default)]
    pub grok_secondary_color: Option<String>,
    #[serde(default)]
    pub cursor_primary_color: Option<String>,
    #[serde(default)]
    pub cursor_secondary_color: Option<String>,
    #[serde(default)]
    pub tool_warning_color: Option<String>,
    #[serde(default)]
    pub tool_danger_color: Option<String>,
    #[serde(default = "default_tool_threshold_color_on")]
    pub tool_warning_color_on: bool,
    #[serde(default = "default_tool_threshold_color_on")]
    pub tool_danger_color_on: bool,
    #[serde(default)]
    pub claude_text_color: Option<String>,
    #[serde(default)]
    pub claude_text_color_on: bool,
    #[serde(default)]
    pub codex_text_color: Option<String>,
    #[serde(default)]
    pub codex_text_color_on: bool,
    #[serde(default)]
    pub grok_text_color: Option<String>,
    #[serde(default)]
    pub grok_text_color_on: bool,
    #[serde(default)]
    pub cursor_text_color: Option<String>,
    #[serde(default)]
    pub cursor_text_color_on: bool,
    #[serde(default)]
    pub info_text_color: Option<String>,
    #[serde(default)]
    pub info_text_color_on: bool,
    #[serde(default)]
    pub ring_text_color: Option<String>,
    #[serde(default)]
    pub ring_text_color_on: bool,
}

const WRAP_META_VERSION: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
struct WrapMeta {
    #[serde(default)]
    version: u32,
    managed_command: String,
    original_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_status_line: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_status_line_present: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_status_line: Option<serde_json::Value>,
}

impl WrapMeta {
    fn managed_status_line(&self) -> serde_json::Value {
        self.managed_status_line.clone().unwrap_or_else(|| {
            serde_json::json!({
                "type": "command",
                "command": self.managed_command,
            })
        })
    }

    fn original_status_line(&self) -> (bool, Option<serde_json::Value>) {
        if self.version >= WRAP_META_VERSION || self.original_status_line_present.is_some() {
            let present = self.original_status_line_present.unwrap_or(false);
            return (
                present,
                present.then(|| {
                    self.original_status_line
                        .clone()
                        .unwrap_or(serde_json::Value::Null)
                }),
            );
        }

        match self.original_command.as_ref() {
            Some(command) => (
                true,
                Some(serde_json::json!({
                    "type": "command",
                    "command": command,
                })),
            ),
            None => (false, None),
        }
    }
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
    60
}

fn default_display_basis() -> String {
    "remaining".into()
}

fn default_stale() -> i64 {
    90
}

fn default_activity_weeks() -> u16 {
    52
}

fn default_activity_scale_mode() -> String {
    "auto".into()
}

fn default_activity_tokens_per_level() -> u64 {
    250_000
}

fn default_bar_mode() -> String {
    "full".into()
}

fn default_full_reset_time_on() -> bool {
    true
}

fn default_limit_order() -> String {
    "primary_first".into()
}

fn default_fullscreen_hide_on() -> bool {
    false
}

fn legacy_fullscreen_hide_on() -> bool {
    true
}

fn default_maximized_hide_on() -> bool {
    false
}

fn default_taskbar_avoid_overlap_on() -> bool {
    true
}

fn default_taskbar_layout_memory_on() -> bool {
    true
}

fn default_taskbar_profile_presentation_on() -> bool {
    true
}

fn default_taskbar_profile_colors_on() -> bool {
    false
}

fn default_indicator_style() -> String {
    "ring".into()
}

fn default_indicator_effect_style() -> String {
    "flat".into()
}

fn default_indicator_track_color_auto() -> bool {
    true
}

fn default_indicator_track_color() -> [u8; 3] {
    [0x6b, 0x72, 0x80]
}

fn default_indicator_track_opacity_percent() -> f32 {
    11.0
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

fn default_ring_center_size_px() -> f32 {
    16.0
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

fn default_bar_content_gap_px() -> f32 {
    14.0
}

fn default_autostart_on() -> bool {
    true
}

fn default_update_check_on() -> bool {
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

fn initial_taskbar_offset_ratio() -> f32 {
    0.0
}

fn default_show_tool() -> bool {
    true
}

fn default_claude_account_auto_collect_on() -> bool {
    true
}

fn default_tool_threshold_color_on() -> bool {
    true
}

fn normalize_taskbar_layout_profile(
    mut profile: TaskbarLayoutProfile,
) -> Option<TaskbarLayoutProfile> {
    profile.monitor_keys = canonical_taskbar_monitor_keys(profile.monitor_keys);
    if profile.monitor_keys.is_empty() {
        return None;
    }

    let normalize_placement = |placement: Option<TaskbarPlacement>| {
        placement.and_then(|mut placement| {
            if !profile.monitor_keys.contains(&placement.monitor_key) {
                return None;
            }
            placement.offset_ratio = clamp_ratio(placement.offset_ratio);
            Some(placement)
        })
    };
    profile.claude = normalize_placement(profile.claude);
    profile.codex = normalize_placement(profile.codex);
    profile.grok = normalize_placement(profile.grok);
    profile.cursor = normalize_placement(profile.cursor);
    if let Some(presentation) = &mut profile.presentation {
        presentation.bar_mode = normalize_bar_mode(&presentation.bar_mode).into();
        presentation.limit_order = normalize_limit_order(&presentation.limit_order).into();
        presentation.indicator_style =
            normalize_indicator_style(&presentation.indicator_style).into();
        presentation.indicator_effect_style =
            normalize_indicator_effect_style(&presentation.indicator_effect_style).into();
        presentation.ring_number_outline_width_px =
            clamp_ring_number_outline_width(presentation.ring_number_outline_width_px);
        presentation.ring_size_px = clamp_px(
            presentation.ring_size_px,
            default_ring_size_px(),
            20.0,
            44.0,
        );
        presentation.ring_thickness_px = clamp_ring_thickness(presentation.ring_thickness_px);
        presentation.ring_gap_px = clamp_ring_gap(presentation.ring_gap_px);
        presentation.ring_center_size_px = clamp_ring_center_size(presentation.ring_center_size_px);
        presentation.ring_number_font_size_px = clamp_px(
            presentation.ring_number_font_size_px,
            default_ring_number_font_size_px(),
            6.0,
            16.0,
        );
        presentation.ring_number_font_weight =
            clamp_font_weight(presentation.ring_number_font_weight);
        presentation.bar_text_font_size_px = clamp_px(
            presentation.bar_text_font_size_px,
            default_bar_text_font_size_px(),
            8.0,
            16.0,
        );
        presentation.bar_text_font_weight = clamp_font_weight(presentation.bar_text_font_weight);
        presentation.bar_content_gap_px = clamp_px(
            presentation.bar_content_gap_px,
            default_bar_content_gap_px(),
            0.0,
            24.0,
        );
    }
    if let Some(appearance) = &mut profile.appearance {
        appearance.indicator_track_opacity_percent = clamp_px(
            appearance.indicator_track_opacity_percent,
            default_indicator_track_opacity_percent(),
            0.0,
            100.0,
        );
    }
    (profile.claude.is_some()
        || profile.codex.is_some()
        || profile.grok.is_some()
        || profile.cursor.is_some())
    .then_some(profile)
}

pub fn canonical_taskbar_monitor_keys(mut monitor_keys: Vec<String>) -> Vec<String> {
    monitor_keys.retain(|key| !key.is_empty() && key.len() <= MAX_TASKBAR_MONITOR_KEY_LEN);
    monitor_keys.sort();
    monitor_keys.dedup();
    monitor_keys.truncate(MAX_TASKBAR_PROFILE_MONITORS);
    monitor_keys
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            palette: default_palette(),
            tool_colors: ToolColors::default(),
            taskbar_text_colors: TaskbarTextColors::default(),
            warn_threshold: default_warn(),
            danger_threshold: default_danger(),
            display_basis: default_display_basis(),
            poll_interval_secs: default_interval(),
            stale_after_secs: default_stale(),
            activity_weeks: default_activity_weeks(),
            activity_scale_mode: default_activity_scale_mode(),
            activity_tokens_per_level: default_activity_tokens_per_level(),
            bar_mode: default_bar_mode(),
            full_reset_time_on: default_full_reset_time_on(),
            limit_order: default_limit_order(),
            fullscreen_hide_on: default_fullscreen_hide_on(),
            maximized_hide_on: default_maximized_hide_on(),
            taskbar_avoid_overlap_on: default_taskbar_avoid_overlap_on(),
            taskbar_bars_paused: false,
            taskbar_layout_memory_on: default_taskbar_layout_memory_on(),
            taskbar_profile_presentation_on: default_taskbar_profile_presentation_on(),
            taskbar_profile_colors_on: default_taskbar_profile_colors_on(),
            taskbar_layout_profiles: Vec::new(),
            taskbar_layout_memory_initialized: false,
            indicator_style: default_indicator_style(),
            indicator_effect_style: default_indicator_effect_style(),
            indicator_track_color_auto: default_indicator_track_color_auto(),
            indicator_track_color: default_indicator_track_color(),
            indicator_track_opacity_percent: default_indicator_track_opacity_percent(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_size_px: default_ring_center_size_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            bar_content_gap_px: default_bar_content_gap_px(),
            autostart_on: default_autostart_on(),
            update_check_on: default_update_check_on(),
            language: default_language(),
            theme: default_theme(),
            font_mode: default_font_mode(),
            taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            claude_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            codex_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            grok_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            cursor_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            claude_taskbar_monitor_key: String::new(),
            codex_taskbar_monitor_key: String::new(),
            grok_taskbar_monitor_key: String::new(),
            cursor_taskbar_monitor_key: String::new(),
            claude_taskbar_target_initialized: false,
            codex_taskbar_target_initialized: false,
            grok_taskbar_target_initialized: false,
            cursor_taskbar_target_initialized: false,
            show_claude: default_show_tool(),
            show_codex: default_show_tool(),
            show_grok: false,
            show_cursor: false,
            claude_account_auto_collect_on: default_claude_account_auto_collect_on(),
        }
    }
}

impl Default for SettingsInput {
    fn default() -> Self {
        Self {
            palette: default_palette_name(),
            warn_threshold: default_warn(),
            danger_threshold: default_danger(),
            display_basis: default_display_basis(),
            poll_interval_secs: default_interval(),
            stale_after_secs: default_stale(),
            activity_weeks: default_activity_weeks(),
            activity_scale_mode: default_activity_scale_mode(),
            activity_tokens_per_level: default_activity_tokens_per_level(),
            bar_mode: default_bar_mode(),
            full_reset_time_on: default_full_reset_time_on(),
            limit_order: default_limit_order(),
            fullscreen_hide_on: default_fullscreen_hide_on(),
            maximized_hide_on: default_maximized_hide_on(),
            taskbar_avoid_overlap_on: default_taskbar_avoid_overlap_on(),
            taskbar_layout_memory_on: default_taskbar_layout_memory_on(),
            taskbar_profile_presentation_on: default_taskbar_profile_presentation_on(),
            taskbar_profile_colors_on: default_taskbar_profile_colors_on(),
            indicator_style: default_indicator_style(),
            indicator_effect_style: default_indicator_effect_style(),
            indicator_track_color_auto: default_indicator_track_color_auto(),
            indicator_track_color: None,
            indicator_track_opacity_percent: default_indicator_track_opacity_percent(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_size_px: default_ring_center_size_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            bar_content_gap_px: default_bar_content_gap_px(),
            autostart_on: default_autostart_on(),
            update_check_on: default_update_check_on(),
            language: default_language(),
            theme: default_theme(),
            font_mode: default_font_mode(),
            taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            claude_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            codex_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            grok_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            cursor_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            claude_taskbar_monitor_key: String::new(),
            codex_taskbar_monitor_key: String::new(),
            grok_taskbar_monitor_key: String::new(),
            cursor_taskbar_monitor_key: String::new(),
            show_claude: default_show_tool(),
            show_codex: default_show_tool(),
            show_grok: false,
            show_cursor: false,
            claude_account_auto_collect_on: default_claude_account_auto_collect_on(),
            mono_color: None,
            custom_safe: None,
            custom_warn: None,
            custom_danger: None,
            claude_primary_color: None,
            claude_secondary_color: None,
            codex_primary_color: None,
            codex_secondary_color: None,
            grok_primary_color: None,
            grok_secondary_color: None,
            cursor_primary_color: None,
            cursor_secondary_color: None,
            tool_warning_color: None,
            tool_danger_color: None,
            tool_warning_color_on: default_tool_threshold_color_on(),
            tool_danger_color_on: default_tool_threshold_color_on(),
            claude_text_color: None,
            claude_text_color_on: false,
            codex_text_color: None,
            codex_text_color_on: false,
            grok_text_color: None,
            grok_text_color_on: false,
            cursor_text_color: None,
            cursor_text_color_on: false,
            info_text_color: None,
            info_text_color_on: false,
            ring_text_color: None,
            ring_text_color_on: false,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        paths::data_dir().map(|dir| dir.join("settings.json"))
    }

    fn agent_dir() -> Option<PathBuf> {
        paths::data_dir()
    }

    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |path| Self::load_from(&path))
    }

    pub fn try_load() -> anyhow::Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        Self::try_load_from(&path)
    }

    pub fn load_with_revision() -> (Self, Option<(u64, SystemTime)>) {
        let Some(path) = Self::path() else {
            return (Self::default(), None);
        };
        Self::load_with_revision_at(&path)
    }

    pub fn try_load_with_revision() -> anyhow::Result<(Self, Option<(u64, SystemTime)>)> {
        let Some(path) = Self::path() else {
            return Ok((Self::default(), None));
        };
        Self::try_load_with_revision_at(&path)
    }

    pub fn try_load_with_revision_at(
        path: &Path,
    ) -> anyhow::Result<(Self, Option<(u64, SystemTime)>)> {
        let _guard = SETTINGS_UPDATE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        for _ in 0..8 {
            let before = Self::storage_revision_at(path);
            let settings = Self::try_load_from(path)?;
            let after = Self::storage_revision_at(path);
            if before == after {
                return Ok((settings, after));
            }
        }
        Ok((Self::try_load_from(path)?, Self::storage_revision_at(path)))
    }

    pub fn load_with_revision_at(path: &Path) -> (Self, Option<(u64, SystemTime)>) {
        Self::load_with_revision_at_with_hook(path, || {})
    }

    fn load_with_revision_at_with_hook(
        path: &Path,
        mut after_load: impl FnMut(),
    ) -> (Self, Option<(u64, SystemTime)>) {
        let _guard = SETTINGS_UPDATE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        for _ in 0..8 {
            let before = Self::storage_revision_at(path);
            let settings = Self::load_from(path);
            after_load();
            let after = Self::storage_revision_at(path);
            if before == after {
                return (settings, after);
            }
        }
        (Self::load_from(path), Self::storage_revision_at(path))
    }

    pub fn storage_revision() -> Option<(u64, SystemTime)> {
        Self::path().as_deref().and_then(Self::storage_revision_at)
    }

    pub fn storage_revision_at(path: &Path) -> Option<(u64, SystemTime)> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some((metadata.len(), metadata.modified().ok()?))
    }

    pub fn load_from(path: &Path) -> Self {
        Self::try_load_from(path).unwrap_or_default()
    }

    pub fn try_load_from(path: &Path) -> anyhow::Result<Self> {
        let contents = match std::fs::read(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.into()),
        };
        let value = serde_json::from_slice::<serde_json::Value>(&contents)?;
        if !value.is_object() {
            anyhow::bail!("settings root must be a JSON object");
        }
        let settings = serde_json::from_value::<Self>(value.clone())?;
        Ok(Self::normalize_loaded(settings, Some(&value)))
    }

    fn load_for_update(path: &Path) -> anyhow::Result<Self> {
        let contents = match std::fs::read(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(Self::normalize_loaded(Self::default(), None));
            }
            Err(err) => return Err(err.into()),
        };
        let value = serde_json::from_slice::<serde_json::Value>(&contents)?;
        if !value.is_object() {
            anyhow::bail!("settings root must be a JSON object");
        }
        let settings = serde_json::from_value::<Self>(value.clone())?;
        Ok(Self::normalize_loaded(settings, Some(&value)))
    }

    fn normalize_loaded(mut settings: Self, value: Option<&serde_json::Value>) -> Self {
        settings.apply_legacy_taskbar_offset(value);
        settings.apply_legacy_taskbar_target_state(value);
        settings.apply_legacy_collection_interval(value);
        settings.apply_legacy_ring_center_size(value);
        settings.apply_legacy_tool_colors(value);
        settings.clamp_offsets();
        settings.normalize_taskbar_layout_profiles();
        settings.clamp_ring_geometry();
        settings.display_basis = normalize_display_basis(&settings.display_basis).into();
        settings.limit_order = normalize_limit_order(&settings.limit_order).into();
        settings.indicator_style = normalize_indicator_style(&settings.indicator_style).into();
        settings.indicator_effect_style =
            normalize_indicator_effect_style(&settings.indicator_effect_style).into();
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

    pub fn update_at(path: &Path, mutator: impl FnOnce(&mut Self)) -> anyhow::Result<Self> {
        let _guard = SETTINGS_UPDATE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let mut settings = Self::load_for_update(path)?;
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
            display_basis: default_display_basis(),
            poll_interval_secs: interval,
            stale_after_secs: default_stale(),
            activity_weeks: default_activity_weeks(),
            activity_scale_mode: default_activity_scale_mode(),
            activity_tokens_per_level: default_activity_tokens_per_level(),
            bar_mode: default_bar_mode(),
            full_reset_time_on: default_full_reset_time_on(),
            limit_order: default_limit_order(),
            fullscreen_hide_on: default_fullscreen_hide_on(),
            maximized_hide_on: default_maximized_hide_on(),
            taskbar_avoid_overlap_on: default_taskbar_avoid_overlap_on(),
            taskbar_layout_memory_on: default_taskbar_layout_memory_on(),
            taskbar_profile_presentation_on: default_taskbar_profile_presentation_on(),
            taskbar_profile_colors_on: default_taskbar_profile_colors_on(),
            indicator_style: default_indicator_style(),
            indicator_effect_style: default_indicator_effect_style(),
            indicator_track_color_auto: default_indicator_track_color_auto(),
            indicator_track_color: None,
            indicator_track_opacity_percent: default_indicator_track_opacity_percent(),
            ring_on: default_ring_on(),
            ring_numbers_on: default_ring_numbers_on(),
            ring_number_outline_on: default_ring_number_outline_on(),
            ring_number_outline_width_px: default_ring_number_outline_width_px(),
            ring_size_px: default_ring_size_px(),
            ring_thickness_px: default_ring_thickness_px(),
            ring_gap_px: default_ring_gap_px(),
            ring_center_size_px: default_ring_center_size_px(),
            ring_number_font_size_px: default_ring_number_font_size_px(),
            ring_number_font_weight: default_ring_number_font_weight(),
            bar_text_font_size_px: default_bar_text_font_size_px(),
            bar_text_font_weight: default_bar_text_font_weight(),
            bar_content_gap_px: default_bar_content_gap_px(),
            autostart_on: default_autostart_on(),
            update_check_on: default_update_check_on(),
            language: default_language(),
            theme: default_theme(),
            font_mode: default_font_mode(),
            taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            claude_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            codex_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            grok_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            cursor_taskbar_offset_ratio: initial_taskbar_offset_ratio(),
            claude_taskbar_monitor_key: String::new(),
            codex_taskbar_monitor_key: String::new(),
            grok_taskbar_monitor_key: String::new(),
            cursor_taskbar_monitor_key: String::new(),
            show_claude: default_show_tool(),
            show_codex: default_show_tool(),
            show_grok: false,
            show_cursor: false,
            claude_account_auto_collect_on: default_claude_account_auto_collect_on(),
            mono_color: None,
            custom_safe: None,
            custom_warn: None,
            custom_danger: None,
            claude_primary_color: None,
            claude_secondary_color: None,
            codex_primary_color: None,
            codex_secondary_color: None,
            grok_primary_color: None,
            grok_secondary_color: None,
            cursor_primary_color: None,
            cursor_secondary_color: None,
            tool_warning_color: None,
            tool_danger_color: None,
            tool_warning_color_on: default_tool_threshold_color_on(),
            tool_danger_color_on: default_tool_threshold_color_on(),
            claude_text_color: None,
            claude_text_color_on: false,
            codex_text_color: None,
            codex_text_color_on: false,
            grok_text_color: None,
            grok_text_color_on: false,
            cursor_text_color: None,
            cursor_text_color_on: false,
            info_text_color: None,
            info_text_color_on: false,
            ring_text_color: None,
            ring_text_color_on: false,
        })
    }

    pub fn from_input(input: SettingsInput) -> Self {
        let warn = clamp_percent(input.warn_threshold);
        let danger = clamp_percent(input.danger_threshold).max(warn);
        Self {
            palette: palette_from_input(&input),
            tool_colors: tool_colors_from_input(&input),
            taskbar_text_colors: taskbar_text_colors_from_input(&input),
            warn_threshold: warn,
            danger_threshold: danger,
            display_basis: normalize_display_basis(&input.display_basis).into(),
            poll_interval_secs: input.poll_interval_secs.max(1),
            stale_after_secs: input.stale_after_secs.max(1),
            activity_weeks: input.activity_weeks.clamp(4, 52),
            activity_scale_mode: normalize_activity_scale_mode(&input.activity_scale_mode).into(),
            activity_tokens_per_level: input.activity_tokens_per_level.clamp(1, 1_000_000_000_000),
            bar_mode: normalize_bar_mode(&input.bar_mode).into(),
            full_reset_time_on: input.full_reset_time_on,
            limit_order: normalize_limit_order(&input.limit_order).into(),
            fullscreen_hide_on: input.fullscreen_hide_on,
            maximized_hide_on: input.maximized_hide_on,
            taskbar_avoid_overlap_on: input.taskbar_avoid_overlap_on,
            taskbar_bars_paused: false,
            taskbar_layout_memory_on: input.taskbar_layout_memory_on,
            taskbar_profile_presentation_on: input.taskbar_profile_presentation_on,
            taskbar_profile_colors_on: input.taskbar_profile_colors_on,
            taskbar_layout_profiles: Vec::new(),
            taskbar_layout_memory_initialized: false,
            indicator_style: normalize_indicator_style(&input.indicator_style).into(),
            indicator_effect_style: normalize_indicator_effect_style(&input.indicator_effect_style)
                .into(),
            indicator_track_color_auto: input.indicator_track_color_auto,
            indicator_track_color: parse_hex_rgb(input.indicator_track_color.as_deref())
                .unwrap_or_else(default_indicator_track_color),
            indicator_track_opacity_percent: clamp_px(
                input.indicator_track_opacity_percent,
                default_indicator_track_opacity_percent(),
                0.0,
                100.0,
            ),
            ring_on: input.ring_on,
            ring_numbers_on: input.ring_numbers_on,
            ring_number_outline_on: input.ring_number_outline_on,
            ring_number_outline_width_px: clamp_ring_number_outline_width(
                input.ring_number_outline_width_px,
            ),
            ring_size_px: clamp_px(input.ring_size_px, default_ring_size_px(), 20.0, 44.0),
            ring_thickness_px: clamp_ring_thickness(input.ring_thickness_px),
            ring_gap_px: clamp_ring_gap(input.ring_gap_px),
            ring_center_size_px: clamp_ring_center_size(input.ring_center_size_px),
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
            bar_content_gap_px: clamp_px(
                input.bar_content_gap_px,
                default_bar_content_gap_px(),
                0.0,
                24.0,
            ),
            autostart_on: input.autostart_on,
            update_check_on: input.update_check_on,
            language: normalize_language(&input.language).into(),
            theme: normalize_theme(&input.theme).into(),
            font_mode: normalize_font_mode(&input.font_mode).into(),
            taskbar_offset_ratio: clamp_ratio(input.taskbar_offset_ratio),
            claude_taskbar_offset_ratio: clamp_ratio(input.claude_taskbar_offset_ratio),
            codex_taskbar_offset_ratio: clamp_ratio(input.codex_taskbar_offset_ratio),
            grok_taskbar_offset_ratio: clamp_ratio(input.grok_taskbar_offset_ratio),
            cursor_taskbar_offset_ratio: clamp_ratio(input.cursor_taskbar_offset_ratio),
            claude_taskbar_monitor_key: input.claude_taskbar_monitor_key,
            codex_taskbar_monitor_key: input.codex_taskbar_monitor_key,
            grok_taskbar_monitor_key: input.grok_taskbar_monitor_key,
            cursor_taskbar_monitor_key: input.cursor_taskbar_monitor_key,
            claude_taskbar_target_initialized: false,
            codex_taskbar_target_initialized: false,
            grok_taskbar_target_initialized: false,
            cursor_taskbar_target_initialized: false,
            show_claude: input.show_claude,
            show_codex: input.show_codex,
            show_grok: input.show_grok,
            show_cursor: input.show_cursor,
            claude_account_auto_collect_on: input.claude_account_auto_collect_on,
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
        if !json_has_field(value, "grok_taskbar_offset_ratio") {
            self.grok_taskbar_offset_ratio = legacy;
        }
        if !json_has_field(value, "cursor_taskbar_offset_ratio") {
            self.cursor_taskbar_offset_ratio = legacy;
        }
    }

    fn apply_legacy_taskbar_target_state(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value.filter(|value| value.is_object()) else {
            return;
        };
        if !json_has_field(value, "claude_taskbar_target_initialized") {
            self.claude_taskbar_target_initialized = true;
        }
        if !json_has_field(value, "codex_taskbar_target_initialized") {
            self.codex_taskbar_target_initialized = true;
        }
        if !json_has_field(value, "grok_taskbar_target_initialized") {
            self.grok_taskbar_target_initialized = false;
        }
        if !json_has_field(value, "cursor_taskbar_target_initialized") {
            self.cursor_taskbar_target_initialized = false;
        }
    }

    fn apply_legacy_collection_interval(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value else {
            return;
        };
        let is_legacy = !json_has_field(value, "display_basis");
        let used_old_default = value
            .get("poll_interval_secs")
            .and_then(|item| item.as_u64())
            == Some(2);
        if is_legacy && used_old_default {
            self.poll_interval_secs = default_interval();
        }
    }

    fn apply_legacy_ring_center_size(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value else {
            return;
        };
        if json_has_field(value, "ring_center_size_px") {
            return;
        }

        let size = json_f32_field(value, "ring_size_px").unwrap_or(default_ring_size_px());
        let thickness =
            json_f32_field(value, "ring_thickness_px").unwrap_or(default_ring_thickness_px());
        let gap = json_f32_field(value, "ring_gap_px").unwrap_or(default_ring_gap_px());
        let legacy_gap = json_f32_field(value, "ring_center_gap_px").unwrap_or(0.0);
        let visible_thickness = (thickness - legacy_gap).max(1.0);
        self.ring_center_size_px =
            clamp_ring_center_size(size - 2.0 * gap - 2.0 * visible_thickness);
    }

    fn apply_legacy_tool_colors(&mut self, value: Option<&serde_json::Value>) {
        fn migrate_cursor_tool_defaults(colors: &mut ToolColors) {
            let previous_default = (colors.cursor_primary == V0_1_14_CURSOR_PRIMARY
                && colors.cursor_secondary == V0_1_14_CURSOR_SECONDARY)
                || (colors.cursor_primary == V0_1_19_CURSOR_PRIMARY
                    && colors.cursor_secondary == V0_1_19_CURSOR_SECONDARY);
            if previous_default {
                colors.cursor_primary = DEFAULT_CURSOR_PRIMARY;
                colors.cursor_secondary = DEFAULT_CURSOR_SECONDARY;
            }
        }

        fn migrate_cursor_text_default(colors: &mut TaskbarTextColors) {
            if !colors.cursor_on
                && (colors.cursor == V0_1_14_CURSOR_PRIMARY
                    || colors.cursor == V0_1_19_CURSOR_PRIMARY)
            {
                colors.cursor = DEFAULT_CURSOR_PRIMARY;
            }
        }

        let Some(value) = value else {
            return;
        };
        if json_has_field(value, "tool_colors") {
            let legacy_with_v0_1_14_cursor = ToolColors {
                cursor_primary: V0_1_14_CURSOR_PRIMARY,
                cursor_secondary: V0_1_14_CURSOR_SECONDARY,
                ..LEGACY_DEFAULT_TOOL_COLORS
            };
            let legacy_with_v0_1_19_cursor = ToolColors {
                cursor_primary: V0_1_19_CURSOR_PRIMARY,
                cursor_secondary: V0_1_19_CURSOR_SECONDARY,
                ..LEGACY_DEFAULT_TOOL_COLORS
            };
            if self.tool_colors == LEGACY_DEFAULT_TOOL_COLORS
                || self.tool_colors == legacy_with_v0_1_14_cursor
                || self.tool_colors == legacy_with_v0_1_19_cursor
            {
                self.tool_colors = ToolColors::default();
            } else {
                migrate_cursor_tool_defaults(&mut self.tool_colors);
            }
        }
        if json_has_field(value, "taskbar_text_colors") {
            migrate_cursor_text_default(&mut self.taskbar_text_colors);
        }
        for appearance in self
            .taskbar_layout_profiles
            .iter_mut()
            .filter_map(|profile| profile.appearance.as_mut())
        {
            migrate_cursor_tool_defaults(&mut appearance.tool_colors);
            migrate_cursor_text_default(&mut appearance.taskbar_text_colors);
        }
    }

    fn clamp_offsets(&mut self) {
        self.taskbar_offset_ratio = clamp_ratio(self.taskbar_offset_ratio);
        self.claude_taskbar_offset_ratio = clamp_ratio(self.claude_taskbar_offset_ratio);
        self.codex_taskbar_offset_ratio = clamp_ratio(self.codex_taskbar_offset_ratio);
        self.grok_taskbar_offset_ratio = clamp_ratio(self.grok_taskbar_offset_ratio);
        self.cursor_taskbar_offset_ratio = clamp_ratio(self.cursor_taskbar_offset_ratio);
    }

    fn normalize_taskbar_layout_profiles(&mut self) {
        let mut normalized = Vec::new();
        for profile in std::mem::take(&mut self.taskbar_layout_profiles) {
            let Some(mut profile) = normalize_taskbar_layout_profile(profile) else {
                continue;
            };
            if let Some(index) = normalized
                .iter()
                .position(|saved: &TaskbarLayoutProfile| saved.monitor_keys == profile.monitor_keys)
            {
                let previous = normalized.remove(index);
                if profile.claude.is_none() {
                    profile.claude = previous.claude;
                }
                if profile.codex.is_none() {
                    profile.codex = previous.codex;
                }
                if profile.grok.is_none() {
                    profile.grok = previous.grok;
                }
                if profile.cursor.is_none() {
                    profile.cursor = previous.cursor;
                }
                if profile.presentation.is_none() {
                    profile.presentation = previous.presentation;
                }
                if profile.appearance.is_none() {
                    profile.appearance = previous.appearance;
                }
            }
            normalized.push(profile);
        }
        if normalized.len() > MAX_TASKBAR_LAYOUT_PROFILES {
            normalized.drain(..normalized.len() - MAX_TASKBAR_LAYOUT_PROFILES);
        }
        self.taskbar_layout_profiles = normalized;
    }

    pub fn taskbar_layout_profile(&self, monitor_keys: &[String]) -> Option<&TaskbarLayoutProfile> {
        self.taskbar_layout_profiles
            .iter()
            .find(|profile| profile.monitor_keys == monitor_keys)
    }

    pub fn taskbar_layout_profile_is_most_recent(&self, monitor_keys: &[String]) -> bool {
        self.taskbar_layout_profiles
            .last()
            .is_some_and(|profile| profile.monitor_keys == monitor_keys)
    }

    pub fn touch_taskbar_layout_profile(&mut self, monitor_keys: &[String]) -> bool {
        let Some(index) = self
            .taskbar_layout_profiles
            .iter()
            .position(|profile| profile.monitor_keys == monitor_keys)
        else {
            return false;
        };
        if index + 1 == self.taskbar_layout_profiles.len() {
            return false;
        }
        let profile = self.taskbar_layout_profiles.remove(index);
        self.taskbar_layout_profiles.push(profile);
        true
    }

    pub fn upsert_taskbar_layout_profile(&mut self, profile: TaskbarLayoutProfile) -> bool {
        let Some(mut profile) = normalize_taskbar_layout_profile(profile) else {
            return false;
        };
        if let Some(index) = self
            .taskbar_layout_profiles
            .iter()
            .position(|saved| saved.monitor_keys == profile.monitor_keys)
        {
            let previous = self.taskbar_layout_profiles.remove(index);
            if profile.claude.is_none() {
                profile.claude = previous.claude;
            }
            if profile.codex.is_none() {
                profile.codex = previous.codex;
            }
            if profile.grok.is_none() {
                profile.grok = previous.grok;
            }
            if profile.cursor.is_none() {
                profile.cursor = previous.cursor;
            }
            if profile.presentation.is_none() {
                profile.presentation = previous.presentation;
            }
            if profile.appearance.is_none() {
                profile.appearance = previous.appearance;
            }
        }
        self.taskbar_layout_profiles.push(profile);
        if self.taskbar_layout_profiles.len() > MAX_TASKBAR_LAYOUT_PROFILES {
            self.taskbar_layout_profiles
                .drain(..self.taskbar_layout_profiles.len() - MAX_TASKBAR_LAYOUT_PROFILES);
        }
        true
    }

    pub fn migrate_taskbar_monitor_key_aliases(
        &mut self,
        replacements: &[(String, String)],
    ) -> bool {
        fn replace_key(key: &mut String, replacements: &[(String, String)]) {
            let Some((_, stable)) = replacements
                .iter()
                .find(|(alias, stable)| key == alias && alias != stable)
            else {
                return;
            };
            *key = stable.clone();
        }

        let before = (
            self.claude_taskbar_monitor_key.clone(),
            self.codex_taskbar_monitor_key.clone(),
            self.grok_taskbar_monitor_key.clone(),
            self.cursor_taskbar_monitor_key.clone(),
            self.taskbar_layout_profiles.clone(),
        );

        if self.claude_taskbar_target_initialized || !self.claude_taskbar_monitor_key.is_empty() {
            replace_key(&mut self.claude_taskbar_monitor_key, replacements);
        }
        if self.codex_taskbar_target_initialized || !self.codex_taskbar_monitor_key.is_empty() {
            replace_key(&mut self.codex_taskbar_monitor_key, replacements);
        }
        if self.grok_taskbar_target_initialized || !self.grok_taskbar_monitor_key.is_empty() {
            replace_key(&mut self.grok_taskbar_monitor_key, replacements);
        }
        if self.cursor_taskbar_target_initialized || !self.cursor_taskbar_monitor_key.is_empty() {
            replace_key(&mut self.cursor_taskbar_monitor_key, replacements);
        }
        for profile in &mut self.taskbar_layout_profiles {
            for key in &mut profile.monitor_keys {
                if !key.is_empty() {
                    replace_key(key, replacements);
                }
            }
            if let Some(placement) = &mut profile.claude {
                if !placement.monitor_key.is_empty() {
                    replace_key(&mut placement.monitor_key, replacements);
                }
            }
            if let Some(placement) = &mut profile.codex {
                if !placement.monitor_key.is_empty() {
                    replace_key(&mut placement.monitor_key, replacements);
                }
            }
            if let Some(placement) = &mut profile.grok {
                if !placement.monitor_key.is_empty() {
                    replace_key(&mut placement.monitor_key, replacements);
                }
            }
            if let Some(placement) = &mut profile.cursor {
                if !placement.monitor_key.is_empty() {
                    replace_key(&mut placement.monitor_key, replacements);
                }
            }
        }
        self.normalize_taskbar_layout_profiles();

        before
            != (
                self.claude_taskbar_monitor_key.clone(),
                self.codex_taskbar_monitor_key.clone(),
                self.grok_taskbar_monitor_key.clone(),
                self.cursor_taskbar_monitor_key.clone(),
                self.taskbar_layout_profiles.clone(),
            )
    }

    fn clamp_ring_geometry(&mut self) {
        self.indicator_track_opacity_percent = clamp_px(
            self.indicator_track_opacity_percent,
            default_indicator_track_opacity_percent(),
            0.0,
            100.0,
        );
        self.ring_size_px = clamp_px(self.ring_size_px, default_ring_size_px(), 20.0, 44.0);
        self.ring_thickness_px = clamp_ring_thickness(self.ring_thickness_px);
        self.ring_gap_px = clamp_ring_gap(self.ring_gap_px);
        self.ring_center_size_px = clamp_ring_center_size(self.ring_center_size_px);
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
        self.bar_content_gap_px = clamp_px(
            self.bar_content_gap_px,
            default_bar_content_gap_px(),
            0.0,
            24.0,
        );
    }

    pub fn install_statusline_wrap(bridge_abs: &str) -> anyhow::Result<()> {
        let home = claude_home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let agent_dir = Self::agent_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
        Self::install_statusline_wrap_at(&home, &agent_dir, bridge_abs)
    }

    pub fn install_statusline_wrap_at(
        home: &Path,
        agent_dir: &Path,
        bridge_abs: &str,
    ) -> anyhow::Result<()> {
        Self::install_statusline_wrap_at_with_hook(home, agent_dir, bridge_abs, |_| {})
    }

    fn install_statusline_wrap_at_with_hook(
        home: &Path,
        agent_dir: &Path,
        bridge_abs: &str,
        before_settings_patch: impl FnOnce(&Path),
    ) -> anyhow::Result<()> {
        let settings_path = claude_settings_path(home);
        let bridge = bridge_abs.replace('\\', "/");
        let managed_command = command_for_bridge(&bridge);
        let value = load_optional_json_object(&settings_path)?;
        let current_status_line = value.get("statusLine").cloned();
        let current_present = value.contains_key("statusLine");

        let wrap_path = agent_dir.join("wrap.json");
        let meta_path = agent_dir.join("wrap-meta.json");
        let previous_meta = if meta_path.try_exists()? {
            Some(read_wrap_meta(&meta_path)?)
        } else {
            None
        };
        let (original_status_line_present, original_status_line) = match previous_meta.as_ref() {
            Some(meta) => {
                let managed_status_line = meta.managed_status_line();
                let (original_present, original_status_line) = meta.original_status_line();
                let already_restored = current_present == original_present
                    && current_status_line.as_ref() == original_status_line.as_ref();
                if current_status_line.as_ref() == Some(&managed_status_line)
                    || already_restored
                    || current_status_line
                        .as_ref()
                        .is_some_and(is_agentjuice_status_line)
                {
                    (original_present, original_status_line)
                } else {
                    anyhow::bail!("Claude statusLine ownership metadata does not match");
                }
            }
            None if current_status_line.as_ref().is_some_and(|status_line| {
                is_verified_agentjuice_status_line(status_line, &bridge)
            }) =>
            {
                (false, None)
            }
            None if current_status_line
                .as_ref()
                .is_some_and(is_agentjuice_status_line) =>
            {
                anyhow::bail!("Claude statusLine ownership metadata does not match");
            }
            None => (current_present, current_status_line.clone()),
        };
        if previous_meta.is_none()
            && current_status_line.as_ref().is_some_and(|status_line| {
                status_line
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(is_agentjuice_command)
                    && !is_agentjuice_status_line(status_line)
            })
        {
            anyhow::bail!("Claude statusLine ownership metadata does not match");
        }
        let original_command = original_status_line
            .as_ref()
            .and_then(|status_line| status_line.get("command"))
            .and_then(serde_json::Value::as_str)
            .and_then(nonempty_trimmed);
        let managed_status_line = serde_json::json!({
            "type": "command",
            "command": managed_command,
        });

        std::fs::create_dir_all(agent_dir)?;
        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if let Some(original) = &original_command {
            replace_file(&wrap_path, original.as_bytes())?;
        } else if wrap_path.exists() {
            let _ = std::fs::remove_file(&wrap_path);
        }

        let backup_path = settings_path.with_extension("json.aj-backup");
        if settings_path.exists() && !backup_path.exists() {
            std::fs::copy(&settings_path, backup_path)?;
        }

        write_wrap_meta(
            &meta_path,
            &WrapMeta {
                version: WRAP_META_VERSION,
                managed_command: managed_command.clone(),
                original_command,
                managed_status_line: Some(managed_status_line.clone()),
                original_status_line_present: Some(original_status_line_present),
                original_status_line,
            },
        )?;
        before_settings_patch(&settings_path);
        patch_status_line_if_unchanged(
            &settings_path,
            current_present,
            current_status_line.as_ref(),
            true,
            Some(managed_status_line),
        )?;
        Ok(())
    }

    pub fn restore_statusline() -> anyhow::Result<()> {
        let home = claude_home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let agent_dir = Self::agent_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
        Self::restore_statusline_at(&home, &agent_dir)
    }

    pub fn restore_statusline_if_installed() -> anyhow::Result<()> {
        let home = claude_home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        let agent_dir = Self::agent_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
        Self::restore_statusline_if_installed_at(&home, &agent_dir)
    }

    pub fn restore_statusline_if_installed_at(home: &Path, agent_dir: &Path) -> anyhow::Result<()> {
        if !agent_dir.join("wrap-meta.json").try_exists()? {
            return Ok(());
        }
        Self::restore_statusline_at(home, agent_dir)
    }

    pub fn restore_statusline_at(home: &Path, agent_dir: &Path) -> anyhow::Result<()> {
        Self::restore_statusline_at_with(home, agent_dir, |_| {}, remove_file_if_exists)
    }

    fn restore_statusline_at_with(
        home: &Path,
        agent_dir: &Path,
        before_settings_patch: impl FnOnce(&Path),
        mut remove: impl FnMut(&Path) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let settings_path = claude_settings_path(home);
        let meta_path = agent_dir.join("wrap-meta.json");
        let wrap_path = agent_dir.join("wrap.json");
        let meta = read_wrap_meta(&meta_path)?;
        let value = load_optional_json_object(&settings_path)?;
        let current_present = value.contains_key("statusLine");
        let current_status_line = value.get("statusLine");
        let managed_status_line = meta.managed_status_line();
        let (original_present, original_status_line) = meta.original_status_line();
        let already_restored = current_present == original_present
            && current_status_line == original_status_line.as_ref();
        if current_status_line == Some(&managed_status_line) {
            before_settings_patch(&settings_path);
            patch_status_line_if_unchanged(
                &settings_path,
                true,
                Some(&managed_status_line),
                original_present,
                original_status_line,
            )?;
        } else if !already_restored {
            anyhow::bail!("Claude statusLine is not managed by Juice or already restored");
        }

        remove(&wrap_path)?;
        remove(&meta_path)?;
        Ok(())
    }
}

fn patch_status_line_if_unchanged(
    settings_path: &Path,
    expected_present: bool,
    expected_status_line: Option<&serde_json::Value>,
    replacement_present: bool,
    replacement_status_line: Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let (mut latest, snapshot) = load_optional_json_object_snapshot(settings_path)?;
    if latest.contains_key("statusLine") != expected_present
        || latest.get("statusLine") != expected_status_line
    {
        anyhow::bail!("Claude statusLine changed during Juice update");
    }
    if replacement_present {
        latest.insert(
            "statusLine".into(),
            replacement_status_line.unwrap_or(serde_json::Value::Null),
        );
    } else {
        latest.remove("statusLine");
    }
    if read_optional_bytes(settings_path)? != snapshot {
        anyhow::bail!("Claude settings changed during Juice update");
    }
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    replace_file(
        settings_path,
        serde_json::to_string_pretty(&latest)?.as_bytes(),
    )?;
    Ok(())
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

fn read_wrap_meta(path: &Path) -> anyhow::Result<WrapMeta> {
    let contents = std::fs::read_to_string(path)?;
    let raw: serde_json::Value = serde_json::from_str(&contents)?;
    let meta: WrapMeta = serde_json::from_value(raw.clone())?;
    if meta.version >= WRAP_META_VERSION {
        let object = raw
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Claude statusLine metadata must be an object"))?;
        if !object
            .get("managed_status_line")
            .is_some_and(serde_json::Value::is_object)
        {
            anyhow::bail!("Claude statusLine metadata has no managed subtree");
        }
        let original_present = object
            .get("original_status_line_present")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("Claude statusLine metadata has no presence flag"))?;
        if original_present && !object.contains_key("original_status_line") {
            anyhow::bail!("Claude statusLine metadata has no original subtree");
        }
    }
    Ok(meta)
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

fn is_agentjuice_status_line(status_line: &serde_json::Value) -> bool {
    let Some(object) = status_line.as_object() else {
        return false;
    };
    object.len() == 2
        && object.get("type").and_then(serde_json::Value::as_str) == Some("command")
        && object
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(is_agentjuice_command)
}

fn is_verified_agentjuice_status_line(status_line: &serde_json::Value, bridge: &str) -> bool {
    let Some(command) = status_line
        .get("command")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    if !is_agentjuice_status_line(status_line) {
        return false;
    }
    let current = PathBuf::from(command.trim().trim_matches('"'));
    let requested = PathBuf::from(bridge);
    let Ok(current_metadata) = std::fs::metadata(&current) else {
        return false;
    };
    let Ok(requested_metadata) = std::fs::metadata(&requested) else {
        return false;
    };
    if !current_metadata.is_file()
        || !requested_metadata.is_file()
        || current_metadata.len() != requested_metadata.len()
    {
        return false;
    }
    std::fs::read(current)
        .and_then(|current| std::fs::read(requested).map(|requested| current == requested))
        .unwrap_or(false)
}

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn claude_home_dir() -> Option<PathBuf> {
    std::env::var_os("AGENT_JUICE_CLAUDE_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn load_optional_json_object(
    path: &Path,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    Ok(load_optional_json_object_snapshot(path)?.0)
}

type JsonObjectSnapshot = (serde_json::Map<String, serde_json::Value>, Option<Vec<u8>>);

fn load_optional_json_object_snapshot(path: &Path) -> anyhow::Result<JsonObjectSnapshot> {
    let contents = read_optional_bytes(path)?;
    let Some(bytes) = contents.as_ref() else {
        return Ok((serde_json::Map::new(), None));
    };
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Claude settings root must be a JSON object"))
        .map(|object| (object, contents))
}

fn read_optional_bytes(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
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

fn normalize_display_basis(value: &str) -> &'static str {
    match value {
        "used" => "used",
        _ => "remaining",
    }
}

fn normalize_activity_scale_mode(value: &str) -> &'static str {
    match value {
        "fixed" => "fixed",
        _ => "auto",
    }
}

fn normalize_indicator_style(value: &str) -> &'static str {
    match value {
        "bar" => "bar",
        _ => "ring",
    }
}

fn normalize_indicator_effect_style(value: &str) -> &'static str {
    match value {
        "soft" => "soft",
        "depth" => "depth",
        "glow" => "glow",
        "breathe" => "breathe",
        _ => "flat",
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

fn clamp_ring_center_size(value: f32) -> f32 {
    clamp_px(value, default_ring_center_size_px(), 4.0, 32.0)
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
        "signal" => Palette::Signal,
        "cvd" => Palette::Cvd,
        "cool" => Palette::Cool,
        "ocean" => Palette::Ocean,
        "forest" => Palette::Forest,
        "sunset" => Palette::Sunset,
        "mono" => {
            Palette::Mono(parse_hex_rgb(input.mono_color.as_deref()).unwrap_or([0x4f, 0x8a, 0x73]))
        }
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

fn tool_colors_from_input(input: &SettingsInput) -> ToolColors {
    let defaults = ToolColors::default();
    ToolColors {
        claude_primary: parse_hex_rgb(input.claude_primary_color.as_deref())
            .unwrap_or(defaults.claude_primary),
        claude_secondary: parse_hex_rgb(input.claude_secondary_color.as_deref())
            .unwrap_or(defaults.claude_secondary),
        codex_primary: parse_hex_rgb(input.codex_primary_color.as_deref())
            .unwrap_or(defaults.codex_primary),
        codex_secondary: parse_hex_rgb(input.codex_secondary_color.as_deref())
            .unwrap_or(defaults.codex_secondary),
        grok_primary: parse_hex_rgb(input.grok_primary_color.as_deref())
            .unwrap_or(defaults.grok_primary),
        grok_secondary: parse_hex_rgb(input.grok_secondary_color.as_deref())
            .unwrap_or(defaults.grok_secondary),
        cursor_primary: parse_hex_rgb(input.cursor_primary_color.as_deref())
            .unwrap_or(defaults.cursor_primary),
        cursor_secondary: parse_hex_rgb(input.cursor_secondary_color.as_deref())
            .unwrap_or(defaults.cursor_secondary),
        warning: parse_hex_rgb(input.tool_warning_color.as_deref()).unwrap_or(defaults.warning),
        danger: parse_hex_rgb(input.tool_danger_color.as_deref()).unwrap_or(defaults.danger),
        warning_on: input.tool_warning_color_on,
        danger_on: input.tool_danger_color_on,
    }
}

fn taskbar_text_colors_from_input(input: &SettingsInput) -> TaskbarTextColors {
    let defaults = TaskbarTextColors::default();
    TaskbarTextColors {
        claude: parse_hex_rgb(input.claude_text_color.as_deref()).unwrap_or(defaults.claude),
        claude_on: input.claude_text_color_on,
        codex: parse_hex_rgb(input.codex_text_color.as_deref()).unwrap_or(defaults.codex),
        codex_on: input.codex_text_color_on,
        grok: parse_hex_rgb(input.grok_text_color.as_deref()).unwrap_or(defaults.grok),
        grok_on: input.grok_text_color_on,
        cursor: parse_hex_rgb(input.cursor_text_color.as_deref()).unwrap_or(defaults.cursor),
        cursor_on: input.cursor_text_color_on,
        info: parse_hex_rgb(input.info_text_color.as_deref()).unwrap_or(defaults.info),
        info_on: input.info_text_color_on,
        ring: parse_hex_rgb(input.ring_text_color.as_deref()).unwrap_or(defaults.ring),
        ring_on: input.ring_text_color_on,
    }
}

fn parse_hex_rgb(value: Option<&str>) -> Option<[u8; 3]> {
    let value = value?.trim().strip_prefix('#').unwrap_or(value?.trim());
    if value.len() != 6 || !value.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }

    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some([red, green, blue])
}

pub(crate) fn replace_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
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
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
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

#[cfg(test)]
mod parser_tests {
    use super::{
        parse_hex_rgb, remove_file_if_exists, Settings, SettingsInput, TaskbarAppearanceProfile,
        TaskbarLayoutProfile, TaskbarPlacement, TaskbarPresentationProfile, ToolColors,
        DEFAULT_CURSOR_PRIMARY, DEFAULT_CURSOR_SECONDARY,
    };
    use crate::render::Palette;
    use std::{
        fs,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "agent-juice-config-unit-{name}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn hex_rgb_rejects_non_ascii_without_panicking() {
        assert_eq!(parse_hex_rgb(Some("#a0B1c2")), Some([0xa0, 0xb1, 0xc2]));
        for invalid in ["aéaaa", "💚12", "12 456", "gg0000"] {
            assert_eq!(parse_hex_rgb(Some(invalid)), None);
        }
    }

    #[test]
    fn taskbar_layout_memory_defaults_on_and_accepts_an_explicit_opt_out() {
        let legacy: Settings = serde_json::from_str("{}").unwrap();
        assert!(legacy.taskbar_layout_memory_on);
        assert!(legacy.taskbar_profile_presentation_on);
        assert!(!legacy.taskbar_profile_colors_on);
        assert!(legacy.taskbar_layout_profiles.is_empty());
        assert!(!legacy.taskbar_layout_memory_initialized);

        let disabled = Settings::from_input(SettingsInput {
            taskbar_layout_memory_on: false,
            ..SettingsInput::default()
        });
        assert!(!disabled.taskbar_layout_memory_on);

        let scoped = Settings::from_input(SettingsInput {
            taskbar_profile_presentation_on: false,
            taskbar_profile_colors_on: true,
            ..SettingsInput::default()
        });
        assert!(!scoped.taskbar_profile_presentation_on);
        assert!(scoped.taskbar_profile_colors_on);
    }

    #[test]
    fn taskbar_profile_snapshots_apply_only_visual_settings() {
        let mut source = Settings {
            palette: Palette::Ocean,
            bar_mode: "quad".into(),
            full_reset_time_on: false,
            limit_order: "secondary_first".into(),
            indicator_style: "bar".into(),
            indicator_effect_style: "glow".into(),
            ring_size_px: 42.0,
            ring_thickness_px: 7.0,
            ring_gap_px: 9.0,
            ring_center_size_px: 20.0,
            bar_content_gap_px: 3.5,
            poll_interval_secs: 777,
            show_cursor: true,
            theme: "dark".into(),
            ..Settings::default()
        };
        source.tool_colors.cursor_primary = [1, 2, 3];
        source.taskbar_text_colors.cursor = [4, 5, 6];
        source.indicator_track_color = [7, 8, 9];
        source.indicator_track_opacity_percent = 37.5;

        let presentation = TaskbarPresentationProfile::from_settings(&source);
        let appearance = TaskbarAppearanceProfile::from_settings(&source);
        let mut target = Settings {
            poll_interval_secs: 120,
            show_cursor: false,
            theme: "light".into(),
            ..Settings::default()
        };
        presentation.apply_to(&mut target);
        appearance.apply_to(&mut target);

        assert_eq!(target.bar_mode, "quad");
        assert_eq!(target.indicator_style, "bar");
        assert_eq!(target.ring_size_px, 42.0);
        assert_eq!(target.bar_content_gap_px, 3.5);
        assert_eq!(target.palette, Palette::Ocean);
        assert_eq!(target.tool_colors.cursor_primary, [1, 2, 3]);
        assert_eq!(target.taskbar_text_colors.cursor, [4, 5, 6]);
        assert_eq!(target.indicator_track_color, [7, 8, 9]);
        assert_eq!(target.indicator_track_opacity_percent, 37.5);
        assert_eq!(target.poll_interval_secs, 120);
        assert!(!target.show_cursor);
        assert_eq!(target.theme, "light");
    }

    #[test]
    fn taskbar_profile_snapshots_are_normalized_with_legacy_safe_defaults() {
        let mut presentation = TaskbarPresentationProfile {
            bar_mode: "unknown".into(),
            limit_order: "unknown".into(),
            indicator_style: "unknown".into(),
            indicator_effect_style: "unknown".into(),
            ring_size_px: 100.0,
            ring_thickness_px: -1.0,
            ring_gap_px: 100.0,
            ring_center_size_px: -1.0,
            ring_number_outline_width_px: 100.0,
            ring_number_font_size_px: 100.0,
            ring_number_font_weight: 99,
            bar_text_font_size_px: 100.0,
            bar_text_font_weight: 9999,
            bar_content_gap_px: 100.0,
            ..TaskbarPresentationProfile::default()
        };
        presentation.full_reset_time_on = false;
        let appearance = TaskbarAppearanceProfile {
            indicator_track_opacity_percent: -5.0,
            ..TaskbarAppearanceProfile::default()
        };
        let mut settings = Settings {
            taskbar_layout_profiles: vec![TaskbarLayoutProfile {
                monitor_keys: vec!["monitor-a".into()],
                claude: Some(TaskbarPlacement {
                    monitor_key: "monitor-a".into(),
                    offset_ratio: 0.5,
                }),
                codex: None,
                grok: None,
                cursor: None,
                presentation: Some(presentation),
                appearance: Some(appearance),
            }],
            ..Settings::default()
        };

        settings.normalize_taskbar_layout_profiles();
        let profile = &settings.taskbar_layout_profiles[0];
        let presentation = profile.presentation.as_ref().unwrap();
        assert_eq!(presentation.bar_mode, "full");
        assert_eq!(presentation.limit_order, "primary_first");
        assert_eq!(presentation.indicator_style, "ring");
        assert_eq!(presentation.indicator_effect_style, "flat");
        assert_eq!(presentation.ring_size_px, 44.0);
        assert_eq!(presentation.ring_thickness_px, 1.0);
        assert_eq!(presentation.ring_gap_px, 14.0);
        assert_eq!(presentation.ring_center_size_px, 4.0);
        assert_eq!(presentation.ring_number_outline_width_px, 4.0);
        assert_eq!(presentation.ring_number_font_size_px, 16.0);
        assert_eq!(presentation.ring_number_font_weight, 100);
        assert_eq!(presentation.bar_text_font_size_px, 16.0);
        assert_eq!(presentation.bar_text_font_weight, 900);
        assert_eq!(presentation.bar_content_gap_px, 24.0);
        assert_eq!(
            profile
                .appearance
                .as_ref()
                .unwrap()
                .indicator_track_opacity_percent,
            0.0
        );
    }

    #[test]
    fn taskbar_pause_defaults_to_resumed_and_round_trips() {
        let legacy: Settings = serde_json::from_str("{}").unwrap();
        assert!(!legacy.taskbar_bars_paused);

        let paused = Settings {
            taskbar_bars_paused: true,
            ..Settings::default()
        };
        let serialized = serde_json::to_string(&paused).unwrap();
        let restored: Settings = serde_json::from_str(&serialized).unwrap();

        assert!(restored.taskbar_bars_paused);
    }

    #[test]
    fn grok_collection_is_opt_in_and_round_trips_through_ui_input() {
        assert!(!Settings::default().show_grok);
        let legacy_input: SettingsInput = serde_json::from_str("{}").unwrap();
        assert!(!legacy_input.show_grok);

        let settings = Settings::from_input(SettingsInput {
            show_grok: true,
            ..SettingsInput::default()
        });
        assert!(settings.show_grok);
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(restored.show_grok);
    }

    #[test]
    fn cursor_collection_is_opt_in_and_round_trips_through_ui_input() {
        assert!(!Settings::default().show_cursor);
        let legacy_input: SettingsInput = serde_json::from_str("{}").unwrap();
        assert!(!legacy_input.show_cursor);

        let settings = Settings::from_input(SettingsInput {
            show_cursor: true,
            cursor_primary_color: Some("#85847f".into()),
            cursor_secondary_color: Some("#0891b2".into()),
            cursor_text_color: Some("#5b9cff".into()),
            cursor_text_color_on: true,
            cursor_taskbar_offset_ratio: 0.45,
            cursor_taskbar_monitor_key: "monitor:cursor".into(),
            ..SettingsInput::default()
        });
        assert!(settings.show_cursor);
        assert_eq!(settings.tool_colors.cursor_primary, DEFAULT_CURSOR_PRIMARY);
        assert_eq!(
            settings.tool_colors.cursor_secondary,
            DEFAULT_CURSOR_SECONDARY
        );
        assert_eq!(settings.taskbar_text_colors.cursor, [0x5b, 0x9c, 0xff]);
        assert!(settings.taskbar_text_colors.cursor_on);
        assert_eq!(settings.cursor_taskbar_offset_ratio, 0.45);
        assert_eq!(settings.cursor_taskbar_monitor_key, "monitor:cursor");
        assert!(!settings.cursor_taskbar_target_initialized);

        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(restored.show_cursor);
    }

    #[test]
    fn grok_ui_settings_persist_colors_text_and_taskbar_target_input() {
        let settings = Settings::from_input(SettingsInput {
            grok_primary_color: Some("#d15288".into()),
            grok_secondary_color: Some("#8269c8".into()),
            grok_text_color: Some("#f070a0".into()),
            grok_text_color_on: true,
            grok_taskbar_offset_ratio: 0.65,
            grok_taskbar_monitor_key: "monitor:grok".into(),
            ..SettingsInput::default()
        });

        assert_eq!(settings.tool_colors.grok_primary, [0xd1, 0x52, 0x88]);
        assert_eq!(settings.tool_colors.grok_secondary, [0x82, 0x69, 0xc8]);
        assert_eq!(settings.taskbar_text_colors.grok, [0xf0, 0x70, 0xa0]);
        assert!(settings.taskbar_text_colors.grok_on);
        assert_eq!(settings.grok_taskbar_offset_ratio, 0.65);
        assert_eq!(settings.grok_taskbar_monitor_key, "monitor:grok");
        assert!(!settings.grok_taskbar_target_initialized);
    }

    #[test]
    fn taskbar_layout_profiles_are_canonical_bounded_and_latest_wins() {
        let root = temp_root("taskbar-layout-normalization");
        let path = root.join("settings.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            r#"{
                "taskbar_layout_profiles": [
                    {
                        "monitor_keys": ["monitor-b", "", "monitor-a", "monitor-a"],
                        "claude": {"monitor_key": "monitor-a", "offset_ratio": 2}
                    },
                    {
                        "monitor_keys": ["monitor-a", "monitor-b"],
                        "claude": {"monitor_key": "missing", "offset_ratio": 0.4},
                        "codex": {"monitor_key": "monitor-b", "offset_ratio": -1}
                    }
                ]
            }"#,
        )
        .unwrap();

        let mut settings = Settings::try_load_from(&path).unwrap();
        assert_eq!(settings.taskbar_layout_profiles.len(), 1);
        let loaded = &settings.taskbar_layout_profiles[0];
        assert_eq!(loaded.monitor_keys, ["monitor-a", "monitor-b"]);
        assert_eq!(loaded.claude.as_ref().unwrap().offset_ratio, 1.0);
        assert_eq!(loaded.codex.as_ref().unwrap().offset_ratio, 0.0);
        assert!(loaded.presentation.is_none());
        assert!(loaded.appearance.is_none());

        for index in 0..18 {
            let monitor_key = format!("monitor-{index:02}");
            assert!(
                settings.upsert_taskbar_layout_profile(TaskbarLayoutProfile {
                    monitor_keys: vec![monitor_key.clone()],
                    claude: Some(TaskbarPlacement {
                        monitor_key,
                        offset_ratio: 0.5,
                    }),
                    codex: None,
                    grok: None,
                    cursor: None,
                    presentation: Some(TaskbarPresentationProfile {
                        bar_mode: "compact".into(),
                        ..TaskbarPresentationProfile::default()
                    }),
                    appearance: Some(TaskbarAppearanceProfile {
                        palette: Palette::Forest,
                        ..TaskbarAppearanceProfile::default()
                    }),
                })
            );
        }
        assert_eq!(settings.taskbar_layout_profiles.len(), 16);
        assert_eq!(
            settings.taskbar_layout_profiles[0].monitor_keys,
            ["monitor-02"]
        );
        assert_eq!(
            settings.taskbar_layout_profiles[15].monitor_keys,
            ["monitor-17"]
        );
        assert!(settings.touch_taskbar_layout_profile(&["monitor-02".into()]));
        assert!(settings.taskbar_layout_profile_is_most_recent(&["monitor-02".into()]));
        assert!(!settings.touch_taskbar_layout_profile(&["monitor-02".into()]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn taskbar_monitor_key_alias_migration_updates_active_targets_and_profiles() {
        let mut settings = Settings {
            claude_taskbar_monitor_key: String::new(),
            claude_taskbar_target_initialized: true,
            codex_taskbar_monitor_key: String::new(),
            codex_taskbar_target_initialized: false,
            taskbar_layout_profiles: vec![
                TaskbarLayoutProfile {
                    monitor_keys: vec!["device:display1".into()],
                    claude: Some(TaskbarPlacement {
                        monitor_key: "device:display1".into(),
                        offset_ratio: 0.2,
                    }),
                    codex: None,
                    grok: None,
                    cursor: None,
                    presentation: Some(TaskbarPresentationProfile {
                        bar_mode: "compact".into(),
                        ..TaskbarPresentationProfile::default()
                    }),
                    appearance: Some(TaskbarAppearanceProfile {
                        palette: Palette::Forest,
                        ..TaskbarAppearanceProfile::default()
                    }),
                },
                TaskbarLayoutProfile {
                    monitor_keys: vec!["monitor-path:primary".into()],
                    claude: None,
                    codex: Some(TaskbarPlacement {
                        monitor_key: "monitor-path:primary".into(),
                        offset_ratio: 0.8,
                    }),
                    grok: None,
                    cursor: None,
                    presentation: None,
                    appearance: None,
                },
            ],
            ..Settings::default()
        };
        let replacements = vec![
            (String::new(), "monitor-path:primary".into()),
            ("device:display1".into(), "monitor-path:primary".into()),
        ];

        assert!(settings.migrate_taskbar_monitor_key_aliases(&replacements));
        assert_eq!(settings.claude_taskbar_monitor_key, "monitor-path:primary");
        assert_eq!(settings.codex_taskbar_monitor_key, "");
        assert_eq!(settings.taskbar_layout_profiles.len(), 1);
        assert_eq!(
            settings.taskbar_layout_profiles[0].monitor_keys,
            vec!["monitor-path:primary".to_string()]
        );
        assert_eq!(
            settings.taskbar_layout_profiles[0]
                .claude
                .as_ref()
                .unwrap()
                .monitor_key,
            "monitor-path:primary"
        );
        assert_eq!(
            settings.taskbar_layout_profiles[0]
                .presentation
                .as_ref()
                .unwrap()
                .bar_mode,
            "compact"
        );
        assert_eq!(
            settings.taskbar_layout_profiles[0]
                .appearance
                .as_ref()
                .unwrap()
                .palette,
            Palette::Forest
        );
        assert_eq!(
            settings.taskbar_layout_profiles[0]
                .codex
                .as_ref()
                .unwrap()
                .offset_ratio,
            0.8
        );
        assert!(!settings.migrate_taskbar_monitor_key_aliases(&replacements));
    }

    #[test]
    fn legacy_tool_colors_gain_threshold_defaults_without_losing_custom_values() {
        let settings: Settings = serde_json::from_str(
            r##"{
                "tool_colors": {
                    "claude_primary": [16, 32, 48],
                    "claude_secondary": [64, 80, 96],
                    "codex_primary": [112, 128, 144],
                    "codex_secondary": [160, 176, 192]
                }
            }"##,
        )
        .unwrap();

        assert_eq!(settings.tool_colors.claude_primary, [16, 32, 48]);
        assert_eq!(settings.tool_colors.codex_secondary, [160, 176, 192]);
        assert_eq!(settings.tool_colors.warning, ToolColors::default().warning);
        assert_eq!(settings.tool_colors.danger, ToolColors::default().danger);
        assert!(settings.tool_colors.warning_on);
        assert!(settings.tool_colors.danger_on);
    }

    #[test]
    fn settings_input_persists_tool_threshold_colors_and_independent_toggles() {
        let settings = Settings::from_input(SettingsInput {
            tool_warning_color: Some("#123456".into()),
            tool_danger_color: Some("#abcdef".into()),
            tool_warning_color_on: false,
            tool_danger_color_on: true,
            ..SettingsInput::default()
        });

        assert_eq!(settings.tool_colors.warning, [0x12, 0x34, 0x56]);
        assert_eq!(settings.tool_colors.danger, [0xab, 0xcd, 0xef]);
        assert!(!settings.tool_colors.warning_on);
        assert!(settings.tool_colors.danger_on);
    }

    #[test]
    fn activity_settings_default_and_clamp_to_supported_ranges() {
        let defaults = Settings::default();
        assert_eq!(defaults.activity_weeks, 52);
        assert_eq!(defaults.activity_scale_mode, "auto");
        assert_eq!(defaults.activity_tokens_per_level, 250_000);

        let clamped = Settings::from_input(SettingsInput {
            activity_weeks: 2,
            activity_scale_mode: "unsupported".into(),
            activity_tokens_per_level: 0,
            ..SettingsInput::default()
        });
        assert_eq!(clamped.activity_weeks, 4);
        assert_eq!(clamped.activity_scale_mode, "auto");
        assert_eq!(clamped.activity_tokens_per_level, 1);

        let fixed = Settings::from_input(SettingsInput {
            activity_weeks: 99,
            activity_scale_mode: "fixed".into(),
            activity_tokens_per_level: u64::MAX,
            ..SettingsInput::default()
        });
        assert_eq!(fixed.activity_weeks, 52);
        assert_eq!(fixed.activity_scale_mode, "fixed");
        assert_eq!(fixed.activity_tokens_per_level, 1_000_000_000_000);
    }

    #[test]
    fn install_preserves_concurrent_unrelated_changes_and_rejects_statusline_conflicts() {
        let root = temp_root("install-conflict");
        let home = root.join("home");
        let data = root.join("data");
        let settings = home.join(".claude").join("settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(
            &settings,
            r#"{"statusLine":{"type":"command","command":"original"},"theme":"old"}"#,
        )
        .unwrap();

        Settings::install_statusline_wrap_at_with_hook(
            &home,
            &data,
            r"C:\Juice\agentjuice-statusline.exe",
            |path| {
                fs::write(
                    path,
                    r#"{"statusLine":{"type":"command","command":"original"},"theme":"new"}"#,
                )
                .unwrap();
            },
        )
        .unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(installed["theme"], "new");
        assert!(installed["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("agentjuice-statusline.exe"));

        let conflict_root = temp_root("install-statusline-conflict");
        let conflict_home = conflict_root.join("home");
        let conflict_data = conflict_root.join("data");
        let conflict_settings = conflict_home.join(".claude").join("settings.json");
        fs::create_dir_all(conflict_settings.parent().unwrap()).unwrap();
        fs::write(
            &conflict_settings,
            r#"{"statusLine":{"type":"command","command":"original"}}"#,
        )
        .unwrap();
        assert!(Settings::install_statusline_wrap_at_with_hook(
            &conflict_home,
            &conflict_data,
            r"C:\Juice\agentjuice-statusline.exe",
            |path| {
                fs::write(
                    path,
                    r#"{"statusLine":{"type":"command","command":"user-change"}}"#,
                )
                .unwrap();
            },
        )
        .is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&conflict_settings).unwrap())
                .unwrap()["statusLine"]["command"],
            "user-change"
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(conflict_root).unwrap();
    }

    #[test]
    fn restore_retries_cleanup_after_settings_are_already_restored() {
        for failed_name in ["wrap.json", "wrap-meta.json"] {
            let root = temp_root(failed_name);
            let home = root.join("home");
            let data = root.join("data");
            let settings = home.join(".claude").join("settings.json");
            fs::create_dir_all(settings.parent().unwrap()).unwrap();
            fs::write(
                &settings,
                r#"{"statusLine":{"type":"command","command":"original"},"keep":true}"#,
            )
            .unwrap();
            Settings::install_statusline_wrap_at(
                &home,
                &data,
                r"C:\Juice\agentjuice-statusline.exe",
            )
            .unwrap();

            let mut failed = false;
            assert!(Settings::restore_statusline_at_with(
                &home,
                &data,
                |_| {},
                |path: &Path| {
                    if !failed
                        && path.file_name().and_then(|name| name.to_str()) == Some(failed_name)
                    {
                        failed = true;
                        anyhow::bail!("injected cleanup failure");
                    }
                    remove_file_if_exists(path)
                },
            )
            .is_err());
            let restored: serde_json::Value =
                serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
            assert_eq!(restored["statusLine"]["command"], "original");
            assert!(data.join("wrap-meta.json").exists());

            Settings::restore_statusline_at(&home, &data).unwrap();
            assert!(!data.join("wrap.json").exists());
            assert!(!data.join("wrap-meta.json").exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn restore_preserves_concurrent_unrelated_changes_and_rejects_statusline_conflicts() {
        for conflict in [false, true] {
            let root = temp_root(if conflict {
                "restore-statusline-conflict"
            } else {
                "restore-unrelated-change"
            });
            let home = root.join("home");
            let data = root.join("data");
            let settings = home.join(".claude").join("settings.json");
            fs::create_dir_all(settings.parent().unwrap()).unwrap();
            fs::write(
                &settings,
                r#"{"statusLine":{"type":"command","command":"original"},"theme":"old"}"#,
            )
            .unwrap();
            Settings::install_statusline_wrap_at(
                &home,
                &data,
                r"C:\Juice\agentjuice-statusline.exe",
            )
            .unwrap();
            let managed: serde_json::Value =
                serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
            let replacement = if conflict {
                serde_json::json!({
                    "statusLine": {"type":"command", "command":"user-change"},
                    "theme": "new"
                })
            } else {
                serde_json::json!({
                    "statusLine": managed["statusLine"].clone(),
                    "theme": "new"
                })
            };

            let result = Settings::restore_statusline_at_with(
                &home,
                &data,
                |path| fs::write(path, serde_json::to_vec(&replacement).unwrap()).unwrap(),
                remove_file_if_exists,
            );
            let current: serde_json::Value =
                serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
            assert_eq!(current["theme"], "new");
            if conflict {
                assert!(result.is_err());
                assert_eq!(current["statusLine"]["command"], "user-change");
                assert!(data.join("wrap-meta.json").exists());
            } else {
                result.unwrap();
                assert_eq!(current["statusLine"]["command"], "original");
                assert!(!data.join("wrap-meta.json").exists());
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn settings_snapshot_retries_when_revision_changes_after_load() {
        let root = temp_root("settings-snapshot");
        let path = root.join("settings.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, r#"{"bar_mode":"full"}"#).unwrap();
        let mut replaced = false;

        let (settings, revision) = Settings::load_with_revision_at_with_hook(&path, || {
            if !replaced {
                fs::write(&path, r#"{"bar_mode":"compact"}"#).unwrap();
                replaced = true;
            }
        });

        assert_eq!(settings.bar_mode, "compact");
        assert_eq!(revision, Settings::storage_revision_at(&path));
        fs::remove_dir_all(root).unwrap();
    }
}
