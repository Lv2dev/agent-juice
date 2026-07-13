pub mod adapters;
#[cfg(windows)]
pub mod appbar;
pub mod collector;
pub mod config;
pub mod model;
pub mod paths;
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
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;

const TASKBAR_DOCK_PADDING: f32 = 0.0;
const TASKBAR_FULL_TEXT_BUDGET: f32 = 179.0;
const TASKBAR_FULL_RESET_TEXT_BUDGET: f32 = 276.0;
const TASKBAR_COMPACT_TEXT_BUDGET: f32 = 135.0;
const TASKBAR_CONTENT_GAP_BASE: f32 = 6.0;
const TASKBAR_QUAD_GAP: f32 = 7.0;
const TASKBAR_INDICATOR_HORIZONTAL_PADDING: f32 = 1.0;
const TASKBAR_MENU_WIDTH: i32 = 96;
const TASKBAR_DRAG_THRESHOLD_PX: i32 = 3;
const TASKBAR_TOOLTIP_DELAY_MS: u64 = 450;
const TASKBAR_TOOLS: [&str; 2] = ["claude", "codex"];
const CODEX_REPRESENTATIVE_CANDIDATES: usize = 32;
const CODEX_ACCOUNT_CACHE_MIN_SECS: i64 = 30;
const CODEX_ACCOUNT_API_TIMEOUT_SECS: u64 = 5;
const CODEX_ROLLOUT_CACHE_MAX_AGE_SECS: u64 = 60;
const CLAUDE_USAGE_CACHE_MIN_SECS: i64 = 60;
const CLAUDE_USAGE_TIMEOUT_SECS: u64 = 10;
const COLLECTION_REFRESH_DEADLINE_SECS: u64 = 15;
const CLAUDE_FALLBACK_RESERVE_SECS: u64 = 2;
const CLAUDE_COLLECTION_MIN_BUDGET_SECS: u64 = 2;
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

#[derive(Default)]
struct TaskbarMenuState {
    claude: Mutex<TaskbarMenuLayout>,
    codex: Mutex<TaskbarMenuLayout>,
}

#[derive(Default, Clone, Copy)]
struct TaskbarMenuLayout {
    open: bool,
    ratio: Option<f32>,
}

#[derive(Default)]
struct TaskbarRecoveryState(AtomicBool);

#[derive(Default)]
struct TaskbarDragState(AtomicBool);

#[derive(Default)]
struct TaskbarTooltipTextState(Mutex<std::collections::HashMap<&'static str, String>>);

#[derive(Clone, Debug, PartialEq)]
struct TaskbarContentLayout {
    mode: String,
    width: i32,
    ratio: Option<f32>,
}

#[derive(Default)]
struct TaskbarContentLayoutState {
    claude: Mutex<Option<TaskbarContentLayout>>,
    codex: Mutex<Option<TaskbarContentLayout>>,
}

#[derive(Default)]
struct TaskbarShutdownState(AtomicBool);

#[derive(Default)]
struct QuitPendingState(AtomicBool);

#[derive(Debug, Clone, PartialEq, Eq)]
enum CollectionErrorKind {
    Deadline,
    Transport,
    Parse,
}

#[derive(Clone)]
struct CachedStatusAttempt {
    attempted_at: DateTime<Utc>,
    last_good: Option<AgentStatus>,
    error: Option<CollectionErrorKind>,
    retry_at: DateTime<Utc>,
}

#[derive(Default)]
struct CollectionFlight {
    in_flight: bool,
    in_flight_force: bool,
    active_generation: u64,
    completed_generation: u64,
    pending_force_generation: Option<u64>,
    pending_force_waiters: usize,
    joined_waiters: usize,
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
        let target_generation = if state.in_flight {
            state.joined_waiters = state.joined_waiters.saturating_add(1);
            if force && !state.in_flight_force {
                let next = state.active_generation.saturating_add(1);
                state.pending_force_waiters = state.pending_force_waiters.saturating_add(1);
                *state.pending_force_generation.get_or_insert(next)
            } else {
                state.active_generation
            }
        } else if force {
            state
                .pending_force_generation
                .take()
                .unwrap_or_else(|| state.completed_generation.saturating_add(1))
        } else if let Some(pending) = state.pending_force_generation {
            pending
        } else {
            state.completed_generation.saturating_add(1)
        };

        loop {
            if state.completed_generation >= target_generation {
                return state.last_result.clone();
            }
            if !state.in_flight
                && (force || state.pending_force_generation != Some(target_generation))
            {
                state.pending_force_generation = state
                    .pending_force_generation
                    .filter(|generation| *generation != target_generation);
                state.in_flight = true;
                state.in_flight_force = force;
                state.active_generation = target_generation;
                break;
            }
            if !state.in_flight && force {
                state.pending_force_generation = None;
                state.in_flight = true;
                state.in_flight_force = true;
                state.active_generation = target_generation;
                break;
            }
            state = self
                .completed
                .wait(state)
                .unwrap_or_else(|err| err.into_inner());
        }
        drop(state);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(collect));
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.in_flight = false;
        state.in_flight_force = false;
        state.completed_generation = target_generation;
        state.joined_waiters = 0;
        if force {
            state.pending_force_waiters = 0;
        }
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
static CODEX_ROLLOUT_CACHE: Lazy<Mutex<collector::RolloutCache>> =
    Lazy::new(|| Mutex::new(collector::RolloutCache::default()));
static CODEX_ROLLOUT_STATUS_CACHE: Lazy<Mutex<Option<AgentStatus>>> =
    Lazy::new(|| Mutex::new(None));
static TASKBAR_LAYOUT_GATE: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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

pub fn tray_refresh_menu_id() -> &'static str {
    "juice-refresh"
}

pub fn tray_resume_bar_menu_id() -> &'static str {
    "juice-resume-bars"
}

pub fn tray_quit_menu_id() -> &'static str {
    "juice-quit"
}

fn exit_after_taskbar_cleanup(app: tauri::AppHandle) {
    if let Some(state) = app.try_state::<TaskbarShutdownState>() {
        state.0.store(true, Ordering::Release);
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        app.exit(0);
    });
}

