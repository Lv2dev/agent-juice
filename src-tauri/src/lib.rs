pub mod adapters;
#[cfg(windows)]
pub mod appbar;
pub mod collector;
pub mod config;
pub mod model;
pub mod render;
pub mod statusline;
#[cfg(windows)]
pub mod taskbar;
pub mod update;

use chrono::{DateTime, Utc};
use config::Settings;
use model::{AgentStatus, Tool};
use once_cell::sync::Lazy;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Condvar, Mutex,
};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;

const TASKBAR_DOCK_WIDTH: i32 = 260;
const TASKBAR_DRAG_THRESHOLD_PX: i32 = 3;
const TASKBAR_TOOLS: [&str; 2] = ["claude", "codex"];
const CODEX_REPRESENTATIVE_CANDIDATES: usize = 32;
const CODEX_ACCOUNT_CACHE_MIN_SECS: i64 = 30;
const CODEX_ACCOUNT_API_TIMEOUT_SECS: u64 = 5;
const CLAUDE_USAGE_CACHE_MIN_SECS: i64 = 60;
const CLAUDE_USAGE_TIMEOUT_SECS: u64 = 10;
const TRAY_ID: &str = "juice";
const TRAY_ICON_IDS: [&str; 1] = [TRAY_ID];
const UPDATE_START_DELAY_SECS: u64 = 15;

#[derive(Clone, serde::Serialize)]
struct TaskbarDraggingPayload {
    tool: &'static str,
    dragging: bool,
}

#[derive(Default)]
struct TaskbarPauseState(AtomicBool);

#[derive(Clone)]
struct CachedStatusAttempt {
    attempted_at: DateTime<Utc>,
    status: Option<AgentStatus>,
}

#[derive(Default)]
struct CollectionFlight {
    in_flight: bool,
    in_flight_force: bool,
    last_result: Vec<AgentStatus>,
}

#[derive(Default)]
struct CollectionCoordinator {
    state: Mutex<CollectionFlight>,
    completed: Condvar,
}

impl CollectionCoordinator {
    fn run(&self, force: bool, collect: impl FnOnce() -> Vec<AgentStatus>) -> Vec<AgentStatus> {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        loop {
            if !state.in_flight {
                state.in_flight = true;
                state.in_flight_force = force;
                break;
            }

            let joined_forced_collection = state.in_flight_force;
            while state.in_flight {
                state = self
                    .completed
                    .wait(state)
                    .unwrap_or_else(|err| err.into_inner());
            }
            if !force || joined_forced_collection {
                return state.last_result.clone();
            }
        }
        drop(state);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(collect));
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.in_flight = false;
        state.in_flight_force = false;
        if let Ok(result) = &outcome {
            state.last_result = result.clone();
        }
        self.completed.notify_all();
        drop(state);

        match outcome {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}

static COLLECTION_COORDINATOR: Lazy<CollectionCoordinator> =
    Lazy::new(CollectionCoordinator::default);
static CODEX_ACCOUNT_CACHE: Lazy<Mutex<Option<CachedStatusAttempt>>> =
    Lazy::new(|| Mutex::new(None));
static CLAUDE_USAGE_CACHE: Lazy<Mutex<Option<CachedStatusAttempt>>> =
    Lazy::new(|| Mutex::new(None));

pub fn tray_tooltip() -> &'static str {
    "Juice"
}

pub fn tray_icon_ids() -> &'static [&'static str] {
    &TRAY_ICON_IDS
}

pub fn tray_id() -> &'static str {
    TRAY_ID
}

pub fn tray_open_menu_id() -> &'static str {
    "juice-open"
}

pub fn tray_pause_bar_menu_id() -> &'static str {
    "juice-pause-bars"
}

pub fn tray_resume_bar_menu_id() -> &'static str {
    "juice-resume-bars"
}

pub fn tray_quit_menu_id() -> &'static str {
    "juice-quit"
}

fn rfc3339_of_mtime(path: &std::path::Path, fallback: DateTime<Utc>) -> String {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(|time| chrono::DateTime::<Utc>::from(time).to_rfc3339())
        .unwrap_or_else(|| fallback.to_rfc3339())
}

fn derive_active(status: &mut AgentStatus, stale_after_secs: i64, now: DateTime<Utc>) {
    status.session.active = chrono::DateTime::parse_from_rfc3339(&status.captured_at)
        .ok()
        .map(|captured| {
            let age = (now - captured.with_timezone(&Utc)).num_seconds();
            age >= 0 && age <= stale_after_secs
        })
        .unwrap_or(false);
}

pub fn collect_all(settings: &Settings) -> Vec<AgentStatus> {
    let data_dir = dirs::data_local_dir().map(|dir| dir.join("agent-juice"));
    let codex_sessions_dir = dirs::home_dir().map(|home| home.join(".codex").join("sessions"));

    collect_all_from(
        settings,
        data_dir.as_deref(),
        codex_sessions_dir.as_deref(),
        Utc::now(),
    )
}

pub fn collect_all_from(
    settings: &Settings,
    data_dir: Option<&std::path::Path>,
    codex_sessions_dir: Option<&std::path::Path>,
    now: DateTime<Utc>,
) -> Vec<AgentStatus> {
    let pc_id = gethostname::gethostname().to_string_lossy().to_string();
    let mut statuses = Vec::new();

    if let Some(dir) = data_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !(name.starts_with("claude_last.") && name.ends_with(".json")) {
                    continue;
                }

                if let Some(status) = parse_claude_status_file(&entry.path(), settings, &pc_id, now)
                {
                    statuses.push(status);
                }
            }
        }
    }

    if let Some(sessions_dir) = codex_sessions_dir {
        for path in collector::list_rollouts(sessions_dir) {
            if let Some(status) = parse_codex_status_file(&path, settings, &pc_id, now) {
                statuses.push(status);
            }
        }
    }

    statuses
}

pub fn collect_representatives(settings: &Settings) -> Vec<AgentStatus> {
    collect_representatives_with_options(settings, false, false)
}

fn collect_representatives_force(settings: &Settings) -> Vec<AgentStatus> {
    collect_representatives_with_options(settings, true, true)
}

async fn collect_representatives_off_thread(settings: Settings, force: bool) -> Vec<AgentStatus> {
    match tauri::async_runtime::spawn_blocking(move || {
        if force {
            collect_representatives_force(&settings)
        } else {
            collect_representatives(&settings)
        }
    })
    .await
    {
        Ok(statuses) => statuses,
        Err(err) => {
            eprintln!("[collector] blocking task failed: {err}");
            Vec::new()
        }
    }
}

fn collect_representatives_with_options(
    settings: &Settings,
    force_codex_account: bool,
    force_claude_usage: bool,
) -> Vec<AgentStatus> {
    let data_dir = dirs::data_local_dir().map(|dir| dir.join("agent-juice"));
    let codex_sessions_dir = dirs::home_dir().map(|home| home.join(".codex").join("sessions"));
    let now = Utc::now();

    COLLECTION_COORDINATOR.run(force_codex_account || force_claude_usage, || {
        collect_representatives_runtime(
            settings,
            data_dir.as_deref(),
            codex_sessions_dir.as_deref(),
            now,
            force_codex_account,
            force_claude_usage,
        )
    })
}

fn collect_representatives_runtime(
    settings: &Settings,
    data_dir: Option<&std::path::Path>,
    codex_sessions_dir: Option<&std::path::Path>,
    now: DateTime<Utc>,
    force_codex_account: bool,
    force_claude_usage: bool,
) -> Vec<AgentStatus> {
    let pc_id = gethostname::gethostname().to_string_lossy().to_string();
    let mut statuses = Vec::new();

    let claude_status = if let Some(path) = latest_matching_file(data_dir, |name| {
        name.starts_with("claude_last.") && name.ends_with(".json")
    }) {
        parse_claude_status_file(&path, settings, &pc_id, now)
    } else {
        None
    };
    let claude_usage = if force_claude_usage || settings.claude_account_auto_collect_on {
        collect_claude_usage_status(settings, &pc_id, now, force_claude_usage)
    } else {
        None
    };
    if let Some(status) = merge_claude_usage_status(claude_status, claude_usage) {
        statuses.push(status);
    }

    if let Some(status) = collect_codex_account_status(settings, &pc_id, now, force_codex_account) {
        statuses.push(status);
    } else if let Some(sessions_dir) = codex_sessions_dir {
        for path in collector::recent_rollouts(sessions_dir, CODEX_REPRESENTATIVE_CANDIDATES) {
            if let Some(status) = parse_codex_status_file(&path, settings, &pc_id, now) {
                statuses.push(status);
                break;
            }
        }
    }

    latest_per_tool(&statuses)
}

pub fn collect_representatives_from(
    settings: &Settings,
    data_dir: Option<&std::path::Path>,
    codex_sessions_dir: Option<&std::path::Path>,
    now: DateTime<Utc>,
) -> Vec<AgentStatus> {
    let pc_id = gethostname::gethostname().to_string_lossy().to_string();
    let mut statuses = Vec::new();

    if let Some(path) = latest_matching_file(data_dir, |name| {
        name.starts_with("claude_last.") && name.ends_with(".json")
    }) {
        if let Some(status) = parse_claude_status_file(&path, settings, &pc_id, now) {
            statuses.push(status);
        }
    }

    if let Some(sessions_dir) = codex_sessions_dir {
        for path in collector::recent_rollouts(sessions_dir, CODEX_REPRESENTATIVE_CANDIDATES) {
            if let Some(status) = parse_codex_status_file(&path, settings, &pc_id, now) {
                statuses.push(status);
                break;
            }
        }
    }

    latest_per_tool(&statuses)
}

fn latest_matching_file(
    dir: Option<&std::path::Path>,
    matches_name: impl Fn(&str) -> bool,
) -> Option<std::path::PathBuf> {
    let dir = dir?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !matches_name(&name) {
                return None;
            }
            std::fs::metadata(entry.path())
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(|modified| (modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn parse_claude_status_file(
    path: &std::path::Path,
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
) -> Option<AgentStatus> {
    let captured_at = rfc3339_of_mtime(path, now);
    let contents = std::fs::read_to_string(path).ok()?;
    let mut status = adapters::claude::parse(&contents, pc_id, &captured_at).ok()?;
    derive_active(&mut status, settings.stale_after_secs, now);
    Some(status)
}

fn parse_codex_status_file(
    path: &std::path::Path,
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
) -> Option<AgentStatus> {
    let session_id = collector::session_id_of(path);
    let captured_at = rfc3339_of_mtime(path, now);
    let line = collector::last_token_count_line_from_file(path).ok()??;
    let mut status =
        adapters::codex::parse_token_count(&line, pc_id, &session_id, &captured_at).ok()?;
    derive_active(&mut status, settings.stale_after_secs, now);
    Some(status)
}

fn cached_status_attempt(
    cache: &Mutex<Option<CachedStatusAttempt>>,
    now: DateTime<Utc>,
    minimum_interval_secs: i64,
    force: bool,
    collect: impl FnOnce() -> Option<AgentStatus>,
) -> Option<AgentStatus> {
    if !force {
        let cache = cache.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = cache.as_ref() {
            let age = (now - cached.attempted_at).num_seconds();
            if (0..minimum_interval_secs).contains(&age) {
                return cached.status.clone();
            }
        }
    }

    let status = collect();
    let mut cache = cache.lock().unwrap_or_else(|err| err.into_inner());
    *cache = Some(CachedStatusAttempt {
        attempted_at: now,
        status: status.clone(),
    });
    status
}

fn collect_codex_account_status(
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    force: bool,
) -> Option<AgentStatus> {
    let mut status = cached_status_attempt(
        &CODEX_ACCOUNT_CACHE,
        now,
        CODEX_ACCOUNT_CACHE_MIN_SECS,
        force,
        || {
            let raw = collector::codex_account_rate_limits_response(
                std::time::Duration::from_secs(CODEX_ACCOUNT_API_TIMEOUT_SECS),
            )
            .ok()?;
            adapters::codex::parse_account_rate_limits_response(&raw, pc_id, &now.to_rfc3339()).ok()
        },
    )?;
    derive_active(&mut status, settings.stale_after_secs, now);
    Some(status)
}

fn collect_claude_usage_status(
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    force: bool,
) -> Option<AgentStatus> {
    let mut status = cached_status_attempt(
        &CLAUDE_USAGE_CACHE,
        now,
        CLAUDE_USAGE_CACHE_MIN_SECS,
        force,
        || {
            let timeout = std::time::Duration::from_secs(CLAUDE_USAGE_TIMEOUT_SECS);
            let captured_at = now.to_rfc3339();
            if let Ok(raw) = collector::claude_oauth_usage_response(timeout) {
                if let Ok(status) =
                    adapters::claude::parse_oauth_usage_response(&raw, pc_id, &captured_at)
                {
                    return Some(status);
                }
            }

            let legacy = collector::claude_usage_output(timeout).ok();
            if let Ok(raw) = collector::claude_oauth_usage_response(timeout) {
                if let Ok(status) =
                    adapters::claude::parse_oauth_usage_response(&raw, pc_id, &captured_at)
                {
                    return Some(status);
                }
            }

            legacy.and_then(|raw| {
                adapters::claude::parse_usage_output(&raw, pc_id, &captured_at).ok()
            })
        },
    )?;
    derive_active(&mut status, settings.stale_after_secs, now);
    Some(status)
}

fn merge_claude_usage_status(
    statusline: Option<AgentStatus>,
    usage: Option<AgentStatus>,
) -> Option<AgentStatus> {
    match (statusline, usage) {
        (Some(mut statusline), Some(usage))
            if usage.session_id == "claude-oauth-usage" && !usage.approx =>
        {
            let exact_primary = usage
                .primary
                .as_ref()
                .and_then(|limit| limit.used_percent)
                .is_some();
            let exact_secondary = usage
                .secondary
                .as_ref()
                .and_then(|limit| limit.used_percent)
                .is_some();
            statusline.captured_at = usage.captured_at;
            statusline.session.active = usage.session.active;
            if exact_primary {
                statusline.primary = usage.primary;
            }
            if exact_secondary {
                statusline.secondary = usage.secondary;
            }
            if exact_primary && exact_secondary {
                statusline.approx = false;
            }
            Some(statusline)
        }
        (Some(mut statusline), Some(usage)) => {
            merge_missing_limit(&mut statusline.primary, usage.primary);
            merge_missing_limit(&mut statusline.secondary, usage.secondary);
            Some(statusline)
        }
        (Some(statusline), None) => Some(statusline),
        (None, Some(usage)) => Some(usage),
        (None, None) => None,
    }
}

fn merge_missing_limit(
    statusline_limit: &mut Option<model::AccountLimit>,
    usage_limit: Option<model::AccountLimit>,
) {
    match (statusline_limit.as_mut(), usage_limit) {
        (Some(statusline), Some(usage)) => {
            if statusline.used_percent.is_none() {
                statusline.used_percent = usage.used_percent;
            }
            if statusline.resets_at.is_none() {
                statusline.resets_at = usage.resets_at;
            }
        }
        (None, Some(usage)) => *statusline_limit = Some(usage),
        _ => {}
    }
}

pub fn latest_per_tool(all: &[AgentStatus]) -> Vec<AgentStatus> {
    let mut claude: Option<&AgentStatus> = None;
    let mut codex: Option<&AgentStatus> = None;

    for status in all {
        let slot = match &status.tool {
            Tool::Claude => &mut claude,
            Tool::Codex => &mut codex,
        };

        if slot
            .as_ref()
            .is_none_or(|current| captured_is_newer(status, current))
        {
            *slot = Some(status);
        }
    }

    [claude, codex].into_iter().flatten().cloned().collect()
}

fn captured_is_newer(next: &AgentStatus, current: &AgentStatus) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(&next.captured_at),
        chrono::DateTime::parse_from_rfc3339(&current.captured_at),
    ) {
        (Ok(next), Ok(current)) => next > current,
        (Ok(_), Err(_)) => true,
        (Err(_), Ok(_)) => false,
        _ => next.captured_at > current.captured_at,
    }
}

#[cfg(test)]
fn tray_png_for_status(status: &AgentStatus, settings: &Settings) -> Option<Vec<u8>> {
    let primary = status.primary.as_ref().and_then(|limit| limit.used_percent);
    let secondary = status
        .secondary
        .as_ref()
        .and_then(|limit| limit.used_percent);
    let outer_color = render::color_for(
        primary.unwrap_or(0.0),
        settings.warn_threshold,
        settings.danger_threshold,
        settings.palette,
    );
    let inner_color = render::color_for(
        secondary.unwrap_or(0.0),
        settings.warn_threshold,
        settings.danger_threshold,
        settings.palette,
    );
    let svg = render::ring_svg(
        primary,
        secondary,
        render::worst(primary, secondary),
        &outer_color,
        &inner_color,
    );

    render::svg_to_png(&svg, 32).ok()
}

#[cfg(test)]
fn status_payload_signature(statuses: &[AgentStatus]) -> String {
    serde_json::to_string(statuses).unwrap_or_default()
}

fn setup_trays(app: &mut tauri::App) -> tauri::Result<()> {
    let default_icon = app.default_window_icon().cloned();
    let menu = MenuBuilder::new(app)
        .text(tray_open_menu_id(), "Juice 열기")
        .text(tray_pause_bar_menu_id(), "바 표출 일시중지")
        .text(tray_resume_bar_menu_id(), "바 표출 재개")
        .separator()
        .text(tray_quit_menu_id(), "종료")
        .build()?;

    let mut builder = TrayIconBuilder::with_id(tray_id())
        .tooltip(tray_tooltip())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            id if id == tray_open_menu_id() => show_panel(app),
            id if id == tray_pause_bar_menu_id() => pause_taskbar_bars_for_manager(app),
            id if id == tray_resume_bar_menu_id() => {
                if let Err(err) = resume_taskbar_bars_for_manager(app) {
                    eprintln!("[taskbar] resume bars failed: {err}");
                }
            }
            id if id == tray_quit_menu_id() => app.exit(0),
            _ => {}
        });
    if let Some(icon) = &default_icon {
        builder = builder.icon(icon.clone());
    }

    let tray = builder
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_panel(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(Mutex::new(tray));
    Ok(())
}