fn request_app_quit(app: &tauri::AppHandle) {
    let Some(pending) = app.try_state::<QuitPendingState>() else {
        exit_after_taskbar_cleanup(app.clone());
        return;
    };
    if pending.0.swap(true, Ordering::AcqRel) {
        return;
    }

    let _ = app.emit("app-quit-requested", ());
    let fallback = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if fallback
            .try_state::<QuitPendingState>()
            .is_some_and(|state| state.0.swap(false, Ordering::AcqRel))
        {
            exit_after_taskbar_cleanup(fallback);
        }
    });
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
    let data_dir = paths::data_dir();
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
    let data_dir = paths::data_dir();
    let codex_sessions_dir = dirs::home_dir().map(|home| home.join(".codex").join("sessions"));
    let now = Utc::now();

    COLLECTION_COORDINATOR.run(force_codex_account || force_claude_usage, || {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(COLLECTION_REFRESH_DEADLINE_SECS);
        collect_representatives_runtime(
            settings,
            data_dir.as_deref(),
            codex_sessions_dir.as_deref(),
            now,
            force_codex_account,
            force_claude_usage,
            deadline,
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
    deadline: std::time::Instant,
) -> Vec<AgentStatus> {
    let pc_id = gethostname::gethostname().to_string_lossy().to_string();
    let mut statuses = Vec::new();
    let collect_claude = force_claude_usage || settings.claude_account_auto_collect_on;
    let claude_reserve = if collect_claude {
        std::time::Duration::from_secs(CLAUDE_COLLECTION_MIN_BUDGET_SECS)
    } else {
        std::time::Duration::ZERO
    };
    let rollout_deadline = deadline_with_reserve(
        deadline,
        std::time::Duration::from_secs(CODEX_ACCOUNT_API_TIMEOUT_SECS) + claude_reserve,
    );

    let claude_status = recent_matching_files(data_dir, |name| {
        name.starts_with("claude_last.") && name.ends_with(".json")
    })
    .into_iter()
    .find_map(|path| parse_claude_status_file(&path, settings, &pc_id, now));
    let codex_rollout = codex_sessions_dir.and_then(|sessions_dir| {
        collect_codex_rollout_status(
            sessions_dir,
            settings,
            &pc_id,
            now,
            force_codex_account,
            rollout_deadline,
        )
    });
    let codex_account = collect_codex_account_status(
        settings,
        &pc_id,
        now,
        force_codex_account,
        deadline,
        claude_reserve,
    );
    let claude_usage = if collect_claude {
        collect_claude_usage_status(settings, &pc_id, now, force_claude_usage, deadline)
    } else {
        None
    };
    if let Some(status) = merge_claude_usage_status(claude_status, claude_usage) {
        statuses.push(status);
    }

    if let Some(status) = merge_codex_account_status(codex_rollout, codex_account) {
        statuses.push(status);
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

    if let Some(status) = recent_matching_files(data_dir, |name| {
        name.starts_with("claude_last.") && name.ends_with(".json")
    })
    .into_iter()
    .find_map(|path| parse_claude_status_file(&path, settings, &pc_id, now))
    {
        statuses.push(status);
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

fn recent_matching_files(
    dir: Option<&std::path::Path>,
    matches_name: impl Fn(&str) -> bool,
) -> Vec<std::path::PathBuf> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut candidates: Vec<_> = entries
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
        .collect();
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    candidates.into_iter().map(|(_, path)| path).collect()
}

fn recent_codex_rollouts(
    sessions_dir: &std::path::Path,
    force: bool,
    deadline: std::time::Instant,
) -> Vec<std::path::PathBuf> {
    CODEX_ROLLOUT_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .recent_with_deadline(
            sessions_dir,
            CODEX_REPRESENTATIVE_CANDIDATES,
            force,
            std::time::Duration::from_secs(CODEX_ROLLOUT_CACHE_MAX_AGE_SECS),
            std::time::Instant::now(),
            Some(deadline),
        )
}

fn collect_codex_rollout_status(
    sessions_dir: &std::path::Path,
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    force: bool,
    deadline: std::time::Instant,
) -> Option<AgentStatus> {
    for path in recent_codex_rollouts(sessions_dir, force, deadline) {
        if std::time::Instant::now() >= deadline {
            break;
        }
        if let Some(status) = parse_codex_status_file_until(&path, settings, pc_id, now, deadline) {
            *CODEX_ROLLOUT_STATUS_CACHE
                .lock()
                .unwrap_or_else(|err| err.into_inner()) = Some(status.clone());
            return Some(status);
        }
    }

    CODEX_ROLLOUT_STATUS_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
        .map(|mut status| {
            derive_active(&mut status, settings.stale_after_secs, now);
            status
        })
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

fn parse_codex_status_file_until(
    path: &std::path::Path,
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    deadline: std::time::Instant,
) -> Option<AgentStatus> {
    let session_id = collector::session_id_of(path);
    let captured_at = rfc3339_of_mtime(path, now);
    let line = collector::last_token_count_line_from_file_until(
        path,
        deadline,
        collector::MAX_ROLLOUT_TAIL_BYTES,
    )
    .ok()??;
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
    collect: impl FnOnce() -> Result<AgentStatus, CollectionErrorKind>,
) -> Option<AgentStatus> {
    if !force {
        let cache = cache.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = cache.as_ref() {
            let retry_backoff_active = cached.error.is_some() && now < cached.retry_at;
            let success_cache_fresh = cached.error.is_none() && now < cached.retry_at;
            if now >= cached.attempted_at && (retry_backoff_active || success_cache_fresh) {
                return cached.last_good.clone();
            }
        }
    }

    let cache_guard = cache.lock().unwrap_or_else(|err| err.into_inner());
    let previous_last_good = cache_guard
        .as_ref()
        .and_then(|cached| cached.last_good.clone());
    drop(cache_guard);

    let outcome = collect();
    let (last_good, error) = match outcome {
        Ok(status) => (Some(status), None),
        Err(error) => (previous_last_good, Some(error)),
    };
    let retry_at = now + chrono::Duration::seconds(minimum_interval_secs);
    let mut cache_guard = cache.lock().unwrap_or_else(|err| err.into_inner());
    *cache_guard = Some(CachedStatusAttempt {
        attempted_at: now,
        last_good: last_good.clone(),
        error,
        retry_at,
    });
    last_good
}

fn remaining_refresh_budget(
    deadline: std::time::Instant,
    subprocess_limit: std::time::Duration,
) -> Result<std::time::Duration, CollectionErrorKind> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(CollectionErrorKind::Deadline);
    }
    Ok(remaining.min(subprocess_limit))
}

fn deadline_with_reserve(
    deadline: std::time::Instant,
    reserve: std::time::Duration,
) -> std::time::Instant {
    let now = std::time::Instant::now();
    now + deadline
        .saturating_duration_since(now)
        .saturating_sub(reserve)
}

fn remaining_refresh_budget_with_reserve(
    deadline: std::time::Instant,
    subprocess_limit: std::time::Duration,
    reserve: std::time::Duration,
) -> Result<std::time::Duration, CollectionErrorKind> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let available = remaining.saturating_sub(reserve);
    if available.is_zero() {
        return Err(CollectionErrorKind::Deadline);
    }
    Ok(available.min(subprocess_limit))
}

fn collect_codex_account_status(
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    force: bool,
    deadline: std::time::Instant,
    reserve: std::time::Duration,
) -> Option<AgentStatus> {
    let mut status = cached_status_attempt(
        &CODEX_ACCOUNT_CACHE,
        now,
        CODEX_ACCOUNT_CACHE_MIN_SECS,
        force,
        || {
            let timeout = remaining_refresh_budget_with_reserve(
                deadline,
                std::time::Duration::from_secs(CODEX_ACCOUNT_API_TIMEOUT_SECS),
                reserve,
            )?;
            let raw = collector::codex_account_rate_limits_response(timeout)
                .map_err(|_| CollectionErrorKind::Transport)?;
            adapters::codex::parse_account_rate_limits_response(&raw, pc_id, &now.to_rfc3339())
                .map_err(|_| CollectionErrorKind::Parse)
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
    deadline: std::time::Instant,
) -> Option<AgentStatus> {
    let mut status = cached_status_attempt(
        &CLAUDE_USAGE_CACHE,
        now,
        CLAUDE_USAGE_CACHE_MIN_SECS,
        force,
        || {
            let captured_at = now.to_rfc3339();
            let oauth_timeout = remaining_refresh_budget_with_reserve(
                deadline,
                std::time::Duration::from_secs(CLAUDE_USAGE_TIMEOUT_SECS),
                std::time::Duration::from_secs(CLAUDE_FALLBACK_RESERVE_SECS),
            );
            let mut oauth_error = CollectionErrorKind::Transport;
            if let Ok(oauth_timeout) = oauth_timeout {
                if let Ok(raw) = collector::claude_oauth_usage_response(oauth_timeout) {
                    match adapters::claude::parse_oauth_usage_response(&raw, pc_id, &captured_at) {
                        Ok(status) => return Ok(status),
                        Err(_) => oauth_error = CollectionErrorKind::Parse,
                    }
                }
            }

            let legacy_timeout = remaining_refresh_budget(
                deadline,
                std::time::Duration::from_secs(CLAUDE_USAGE_TIMEOUT_SECS),
            )?;
            let raw = collector::claude_usage_output(legacy_timeout).map_err(|_| oauth_error)?;
            adapters::claude::parse_usage_output(&raw, pc_id, &captured_at)
                .map_err(|_| CollectionErrorKind::Parse)
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
        (Some(statusline), Some(usage)) if statusline.session.active && !usage.session.active => {
            Some(statusline)
        }
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
            merge_preferred_limit(&mut statusline.primary, usage.primary);
            merge_preferred_limit(&mut statusline.secondary, usage.secondary);
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

fn merge_codex_account_status(
    rollout: Option<AgentStatus>,
    account: Option<AgentStatus>,
) -> Option<AgentStatus> {
    match (rollout, account) {
        (Some(rollout), Some(account)) if rollout.session.active && !account.session.active => {
            Some(rollout)
        }
        (Some(mut rollout), Some(account)) => {
            merge_authoritative_limit(&mut rollout.primary, account.primary);
            merge_authoritative_limit(&mut rollout.secondary, account.secondary);
            rollout.approx = false;
            Some(rollout)
        }
        (Some(rollout), None) => Some(rollout),
        (None, Some(account)) => Some(account),
        (None, None) => None,
    }
}

fn merge_authoritative_limit(
    fallback_limit: &mut Option<model::AccountLimit>,
    authoritative_limit: Option<model::AccountLimit>,
) {
    match authoritative_limit {
        Some(authoritative) => merge_preferred_limit(fallback_limit, Some(authoritative)),
        None => *fallback_limit = None,
    }
}

fn merge_preferred_limit(
    fallback_limit: &mut Option<model::AccountLimit>,
    preferred_limit: Option<model::AccountLimit>,
) {
    match (fallback_limit.as_mut(), preferred_limit) {
        (Some(fallback), Some(preferred)) => {
            fallback.label = preferred.label;
            if preferred.used_percent.is_some() {
                fallback.used_percent = preferred.used_percent;
            }
            if preferred.resets_at.is_some() {
                fallback.resets_at = preferred.resets_at;
            }
        }
        (None, Some(preferred)) => *fallback_limit = Some(preferred),
        _ => {}
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
        .text(tray_refresh_menu_id(), "사용량 새로고침")
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
            id if id == tray_refresh_menu_id() => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let statuses = collect_representatives_off_thread(Settings::load(), true).await;
                    let _ = app.emit("status-updated", &statuses);
                });
            }
            id if id == tray_pause_bar_menu_id() => pause_taskbar_bars_for_manager(app),
            id if id == tray_resume_bar_menu_id() => {
                if let Err(err) = resume_taskbar_bars_for_manager(app) {
                    eprintln!("[taskbar] resume bars failed: {err}");
                }
            }
            id if id == tray_quit_menu_id() => request_app_quit(app),
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
        let _ = window.emit("panel-visibility-updated", true);
    }
}

fn setup_panel_close_hide(app: &tauri::App) {
    if let Some(window) = app.get_webview_window("panel") {
        let panel = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = panel.emit("panel-visibility-updated", false);
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

#[cfg(windows)]
fn taskbar_bar_window_is_alive(app: &tauri::AppHandle, tool: &str) -> bool {
    taskbar_bar_label(tool)
        .and_then(|label| app.get_webview_window(label))
        .and_then(|window| window.hwnd().ok())
        .is_some_and(taskbar::window_is_valid)
}

#[cfg(windows)]
fn taskbar_bar_window_is_visible(app: &tauri::AppHandle, tool: &str) -> bool {
    taskbar_bar_label(tool)
        .and_then(|label| app.get_webview_window(label))
        .and_then(|window| window.hwnd().ok())
        .is_some_and(taskbar::window_is_visible)
}

#[cfg(windows)]
fn create_taskbar_bar_window(app: &tauri::AppHandle, tool: &str) -> anyhow::Result<()> {
    let label = taskbar_bar_label(tool).ok_or_else(|| anyhow::anyhow!("unknown taskbar tool"))?;
    let url = WebviewUrl::App(format!("bar.html?tool={tool}").into());
    WebviewWindowBuilder::new(app, label, url)
        .title("Juice Bar")
        .inner_size(260.0, 40.0)
        .decorations(false)
        .visible(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .resizable(false)
        .shadow(false)
        .transparent(true)
        .build()?;
    Ok(())
}

#[cfg(windows)]
fn request_taskbar_bar_recovery(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<TaskbarRecoveryState>() else {
        return;
    };
    if state.0.swap(true, Ordering::AcqRel) {
        return;
    }

    let recovery_app = app.clone();
    if let Err(err) = app.run_on_main_thread(move || {
        for tool in TASKBAR_TOOLS {
            if taskbar_bar_window_is_alive(&recovery_app, tool) {
                continue;
            }
            set_taskbar_menu_state(&recovery_app, tool, false);
            if let Some(stale) =
                taskbar_bar_label(tool).and_then(|label| recovery_app.get_webview_window(label))
            {
                let _ = stale.destroy();
            }
            if let Err(err) = create_taskbar_bar_window(&recovery_app, tool) {
                eprintln!("[taskbar] recreate {tool} bar failed: {err}");
            }
        }
        if let Err(err) = apply_taskbar_dock(&recovery_app, &Settings::load()) {
            eprintln!("[taskbar] restore dock after shell restart failed: {err}");
        }
        if let Some(state) = recovery_app.try_state::<TaskbarRecoveryState>() {
            state.0.store(false, Ordering::Release);
        }
    }) {
        state.0.store(false, Ordering::Release);
        eprintln!("[taskbar] schedule shell recovery failed: {err}");
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
    let _layout_guard = TASKBAR_LAYOUT_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    position_taskbar_bar_on_taskbar_unlocked(manager, tool, taskbar, rect)
}

#[cfg(windows)]
fn position_taskbar_bar_on_taskbar_unlocked<R: tauri::Runtime>(
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
    let enabled = match normalize_taskbar_tool(tool)? {
        "claude" => settings.show_claude,
        "codex" => settings.show_codex,
        _ => false,
    };
    if !enabled {
        return None;
    }

    let ring_size = settings.ring_size_px.clamp(20.0, 44.0);
    let text_scale = settings.bar_text_font_size_px.clamp(8.0, 16.0) / 11.0;
    let content_gap_delta = settings.bar_content_gap_px.clamp(0.0, 24.0) - TASKBAR_CONTENT_GAP_BASE;
    let width = match settings.bar_mode.as_str() {
        "compact" => {
            ring_size
                + TASKBAR_DOCK_PADDING
                + TASKBAR_COMPACT_TEXT_BUDGET * text_scale
                + content_gap_delta
        }
        "dual" => ring_size + TASKBAR_INDICATOR_HORIZONTAL_PADDING,
        "quad" if settings.indicator_style == "bar" => {
            ring_size + TASKBAR_INDICATOR_HORIZONTAL_PADDING
        }
        "quad" => ring_size * 2.0 + TASKBAR_QUAD_GAP + TASKBAR_INDICATOR_HORIZONTAL_PADDING,
        _ => {
            let text_budget = if settings.full_reset_time_on {
                TASKBAR_FULL_RESET_TEXT_BUDGET
            } else {
                TASKBAR_FULL_TEXT_BUDGET
            };
            ring_size + TASKBAR_DOCK_PADDING + text_budget * text_scale + content_gap_delta
        }
    };

    Some(width.ceil() as i32)
}

fn taskbar_content_width_for_mode(
    mode: &str,
    layout: Option<&TaskbarContentLayout>,
) -> Option<i32> {
    if !matches!(mode, "full" | "compact") {
        return None;
    }
    layout
        .filter(|layout| layout.mode == mode)
        .map(|layout| layout.width)
}

fn taskbar_content_layout_slot<'a>(
    state: &'a TaskbarContentLayoutState,
    tool: &str,
) -> Option<&'a Mutex<Option<TaskbarContentLayout>>> {
    match normalize_taskbar_tool(tool)? {
        "claude" => Some(&state.claude),
        "codex" => Some(&state.codex),
        _ => None,
    }
}

fn taskbar_content_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    tool: &str,
) -> Option<TaskbarContentLayout> {
    let state = manager.try_state::<TaskbarContentLayoutState>()?;
    let layout = taskbar_content_layout_slot(&state, tool)?
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()?;
    taskbar_content_width_for_mode(&settings.bar_mode, Some(&layout)).map(|_| layout)
}

fn set_taskbar_content_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    layout: Option<TaskbarContentLayout>,
) -> Option<TaskbarContentLayout> {
    let state = manager.try_state::<TaskbarContentLayoutState>()?;
    let slot = taskbar_content_layout_slot(&state, tool)?;
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    std::mem::replace(&mut *current, layout)
}

fn set_taskbar_content_layout_ratio<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    ratio: f32,
) {
    let Some(state) = manager.try_state::<TaskbarContentLayoutState>() else {
        return;
    };
    let Some(slot) = taskbar_content_layout_slot(&state, tool) else {
        return;
    };
    if let Some(layout) = slot.lock().unwrap_or_else(|err| err.into_inner()).as_mut() {
        layout.ratio = Some(ratio.clamp(0.0, 1.0));
    };
}

fn taskbar_dock_width_for_manager<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    tool: &str,
) -> Option<i32> {
    let fallback = taskbar_dock_width(settings, tool)?;
    Some(
        taskbar_content_layout(manager, settings, tool)
            .map(|layout| layout.width)
            .unwrap_or(fallback),
    )
}

fn taskbar_width_with_menu(width: i32, menu_open: bool) -> i32 {
    if menu_open {
        width.max(TASKBAR_MENU_WIDTH)
    } else {
        width
    }
}

fn taskbar_physical_length(logical_length: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    (((logical_length.max(1) as i64) * (dpi as i64) + 48) / 96).clamp(1, i32::MAX as i64) as i32
}

fn taskbar_ratio_preserving_leading_edge(
    taskbar_rect: taskbar::DockRect,
    current_window: taskbar::DockRect,
    new_length: i32,
) -> Option<f32> {
    let mut resized_window = current_window;
    if taskbar_rect.width >= taskbar_rect.height {
        resized_window.width = new_length;
        resized_window.height = taskbar_rect.height;
    } else {
        resized_window.width = taskbar_rect.width;
        resized_window.height = new_length;
    }
    taskbar::offset_ratio_for_taskbar_rect(taskbar_rect, resized_window)
}

#[cfg(windows)]
fn taskbar_physical_length_for_window(
    logical_length: i32,
    hwnd: windows::Win32::Foundation::HWND,
) -> i32 {
    use windows::Win32::UI::HiDpi::GetDpiForWindow;

    taskbar_physical_length(logical_length, unsafe { GetDpiForWindow(hwnd) })
}

fn hide_taskbar_bar<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, tool: &str) {
    set_taskbar_menu_state(manager, tool, false);
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

fn taskbar_drag_active<R: tauri::Runtime>(manager: &impl tauri::Manager<R>) -> bool {
    manager
        .try_state::<TaskbarDragState>()
        .map(|state| state.0.load(Ordering::Acquire))
        .unwrap_or(false)
}

fn set_taskbar_drag_active<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, active: bool) {
    if let Some(state) = manager.try_state::<TaskbarDragState>() {
        state.0.store(active, Ordering::Release);
    }
}

fn set_taskbar_bars_paused<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, paused: bool) {
    if let Some(state) = manager.try_state::<TaskbarPauseState>() {
        state.0.store(paused, Ordering::Relaxed);
    }
}