fn show_panel<R: tauri::Runtime>(manager: &impl tauri::Manager<R>) {
    if let Some(window) = manager.get_webview_window("panel") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn setup_panel_close_hide(app: &tauri::App) {
    if let Some(window) = app.get_webview_window("panel") {
        let panel = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = panel.hide();
            }
        });
    }
}

fn spawn_status_loop(handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let settings = Settings::load();
            let interval_secs = settings.poll_interval_secs.max(1);
            let representatives = collect_representatives_off_thread(settings, false).await;

            let _ = handle.emit("status-updated", &representatives);
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    });
}

fn normalize_taskbar_tool(tool: &str) -> Option<&'static str> {
    match tool {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    }
}

fn taskbar_bar_label(tool: &str) -> Option<&'static str> {
    match normalize_taskbar_tool(tool)? {
        "claude" => Some("bar-claude"),
        "codex" => Some("bar-codex"),
        _ => None,
    }
}

fn taskbar_offset_ratio(settings: &Settings, tool: &str) -> f32 {
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_offset_ratio,
        Some("codex") => settings.codex_taskbar_offset_ratio,
        _ => settings.taskbar_offset_ratio,
    }
}

fn set_taskbar_offset_ratio(settings: &mut Settings, tool: &str, ratio: f32) {
    let ratio = ratio.clamp(0.0, 1.0);
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_offset_ratio = ratio,
        Some("codex") => settings.codex_taskbar_offset_ratio = ratio,
        _ => settings.taskbar_offset_ratio = ratio,
    }
}

fn taskbar_monitor_key<'a>(settings: &'a Settings, tool: &str) -> &'a str {
    match normalize_taskbar_tool(tool) {
        Some("claude") => &settings.claude_taskbar_monitor_key,
        Some("codex") => &settings.codex_taskbar_monitor_key,
        _ => "",
    }
}

fn set_taskbar_target(settings: &mut Settings, tool: &str, monitor_key: &str, ratio: f32) {
    set_taskbar_offset_ratio(settings, tool, ratio);
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_monitor_key = monitor_key.to_string(),
        Some("codex") => settings.codex_taskbar_monitor_key = monitor_key.to_string(),
        _ => {}
    }
}

#[cfg(windows)]
fn position_taskbar_bar_on_taskbar<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    taskbar: &taskbar::ShellTaskbarWindow,
    rect: taskbar::DockRect,
) -> anyhow::Result<()> {
    let label = taskbar_bar_label(tool).ok_or_else(|| anyhow::anyhow!("unknown taskbar tool"))?;
    let window = manager
        .get_webview_window(label)
        .ok_or_else(|| anyhow::anyhow!("no {label} window"))?;

    apply_taskbar_owned_bar(&window, taskbar.hwnd, rect)?;
    Ok(())
}

fn taskbar_tool_enabled(settings: &Settings, tool: &str) -> bool {
    taskbar_dock_width(settings, tool).is_some()
}

#[cfg(test)]
fn should_show_taskbar_bar(settings: &Settings, tool: &str) -> bool {
    taskbar_tool_enabled(settings, tool)
}

#[cfg(test)]
fn should_show_taskbar_bar_with_fullscreen(
    settings: &Settings,
    tool: &str,
    fullscreen_active: bool,
) -> bool {
    should_show_taskbar_bar_with_window_state(settings, tool, fullscreen_active, false)
}

fn should_show_taskbar_bar_with_window_state(
    settings: &Settings,
    tool: &str,
    fullscreen_active: bool,
    maximized_active: bool,
) -> bool {
    if fullscreen_active && settings.fullscreen_hide_on {
        return false;
    }
    if maximized_active && settings.maximized_hide_on {
        return false;
    }
    taskbar_tool_enabled(settings, tool)
}

fn should_show_taskbar_bar_with_pause(
    settings: &Settings,
    tool: &str,
    fullscreen_active: bool,
    maximized_active: bool,
    taskbar_paused: bool,
) -> bool {
    if taskbar_paused {
        return false;
    }
    should_show_taskbar_bar_with_window_state(settings, tool, fullscreen_active, maximized_active)
}

fn taskbar_dock_width(settings: &Settings, tool: &str) -> Option<i32> {
    match normalize_taskbar_tool(tool)? {
        "claude" if settings.show_claude => Some(TASKBAR_DOCK_WIDTH),
        "codex" if settings.show_codex => Some(TASKBAR_DOCK_WIDTH),
        _ => None,
    }
}

fn hide_taskbar_bar<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, tool: &str) {
    let Some(label) = taskbar_bar_label(tool) else {
        return;
    };
    if let Some(window) = manager.get_webview_window(label) {
        #[cfg(windows)]
        {
            match window
                .hwnd()
                .map_err(|err| anyhow::anyhow!(err.to_string()))
                .and_then(taskbar::hide_window)
            {
                Ok(()) => return,
                Err(err) => eprintln!("[taskbar] native hide {tool} bar failed: {err}"),
            }
        }
        let _ = window.hide();
    }
}

fn hide_all_taskbar_bars<R: tauri::Runtime>(manager: &impl tauri::Manager<R>) {
    for tool in TASKBAR_TOOLS {
        hide_taskbar_bar(manager, tool);
    }
}

fn taskbar_bars_paused<R: tauri::Runtime>(manager: &impl tauri::Manager<R>) -> bool {
    manager
        .try_state::<TaskbarPauseState>()
        .map(|state| state.0.load(Ordering::Relaxed))
        .unwrap_or(false)
}

fn set_taskbar_bars_paused<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, paused: bool) {
    if let Some(state) = manager.try_state::<TaskbarPauseState>() {
        state.0.store(paused, Ordering::Relaxed);
    }
}

fn pause_taskbar_bars_for_manager<R: tauri::Runtime>(manager: &impl tauri::Manager<R>) {
    set_taskbar_bars_paused(manager, true);
    hide_all_taskbar_bars(manager);
}

fn resume_taskbar_bars_for_manager<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
) -> anyhow::Result<()> {
    set_taskbar_bars_paused(manager, false);
    apply_taskbar_dock(manager, &Settings::load())
}

#[cfg(windows)]
fn taskbar_bar_hwnds<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
) -> Vec<windows::Win32::Foundation::HWND> {
    TASKBAR_TOOLS
        .iter()
        .filter_map(|tool| taskbar_bar_label(tool))
        .filter_map(|label| manager.get_webview_window(label))
        .filter_map(|window| window.hwnd().ok())
        .collect()
}

#[cfg(windows)]
fn taskbar_hide_window_state_for_monitor<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    monitor: taskbar::DockRect,
) -> (bool, bool) {
    let excluded = taskbar_bar_hwnds(manager);
    let (fullscreen_present, maximized_present) =
        taskbar::visible_windows_coverage_on_monitor(&excluded, monitor);
    let fullscreen_active = settings.fullscreen_hide_on && fullscreen_present;
    let maximized_active = settings.maximized_hide_on && maximized_present;
    (fullscreen_active, maximized_active)
}

#[cfg(windows)]
fn bar_overlay_ex_style(current: isize) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };

    current | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize | WS_EX_TOPMOST.0 as isize
}

#[cfg(windows)]
fn bar_overlay_window_style(current: isize) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{WS_CHILD, WS_POPUP};

    (current | WS_POPUP.0 as isize) & !(WS_CHILD.0 as isize)
}

#[cfg(windows)]
fn apply_taskbar_owned_bar<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    owner_hwnd: windows::Win32::Foundation::HWND,
    rect: taskbar::DockRect,
) -> anyhow::Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWLP_HWNDPARENT, GWL_EXSTYLE,
        GWL_STYLE, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    };

    let hwnd = window.hwnd()?;
    unsafe {
        let current_ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, bar_overlay_ex_style(current_ex_style));

        let current_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, bar_overlay_window_style(current_style));

        SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner_hwnd.0 as isize);
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        )?;
    }
    Ok(())
}

fn try_setup_taskbar_dock(app: &tauri::App, settings: &Settings) -> anyhow::Result<()> {
    if std::env::var_os("AGENT_JUICE_FORCE_TASKBAR_DOCK_FAILURE").is_some() {
        return Err(anyhow::anyhow!("forced taskbar dock failure"));
    }

    #[cfg(windows)]
    {
        let taskbar_paused = taskbar_bars_paused(app);
        for tool in TASKBAR_TOOLS {
            let width = match taskbar_dock_width(settings, tool) {
                Some(width) => width,
                None => {
                    hide_taskbar_bar(app, tool);
                    continue;
                }
            };
            let taskbar =
                taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(settings, tool))?;
            let (fullscreen_active, maximized_active) =
                taskbar_hide_window_state_for_monitor(app, settings, taskbar.monitor);
            if !should_show_taskbar_bar_with_pause(
                settings,
                tool,
                fullscreen_active,
                maximized_active,
                taskbar_paused,
            ) {
                hide_taskbar_bar(app, tool);
                continue;
            }

            let target = taskbar::TaskbarTarget {
                key: taskbar.key.clone(),
                rect: taskbar::DockRect {
                    x: taskbar.left,
                    y: taskbar.top,
                    width: taskbar.right - taskbar.left,
                    height: taskbar.bottom - taskbar.top,
                },
                monitor: taskbar.monitor,
                primary: taskbar.primary,
            };
            let rect = taskbar::dock_rect_for_taskbar_target(
                &target,
                width,
                taskbar_offset_ratio(settings, tool),
            )
            .ok_or_else(|| anyhow::anyhow!("invalid shell taskbar rectangle"))?;
            position_taskbar_bar_on_taskbar(app, tool, &taskbar, rect)?;
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = app;
        Err(anyhow::anyhow!("taskbar dock is Windows-only"))
    }
}

#[tauri::command]
async fn get_status() -> Vec<AgentStatus> {
    collect_representatives_off_thread(Settings::load(), false).await
}

#[tauri::command]
async fn refresh_status(
    window: tauri::Window,
    app: tauri::AppHandle,
) -> Result<Vec<AgentStatus>, String> {
    ensure_status_refresh_command(window.label())?;
    let statuses = collect_representatives_off_thread(Settings::load(), true).await;
    let _ = app.emit("status-updated", &statuses);
    Ok(statuses)
}

fn update_error_result() -> update::UpdateCheckResult {
    update::UpdateCheckResult {
        status: "error".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
        latest_version: None,
        release_url: None,
        checked_at: None,
        checked_now: false,
        error: Some("check_failed".into()),
    }
}

async fn run_update_check(force: bool) -> Result<update::UpdateCheckResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        update::check_for_update(env!("CARGO_PKG_VERSION"), force)
    })
    .await
    .map_err(|_| "update check task failed".to_string())?
    .map_err(|_| "update check failed".to_string())
}

fn show_update_notification(app: &tauri::AppHandle, result: &update::UpdateCheckResult) {
    if result.status != "update_available" {
        return;
    }
    let Some(version) = result.latest_version.as_deref() else {
        return;
    };
    if !update::claim_notification(version).unwrap_or(false) {
        return;
    }

    let settings = Settings::load();
    let (title, body) = if notification_uses_korean(&settings.language, system_ui_language()) {
        (
            "Juice 업데이트",
            format!("Juice {version} 버전을 사용할 수 있습니다."),
        )
    } else {
        ("Juice update", format!("Juice {version} is available."))
    };
    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        eprintln!("[update] notification failed: {err}");
    }
}

fn notification_uses_korean(language: &str, system_ui_language: u16) -> bool {
    match language {
        "ko" => true,
        "en" => false,
        _ => system_ui_language & 0x03ff == 0x0012,
    }
}

fn system_ui_language() -> u16 {
    #[cfg(windows)]
    {
        unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() }
    }

    #[cfg(not(windows))]
    {
        0x0409
    }
}

fn spawn_update_check(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(UPDATE_START_DELAY_SECS)).await;
        if !Settings::load().update_check_on {
            return;
        }
        let Ok(result) = run_update_check(false).await else {
            return;
        };
        show_update_notification(&app, &result);
        let _ = app.emit("update-status", &result);
    });
}

#[tauri::command]
fn get_update_status(window: tauri::Window) -> Result<update::UpdateCheckResult, String> {
    ensure_panel_command(window.label())?;
    Ok(update::cached_result(env!("CARGO_PKG_VERSION")))
}

#[tauri::command]
async fn check_for_updates(
    window: tauri::Window,
    app: tauri::AppHandle,
) -> Result<update::UpdateCheckResult, String> {
    ensure_panel_command(window.label())?;
    let result = run_update_check(true)
        .await
        .unwrap_or_else(|_| update_error_result());
    let _ = app.emit("update-status", &result);
    Ok(result)
}

#[tauri::command]
fn open_release_page(window: tauri::Window, url: Option<String>) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    let target = url.unwrap_or_else(|| update::releases_url().to_string());
    if !update::is_release_url_allowed(&target) {
        return Err("release URL is not allowed".into());
    }

    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe")
            .arg(target)
            .spawn()
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = target;
        Err("opening the release page is Windows-only".into())
    }
}

#[tauri::command]
fn get_settings() -> Settings {
    Settings::load()
}

#[tauri::command]
fn save_settings(
    window: tauri::Window,
    app: tauri::AppHandle,
    input: config::SettingsInput,
) -> Result<Settings, String> {
    ensure_panel_command(window.label())?;
    let mut requested = Settings::from_input(input);
    let settings = Settings::update(move |current| {
        requested.claude_taskbar_offset_ratio = current.claude_taskbar_offset_ratio;
        requested.codex_taskbar_offset_ratio = current.codex_taskbar_offset_ratio;
        requested.claude_taskbar_monitor_key = current.claude_taskbar_monitor_key.clone();
        requested.codex_taskbar_monitor_key = current.codex_taskbar_monitor_key.clone();
        *current = requested;
    })
    .map_err(|err| err.to_string())?;
    if let Err(err) = apply_taskbar_dock(&app, &settings) {
        eprintln!("[taskbar] reposition failed: {err}");
    }
    apply_autostart_for_release(&app, &settings);
    let _ = app.emit("settings-updated", &settings);
    Ok(settings)
}