fn taskbar_menu_is_open<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, tool: &str) -> bool {
    manager
        .try_state::<TaskbarMenuState>()
        .and_then(|state| {
            let target = match normalize_taskbar_tool(tool) {
                Some("claude") => Some(&state.claude),
                Some("codex") => Some(&state.codex),
                _ => None,
            }?;
            Some(target.lock().unwrap_or_else(|err| err.into_inner()).open)
        })
        .unwrap_or(false)
}

fn set_taskbar_menu_state<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    open: bool,
) {
    set_taskbar_menu_layout(manager, tool, open, None);
}

fn set_taskbar_menu_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    open: bool,
    ratio: Option<f32>,
) {
    let Some(state) = manager.try_state::<TaskbarMenuState>() else {
        return;
    };
    let target = match normalize_taskbar_tool(tool) {
        Some("claude") => Some(&state.claude),
        Some("codex") => Some(&state.codex),
        _ => None,
    };
    if let Some(target) = target {
        *target.lock().unwrap_or_else(|err| err.into_inner()) = TaskbarMenuLayout {
            open,
            ratio: ratio.map(|value| value.clamp(0.0, 1.0)),
        };
    }
}

fn taskbar_layout_ratio<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    tool: &str,
) -> f32 {
    let menu_ratio = manager.try_state::<TaskbarMenuState>().and_then(|state| {
        let target = match normalize_taskbar_tool(tool) {
            Some("claude") => Some(&state.claude),
            Some("codex") => Some(&state.codex),
            _ => None,
        }?;
        let layout = *target.lock().unwrap_or_else(|err| err.into_inner());
        layout.open.then_some(layout.ratio).flatten()
    });
    menu_ratio
        .or_else(|| taskbar_content_layout(manager, settings, tool).and_then(|layout| layout.ratio))
        .unwrap_or_else(|| taskbar_offset_ratio(settings, tool))
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
struct TaskbarDockSnapshot {
    taskbars: Vec<taskbar::ShellTaskbarWindow>,
    monitor_states: std::collections::HashMap<String, (bool, bool)>,
}

fn should_scan_taskbar_coverage(settings: &Settings, paused: bool) -> bool {
    !paused
        && (settings.fullscreen_hide_on || settings.maximized_hide_on)
        && TASKBAR_TOOLS
            .iter()
            .any(|tool| taskbar_dock_width(settings, tool).is_some())
}

#[cfg(windows)]
fn taskbar_from_snapshot<'a>(
    snapshot: &'a TaskbarDockSnapshot,
    preferred_key: &str,
) -> anyhow::Result<&'a taskbar::ShellTaskbarWindow> {
    snapshot
        .taskbars
        .iter()
        .find(|taskbar| !preferred_key.is_empty() && taskbar.key == preferred_key)
        .or_else(|| snapshot.taskbars.iter().find(|taskbar| taskbar.primary))
        .or_else(|| snapshot.taskbars.first())
        .ok_or_else(|| anyhow::anyhow!("no shell taskbar windows found"))
}

#[cfg(windows)]
fn taskbar_dock_snapshot<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
) -> anyhow::Result<TaskbarDockSnapshot> {
    let taskbars = taskbar::shell_taskbar_windows()?;
    let mut monitor_states = std::collections::HashMap::new();
    let should_scan_coverage = should_scan_taskbar_coverage(settings, taskbar_bars_paused(manager));

    if should_scan_coverage {
        let excluded = taskbar_bar_hwnds(manager);
        for tool in TASKBAR_TOOLS {
            if taskbar_dock_width(settings, tool).is_none() {
                continue;
            }
            let taskbar = taskbars
                .iter()
                .find(|taskbar| {
                    let key = taskbar_monitor_key(settings, tool);
                    !key.is_empty() && taskbar.key == key
                })
                .or_else(|| taskbars.iter().find(|taskbar| taskbar.primary))
                .or_else(|| taskbars.first())
                .ok_or_else(|| anyhow::anyhow!("no shell taskbar windows found"))?;
            monitor_states
                .entry(taskbar.key.clone())
                .or_insert_with(|| {
                    let (fullscreen, maximized) =
                        taskbar::visible_windows_coverage_on_monitor(&excluded, taskbar.monitor);
                    (
                        settings.fullscreen_hide_on && fullscreen,
                        settings.maximized_hide_on && maximized,
                    )
                });
        }
    }

    Ok(TaskbarDockSnapshot {
        taskbars,
        monitor_states,
    })
}