#[tauri::command]
fn move_taskbar_bar(
    window: tauri::Window,
    app: tauri::AppHandle,
    tool: String,
    screen_x: i32,
    grab_offset_x: i32,
    persist: bool,
) -> Result<Settings, String> {
    let mut settings = Settings::load();
    ensure_matching_bar_command(window.label(), &tool)?;

    #[cfg(windows)]
    {
        let tool =
            normalize_taskbar_tool(&tool).ok_or_else(|| "unknown taskbar tool".to_string())?;
        let width = taskbar_dock_width(&settings, tool)
            .ok_or_else(|| "taskbar bar is hidden".to_string())?;
        let taskbar = taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool))
            .map_err(|err| err.to_string())?;
        let (rect, ratio) = taskbar::dock_rect_for_taskbar_drag(
            taskbar.left,
            taskbar.top,
            taskbar.right,
            taskbar.bottom,
            width,
            screen_x,
            grab_offset_x,
        )
        .ok_or_else(|| "invalid shell taskbar rectangle".to_string())?;
        position_taskbar_bar_on_taskbar(&app, tool, &taskbar, rect)
            .map_err(|err| err.to_string())?;
        if persist {
            settings = Settings::update(|current| {
                set_taskbar_target(current, tool, &taskbar.key, ratio);
            })
            .map_err(|err| err.to_string())?;
            let _ = app.emit("settings-updated", &settings);
        } else {
            set_taskbar_target(&mut settings, tool, &taskbar.key, ratio);
        }
        Ok(settings)
    }

    #[cfg(not(windows))]
    {
        let _ = (app, tool, screen_x, grab_offset_x, persist);
        Err("taskbar dock is Windows-only".into())
    }
}

#[tauri::command]
fn pause_taskbar_bars(window: tauri::Window, app: tauri::AppHandle) -> Result<(), String> {
    ensure_taskbar_bar_command(window.label())?;
    pause_taskbar_bars_for_manager(&app);
    Ok(())
}

#[tauri::command]
fn minimize_panel(window: tauri::Window) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    window.minimize().map_err(|err| err.to_string())
}

#[tauri::command]
fn toggle_panel_maximized(window: tauri::Window) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    if window.is_maximized().map_err(|err| err.to_string())? {
        window.unmaximize().map_err(|err| err.to_string())
    } else {
        window.maximize().map_err(|err| err.to_string())
    }
}

#[tauri::command]
fn hide_panel_window(window: tauri::Window) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    window.hide().map_err(|err| err.to_string())
}

#[tauri::command]
fn start_panel_drag(window: tauri::Window) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    window.start_dragging().map_err(|err| err.to_string())
}

fn ensure_panel_command(label: &str) -> Result<(), String> {
    if label == "panel" {
        Ok(())
    } else {
        Err("command is panel-only".into())
    }
}

fn ensure_taskbar_bar_command(label: &str) -> Result<(), String> {
    if TASKBAR_TOOLS
        .iter()
        .filter_map(|tool| taskbar_bar_label(tool))
        .any(|bar_label| bar_label == label)
    {
        Ok(())
    } else {
        Err("command is restricted to taskbar bar windows".into())
    }
}

fn ensure_status_refresh_command(label: &str) -> Result<(), String> {
    if ensure_panel_command(label).is_ok() || ensure_taskbar_bar_command(label).is_ok() {
        Ok(())
    } else {
        Err("command is restricted to panel or taskbar bar windows".into())
    }
}

fn ensure_matching_bar_command(label: &str, tool: &str) -> Result<(), String> {
    match normalize_taskbar_tool(tool) {
        Some("claude") if label == "bar-claude" => Ok(()),
        Some("codex") if label == "bar-codex" => Ok(()),
        Some(_) => Err("command is restricted to its taskbar bar window".into()),
        None => Err("unknown taskbar tool".into()),
    }
}

fn apply_taskbar_dock<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let taskbar_paused = taskbar_bars_paused(manager);
        for tool in TASKBAR_TOOLS {
            let width = match taskbar_dock_width(settings, tool) {
                Some(width) => width,
                None => {
                    hide_taskbar_bar(manager, tool);
                    continue;
                }
            };
            let taskbar =
                taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(settings, tool))?;
            let (fullscreen_active, maximized_active) =
                taskbar_hide_window_state_for_monitor(manager, settings, taskbar.monitor);
            if !should_show_taskbar_bar_with_pause(
                settings,
                tool,
                fullscreen_active,
                maximized_active,
                taskbar_paused,
            ) {
                hide_taskbar_bar(manager, tool);
                continue;
            }

            let target = taskbar::TaskbarTarget {
                key: taskbar.key.clone(),
                rect: taskbar::DockRect {
                    x: taskbar.left,
                    y: taskbar.top,
                    width: taskbar.right - taskbar.left,
                    height: taskbar.bottom - taskbar.top,
                },
                monitor: taskbar.monitor,
                primary: taskbar.primary,
            };
            let rect = taskbar::dock_rect_for_taskbar_target(
                &target,
                width,
                taskbar_offset_ratio(settings, tool),
            )
            .ok_or_else(|| anyhow::anyhow!("invalid shell taskbar rectangle"))?;
            position_taskbar_bar_on_taskbar(manager, tool, &taskbar, rect)?;
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = (manager, settings);
        Ok(())
    }
}

#[cfg(windows)]
fn save_taskbar_drag_target(
    app: &tauri::AppHandle,
    tool: &str,
    monitor_key: &str,
    ratio: f32,
) -> anyhow::Result<()> {
    let settings = Settings::update(|current| {
        set_taskbar_target(current, tool, monitor_key, ratio);
    })?;
    let _ = app.emit("settings-updated", &settings);
    Ok(())
}

#[cfg(windows)]
fn current_bar_rect(
    app: &tauri::AppHandle,
    tool: &str,
) -> anyhow::Result<windows::Win32::Foundation::RECT> {
    use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};

    let label = taskbar_bar_label(tool).ok_or_else(|| anyhow::anyhow!("unknown taskbar tool"))?;
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| anyhow::anyhow!("no {label} window"))?;
    let hwnd = window.hwnd()?;
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect)?;
    }
    Ok(rect)
}

#[cfg(windows)]
fn current_bar_drag_start(
    app: &tauri::AppHandle,
    point: windows::Win32::Foundation::POINT,
) -> Option<(&'static str, i32, i32, i32)> {
    let settings = Settings::load();
    let taskbar_paused = taskbar_bars_paused(app);

    if let Some(tool) = taskbar_tool_at_point(app, point) {
        return current_bar_drag_start_for_tool(app, &settings, tool, point, taskbar_paused);
    }

    for tool in TASKBAR_TOOLS.iter().rev().copied() {
        if let Some(start) =
            current_bar_drag_start_for_tool(app, &settings, tool, point, taskbar_paused)
        {
            return Some(start);
        }
    }
    None
}

#[cfg(windows)]
fn current_bar_drag_start_for_tool(
    app: &tauri::AppHandle,
    settings: &Settings,
    tool: &'static str,
    point: windows::Win32::Foundation::POINT,
    taskbar_paused: bool,
) -> Option<(&'static str, i32, i32, i32)> {
    let taskbar =
        taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(settings, tool)).ok()?;
    let (fullscreen_active, maximized_active) =
        taskbar_hide_window_state_for_monitor(app, settings, taskbar.monitor);
    if !should_show_taskbar_bar_with_pause(
        settings,
        tool,
        fullscreen_active,
        maximized_active,
        taskbar_paused,
    ) {
        return None;
    }

    let rect = current_bar_rect(app, tool).ok()?;
    if !point_inside_rect(point, rect) {
        return None;
    }

    let bar_width = rect.right.checked_sub(rect.left).unwrap_or(1).max(1);
    let bar_height = rect.bottom.checked_sub(rect.top).unwrap_or(1).max(1);
    let desired_length = bar_width.max(bar_height);

    Some((
        tool,
        point.x - rect.left,
        point.y - rect.top,
        desired_length.max(1),
    ))
}