#[cfg(windows)]
fn taskbar_hide_window_state_for_monitor<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    monitor: taskbar::DockRect,
) -> (bool, bool) {
    if taskbar_bars_paused(manager) || (!settings.fullscreen_hide_on && !settings.maximized_hide_on)
    {
        return (false, false);
    }
    let excluded = taskbar_bar_hwnds(manager);
    let (fullscreen, maximized) = taskbar::visible_windows_coverage_on_monitor(&excluded, monitor);
    (
        settings.fullscreen_hide_on && fullscreen,
        settings.maximized_hide_on && maximized,
    )
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
fn bar_overlay_contract_matches(
    ex_style: isize,
    style: isize,
    owner: isize,
    expected_owner: isize,
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    let required_ex =
        WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize | WS_EX_TOPMOST.0 as isize;
    ex_style & required_ex == required_ex
        && style & WS_POPUP.0 as isize != 0
        && style & WS_CHILD.0 as isize == 0
        && owner == expected_owner
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

    unsafe fn set_window_long_checked(
        hwnd: windows::Win32::Foundation::HWND,
        index: windows::Win32::UI::WindowsAndMessaging::WINDOW_LONG_PTR_INDEX,
        value: isize,
        label: &str,
    ) -> anyhow::Result<()> {
        use windows::Win32::Foundation::{GetLastError, SetLastError, WIN32_ERROR};
        SetLastError(WIN32_ERROR(0));
        let previous = SetWindowLongPtrW(hwnd, index, value);
        let error = GetLastError();
        if previous == 0 && error != WIN32_ERROR(0) {
            anyhow::bail!("{label} failed: {}", error.0);
        }
        Ok(())
    }

    let hwnd = window.hwnd()?;
    unsafe {
        let current_ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        set_window_long_checked(
            hwnd,
            GWL_EXSTYLE,
            bar_overlay_ex_style(current_ex_style),
            "taskbar ex-style apply",
        )?;

        let current_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        set_window_long_checked(
            hwnd,
            GWL_STYLE,
            bar_overlay_window_style(current_style),
            "taskbar style apply",
        )?;

        set_window_long_checked(
            hwnd,
            GWLP_HWNDPARENT,
            owner_hwnd.0 as isize,
            "taskbar owner apply",
        )?;
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        )?;
        if !bar_overlay_contract_matches(
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
            GetWindowLongPtrW(hwnd, GWL_STYLE),
            GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT),
            owner_hwnd.0 as isize,
        ) {
            anyhow::bail!("taskbar window style or owner read-back mismatch");
        }
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
            let width = match taskbar_dock_width_for_manager(app, settings, tool) {
                Some(width) => width,
                None => {
                    hide_taskbar_bar(app, tool);
                    continue;
                }
            };
            let taskbar =
                taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(settings, tool))?;
            let width = taskbar_physical_length_for_window(width, taskbar.hwnd);
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
                taskbar_layout_ratio(app, settings, tool),
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
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::{w, PCWSTR},
            Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        };
        let target = std::ffi::OsStr::new(&target)
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            return Err(format!("Windows shell open failed: {}", result.0 as isize));
        }
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

#[derive(serde::Serialize)]
struct SaveSettingsResult {
    settings: Settings,
    taskbar_applied: bool,
    autostart_applied: bool,
    warnings: Vec<String>,
}

fn settings_apply_report(
    settings: Settings,
    taskbar_result: Result<(), String>,
    autostart_result: Result<(), String>,
) -> SaveSettingsResult {
    let taskbar_applied = taskbar_result.is_ok();
    let autostart_applied = autostart_result.is_ok();
    let mut warnings = Vec::new();
    if let Err(error) = taskbar_result {
        warnings.push(format!("taskbar: {error}"));
    }
    if let Err(error) = autostart_result {
        warnings.push(format!("autostart: {error}"));
    }
    SaveSettingsResult {
        settings,
        taskbar_applied,
        autostart_applied,
        warnings,
    }
}

fn retry_settings_side_effects(
    app: tauri::AppHandle,
    mut retry_taskbar: bool,
    mut retry_autostart: bool,
) {
    if !retry_taskbar && !retry_autostart {
        return;
    }
    tauri::async_runtime::spawn(async move {
        for delay in [500, 1_500, 3_000] {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            let latest = Settings::load();
            if retry_taskbar && !taskbar_drag_active(&app) {
                retry_taskbar = apply_taskbar_dock(&app, &latest).is_err();
            }
            if retry_autostart {
                retry_autostart = apply_autostart_for_release(&app, &latest).is_err();
            }
            if !retry_taskbar && !retry_autostart {
                return;
            }
        }
        eprintln!(
            "[settings] side-effect reconciliation pending: taskbar={retry_taskbar}, autostart={retry_autostart}"
        );
    });
}

#[tauri::command]
fn save_settings(
    window: tauri::Window,
    app: tauri::AppHandle,
    input: config::SettingsInput,
) -> Result<SaveSettingsResult, String> {
    ensure_panel_command(window.label())?;
    let mut requested = Settings::from_input(input);
    let position_app = app.clone();
    let settings = Settings::update(move |current| {
        requested.claude_taskbar_offset_ratio = current.claude_taskbar_offset_ratio;
        requested.codex_taskbar_offset_ratio = current.codex_taskbar_offset_ratio;
        requested.claude_taskbar_monitor_key = current.claude_taskbar_monitor_key.clone();
        requested.codex_taskbar_monitor_key = current.codex_taskbar_monitor_key.clone();
        #[cfg(windows)]
        if !taskbar_drag_active(&position_app) {
            preserve_taskbar_leading_edges(&position_app, current, &mut requested);
        }
        *current = requested;
    })
    .map_err(|err| err.to_string())?;
    let taskbar_result = if taskbar_drag_active(&app) {
        Ok(())
    } else {
        apply_taskbar_dock(&app, &settings).map_err(|err| err.to_string())
    };
    let autostart_result =
        apply_autostart_for_release(&app, &settings).map_err(|err| err.to_string());
    let _ = app.emit("settings-updated", &settings);
    let report = settings_apply_report(settings, taskbar_result, autostart_result);
    retry_settings_side_effects(app, !report.taskbar_applied, !report.autostart_applied);
    Ok(report)
}

#[cfg(windows)]
fn preserve_taskbar_leading_edges(
    app: &tauri::AppHandle,
    current: &Settings,
    requested: &mut Settings,
) {
    for tool in TASKBAR_TOOLS {
        let (Some(current_length), Some(requested_length)) = (
            taskbar_dock_width_for_manager(app, current, tool),
            taskbar_dock_width_for_manager(app, requested, tool),
        ) else {
            continue;
        };
        if current_length == requested_length {
            continue;
        }

        let Ok(taskbar) = taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(current, tool))
        else {
            continue;
        };
        let Ok(window_rect) = current_bar_rect(app, tool) else {
            continue;
        };
        let taskbar_rect = taskbar::DockRect {
            x: taskbar.left,
            y: taskbar.top,
            width: taskbar.right - taskbar.left,
            height: taskbar.bottom - taskbar.top,
        };
        let current_window = taskbar::DockRect {
            x: window_rect.left,
            y: window_rect.top,
            width: window_rect.right - window_rect.left,
            height: window_rect.bottom - window_rect.top,
        };
        let requested_length = taskbar_physical_length_for_window(requested_length, taskbar.hwnd);
        if let Some(ratio) =
            taskbar_ratio_preserving_leading_edge(taskbar_rect, current_window, requested_length)
        {
            set_taskbar_offset_ratio(requested, tool, ratio);
        }
    }
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
        let width = taskbar_dock_width_for_manager(&app, &settings, tool)
            .ok_or_else(|| "taskbar bar is hidden".to_string())?;
        let taskbar = taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool))
            .map_err(|err| err.to_string())?;
        let width = taskbar_physical_length_for_window(width, taskbar.hwnd);
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
        set_taskbar_content_layout_ratio(&app, tool, ratio);
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
    let _ = window.emit("panel-visibility-updated", false);
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
    let _ = window.emit("panel-visibility-updated", false);
    window.hide().map_err(|err| err.to_string())
}

#[tauri::command]
fn complete_app_quit(window: tauri::Window, app: tauri::AppHandle) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    let should_exit = app
        .try_state::<QuitPendingState>()
        .is_some_and(|state| state.0.swap(false, Ordering::AcqRel));
    if should_exit {
        exit_after_taskbar_cleanup(app);
    }
    Ok(())
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

fn taskbar_orientation(width: i32, height: i32) -> &'static str {
    if width >= height {
        "horizontal"
    } else {
        "vertical"
    }
}

#[tauri::command]
fn get_taskbar_orientation(window: tauri::Window, tool: String) -> Result<String, String> {
    ensure_matching_bar_command(window.label(), &tool)?;

    #[cfg(windows)]
    {
        let settings = Settings::load();
        let tool =
            normalize_taskbar_tool(&tool).ok_or_else(|| "unknown taskbar tool".to_string())?;
        let taskbar = taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool))
            .map_err(|err| err.to_string())?;
        Ok(
            taskbar_orientation(taskbar.right - taskbar.left, taskbar.bottom - taskbar.top)
                .to_string(),
        )
    }

    #[cfg(not(windows))]
    Ok("horizontal".to_string())
}

#[tauri::command]
fn set_taskbar_content_width(
    window: tauri::Window,
    app: tauri::AppHandle,
    tool: String,
    width: f64,
) -> Result<bool, String> {
    ensure_matching_bar_command(window.label(), &tool)?;
    if !width.is_finite() || !(1.0..=1024.0).contains(&width) {
        return Err("taskbar content width is out of range".into());
    }

    let tool = normalize_taskbar_tool(&tool).ok_or_else(|| "unknown taskbar tool".to_string())?;
    let settings = Settings::load();
    if !matches!(settings.bar_mode.as_str(), "full" | "compact") {
        return Ok(false);
    }
    let width = width.ceil() as i32;
    let previous = taskbar_content_layout(&app, &settings, tool);
    if previous
        .as_ref()
        .is_some_and(|layout| layout.mode == settings.bar_mode && layout.width == width)
    {
        return Ok(false);
    }

    #[cfg(windows)]
    let ratio = (|| {
        let current_rect = current_bar_rect(&app, tool).ok()?;
        let taskbar =
            taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool)).ok()?;
        let taskbar_rect = taskbar::DockRect {
            x: taskbar.left,
            y: taskbar.top,
            width: taskbar.right - taskbar.left,
            height: taskbar.bottom - taskbar.top,
        };
        let current_window = taskbar::DockRect {
            x: current_rect.left,
            y: current_rect.top,
            width: current_rect.right - current_rect.left,
            height: current_rect.bottom - current_rect.top,
        };
        let new_length = taskbar_physical_length_for_window(width, taskbar.hwnd);
        taskbar_ratio_preserving_leading_edge(taskbar_rect, current_window, new_length)
    })()
    .or_else(|| Some(taskbar_offset_ratio(&settings, tool)));
    #[cfg(not(windows))]
    let ratio = Some(taskbar_offset_ratio(&settings, tool));

    let next = TaskbarContentLayout {
        mode: settings.bar_mode.clone(),
        width,
        ratio,
    };
    set_taskbar_content_layout(&app, tool, Some(next));
    if let Err(err) = apply_taskbar_dock(&app, &settings) {
        set_taskbar_content_layout(&app, tool, previous);
        return Err(err.to_string());
    }
    Ok(true)
}

#[tauri::command]
fn set_taskbar_tooltip(
    window: tauri::Window,
    app: tauri::AppHandle,
    tool: String,
    text: String,
) -> Result<(), String> {
    ensure_matching_bar_command(window.label(), &tool)?;
    let tool = normalize_taskbar_tool(&tool).ok_or_else(|| "unknown taskbar tool".to_string())?;
    let text = text.replace('\0', "");
    if text.chars().count() > 512 {
        return Err("taskbar tooltip is too long".into());
    }

    let state = app
        .try_state::<TaskbarTooltipTextState>()
        .ok_or_else(|| "taskbar tooltip state is unavailable".to_string())?;
    state
        .0
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(tool, text);
    Ok(())
}

#[tauri::command]
fn set_taskbar_menu_open(
    window: tauri::Window,
    app: tauri::AppHandle,
    tool: String,
    open: bool,
) -> Result<(), String> {
    ensure_matching_bar_command(window.label(), &tool)?;
    let settings = Settings::load();
    #[cfg(windows)]
    let current_rect = current_bar_rect(&app, &tool).ok();
    #[cfg(windows)]
    let menu_ratio = if open {
        if let (Some(current_rect), Some(logical_width)) = (
            current_rect,
            taskbar_dock_width_for_manager(&app, &settings, &tool),
        ) {
            if let Ok(taskbar) =
                taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, &tool))
            {
                let taskbar_rect = taskbar::DockRect {
                    x: taskbar.left,
                    y: taskbar.top,
                    width: taskbar.right - taskbar.left,
                    height: taskbar.bottom - taskbar.top,
                };
                let current_window = taskbar::DockRect {
                    x: current_rect.left,
                    y: current_rect.top,
                    width: current_rect.right - current_rect.left,
                    height: current_rect.bottom - current_rect.top,
                };
                let expanded = taskbar_physical_length_for_window(
                    taskbar_width_with_menu(logical_width, true),
                    taskbar.hwnd,
                );
                taskbar_ratio_preserving_leading_edge(taskbar_rect, current_window, expanded)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    #[cfg(not(windows))]
    let menu_ratio = None;
    if open && menu_ratio.is_none() {
        return Err("taskbar menu geometry is unavailable".into());
    }
    set_taskbar_menu_layout(&app, &tool, open, menu_ratio);
    if let Err(err) = apply_taskbar_dock(&app, &settings) {
        set_taskbar_menu_state(&app, &tool, false);
        return Err(err.to_string());
    }
    Ok(())
}

fn apply_taskbar_dock<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let snapshot = taskbar_dock_snapshot(manager, settings)?;
        apply_taskbar_dock_with_snapshot(manager, settings, &snapshot)
    }

    #[cfg(not(windows))]
    {
        let _ = (manager, settings);
        Ok(())
    }
}

#[cfg(windows)]
fn apply_taskbar_dock_with_snapshot<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    snapshot: &TaskbarDockSnapshot,
) -> anyhow::Result<()> {
    let _layout_guard = TASKBAR_LAYOUT_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let taskbar_paused = taskbar_bars_paused(manager);
    for tool in TASKBAR_TOOLS {
        let width = match taskbar_dock_width_for_manager(manager, settings, tool) {
            Some(width) => taskbar_width_with_menu(width, taskbar_menu_is_open(manager, tool)),
            None => {
                hide_taskbar_bar(manager, tool);
                continue;
            }
        };
        let taskbar = taskbar_from_snapshot(snapshot, taskbar_monitor_key(settings, tool))?;
        let width = taskbar_physical_length_for_window(width, taskbar.hwnd);
        let (fullscreen_active, maximized_active) = snapshot
            .monitor_states
            .get(&taskbar.key)
            .copied()
            .unwrap_or_default();
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
            taskbar_layout_ratio(manager, settings, tool),
        )
        .ok_or_else(|| anyhow::anyhow!("invalid shell taskbar rectangle"))?;
        position_taskbar_bar_on_taskbar_unlocked(manager, tool, taskbar, rect)?;
    }
    Ok(())
}

#[cfg(windows)]
fn taskbar_dock_signature(
    app: &tauri::AppHandle,
    settings: &Settings,
    snapshot: &TaskbarDockSnapshot,
) -> anyhow::Result<String> {
    let mut signature = serde_json::to_string(settings)?;
    signature.push_str(if taskbar_bars_paused(app) {
        "|paused"
    } else {
        "|active"
    });

    for tool in TASKBAR_TOOLS {
        let taskbar = taskbar_from_snapshot(snapshot, taskbar_monitor_key(settings, tool))?;
        let window_state = snapshot
            .monitor_states
            .get(&taskbar.key)
            .copied()
            .unwrap_or_default();
        let width = taskbar_dock_width_for_manager(app, settings, tool).unwrap_or_default();
        signature.push_str(&format!(
            "|{tool}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            taskbar.hwnd.0 as isize,
            taskbar.left,
            taskbar.top,
            taskbar.right,
            taskbar.bottom,
            width,
            taskbar_layout_ratio(app, settings, tool),
            window_state.0,
            window_state.1,
            taskbar_menu_is_open(app, tool),
            taskbar_bar_window_is_alive(app, tool),
            taskbar_bar_window_is_visible(app, tool),
        ));
    }
    Ok(signature)
}

#[cfg(windows)]
fn save_taskbar_drag_target(
    app: &tauri::AppHandle,
    tool: &str,
    monitor_key: &str,
    dropped_rect: taskbar::DockRect,
) -> anyhow::Result<()> {
    let mut position_error = None;
    let settings = Settings::update(|current| {
        let result = (|| {
            let taskbar = taskbar::shell_taskbar_window_for_key(monitor_key)?;
            let logical_length = taskbar_dock_width_for_manager(app, current, tool)
                .ok_or_else(|| anyhow::anyhow!("taskbar bar is hidden"))?;
            let physical_length = taskbar_physical_length_for_window(logical_length, taskbar.hwnd);
            let taskbar_rect = taskbar::DockRect {
                x: taskbar.left,
                y: taskbar.top,
                width: taskbar.right - taskbar.left,
                height: taskbar.bottom - taskbar.top,
            };
            let mut final_rect = dropped_rect;
            if taskbar_rect.width >= taskbar_rect.height {
                final_rect.width = physical_length;
                final_rect.height = taskbar_rect.height;
            } else {
                final_rect.width = taskbar_rect.width;
                final_rect.height = physical_length;
            }
            let ratio = taskbar::offset_ratio_for_taskbar_rect(taskbar_rect, final_rect)
                .ok_or_else(|| anyhow::anyhow!("invalid dropped taskbar rectangle"))?;
            set_taskbar_target(current, tool, monitor_key, ratio);
            anyhow::Ok(())
        })();
        if let Err(err) = result {
            position_error = Some(err);
        }
    })?;
    if let Some(err) = position_error {
        return Err(err);
    }
    set_taskbar_content_layout_ratio(app, tool, taskbar_offset_ratio(&settings, tool));
    let _ = app.emit("settings-updated", &settings);
    apply_taskbar_dock(app, &settings)?;
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
struct TaskbarDragStart {
    tool: &'static str,
    logical_length: i32,
    grab_axis_ratio: f32,
    grab_cross_ratio: f32,
}

#[cfg(windows)]
fn current_bar_drag_start(
    app: &tauri::AppHandle,
    point: windows::Win32::Foundation::POINT,
) -> Option<TaskbarDragStart> {
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
) -> Option<TaskbarDragStart> {
    let rect = current_bar_rect(app, tool).ok()?;
    let window_visible = taskbar_bar_label(tool)
        .and_then(|label| app.get_webview_window(label))
        .and_then(|window| window.hwnd().ok())
        .is_some_and(taskbar::window_is_visible);
    if !taskbar_drag_candidate_precheck(
        (point.x, point.y),
        taskbar::DockRect {
            x: rect.left,
            y: rect.top,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        },
        taskbar_menu_is_open(app, tool),
        taskbar_paused,
        taskbar_tool_enabled(settings, tool),
        window_visible,
    ) {
        return None;
    }
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

    let bar_width = rect.right.checked_sub(rect.left).unwrap_or(1).max(1);
    let bar_height = rect.bottom.checked_sub(rect.top).unwrap_or(1).max(1);
    let taskbar_width = taskbar.right.checked_sub(taskbar.left).unwrap_or(1).max(1);
    let taskbar_height = taskbar.bottom.checked_sub(taskbar.top).unwrap_or(1).max(1);
    let horizontal = taskbar_width >= taskbar_height;
    let (axis_offset, axis_length, cross_offset, cross_length) = if horizontal {
        (
            point.x - rect.left,
            bar_width,
            point.y - rect.top,
            bar_height,
        )
    } else {
        (
            point.y - rect.top,
            bar_height,
            point.x - rect.left,
            bar_width,
        )
    };

    Some(TaskbarDragStart {
        tool,
        logical_length: taskbar_dock_width_for_manager(app, settings, tool)?,
        grab_axis_ratio: (axis_offset as f32 / axis_length as f32).clamp(0.0, 1.0),
        grab_cross_ratio: (cross_offset as f32 / cross_length as f32).clamp(0.0, 1.0),
    })
}

fn taskbar_drag_candidate_precheck(
    point: (i32, i32),
    rect: taskbar::DockRect,
    menu_open: bool,
    paused: bool,
    enabled: bool,
    visible: bool,
) -> bool {
    !menu_open
        && !paused
        && enabled
        && visible
        && point.0 >= rect.x
        && point.0 < rect.x + rect.width
        && point.1 >= rect.y
        && point.1 < rect.y + rect.height
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
fn current_bar_at_point(
    app: &tauri::AppHandle,
    point: windows::Win32::Foundation::POINT,
) -> Option<&'static str> {
    if let Some(tool) = taskbar_tool_at_point(app, point) {
        return Some(tool);
    }

    for tool in TASKBAR_TOOLS.iter().rev().copied() {
        let Some(label) = taskbar_bar_label(tool) else {
            continue;
        };
        let Some(window) = app.get_webview_window(label) else {
            continue;
        };
        let visible = window.hwnd().ok().is_some_and(taskbar::window_is_visible);
        if !visible {
            continue;
        }
        if current_bar_rect(app, tool)
            .ok()
            .is_some_and(|rect| point_inside_rect(point, rect))
        {
            return Some(tool);
        }
    }
    None
}

#[cfg(windows)]
fn set_taskbar_tooltip_visible(
    app: &tauri::AppHandle,
    tool: &str,
    visible: bool,
) -> anyhow::Result<()> {
    let label = taskbar_bar_label(tool).ok_or_else(|| anyhow::anyhow!("unknown taskbar tool"))?;
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| anyhow::anyhow!("no {label} window"))?;
    taskbar::show_window_tooltip(window.hwnd()?, visible)
}