#[cfg(windows)]
fn taskbar_tool_at_point(
    app: &tauri::AppHandle,
    point: windows::Win32::Foundation::POINT,
) -> Option<&'static str> {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};

    let hit = unsafe { WindowFromPoint(point) };
    if hit.0.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    let candidate = if root.0.is_null() { hit } else { root };

    for tool in TASKBAR_TOOLS {
        let Some(label) = taskbar_bar_label(tool) else {
            continue;
        };
        let Some(window) = app.get_webview_window(label) else {
            continue;
        };
        let Ok(hwnd) = window.hwnd() else {
            continue;
        };
        if hwnd == hit || hwnd == candidate {
            return Some(tool);
        }
    }

    None
}

#[cfg(windows)]
fn point_inside_rect(
    point: windows::Win32::Foundation::POINT,
    rect: windows::Win32::Foundation::RECT,
) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

#[cfg(windows)]
fn cursor_position() -> anyhow::Result<windows::Win32::Foundation::POINT> {
    use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};

    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point)?;
    }
    Ok(point)
}

#[cfg(windows)]
fn left_mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

    let state = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) };
    (state as u16 & 0x8000) != 0
}

fn spawn_taskbar_drag_loop(app: tauri::AppHandle) {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn(async move {
            let mut was_down = false;
            let mut drag_tool: Option<&'static str> = None;
            let mut grab_offset_x: Option<i32> = None;
            let mut grab_offset_y: Option<i32> = None;
            let mut drag_length: Option<i32> = None;
            let mut start_x: Option<i32> = None;
            let mut start_y: Option<i32> = None;
            let mut last_ratio: Option<f32> = None;
            let mut last_monitor_key: Option<String> = None;
            let mut emitted_dragging = false;

            loop {
                let down = left_mouse_button_down();
                let point = cursor_position().ok();

                if down {
                    if !was_down {
                        let drag_start =
                            point.and_then(|point| current_bar_drag_start(&app, point));
                        drag_tool = drag_start.map(|(tool, _, _, _)| tool);
                        grab_offset_x = drag_start.map(|(_, grab_offset_x, _, _)| grab_offset_x);
                        grab_offset_y = drag_start.map(|(_, _, grab_offset_y, _)| grab_offset_y);
                        drag_length = drag_start.map(|(_, _, _, length)| length);
                        start_x = point.map(|point| point.x);
                        start_y = point.map(|point| point.y);
                        last_ratio = None;
                        last_monitor_key = None;
                    }

                    if let (
                        Some(tool),
                        Some(point),
                        Some(grab_offset_x),
                        Some(grab_offset_y),
                        Some(length),
                    ) = (drag_tool, point, grab_offset_x, grab_offset_y, drag_length)
                    {
                        let moved = start_x
                            .zip(start_y)
                            .map(|(start_x, start_y)| {
                                (point.x - start_x).abs() > TASKBAR_DRAG_THRESHOLD_PX
                                    || (point.y - start_y).abs() > TASKBAR_DRAG_THRESHOLD_PX
                            })
                            .unwrap_or(false);
                        if !moved {
                            was_down = down;
                            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                            continue;
                        }
                        if !emitted_dragging {
                            let _ = app.emit(
                                "taskbar-dragging-updated",
                                TaskbarDraggingPayload {
                                    tool,
                                    dragging: true,
                                },
                            );
                            emitted_dragging = true;
                        }
                        let settings = Settings::load();
                        let preferred_key = taskbar_monitor_key(&settings, tool).to_string();
                        if let Ok((taskbar, rect, ratio)) =
                            taskbar::shell_taskbar_drag_rect_at_point_for_key(
                                length,
                                point.x,
                                point.y,
                                grab_offset_x,
                                grab_offset_y,
                                &preferred_key,
                            )
                        {
                            if position_taskbar_bar_on_taskbar(&app, tool, &taskbar, rect).is_ok() {
                                last_ratio = Some(ratio);
                                last_monitor_key = Some(taskbar.key);
                            }
                        }
                    }
                } else if was_down {
                    if let (Some(tool), Some(ratio), Some(monitor_key)) =
                        (drag_tool, last_ratio.take(), last_monitor_key.take())
                    {
                        if let Err(err) = save_taskbar_drag_target(&app, tool, &monitor_key, ratio)
                        {
                            eprintln!("[taskbar] save dragged {tool} bar position failed: {err}");
                        }
                    }
                    if emitted_dragging {
                        if let Some(tool) = drag_tool {
                            let _ = app.emit(
                                "taskbar-dragging-updated",
                                TaskbarDraggingPayload {
                                    tool,
                                    dragging: false,
                                },
                            );
                        }
                        emitted_dragging = false;
                    }
                    drag_tool = None;
                    grab_offset_x = None;
                    grab_offset_y = None;
                    drag_length = None;
                    start_x = None;
                    start_y = None;
                    last_monitor_key = None;
                }

                was_down = down;
                tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            }
        });
    }

    #[cfg(not(windows))]
    {
        let _ = app;
    }
}

fn spawn_taskbar_visibility_loop(app: tauri::AppHandle) {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn(async move {
            loop {
                if !left_mouse_button_down() {
                    let settings = Settings::load();
                    let _ = apply_taskbar_dock(&app, &settings);
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    #[cfg(not(windows))]
    {
        let _ = app;
    }
}

#[tauri::command]
fn install_statusline(window: tauri::Window) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    Settings::install_statusline_wrap(&statusline_bridge_path()?).map_err(|err| err.to_string())
}

#[tauri::command]
fn restore_statusline(window: tauri::Window) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    Settings::restore_statusline().map_err(|err| err.to_string())
}

fn statusline_bridge_path() -> Result<String, String> {
    let name = if cfg!(windows) {
        "agentjuice-statusline.exe"
    } else {
        "agentjuice-statusline"
    };

    std::env::current_exe()
        .map_err(|err| err.to_string())
        .map(|path| {
            path.with_file_name(name)
                .to_string_lossy()
                .replace('\\', "/")
        })
}

fn auto_connect_statusline_for_release() {
    if cfg!(debug_assertions) {
        return;
    }

    let result = statusline_bridge_path().and_then(|bridge| {
        Settings::install_statusline_wrap(&bridge).map_err(|err| err.to_string())
    });
    if let Err(err) = result {
        eprintln!("[statusline] auto-connect failed: {err}");
    }
}

fn apply_autostart_for_release<R, M>(manager: &M, settings: &Settings)
where
    R: tauri::Runtime,
    M: tauri::Manager<R>,
{
    if cfg!(debug_assertions) {
        return;
    }

    if !settings.autostart_on {
        if let Err(err) = manager.autolaunch().disable() {
            eprintln!("[autostart] disable failed: {err}");
        }
        return;
    }

    if let Err(err) = manager.autolaunch().enable() {
        eprintln!("[autostart] enable failed: {err}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            get_status,
            refresh_status,
            get_update_status,
            check_for_updates,
            open_release_page,
            get_settings,
            save_settings,
            move_taskbar_bar,
            pause_taskbar_bars,
            minimize_panel,
            toggle_panel_maximized,
            hide_panel_window,
            start_panel_drag,
            install_statusline,
            restore_statusline
        ])
        .setup(|app| {
            app.manage(TaskbarPauseState::default());
            let settings = Settings::load();
            if let Err(err) = try_setup_taskbar_dock(app, &settings) {
                eprintln!("[taskbar] fallback to tray: {err}");
            }
            setup_panel_close_hide(app);
            setup_trays(app)?;
            apply_autostart_for_release(app, &settings);
            auto_connect_statusline_for_release();
            spawn_status_loop(app.handle().clone());
            spawn_taskbar_drag_loop(app.handle().clone());
            spawn_taskbar_visibility_loop(app.handle().clone());
            spawn_update_check(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use crate::{
        config::Settings,
        model::{AccountLimit, AgentStatus, SessionInfo, Tool},
    };

    #[test]
    fn placeholder_tray_uses_product_tooltip() {
        assert_eq!(super::tray_tooltip(), "Juice");
    }

    #[test]
    fn tray_uses_one_product_icon_id() {
        assert_eq!(super::tray_icon_ids(), ["juice"]);
        assert!(!super::tray_icon_ids()
            .iter()
            .any(|id| id.starts_with("aj-")));
    }

    #[test]
    fn tray_menu_exposes_open_and_quit_actions() {
        assert_eq!(super::tray_open_menu_id(), "juice-open");
        assert_eq!(super::tray_pause_bar_menu_id(), "juice-pause-bars");
        assert_eq!(super::tray_resume_bar_menu_id(), "juice-resume-bars");
        assert_eq!(super::tray_quit_menu_id(), "juice-quit");
    }

    #[test]
    fn update_notification_follows_explicit_or_system_ui_language() {
        assert!(super::notification_uses_korean("ko", 0x0409));
        assert!(!super::notification_uses_korean("en", 0x0412));
        assert!(super::notification_uses_korean("system", 0x0412));
        assert!(!super::notification_uses_korean("system", 0x0409));
    }

    #[test]
    fn taskbar_bar_visibility_follows_tool_display_settings() {
        let mut settings = Settings::default();
        assert_eq!(super::taskbar_bar_label("claude"), Some("bar-claude"));
        assert_eq!(super::taskbar_bar_label("codex"), Some("bar-codex"));
        assert!(super::should_show_taskbar_bar(&settings, "claude"));
        assert!(super::should_show_taskbar_bar(&settings, "codex"));
        assert!(!super::should_show_taskbar_bar_with_fullscreen(
            &settings, "claude", true
        ));
        assert!(!super::should_show_taskbar_bar_with_fullscreen(
            &settings, "codex", true
        ));
        assert!(super::should_show_taskbar_bar_with_window_state(
            &settings, "codex", false, true
        ));
        assert!(!super::should_show_taskbar_bar_with_pause(
            &settings, "codex", false, false, true
        ));
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(260));
        assert_eq!(super::taskbar_dock_width(&settings, "codex"), Some(260));

        settings.fullscreen_hide_on = false;
        assert!(super::should_show_taskbar_bar_with_fullscreen(
            &settings, "claude", true
        ));
        assert!(super::should_show_taskbar_bar_with_fullscreen(
            &settings, "codex", true
        ));
        settings.fullscreen_hide_on = true;
        settings.maximized_hide_on = true;
        assert!(!super::should_show_taskbar_bar_with_window_state(
            &settings, "claude", false, true
        ));
        assert!(!super::should_show_taskbar_bar_with_window_state(
            &settings, "codex", false, true
        ));
        settings.maximized_hide_on = false;

        settings.show_claude = false;
        assert!(!super::should_show_taskbar_bar(&settings, "claude"));
        assert!(super::should_show_taskbar_bar(&settings, "codex"));
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), None);
        assert_eq!(super::taskbar_dock_width(&settings, "codex"), Some(260));

        settings.show_codex = false;
        assert!(!super::should_show_taskbar_bar(&settings, "claude"));
        assert!(!super::should_show_taskbar_bar(&settings, "codex"));
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), None);
        assert_eq!(super::taskbar_dock_width(&settings, "codex"), None);
    }

    #[test]
    fn taskbar_offsets_are_saved_per_tool() {
        let mut settings = Settings::default();

        assert_eq!(super::taskbar_offset_ratio(&settings, "claude"), 0.5);
        assert_eq!(super::taskbar_offset_ratio(&settings, "codex"), 0.5);

        super::set_taskbar_offset_ratio(&mut settings, "claude", 0.2);
        super::set_taskbar_offset_ratio(&mut settings, "codex", 0.8);

        assert_eq!(settings.claude_taskbar_offset_ratio, 0.2);
        assert_eq!(settings.codex_taskbar_offset_ratio, 0.8);
    }

    #[test]
    fn ipc_command_guards_restrict_sensitive_calls_to_expected_windows() {
        assert!(super::ensure_panel_command("panel").is_ok());
        assert!(super::ensure_panel_command("bar-claude").is_err());
        assert!(super::ensure_taskbar_bar_command("bar-claude").is_ok());
        assert!(super::ensure_taskbar_bar_command("bar-codex").is_ok());
        assert!(super::ensure_taskbar_bar_command("panel").is_err());
        assert!(super::ensure_status_refresh_command("panel").is_ok());
        assert!(super::ensure_status_refresh_command("bar-claude").is_ok());
        assert!(super::ensure_status_refresh_command("bar-codex").is_ok());
        assert!(super::ensure_status_refresh_command("settings").is_err());

        assert!(super::ensure_matching_bar_command("bar-claude", "claude").is_ok());
        assert!(super::ensure_matching_bar_command("bar-codex", "codex").is_ok());
        assert!(super::ensure_matching_bar_command("bar-claude", "codex").is_err());
        assert!(super::ensure_matching_bar_command("panel", "claude").is_err());
        assert!(super::ensure_matching_bar_command("bar-claude", "unknown").is_err());
    }

    #[test]
    fn tray_png_for_status_returns_png() {
        let status = AgentStatus {
            schema_version: "agent_status.v1".into(),
            pc_id: "PC".into(),
            tool: Tool::Claude,
            session_id: "s1".into(),
            captured_at: "2026-07-07T00:00:00Z".into(),
            primary: Some(AccountLimit {
                label: "5h".into(),
                used_percent: Some(88.0),
                resets_at: None,
            }),
            secondary: Some(AccountLimit {
                label: "week".into(),
                used_percent: Some(41.0),
                resets_at: None,
            }),
            session: SessionInfo {
                active: true,
                context_used_percent: Some(63.0),
            },
            cost_estimate_usd: None,
            approx: true,
        };

        let png = super::tray_png_for_status(&status, &Settings::default()).unwrap();
        assert!(png.len() > 8 && &png[1..4] == b"PNG");
    }

    #[test]
    fn claude_usage_merge_fills_missing_limits_without_overwriting_statusline_limits() {
        let mut statusline = status_for_signature("statusline");

        let usage = AgentStatus {
            schema_version: "agent_status.v1".into(),
            pc_id: "PC".into(),
            tool: Tool::Claude,
            session_id: "claude-usage".into(),
            captured_at: "2026-07-09T12:00:00Z".into(),
            primary: Some(AccountLimit {
                label: "5h".into(),
                used_percent: Some(12.0),
                resets_at: Some("2026-07-09T15:00:00Z".into()),
            }),
            secondary: Some(AccountLimit {
                label: "week".into(),
                used_percent: Some(38.0),
                resets_at: Some("2026-07-14T01:00:00Z".into()),
            }),
            session: SessionInfo {
                active: true,
                context_used_percent: None,
            },
            cost_estimate_usd: None,
            approx: true,
        };

        let merged =
            super::merge_claude_usage_status(Some(statusline.clone()), Some(usage.clone()))
                .unwrap();
        assert_eq!(merged.session_id, "statusline");
        assert_eq!(merged.primary.as_ref().unwrap().used_percent, Some(12.0));
        assert_eq!(merged.secondary.as_ref().unwrap().used_percent, Some(38.0));
        assert_eq!(
            merged.primary.as_ref().unwrap().resets_at.as_deref(),
            Some("2026-07-09T15:00:00Z")
        );

        statusline.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(77.0),
            resets_at: Some("2026-07-09T15:00:00Z".into()),
        });
        statusline.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(55.0),
            resets_at: Some("2026-07-14T01:00:00Z".into()),
        });
        let preserved =
            super::merge_claude_usage_status(Some(statusline), Some(usage.clone())).unwrap();
        assert_eq!(
            preserved.secondary.as_ref().unwrap().used_percent,
            Some(55.0)
        );
        assert_eq!(preserved.primary.as_ref().unwrap().used_percent, Some(77.0));

        let usage_only = super::merge_claude_usage_status(None, Some(usage)).unwrap();
        assert_eq!(usage_only.session_id, "claude-usage");
        assert_eq!(
            usage_only.primary.as_ref().unwrap().used_percent,
            Some(12.0)
        );
        assert_eq!(
            usage_only.secondary.as_ref().unwrap().used_percent,
            Some(38.0)
        );
    }

    #[test]
    fn claude_usage_merge_preserves_statusline_freshness_and_reset_metadata() {
        let mut statusline = status_for_signature("statusline-stale");
        statusline.captured_at = "2026-07-09T00:00:00Z".into();
        statusline.session.active = false;
        statusline.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: None,
            resets_at: Some("2026-07-09T15:00:00Z".into()),
        });

        let mut usage = status_for_signature("claude-usage");
        usage.captured_at = "2026-07-10T00:00:00Z".into();
        usage.session.active = true;
        usage.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(24.0),
            resets_at: None,
        });

        let merged = super::merge_claude_usage_status(Some(statusline), Some(usage)).unwrap();

        assert_eq!(merged.captured_at, "2026-07-09T00:00:00Z");
        assert!(!merged.session.active);
        assert_eq!(merged.primary.as_ref().unwrap().used_percent, Some(24.0));
        assert_eq!(
            merged.primary.as_ref().unwrap().resets_at.as_deref(),
            Some("2026-07-09T15:00:00Z")
        );
    }

    #[test]
    fn exact_claude_oauth_usage_overrides_stale_statusline_account_limits() {
        let mut statusline = status_for_signature("statusline-stale");
        statusline.captured_at = "2026-07-09T00:00:00Z".into();
        statusline.session.active = false;
        statusline.session.context_used_percent = Some(63.0);
        statusline.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(0.0),
            resets_at: None,
        });
        statusline.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(0.0),
            resets_at: None,
        });

        let mut oauth = status_for_signature("claude-oauth-usage");
        oauth.captured_at = "2026-07-10T03:00:00Z".into();
        oauth.session.active = true;
        oauth.session.context_used_percent = None;
        oauth.approx = false;
        oauth.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(78.0),
            resets_at: Some("2026-07-10T04:50:00Z".into()),
        });
        oauth.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(10.0),
            resets_at: Some("2026-07-16T12:00:00Z".into()),
        });

        let merged = super::merge_claude_usage_status(Some(statusline), Some(oauth)).unwrap();
        assert_eq!(merged.captured_at, "2026-07-10T03:00:00Z");
        assert!(merged.session.active);
        assert_eq!(merged.session.context_used_percent, Some(63.0));
        assert_eq!(merged.primary.as_ref().unwrap().used_percent, Some(78.0));
        assert_eq!(merged.secondary.as_ref().unwrap().used_percent, Some(10.0));
        assert!(!merged.approx);
    }

    #[test]
    fn failed_collection_attempt_is_cached_but_force_bypasses_it() {
        use chrono::{Duration, TimeZone, Utc};
        use std::sync::{atomic::AtomicUsize, Mutex};

        let cache = Mutex::new(None);
        let attempts = AtomicUsize::new(0);
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 0, 0, 0).unwrap();

        let first = super::cached_status_attempt(&cache, now, 30, false, || {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        });
        let cached =
            super::cached_status_attempt(&cache, now + Duration::seconds(1), 30, false, || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(status_for_signature("should-not-run"))
            });
        let forced =
            super::cached_status_attempt(&cache, now + Duration::seconds(1), 30, true, || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(status_for_signature("forced"))
            });

        assert!(first.is_none());
        assert!(cached.is_none());
        assert_eq!(forced.unwrap().session_id, "forced");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn overlapping_collection_requests_share_one_result() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        };

        const THREADS: usize = 8;
        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let barrier = Arc::new(Barrier::new(THREADS));
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();

        for _ in 0..THREADS {
            let coordinator = Arc::clone(&coordinator);
            let barrier = Arc::clone(&barrier);
            let attempts = Arc::clone(&attempts);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                coordinator.run(false, || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    vec![status_for_signature("shared")]
                })
            }));
        }

        for worker in workers {
            let result = worker.join().unwrap();
            assert_eq!(result[0].session_id, "shared");
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_collection_runs_after_an_overlapping_normal_collection() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc, Barrier,
        };

        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = mpsc::channel();

        let normal_coordinator = Arc::clone(&coordinator);
        let normal_attempts = Arc::clone(&attempts);
        let normal_release = Arc::clone(&release);
        let normal = std::thread::spawn(move || {
            normal_coordinator.run(false, || {
                normal_attempts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                normal_release.wait();
                vec![status_for_signature("normal")]
            })
        });
        started_rx.recv().unwrap();

        let forced_coordinator = Arc::clone(&coordinator);
        let forced_attempts = Arc::clone(&attempts);
        let forced = std::thread::spawn(move || {
            forced_coordinator.run(true, || {
                forced_attempts.fetch_add(1, Ordering::SeqCst);
                vec![status_for_signature("forced")]
            })
        });
        release.wait();

        assert_eq!(normal.join().unwrap()[0].session_id, "normal");
        assert_eq!(forced.join().unwrap()[0].session_id, "forced");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn panel_save_and_native_drag_preserve_each_others_settings() {
        use std::sync::{Arc, Barrier};

        let root = std::env::temp_dir().join(format!(
            "agent-juice-settings-race-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        Settings::default().save_to(&path).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let panel_path = path.clone();
        let panel_barrier = Arc::clone(&barrier);
        let panel = std::thread::spawn(move || {
            panel_barrier.wait();
            Settings::update_at(&panel_path, |current| {
                let claude_monitor = current.claude_taskbar_monitor_key.clone();
                let codex_monitor = current.codex_taskbar_monitor_key.clone();
                let claude_offset = current.claude_taskbar_offset_ratio;
                let codex_offset = current.codex_taskbar_offset_ratio;
                let requested = Settings {
                    theme: "dark".into(),
                    claude_taskbar_offset_ratio: claude_offset,
                    codex_taskbar_offset_ratio: codex_offset,
                    claude_taskbar_monitor_key: claude_monitor,
                    codex_taskbar_monitor_key: codex_monitor,
                    ..Settings::default()
                };
                *current = requested;
            })
            .unwrap();
        });
        let drag_path = path.clone();
        let drag_barrier = Arc::clone(&barrier);
        let drag = std::thread::spawn(move || {
            drag_barrier.wait();
            Settings::update_at(&drag_path, |current| {
                current.codex_taskbar_monitor_key = "monitor:secondary".into();
                current.codex_taskbar_offset_ratio = 0.8;
            })
            .unwrap();
        });

        panel.join().unwrap();
        drag.join().unwrap();
        let saved = Settings::load_from(&path);
        assert_eq!(saved.theme, "dark");
        assert_eq!(saved.codex_taskbar_monitor_key, "monitor:secondary");
        assert_eq!(saved.codex_taskbar_offset_ratio, 0.8);

        let temp_files = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".aj-tmp"))
            .count();
        assert_eq!(temp_files, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn status_payload_signature_changes_with_status_content() {
        let first = vec![status_for_signature("a")];
        let second = vec![status_for_signature("b")];

        assert_eq!(
            super::status_payload_signature(&first),
            super::status_payload_signature(&first)
        );
        assert_ne!(
            super::status_payload_signature(&first),
            super::status_payload_signature(&second)
        );
    }

    #[cfg(windows)]
    #[test]
    fn bar_overlay_style_keeps_no_activate_tool_window_and_topmost() {
        use windows::Win32::UI::WindowsAndMessaging::{
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        };

        let style = super::bar_overlay_ex_style(0);

        assert_ne!(style & WS_EX_NOACTIVATE.0 as isize, 0);
        assert_ne!(style & WS_EX_TOOLWINDOW.0 as isize, 0);
        assert_ne!(style & WS_EX_TOPMOST.0 as isize, 0);
    }

    #[cfg(windows)]
    #[test]
    fn bar_overlay_window_style_stays_top_level_for_drag_input() {
        use windows::Win32::UI::WindowsAndMessaging::{WS_CHILD, WS_POPUP};

        let style = super::bar_overlay_window_style(WS_CHILD.0 as isize);

        assert_eq!(style & WS_CHILD.0 as isize, 0);
        assert_ne!(style & WS_POPUP.0 as isize, 0);
    }

    fn status_for_signature(session_id: &str) -> AgentStatus {
        AgentStatus {
            schema_version: "agent_status.v1".into(),
            pc_id: "PC".into(),
            tool: Tool::Claude,
            session_id: session_id.into(),
            captured_at: "2026-07-07T00:00:00Z".into(),
            primary: None,
            secondary: None,
            session: SessionInfo {
                active: true,
                context_used_percent: None,
            },
            cost_estimate_usd: None,
            approx: true,
        }
    }
}