#[cfg(windows)]
fn sync_taskbar_tooltips(
    app: &tauri::AppHandle,
    registered: &mut std::collections::HashMap<&'static str, (isize, String)>,
) -> Vec<&'static str> {
    let desired = app
        .try_state::<TaskbarTooltipTextState>()
        .map(|state| {
            state
                .0
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .clone()
        })
        .unwrap_or_default();
    let mut removed = Vec::new();

    for tool in TASKBAR_TOOLS {
        let next = desired.get(tool).and_then(|text| {
            let window = taskbar_bar_label(tool).and_then(|label| app.get_webview_window(label))?;
            let hwnd = window.hwnd().ok()?;
            Some((hwnd, hwnd.0 as isize, text))
        });
        let next_key = next.as_ref().map(|(_, key, _)| *key);
        if let Some((old_key, _)) = registered.get(tool).cloned() {
            if Some(old_key) != next_key {
                let old = windows::Win32::Foundation::HWND(old_key as *mut core::ffi::c_void);
                if let Err(err) = taskbar::remove_window_tooltip(old) {
                    eprintln!("[taskbar] tooltip cleanup failed for {tool}: {err}");
                }
                registered.remove(tool);
                set_taskbar_menu_state(app, tool, false);
                removed.push(tool);
            }
        }
        let Some((hwnd, key, text)) = next else {
            continue;
        };
        if registered
            .get(tool)
            .is_some_and(|current| current.0 == key && current.1 == *text)
        {
            continue;
        }
        match taskbar::set_window_tooltip(hwnd, text) {
            Ok(()) => {
                registered.insert(tool, (key, text.clone()));
            }
            Err(err) => eprintln!("[taskbar] tooltip sync failed for {tool}: {err}"),
        }
    }
    removed
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
        let spawn_result = std::thread::Builder::new()
            .name("juice-taskbar-input".into())
            .spawn(move || {
                let mut was_down = false;
                let mut drag_tool: Option<&'static str> = None;
                let mut drag_logical_length: Option<i32> = None;
                let mut grab_axis_ratio: Option<f32> = None;
                let mut grab_cross_ratio: Option<f32> = None;
                let mut drag_monitor_key: Option<String> = None;
                let mut start_x: Option<i32> = None;
                let mut start_y: Option<i32> = None;
                let mut last_rect: Option<taskbar::DockRect> = None;
                let mut last_monitor_key: Option<String> = None;
                let mut emitted_dragging = false;
                let mut hover_candidate: Option<&'static str> = None;
                let mut hover_started_at = std::time::Instant::now();
                let mut visible_tooltip: Option<&'static str> = None;
                let mut registered_tooltips = std::collections::HashMap::new();
                #[cfg(debug_assertions)]
                let forced_hover = std::env::var("AGENT_JUICE_TEST_HOVER_TOOL")
                    .ok()
                    .and_then(|tool| normalize_taskbar_tool(&tool));

                loop {
                    if app
                        .try_state::<TaskbarShutdownState>()
                        .is_some_and(|state| state.0.load(Ordering::Acquire))
                    {
                        if let Some(tool) = visible_tooltip.take() {
                            let _ = set_taskbar_tooltip_visible(&app, tool, false);
                        }
                        for tool in TASKBAR_TOOLS {
                            set_taskbar_menu_state(&app, tool, false);
                        }
                        let _ = taskbar::clear_current_thread_tooltips();
                        break;
                    }
                    let removed = sync_taskbar_tooltips(&app, &mut registered_tooltips);
                    if visible_tooltip.is_some_and(|tool| removed.contains(&tool)) {
                        visible_tooltip = None;
                    }
                    if hover_candidate.is_some_and(|tool| removed.contains(&tool)) {
                        hover_candidate = None;
                        hover_started_at = std::time::Instant::now();
                    }
                    let down = left_mouse_button_down();
                    let point = cursor_position().ok();
                    let next_hover = {
                        #[cfg(debug_assertions)]
                        if forced_hover.is_some() {
                            forced_hover
                        } else if down {
                            None
                        } else {
                            point
                                .and_then(|point| current_bar_at_point(&app, point))
                                .filter(|tool| !taskbar_menu_is_open(&app, tool))
                        }
                        #[cfg(not(debug_assertions))]
                        {
                            if down {
                                None
                            } else {
                                point
                                    .and_then(|point| current_bar_at_point(&app, point))
                                    .filter(|tool| !taskbar_menu_is_open(&app, tool))
                            }
                        }
                    };
                    if next_hover != hover_candidate {
                        if let Some(tool) = visible_tooltip.take() {
                            let _ = set_taskbar_tooltip_visible(&app, tool, false);
                        }
                        hover_candidate = next_hover;
                        hover_started_at = std::time::Instant::now();
                    }
                    if visible_tooltip.is_none()
                        && hover_started_at.elapsed()
                            >= std::time::Duration::from_millis(TASKBAR_TOOLTIP_DELAY_MS)
                    {
                        if let Some(tool) = hover_candidate {
                            if set_taskbar_tooltip_visible(&app, tool, true).is_ok() {
                                visible_tooltip = Some(tool);
                            }
                        }
                    }

                    if down {
                        if !was_down {
                            let drag_start =
                                point.and_then(|point| current_bar_drag_start(&app, point));
                            drag_tool = drag_start.as_ref().map(|start| start.tool);
                            drag_logical_length =
                                drag_start.as_ref().map(|start| start.logical_length);
                            grab_axis_ratio =
                                drag_start.as_ref().map(|start| start.grab_axis_ratio);
                            grab_cross_ratio =
                                drag_start.as_ref().map(|start| start.grab_cross_ratio);
                            drag_monitor_key = drag_tool.map(|tool| {
                                taskbar_monitor_key(&Settings::load(), tool).to_string()
                            });
                            start_x = point.map(|point| point.x);
                            start_y = point.map(|point| point.y);
                            last_rect = None;
                            last_monitor_key = None;
                            set_taskbar_drag_active(&app, drag_tool.is_some());
                        }

                        if let (
                            Some(tool),
                            Some(point),
                            Some(logical_length),
                            Some(grab_axis_ratio),
                            Some(grab_cross_ratio),
                        ) = (
                            drag_tool,
                            point,
                            drag_logical_length,
                            grab_axis_ratio,
                            grab_cross_ratio,
                        ) {
                            let moved = start_x
                                .zip(start_y)
                                .map(|(start_x, start_y)| {
                                    (point.x - start_x).abs() > TASKBAR_DRAG_THRESHOLD_PX
                                        || (point.y - start_y).abs() > TASKBAR_DRAG_THRESHOLD_PX
                                })
                                .unwrap_or(false);
                            if !moved {
                                was_down = down;
                                taskbar::pump_current_thread_messages();
                                std::thread::sleep(std::time::Duration::from_millis(16));
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
                            if let Ok((taskbar, rect, ratio)) =
                                taskbar::shell_taskbar_drag_rect_at_point_for_key(
                                    logical_length,
                                    point.x,
                                    point.y,
                                    grab_axis_ratio,
                                    grab_cross_ratio,
                                    drag_monitor_key.as_deref().unwrap_or_default(),
                                )
                            {
                                if position_taskbar_bar_on_taskbar(&app, tool, &taskbar, rect)
                                    .is_ok()
                                {
                                    let _ = ratio;
                                    last_rect = Some(rect);
                                    last_monitor_key = Some(taskbar.key);
                                }
                            }
                        }
                    } else if was_down {
                        if let (Some(tool), Some(rect), Some(monitor_key)) =
                            (drag_tool, last_rect.take(), last_monitor_key.take())
                        {
                            if let Err(err) =
                                save_taskbar_drag_target(&app, tool, &monitor_key, rect)
                            {
                                eprintln!(
                                    "[taskbar] save dragged {tool} bar position failed: {err}"
                                );
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
                        set_taskbar_drag_active(&app, false);
                        drag_logical_length = None;
                        grab_axis_ratio = None;
                        grab_cross_ratio = None;
                        drag_monitor_key = None;
                        start_x = None;
                        start_y = None;
                        last_monitor_key = None;
                    }

                    was_down = down;
                    taskbar::pump_current_thread_messages();
                    let cadence_ms = if down {
                        16
                    } else if hover_candidate.is_some() || visible_tooltip.is_some() {
                        50
                    } else {
                        150
                    };
                    std::thread::sleep(std::time::Duration::from_millis(cadence_ms));
                }
            });
        if let Err(err) = spawn_result {
            eprintln!("[taskbar] input thread failed to start: {err}");
        }
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
            let mut last_signature: Option<String> = None;
            let (mut settings, mut settings_revision) = Settings::load_with_revision();
            loop {
                if !left_mouse_button_down() {
                    if TASKBAR_TOOLS
                        .iter()
                        .any(|tool| !taskbar_bar_window_is_alive(&app, tool))
                    {
                        last_signature = None;
                        request_taskbar_bar_recovery(&app);
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                    let next_revision = Settings::storage_revision();
                    if next_revision != settings_revision {
                        (settings, settings_revision) = Settings::load_with_revision();
                    }
                    let dock_result = (|| -> anyhow::Result<Option<String>> {
                        let snapshot = taskbar_dock_snapshot(&app, &settings)?;
                        let signature = taskbar_dock_signature(&app, &settings, &snapshot)?;
                        if last_signature.as_ref() == Some(&signature) {
                            return Ok(None);
                        }
                        apply_taskbar_dock_with_snapshot(&app, &settings, &snapshot)?;
                        Ok(Some(signature))
                    })();
                    match dock_result {
                        Ok(Some(signature)) => last_signature = Some(signature),
                        Ok(None) => {}
                        Err(err) => {
                            last_signature = None;
                            eprintln!("[taskbar] inspect dock state failed: {err}");
                        }
                    }
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

#[cfg(windows)]
fn quoted_windows_executable(path: &std::path::Path) -> String {
    format!("\"{}\"", path.to_string_lossy().trim_matches('"'))
}

#[cfg(windows)]
fn enforce_quoted_autostart_value<R, M>(manager: &M) -> anyhow::Result<()>
where
    R: tauri::Runtime,
    M: tauri::Manager<R>,
{
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")?;
    let value = quoted_windows_executable(&std::env::current_exe()?);
    let name = &manager.app_handle().package_info().name;
    run.set_value(name, &value)?;
    let stored: String = run.get_value(name)?;
    if stored != value {
        return Err(anyhow::anyhow!("autostart registry value was not quoted"));
    }
    Ok(())
}

fn apply_autostart_for_release<R, M>(manager: &M, settings: &Settings) -> anyhow::Result<()>
where
    R: tauri::Runtime,
    M: tauri::Manager<R>,
{
    if cfg!(debug_assertions) {
        return Ok(());
    }

    if !settings.autostart_on {
        manager.autolaunch().disable()?;
        return Ok(());
    }

    manager.autolaunch().enable()?;
    #[cfg(windows)]
    {
        enforce_quoted_autostart_value(manager)?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_panel(app);
        }))
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
            get_taskbar_orientation,
            set_taskbar_content_width,
            set_taskbar_tooltip,
            set_taskbar_menu_open,
            move_taskbar_bar,
            pause_taskbar_bars,
            minimize_panel,
            toggle_panel_maximized,
            hide_panel_window,
            complete_app_quit,
            start_panel_drag,
            install_statusline,
            restore_statusline
        ])
        .setup(|app| {
            app.manage(TaskbarPauseState::default());
            app.manage(TaskbarMenuState::default());
            app.manage(TaskbarRecoveryState::default());
            app.manage(TaskbarDragState::default());
            app.manage(TaskbarTooltipTextState::default());
            app.manage(TaskbarContentLayoutState::default());
            app.manage(TaskbarShutdownState::default());
            app.manage(QuitPendingState::default());
            let settings = Settings::load();
            if let Err(err) = try_setup_taskbar_dock(app, &settings) {
                eprintln!("[taskbar] fallback to tray: {err}");
            }
            setup_panel_close_hide(app);
            setup_trays(app)?;
            if let Err(err) = apply_autostart_for_release(app, &settings) {
                eprintln!("[autostart] startup apply failed: {err}");
            }
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
        taskbar,
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
        assert_eq!(super::tray_refresh_menu_id(), "juice-refresh");
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
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(320));
        assert_eq!(super::taskbar_dock_width(&settings, "codex"), Some(320));

        settings.bar_mode = "compact".into();
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(179));
        settings.bar_content_gap_px = 0.0;
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(165));
        settings.bar_content_gap_px = 24.0;
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(189));
        settings.bar_content_gap_px = 4.0;
        settings.bar_mode = "dual".into();
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(37));
        settings.bar_mode = "quad".into();
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(80));
        settings.indicator_style = "bar".into();
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(37));
        settings.indicator_style = "ring".into();
        settings.ring_size_px = 44.0;
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(96));
        assert_eq!(super::taskbar_width_with_menu(37, true), 96);
        assert_eq!(super::taskbar_width_with_menu(80, true), 96);
        assert_eq!(super::taskbar_width_with_menu(96, true), 96);
        assert_eq!(super::taskbar_width_with_menu(37, false), 37);
        assert_eq!(super::taskbar_physical_length(36, 0), 36);
        assert_eq!(super::taskbar_physical_length(36, 96), 36);
        assert_eq!(super::taskbar_physical_length(36, 120), 45);
        assert_eq!(super::taskbar_physical_length(36, 144), 54);
        settings.bar_mode = "full".into();
        settings.ring_size_px = 36.0;
        settings.full_reset_time_on = true;
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(310));
        settings.full_reset_time_on = false;
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), Some(213));

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
        assert_eq!(super::taskbar_dock_width(&settings, "codex"), Some(213));

        settings.show_codex = false;
        assert!(!super::should_show_taskbar_bar(&settings, "claude"));
        assert!(!super::should_show_taskbar_bar(&settings, "codex"));
        assert_eq!(super::taskbar_dock_width(&settings, "claude"), None);
        assert_eq!(super::taskbar_dock_width(&settings, "codex"), None);
    }

    #[test]
    fn measured_taskbar_content_width_is_scoped_to_its_text_mode() {
        let layout = super::TaskbarContentLayout {
            mode: "full".into(),
            width: 188,
            ratio: Some(0.42),
        };
        assert_eq!(
            super::taskbar_content_width_for_mode("full", Some(&layout)),
            Some(188)
        );
        assert_eq!(
            super::taskbar_content_width_for_mode("compact", Some(&layout)),
            None
        );
        assert_eq!(
            super::taskbar_content_width_for_mode("dual", Some(&layout)),
            None
        );
        assert_eq!(super::taskbar_content_width_for_mode("full", None), None);
    }

    #[test]
    fn taskbar_coverage_scan_skips_paused_or_fully_disabled_bars() {
        let mut settings = Settings::default();
        assert!(super::should_scan_taskbar_coverage(&settings, false));
        assert!(!super::should_scan_taskbar_coverage(&settings, true));

        settings.show_claude = false;
        settings.show_codex = false;
        assert!(!super::should_scan_taskbar_coverage(&settings, false));

        settings.show_codex = true;
        settings.fullscreen_hide_on = false;
        settings.maximized_hide_on = false;
        assert!(!super::should_scan_taskbar_coverage(&settings, false));
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
    fn taskbar_ratio_preserves_leading_edge_when_window_length_changes() {
        let horizontal = taskbar::DockRect {
            x: 100,
            y: 1040,
            width: 1820,
            height: 40,
        };
        let horizontal_window = taskbar::DockRect {
            x: 740,
            y: 1040,
            width: 260,
            height: 40,
        };
        let ratio = super::taskbar_ratio_preserving_leading_edge(horizontal, horizontal_window, 64)
            .unwrap();
        let restored =
            taskbar::dock_rect_for_taskbar_at_offset(100, 1040, 1920, 1080, 64, ratio).unwrap();
        assert_eq!(restored.x, horizontal_window.x);

        let vertical = taskbar::DockRect {
            x: 0,
            y: 80,
            width: 48,
            height: 1000,
        };
        let vertical_window = taskbar::DockRect {
            x: 0,
            y: 420,
            width: 48,
            height: 260,
        };
        let ratio =
            super::taskbar_ratio_preserving_leading_edge(vertical, vertical_window, 107).unwrap();
        let restored =
            taskbar::dock_rect_for_taskbar_at_offset(0, 80, 48, 1080, 107, ratio).unwrap();
        assert_eq!(restored.y, vertical_window.y);

        let near_edge = taskbar::DockRect {
            x: 1790,
            ..horizontal_window
        };
        let ratio =
            super::taskbar_ratio_preserving_leading_edge(horizontal, near_edge, 260).unwrap();
        let restored =
            taskbar::dock_rect_for_taskbar_at_offset(100, 1040, 1920, 1080, 260, ratio).unwrap();
        assert_eq!(restored.x, 1660);
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
    fn taskbar_orientation_uses_the_shell_rectangle_axis() {
        assert_eq!(super::taskbar_orientation(1920, 48), "horizontal");
        assert_eq!(super::taskbar_orientation(48, 48), "horizontal");
        assert_eq!(super::taskbar_orientation(48, 1080), "vertical");
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
        assert_eq!(merged.captured_at, "2026-07-09T00:00:00Z");
        assert!(!merged.session.active);
        assert_eq!(merged.session.context_used_percent, Some(63.0));
        assert_eq!(merged.primary.as_ref().unwrap().used_percent, Some(78.0));
        assert_eq!(merged.secondary.as_ref().unwrap().used_percent, Some(10.0));
        assert!(!merged.approx);
    }

    #[test]
    fn stale_claude_account_does_not_override_fresh_statusline_limits() {
        let mut statusline = status_for_signature("fresh-statusline");
        statusline.session.active = true;
        statusline.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(21.0),
            resets_at: None,
        });

        let mut oauth = status_for_signature("claude-oauth-usage");
        oauth.session.active = false;
        oauth.approx = false;
        oauth.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(89.0),
            resets_at: None,
        });

        let merged = super::merge_claude_usage_status(Some(statusline), Some(oauth)).unwrap();
        assert!(merged.session.active);
        assert_eq!(merged.primary.unwrap().used_percent, Some(21.0));
        assert!(merged.approx);
    }

    #[test]
    fn exact_limits_merge_used_percent_and_reset_independently() {
        let mut statusline = status_for_signature("claude-session");
        statusline.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(11.0),
            resets_at: Some("fallback-primary-reset".into()),
        });
        statusline.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(22.0),
            resets_at: Some("fallback-secondary-reset".into()),
        });

        let mut exact = status_for_signature("claude-oauth-usage");
        exact.approx = false;
        exact.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(77.0),
            resets_at: None,
        });
        exact.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: None,
            resets_at: Some("exact-secondary-reset".into()),
        });

        let merged = super::merge_claude_usage_status(Some(statusline), Some(exact)).unwrap();
        let primary = merged.primary.unwrap();
        let secondary = merged.secondary.unwrap();
        assert_eq!(primary.used_percent, Some(77.0));
        assert_eq!(primary.resets_at.as_deref(), Some("fallback-primary-reset"));
        assert_eq!(secondary.used_percent, Some(22.0));
        assert_eq!(
            secondary.resets_at.as_deref(),
            Some("exact-secondary-reset")
        );
        assert!(merged.approx);
    }

    #[test]
    fn codex_account_limits_preserve_rollout_session_freshness() {
        let mut rollout = status_for_signature("rollout-session");
        rollout.tool = Tool::Codex;
        rollout.captured_at = "2026-07-11T01:00:00Z".into();
        rollout.session.active = false;
        rollout.session.context_used_percent = Some(64.0);
        rollout.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(12.0),
            resets_at: Some("rollout-reset".into()),
        });
        rollout.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(23.0),
            resets_at: Some("rollout-week-reset".into()),
        });

        let mut account = status_for_signature("app-server-account");
        account.tool = Tool::Codex;
        account.captured_at = "2026-07-11T02:00:00Z".into();
        account.session.active = true;
        account.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(81.0),
            resets_at: None,
        });
        account.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(82.0),
            resets_at: None,
        });

        let merged = super::merge_codex_account_status(Some(rollout), Some(account)).unwrap();
        assert_eq!(merged.session_id, "rollout-session");
        assert_eq!(merged.captured_at, "2026-07-11T01:00:00Z");
        assert!(!merged.session.active);
        assert_eq!(merged.session.context_used_percent, Some(64.0));
        assert_eq!(merged.primary.as_ref().unwrap().used_percent, Some(81.0));
        assert_eq!(
            merged.primary.as_ref().unwrap().resets_at.as_deref(),
            Some("rollout-reset")
        );
        assert!(!merged.approx);
    }

    #[test]
    fn codex_account_limits_clear_fallback_windows_absent_from_exact_response() {
        let mut rollout = status_for_signature("rollout-session");
        rollout.tool = Tool::Codex;
        rollout.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(12.0),
            resets_at: Some("rollout-5h-reset".into()),
        });
        rollout.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(23.0),
            resets_at: Some("rollout-week-reset".into()),
        });

        let mut weekly_account = status_for_signature("app-server-account");
        weekly_account.tool = Tool::Codex;
        weekly_account.approx = false;
        weekly_account.primary = None;
        weekly_account.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(16.0),
            resets_at: None,
        });

        let weekly =
            super::merge_codex_account_status(Some(rollout.clone()), Some(weekly_account)).unwrap();
        assert!(weekly.primary.is_none());
        assert_eq!(weekly.secondary.as_ref().unwrap().used_percent, Some(16.0));
        assert_eq!(
            weekly.secondary.as_ref().unwrap().resets_at.as_deref(),
            Some("rollout-week-reset")
        );
        assert!(!weekly.approx);

        let mut five_hour_account = status_for_signature("app-server-account");
        five_hour_account.tool = Tool::Codex;
        five_hour_account.approx = false;
        five_hour_account.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(9.0),
            resets_at: None,
        });
        five_hour_account.secondary = None;

        let five_hour =
            super::merge_codex_account_status(Some(rollout), Some(five_hour_account)).unwrap();
        assert_eq!(five_hour.primary.as_ref().unwrap().used_percent, Some(9.0));
        assert!(five_hour.secondary.is_none());
        assert!(!five_hour.approx);
    }

    #[test]
    fn stale_codex_account_does_not_override_fresh_rollout_limits() {
        let mut rollout = status_for_signature("fresh-rollout");
        rollout.tool = Tool::Codex;
        rollout.session.active = true;
        rollout.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(17.0),
            resets_at: None,
        });

        let mut account = status_for_signature("app-server-account");
        account.tool = Tool::Codex;
        account.session.active = false;
        account.approx = false;
        account.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(94.0),
            resets_at: None,
        });

        let merged = super::merge_codex_account_status(Some(rollout), Some(account)).unwrap();
        assert!(merged.session.active);
        assert_eq!(merged.primary.unwrap().used_percent, Some(17.0));
        assert!(merged.approx);
    }

    #[test]
    fn claude_representative_backtracks_from_malformed_newest_file() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-claude-backtrack-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("claude_last.older.json"),
            r#"{"session_id":"older-valid","context_window":{"used_percentage":42}}"#,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("claude_last.newest.json"), "{malformed").unwrap();

        let statuses = super::collect_representatives_from(
            &Settings::default(),
            Some(&root),
            None,
            chrono::Utc::now(),
        );

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].session_id, "older-valid");
        std::fs::remove_dir_all(root).unwrap();
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
            Err(super::CollectionErrorKind::Transport)
        });
        let cached =
            super::cached_status_attempt(&cache, now + Duration::seconds(1), 30, false, || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(status_for_signature("should-not-run"))
            });
        let forced =
            super::cached_status_attempt(&cache, now + Duration::seconds(1), 30, true, || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(status_for_signature("forced"))
            });

        let preserved =
            super::cached_status_attempt(&cache, now + Duration::seconds(2), 30, true, || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(super::CollectionErrorKind::Parse)
            });

        assert!(first.is_none());
        assert!(cached.is_none());
        assert_eq!(forced.unwrap().session_id, "forced");
        assert_eq!(preserved.unwrap().session_id, "forced");
        let cached = cache.lock().unwrap();
        assert_eq!(
            cached.as_ref().unwrap().attempted_at,
            now + Duration::seconds(2)
        );
        assert_eq!(
            cached.as_ref().unwrap().retry_at,
            now + Duration::seconds(32)
        );
        assert_eq!(
            cached.as_ref().unwrap().error,
            Some(super::CollectionErrorKind::Parse)
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn refresh_budget_caps_subprocesses_and_rejects_expired_deadlines() {
        let capped = super::remaining_refresh_budget(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(3),
        )
        .unwrap();
        assert!(capped <= std::time::Duration::from_secs(3));
        assert!(capped > std::time::Duration::ZERO);
        assert_eq!(
            super::remaining_refresh_budget(
                std::time::Instant::now(),
                std::time::Duration::from_secs(3)
            ),
            Err(super::CollectionErrorKind::Deadline)
        );

        let final_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let rollout_deadline =
            super::deadline_with_reserve(final_deadline, std::time::Duration::from_secs(7));
        let rollout_budget = rollout_deadline.saturating_duration_since(std::time::Instant::now());
        assert!(rollout_budget <= std::time::Duration::from_secs(8));
        assert!(
            rollout_deadline
                <= final_deadline
                    .checked_sub(std::time::Duration::from_secs(7))
                    .unwrap()
        );
        let reserved = super::remaining_refresh_budget_with_reserve(
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        assert!(reserved <= std::time::Duration::from_secs(3));
        assert!(reserved > std::time::Duration::ZERO);
        assert_eq!(
            super::remaining_refresh_budget_with_reserve(
                std::time::Instant::now() + std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(2),
            ),
            Err(super::CollectionErrorKind::Deadline)
        );
    }

    #[test]
    fn settings_apply_report_keeps_saved_settings_and_all_side_effect_failures() {
        let settings = Settings::default();
        let report = super::settings_apply_report(
            settings.clone(),
            Err("taskbar unavailable".into()),
            Err("registry denied".into()),
        );

        assert_eq!(report.settings.bar_mode, settings.bar_mode);
        assert_eq!(report.settings.show_claude, settings.show_claude);
        assert_eq!(report.settings.show_codex, settings.show_codex);
        assert!(!report.taskbar_applied);
        assert!(!report.autostart_applied);
        assert_eq!(report.warnings.len(), 2);
        assert!(report.warnings[0].contains("taskbar unavailable"));
        assert!(report.warnings[1].contains("registry denied"));
    }

    #[test]
    fn taskbar_drag_precheck_rejects_menu_actions_and_outside_clicks() {
        let rect = taskbar::DockRect {
            x: 100,
            y: 200,
            width: 80,
            height: 40,
        };
        assert!(super::taskbar_drag_candidate_precheck(
            (120, 220),
            rect,
            false,
            false,
            true,
            true
        ));
        assert!(!super::taskbar_drag_candidate_precheck(
            (120, 220),
            rect,
            true,
            false,
            true,
            true
        ));
        assert!(!super::taskbar_drag_candidate_precheck(
            (99, 220),
            rect,
            false,
            false,
            true,
            true
        ));
        assert!(!super::taskbar_drag_candidate_precheck(
            (120, 220),
            rect,
            false,
            true,
            true,
            true
        ));
    }

    #[cfg(windows)]
    #[test]
    fn taskbar_style_contract_requires_all_flags_and_exact_owner() {
        use windows::Win32::UI::WindowsAndMessaging::{
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        };
        let ex_style =
            WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize | WS_EX_TOPMOST.0 as isize;
        let style = WS_POPUP.0 as isize;
        assert!(super::bar_overlay_contract_matches(ex_style, style, 42, 42));
        assert!(!super::bar_overlay_contract_matches(
            ex_style & !(WS_EX_NOACTIVATE.0 as isize),
            style,
            42,
            42,
        ));
        assert!(!super::bar_overlay_contract_matches(ex_style, style, 7, 42));
    }

    #[test]
    fn collection_bookkeeping_does_not_change_agent_status_v1_payload() {
        let value = serde_json::to_value(status_for_signature("payload-contract")).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.get("schema_version").unwrap(), "agent_status.v1");
        assert_eq!(object.len(), 10);
        for internal in ["attempted_at", "last_good", "error", "retry_at"] {
            assert!(!object.contains_key(internal));
        }
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
    fn multiple_forced_waiters_share_one_forced_collection_after_normal() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc, Barrier,
        };

        const WAITERS: usize = 8;
        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let normal_release = Arc::new(Barrier::new(2));
        let attempts = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();

        let normal_coordinator = Arc::clone(&coordinator);
        let normal_release_worker = Arc::clone(&normal_release);
        let normal_attempts = Arc::clone(&attempts);
        let normal = std::thread::spawn(move || {
            normal_coordinator.run(false, || {
                normal_attempts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                normal_release_worker.wait();
                vec![status_for_signature("normal")]
            })
        });
        started_rx.recv().unwrap();

        let start = Arc::new(Barrier::new(WAITERS + 1));
        let mut waiters = Vec::new();
        for _ in 0..WAITERS {
            let coordinator = Arc::clone(&coordinator);
            let attempts = Arc::clone(&attempts);
            let start = Arc::clone(&start);
            waiters.push(std::thread::spawn(move || {
                start.wait();
                coordinator.run(true, || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    vec![status_for_signature("forced")]
                })
            }));
        }
        start.wait();
        while coordinator.state.lock().unwrap().pending_force_waiters < WAITERS {
            std::thread::yield_now();
        }
        normal_release.wait();

        assert_eq!(normal.join().unwrap()[0].session_id, "normal");
        for waiter in waiters {
            assert_eq!(waiter.join().unwrap()[0].session_id, "forced");
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn collection_panic_wakes_joined_waiters() {
        use std::sync::{mpsc, Arc, Barrier};

        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let release = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = mpsc::channel();
        let panic_coordinator = Arc::clone(&coordinator);
        let panic_release = Arc::clone(&release);
        let panicking = std::thread::spawn(move || {
            std::panic::catch_unwind(|| {
                panic_coordinator.run(false, || {
                    started_tx.send(()).unwrap();
                    panic_release.wait();
                    panic!("fixture panic");
                })
            })
        });
        started_rx.recv().unwrap();

        let waiter_coordinator = Arc::clone(&coordinator);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result =
                waiter_coordinator.run(false, || vec![status_for_signature("must-not-run")]);
            done_tx.send(result).unwrap();
        });
        while coordinator.state.lock().unwrap().joined_waiters == 0 {
            std::thread::yield_now();
        }
        release.wait();

        assert!(panicking.join().unwrap().is_err());
        assert!(done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .is_empty());
        waiter.join().unwrap();
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
