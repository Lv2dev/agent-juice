pub mod activity;
pub mod adapters;
pub mod antigravity;
pub mod antigravity_cli;
mod claude_desktop;
pub mod codex_activity;
pub mod collector;
pub mod config;
pub mod cursor_activity;
pub mod cursor_dashboard;
pub mod cursor_pty;
pub(crate) mod http_transport;
pub mod model;
pub mod paths;
pub mod render;
#[cfg(windows)]
mod single_instance;
pub mod statusline;
mod system_activity;
#[cfg(windows)]
pub mod taskbar;
mod text_scale;
pub mod update;

#[cfg(all(test, windows))]
#[link(name = "test-common-controls")]
unsafe extern "C" {}

use chrono::{DateTime, Utc};
use config::{
    Settings, TaskbarAppearanceProfile, TaskbarLayoutProfile, TaskbarMonitorMode, TaskbarPlacement,
    TaskbarPresentationProfile, TaskbarTopology,
};
use model::{AgentStatus, Tool};
use once_cell::sync::Lazy;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Condvar, Mutex, MutexGuard, TryLockError,
};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

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
const TASKBAR_TOPOLOGY_STABLE_OBSERVATIONS: u8 = 3;
const MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS: usize = 32;
const TASKBAR_TOOLS: [&str; 5] = ["claude", "codex", "grok", "cursor", "antigravity"];
const CODEX_REPRESENTATIVE_CANDIDATES: usize = 32;
const CODEX_ACCOUNT_CACHE_MIN_SECS: i64 = 30;
const CODEX_ACCOUNT_API_TIMEOUT_SECS: u64 = 5;
const CODEX_ROLLOUT_CACHE_MAX_AGE_SECS: u64 = 60;
const CLAUDE_USAGE_CACHE_MIN_SECS: i64 = 5 * 60;
const CLAUDE_USAGE_TIMEOUT_SECS: u64 = 10;
const GROK_BILLING_CACHE_MIN_SECS: i64 = 60;
const GROK_BILLING_TIMEOUT_SECS: u64 = 8;
const CURSOR_USAGE_CACHE_MIN_SECS: i64 = 5 * 60;
const CURSOR_USAGE_TIMEOUT_SECS: u64 = 20;
const CURSOR_AGENT_FALLBACK_MAX_RESERVE_SECS: u64 = 10;
const CURSOR_SCOPE_REVALIDATION_MAX_SECS: u64 = 1;
const COLLECTION_REFRESH_DEADLINE_SECS: u64 = 15;
const ACTIVITY_BACKFILL_MAX_PASSES: usize = 16;
const ACTIVITY_BACKFILL_MAX_SECS: u64 = 30;
const CODEX_ACTIVITY_TIMEOUT_SECS: u64 = 5;
const CURSOR_ACTIVITY_MAX_PASSES: usize = 4;
const CURSOR_ACTIVITY_PASS_SECS: u64 = 8;
const CURSOR_ACTIVITY_MAX_SECS: u64 = 30;
const CLAUDE_FALLBACK_RESERVE_SECS: u64 = 2;
const CLAUDE_COLLECTION_MIN_BUDGET_SECS: u64 = 2;
const GROK_COLLECTION_MIN_BUDGET_SECS: u64 = 2;
const COLLECTION_MAX_BACKOFF_SECS: i64 = 30 * 60;
const COLLECTION_STICKY_ERROR_MIN_BACKOFF_SECS: i64 = 5 * 60;
const TRAY_ID: &str = "juice";
const TRAY_ICON_IDS: [&str; 1] = [TRAY_ID];
const UPDATE_START_DELAY_SECS: u64 = 15;
const UPDATE_CHECK_TIMEOUT_SECS: u64 = 20;
const UPDATE_DOWNLOAD_TIMEOUT_SECS: u64 = 300;

static UPDATE_OPERATION_GATE: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));
static TASKBAR_PAUSE_WRITE_GATE: Mutex<()> = Mutex::new(());

#[derive(Clone, serde::Serialize)]
struct TaskbarDraggingPayload {
    tool: &'static str,
    dragging: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum UpdateInstallEvent {
    Started {
        version: String,
    },
    Progress {
        downloaded_bytes: u64,
        content_length: Option<u64>,
    },
    Verifying,
    Installing,
}

#[derive(Default)]
struct TaskbarPauseState(AtomicBool);

#[derive(Default)]
struct TaskbarMenuState {
    claude: Mutex<TaskbarMenuSlot>,
    codex: Mutex<TaskbarMenuSlot>,
    grok: Mutex<TaskbarMenuSlot>,
    cursor: Mutex<TaskbarMenuSlot>,
    antigravity: Mutex<TaskbarMenuSlot>,
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct TaskbarMenuLayout {
    open: bool,
    ratio: Option<f32>,
}

#[derive(Default)]
struct TaskbarMenuSlot {
    revision: u64,
    layout: TaskbarMenuLayout,
}

#[derive(Debug)]
struct TaskbarMenuWrite {
    revision: u64,
}

#[derive(Default)]
struct TaskbarRecoveryState(AtomicBool);

#[derive(Default)]
struct TaskbarDragState(AtomicBool);

#[derive(Default)]
struct TaskbarStableTopologyState(Mutex<TaskbarStableTopologyData>);

#[derive(Default)]
struct TaskbarStableTopologyData {
    topology: TaskbarTopology,
    pending_placements: Vec<PendingTaskbarProfilePlacement>,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingTaskbarProfilePlacement {
    topology: TaskbarTopology,
    tool: &'static str,
    placement: TaskbarPlacement,
}

#[derive(Default)]
struct TaskbarTooltipTextState(Mutex<std::collections::HashMap<&'static str, String>>);

#[derive(Clone, Debug, PartialEq)]
struct TaskbarContentLayout {
    mode: String,
    width: i32,
    ratio: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskbarContentWidthDecision {
    RetryAfterTarget,
    AlreadyApplied,
    Apply,
}

fn taskbar_content_width_decision(
    target_initialized: bool,
    layout_matches: bool,
    window_matches: bool,
) -> TaskbarContentWidthDecision {
    if !target_initialized {
        TaskbarContentWidthDecision::RetryAfterTarget
    } else if layout_matches && window_matches {
        TaskbarContentWidthDecision::AlreadyApplied
    } else {
        TaskbarContentWidthDecision::Apply
    }
}

#[derive(Default)]
struct TaskbarContentLayoutSlot {
    revision: u64,
    layout: Option<TaskbarContentLayout>,
}

struct TaskbarContentLayoutWrite {
    revision: u64,
    previous: Option<TaskbarContentLayout>,
}

#[derive(Default)]
struct TaskbarContentLayoutState {
    claude: Mutex<TaskbarContentLayoutSlot>,
    codex: Mutex<TaskbarContentLayoutSlot>,
    grok: Mutex<TaskbarContentLayoutSlot>,
    cursor: Mutex<TaskbarContentLayoutSlot>,
    antigravity: Mutex<TaskbarContentLayoutSlot>,
}

#[derive(Default)]
struct TaskbarWindowState {
    claude: Mutex<Option<TaskbarWindowHandle>>,
    codex: Mutex<Option<TaskbarWindowHandle>>,
    grok: Mutex<Option<TaskbarWindowHandle>>,
    cursor: Mutex<Option<TaskbarWindowHandle>>,
    antigravity: Mutex<Option<TaskbarWindowHandle>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskbarWindowHandle {
    raw: isize,
    generation: u64,
}

#[derive(Default)]
struct TaskbarShutdownState(AtomicBool);

#[derive(Default)]
struct QuitPendingState(AtomicBool);

#[derive(Default)]
struct SettingsSideEffectRetryState {
    running: AtomicBool,
    taskbar_pending: AtomicBool,
    autostart_pending: AtomicBool,
    taskbar_requests: AtomicU64,
    autostart_requests: AtomicU64,
}

#[cfg(windows)]
fn spawn_single_instance_listener(app: tauri::AppHandle, event: single_instance::InstanceEvent) {
    let _ = std::thread::Builder::new()
        .name("juice-single-instance".into())
        .spawn(move || loop {
            if app
                .try_state::<TaskbarShutdownState>()
                .is_some_and(|state| state.0.load(Ordering::Acquire))
            {
                break;
            }
            match event.wait(std::time::Duration::from_millis(100)) {
                Ok(true) => {
                    let activation_app = app.clone();
                    if let Err(err) = app.run_on_main_thread(move || {
                        let quit_cancelled = activation_app
                            .try_state::<QuitPendingState>()
                            .is_some_and(|pending| pending.0.swap(false, Ordering::AcqRel));
                        if quit_cancelled {
                            let _ = activation_app
                                .emit("app-quit-cancelled", "single-instance activation");
                        }
                        show_panel(&activation_app);
                    }) {
                        eprintln!("[single-instance] activation dispatch failed: {err}");
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    eprintln!("[single-instance] activation wait failed: {err}");
                    break;
                }
            }
        });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CollectionErrorKind {
    Deadline,
    Transport,
    Parse,
    LoginRequired,
    Unavailable,
    RateLimited,
    CredentialsDecode,
    SourceChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CollectionHealth {
    Idle,
    Ready,
    AppRequired,
    LoginRequired,
    Unavailable,
    TransientError,
    RateLimited,
    ParseError,
    CredentialsError,
    SourceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct CollectionHealthSnapshot {
    claude: CollectionHealth,
    codex: CollectionHealth,
    grok: CollectionHealth,
    cursor: CollectionHealth,
    antigravity: CollectionHealth,
}

#[derive(Clone)]
struct CachedStatusAttempt {
    attempted_at: DateTime<Utc>,
    last_good: Option<AgentStatus>,
    error: Option<CollectionErrorKind>,
    retry_at: DateTime<Utc>,
    consecutive_failures: u32,
}

#[derive(Clone)]
enum CursorStatusSource {
    Agent(Option<cursor_dashboard::AccountScope>),
    Dashboard(cursor_dashboard::AccountScope),
}

#[derive(Clone)]
struct CursorCachedStatusAttempt {
    attempted_at: DateTime<Utc>,
    last_good: Option<AgentStatus>,
    source: Option<CursorStatusSource>,
    // Failed display values must not erase the identity used to guard fallback.
    known_scope: Option<cursor_dashboard::AccountScope>,
    error: Option<CollectionErrorKind>,
    retry_at: DateTime<Utc>,
    consecutive_failures: u32,
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
    fn last_result(&self) -> Vec<AgentStatus> {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .last_result
            .clone()
    }

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
        let result = match outcome {
            Ok(result) => {
                state.last_result = result.clone();
                result
            }
            Err(_) => state.last_result.clone(),
        };
        self.completed.notify_all();
        drop(state);
        result
    }

    fn run_if_idle_with_flag(
        &self,
        collect: impl FnOnce() -> Vec<AgentStatus>,
    ) -> (Vec<AgentStatus>, bool) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        if state.in_flight {
            return (state.last_result.clone(), false);
        }
        let generation = state.completed_generation.saturating_add(1);
        state.in_flight = true;
        state.active_generation = generation;
        drop(state);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(collect));
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.in_flight = false;
        state.completed_generation = generation;
        let result = match outcome {
            Ok(result) => {
                state.last_result = result.clone();
                result
            }
            Err(_) => state.last_result.clone(),
        };
        self.completed.notify_all();
        (result, true)
    }
}

static COLLECTION_COORDINATOR: Lazy<CollectionCoordinator> =
    Lazy::new(CollectionCoordinator::default);
static CURSOR_COLLECTION_COORDINATOR: Lazy<CollectionCoordinator> =
    Lazy::new(CollectionCoordinator::default);
static CODEX_ACCOUNT_CACHE: Lazy<Mutex<Option<CachedStatusAttempt>>> =
    Lazy::new(|| Mutex::new(None));
static CLAUDE_USAGE_CACHE: Lazy<Mutex<Option<CachedStatusAttempt>>> =
    Lazy::new(|| Mutex::new(None));
static CLAUDE_SOURCE_REVISION: Lazy<Mutex<Option<claude_desktop::Revision>>> =
    Lazy::new(|| Mutex::new(None));
static GROK_BILLING_CACHE: Lazy<Mutex<Option<CachedStatusAttempt>>> =
    Lazy::new(|| Mutex::new(None));
static CURSOR_USAGE_CACHE: Lazy<Mutex<Option<CursorCachedStatusAttempt>>> =
    Lazy::new(|| Mutex::new(None));
static CODEX_ROLLOUT_CACHE: Lazy<Mutex<collector::RolloutCache>> =
    Lazy::new(|| Mutex::new(collector::RolloutCache::default()));
static CODEX_ROLLOUT_STATUS_CACHE: Lazy<Mutex<Option<AgentStatus>>> =
    Lazy::new(|| Mutex::new(None));
static TASKBAR_LAYOUT_GATE: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static TASKBAR_PROFILE_GATE: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static TASKBAR_SETTINGS_WRITE_GATE: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static TASKBAR_SETTINGS_GENERATION: AtomicU64 = AtomicU64::new(0);
static COLLECTION_POLICY_RECOVERY: Lazy<Mutex<CollectionPolicyRecovery>> =
    Lazy::new(|| Mutex::new(CollectionPolicyRecovery::default()));
static TASKBAR_WINDOW_GENERATION: AtomicU64 = AtomicU64::new(0);
static FORCE_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static ACTIVITY_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static CURSOR_ACTIVITY_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static CURSOR_ACTIVITY_GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_LOCAL_ACTIVITY: Lazy<Mutex<Option<activity::ActivitySnapshot>>> =
    Lazy::new(|| Mutex::new(None));

struct ForceRefreshGuard;

impl Drop for ForceRefreshGuard {
    fn drop(&mut self) {
        FORCE_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

fn try_begin_force_refresh() -> Option<ForceRefreshGuard> {
    FORCE_REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| ForceRefreshGuard)
}

struct ActivityRefreshGuard;

impl Drop for ActivityRefreshGuard {
    fn drop(&mut self) {
        ACTIVITY_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

fn try_begin_activity_refresh() -> Option<ActivityRefreshGuard> {
    ACTIVITY_REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| ActivityRefreshGuard)
}

struct CursorActivityRefreshGuard;

impl Drop for CursorActivityRefreshGuard {
    fn drop(&mut self) {
        CURSOR_ACTIVITY_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

fn try_begin_cursor_activity_refresh() -> Option<(CursorActivityRefreshGuard, u64)> {
    CURSOR_ACTIVITY_REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| {
            let generation = CURSOR_ACTIVITY_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
            (CursorActivityRefreshGuard, generation)
        })
}

fn mark_taskbar_settings_changed() -> u64 {
    TASKBAR_SETTINGS_GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

struct TaskbarSettingsSnapshot {
    settings: Settings,
    revision: Option<(u64, std::time::SystemTime)>,
    generation: u64,
    collection_changes: CollectionChanges,
}

fn collection_tool_index(tool: &Tool) -> usize {
    match tool {
        Tool::Claude => 0,
        Tool::Codex => 1,
        Tool::Grok => 2,
        Tool::Cursor => 3,
        Tool::Antigravity => 4,
    }
}

#[derive(Clone, Copy, Default)]
struct CollectionPolicyRetry {
    pending: bool,
    failures: u32,
    retry_at: Option<std::time::Instant>,
}

#[derive(Default)]
struct CollectionPolicyRecovery {
    entries: [CollectionPolicyRetry; TASKBAR_TOOLS.len()],
}

impl CollectionPolicyRecovery {
    fn record(&mut self, tool: &Tool, failed: bool, now: std::time::Instant) {
        if !matches!(
            tool,
            Tool::Claude | Tool::Codex | Tool::Grok | Tool::Antigravity
        ) {
            return;
        }
        let entry = &mut self.entries[collection_tool_index(tool)];
        if failed {
            entry.pending = true;
            entry.failures = entry.failures.saturating_add(1);
            let delay = std::time::Duration::from_secs(
                (1u64 << entry.failures.saturating_sub(1).min(6)).min(60),
            );
            entry.retry_at = Some(now.checked_add(delay).unwrap_or(now));
        } else {
            *entry = CollectionPolicyRetry::default();
        }
    }

    fn due(&self, tool: &Tool, now: std::time::Instant) -> bool {
        let entry = &self.entries[collection_tool_index(tool)];
        entry.pending && entry.retry_at.is_none_or(|at| now >= at)
    }

    fn any_due(&self, now: std::time::Instant) -> bool {
        [&Tool::Claude, &Tool::Codex, &Tool::Grok, &Tool::Antigravity]
            .into_iter()
            .any(|tool| self.due(tool, now))
    }
}

#[derive(Clone, Copy)]
struct CollectionPolicy {
    enabled: [bool; TASKBAR_TOOLS.len()],
    activity_weeks: u16,
}

impl CollectionPolicy {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: [
                settings.show_claude,
                settings.show_codex,
                settings.show_grok,
                settings.show_cursor,
                settings.show_antigravity,
            ],
            activity_weeks: settings.activity_weeks,
        }
    }

    fn enabled(&self, tool: &Tool) -> bool {
        self.enabled[collection_tool_index(tool)]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CollectionChanges {
    transitions: [Option<bool>; TASKBAR_TOOLS.len()],
    cursor_activity_range_changed: bool,
}

impl CollectionChanges {
    fn between(before: CollectionPolicy, after: CollectionPolicy) -> Self {
        Self {
            transitions: std::array::from_fn(|index| {
                (before.enabled[index] != after.enabled[index]).then_some(after.enabled[index])
            }),
            cursor_activity_range_changed: before.enabled(&Tool::Cursor)
                && after.enabled(&Tool::Cursor)
                && before.activity_weeks != after.activity_weeks,
        }
    }

    fn transition(&self, tool: &Tool) -> Option<bool> {
        self.transitions[collection_tool_index(tool)]
    }

    fn enabled_now(&self, tool: &Tool) -> bool {
        self.transition(tool) == Some(true)
    }

    fn any(&self) -> bool {
        self.transitions.iter().any(Option::is_some)
    }
}

fn mutate_settings_with_collection_changes(
    current: &mut Settings,
    mutator: impl FnOnce(&mut Settings) -> anyhow::Result<()>,
) -> anyhow::Result<CollectionChanges> {
    let before = CollectionPolicy::from_settings(current);
    mutator(current)?;
    Ok(CollectionChanges::between(
        before,
        CollectionPolicy::from_settings(current),
    ))
}

fn persist_settings_with_collection_policy(
    update: impl FnOnce(&mut dyn FnMut(&mut Settings) -> anyhow::Result<()>) -> anyhow::Result<Settings>,
    mutator: impl FnOnce(&mut Settings) -> anyhow::Result<()>,
    load_current: impl FnOnce() -> anyhow::Result<Settings>,
    mut apply_claude: impl FnMut(bool) -> anyhow::Result<()>,
) -> anyhow::Result<(Settings, CollectionChanges)> {
    let mut mutator = Some(mutator);
    let mut changes = CollectionChanges::default();
    let mut claude_attempted = false;
    let result = update(&mut |current| {
        changes = mutate_settings_with_collection_changes(
            current,
            mutator.take().expect("settings mutation must run once"),
        )?;
        if let Some(enabled) = changes.transition(&Tool::Claude) {
            claude_attempted = true;
            apply_claude(enabled)?;
        }
        Ok(())
    });
    match result {
        Ok(settings) => Ok((settings, changes)),
        Err(error) => {
            if claude_attempted {
                // The caller holds the settings gate, so no newer in-process save can interleave.
                let rollback = load_current().and_then(|current| apply_claude(current.show_claude));
                if let Err(rollback_error) = rollback {
                    anyhow::bail!("{error}; Claude collection rollback failed: {rollback_error}");
                }
            }
            Err(error)
        }
    }
}

fn update_taskbar_settings(
    mutator: impl FnOnce(&mut Settings),
) -> anyhow::Result<TaskbarSettingsSnapshot> {
    try_update_taskbar_settings(|settings| {
        mutator(settings);
        Ok(())
    })
}

fn try_update_taskbar_settings(
    mutator: impl FnOnce(&mut Settings) -> anyhow::Result<()>,
) -> anyhow::Result<TaskbarSettingsSnapshot> {
    let _guard = TASKBAR_SETTINGS_WRITE_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let (settings, collection_changes) = persist_settings_with_collection_policy(
        |edit| Settings::try_update(edit),
        mutator,
        Settings::try_load,
        |enabled| apply_claude_statusline_for_release(enabled).map_err(anyhow::Error::msg),
    )?;
    if collection_changes.transition(&Tool::Claude).is_some() {
        COLLECTION_POLICY_RECOVERY
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .record(&Tool::Claude, false, std::time::Instant::now());
    }
    let revision = Settings::storage_revision();
    let generation = mark_taskbar_settings_changed();
    Ok(TaskbarSettingsSnapshot {
        settings,
        revision,
        generation,
        collection_changes,
    })
}

fn with_taskbar_settings_read<T>(
    reader: impl FnOnce(u64) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let _guard = TASKBAR_SETTINGS_WRITE_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    reader(TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire))
}

fn load_settings_with_generation() -> anyhow::Result<(Settings, u64)> {
    with_taskbar_settings_read(|generation| {
        let settings = Settings::try_load()?;
        if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != generation {
            anyhow::bail!("settings changed while loading");
        }
        Ok((settings, generation))
    })
}

fn try_taskbar_layout_gate<T>(gate: &Mutex<T>) -> anyhow::Result<MutexGuard<'_, T>> {
    match gate.try_lock() {
        Ok(guard) => Ok(guard),
        Err(TryLockError::Poisoned(err)) => Ok(err.into_inner()),
        Err(TryLockError::WouldBlock) => {
            anyhow::bail!("taskbar layout update already in progress")
        }
    }
}

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

fn begin_native_shutdown(app: &tauri::AppHandle) {
    if let Some(scale) = app.try_state::<text_scale::SystemTextScale>() {
        scale.stop();
    }
    collector::begin_codex_app_server_shutdown();
    collector::begin_grok_acp_shutdown();
    if let Some(state) = app.try_state::<TaskbarShutdownState>() {
        state.0.store(true, Ordering::Release);
    }
    if let Some(shutdown) = app.try_state::<system_activity::SystemActivityShutdown>() {
        shutdown.stop();
    }
}

fn exit_after_taskbar_cleanup(app: tauri::AppHandle) {
    begin_native_shutdown(&app);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        app.exit(0);
    });
}

fn exit_after_update_cleanup(app: tauri::AppHandle) {
    begin_native_shutdown(&app);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        app.exit(0);
        std::thread::sleep(std::time::Duration::from_secs(2));
        std::process::exit(0);
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
            let panel_app = fallback.clone();
            let _ = fallback.run_on_main_thread(move || {
                show_panel(&panel_app);
                let _ = panel_app.emit("app-quit-cancelled", "settings flush timed out");
            });
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

    if settings.show_claude {
        if let Some(dir) = data_dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !(name.starts_with("claude_last.") && name.ends_with(".json")) {
                        continue;
                    }

                    if let Some(status) =
                        parse_claude_status_file(&entry.path(), settings, &pc_id, now)
                    {
                        statuses.push(status);
                    }
                }
            }
        }
    }

    if settings.show_codex {
        if let Some(sessions_dir) = codex_sessions_dir {
            for path in collector::list_rollouts(sessions_dir) {
                if let Some(status) = parse_codex_status_file(&path, settings, &pc_id, now) {
                    statuses.push(status);
                }
            }
        }
    }

    statuses
}

pub fn collect_representatives(settings: &Settings) -> Vec<AgentStatus> {
    collect_representatives_with_options(settings, false, false, false, false)
}

async fn collect_representatives_off_thread(settings: Settings, force: bool) -> Vec<AgentStatus> {
    collect_representatives_off_thread_with_options(settings, force, force, force, force, None)
        .await
}

async fn collect_activity_off_thread(
    settings: Settings,
    force_codex: bool,
) -> Result<activity::ActivitySnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (local, codex) = std::thread::scope(|scope| {
            let codex_worker = settings.show_codex.then(|| {
                scope.spawn(|| {
                    codex_activity::collect(
                        force_codex,
                        std::time::Instant::now()
                            + std::time::Duration::from_secs(CODEX_ACTIVITY_TIMEOUT_SECS),
                    )
                })
            });
            let local = activity::refresh(settings.show_claude, settings.show_grok);
            let codex = codex_worker.and_then(|worker| worker.join().ok());
            (local, codex)
        });
        let local = local?;
        let local = activity::merge_codex_activity(local, codex.as_ref(), settings.show_codex);
        *LAST_LOCAL_ACTIVITY
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(local.clone());
        let cursor = if settings.show_cursor {
            cursor_activity::cached_view(
                settings.activity_weeks,
                Utc::now(),
                std::time::Instant::now() + std::time::Duration::from_millis(500),
            )
            .ok()
        } else {
            None
        };
        Ok(activity::merge_cursor_activity(
            local,
            cursor.as_ref(),
            settings.show_cursor,
        ))
    })
    .await
    .map_err(|err| format!("activity collection task failed: {err}"))?
    .map_err(|err: anyhow::Error| err.to_string())
}

fn spawn_activity_refresh(app: tauri::AppHandle, settings: Settings, force_codex: bool) {
    let Some(guard) = try_begin_activity_refresh() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let started = std::time::Instant::now();
        for pass in 0..ACTIVITY_BACKFILL_MAX_PASSES {
            match collect_activity_off_thread(settings.clone(), force_codex && pass == 0).await {
                Ok(snapshot) => {
                    let backfill_pending = snapshot.local_backfill_pending;
                    if !CURSOR_ACTIVITY_REFRESH_IN_FLIGHT.load(Ordering::Acquire) {
                        let _ = app.emit("activity-updated", snapshot);
                    }
                    if !backfill_pending {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("[activity] refresh failed: {err}");
                    break;
                }
            }
            if pass + 1 >= ACTIVITY_BACKFILL_MAX_PASSES
                || started.elapsed() >= std::time::Duration::from_secs(ACTIVITY_BACKFILL_MAX_SECS)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        }
    });
}

fn spawn_cursor_activity_refresh(app: tauri::AppHandle, settings: Settings, force: bool) {
    if !settings.show_cursor {
        return;
    }
    let Some((guard, generation)) = try_begin_cursor_activity_refresh() else {
        return;
    };
    let settings_generation = TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire);
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let weeks = settings.activity_weeks;
        let result = tauri::async_runtime::spawn_blocking(move || {
            let started = std::time::Instant::now();
            let mut force_current = force;
            let mut latest = None;
            for _ in 0..CURSOR_ACTIVITY_MAX_PASSES {
                if started.elapsed() >= std::time::Duration::from_secs(CURSOR_ACTIVITY_MAX_SECS) {
                    break;
                }
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(CURSOR_ACTIVITY_PASS_SECS);
                let step = cursor_activity::refresh_step(
                    weeks,
                    Utc::now(),
                    force_current,
                    deadline,
                    || {
                        TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) == settings_generation
                            && CURSOR_ACTIVITY_GENERATION.load(Ordering::Acquire) == generation
                    },
                )?;
                force_current = false;
                let complete = step.kind == cursor_activity::RefreshStepKind::Complete;
                latest = Some(step.view);
                if complete {
                    break;
                }
            }
            Ok::<_, cursor_activity::ActivityError>(latest)
        })
        .await;
        if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != settings_generation
            || CURSOR_ACTIVITY_GENERATION.load(Ordering::Acquire) != generation
        {
            return;
        }
        let current_settings = match Settings::try_load() {
            Ok(settings) if settings.show_cursor => settings,
            _ => return,
        };
        let local = LAST_LOCAL_ACTIVITY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let Some(local) = local else {
            return;
        };
        let cursor = match result {
            Ok(Ok(Some(view))) => Some(view),
            Ok(Ok(None)) => cursor_activity::cached_view(
                current_settings.activity_weeks,
                Utc::now(),
                std::time::Instant::now() + std::time::Duration::from_millis(500),
            )
            .ok(),
            Ok(Err(error)) => {
                eprintln!("[activity] Cursor refresh failed: {error}");
                if matches!(
                    error.kind,
                    cursor_dashboard::DashboardErrorKind::Deadline
                        | cursor_dashboard::DashboardErrorKind::Transport
                ) {
                    cursor_activity::cached_view(
                        current_settings.activity_weeks,
                        Utc::now(),
                        std::time::Instant::now() + std::time::Duration::from_millis(500),
                    )
                    .ok()
                    .map(|mut view| {
                        view.partial = true;
                        view
                    })
                } else {
                    None
                }
            }
            Err(error) => {
                eprintln!("[activity] Cursor task failed: {error}");
                None
            }
        };
        let mut snapshot =
            activity::merge_cursor_activity(local, cursor.as_ref(), current_settings.show_cursor);
        if cursor.is_none() {
            snapshot.cursor_partial = true;
            snapshot.cursor_backfill_pending = false;
            snapshot.partial = snapshot.local_partial || snapshot.cursor_partial;
            snapshot.backfill_pending = snapshot.local_backfill_pending;
        }
        if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) == settings_generation
            && CURSOR_ACTIVITY_GENERATION.load(Ordering::Acquire) == generation
        {
            let _ = app.emit("activity-updated", snapshot);
        }
    });
}

async fn collect_representatives_off_thread_with_options(
    settings: Settings,
    force_codex_account: bool,
    force_claude_usage: bool,
    force_grok_billing: bool,
    force_cursor_usage: bool,
    late_cursor_app: Option<tauri::AppHandle>,
) -> Vec<AgentStatus> {
    match tauri::async_runtime::spawn_blocking(move || {
        collect_representatives_with_options_and_late_app(
            &settings,
            force_codex_account,
            force_claude_usage,
            force_grok_billing,
            force_cursor_usage,
            late_cursor_app,
        )
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

async fn collect_force_refresh_off_thread(
    settings: Settings,
    late_cursor_app: Option<tauri::AppHandle>,
) -> (Vec<AgentStatus>, bool) {
    let Some(_guard) = try_begin_force_refresh() else {
        return (
            filter_enabled_statuses(combined_collection_last_result(), &settings),
            false,
        );
    };
    (
        collect_representatives_off_thread_with_options(
            settings,
            true,
            true,
            true,
            true,
            late_cursor_app,
        )
        .await,
        true,
    )
}

fn collect_representatives_with_options(
    settings: &Settings,
    force_codex_account: bool,
    force_claude_usage: bool,
    force_grok_billing: bool,
    force_cursor_usage: bool,
) -> Vec<AgentStatus> {
    collect_representatives_with_options_and_late_app(
        settings,
        force_codex_account,
        force_claude_usage,
        force_grok_billing,
        force_cursor_usage,
        None,
    )
}

fn collect_representatives_with_options_and_late_app(
    settings: &Settings,
    force_codex_account: bool,
    force_claude_usage: bool,
    force_grok_billing: bool,
    force_cursor_usage: bool,
    late_cursor_app: Option<tauri::AppHandle>,
) -> Vec<AgentStatus> {
    if recover_pending_collection_policies().is_err() {
        eprintln!("[collector] pending collection policy recovery failed");
    }
    if !settings.show_claude
        && !settings.show_codex
        && !settings.show_grok
        && !settings.show_cursor
        && !settings.show_antigravity
    {
        return Vec::new();
    }
    let data_dir = paths::data_dir();
    let codex_sessions_dir = dirs::home_dir().map(|home| home.join(".codex").join("sessions"));
    let now = Utc::now();
    let started = std::time::Instant::now();
    let provider_deadline =
        started + std::time::Duration::from_secs(COLLECTION_REFRESH_DEADLINE_SECS);
    let cursor_deadline = started + std::time::Duration::from_secs(CURSOR_USAGE_TIMEOUT_SECS);
    let on_time_cursor_app = late_cursor_app.clone();
    let antigravity_result = settings.show_antigravity.then(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let force =
            force_codex_account || force_claude_usage || force_grok_billing || force_cursor_usage;
        let thread = std::thread::Builder::new()
            .name("juice-antigravity-usage".into())
            .spawn(move || {
                antigravity::refresh(force, started + antigravity::COLLECTION_TIMEOUT);
                // Queue completion only: a toggle can retire this result while
                // the other providers are still being collected.
                let _ = sender.send(());
            })
            .ok();
        (receiver, thread)
    });
    let cursor_result = settings.show_cursor.then(|| {
        let cursor_settings = settings.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        let thread = std::thread::Builder::new()
            .name("juice-cursor-usage".into())
            .spawn(move || {
                let result = collect_cursor_representative(
                    &cursor_settings,
                    force_cursor_usage,
                    now,
                    cursor_deadline,
                );
                deliver_cursor_result(sender, result, |result| {
                    let Some(app) = late_cursor_app else {
                        return;
                    };
                    let _ = publish_late_cursor_result(
                        result,
                        Settings::try_load,
                        combined_collection_last_result,
                        &CURSOR_USAGE_CACHE,
                        |snapshot| emit_validated_collection_snapshot(&app, &snapshot),
                    );
                });
            })
            .ok();
        (receiver, thread)
    });

    let force = (settings.show_codex && force_codex_account)
        || (settings.show_claude && force_claude_usage)
        || (settings.show_grok && force_grok_billing);
    let mut statuses = COLLECTION_COORDINATOR.run(force, || {
        collect_representatives_runtime(
            settings,
            data_dir.as_deref(),
            codex_sessions_dir.as_deref(),
            now,
            CollectionForces {
                codex_account: force_codex_account,
                claude_usage: force_claude_usage,
                grok_billing: force_grok_billing,
                cursor_usage: false,
            },
            provider_deadline,
        )
    });
    let mut emit_on_time_cursor = false;
    if let Some((receiver, thread)) = cursor_result {
        let remaining = provider_deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(result) => {
                emit_on_time_cursor = result.attempted;
                statuses.extend(result.statuses);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                statuses.extend(CURSOR_COLLECTION_COORDINATOR.last_result())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        }
        drop(thread);
    }
    if let Some((receiver, thread)) = antigravity_result {
        let remaining = provider_deadline.saturating_duration_since(std::time::Instant::now());
        let _ = receiver.recv_timeout(remaining);
        statuses.extend(antigravity::cached());
        drop(thread);
    }
    let statuses = filter_enabled_statuses(
        filter_current_cursor_samples(latest_per_tool(&statuses), &CURSOR_USAGE_CACHE),
        settings,
    );
    if emit_on_time_cursor {
        if let Some(app) = on_time_cursor_app {
            let _ = publish_late_cursor_result(
                CursorCollectionResult {
                    attempted: true,
                    statuses: statuses
                        .iter()
                        .filter(|status| status.tool == Tool::Cursor)
                        .cloned()
                        .collect(),
                },
                Settings::try_load,
                combined_collection_last_result,
                &CURSOR_USAGE_CACHE,
                |snapshot| emit_validated_collection_snapshot(&app, &snapshot),
            );
        }
    }
    statuses
}

#[derive(Clone)]
struct CursorCollectionResult {
    statuses: Vec<AgentStatus>,
    attempted: bool,
}

fn collect_cursor_representative(
    settings: &Settings,
    force: bool,
    now: DateTime<Utc>,
    deadline: std::time::Instant,
) -> CursorCollectionResult {
    let (statuses, attempted) = CURSOR_COLLECTION_COORDINATOR.run_if_idle_with_flag(|| {
        let pc_id = gethostname::gethostname().to_string_lossy().to_string();
        collect_cursor_usage_status(settings, &pc_id, now, force, deadline)
            .into_iter()
            .collect()
    });
    CursorCollectionResult {
        statuses: filter_current_cursor_samples(statuses, &CURSOR_USAGE_CACHE),
        attempted,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CollectionPlan {
    claude_status: bool,
    claude_account: bool,
    codex: bool,
    grok: bool,
    cursor: bool,
    force_claude_account: bool,
    force_codex_account: bool,
    force_grok_billing: bool,
    force_cursor_usage: bool,
}

fn collection_plan(
    settings: &Settings,
    force_codex_account: bool,
    force_claude_usage: bool,
    force_grok_billing: bool,
    force_cursor_usage: bool,
) -> CollectionPlan {
    CollectionPlan {
        claude_status: settings.show_claude,
        claude_account: settings.show_claude
            && (force_claude_usage || settings.claude_account_auto_collect_on),
        codex: settings.show_codex,
        grok: settings.show_grok,
        cursor: settings.show_cursor,
        force_claude_account: settings.show_claude && force_claude_usage,
        force_codex_account: settings.show_codex && force_codex_account,
        force_grok_billing: settings.show_grok && force_grok_billing,
        force_cursor_usage: settings.show_cursor && force_cursor_usage,
    }
}

fn filter_enabled_statuses(
    mut statuses: Vec<AgentStatus>,
    settings: &Settings,
) -> Vec<AgentStatus> {
    statuses.retain(|status| match status.tool {
        Tool::Claude => settings.show_claude,
        Tool::Codex => settings.show_codex,
        Tool::Grok => settings.show_grok,
        Tool::Cursor => settings.show_cursor,
        Tool::Antigravity => settings.show_antigravity,
    });
    statuses
}

fn combined_collection_last_result() -> Vec<AgentStatus> {
    let mut statuses = COLLECTION_COORDINATOR.last_result();
    statuses.extend(CURSOR_COLLECTION_COORDINATOR.last_result());
    statuses.extend(antigravity::cached());
    filter_current_cursor_samples(latest_per_tool(&statuses), &CURSOR_USAGE_CACHE)
}

fn filter_current_cursor_samples(
    mut statuses: Vec<AgentStatus>,
    cache: &Mutex<Option<CursorCachedStatusAttempt>>,
) -> Vec<AgentStatus> {
    let cached = cache.lock().unwrap_or_else(|error| error.into_inner());
    let current = cached
        .as_ref()
        .filter(|cached| {
            let scope_valid = match cached.source {
                Some(CursorStatusSource::Dashboard(scope))
                | Some(CursorStatusSource::Agent(Some(scope))) => cached.known_scope == Some(scope),
                Some(CursorStatusSource::Agent(None)) => cached.known_scope.is_none(),
                None => false,
            };
            scope_valid
                && !matches!(
                    cached.error,
                    Some(
                        CollectionErrorKind::LoginRequired
                            | CollectionErrorKind::SourceChanged
                            | CollectionErrorKind::CredentialsDecode
                            | CollectionErrorKind::Unavailable
                    )
                )
        })
        .and_then(|cached| cached.last_good.as_ref());
    statuses.retain(|status| {
        if status.tool != Tool::Cursor {
            return true;
        }
        current.is_some_and(|current| {
            let mut sample = status.clone();
            sample.session.active = current.session.active;
            sample == *current
        })
    });
    statuses
}

fn deliver_cursor_result<T>(
    sender: std::sync::mpsc::SyncSender<T>,
    value: T,
    on_late: impl FnOnce(T),
) {
    if let Err(std::sync::mpsc::SendError(value)) = sender.send(value) {
        on_late(value);
    }
}

fn publish_late_cursor_result(
    result: CursorCollectionResult,
    load_current_settings: impl FnOnce() -> anyhow::Result<Settings>,
    current_snapshot: impl FnOnce() -> Vec<AgentStatus>,
    cursor_cache: &Mutex<Option<CursorCachedStatusAttempt>>,
    publish: impl FnOnce(Vec<AgentStatus>),
) -> anyhow::Result<()> {
    if !result.attempted {
        return Ok(());
    }
    // Keep settings writes out until the current-settings snapshot is published.
    with_taskbar_settings_read(|generation| {
        let settings = load_current_settings()?;
        if !settings.show_cursor {
            return Ok(());
        }
        let mut snapshot = current_snapshot();
        snapshot.extend(result.statuses);
        let snapshot = filter_current_cursor_samples(snapshot, cursor_cache);
        let snapshot = filter_enabled_statuses(latest_per_tool(&snapshot), &settings);
        if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) == generation {
            publish(snapshot);
        }
        Ok(())
    })
}

#[derive(Clone, Copy)]
struct CollectionForces {
    codex_account: bool,
    claude_usage: bool,
    grok_billing: bool,
    cursor_usage: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CollectionDeadlines {
    rollout: std::time::Instant,
    codex_reserve: std::time::Duration,
    claude: std::time::Instant,
    grok: std::time::Instant,
}

fn collection_deadlines(plan: CollectionPlan, deadline: std::time::Instant) -> CollectionDeadlines {
    let claude_reserve = if plan.claude_account {
        std::time::Duration::from_secs(CLAUDE_COLLECTION_MIN_BUDGET_SECS)
    } else {
        std::time::Duration::ZERO
    };
    let grok_reserve = if plan.grok {
        std::time::Duration::from_secs(GROK_COLLECTION_MIN_BUDGET_SECS)
    } else {
        std::time::Duration::ZERO
    };
    CollectionDeadlines {
        rollout: deadline_with_reserve(
            deadline,
            std::time::Duration::from_secs(CODEX_ACCOUNT_API_TIMEOUT_SECS)
                + claude_reserve
                + grok_reserve,
        ),
        codex_reserve: claude_reserve + grok_reserve,
        claude: deadline_with_reserve(deadline, grok_reserve),
        grok: deadline,
    }
}

fn collect_representatives_runtime(
    settings: &Settings,
    data_dir: Option<&std::path::Path>,
    codex_sessions_dir: Option<&std::path::Path>,
    now: DateTime<Utc>,
    forces: CollectionForces,
    deadline: std::time::Instant,
) -> Vec<AgentStatus> {
    let pc_id = gethostname::gethostname().to_string_lossy().to_string();
    let mut statuses = Vec::new();
    let plan = collection_plan(
        settings,
        forces.codex_account,
        forces.claude_usage,
        forces.grok_billing,
        forces.cursor_usage,
    );
    let deadlines = collection_deadlines(plan, deadline);

    let claude_status = plan
        .claude_status
        .then(|| {
            recent_matching_files(data_dir, |name| {
                name.starts_with("claude_last.") && name.ends_with(".json")
            })
            .into_iter()
            .find_map(|path| parse_claude_status_file(&path, settings, &pc_id, now))
        })
        .flatten();
    let codex_rollout = plan
        .codex
        .then(|| {
            codex_sessions_dir.and_then(|sessions_dir| {
                collect_codex_rollout_status(
                    sessions_dir,
                    settings,
                    &pc_id,
                    now,
                    plan.force_codex_account,
                    deadlines.rollout,
                )
            })
        })
        .flatten();
    let codex_account = if plan.codex {
        collect_codex_account_status(
            settings,
            &pc_id,
            now,
            plan.force_codex_account,
            deadline,
            deadlines.codex_reserve,
        )
    } else {
        None
    };
    let desktop_source = plan.claude_account && claude_desktop::selected();
    let claude_usage = if plan.claude_account {
        collect_claude_usage_status(
            settings,
            &pc_id,
            now,
            plan.force_claude_account,
            deadlines.claude,
            desktop_source,
        )
    } else {
        None
    };
    if let Some(status) =
        merge_claude_usage_status(claude_status.filter(|_| !desktop_source), claude_usage)
    {
        statuses.push(status);
    }

    if let Some(status) = merge_codex_account_status(codex_rollout, codex_account) {
        statuses.push(status);
    }

    if plan.grok {
        if let Some(status) = collect_grok_billing_status(
            settings,
            &pc_id,
            now,
            plan.force_grok_billing,
            deadlines.grok,
        ) {
            statuses.push(status);
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

    if settings.show_claude {
        if let Some(status) = recent_matching_files(data_dir, |name| {
            name.starts_with("claude_last.") && name.ends_with(".json")
        })
        .into_iter()
        .find_map(|path| parse_claude_status_file(&path, settings, &pc_id, now))
        {
            statuses.push(status);
        }
    }

    if settings.show_codex {
        if let Some(sessions_dir) = codex_sessions_dir {
            for path in collector::recent_rollouts(sessions_dir, CODEX_REPRESENTATIVE_CANDIDATES) {
                if let Some(status) = parse_codex_status_file(&path, settings, &pc_id, now) {
                    statuses.push(status);
                    break;
                }
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
    {
        let cache = cache.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = cache.as_ref() {
            let retry_backoff_active = cached.error.is_some() && now < cached.retry_at;
            let success_cache_fresh = cached.error.is_none() && now < cached.retry_at;
            let honor_cooldown = !force || cached.error == Some(CollectionErrorKind::RateLimited);
            if honor_cooldown
                && now >= cached.attempted_at
                && (retry_backoff_active || success_cache_fresh)
            {
                return cached.last_good.clone();
            }
        }
    }

    let cache_guard = cache.lock().unwrap_or_else(|err| err.into_inner());
    let previous_last_good = cache_guard
        .as_ref()
        .and_then(|cached| cached.last_good.clone());
    let previous_failures = cache_guard
        .as_ref()
        .map_or(0, |cached| cached.consecutive_failures);
    let authentication_failed = cache_guard
        .as_ref()
        .is_some_and(|cached| cached.error == Some(CollectionErrorKind::LoginRequired));
    drop(cache_guard);

    let outcome = collect();
    let (last_good, error, consecutive_failures, retry_secs) = match outcome {
        Ok(status) => (Some(status), None, 0, minimum_interval_secs.max(1)),
        Err(error) => {
            let consecutive_failures = previous_failures.saturating_add(1);
            let retry_secs =
                collection_retry_delay_secs(&error, minimum_interval_secs, consecutive_failures);
            let authentication_failed =
                authentication_failed || error == CollectionErrorKind::LoginRequired;
            (
                if authentication_failed {
                    None
                } else {
                    previous_last_good
                },
                Some(if authentication_failed {
                    CollectionErrorKind::LoginRequired
                } else {
                    error
                }),
                consecutive_failures,
                retry_secs,
            )
        }
    };
    let retry_at = now + chrono::Duration::seconds(retry_secs);
    let mut cache_guard = cache.lock().unwrap_or_else(|err| err.into_inner());
    *cache_guard = Some(CachedStatusAttempt {
        attempted_at: now,
        last_good: last_good.clone(),
        error,
        retry_at,
        consecutive_failures,
    });
    last_good
}

fn collection_retry_delay_secs(
    error: &CollectionErrorKind,
    minimum_interval_secs: i64,
    consecutive_failures: u32,
) -> i64 {
    let minimum = minimum_interval_secs.max(1);
    let base = if matches!(
        error,
        CollectionErrorKind::Parse
            | CollectionErrorKind::LoginRequired
            | CollectionErrorKind::Unavailable
            | CollectionErrorKind::RateLimited
            | CollectionErrorKind::CredentialsDecode
            | CollectionErrorKind::SourceChanged
    ) {
        minimum.max(COLLECTION_STICKY_ERROR_MIN_BACKOFF_SECS)
    } else {
        minimum
    };
    let exponent = consecutive_failures.saturating_sub(1).min(4);
    base.saturating_mul(1i64 << exponent)
        .min(COLLECTION_MAX_BACKOFF_SECS)
}

fn classify_collection_error(error: &anyhow::Error) -> CollectionErrorKind {
    if error.is::<collector::ClaudeUsageRateLimited>() {
        return CollectionErrorKind::RateLimited;
    }
    if collector::error_requires_login(error) {
        return CollectionErrorKind::LoginRequired;
    }
    let message = format!("{error:#}").to_ascii_lowercase();
    if [
        "executable was not found",
        "executable unavailable",
        "command unavailable",
        "program not found",
        "os error 2",
        "cannot find the file",
        "지정된 파일을 찾을 수 없습니다",
    ]
    .iter()
    .any(|marker| message.contains(marker))
    {
        CollectionErrorKind::Unavailable
    } else {
        CollectionErrorKind::Transport
    }
}

fn cached_collection_health(cache: &Mutex<Option<CachedStatusAttempt>>) -> CollectionHealth {
    let cache = cache.lock().unwrap_or_else(|err| err.into_inner());
    match cache.as_ref().and_then(|attempt| attempt.error.as_ref()) {
        Some(CollectionErrorKind::LoginRequired) => CollectionHealth::LoginRequired,
        Some(CollectionErrorKind::Unavailable) => CollectionHealth::Unavailable,
        Some(CollectionErrorKind::RateLimited) => CollectionHealth::RateLimited,
        Some(CollectionErrorKind::CredentialsDecode) => CollectionHealth::CredentialsError,
        Some(CollectionErrorKind::SourceChanged) => CollectionHealth::SourceChanged,
        Some(
            CollectionErrorKind::Deadline
            | CollectionErrorKind::Transport
            | CollectionErrorKind::Parse,
        ) => CollectionHealth::TransientError,
        None if cache
            .as_ref()
            .and_then(|attempt| attempt.last_good.as_ref())
            .is_some() =>
        {
            CollectionHealth::Ready
        }
        None => CollectionHealth::Unavailable,
    }
}

fn cursor_collection_health() -> CollectionHealth {
    let cache = CURSOR_USAGE_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    match cache.as_ref().and_then(|attempt| attempt.error.as_ref()) {
        Some(CollectionErrorKind::LoginRequired) => CollectionHealth::LoginRequired,
        Some(CollectionErrorKind::Unavailable) => CollectionHealth::Unavailable,
        Some(CollectionErrorKind::RateLimited) => CollectionHealth::RateLimited,
        Some(CollectionErrorKind::CredentialsDecode) => CollectionHealth::CredentialsError,
        Some(CollectionErrorKind::SourceChanged) => CollectionHealth::SourceChanged,
        Some(
            CollectionErrorKind::Deadline
            | CollectionErrorKind::Transport
            | CollectionErrorKind::Parse,
        ) => CollectionHealth::TransientError,
        None if cache
            .as_ref()
            .and_then(|attempt| attempt.last_good.as_ref())
            .is_some() =>
        {
            CollectionHealth::Ready
        }
        None => CollectionHealth::Unavailable,
    }
}

fn collection_health_snapshot() -> CollectionHealthSnapshot {
    CollectionHealthSnapshot {
        claude: claude_collection_health(&CLAUDE_USAGE_CACHE),
        codex: cached_collection_health(&CODEX_ACCOUNT_CACHE),
        grok: cached_collection_health(&GROK_BILLING_CACHE),
        cursor: cursor_collection_health(),
        antigravity: match antigravity::last_error() {
            Some(antigravity::Error::AppRequired) => CollectionHealth::AppRequired,
            Some(antigravity::Error::LoginRequired | antigravity::Error::CliLoginRequired) => {
                CollectionHealth::LoginRequired
            }
            Some(antigravity::Error::Unavailable | antigravity::Error::CliUnavailable) => {
                CollectionHealth::TransientError
            }
            None if antigravity::cached().is_some() => CollectionHealth::Ready,
            None => CollectionHealth::Unavailable,
        },
    }
}

fn claude_collection_health(cache: &Mutex<Option<CachedStatusAttempt>>) -> CollectionHealth {
    let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        return CollectionHealth::Idle;
    }
    let parse_failed = guard
        .as_ref()
        .is_some_and(|c| c.error == Some(CollectionErrorKind::Parse));
    drop(guard);
    if parse_failed {
        CollectionHealth::ParseError
    } else {
        cached_collection_health(cache)
    }
}

fn consume_current_collection_snapshot<T>(
    load_current_settings: impl FnOnce() -> anyhow::Result<Settings>,
    current_snapshot: impl FnOnce() -> Vec<AgentStatus>,
    cursor_cache: &Mutex<Option<CursorCachedStatusAttempt>>,
    consume: impl FnOnce(Vec<AgentStatus>) -> T,
) -> anyhow::Result<T> {
    with_taskbar_settings_read(|generation| {
        let settings = load_current_settings()?;
        let snapshot = filter_current_cursor_samples(current_snapshot(), cursor_cache);
        let snapshot = filter_enabled_statuses(latest_per_tool(&snapshot), &settings);
        anyhow::ensure!(
            TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) == generation,
            "settings changed while publishing collection"
        );
        Ok(consume(snapshot))
    })
}

fn current_collection_snapshot() -> anyhow::Result<Vec<AgentStatus>> {
    consume_current_collection_snapshot(
        Settings::try_load,
        combined_collection_last_result,
        &CURSOR_USAGE_CACHE,
        |snapshot| snapshot,
    )
}

fn emit_collection_snapshot(app: &tauri::AppHandle) {
    let _ = consume_current_collection_snapshot(
        Settings::try_load,
        combined_collection_last_result,
        &CURSOR_USAGE_CACHE,
        |snapshot| emit_validated_collection_snapshot(app, &snapshot),
    );
}

fn emit_validated_collection_snapshot(app: &tauri::AppHandle, statuses: &[AgentStatus]) {
    let statuses = filter_current_cursor_samples(statuses.to_vec(), &CURSOR_USAGE_CACHE);
    let _ = app.emit("collection-health-updated", collection_health_snapshot());
    let _ = app.emit("status-updated", &statuses);
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
                .map_err(|error| classify_collection_error(&error))?;
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
    desktop_source: bool,
) -> Option<AgentStatus> {
    let prepared = desktop_source.then(claude_desktop::prepare);
    let revision = prepared.as_ref().and_then(|r| r.as_ref().ok()).map_or_else(
        || claude_desktop::revision(desktop_source),
        |c| c.revision(),
    );
    reconcile_claude_source(&CLAUDE_USAGE_CACHE, &CLAUDE_SOURCE_REVISION, revision);
    let status = cached_status_attempt(
        &CLAUDE_USAGE_CACHE,
        now,
        CLAUDE_USAGE_CACHE_MIN_SECS,
        force,
        || {
            let captured_at = now.to_rfc3339();
            if desktop_source {
                let timeout = remaining_refresh_budget(
                    deadline,
                    std::time::Duration::from_secs(CLAUDE_USAGE_TIMEOUT_SECS),
                )?;
                let raw = prepared
                    .expect("desktop credentials prepared")
                    .and_then(|c| {
                        claude_desktop::usage_prepared(c, std::time::Instant::now() + timeout)
                    })
                    .map_err(|e| match e {
                        claude_desktop::Error::LoginRequired => CollectionErrorKind::LoginRequired,
                        claude_desktop::Error::Deadline => CollectionErrorKind::Deadline,
                        claude_desktop::Error::Transport => CollectionErrorKind::Transport,
                        claude_desktop::Error::Parse => CollectionErrorKind::Parse,
                        claude_desktop::Error::RateLimited => CollectionErrorKind::RateLimited,
                        claude_desktop::Error::Decode => CollectionErrorKind::CredentialsDecode,
                        claude_desktop::Error::Changed => CollectionErrorKind::SourceChanged,
                        claude_desktop::Error::Unavailable => CollectionErrorKind::Unavailable,
                    })?;
                let mut status =
                    adapters::claude::parse_oauth_usage_response(&raw, pc_id, &captured_at)
                        .map_err(|_| CollectionErrorKind::Parse)?;
                status.session_id = "claude-desktop-usage".into();
                return Ok(status);
            }
            let oauth_timeout = remaining_refresh_budget_with_reserve(
                deadline,
                std::time::Duration::from_secs(CLAUDE_USAGE_TIMEOUT_SECS),
                std::time::Duration::from_secs(CLAUDE_FALLBACK_RESERVE_SECS),
            );
            let mut oauth_error = CollectionErrorKind::Transport;
            if let Ok(oauth_timeout) = oauth_timeout {
                match collector::claude_oauth_usage_response(oauth_timeout) {
                    Ok(raw) => {
                        match adapters::claude::parse_oauth_usage_response(
                            &raw,
                            pc_id,
                            &captured_at,
                        ) {
                            Ok(status) => return Ok(status),
                            Err(_) => oauth_error = CollectionErrorKind::Parse,
                        }
                    }
                    Err(error) => oauth_error = classify_collection_error(&error),
                }
            }

            if !claude_legacy_fallback_allowed(&oauth_error, force) {
                return Err(oauth_error);
            }

            let legacy_timeout = remaining_refresh_budget(
                deadline,
                std::time::Duration::from_secs(CLAUDE_USAGE_TIMEOUT_SECS),
            )?;
            let raw = collector::claude_usage_output(legacy_timeout).map_err(|error| {
                let fallback_error = classify_collection_error(&error);
                if matches!(
                    fallback_error,
                    CollectionErrorKind::LoginRequired | CollectionErrorKind::Unavailable
                ) {
                    fallback_error
                } else {
                    oauth_error.clone()
                }
            })?;
            if collector::text_requires_login(&raw) {
                return Err(CollectionErrorKind::LoginRequired);
            }
            parse_claude_fallback_usage(&raw, pc_id, &captured_at, oauth_error)
        },
    );
    let mut status = if desktop_source {
        discard_failed_desktop_status(&CLAUDE_USAGE_CACHE, status)
    } else {
        status
    }?;
    derive_active(&mut status, settings.stale_after_secs, now);
    Some(status)
}

fn reconcile_claude_source(
    cache: &Mutex<Option<CachedStatusAttempt>>,
    previous: &Mutex<Option<claude_desktop::Revision>>,
    revision: claude_desktop::Revision,
) {
    let mut previous = previous.lock().unwrap_or_else(|e| e.into_inner());
    if previous.as_ref() != Some(&revision) {
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *previous = Some(revision);
    }
}

fn discard_failed_desktop_status(
    cache: &Mutex<Option<CachedStatusAttempt>>,
    status: Option<AgentStatus>,
) -> Option<AgentStatus> {
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(attempt) = cache.as_mut().filter(|attempt| {
        matches!(
            attempt.error,
            Some(
                CollectionErrorKind::LoginRequired
                    | CollectionErrorKind::Unavailable
                    | CollectionErrorKind::CredentialsDecode
                    | CollectionErrorKind::SourceChanged
            )
        )
    }) {
        attempt.last_good = None;
        return None;
    }
    status
}

fn claude_legacy_fallback_allowed(error: &CollectionErrorKind, force: bool) -> bool {
    !matches!(
        error,
        CollectionErrorKind::LoginRequired | CollectionErrorKind::RateLimited
    ) && (force || matches!(error, CollectionErrorKind::Parse))
}

fn parse_claude_fallback_usage(
    raw: &str,
    pc_id: &str,
    captured_at: &str,
    oauth_error: CollectionErrorKind,
) -> Result<AgentStatus, CollectionErrorKind> {
    adapters::claude::parse_usage_output(raw, pc_id, captured_at).map_err(|_| oauth_error)
}

fn collect_grok_billing_status(
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    force: bool,
    deadline: std::time::Instant,
) -> Option<AgentStatus> {
    let mut status = cached_status_attempt(
        &GROK_BILLING_CACHE,
        now,
        GROK_BILLING_CACHE_MIN_SECS,
        force,
        || {
            let timeout = remaining_refresh_budget(
                deadline,
                std::time::Duration::from_secs(GROK_BILLING_TIMEOUT_SECS),
            )?;
            let raw = collector::grok_billing_response(timeout)
                .map_err(|error| classify_collection_error(&error))?;
            adapters::grok::parse_billing_response(&raw, pc_id, &now.to_rfc3339())
                .map_err(|_| CollectionErrorKind::Parse)
        },
    )?;
    derive_active(&mut status, settings.stale_after_secs, now);
    Some(status)
}

fn collect_cursor_usage_status(
    settings: &Settings,
    pc_id: &str,
    now: DateTime<Utc>,
    force: bool,
    deadline: std::time::Instant,
) -> Option<AgentStatus> {
    let mut previous = CURSOR_USAGE_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone();
    if !force {
        if let Some(cached) = previous.as_mut() {
            let retry_backoff_active = cached.error.is_some() && now < cached.retry_at;
            let success_cache_fresh = cached.error.is_none() && now < cached.retry_at;
            if now >= cached.attempted_at && (retry_backoff_active || success_cache_fresh) {
                cached.last_good.as_ref()?;
                let current_scope = current_cursor_scope(
                    deadline.min(std::time::Instant::now() + std::time::Duration::from_millis(300)),
                );
                if revalidate_cursor_cached_source(cached, current_scope) {
                    let mut status = cached.last_good.clone()?;
                    derive_active(&mut status, cursor_stale_after_secs(settings), now);
                    return Some(status);
                }
                if let Some(cached) = CURSOR_USAGE_CACHE
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .as_mut()
                {
                    cached.last_good = None;
                    cached.source = None;
                    cached.error = Some(CollectionErrorKind::SourceChanged);
                    cached.known_scope = cached
                        .known_scope
                        .or(previous.as_ref().and_then(|cached| cached.known_scope));
                }
            }
        }
    }

    let captured_at = now.to_rfc3339();
    let cursor_budget = deadline.saturating_duration_since(std::time::Instant::now());
    let fallback_reserve = std::time::Duration::from_secs(CURSOR_AGENT_FALLBACK_MAX_RESERVE_SECS)
        .min(cursor_budget / 2);
    let dashboard_deadline = deadline
        .checked_sub(fallback_reserve)
        .unwrap_or_else(std::time::Instant::now);
    let dashboard_outcome =
        collect_cursor_dashboard_status(pc_id, &captured_at, dashboard_deadline, deadline);
    let observed_scope = match &dashboard_outcome {
        Ok((_, scope)) => Some(*scope),
        Err((_, scope)) => *scope,
    };
    if let Some(scope) = observed_scope {
        remember_cursor_account_scope(&CURSOR_USAGE_CACHE, now, scope);
    }
    if let Err((dashboard_error, scope)) = &dashboard_outcome {
        let same_scope_last_good = scope.is_some_and(|scope| {
            matches!(
                previous.as_ref().and_then(|cached| cached.source.as_ref()),
                Some(CursorStatusSource::Dashboard(previous_scope)) if *previous_scope == scope
            )
        }) && matches!(
            dashboard_error,
            CollectionErrorKind::Deadline | CollectionErrorKind::Transport
        );
        if same_scope_last_good {
            if let Some(status) = previous
                .as_ref()
                .and_then(|cached| cached.last_good.clone())
            {
                return cache_cursor_status(
                    &CURSOR_USAGE_CACHE,
                    now,
                    Some(status),
                    scope.map(CursorStatusSource::Dashboard),
                    Some(dashboard_error.clone()),
                    settings,
                );
            }
        }
    }
    let fallback_scope = dashboard_outcome
        .as_ref()
        .err()
        .and_then(|(_, scope)| *scope);
    let outcome = resolve_cursor_dashboard_first(
        dashboard_outcome,
        || collect_cursor_agent_status(pc_id, &captured_at, deadline, fallback_scope),
        || current_cursor_scope(deadline),
        previous
            .as_ref()
            .and_then(|cached| cached.known_scope)
            .or(observed_scope),
    );

    let (last_good, source, error) = match outcome {
        Ok((status, source)) => (Some(status), Some(source), None),
        Err(error) => (None, None, Some(error)),
    };
    cache_cursor_status(&CURSOR_USAGE_CACHE, now, last_good, source, error, settings)
}

fn remember_cursor_account_scope(
    cache: &Mutex<Option<CursorCachedStatusAttempt>>,
    now: DateTime<Utc>,
    scope: cursor_dashboard::AccountScope,
) {
    let mut cache = cache.lock().unwrap_or_else(|err| err.into_inner());
    let cached = cache.get_or_insert_with(|| CursorCachedStatusAttempt {
        attempted_at: now,
        last_good: None,
        source: None,
        known_scope: None,
        error: None,
        retry_at: now,
        consecutive_failures: 0,
    });
    cached.known_scope = cached.known_scope.or(Some(scope));
    let source_changed = match cached.source {
        Some(CursorStatusSource::Dashboard(previous))
        | Some(CursorStatusSource::Agent(Some(previous))) => previous != scope,
        Some(CursorStatusSource::Agent(None)) => true,
        None => false,
    };
    if source_changed {
        cached.last_good = None;
        cached.source = None;
        cached.error = Some(CollectionErrorKind::SourceChanged);
    }
}

fn revalidate_cursor_cached_source(
    cached: &mut CursorCachedStatusAttempt,
    current_scope: Result<cursor_dashboard::AccountScope, CollectionErrorKind>,
) -> bool {
    let matches = match cached.source {
        Some(CursorStatusSource::Dashboard(scope))
        | Some(CursorStatusSource::Agent(Some(scope))) => current_scope == Ok(scope),
        Some(CursorStatusSource::Agent(None)) if cached.known_scope.is_none() => matches!(
            current_scope,
            Err(CollectionErrorKind::LoginRequired | CollectionErrorKind::Unavailable)
        ),
        _ => false,
    };
    if !matches {
        // Once credentials appear, a later read failure must not revive legacy-only values.
        cached.known_scope = cached.known_scope.or(current_scope.ok());
        cached.last_good = None;
        cached.source = None;
        cached.error = Some(CollectionErrorKind::SourceChanged);
    }
    matches
}

fn current_cursor_scope(
    deadline: std::time::Instant,
) -> Result<cursor_dashboard::AccountScope, CollectionErrorKind> {
    cursor_dashboard::read_credentials(deadline)
        .map(|credentials| credentials.scope)
        .map_err(|error| dashboard_collection_error(error.kind))
}

fn validate_cursor_dashboard_response<T>(
    expected: cursor_dashboard::AccountScope,
    response: Result<T, CollectionErrorKind>,
    current_scope: impl FnOnce() -> Result<cursor_dashboard::AccountScope, CollectionErrorKind>,
) -> Result<T, (CollectionErrorKind, Option<cursor_dashboard::AccountScope>)> {
    // Revalidate failed responses too, before retaining last-good or starting a fallback.
    match current_scope() {
        Ok(current) if current == expected => response.map_err(|error| (error, Some(expected))),
        Err(CollectionErrorKind::LoginRequired) => {
            Err((CollectionErrorKind::LoginRequired, Some(expected)))
        }
        _ => Err((CollectionErrorKind::SourceChanged, Some(expected))),
    }
}

fn collect_cursor_dashboard_status(
    pc_id: &str,
    captured_at: &str,
    deadline: std::time::Instant,
    revalidation_deadline: std::time::Instant,
) -> Result<
    (AgentStatus, cursor_dashboard::AccountScope),
    (CollectionErrorKind, Option<cursor_dashboard::AccountScope>),
> {
    let credentials = cursor_dashboard::read_credentials(deadline)
        .map_err(|error| (dashboard_collection_error(error.kind), None))?;
    let scope = credentials.scope;
    let response = cursor_dashboard::current_period_usage(&credentials, deadline)
        .map_err(|error| dashboard_collection_error(error.kind));
    let usage = validate_cursor_dashboard_response(scope, response, || {
        current_cursor_scope(cursor_scope_revalidation_deadline(
            revalidation_deadline,
            std::time::Instant::now(),
        ))
    })?;
    let reset = DateTime::<Utc>::from_timestamp_millis(usage.billing_cycle_end_ms)
        .ok_or((CollectionErrorKind::Parse, Some(scope)))?;
    let status = adapters::cursor::dashboard_usage_status(
        pc_id,
        captured_at,
        usage.cursor_models_used_percent,
        usage.other_models_used_percent,
        reset.to_rfc3339(),
    )
    .map_err(|_| (CollectionErrorKind::Parse, Some(scope)))?;
    Ok((status, scope))
}

fn cursor_scope_revalidation_deadline(
    outer_deadline: std::time::Instant,
    now: std::time::Instant,
) -> std::time::Instant {
    outer_deadline.min(now + std::time::Duration::from_secs(CURSOR_SCOPE_REVALIDATION_MAX_SECS))
}

fn collect_cursor_agent_status(
    pc_id: &str,
    captured_at: &str,
    deadline: std::time::Instant,
    expected_scope: Option<cursor_dashboard::AccountScope>,
) -> Result<AgentStatus, CollectionErrorKind> {
    if std::time::Instant::now() >= deadline {
        return Err(CollectionErrorKind::Deadline);
    }
    let workspace = paths::data_dir()
        .map(|path| path.join("cursor-usage-workspace"))
        .ok_or(CollectionErrorKind::Unavailable)?;
    let proof = match expected_scope {
        Some(expected) => {
            let proof = cursor_dashboard::read_cli_identity(deadline)
                .map_err(|error| dashboard_collection_error(error.kind))?;
            ensure_cursor_cli_scope(expected, proof.scope)?;
            Some(proof)
        }
        None => None,
    };
    let capture = match proof.as_ref() {
        Some(proof) => {
            cursor_pty::capture_cursor_usage_with_identity_until(&workspace, deadline, proof)
        }
        None => cursor_pty::capture_cursor_usage_until(&workspace, deadline),
    };
    let raw = capture.map_err(|error| {
        if let Some(error) = error.downcast_ref::<cursor_dashboard::DashboardError>() {
            return dashboard_collection_error(error.kind);
        }
        if std::time::Instant::now() >= deadline
            || format!("{error:#}")
                .to_ascii_lowercase()
                .contains("timed out")
        {
            CollectionErrorKind::Deadline
        } else {
            classify_collection_error(&error)
        }
    })?;
    adapters::cursor::parse_usage_status(&raw, pc_id, captured_at)
        .map_err(|_| CollectionErrorKind::Parse)
}

fn ensure_cursor_cli_scope(
    expected: cursor_dashboard::AccountScope,
    actual: cursor_dashboard::AccountScope,
) -> Result<(), CollectionErrorKind> {
    if expected == actual {
        Ok(())
    } else {
        Err(CollectionErrorKind::SourceChanged)
    }
}

fn cursor_agent_fallback_allowed(error: &CollectionErrorKind) -> bool {
    matches!(
        error,
        CollectionErrorKind::Parse
            | CollectionErrorKind::LoginRequired
            | CollectionErrorKind::Unavailable
    )
}

fn resolve_cursor_dashboard_first(
    dashboard: Result<
        (AgentStatus, cursor_dashboard::AccountScope),
        (CollectionErrorKind, Option<cursor_dashboard::AccountScope>),
    >,
    collect_agent: impl FnOnce() -> Result<AgentStatus, CollectionErrorKind>,
    current_scope: impl FnOnce() -> Result<cursor_dashboard::AccountScope, CollectionErrorKind>,
    previous_scope: Option<cursor_dashboard::AccountScope>,
) -> Result<(AgentStatus, CursorStatusSource), CollectionErrorKind> {
    match dashboard {
        Ok((status, scope)) => Ok((status, CursorStatusSource::Dashboard(scope))),
        Err((error, None)) if previous_scope.is_some() => {
            Err(if error == CollectionErrorKind::LoginRequired {
                CollectionErrorKind::LoginRequired
            } else {
                CollectionErrorKind::SourceChanged
            })
        }
        Err((CollectionErrorKind::LoginRequired, Some(_))) => {
            Err(CollectionErrorKind::LoginRequired)
        }
        Err((dashboard_error, _)) if !cursor_agent_fallback_allowed(&dashboard_error) => {
            Err(dashboard_error)
        }
        Err((dashboard_error, scope)) => {
            let result = collect_agent();
            let result = if let Some(scope) = scope {
                validate_cursor_dashboard_response(scope, result, current_scope)
                    .map_err(|(error, _)| error)
            } else {
                result
            };
            match result {
                Ok(status) => Ok((status, CursorStatusSource::Agent(scope))),
                Err(CollectionErrorKind::SourceChanged) => Err(CollectionErrorKind::SourceChanged),
                Err(CollectionErrorKind::LoginRequired) if scope.is_some() => {
                    Err(CollectionErrorKind::LoginRequired)
                }
                Err(agent_error) => Err(combine_cursor_collection_errors(
                    agent_error,
                    dashboard_error,
                )),
            }
        }
    }
}

fn dashboard_collection_error(kind: cursor_dashboard::DashboardErrorKind) -> CollectionErrorKind {
    match kind {
        cursor_dashboard::DashboardErrorKind::Deadline => CollectionErrorKind::Deadline,
        cursor_dashboard::DashboardErrorKind::Transport => CollectionErrorKind::Transport,
        cursor_dashboard::DashboardErrorKind::Parse
        | cursor_dashboard::DashboardErrorKind::Oversized => CollectionErrorKind::Parse,
        cursor_dashboard::DashboardErrorKind::LoginRequired => CollectionErrorKind::LoginRequired,
        cursor_dashboard::DashboardErrorKind::Unavailable => CollectionErrorKind::Unavailable,
        cursor_dashboard::DashboardErrorKind::ScopeChanged => CollectionErrorKind::SourceChanged,
    }
}

fn combine_cursor_collection_errors(
    agent: CollectionErrorKind,
    dashboard: CollectionErrorKind,
) -> CollectionErrorKind {
    if matches!(agent, CollectionErrorKind::SourceChanged)
        || matches!(dashboard, CollectionErrorKind::SourceChanged)
    {
        return CollectionErrorKind::SourceChanged;
    }
    if matches!(dashboard, CollectionErrorKind::LoginRequired) {
        return CollectionErrorKind::LoginRequired;
    }
    if matches!(
        agent,
        CollectionErrorKind::Deadline | CollectionErrorKind::Transport | CollectionErrorKind::Parse
    ) || matches!(
        dashboard,
        CollectionErrorKind::Deadline | CollectionErrorKind::Transport | CollectionErrorKind::Parse
    ) {
        return CollectionErrorKind::Transport;
    }
    if matches!(agent, CollectionErrorKind::LoginRequired) {
        CollectionErrorKind::LoginRequired
    } else {
        CollectionErrorKind::Unavailable
    }
}

fn cache_cursor_status(
    cache: &Mutex<Option<CursorCachedStatusAttempt>>,
    now: DateTime<Utc>,
    last_good: Option<AgentStatus>,
    source: Option<CursorStatusSource>,
    error: Option<CollectionErrorKind>,
    settings: &Settings,
) -> Option<AgentStatus> {
    let mut cached = cache.lock().unwrap_or_else(|err| err.into_inner());
    let previous_failures = cached
        .as_ref()
        .map_or(0, |cached| cached.consecutive_failures);
    let known_scope = match source.as_ref() {
        Some(CursorStatusSource::Dashboard(scope))
        | Some(CursorStatusSource::Agent(Some(scope))) => Some(*scope),
        _ => cached.as_ref().and_then(|cached| cached.known_scope),
    };
    let consecutive_failures = if error.is_some() {
        previous_failures.saturating_add(1)
    } else {
        0
    };
    let retry_secs = error.as_ref().map_or(CURSOR_USAGE_CACHE_MIN_SECS, |error| {
        collection_retry_delay_secs(error, CURSOR_USAGE_CACHE_MIN_SECS, consecutive_failures)
    });
    let retry_at = now + chrono::Duration::seconds(retry_secs);
    *cached = Some(CursorCachedStatusAttempt {
        attempted_at: now,
        last_good: last_good.clone(),
        source,
        known_scope,
        error,
        retry_at,
        consecutive_failures,
    });
    drop(cached);
    let mut status = last_good?;
    derive_active(&mut status, cursor_stale_after_secs(settings), now);
    Some(status)
}

fn cursor_stale_after_secs(settings: &Settings) -> i64 {
    settings
        .stale_after_secs
        .max(CURSOR_USAGE_CACHE_MIN_SECS + 30)
}

fn merge_claude_usage_status(
    statusline: Option<AgentStatus>,
    usage: Option<AgentStatus>,
) -> Option<AgentStatus> {
    match (statusline, usage) {
        (_, Some(usage)) if usage.session_id == "claude-desktop-usage" => Some(usage),
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
    let mut grok: Option<&AgentStatus> = None;
    let mut cursor: Option<&AgentStatus> = None;
    let mut antigravity: Option<&AgentStatus> = None;

    for status in all {
        let slot = match &status.tool {
            Tool::Claude => &mut claude,
            Tool::Codex => &mut codex,
            Tool::Grok => &mut grok,
            Tool::Cursor => &mut cursor,
            Tool::Antigravity => &mut antigravity,
        };

        if slot
            .as_ref()
            .is_none_or(|current| captured_is_newer(status, current))
        {
            *slot = Some(status);
        }
    }

    [claude, codex, grok, cursor, antigravity]
        .into_iter()
        .flatten()
        .cloned()
        .collect()
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

fn tray_menu_labels(language: &str, system_language: u16) -> [&'static str; 5] {
    if notification_uses_korean(language, system_language) {
        [
            "Juice 열기",
            "사용량 새로고침",
            "바 표출 일시중지",
            "바 표출 재개",
            "종료",
        ]
    } else {
        [
            "Open Juice",
            "Refresh usage",
            "Pause bars",
            "Resume bars",
            "Quit",
        ]
    }
}

fn build_tray_menu<R: tauri::Runtime>(
    app: &impl tauri::Manager<R>,
    language: &str,
) -> tauri::Result<tauri::menu::Menu<R>> {
    let labels = tray_menu_labels(language, system_ui_language());
    MenuBuilder::new(app)
        .text(tray_open_menu_id(), labels[0])
        .text(tray_refresh_menu_id(), labels[1])
        .text(tray_pause_bar_menu_id(), labels[2])
        .text(tray_resume_bar_menu_id(), labels[3])
        .separator()
        .text(tray_quit_menu_id(), labels[4])
        .build()
}

fn refresh_tray_menu(app: &tauri::AppHandle, language: &str) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id(tray_id()) {
        tray.set_menu(Some(build_tray_menu(app, language)?))?;
    }
    Ok(())
}

fn setup_trays(app: &mut tauri::App) -> tauri::Result<()> {
    let default_icon = app.default_window_icon().cloned();
    let menu = build_tray_menu(app, &Settings::load().language)?;

    let mut builder = TrayIconBuilder::with_id(tray_id())
        .tooltip(tray_tooltip())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            id if id == tray_open_menu_id() => show_panel(app),
            id if id == tray_refresh_menu_id() => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let Ok(settings) = Settings::try_load() else {
                        return;
                    };
                    let activity_settings = settings.clone();
                    let (_, collected) =
                        collect_force_refresh_off_thread(settings, Some(app.clone())).await;
                    if collected {
                        emit_collection_snapshot(&app);
                    }
                    spawn_activity_refresh(app.clone(), activity_settings.clone(), true);
                    spawn_cursor_activity_refresh(app, activity_settings, true);
                });
            }
            id if id == tray_pause_bar_menu_id() => {
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Err(err) = pause_taskbar_bars_for_manager(&app) {
                        eprintln!("[taskbar] pause bars failed: {err}");
                    }
                });
            }
            id if id == tray_resume_bar_menu_id() => {
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Err(err) = resume_taskbar_bars_for_manager(&app) {
                        eprintln!("[taskbar] resume bars failed: {err}");
                    }
                });
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

fn create_panel_window(app: &tauri::AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("index.html".into()))
        .title("Juice")
        .inner_size(620.0, 720.0)
        .min_inner_size(480.0, 560.0)
        .decorations(false)
        .visible(false)
        .skip_taskbar(false)
        .always_on_top(false)
        .resizable(true)
        .shadow(true)
        .build()?;
    attach_panel_close_hide(&window);
    Ok(window)
}

fn show_panel(app: &tauri::AppHandle) {
    let window = match app.get_webview_window("panel") {
        Some(window) => window,
        None => match create_panel_window(app) {
            Ok(window) => window,
            Err(err) => {
                eprintln!("[panel] recreate failed: {err}");
                return;
            }
        },
    };
    if let Err(err) = window.show() {
        eprintln!("[panel] show failed: {err}");
        return;
    }
    let _ = window.unminimize();
    let _ = window.set_focus();
    let _ = window.emit("panel-visibility-updated", true);
}

fn attach_panel_close_hide(window: &tauri::WebviewWindow) {
    let panel = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = panel.emit("panel-visibility-updated", false);
            let _ = panel.hide();
        }
    });
}

fn setup_panel_close_hide(app: &tauri::App) {
    if let Some(window) = app.get_webview_window("panel") {
        attach_panel_close_hide(&window);
    }
}

fn spawn_status_loop(
    handle: tauri::AppHandle,
    mut system_activity: system_activity::SystemActivityMonitor,
) {
    tauri::async_runtime::spawn(async move {
        system_activity
            .wait_until_ready_or_timeout(std::time::Duration::from_millis(1_500))
            .await;
        loop {
            if system_activity.is_shutdown() {
                return;
            }
            system_activity.wait_until_active().await;
            if system_activity.is_shutdown() {
                return;
            }
            let settings = match Settings::try_load() {
                Ok(settings) => settings,
                Err(err) => {
                    eprintln!("[collector] settings unavailable; poll skipped: {err}");
                    let snapshot = system_activity.snapshot();
                    system_activity
                        .wait_for_change_or_timeout(
                            snapshot.generation,
                            std::time::Duration::from_secs(1),
                        )
                        .await;
                    continue;
                }
            };
            let interval_secs = settings.poll_interval_secs.max(1);
            let started = system_activity.snapshot();
            if !started.active {
                continue;
            }
            let _ = collect_representatives_off_thread(settings, false).await;

            if !system_activity.publish_if_current(started, || {
                emit_collection_snapshot(&handle);
            }) {
                continue;
            }
            let waiting = system_activity.snapshot();
            if !waiting.active {
                continue;
            }
            system_activity
                .wait_for_change_or_timeout(
                    waiting.generation,
                    std::time::Duration::from_secs(interval_secs),
                )
                .await;
        }
    });
}

fn normalize_taskbar_tool(tool: &str) -> Option<&'static str> {
    match tool {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        "cursor" => Some("cursor"),
        "antigravity" => Some("antigravity"),
        _ => None,
    }
}

fn isolated_forced_taskbar_hover(
    isolated_data_dir: bool,
    candidate: Option<&str>,
) -> Option<&'static str> {
    isolated_data_dir
        .then(|| candidate.and_then(normalize_taskbar_tool))
        .flatten()
}

fn taskbar_bar_label(tool: &str) -> Option<&'static str> {
    match normalize_taskbar_tool(tool)? {
        "claude" => Some("bar-claude"),
        "codex" => Some("bar-codex"),
        "grok" => Some("bar-grok"),
        "cursor" => Some("bar-cursor"),
        "antigravity" => Some("bar-antigravity"),
        _ => None,
    }
}

fn taskbar_window_slot<'a>(
    state: &'a TaskbarWindowState,
    tool: &str,
) -> Option<&'a Mutex<Option<TaskbarWindowHandle>>> {
    match normalize_taskbar_tool(tool)? {
        "claude" => Some(&state.claude),
        "codex" => Some(&state.codex),
        "grok" => Some(&state.grok),
        "cursor" => Some(&state.cursor),
        "antigravity" => Some(&state.antigravity),
        _ => None,
    }
}

#[cfg(windows)]
fn taskbar_window_handle<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
) -> Option<TaskbarWindowHandle> {
    let state = manager.try_state::<TaskbarWindowState>()?;
    let handle = *taskbar_window_slot(&state, tool)?
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    handle
}

#[cfg(windows)]
fn taskbar_window_hwnd<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
) -> Option<windows::Win32::Foundation::HWND> {
    taskbar_window_handle(manager, tool)
        .map(|handle| windows::Win32::Foundation::HWND(handle.raw as *mut core::ffi::c_void))
}

#[cfg(windows)]
fn set_taskbar_window_hwnd<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    hwnd: Option<windows::Win32::Foundation::HWND>,
) {
    let Some(state) = manager.try_state::<TaskbarWindowState>() else {
        return;
    };
    let Some(slot) = taskbar_window_slot(&state, tool) else {
        return;
    };
    let generation = TASKBAR_WINDOW_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    *slot.lock().unwrap_or_else(|err| err.into_inner()) = hwnd.map(|value| TaskbarWindowHandle {
        raw: value.0 as isize,
        generation,
    });
}

#[cfg(windows)]
fn register_taskbar_window_handles(app: &tauri::App) {
    for tool in TASKBAR_TOOLS {
        let hwnd = taskbar_bar_label(tool)
            .and_then(|label| app.get_webview_window(label))
            .and_then(|window| window.hwnd().ok());
        set_taskbar_window_hwnd(app, tool, hwnd);
    }
}

#[cfg(windows)]
fn taskbar_bar_window_is_alive(app: &tauri::AppHandle, tool: &str) -> bool {
    taskbar_window_hwnd(app, tool).is_some_and(taskbar::window_is_valid)
}

#[cfg(windows)]
fn taskbar_bar_window_is_visible(app: &tauri::AppHandle, tool: &str) -> bool {
    taskbar_window_hwnd(app, tool).is_some_and(taskbar::window_is_visible)
}

#[cfg(windows)]
fn taskbar_bar_hit_is_cover(
    bar: windows::Win32::Foundation::HWND,
    hit: windows::Win32::Foundation::HWND,
    root: windows::Win32::Foundation::HWND,
    juice_bars: &[windows::Win32::Foundation::HWND],
) -> bool {
    if hit.0.is_null() || bar == hit || (!root.0.is_null() && bar == root) {
        return false;
    }
    !juice_bars
        .iter()
        .any(|candidate| *candidate == hit || (!root.0.is_null() && *candidate == root))
}

#[cfg(windows)]
fn taskbar_bar_window_is_covered(app: &tauri::AppHandle, tool: &str) -> bool {
    use windows::Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT},
    };

    let Some(hwnd) = taskbar_window_hwnd(app, tool) else {
        return false;
    };
    let Ok(rect) = current_bar_rect(app, tool) else {
        return false;
    };
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return false;
    }
    let point = POINT {
        x: rect.left + (rect.right - rect.left) / 2,
        y: rect.top + (rect.bottom - rect.top) / 2,
    };
    let hit = unsafe { WindowFromPoint(point) };
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    taskbar_bar_hit_is_cover(hwnd, hit, root, &taskbar_bar_hwnds(app))
}

#[cfg(windows)]
fn taskbar_bar_window_overlay_contract_matches(app: &tauri::AppHandle, tool: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GWLP_HWNDPARENT, GWL_EXSTYLE, GWL_STYLE,
    };

    let Some(hwnd) = taskbar_window_hwnd(app, tool) else {
        return false;
    };
    unsafe {
        bar_overlay_contract_matches(
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
            GetWindowLongPtrW(hwnd, GWL_STYLE),
            GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT),
        )
    }
}

fn taskbar_observation_requires_reapply(
    expected_visible: bool,
    visible: bool,
    covered: bool,
    overlay_contract: bool,
) -> bool {
    expected_visible != visible || (expected_visible && visible && (covered || !overlay_contract))
}

#[cfg(windows)]
fn create_taskbar_bar_window(app: &tauri::AppHandle, tool: &str) -> anyhow::Result<()> {
    let label = taskbar_bar_label(tool).ok_or_else(|| anyhow::anyhow!("unknown taskbar tool"))?;
    let url = WebviewUrl::App(format!("bar.html?tool={tool}").into());
    let window = WebviewWindowBuilder::new(app, label, url)
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
    set_taskbar_window_hwnd(app, tool, Some(window.hwnd()?));
    Ok(())
}

#[cfg(windows)]
fn request_taskbar_bar_recovery(app: &tauri::AppHandle) {
    if app
        .try_state::<TaskbarShutdownState>()
        .is_some_and(|state| state.0.load(Ordering::Acquire))
    {
        return;
    }
    let Some(state) = app.try_state::<TaskbarRecoveryState>() else {
        return;
    };
    if state.0.swap(true, Ordering::AcqRel) {
        return;
    }

    let settings_snapshot = match load_settings_with_generation() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            state.0.store(false, Ordering::Release);
            eprintln!("[taskbar] recovery skipped; settings unavailable: {err}");
            return;
        }
    };
    let recovery_app = app.clone();
    if let Err(err) = app.run_on_main_thread(move || {
        if recovery_app
            .try_state::<TaskbarShutdownState>()
            .is_some_and(|state| state.0.load(Ordering::Acquire))
        {
            if let Some(state) = recovery_app.try_state::<TaskbarRecoveryState>() {
                state.0.store(false, Ordering::Release);
            }
            return;
        }
        {
            let Ok(_layout_guard) = try_taskbar_layout_gate(&TASKBAR_LAYOUT_GATE) else {
                if let Some(state) = recovery_app.try_state::<TaskbarRecoveryState>() {
                    state.0.store(false, Ordering::Release);
                }
                return;
            };
            for tool in TASKBAR_TOOLS {
                if taskbar_bar_window_is_alive(&recovery_app, tool) {
                    continue;
                }
                set_taskbar_menu_state(&recovery_app, tool, false);
                if let Some(stale) =
                    taskbar_bar_label(tool).and_then(|label| recovery_app.get_webview_window(label))
                {
                    set_taskbar_window_hwnd(&recovery_app, tool, None);
                    let _ = stale.destroy();
                }
                if let Err(err) = create_taskbar_bar_window(&recovery_app, tool) {
                    eprintln!("[taskbar] recreate {tool} bar failed: {err}");
                }
            }
        }
        if let Err(err) = apply_taskbar_dock_for_generation(
            &recovery_app,
            &settings_snapshot.0,
            settings_snapshot.1,
        ) {
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
        Some("grok") => settings.grok_taskbar_offset_ratio,
        Some("cursor") => settings.cursor_taskbar_offset_ratio,
        Some("antigravity") => settings.antigravity_taskbar_offset_ratio,
        _ => settings.taskbar_offset_ratio,
    }
}

fn set_taskbar_offset_ratio(settings: &mut Settings, tool: &str, ratio: f32) {
    let ratio = ratio.clamp(0.0, 1.0);
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_offset_ratio = ratio,
        Some("codex") => settings.codex_taskbar_offset_ratio = ratio,
        Some("grok") => settings.grok_taskbar_offset_ratio = ratio,
        Some("cursor") => settings.cursor_taskbar_offset_ratio = ratio,
        Some("antigravity") => settings.antigravity_taskbar_offset_ratio = ratio,
        _ => settings.taskbar_offset_ratio = ratio,
    }
}

fn taskbar_monitor_key<'a>(settings: &'a Settings, tool: &str) -> &'a str {
    match normalize_taskbar_tool(tool) {
        Some("claude") => &settings.claude_taskbar_monitor_key,
        Some("codex") => &settings.codex_taskbar_monitor_key,
        Some("grok") => &settings.grok_taskbar_monitor_key,
        Some("cursor") => &settings.cursor_taskbar_monitor_key,
        Some("antigravity") => &settings.antigravity_taskbar_monitor_key,
        _ => "",
    }
}

fn taskbar_target_initialized(settings: &Settings, tool: &str) -> bool {
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_target_initialized,
        Some("codex") => settings.codex_taskbar_target_initialized,
        Some("grok") => settings.grok_taskbar_target_initialized,
        Some("cursor") => settings.cursor_taskbar_target_initialized,
        Some("antigravity") => settings.antigravity_taskbar_target_initialized,
        _ => true,
    }
}

fn set_taskbar_target_initialized(settings: &mut Settings, tool: &str, initialized: bool) {
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_target_initialized = initialized,
        Some("codex") => settings.codex_taskbar_target_initialized = initialized,
        Some("grok") => settings.grok_taskbar_target_initialized = initialized,
        Some("cursor") => settings.cursor_taskbar_target_initialized = initialized,
        Some("antigravity") => settings.antigravity_taskbar_target_initialized = initialized,
        _ => {}
    }
}

fn taskbar_targets_need_initialization(settings: &Settings) -> bool {
    TASKBAR_TOOLS.into_iter().any(|tool| {
        taskbar_tool_enabled(settings, tool) && !taskbar_target_initialized(settings, tool)
    })
}

fn set_taskbar_target(settings: &mut Settings, tool: &str, monitor_key: &str, ratio: f32) {
    set_taskbar_offset_ratio(settings, tool, ratio);
    match normalize_taskbar_tool(tool) {
        Some("claude") => settings.claude_taskbar_monitor_key = monitor_key.to_string(),
        Some("codex") => settings.codex_taskbar_monitor_key = monitor_key.to_string(),
        Some("grok") => settings.grok_taskbar_monitor_key = monitor_key.to_string(),
        Some("cursor") => settings.cursor_taskbar_monitor_key = monitor_key.to_string(),
        Some("antigravity") => settings.antigravity_taskbar_monitor_key = monitor_key.to_string(),
        _ => {}
    }
    set_taskbar_target_initialized(settings, tool, true);
}

fn taskbar_profile_placement(
    settings: &Settings,
    tool: &str,
    topology: &TaskbarTopology,
) -> Option<TaskbarPlacement> {
    let monitor_key = taskbar_monitor_key(settings, tool);
    (taskbar_target_initialized(settings, tool)
        && topology.monitor_keys.iter().any(|key| key == monitor_key))
    .then(|| TaskbarPlacement {
        monitor_key: monitor_key.to_string(),
        offset_ratio: taskbar_offset_ratio(settings, tool),
    })
}

fn taskbar_layout_profile_from_current(
    settings: &Settings,
    topology: &TaskbarTopology,
) -> Option<TaskbarLayoutProfile> {
    if !topology.is_valid() {
        return None;
    }
    let profile = TaskbarLayoutProfile {
        topology: topology.clone(),
        claude: taskbar_profile_placement(settings, "claude", topology),
        codex: taskbar_profile_placement(settings, "codex", topology),
        grok: taskbar_profile_placement(settings, "grok", topology),
        cursor: taskbar_profile_placement(settings, "cursor", topology),
        antigravity: taskbar_profile_placement(settings, "antigravity", topology),
        presentation: settings
            .taskbar_profile_presentation_on
            .then(|| TaskbarPresentationProfile::from_settings(settings)),
        appearance: settings
            .taskbar_profile_colors_on
            .then(|| TaskbarAppearanceProfile::from_settings(settings)),
    };
    (profile.claude.is_some()
        || profile.codex.is_some()
        || profile.grok.is_some()
        || profile.cursor.is_some()
        || profile.antigravity.is_some())
    .then_some(profile)
}

fn apply_taskbar_layout_profile(settings: &mut Settings, topology: &TaskbarTopology) -> bool {
    let Some(profile) = settings.taskbar_layout_profile(topology).cloned() else {
        return false;
    };
    let before = settings.clone();
    if let Some(placement) = profile.claude {
        set_taskbar_target(
            settings,
            "claude",
            &placement.monitor_key,
            placement.offset_ratio,
        );
    }
    if let Some(placement) = profile.codex {
        set_taskbar_target(
            settings,
            "codex",
            &placement.monitor_key,
            placement.offset_ratio,
        );
    }
    if let Some(placement) = profile.grok {
        set_taskbar_target(
            settings,
            "grok",
            &placement.monitor_key,
            placement.offset_ratio,
        );
    }
    if let Some(placement) = profile.cursor {
        set_taskbar_target(
            settings,
            "cursor",
            &placement.monitor_key,
            placement.offset_ratio,
        );
    }
    if let Some(placement) = profile.antigravity {
        set_taskbar_target(
            settings,
            "antigravity",
            &placement.monitor_key,
            placement.offset_ratio,
        );
    }
    if settings.taskbar_profile_presentation_on {
        if let Some(presentation) = profile.presentation {
            presentation.apply_to(settings);
        }
    }
    if settings.taskbar_profile_colors_on {
        if let Some(appearance) = profile.appearance {
            appearance.apply_to(settings);
        }
    }
    !taskbar_targets_match(&before, settings)
        || TaskbarPresentationProfile::from_settings(&before)
            != TaskbarPresentationProfile::from_settings(settings)
        || TaskbarAppearanceProfile::from_settings(&before)
            != TaskbarAppearanceProfile::from_settings(settings)
}

fn record_taskbar_layout_profile(settings: &mut Settings, topology: &TaskbarTopology) -> bool {
    if !settings.taskbar_layout_memory_on {
        return false;
    }
    let Some(profile) = taskbar_layout_profile_from_current(settings, topology) else {
        return false;
    };
    settings.taskbar_layout_memory_initialized = true;
    settings.upsert_taskbar_layout_profile(profile)
}

fn complete_taskbar_layout_profile(settings: &mut Settings, topology: &TaskbarTopology) -> bool {
    let Some(mut profile) = settings.taskbar_layout_profile(topology).cloned() else {
        return false;
    };
    let Some(current) = taskbar_layout_profile_from_current(settings, topology) else {
        return false;
    };
    let mut changed = false;
    if settings.taskbar_profile_presentation_on {
        if let Some(presentation) = &mut profile.presentation {
            if presentation.font_mode.is_none() {
                presentation.font_mode = Some(settings.font_mode.clone());
                changed = true;
            }
        }
    }
    if profile.claude.is_none() && current.claude.is_some() {
        profile.claude = current.claude;
        changed = true;
    }
    if profile.codex.is_none() && current.codex.is_some() {
        profile.codex = current.codex;
        changed = true;
    }
    if profile.grok.is_none() && current.grok.is_some() {
        profile.grok = current.grok;
        changed = true;
    }
    if profile.cursor.is_none() && current.cursor.is_some() {
        profile.cursor = current.cursor;
        changed = true;
    }
    if profile.antigravity.is_none() && current.antigravity.is_some() {
        profile.antigravity = current.antigravity;
        changed = true;
    }
    if settings.taskbar_profile_presentation_on
        && profile.presentation.is_none()
        && current.presentation.is_some()
    {
        profile.presentation = current.presentation;
        changed = true;
    }
    if settings.taskbar_profile_colors_on
        && profile.appearance.is_none()
        && current.appearance.is_some()
    {
        profile.appearance = current.appearance;
        changed = true;
    }
    changed && settings.upsert_taskbar_layout_profile(profile)
}

fn apply_pending_taskbar_profile_placements(
    settings: &mut Settings,
    topology: &TaskbarTopology,
    pending: &[PendingTaskbarProfilePlacement],
) -> bool {
    if !settings.taskbar_layout_memory_on || pending.is_empty() {
        return false;
    }
    let mut profile = settings
        .taskbar_layout_profile(topology)
        .cloned()
        .or_else(|| taskbar_layout_profile_from_current(settings, topology))
        .unwrap_or_else(|| TaskbarLayoutProfile {
            topology: topology.clone(),
            claude: None,
            codex: None,
            grok: None,
            cursor: None,
            antigravity: None,
            presentation: settings
                .taskbar_profile_presentation_on
                .then(|| TaskbarPresentationProfile::from_settings(settings)),
            appearance: settings
                .taskbar_profile_colors_on
                .then(|| TaskbarAppearanceProfile::from_settings(settings)),
        });
    let mut has_matching_placement = false;
    for item in pending {
        if &item.topology != topology
            || !topology.monitor_keys.contains(&item.placement.monitor_key)
        {
            continue;
        }
        has_matching_placement = true;
        match item.tool {
            "claude" => profile.claude = Some(item.placement.clone()),
            "codex" => profile.codex = Some(item.placement.clone()),
            "grok" => profile.grok = Some(item.placement.clone()),
            "cursor" => profile.cursor = Some(item.placement.clone()),
            "antigravity" => profile.antigravity = Some(item.placement.clone()),
            _ => {}
        }
    }
    if !has_matching_placement {
        return false;
    }

    let initialized = settings.taskbar_layout_memory_initialized;
    settings.taskbar_layout_memory_initialized = true;
    let mut changed = !initialized;
    changed |= settings.upsert_taskbar_layout_profile(profile);
    changed |= apply_taskbar_layout_profile(settings, topology);
    changed
}

#[cfg(windows)]
#[derive(Default)]
struct TaskbarTopologyStability {
    candidate: TaskbarTopology,
    observations: u8,
    active: Option<TaskbarTopology>,
}

#[cfg(windows)]
impl TaskbarTopologyStability {
    fn allows_native_layout(&self, topology: &TaskbarTopology) -> bool {
        topology.is_valid() && self.active.as_ref() == Some(topology)
    }

    fn rearm(&mut self) {
        self.candidate = TaskbarTopology::default();
        self.observations = 0;
        self.active = None;
    }

    fn observe(&mut self, topology: TaskbarTopology) -> Option<TaskbarTopology> {
        if !topology.is_valid() {
            self.rearm();
            return None;
        }
        if self.candidate == topology {
            self.observations = self.observations.saturating_add(1);
        } else {
            self.candidate = topology;
            self.observations = 1;
            // The published topology is invalidated on a change, including a brief one.
            self.active = None;
        }
        if self.observations < TASKBAR_TOPOLOGY_STABLE_OBSERVATIONS
            || self.active.as_ref() == Some(&self.candidate)
        {
            return None;
        }
        self.active = Some(self.candidate.clone());
        Some(self.candidate.clone())
    }
}

fn pending_taskbar_target_ratios(
    settings: &Settings,
    taskbar_rect: taskbar::DockRect,
    tool_lengths: [Option<i32>; TASKBAR_TOOLS.len()],
) -> Option<[Option<f32>; TASKBAR_TOOLS.len()]> {
    if taskbar_rect.width <= 0 || taskbar_rect.height <= 0 {
        return None;
    }

    let horizontal = taskbar_rect.width >= taskbar_rect.height;
    let axis_length = if horizontal {
        taskbar_rect.width
    } else {
        taskbar_rect.height
    };
    let right = taskbar_rect.x.checked_add(taskbar_rect.width)?;
    let bottom = taskbar_rect.y.checked_add(taskbar_rect.height)?;
    let initialized = [
        settings.claude_taskbar_target_initialized,
        settings.codex_taskbar_target_initialized,
        settings.grok_taskbar_target_initialized,
        settings.cursor_taskbar_target_initialized,
        settings.antigravity_taskbar_target_initialized,
    ];
    let existing_ratios = [
        settings.claude_taskbar_offset_ratio,
        settings.codex_taskbar_offset_ratio,
        settings.grok_taskbar_offset_ratio,
        settings.cursor_taskbar_offset_ratio,
        settings.antigravity_taskbar_offset_ratio,
    ];
    let mut occupied = Vec::new();

    for (index, length) in tool_lengths.into_iter().enumerate() {
        let Some(length) = length.filter(|_| initialized[index]) else {
            continue;
        };
        let length = length.max(1).min(axis_length);
        let window = taskbar::dock_rect_for_taskbar_at_offset(
            taskbar_rect.x,
            taskbar_rect.y,
            right,
            bottom,
            length,
            existing_ratios[index],
        )?;
        let start = if horizontal {
            window.x.checked_sub(taskbar_rect.x)?
        } else {
            window.y.checked_sub(taskbar_rect.y)?
        };
        occupied.push((start, start.saturating_add(length)));
    }

    let mut ratios = [None; TASKBAR_TOOLS.len()];

    for (index, length) in tool_lengths.into_iter().enumerate() {
        let Some(length) = length.filter(|_| !initialized[index]) else {
            continue;
        };
        let length = length.max(1).min(axis_length);
        occupied.sort_unstable_by_key(|interval| interval.0);
        let mut leading_offset = 0;
        for &(start, end) in &occupied {
            if start.saturating_sub(leading_offset) >= length {
                break;
            }
            leading_offset = leading_offset.max(end);
        }
        leading_offset = leading_offset.min(axis_length.saturating_sub(length));
        let window = if horizontal {
            taskbar::DockRect {
                x: taskbar_rect.x.checked_add(leading_offset)?,
                y: taskbar_rect.y,
                width: length,
                height: taskbar_rect.height,
            }
        } else {
            taskbar::DockRect {
                x: taskbar_rect.x,
                y: taskbar_rect.y.checked_add(leading_offset)?,
                width: taskbar_rect.width,
                height: length,
            }
        };
        ratios[index] = Some(taskbar::offset_ratio_for_taskbar_rect(
            taskbar_rect,
            window,
        )?);
        occupied.push((leading_offset, leading_offset.saturating_add(length)));
    }

    ratios
        .into_iter()
        .any(|ratio| ratio.is_some())
        .then_some(ratios)
}

#[cfg(windows)]
fn initialize_pending_taskbar_targets<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
) -> anyhow::Result<TaskbarSettingsSnapshot> {
    if !taskbar_targets_need_initialization(settings) {
        return Ok(TaskbarSettingsSnapshot {
            settings: settings.clone(),
            revision: Settings::storage_revision(),
            generation: TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire),
            collection_changes: CollectionChanges::default(),
        });
    }
    let taskbar = taskbar::shell_taskbar_window_for_key("")?;
    let taskbar_rect = taskbar::DockRect {
        x: taskbar.left,
        y: taskbar.top,
        width: taskbar.right - taskbar.left,
        height: taskbar.bottom - taskbar.top,
    };
    let mut tool_lengths = [None; TASKBAR_TOOLS.len()];
    for (index, tool) in TASKBAR_TOOLS.into_iter().enumerate() {
        tool_lengths[index] = taskbar_dock_width_for_manager(manager, settings, tool)
            .map(|length| taskbar_physical_length_for_window(length, taskbar.hwnd));
    }
    if pending_taskbar_target_ratios(settings, taskbar_rect, tool_lengths).is_none() {
        anyhow::bail!("pending taskbar target has no valid placement");
    }

    let monitor_key = taskbar.key.clone();
    update_taskbar_settings(|current| {
        let Some(ratios) = pending_taskbar_target_ratios(current, taskbar_rect, tool_lengths)
        else {
            return;
        };
        for (tool, ratio) in TASKBAR_TOOLS.into_iter().zip(ratios) {
            if let Some(ratio) = ratio {
                set_taskbar_target(current, tool, &monitor_key, ratio);
            }
        }
    })
}

#[cfg(windows)]
fn position_taskbar_bar_on_taskbar<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    rect: taskbar::DockRect,
) -> anyhow::Result<()> {
    let hwnd = taskbar_window_hwnd(manager, tool)
        .ok_or_else(|| anyhow::anyhow!("no taskbar bar window for {tool}"))?;
    let _layout_guard = try_taskbar_layout_gate(&TASKBAR_LAYOUT_GATE)?;
    apply_taskbar_overlay(hwnd, rect)
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
        "grok" => settings.show_grok,
        "cursor" => settings.show_cursor,
        "antigravity" => settings.show_antigravity,
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
        "quad" if tool == "grok" => ring_size + TASKBAR_INDICATOR_HORIZONTAL_PADDING,
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
    layout
        .filter(|layout| layout.mode == mode)
        .map(|layout| layout.width)
}

fn taskbar_content_layout_slot<'a>(
    state: &'a TaskbarContentLayoutState,
    tool: &str,
) -> Option<&'a Mutex<TaskbarContentLayoutSlot>> {
    match normalize_taskbar_tool(tool)? {
        "claude" => Some(&state.claude),
        "codex" => Some(&state.codex),
        "grok" => Some(&state.grok),
        "cursor" => Some(&state.cursor),
        "antigravity" => Some(&state.antigravity),
        _ => None,
    }
}

fn taskbar_content_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    tool: &str,
) -> Option<TaskbarContentLayout> {
    taskbar_content_layout_snapshot(manager, settings, tool)?.1
}

fn taskbar_content_layout_snapshot<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    tool: &str,
) -> Option<(u64, Option<TaskbarContentLayout>)> {
    let state = manager.try_state::<TaskbarContentLayoutState>()?;
    let slot = taskbar_content_layout_slot(&state, tool)?
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    Some((
        slot.revision,
        slot.layout
            .as_ref()
            .filter(|layout| {
                taskbar_content_width_for_mode(&settings.bar_mode, Some(layout)).is_some()
            })
            .cloned(),
    ))
}

#[cfg(windows)]
fn taskbar_content_layout_revision<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
) -> Option<u64> {
    let state = manager.try_state::<TaskbarContentLayoutState>()?;
    let slot = taskbar_content_layout_slot(&state, tool)?
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    Some(slot.revision)
}

#[cfg(windows)]
fn validate_taskbar_content_layout_plan(
    revisions: &[(&str, Option<u64>)],
    mut current: impl FnMut(&str) -> Option<u64>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        revisions
            .iter()
            .all(|(tool, revision)| current(tool) == *revision),
        "taskbar content changed while layout was being planned"
    );
    Ok(())
}

fn begin_taskbar_content_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    expected_revision: u64,
    layout: TaskbarContentLayout,
) -> anyhow::Result<TaskbarContentLayoutWrite> {
    let state = manager
        .try_state::<TaskbarContentLayoutState>()
        .ok_or_else(|| anyhow::anyhow!("taskbar content state is unavailable"))?;
    let slot = taskbar_content_layout_slot(&state, tool)
        .ok_or_else(|| anyhow::anyhow!("taskbar content slot is unavailable"))?;
    begin_taskbar_content_layout_in_slot(slot, expected_revision, layout)
}

fn acknowledge_taskbar_content_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    expected_revision: u64,
) -> anyhow::Result<()> {
    let state = manager
        .try_state::<TaskbarContentLayoutState>()
        .ok_or_else(|| anyhow::anyhow!("taskbar content state is unavailable"))?;
    let slot = taskbar_content_layout_slot(&state, tool)
        .ok_or_else(|| anyhow::anyhow!("taskbar content slot is unavailable"))?;
    acknowledge_taskbar_content_layout_in_slot(slot, expected_revision)
}

fn acknowledge_taskbar_content_layout_in_slot(
    slot: &Mutex<TaskbarContentLayoutSlot>,
    expected_revision: u64,
) -> anyhow::Result<()> {
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    anyhow::ensure!(
        current.revision == expected_revision,
        "taskbar content changed while measuring"
    );
    current.revision = current.revision.wrapping_add(1);
    Ok(())
}

fn begin_taskbar_content_layout_in_slot(
    slot: &Mutex<TaskbarContentLayoutSlot>,
    expected_revision: u64,
    layout: TaskbarContentLayout,
) -> anyhow::Result<TaskbarContentLayoutWrite> {
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    anyhow::ensure!(
        current.revision == expected_revision,
        "taskbar content changed while measuring"
    );
    current.revision = current.revision.wrapping_add(1);
    let previous = current.layout.replace(layout);
    Ok(TaskbarContentLayoutWrite {
        revision: current.revision,
        previous,
    })
}

fn rollback_taskbar_content_layout<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    write: TaskbarContentLayoutWrite,
) {
    let Some(state) = manager.try_state::<TaskbarContentLayoutState>() else {
        return;
    };
    let Some(slot) = taskbar_content_layout_slot(&state, tool) else {
        return;
    };
    rollback_taskbar_content_layout_in_slot(slot, write);
}

fn rollback_taskbar_content_layout_in_slot(
    slot: &Mutex<TaskbarContentLayoutSlot>,
    write: TaskbarContentLayoutWrite,
) -> bool {
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    if current.revision != write.revision {
        return false;
    }
    current.revision = current.revision.wrapping_add(1);
    current.layout = write.previous;
    true
}

fn update_taskbar_content_layout_ratio(layout: &mut Option<TaskbarContentLayout>, ratio: f32) {
    if let Some(layout) = layout.as_mut() {
        layout.ratio = Some(ratio.clamp(0.0, 1.0));
    }
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
    set_taskbar_content_layout_ratio_in_slot(slot, ratio);
}

fn set_taskbar_content_layout_ratio_in_slot(slot: &Mutex<TaskbarContentLayoutSlot>, ratio: f32) {
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    current.revision = current.revision.wrapping_add(1);
    update_taskbar_content_layout_ratio(&mut current.layout, ratio);
}

#[cfg(windows)]
fn set_taskbar_content_layout_ratio_for_generation<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    ratio: f32,
    expected_generation: u64,
) -> anyhow::Result<()> {
    with_taskbar_settings_read(|generation| {
        anyhow::ensure!(
            generation == expected_generation,
            "taskbar settings changed before position publication"
        );
        set_taskbar_content_layout_ratio(manager, tool, ratio);
        Ok(())
    })
}

fn sync_taskbar_content_layout_ratios<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
) {
    for tool in TASKBAR_TOOLS {
        set_taskbar_content_layout_ratio(manager, tool, taskbar_offset_ratio(settings, tool));
    }
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
    (((logical_length.max(1) as i64) * (dpi as i64) + 95) / 96).clamp(1, i32::MAX as i64) as i32
}

fn taskbar_measured_logical_length(
    css_length: f64,
    device_pixel_ratio: Option<f64>,
    target_scale: f64,
) -> Result<i32, String> {
    let pixel_ratio = device_pixel_ratio.unwrap_or(target_scale);
    if !css_length.is_finite()
        || !(1.0..=2304.0).contains(&css_length)
        || !pixel_ratio.is_finite()
        || !(0.25..=8.0).contains(&pixel_ratio)
        || !target_scale.is_finite()
        || !(0.25..=8.0).contains(&target_scale)
    {
        return Err("taskbar content measurement is out of range".into());
    }
    let logical_length = (css_length * pixel_ratio / target_scale).ceil();
    if !(1.0..=4096.0).contains(&logical_length) {
        return Err("taskbar content measurement is out of range".into());
    }
    Ok(logical_length as i32)
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

fn taskbar_ratio_preserving_layout_leading_edge(
    taskbar_rect: taskbar::DockRect,
    current_length: i32,
    current_ratio: f32,
    new_length: i32,
) -> Option<f32> {
    let current_window = taskbar::dock_rect_for_taskbar_at_offset(
        taskbar_rect.x,
        taskbar_rect.y,
        taskbar_rect.x.checked_add(taskbar_rect.width)?,
        taskbar_rect.y.checked_add(taskbar_rect.height)?,
        current_length,
        current_ratio,
    )?;
    taskbar_ratio_preserving_leading_edge(taskbar_rect, current_window, new_length)
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
    #[cfg(windows)]
    if let Some(hwnd) = taskbar_window_hwnd(manager, tool) {
        if let Err(err) = taskbar::hide_window(hwnd) {
            eprintln!("[taskbar] native hide {tool} bar failed: {err}");
        }
    }

    #[cfg(not(windows))]
    if let Some(label) = taskbar_bar_label(tool) {
        if let Some(window) = manager.get_webview_window(label) {
            let _ = window.hide();
        }
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

fn set_stable_taskbar_topology<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    topology: &TaskbarTopology,
) -> bool {
    let Some(state) = manager.try_state::<TaskbarStableTopologyState>() else {
        return false;
    };
    let mut state = state.0.lock().unwrap_or_else(|err| err.into_inner());
    try_publish_stable_taskbar_topology(&mut state, topology)
}

fn try_publish_stable_taskbar_topology(
    state: &mut TaskbarStableTopologyData,
    topology: &TaskbarTopology,
) -> bool {
    if !topology.is_empty()
        && state
            .pending_placements
            .iter()
            .any(|item| &item.topology == topology)
    {
        return false;
    }
    state.topology = topology.clone();
    true
}

fn stable_taskbar_topology_matches<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    topology: &TaskbarTopology,
) -> bool {
    manager
        .try_state::<TaskbarStableTopologyState>()
        .map(|state| {
            state
                .0
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .topology
                == *topology
        })
        .unwrap_or(false)
}

fn stable_taskbar_topology<R: tauri::Runtime>(manager: &impl tauri::Manager<R>) -> TaskbarTopology {
    manager
        .try_state::<TaskbarStableTopologyState>()
        .map(|state| {
            state
                .0
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .topology
                .clone()
        })
        .unwrap_or_default()
}

fn remember_pending_taskbar_profile_placement<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    item: PendingTaskbarProfilePlacement,
) -> bool {
    let Some(state) = manager.try_state::<TaskbarStableTopologyState>() else {
        return false;
    };
    let mut state = state.0.lock().unwrap_or_else(|err| err.into_inner());
    store_pending_taskbar_profile_placement(&mut state, item)
}

fn store_pending_taskbar_profile_placement(
    state: &mut TaskbarStableTopologyData,
    item: PendingTaskbarProfilePlacement,
) -> bool {
    if state.topology == item.topology {
        return false;
    }
    state
        .pending_placements
        .retain(|saved| saved.topology != item.topology || saved.tool != item.tool);
    state.pending_placements.push(item);
    if state.pending_placements.len() > MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS {
        let overflow = state.pending_placements.len() - MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS;
        state.pending_placements.drain(..overflow);
    }
    true
}

fn pending_taskbar_profile_placements<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    topology: &TaskbarTopology,
) -> Vec<PendingTaskbarProfilePlacement> {
    manager
        .try_state::<TaskbarStableTopologyState>()
        .map(|state| {
            state
                .0
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .pending_placements
                .iter()
                .filter(|item| &item.topology == topology)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn clear_pending_taskbar_profile_placements<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    processed: &[PendingTaskbarProfilePlacement],
) {
    if processed.is_empty() {
        return;
    }
    let Some(state) = manager.try_state::<TaskbarStableTopologyState>() else {
        return;
    };
    state
        .0
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .pending_placements
        .retain(|item| !processed.contains(item));
}

fn clear_all_pending_taskbar_profile_placements<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
) {
    if let Some(state) = manager.try_state::<TaskbarStableTopologyState>() {
        clear_pending_taskbar_profile_state(
            &mut state.0.lock().unwrap_or_else(|err| err.into_inner()),
        );
    }
}

fn clear_pending_taskbar_profile_state(state: &mut TaskbarStableTopologyData) {
    state.pending_placements.clear();
}

fn set_taskbar_bars_paused<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, paused: bool) {
    if let Some(state) = manager.try_state::<TaskbarPauseState>() {
        state.0.store(paused, Ordering::Relaxed);
    }
}

fn taskbar_menu_slot<'a>(
    state: &'a TaskbarMenuState,
    tool: &str,
) -> Option<&'a Mutex<TaskbarMenuSlot>> {
    match normalize_taskbar_tool(tool)? {
        "claude" => Some(&state.claude),
        "codex" => Some(&state.codex),
        "grok" => Some(&state.grok),
        "cursor" => Some(&state.cursor),
        "antigravity" => Some(&state.antigravity),
        _ => None,
    }
}

fn taskbar_menu_layout_snapshot<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
) -> Option<(u64, TaskbarMenuLayout)> {
    let state = manager.try_state::<TaskbarMenuState>()?;
    let slot = taskbar_menu_slot(&state, tool)?
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    Some((slot.revision, slot.layout))
}

fn taskbar_menu_is_open<R: tauri::Runtime>(manager: &impl tauri::Manager<R>, tool: &str) -> bool {
    taskbar_menu_layout_snapshot(manager, tool)
        .map(|(_, layout)| layout.open)
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
    if let Some(slot) = taskbar_menu_slot(&state, tool) {
        set_taskbar_menu_layout_in_slot(slot, open, ratio);
    }
}

fn set_taskbar_menu_layout_in_slot(
    slot: &Mutex<TaskbarMenuSlot>,
    open: bool,
    ratio: Option<f32>,
) -> TaskbarMenuWrite {
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    current.revision = current.revision.wrapping_add(1);
    current.layout = TaskbarMenuLayout {
        open,
        ratio: ratio.map(|value| value.clamp(0.0, 1.0)),
    };
    TaskbarMenuWrite {
        revision: current.revision,
    }
}

fn set_taskbar_menu_layout_for_generation<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    open: bool,
    ratio: Option<f32>,
    expected_generation: u64,
) -> anyhow::Result<TaskbarMenuWrite> {
    with_taskbar_settings_read(|generation| {
        anyhow::ensure!(
            generation == expected_generation,
            "taskbar settings changed before menu publication"
        );
        let state = manager
            .try_state::<TaskbarMenuState>()
            .ok_or_else(|| anyhow::anyhow!("taskbar menu state is unavailable"))?;
        let slot = taskbar_menu_slot(&state, tool)
            .ok_or_else(|| anyhow::anyhow!("taskbar menu slot is unavailable"))?;
        Ok(set_taskbar_menu_layout_in_slot(slot, open, ratio))
    })
}

fn close_taskbar_menu_layout_if_current<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    tool: &str,
    write: TaskbarMenuWrite,
) -> bool {
    let Some(state) = manager.try_state::<TaskbarMenuState>() else {
        return false;
    };
    let Some(slot) = taskbar_menu_slot(&state, tool) else {
        return false;
    };
    close_taskbar_menu_layout_in_slot_if_current(slot, write)
}

fn close_taskbar_menu_layout_in_slot_if_current(
    slot: &Mutex<TaskbarMenuSlot>,
    write: TaskbarMenuWrite,
) -> bool {
    let mut current = slot.lock().unwrap_or_else(|err| err.into_inner());
    if current.revision != write.revision {
        return false;
    }
    current.revision = current.revision.wrapping_add(1);
    current.layout = TaskbarMenuLayout::default();
    true
}

fn taskbar_layout_ratio<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    tool: &str,
) -> f32 {
    let menu_ratio = taskbar_menu_layout_snapshot(manager, tool)
        .and_then(|(_, layout)| layout.open.then_some(layout.ratio).flatten());
    menu_ratio
        .or_else(|| taskbar_content_layout(manager, settings, tool).and_then(|layout| layout.ratio))
        .unwrap_or_else(|| taskbar_offset_ratio(settings, tool))
}

fn pause_taskbar_bars_for_manager<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
) -> anyhow::Result<()> {
    let _pause_guard = TASKBAR_PAUSE_WRITE_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let snapshot = update_taskbar_settings(|settings| settings.taskbar_bars_paused = true)?;
    set_taskbar_bars_paused(manager, snapshot.settings.taskbar_bars_paused);
    hide_all_taskbar_bars(manager);
    Ok(())
}

fn resume_taskbar_bars_for_manager<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
) -> anyhow::Result<()> {
    let _pause_guard = TASKBAR_PAUSE_WRITE_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let snapshot = update_taskbar_settings(|settings| settings.taskbar_bars_paused = false)?;
    set_taskbar_bars_paused(manager, snapshot.settings.taskbar_bars_paused);
    apply_taskbar_dock_for_generation(manager, &snapshot.settings, snapshot.generation)
}

#[cfg(windows)]
fn taskbar_bar_hwnds<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
) -> Vec<windows::Win32::Foundation::HWND> {
    TASKBAR_TOOLS
        .iter()
        .filter_map(|tool| taskbar_window_hwnd(manager, tool))
        .collect()
}

#[cfg(windows)]
struct TaskbarDockSnapshot {
    taskbars: Vec<taskbar::ShellTaskbarWindow>,
    monitor_states: std::collections::HashMap<String, (bool, bool)>,
}

#[cfg(windows)]
fn taskbar_topology_signature(snapshot: &TaskbarDockSnapshot) -> String {
    snapshot
        .taskbars
        .iter()
        .map(|taskbar| {
            format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                taskbar.key,
                taskbar.left,
                taskbar.top,
                taskbar.right,
                taskbar.bottom,
                taskbar.monitor.width,
                taskbar.monitor.height,
                taskbar.dpi
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(windows)]
fn taskbar_profile_topology_keys(snapshot: &TaskbarDockSnapshot) -> TaskbarTopology {
    taskbar_profile_topology(&snapshot.taskbars)
}

#[cfg(windows)]
fn taskbar_profile_topology(taskbars: &[taskbar::ShellTaskbarWindow]) -> TaskbarTopology {
    TaskbarTopology::from_modes(
        taskbars
            .iter()
            .map(|taskbar| TaskbarMonitorMode {
                monitor_key: taskbar.key.clone(),
                width: taskbar.monitor.width,
                height: taskbar.monitor.height,
                dpi: taskbar.dpi,
            })
            .collect(),
    )
}

#[cfg(windows)]
fn reconcile_taskbar_layout_profile(
    settings: &Settings,
    topology: &TaskbarTopology,
    pending: &[PendingTaskbarProfilePlacement],
) -> anyhow::Result<bool> {
    if !settings.taskbar_layout_memory_on || !topology.is_valid() {
        return Ok(false);
    }

    if !taskbar_layout_profile_reconcile_needed(settings, topology, pending) {
        return Ok(false);
    }

    let mut changed = false;
    update_taskbar_settings(|current| {
        changed = reconcile_taskbar_profile_state(current, topology, pending);
    })?;
    if !changed {
        return Ok(false);
    }
    Ok(true)
}

fn reconcile_taskbar_profile_state(
    current: &mut Settings,
    topology: &TaskbarTopology,
    pending: &[PendingTaskbarProfilePlacement],
) -> bool {
    if !current.taskbar_layout_memory_on || !topology.is_valid() {
        return false;
    }
    let mut changed = current.inherit_legacy_taskbar_layout_profile(topology);
    if !pending.is_empty() {
        changed |= apply_pending_taskbar_profile_placements(current, topology, pending);
    } else if current.taskbar_layout_profile(topology).is_some() {
        changed |= complete_taskbar_layout_profile(current, topology);
        changed |= apply_taskbar_layout_profile(current, topology);
        changed |= current.touch_taskbar_layout_profile(topology);
    } else {
        changed |= record_taskbar_layout_profile(current, topology);
    }
    changed
}

fn taskbar_layout_profile_reconcile_needed(
    settings: &Settings,
    topology: &TaskbarTopology,
    pending: &[PendingTaskbarProfilePlacement],
) -> bool {
    if !settings.taskbar_layout_memory_on || topology.is_empty() {
        return false;
    }
    if !pending.is_empty() {
        true
    } else if settings.taskbar_layout_profile(topology).is_some() {
        let mut projected = settings.clone();
        complete_taskbar_layout_profile(&mut projected, topology)
            || apply_taskbar_layout_profile(&mut projected, topology)
            || !settings.taskbar_layout_profile_is_most_recent(topology)
    } else {
        let mut projected = settings.clone();
        projected.inherit_legacy_taskbar_layout_profile(topology)
            || taskbar_layout_profile_from_current(settings, topology).is_some()
    }
}

#[cfg(windows)]
fn emit_taskbar_topology(
    app: &tauri::AppHandle,
    settings: &Settings,
    snapshot: &TaskbarDockSnapshot,
) {
    for tool in TASKBAR_TOOLS {
        let Ok(taskbar) = taskbar_from_snapshot(snapshot, taskbar_monitor_key(settings, tool))
        else {
            continue;
        };
        let orientation =
            taskbar_orientation(taskbar.right - taskbar.left, taskbar.bottom - taskbar.top);
        if let Some(label) = taskbar_bar_label(tool) {
            let _ = app.emit_to(label, "taskbar-topology-updated", orientation);
        }
    }
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
        .find(|taskbar| {
            !preferred_key.is_empty()
                && (taskbar.key == preferred_key
                    || taskbar.device_key == preferred_key
                    || taskbar.legacy_key == preferred_key)
        })
        .or_else(|| snapshot.taskbars.iter().find(|taskbar| taskbar.primary))
        .or_else(|| snapshot.taskbars.first())
        .ok_or_else(|| anyhow::anyhow!("no shell taskbar windows found"))
}

#[cfg(windows)]
fn taskbar_from_snapshot_for_layout<'a>(
    snapshot: &'a TaskbarDockSnapshot,
    preferred_key: &str,
    topology_confirmed: bool,
) -> anyhow::Result<&'a taskbar::ShellTaskbarWindow> {
    let taskbar = taskbar_from_snapshot(snapshot, preferred_key)?;
    // IPC-driven size/menu updates must not bypass the observer's fallback guard.
    anyhow::ensure!(
        taskbar_matches_monitor_key(taskbar, preferred_key) || topology_confirmed,
        "taskbar monitor layout is not stable yet"
    );
    Ok(taskbar)
}

#[cfg(windows)]
fn taskbar_matches_monitor_key(taskbar: &taskbar::ShellTaskbarWindow, key: &str) -> bool {
    key.is_empty() || taskbar.key == key || taskbar.device_key == key || taskbar.legacy_key == key
}

#[cfg(windows)]
fn with_confirmed_taskbar_fallback(
    gate: &Mutex<()>,
    expected: &TaskbarTopology,
    current: impl FnOnce() -> TaskbarTopology,
    apply: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Publication/invalidation also owns this gate; retain it through native commit.
    let _profile_guard = try_taskbar_layout_gate(gate)?;
    anyhow::ensure!(
        expected.is_valid() && current() == *expected,
        "taskbar monitor layout changed while layout was being planned"
    );
    apply()
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
                    !key.is_empty()
                        && (taskbar.key == key
                            || taskbar.device_key == key
                            || taskbar.legacy_key == key)
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
fn bar_overlay_contract_matches(ex_style: isize, style: isize, owner: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    let required_ex =
        WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize | WS_EX_TOPMOST.0 as isize;
    ex_style & required_ex == required_ex
        && style & WS_POPUP.0 as isize != 0
        && style & WS_CHILD.0 as isize == 0
        && owner == 0
}

#[cfg(windows)]
fn apply_taskbar_overlay(
    hwnd: windows::Win32::Foundation::HWND,
    rect: taskbar::DockRect,
) -> anyhow::Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWLP_HWNDPARENT, GWL_EXSTYLE,
        GWL_STYLE, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOOWNERZORDER, SWP_SHOWWINDOW,
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

    unsafe {
        let current_ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let desired_ex_style = bar_overlay_ex_style(current_ex_style);
        let ex_style_changed = current_ex_style != desired_ex_style;
        if ex_style_changed {
            set_window_long_checked(
                hwnd,
                GWL_EXSTYLE,
                desired_ex_style,
                "taskbar ex-style apply",
            )?;
        }

        let current_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let desired_style = bar_overlay_window_style(current_style);
        let style_changed = current_style != desired_style;
        if style_changed {
            set_window_long_checked(hwnd, GWL_STYLE, desired_style, "taskbar style apply")?;
        }

        let current_owner = GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT);
        let owner_changed = current_owner != 0;
        if owner_changed {
            set_window_long_checked(hwnd, GWLP_HWNDPARENT, 0, "taskbar owner detach")?;
        }
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS | SWP_NOOWNERZORDER;
        if ex_style_changed || style_changed || owner_changed {
            flags |= SWP_FRAMECHANGED;
        }
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            flags,
        )?;
        if !bar_overlay_contract_matches(
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
            GetWindowLongPtrW(hwnd, GWL_STYLE),
            GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT),
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
            position_taskbar_bar_on_taskbar(app, tool, rect)?;
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
async fn get_status(app: tauri::AppHandle) -> Result<Vec<AgentStatus>, String> {
    let settings = Settings::try_load().map_err(|err| err.to_string())?;
    collect_representatives_off_thread_with_options(
        settings,
        false,
        false,
        false,
        false,
        Some(app),
    )
    .await;
    current_collection_snapshot().map_err(|err| err.to_string())
}

#[tauri::command]
fn get_collection_health() -> CollectionHealthSnapshot {
    collection_health_snapshot()
}

#[tauri::command]
async fn get_activity(
    window: tauri::Window,
    app: tauri::AppHandle,
) -> Result<activity::ActivitySnapshot, String> {
    ensure_panel_command(window.label())?;
    drop(window);
    let settings = Settings::try_load().map_err(|err| err.to_string())?;
    let snapshot = collect_activity_off_thread(settings.clone(), false).await?;
    if snapshot.local_backfill_pending {
        spawn_activity_refresh(app.clone(), settings.clone(), false);
    }
    spawn_cursor_activity_refresh(app, settings, false);
    Ok(snapshot)
}

#[tauri::command]
async fn refresh_status(
    window: tauri::Window,
    app: tauri::AppHandle,
) -> Result<Vec<AgentStatus>, String> {
    ensure_status_refresh_command(window.label())?;
    let settings = Settings::try_load().map_err(|err| err.to_string())?;
    let activity_settings = settings.clone();
    let (_, collected) = collect_force_refresh_off_thread(settings, Some(app.clone())).await;
    if collected {
        emit_collection_snapshot(&app);
    }
    spawn_activity_refresh(app.clone(), activity_settings.clone(), true);
    spawn_cursor_activity_refresh(app, activity_settings, true);
    current_collection_snapshot().map_err(|err| err.to_string())
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

fn updater_with_timeout(
    app: &tauri::AppHandle,
    timeout_secs: u64,
) -> Result<tauri_plugin_updater::Updater, String> {
    app.updater_builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|_| "update client unavailable".to_string())
}

async fn run_update_check(
    app: &tauri::AppHandle,
    force: bool,
) -> Result<update::UpdateCheckResult, String> {
    if !update::update_check_is_due(force) {
        return Ok(update::cached_result(env!("CARGO_PKG_VERSION")));
    }
    let Ok(_guard) = UPDATE_OPERATION_GATE.try_lock() else {
        return Ok(update::cached_result(env!("CARGO_PKG_VERSION")));
    };
    if !update::update_check_is_due(force) {
        return Ok(update::cached_result(env!("CARGO_PKG_VERSION")));
    }

    let available = updater_with_timeout(app, UPDATE_CHECK_TIMEOUT_SECS)?
        .check()
        .await
        .map_err(|_| "update check failed".to_string())?;
    update::record_update_check(
        env!("CARGO_PKG_VERSION"),
        available
            .as_ref()
            .map(|candidate| candidate.version.as_str()),
    )
    .map_err(|_| "update state save failed".to_string())
}

fn show_update_notification(app: &tauri::AppHandle, result: &update::UpdateCheckResult) {
    if result.status != "update_available" {
        return;
    }
    let Some(version) = result.latest_version.as_deref() else {
        return;
    };
    let Ok(settings) = Settings::try_load() else {
        return;
    };
    if !settings.update_check_on {
        return;
    }
    let Ok(Some(notification)) = update::prepare_notification(version) else {
        return;
    };

    let (title, body) = if notification_uses_korean(&settings.language, system_ui_language()) {
        (
            "Juice 업데이트",
            format!("Juice {version} 버전을 사용할 수 있습니다."),
        )
    } else {
        ("Juice update", format!("Juice {version} is available."))
    };
    match app.notification().builder().title(title).body(body).show() {
        Ok(()) => {
            if let Err(err) = notification.commit() {
                eprintln!("[update] notification commit failed: {err}");
            }
        }
        Err(err) => eprintln!("[update] notification failed: {err}"),
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
        let Ok(settings) = Settings::try_load() else {
            return;
        };
        if !settings.update_check_on {
            return;
        }
        let Ok(result) = run_update_check(&app, false).await else {
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
    let result = run_update_check(&app, true)
        .await
        .unwrap_or_else(|_| update_error_result());
    let _ = app.emit("update-status", &result);
    Ok(result)
}

async fn await_update_download<T, E>(
    download: impl std::future::Future<Output = Result<T, E>>,
    oversize: &tokio::sync::Notify,
    timeout: std::time::Duration,
) -> Result<T, String> {
    tokio::select! {
        result = tokio::time::timeout(timeout, download) => result
            .map_err(|_| "update download timed out".to_string())?
            .map_err(|_| "update download or signature verification failed".to_string()),
        _ = oversize.notified() => Err("update package exceeds the size limit".to_string()),
    }
}

#[tauri::command]
async fn install_update(
    window: tauri::Window,
    app: tauri::AppHandle,
    expected_version: String,
    on_event: tauri::ipc::Channel<UpdateInstallEvent>,
) -> Result<(), String> {
    ensure_panel_command(window.label())?;
    drop(window);
    let expected = update::release_info_for_version(&expected_version)
        .map_err(|_| "invalid expected update version".to_string())?;
    let _guard = UPDATE_OPERATION_GATE
        .try_lock()
        .map_err(|_| "update operation already in progress".to_string())?;
    let updater = updater_with_timeout(&app, UPDATE_DOWNLOAD_TIMEOUT_SECS)?;
    let available = tokio::time::timeout(
        std::time::Duration::from_secs(UPDATE_CHECK_TIMEOUT_SECS),
        updater.check(),
    )
    .await
    .map_err(|_| "update check timed out".to_string())?
    .map_err(|_| "update check failed".to_string())?
    .ok_or_else(|| "update is no longer available".to_string())?;
    let actual = update::release_info_for_version(&available.version)
        .map_err(|_| "invalid available update version".to_string())?;
    if actual.version != expected.version {
        return Err("available update changed; check again".to_string());
    }
    if !update::is_updater_asset_url_allowed(available.download_url.as_str(), &actual.version) {
        return Err("update download URL is not allowed".to_string());
    }

    let _ = on_event.send(UpdateInstallEvent::Started {
        version: actual.version.clone(),
    });
    let downloaded = std::sync::Arc::new(AtomicU64::new(0));
    let progress_downloaded = downloaded.clone();
    let progress_events = on_event.clone();
    let verifying_events = on_event.clone();
    let oversize = std::sync::Arc::new(AtomicBool::new(false));
    let oversize_progress = oversize.clone();
    let oversize_notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let oversize_progress_notify = oversize_notify.clone();
    let download = available.download(
        move |chunk_length, content_length| {
            let total = progress_downloaded
                .fetch_add(chunk_length as u64, Ordering::Relaxed)
                .saturating_add(chunk_length as u64);
            if update::update_package_size_is_allowed(total, content_length) {
                let _ = progress_events.send(UpdateInstallEvent::Progress {
                    downloaded_bytes: total,
                    content_length,
                });
            } else if !oversize_progress.swap(true, Ordering::AcqRel) {
                oversize_progress_notify.notify_one();
            }
        },
        move || {
            let _ = verifying_events.send(UpdateInstallEvent::Verifying);
        },
    );
    let bytes = await_update_download(
        download,
        &oversize_notify,
        std::time::Duration::from_secs(UPDATE_DOWNLOAD_TIMEOUT_SECS),
    )
    .await?;
    if !update::update_package_size_is_allowed(bytes.len() as u64, Some(bytes.len() as u64)) {
        return Err("update package exceeds the size limit".to_string());
    }

    let version = actual.version.clone();
    let installer = tauri::async_runtime::spawn_blocking(move || {
        update::prepare_verified_installer(&bytes, &version)
    })
    .await
    .map_err(|_| "update installer validation task failed".to_string())?
    .map_err(|_| "update installer version validation failed".to_string())?;

    let launch_path = installer.clone();
    let launch_version = actual.version.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = update::spawn_update_helper(&launch_path, &launch_version);
        if result.is_err() {
            let _ = std::fs::remove_file(&launch_path);
        }
        result
    })
    .await
    .map_err(|_| "update helper launch task failed".to_string())?
    .map_err(|error| format!("update helper could not be started: {error}"))?;

    let _ = on_event.send(UpdateInstallEvent::Installing);
    exit_after_update_cleanup(app);
    Ok(())
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
async fn get_settings() -> Result<SettingsSnapshot, String> {
    tauri::async_runtime::spawn_blocking(current_settings_snapshot)
        .await
        .map_err(|err| format!("settings load task failed: {err}"))?
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn get_system_text_scale(app: tauri::AppHandle) -> text_scale::TextScaleSnapshot {
    app.try_state::<text_scale::SystemTextScale>()
        .map(|state| state.snapshot())
        .unwrap_or_default()
}

#[tauri::command]
async fn clear_taskbar_layout_profiles(
    window: tauri::Window,
    app: tauri::AppHandle,
) -> Result<SettingsSnapshot, String> {
    ensure_panel_command(window.label())?;
    drop(window);
    let pending_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _profile_guard = TASKBAR_PROFILE_GATE
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        update_taskbar_settings(|current| {
            current.taskbar_layout_profiles.clear();
            current.taskbar_layout_memory_initialized = true;
        })?;
        clear_all_pending_taskbar_profile_placements(&pending_app);
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|err| format!("taskbar layout reset task failed: {err}"))?
    .map_err(|err| err.to_string())?;
    publish_current_settings(&app).map_err(|err| err.to_string())
}

#[derive(Clone, serde::Serialize)]
struct SettingsSnapshot {
    #[serde(flatten)]
    settings: Settings,
    settings_revision: String,
}

#[derive(Default)]
struct SettingsPublicationState {
    revision: u64,
    generation: u64,
    settings: Option<Settings>,
}

static SETTINGS_PUBLICATION_STATE: Mutex<SettingsPublicationState> =
    Mutex::new(SettingsPublicationState {
        revision: 0,
        generation: 0,
        settings: None,
    });

fn settings_snapshot_in_state(
    state: &mut SettingsPublicationState,
    settings: Settings,
    generation: u64,
) -> anyhow::Result<SettingsSnapshot> {
    if state.generation != generation || state.settings.as_ref() != Some(&settings) {
        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("settings publication revision exhausted"))?;
        state.revision = revision;
        state.generation = generation;
        state.settings = Some(settings.clone());
    }
    Ok(SettingsSnapshot {
        settings,
        settings_revision: state.revision.to_string(),
    })
}

fn with_current_settings_snapshot<T>(
    reader: impl FnOnce(&SettingsSnapshot) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    with_taskbar_settings_read(|generation| {
        let settings = Settings::try_load()?;
        let snapshot = settings_snapshot_in_state(
            &mut SETTINGS_PUBLICATION_STATE
                .lock()
                .unwrap_or_else(|err| err.into_inner()),
            settings,
            generation,
        )?;
        // Keep the settings gate through publication, not through awaited side effects.
        reader(&snapshot)
    })
}

fn current_settings_snapshot() -> anyhow::Result<SettingsSnapshot> {
    with_current_settings_snapshot(|snapshot| Ok(snapshot.clone()))
}

fn publish_current_settings(app: &tauri::AppHandle) -> anyhow::Result<SettingsSnapshot> {
    with_current_settings_snapshot(|snapshot| {
        let _ = app.emit("settings-updated", snapshot);
        Ok(snapshot.clone())
    })
}

#[derive(serde::Serialize)]
struct SaveSettingsResult {
    settings: SettingsSnapshot,
    taskbar_applied: bool,
    autostart_applied: bool,
    warnings: Vec<String>,
}

fn settings_apply_report(
    settings: SettingsSnapshot,
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

fn autostart_setting_changed(current: &Settings, requested: &Settings) -> bool {
    current.autostart_on != requested.autostart_on
}

fn taskbar_targets_match(left: &Settings, right: &Settings) -> bool {
    left.claude_taskbar_offset_ratio == right.claude_taskbar_offset_ratio
        && left.codex_taskbar_offset_ratio == right.codex_taskbar_offset_ratio
        && left.grok_taskbar_offset_ratio == right.grok_taskbar_offset_ratio
        && left.cursor_taskbar_offset_ratio == right.cursor_taskbar_offset_ratio
        && left.antigravity_taskbar_offset_ratio == right.antigravity_taskbar_offset_ratio
        && left.claude_taskbar_monitor_key == right.claude_taskbar_monitor_key
        && left.codex_taskbar_monitor_key == right.codex_taskbar_monitor_key
        && left.grok_taskbar_monitor_key == right.grok_taskbar_monitor_key
        && left.cursor_taskbar_monitor_key == right.cursor_taskbar_monitor_key
        && left.antigravity_taskbar_monitor_key == right.antigravity_taskbar_monitor_key
        && left.claude_taskbar_target_initialized == right.claude_taskbar_target_initialized
        && left.codex_taskbar_target_initialized == right.codex_taskbar_target_initialized
        && left.grok_taskbar_target_initialized == right.grok_taskbar_target_initialized
        && left.cursor_taskbar_target_initialized == right.cursor_taskbar_target_initialized
        && left.antigravity_taskbar_target_initialized
            == right.antigravity_taskbar_target_initialized
}

fn preserve_taskbar_targets(current: &Settings, requested: &mut Settings) {
    preserve_taskbar_positions(current, requested);
    preserve_taskbar_layout_memory(current, requested);
}

fn preserve_taskbar_positions(current: &Settings, requested: &mut Settings) {
    requested.claude_taskbar_offset_ratio = current.claude_taskbar_offset_ratio;
    requested.codex_taskbar_offset_ratio = current.codex_taskbar_offset_ratio;
    requested.grok_taskbar_offset_ratio = current.grok_taskbar_offset_ratio;
    requested.cursor_taskbar_offset_ratio = current.cursor_taskbar_offset_ratio;
    requested.antigravity_taskbar_offset_ratio = current.antigravity_taskbar_offset_ratio;
    requested.claude_taskbar_monitor_key = current.claude_taskbar_monitor_key.clone();
    requested.codex_taskbar_monitor_key = current.codex_taskbar_monitor_key.clone();
    requested.grok_taskbar_monitor_key = current.grok_taskbar_monitor_key.clone();
    requested.cursor_taskbar_monitor_key = current.cursor_taskbar_monitor_key.clone();
    requested.antigravity_taskbar_monitor_key = current.antigravity_taskbar_monitor_key.clone();
    requested.claude_taskbar_target_initialized = current.claude_taskbar_target_initialized;
    requested.codex_taskbar_target_initialized = current.codex_taskbar_target_initialized;
    requested.grok_taskbar_target_initialized = current.grok_taskbar_target_initialized;
    requested.cursor_taskbar_target_initialized = current.cursor_taskbar_target_initialized;
    requested.antigravity_taskbar_target_initialized =
        current.antigravity_taskbar_target_initialized;
}

fn preserve_taskbar_layout_memory(current: &Settings, requested: &mut Settings) {
    requested.taskbar_layout_profiles = current.taskbar_layout_profiles.clone();
    requested.taskbar_layout_memory_initialized = current.taskbar_layout_memory_initialized;
}

fn preserve_concurrent_taskbar_state(
    current: &Settings,
    baseline: &Settings,
    requested: &mut Settings,
    drag_active: bool,
) {
    requested.taskbar_bars_paused = current.taskbar_bars_paused;
    preserve_taskbar_layout_memory(current, requested);
    if drag_active || !taskbar_targets_match(current, baseline) {
        preserve_taskbar_positions(current, requested);
    }
}

fn merge_settings_edits(
    current: &Settings,
    baseline: Option<&Settings>,
    requested: &Settings,
) -> anyhow::Result<Settings> {
    let Some(baseline) = baseline else {
        return Ok(requested.clone());
    };
    fn apply_changes(
        current: &mut serde_json::Value,
        before: &serde_json::Value,
        after: &serde_json::Value,
    ) {
        if before == after {
            return;
        }
        if let (Some(current), Some(before), Some(after)) = (
            current.as_object_mut(),
            before.as_object(),
            after.as_object(),
        ) {
            for key in before.keys().filter(|key| !after.contains_key(*key)) {
                current.remove(key);
            }
            for (key, value) in after {
                apply_changes(
                    current.entry(key).or_insert(serde_json::Value::Null),
                    before.get(key).unwrap_or(&serde_json::Value::Null),
                    value,
                );
            }
        } else {
            *current = after.clone();
        }
    }
    let mut merged = serde_json::to_value(current)?;
    apply_changes(
        &mut merged,
        &serde_json::to_value(baseline)?,
        &serde_json::to_value(requested)?,
    );
    Ok(serde_json::from_value(merged)?)
}

fn validate_settings_edit_topology(
    current: &Settings,
    baseline: Option<&Settings>,
    requested: &Settings,
    edit_topology: Option<&TaskbarTopology>,
    current_topology: &TaskbarTopology,
) -> anyhow::Result<()> {
    let (Some(baseline), Some(edit_topology)) = (baseline, edit_topology) else {
        return Ok(());
    };
    let presentation_changed = (current.taskbar_profile_presentation_on
        || requested.taskbar_profile_presentation_on)
        && TaskbarPresentationProfile::from_settings(baseline)
            != TaskbarPresentationProfile::from_settings(requested);
    let colors_changed = (current.taskbar_profile_colors_on || requested.taskbar_profile_colors_on)
        && TaskbarAppearanceProfile::from_settings(baseline)
            != TaskbarAppearanceProfile::from_settings(requested);
    if (current.taskbar_layout_memory_on || requested.taskbar_layout_memory_on)
        && (presentation_changed || colors_changed)
        && edit_topology.clone().canonicalized() != current_topology.clone().canonicalized()
    {
        anyhow::bail!("monitor layout changed; settings reloaded, please retry the edit");
    }
    Ok(())
}

fn retry_settings_side_effects(app: tauri::AppHandle, retry_taskbar: bool, retry_autostart: bool) {
    if !retry_taskbar && !retry_autostart {
        return;
    }
    let Some(state) = app.try_state::<SettingsSideEffectRetryState>() else {
        return;
    };
    if retry_taskbar {
        state.taskbar_requests.fetch_add(1, Ordering::AcqRel);
        state.taskbar_pending.store(true, Ordering::Release);
    }
    if retry_autostart {
        state.autostart_requests.fetch_add(1, Ordering::AcqRel);
        state.autostart_pending.store(true, Ordering::Release);
    }
    if state.running.swap(true, Ordering::AcqRel) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let mut last_taskbar_request = None;
        let mut last_autostart_request = None;
        for delay in [500, 1_500, 3_000] {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            let Some(state) = app.try_state::<SettingsSideEffectRetryState>() else {
                return;
            };
            let retry_taskbar = state.taskbar_pending.swap(false, Ordering::AcqRel);
            let retry_autostart = state.autostart_pending.swap(false, Ordering::AcqRel);
            if retry_taskbar {
                last_taskbar_request = Some(state.taskbar_requests.load(Ordering::Acquire));
            }
            if retry_autostart {
                last_autostart_request = Some(state.autostart_requests.load(Ordering::Acquire));
            }
            if !retry_taskbar && !retry_autostart {
                break;
            }
            let (latest, generation) = match load_settings_with_generation() {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    eprintln!("[settings] side-effect retry skipped; settings unavailable: {err}");
                    state
                        .taskbar_pending
                        .fetch_or(retry_taskbar, Ordering::AcqRel);
                    state
                        .autostart_pending
                        .fetch_or(retry_autostart, Ordering::AcqRel);
                    continue;
                }
            };
            if retry_taskbar
                && (taskbar_drag_active(&app)
                    || apply_taskbar_dock_for_generation(&app, &latest, generation).is_err())
            {
                state.taskbar_pending.store(true, Ordering::Release);
            }
            if retry_autostart
                && (TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != generation
                    || apply_autostart_for_release(&app, &latest).is_err())
            {
                state.autostart_pending.store(true, Ordering::Release);
            }
            if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != generation {
                state
                    .taskbar_pending
                    .fetch_or(retry_taskbar, Ordering::AcqRel);
                state
                    .autostart_pending
                    .fetch_or(retry_autostart, Ordering::AcqRel);
            }
        }
        if let Some(state) = app.try_state::<SettingsSideEffectRetryState>() {
            let exhausted_taskbar = clear_exhausted_retry(
                &state.taskbar_pending,
                &state.taskbar_requests,
                last_taskbar_request,
            );
            let exhausted_autostart = clear_exhausted_retry(
                &state.autostart_pending,
                &state.autostart_requests,
                last_autostart_request,
            );
            state.running.store(false, Ordering::Release);
            if exhausted_taskbar || exhausted_autostart {
                eprintln!(
                    "[settings] side-effect reconciliation exhausted: taskbar={exhausted_taskbar}, autostart={exhausted_autostart}"
                );
            }
            let newly_pending_taskbar = state.taskbar_pending.load(Ordering::Acquire);
            let newly_pending_autostart = state.autostart_pending.load(Ordering::Acquire);
            if newly_pending_taskbar || newly_pending_autostart {
                retry_settings_side_effects(
                    app.clone(),
                    newly_pending_taskbar,
                    newly_pending_autostart,
                );
            }
        }
    });
}

fn clear_exhausted_retry(
    pending: &AtomicBool,
    requests: &AtomicU64,
    attempted_request: Option<u64>,
) -> bool {
    let Some(attempted_request) = attempted_request else {
        return false;
    };
    if !pending.load(Ordering::Acquire) || requests.load(Ordering::Acquire) != attempted_request {
        return false;
    }
    pending.store(false, Ordering::Release);
    if requests.load(Ordering::Acquire) != attempted_request {
        pending.store(true, Ordering::Release);
        return false;
    }
    true
}

#[tauri::command]
async fn save_settings(
    window: tauri::Window,
    app: tauri::AppHandle,
    input: config::SettingsInput,
    edit_baseline: Option<config::SettingsInput>,
    edit_topology: Option<TaskbarTopology>,
) -> Result<SaveSettingsResult, String> {
    ensure_panel_command(window.label())?;
    drop(window);
    let edit_request = Settings::from_input(input);
    let edit_baseline = edit_baseline.map(Settings::from_input);
    let baseline = tauri::async_runtime::spawn_blocking(Settings::try_load)
        .await
        .map_err(|err| format!("settings load task failed: {err}"))?
        .map_err(|err| err.to_string())?;
    let mut requested = merge_settings_edits(&baseline, edit_baseline.as_ref(), &edit_request)
        .map_err(|err| err.to_string())?;
    let previous_language = baseline.language.clone();
    let drag_was_active = taskbar_drag_active(&app);
    preserve_taskbar_targets(&baseline, &mut requested);
    #[cfg(windows)]
    if !drag_was_active {
        preserve_taskbar_leading_edges(&app, &baseline, &mut requested);
    }
    let update_app = app.clone();
    let save_result = tauri::async_runtime::spawn_blocking(move || {
        // Read Shell outside the profile/settings locks. A display change may have
        // happened before the background observer cleared its stable snapshot.
        #[cfg(windows)]
        let observed_topology = taskbar::shell_taskbar_windows()
            .map(|taskbars| taskbar_profile_topology(&taskbars))
            .unwrap_or_default();
        let _profile_guard = TASKBAR_PROFILE_GATE
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let profile_topology = stable_taskbar_topology(&update_app);
        #[cfg(windows)]
        let profile_topology = if profile_topology == observed_topology {
            profile_topology
        } else {
            TaskbarTopology::default()
        };
        let mut autostart_changed = false;
        let snapshot = try_update_taskbar_settings(|current| {
            validate_settings_edit_topology(
                current,
                edit_baseline.as_ref(),
                &edit_request,
                edit_topology.as_ref(),
                &profile_topology,
            )?;
            let mut merged = merge_settings_edits(current, edit_baseline.as_ref(), &edit_request)?;
            preserve_taskbar_positions(&requested, &mut merged);
            requested = merged;
            autostart_changed = autostart_setting_changed(current, &requested);
            preserve_concurrent_taskbar_state(
                current,
                &baseline,
                &mut requested,
                drag_was_active || taskbar_drag_active(&update_app),
            );
            *current = requested;
            if !drag_was_active && !profile_topology.is_empty() {
                record_taskbar_layout_profile(current, &profile_topology);
            }
            Ok(())
        })?;
        Ok::<_, anyhow::Error>((snapshot, autostart_changed))
    })
    .await;
    let save_result = match save_result {
        Ok(result) => result.map_err(|err| err.to_string()),
        Err(err) => Err(format!("settings save task failed: {err}")),
    };
    let (snapshot, autostart_changed) = save_result?;
    let TaskbarSettingsSnapshot {
        settings,
        generation,
        collection_changes,
        ..
    } = snapshot;
    let claude_enabled_now = collection_changes.enabled_now(&Tool::Claude);
    let codex_enabled_now = collection_changes.enabled_now(&Tool::Codex);
    let grok_enabled_now = collection_changes.enabled_now(&Tool::Grok);
    let cursor_enabled_now = collection_changes.enabled_now(&Tool::Cursor);
    let antigravity_enabled_now = collection_changes.enabled_now(&Tool::Antigravity);
    if previous_language != settings.language {
        if let Err(error) = refresh_tray_menu(&app, &settings.language) {
            eprintln!("[tray] menu language update failed: {error}");
        }
    }
    if collection_changes.transition(&Tool::Codex).is_some() {
        if let Err(error) = reconcile_broker_policy_off_thread(Tool::Codex).await {
            eprintln!("[codex] app-server broker state update failed: {error}");
        }
    }
    if collection_changes.transition(&Tool::Grok).is_some() {
        if let Err(error) = reconcile_broker_policy_off_thread(Tool::Grok).await {
            eprintln!("[grok] ACP broker state update failed: {error}");
        }
    }
    if collection_changes.transition(&Tool::Antigravity).is_some()
        && reconcile_antigravity_cli_off_thread().await.is_err()
    {
        eprintln!(
            "[antigravity] CLI connection reconciliation failed; GUI collection remains available"
        );
    }
    if recover_pending_collection_policies_off_thread()
        .await
        .is_err()
    {
        eprintln!("[collector] pending collection policy recovery failed");
    }
    if !taskbar_drag_active(&app) {
        let _ = with_taskbar_settings_read(|current_generation| {
            anyhow::ensure!(
                current_generation == generation,
                "taskbar settings changed before ratio synchronization"
            );
            sync_taskbar_content_layout_ratios(&app, &settings);
            Ok(())
        });
    }
    let taskbar_result = if taskbar_drag_active(&app) {
        Ok(())
    } else {
        apply_taskbar_dock_for_generation(&app, &settings, generation)
            .map_err(|err| err.to_string())
    };
    let autostart_result = if autostart_changed {
        let result = if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != generation {
            Err("autostart settings changed while applying".into())
        } else {
            apply_autostart_for_release(&app, &settings).map_err(|err| err.to_string())
        };
        if result.is_ok() && TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != generation {
            Err("autostart settings changed while applying".into())
        } else {
            result
        }
    } else {
        Ok(())
    };
    if collection_changes.any() {
        emit_collection_snapshot(&app);
        if claude_enabled_now
            || codex_enabled_now
            || grok_enabled_now
            || cursor_enabled_now
            || antigravity_enabled_now
        {
            let refresh_app = app.clone();
            let refresh_settings = settings.clone();
            tauri::async_runtime::spawn(async move {
                let force_claude =
                    claude_enabled_now && refresh_settings.claude_account_auto_collect_on;
                let _ = collect_representatives_off_thread_with_options(
                    refresh_settings,
                    codex_enabled_now,
                    force_claude,
                    grok_enabled_now,
                    cursor_enabled_now,
                    Some(refresh_app.clone()),
                )
                .await;
                emit_collection_snapshot(&refresh_app);
            });
            if claude_enabled_now || codex_enabled_now || grok_enabled_now {
                spawn_activity_refresh(app.clone(), settings.clone(), codex_enabled_now);
            }
            if cursor_enabled_now {
                spawn_cursor_activity_refresh(app.clone(), settings.clone(), true);
            }
        }
    }
    if collection_changes.cursor_activity_range_changed {
        spawn_cursor_activity_refresh(app.clone(), settings.clone(), false);
    }
    let published = publish_current_settings(&app).map_err(|err| err.to_string())?;
    let report = settings_apply_report(published, taskbar_result, autostart_result);
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
        let taskbar_rect = taskbar::DockRect {
            x: taskbar.left,
            y: taskbar.top,
            width: taskbar.right - taskbar.left,
            height: taskbar.bottom - taskbar.top,
        };
        let current_ratio = taskbar_content_layout(app, current, tool)
            .and_then(|layout| layout.ratio)
            .unwrap_or_else(|| taskbar_offset_ratio(current, tool));
        let current_length = taskbar_physical_length_for_window(current_length, taskbar.hwnd);
        let requested_length = taskbar_physical_length_for_window(requested_length, taskbar.hwnd);
        if let Some(ratio) = taskbar_ratio_preserving_layout_leading_edge(
            taskbar_rect,
            current_length,
            current_ratio,
            requested_length,
        ) {
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
    ensure_matching_bar_command(window.label(), &tool)?;

    #[cfg(windows)]
    {
        let (mut settings, mut settings_generation) =
            load_settings_with_generation().map_err(|err| err.to_string())?;
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
        position_taskbar_bar_on_taskbar(&app, tool, rect).map_err(|err| err.to_string())?;
        if persist {
            let snapshot = update_taskbar_settings(|current| {
                set_taskbar_target(current, tool, &taskbar.key, ratio);
            })
            .map_err(|err| err.to_string())?;
            settings = snapshot.settings;
            settings_generation = snapshot.generation;
            let _ = publish_current_settings(&app);
        } else {
            set_taskbar_target(&mut settings, tool, &taskbar.key, ratio);
        }
        set_taskbar_content_layout_ratio_for_generation(&app, tool, ratio, settings_generation)
            .map_err(|err| err.to_string())?;
        Ok(settings)
    }

    #[cfg(not(windows))]
    {
        let _ = (app, tool, screen_x, grab_offset_x, persist);
        Err("taskbar dock is Windows-only".into())
    }
}

#[tauri::command]
async fn pause_taskbar_bars(window: tauri::Window, app: tauri::AppHandle) -> Result<(), String> {
    ensure_taskbar_bar_command(window.label())?;
    drop(window);
    tauri::async_runtime::spawn_blocking(move || pause_taskbar_bars_for_manager(&app))
        .await
        .map_err(|err| format!("taskbar pause task failed: {err}"))?
        .map_err(|err| err.to_string())
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
        Some("grok") if label == "bar-grok" => Ok(()),
        Some("cursor") if label == "bar-cursor" => Ok(()),
        Some("antigravity") if label == "bar-antigravity" => Ok(()),
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
async fn get_taskbar_orientation(window: tauri::Window, tool: String) -> Result<String, String> {
    ensure_matching_bar_command(window.label(), &tool)?;

    #[cfg(windows)]
    {
        let settings = Settings::try_load().map_err(|err| err.to_string())?;
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
async fn set_taskbar_content_width(
    window: tauri::Window,
    app: tauri::AppHandle,
    tool: String,
    width: f64,
    mode: Option<String>,
    device_pixel_ratio: Option<f64>,
) -> Result<bool, String> {
    ensure_matching_bar_command(window.label(), &tool)?;
    if !width.is_finite() || !(1.0..=2304.0).contains(&width) {
        return Err("taskbar content width is out of range".into());
    }

    let tool = normalize_taskbar_tool(&tool).ok_or_else(|| "unknown taskbar tool".to_string())?;
    let (settings, settings_generation) =
        load_settings_with_generation().map_err(|err| err.to_string())?;
    if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != settings_generation {
        return Err("taskbar settings changed before content layout update".into());
    }
    if mode.as_ref().is_some_and(|mode| mode != &settings.bar_mode) {
        return Err("taskbar mode changed while measuring".into());
    }
    let target_initialized = taskbar_target_initialized(&settings, tool);
    #[cfg(windows)]
    let target_scale = {
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        let taskbar = taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool))
            .map_err(|err| err.to_string())?;
        // Dock geometry uses the shell window's DPI; WebView CSS pixels may use a different scale.
        (unsafe { GetDpiForWindow(taskbar.hwnd) }).max(96) as f64 / 96.0
    };
    #[cfg(not(windows))]
    let target_scale = 1.0;
    let width = taskbar_measured_logical_length(width, device_pixel_ratio, target_scale)?;
    let (layout_revision, previous) = taskbar_content_layout_snapshot(&app, &settings, tool)
        .ok_or_else(|| "taskbar content snapshot is unavailable".to_string())?;
    let layout_matches = previous
        .as_ref()
        .is_some_and(|layout| layout.mode == settings.bar_mode && layout.width == width);
    #[cfg(windows)]
    let window_matches = (|| {
        let taskbar =
            taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool)).ok()?;
        let rect = current_bar_rect(&app, tool).ok()?;
        let actual =
            if taskbar_orientation(taskbar.right - taskbar.left, taskbar.bottom - taskbar.top)
                == "vertical"
            {
                rect.bottom - rect.top
            } else {
                rect.right - rect.left
            };
        let expected = taskbar_physical_length_for_window(width, taskbar.hwnd);
        Some(actual >= expected && actual <= expected + 1)
    })()
    .unwrap_or(false);
    #[cfg(not(windows))]
    let window_matches = true;
    match taskbar_content_width_decision(target_initialized, layout_matches, window_matches) {
        TaskbarContentWidthDecision::RetryAfterTarget => {
            return Err("taskbar target is not initialized".into())
        }
        TaskbarContentWidthDecision::AlreadyApplied => {
            acknowledge_taskbar_content_layout(&app, tool, layout_revision)
                .map_err(|err| err.to_string())?;
            return Ok(false);
        }
        TaskbarContentWidthDecision::Apply => {}
    }

    #[cfg(windows)]
    let ratio = (|| {
        let taskbar =
            taskbar::shell_taskbar_window_for_key(taskbar_monitor_key(&settings, tool)).ok()?;
        let taskbar_rect = taskbar::DockRect {
            x: taskbar.left,
            y: taskbar.top,
            width: taskbar.right - taskbar.left,
            height: taskbar.bottom - taskbar.top,
        };
        let current_ratio = previous
            .as_ref()
            .and_then(|layout| layout.ratio)
            .unwrap_or_else(|| taskbar_offset_ratio(&settings, tool));
        let current_length = previous
            .as_ref()
            .map(|layout| layout.width)
            .or_else(|| taskbar_dock_width(&settings, tool))?;
        let current_length = taskbar_physical_length_for_window(current_length, taskbar.hwnd);
        let new_length = taskbar_physical_length_for_window(width, taskbar.hwnd);
        taskbar_ratio_preserving_layout_leading_edge(
            taskbar_rect,
            current_length,
            current_ratio,
            new_length,
        )
    })()
    .or_else(|| Some(taskbar_offset_ratio(&settings, tool)));
    #[cfg(not(windows))]
    let ratio = Some(taskbar_offset_ratio(&settings, tool));

    let next = TaskbarContentLayout {
        mode: settings.bar_mode.clone(),
        width,
        ratio,
    };
    let write = begin_taskbar_content_layout(&app, tool, layout_revision, next)
        .map_err(|err| err.to_string())?;
    if let Err(err) = apply_taskbar_dock_for_generation(&app, &settings, settings_generation) {
        rollback_taskbar_content_layout(&app, tool, write);
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
async fn set_taskbar_menu_open(
    window: tauri::Window,
    app: tauri::AppHandle,
    tool: String,
    open: bool,
) -> Result<(), String> {
    ensure_matching_bar_command(window.label(), &tool)?;
    let (settings, settings_generation) =
        load_settings_with_generation().map_err(|err| err.to_string())?;
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
    let write =
        set_taskbar_menu_layout_for_generation(&app, &tool, open, menu_ratio, settings_generation)
            .map_err(|err| err.to_string())?;
    if let Err(err) = apply_taskbar_dock_for_generation(&app, &settings, settings_generation) {
        close_taskbar_menu_layout_if_current(&app, &tool, write);
        return Err(err.to_string());
    }
    Ok(())
}

fn apply_taskbar_dock_for_generation<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    expected_generation: u64,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != expected_generation {
            anyhow::bail!("taskbar settings changed before layout planning");
        }
        let snapshot = taskbar_dock_snapshot(manager, settings)?;
        apply_taskbar_dock_with_snapshot(manager, settings, &snapshot, expected_generation)
    }

    #[cfg(not(windows))]
    {
        let _ = (manager, settings, expected_generation);
        Ok(())
    }
}

#[cfg(all(windows, test))]
fn resolve_taskbar_position_pair(
    enabled: bool,
    first_taskbar: isize,
    first_taskbar_rect: taskbar::DockRect,
    first_rect: taskbar::DockRect,
    second_taskbar: isize,
    second_rect: taskbar::DockRect,
) -> (taskbar::DockRect, taskbar::DockRect) {
    if !enabled || first_taskbar != second_taskbar {
        return (first_rect, second_rect);
    }
    taskbar::resolve_taskbar_pair_overlap(first_taskbar_rect, first_rect, second_rect)
}

#[cfg(windows)]
fn apply_taskbar_dock_with_snapshot<R: tauri::Runtime>(
    manager: &impl tauri::Manager<R>,
    settings: &Settings,
    snapshot: &TaskbarDockSnapshot,
    expected_generation: u64,
) -> anyhow::Result<()> {
    #[derive(Clone, Copy)]
    enum Action {
        Hide(&'static str, TaskbarWindowHandle),
        Position {
            tool: &'static str,
            handle: TaskbarWindowHandle,
            taskbar_hwnd: isize,
            taskbar_rect: taskbar::DockRect,
            rect: taskbar::DockRect,
        },
    }

    let content_revisions = TASKBAR_TOOLS
        .iter()
        .map(|tool| (*tool, taskbar_content_layout_revision(manager, tool)))
        .collect::<Vec<_>>();
    let taskbar_paused = taskbar_bars_paused(manager);
    let topology_confirmed =
        stable_taskbar_topology_matches(manager, &taskbar_profile_topology_keys(snapshot));
    let mut actions = Vec::with_capacity(TASKBAR_TOOLS.len());
    let mut menu_cleanup = Vec::new();
    let mut queue_menu_cleanup = |tool| {
        if let Some((revision, _)) = taskbar_menu_layout_snapshot(manager, tool) {
            menu_cleanup.push((tool, TaskbarMenuWrite { revision }));
        }
    };
    let mut requires_confirmed_topology = false;
    for tool in TASKBAR_TOOLS {
        let window_handle = taskbar_window_handle(manager, tool);
        let width = match taskbar_dock_width_for_manager(manager, settings, tool) {
            Some(width) => taskbar_width_with_menu(width, taskbar_menu_is_open(manager, tool)),
            None => {
                queue_menu_cleanup(tool);
                if let Some(handle) = window_handle {
                    actions.push(Action::Hide(tool, handle));
                }
                continue;
            }
        };
        if !taskbar_target_initialized(settings, tool) {
            queue_menu_cleanup(tool);
            if let Some(handle) = window_handle {
                actions.push(Action::Hide(tool, handle));
            }
            continue;
        }
        let taskbar = taskbar_from_snapshot_for_layout(
            snapshot,
            taskbar_monitor_key(settings, tool),
            topology_confirmed,
        )?;
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
            queue_menu_cleanup(tool);
            if let Some(handle) = window_handle {
                actions.push(Action::Hide(tool, handle));
            }
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
        let handle =
            window_handle.ok_or_else(|| anyhow::anyhow!("no taskbar bar window for {tool}"))?;
        requires_confirmed_topology |=
            !taskbar_matches_monitor_key(taskbar, taskbar_monitor_key(settings, tool));
        actions.push(Action::Position {
            tool,
            handle,
            taskbar_hwnd: taskbar.hwnd.0 as isize,
            taskbar_rect: target.rect,
            rect,
        });
    }

    let mut positions = actions
        .iter()
        .enumerate()
        .filter_map(|(index, action)| match action {
            Action::Position {
                taskbar_hwnd,
                taskbar_rect,
                rect,
                ..
            } => Some((index, *taskbar_hwnd, *taskbar_rect, *rect)),
            Action::Hide(_, _) => None,
        })
        .collect::<Vec<_>>();
    if settings.taskbar_avoid_overlap_on {
        let taskbars = positions
            .iter()
            .map(|position| position.1)
            .collect::<std::collections::BTreeSet<_>>();
        for taskbar_hwnd in taskbars {
            let indices = positions
                .iter()
                .enumerate()
                .filter_map(|(index, position)| (position.1 == taskbar_hwnd).then_some(index))
                .collect::<Vec<_>>();
            if indices.len() < 2 {
                continue;
            }
            let taskbar_rect = positions[indices[0]].2;
            let mut rects = indices
                .iter()
                .map(|index| positions[*index].3)
                .collect::<Vec<_>>();
            taskbar::resolve_taskbar_overlaps(taskbar_rect, &mut rects);
            for (position_index, rect) in indices.into_iter().zip(rects) {
                positions[position_index].3 = rect;
            }
        }
        for (action_index, _, _, rect) in positions {
            if let Action::Position {
                rect: action_rect, ..
            } = &mut actions[action_index]
            {
                *action_rect = rect;
            }
        }
    }

    let _layout_guard = try_taskbar_layout_gate(&TASKBAR_LAYOUT_GATE)?;
    let commit = || {
        if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != expected_generation {
            anyhow::bail!("taskbar settings changed while layout was being planned");
        }
        validate_taskbar_content_layout_plan(&content_revisions, |tool| {
            taskbar_content_layout_revision(manager, tool)
        })?;
        for action in actions {
            let (tool, handle) = match action {
                Action::Hide(tool, handle) => (tool, handle),
                Action::Position { tool, handle, .. } => (tool, handle),
            };
            if taskbar_window_handle(manager, tool) != Some(handle) {
                anyhow::bail!("taskbar window changed while layout was being planned");
            }
            let hwnd = windows::Win32::Foundation::HWND(handle.raw as *mut core::ffi::c_void);
            if !taskbar::window_is_valid(hwnd) {
                anyhow::bail!("taskbar window was destroyed while layout was being planned");
            }
            match action {
                Action::Hide(_, _) => taskbar::hide_window(hwnd)?,
                Action::Position { rect, .. } => apply_taskbar_overlay(hwnd, rect)?,
            }
        }
        for (tool, write) in menu_cleanup {
            close_taskbar_menu_layout_if_current(manager, tool, write);
        }
        Ok(())
    };
    if requires_confirmed_topology {
        with_confirmed_taskbar_fallback(
            &TASKBAR_PROFILE_GATE,
            &taskbar_profile_topology_keys(snapshot),
            || stable_taskbar_topology(manager),
            commit,
        )
    } else {
        commit()
    }
}

#[cfg(windows)]
fn taskbar_dock_signature(
    app: &tauri::AppHandle,
    settings: &Settings,
    snapshot: &TaskbarDockSnapshot,
) -> anyhow::Result<(String, bool)> {
    let mut signature = serde_json::to_string(settings)?;
    let mut requires_reapply = false;
    let taskbar_paused = taskbar_bars_paused(app);
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
        let width = taskbar_dock_width_for_manager(app, settings, tool);
        let expected_visible = width.is_some()
            && taskbar_target_initialized(settings, tool)
            && should_show_taskbar_bar_with_pause(
                settings,
                tool,
                window_state.0,
                window_state.1,
                taskbar_paused,
            );
        let visible = taskbar_bar_window_is_visible(app, tool);
        let covered = visible && taskbar_bar_window_is_covered(app, tool);
        let overlay_contract = taskbar_bar_window_overlay_contract_matches(app, tool);
        requires_reapply |= taskbar_observation_requires_reapply(
            expected_visible,
            visible,
            covered,
            overlay_contract,
        );
        signature.push_str(&format!(
            "|{tool}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            taskbar.hwnd.0 as isize,
            taskbar.left,
            taskbar.top,
            taskbar.right,
            taskbar.bottom,
            width.unwrap_or_default(),
            taskbar_layout_ratio(app, settings, tool),
            window_state.0,
            window_state.1,
            taskbar_menu_is_open(app, tool),
            taskbar_bar_window_is_alive(app, tool),
            visible,
            covered,
            overlay_contract,
        ));
    }
    Ok((signature, requires_reapply))
}

#[cfg(windows)]
fn migrate_legacy_taskbar_monitor_keys(
    settings: &Settings,
    snapshot: &TaskbarDockSnapshot,
) -> anyhow::Result<bool> {
    let mut replacements = Vec::new();
    for taskbar in &snapshot.taskbars {
        for alias in [&taskbar.device_key, &taskbar.legacy_key] {
            if alias != &taskbar.key
                && !replacements
                    .iter()
                    .any(|(saved, _): &(String, String)| saved == alias)
            {
                replacements.push((alias.clone(), taskbar.key.clone()));
            }
        }
    }
    if let Some(primary) = snapshot.taskbars.iter().find(|taskbar| taskbar.primary) {
        replacements.push((String::new(), primary.key.clone()));
    }

    let mut projected = settings.clone();
    if !projected.migrate_taskbar_monitor_key_aliases(&replacements) {
        return Ok(false);
    }

    let mut changed = false;
    update_taskbar_settings(|current| {
        changed = current.migrate_taskbar_monitor_key_aliases(&replacements);
    })?;
    if !changed {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(windows)]
fn save_taskbar_drag_target(
    app: &tauri::AppHandle,
    tool: &str,
    monitor_key: &str,
    dropped_rect: taskbar::DockRect,
) -> anyhow::Result<()> {
    let tool =
        normalize_taskbar_tool(tool).ok_or_else(|| anyhow::anyhow!("unknown taskbar tool"))?;
    let current = Settings::try_load()?;
    let taskbars = taskbar::shell_taskbar_windows()?;
    let taskbar = taskbars
        .iter()
        .find(|taskbar| {
            !monitor_key.is_empty()
                && (taskbar.key == monitor_key
                    || taskbar.device_key == monitor_key
                    || taskbar.legacy_key == monitor_key)
        })
        .or_else(|| taskbars.iter().find(|taskbar| taskbar.primary))
        .or_else(|| taskbars.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no shell taskbar windows found"))?;
    let topology = taskbar_profile_topology(&taskbars);
    let logical_length = taskbar_dock_width_for_manager(app, &current, tool)
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
    let stable_key = taskbar.key.clone();
    let _profile_guard = TASKBAR_PROFILE_GATE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let profile_topology_is_stable = stable_taskbar_topology_matches(app, &topology);
    let snapshot = update_taskbar_settings(|current| {
        set_taskbar_target(current, tool, &stable_key, ratio);
        if profile_topology_is_stable {
            record_taskbar_layout_profile(current, &topology);
        }
    })?;
    let mut settings = snapshot.settings;
    let mut settings_generation = snapshot.generation;
    if settings.taskbar_layout_memory_on && !profile_topology_is_stable {
        let pending = PendingTaskbarProfilePlacement {
            topology: topology.clone(),
            tool,
            placement: TaskbarPlacement {
                monitor_key: stable_key,
                offset_ratio: ratio,
            },
        };
        if !remember_pending_taskbar_profile_placement(app, pending.clone()) {
            let committed = update_taskbar_settings(|current| {
                apply_pending_taskbar_profile_placements(
                    current,
                    &topology,
                    std::slice::from_ref(&pending),
                );
            })?;
            settings = committed.settings;
            settings_generation = committed.generation;
        }
    }
    drop(_profile_guard);
    set_taskbar_content_layout_ratio_for_generation(
        app,
        tool,
        taskbar_offset_ratio(&settings, tool),
        settings_generation,
    )?;
    let _ = publish_current_settings(app);
    apply_taskbar_dock_for_generation(app, &settings, settings_generation)?;
    Ok(())
}

#[cfg(windows)]
fn current_bar_rect(
    app: &tauri::AppHandle,
    tool: &str,
) -> anyhow::Result<windows::Win32::Foundation::RECT> {
    use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};

    let hwnd = taskbar_window_hwnd(app, tool)
        .ok_or_else(|| anyhow::anyhow!("no taskbar bar window for {tool}"))?;
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
    let settings = Settings::try_load().ok()?;
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
    let window_visible = taskbar_window_hwnd(app, tool).is_some_and(taskbar::window_is_visible);
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
        let Some(hwnd) = taskbar_window_hwnd(app, tool) else {
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
        let visible = taskbar_window_hwnd(app, tool).is_some_and(taskbar::window_is_visible);
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
    let hwnd = taskbar_window_hwnd(app, tool)
        .ok_or_else(|| anyhow::anyhow!("no taskbar bar window for {tool}"))?;
    taskbar::show_window_tooltip(hwnd, visible)
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
            let hwnd = taskbar_window_hwnd(app, tool)?;
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
                let forced_hover_candidate = std::env::var("AGENT_JUICE_TEST_HOVER_TOOL").ok();
                let forced_hover = isolated_forced_taskbar_hover(
                    std::env::var_os("AGENT_JUICE_DATA_DIR").is_some(),
                    forced_hover_candidate.as_deref(),
                );

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
                        if forced_hover.is_some() {
                            forced_hover
                        } else if down {
                            None
                        } else {
                            point
                                .and_then(|point| current_bar_at_point(&app, point))
                                .filter(|tool| !taskbar_menu_is_open(&app, tool))
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
                            drag_monitor_key = drag_tool.and_then(|tool| {
                                Settings::try_load().ok().map(|settings| {
                                    taskbar_monitor_key(&settings, tool).to_string()
                                })
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
                                if position_taskbar_bar_on_taskbar(&app, tool, rect).is_ok() {
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
        std::thread::spawn(move || {
            let mut last_signature: Option<String> = None;
            let mut last_topology_signature: Option<String> = None;
            let mut topology_stability = TaskbarTopologyStability::default();
            let mut settings = Settings::default();
            let mut settings_revision = None;
            let mut settings_generation = TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire);
            let mut settings_valid = false;
            let mut settings_event_pending = false;
            let mut profile_ratio_sync_pending = false;
            loop {
                if app
                    .try_state::<TaskbarShutdownState>()
                    .is_some_and(|state| state.0.load(Ordering::Acquire))
                {
                    break;
                }
                if !left_mouse_button_down() {
                    if TASKBAR_TOOLS
                        .iter()
                        .any(|tool| !taskbar_bar_window_is_alive(&app, tool))
                    {
                        last_signature = None;
                        request_taskbar_bar_recovery(&app);
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    let next_revision = Settings::storage_revision();
                    if !settings_valid || next_revision != settings_revision {
                        let reload_result = with_taskbar_settings_read(|load_generation| {
                            let (loaded, revision) = Settings::try_load_with_revision()?;
                            if profile_ratio_sync_pending {
                                sync_taskbar_content_layout_ratios(&app, &loaded);
                            }
                            if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire)
                                != load_generation
                            {
                                anyhow::bail!("settings changed while synchronizing taskbar state");
                            }
                            Ok((loaded, revision, load_generation))
                        });
                        match reload_result {
                            Ok((loaded, revision, load_generation)) => {
                                settings = loaded;
                                settings_revision = revision;
                                settings_generation = load_generation;
                                settings_valid = true;
                                profile_ratio_sync_pending = false;
                                if settings_event_pending {
                                    settings_event_pending =
                                        publish_current_settings(&app).is_err();
                                    if settings_event_pending {
                                        settings_valid = false;
                                    }
                                }
                                topology_stability.rearm();
                            }
                            Err(err) => {
                                settings_valid = false;
                                last_signature = None;
                                hide_all_taskbar_bars(&app);
                                eprintln!("[taskbar] settings unavailable; bars hidden: {err}");
                                std::thread::sleep(std::time::Duration::from_millis(500));
                                continue;
                            }
                        }
                    }
                    if taskbar_targets_need_initialization(&settings) {
                        match initialize_pending_taskbar_targets(&app, &settings) {
                            Ok(initialized) => {
                                settings = initialized.settings;
                                settings_revision = initialized.revision;
                                settings_generation = initialized.generation;
                                last_signature = None;
                                topology_stability.rearm();
                            }
                            Err(err) => {
                                last_signature = None;
                                eprintln!("[taskbar] pending target initialization failed: {err}");
                                std::thread::sleep(std::time::Duration::from_millis(500));
                                continue;
                            }
                        }
                    }
                    if TASKBAR_SETTINGS_GENERATION.load(Ordering::Acquire) != settings_generation {
                        settings_valid = false;
                        last_signature = None;
                        continue;
                    }
                    let snapshot = match taskbar_dock_snapshot(&app, &settings) {
                        Ok(snapshot) => snapshot,
                        Err(err) => {
                            last_signature = None;
                            topology_stability.observe(TaskbarTopology::default());
                            eprintln!("[taskbar] inspect dock state failed: {err}");
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            continue;
                        }
                    };
                    let profile_topology = taskbar_profile_topology_keys(&snapshot);
                    if !stable_taskbar_topology_matches(&app, &profile_topology) {
                        let _profile_guard = TASKBAR_PROFILE_GATE
                            .lock()
                            .unwrap_or_else(|err| err.into_inner());
                        let _ = set_stable_taskbar_topology(&app, &TaskbarTopology::default());
                    }
                    let settled_topology = topology_stability.observe(profile_topology.clone());
                    if !topology_stability.allows_native_layout(&profile_topology) {
                        last_signature = None;
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    let topology_signature = taskbar_topology_signature(&snapshot);
                    if last_topology_signature.as_ref() != Some(&topology_signature) {
                        emit_taskbar_topology(&app, &settings, &snapshot);
                        last_topology_signature = Some(topology_signature);
                    }
                    match migrate_legacy_taskbar_monitor_keys(&settings, &snapshot) {
                        Ok(true) => {
                            settings_valid = false;
                            settings_event_pending = true;
                            last_signature = None;
                            continue;
                        }
                        Ok(false) => {}
                        Err(err) => {
                            last_signature = None;
                            eprintln!("[taskbar] monitor key migration failed: {err}");
                        }
                    }
                    if let Some(stable_topology) = settled_topology {
                        let _profile_guard = TASKBAR_PROFILE_GATE
                            .lock()
                            .unwrap_or_else(|err| err.into_inner());
                        let pending = pending_taskbar_profile_placements(&app, &stable_topology);
                        match reconcile_taskbar_layout_profile(
                            &settings,
                            &stable_topology,
                            &pending,
                        ) {
                            Ok(true) => {
                                clear_pending_taskbar_profile_placements(&app, &pending);
                                if !set_stable_taskbar_topology(&app, &stable_topology) {
                                    topology_stability.rearm();
                                }
                                settings_valid = false;
                                settings_event_pending = true;
                                profile_ratio_sync_pending = true;
                                last_signature = None;
                                continue;
                            }
                            Ok(false) => {
                                clear_pending_taskbar_profile_placements(&app, &pending);
                                if !set_stable_taskbar_topology(&app, &stable_topology) {
                                    topology_stability.rearm();
                                    settings_valid = false;
                                    last_signature = None;
                                    continue;
                                }
                            }
                            Err(err) => {
                                let _ =
                                    set_stable_taskbar_topology(&app, &TaskbarTopology::default());
                                topology_stability.rearm();
                                settings_valid = false;
                                last_signature = None;
                                eprintln!("[taskbar] layout profile reconciliation failed: {err}");
                                continue;
                            }
                        }
                    }
                    let dock_result = (|| -> anyhow::Result<Option<String>> {
                        let (signature, requires_reapply) =
                            taskbar_dock_signature(&app, &settings, &snapshot)?;
                        if !requires_reapply && last_signature.as_ref() == Some(&signature) {
                            return Ok(None);
                        }
                        apply_taskbar_dock_with_snapshot(
                            &app,
                            &settings,
                            &snapshot,
                            settings_generation,
                        )?;
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
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
    }

    #[cfg(not(windows))]
    {
        let _ = app;
    }
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

fn apply_claude_statusline_for_release(enabled: bool) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Ok(());
    }

    if enabled {
        statusline_bridge_path().and_then(|bridge| {
            Settings::install_statusline_wrap(&bridge).map_err(|err| err.to_string())
        })
    } else {
        Settings::restore_statusline_if_installed().map_err(|err| err.to_string())
    }
}

fn reconcile_claude_statusline_for_release() -> Result<(), String> {
    reconcile_current_collection_policy(
        &Tool::Claude,
        Settings::try_load,
        |enabled| apply_claude_statusline_for_release(enabled).map_err(anyhow::Error::msg),
        |failed| {
            COLLECTION_POLICY_RECOVERY
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .record(&Tool::Claude, failed, std::time::Instant::now());
        },
    )
    .map_err(|err| err.to_string())
}

async fn reconcile_claude_statusline_off_thread() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(reconcile_claude_statusline_for_release)
        .await
        .map_err(|err| format!("Claude collection task failed: {err}"))?
}

fn spawn_claude_statusline_reconcile() {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = reconcile_claude_statusline_off_thread().await {
            eprintln!("[statusline] startup reconcile failed: {err}");
        }
    });
}

fn reconcile_current_collection_policy(
    tool: &Tool,
    load_settings: impl FnOnce() -> anyhow::Result<Settings>,
    reconcile: impl FnOnce(bool) -> anyhow::Result<()>,
    publish_failure: impl FnOnce(bool),
) -> anyhow::Result<()> {
    with_taskbar_settings_read(|_| {
        // Keep the current setting, binding change and health publication in one order.
        let result = apply_collection_policy_from_settings(tool, load_settings(), reconcile);
        publish_failure(result.is_err());
        result
    })
}

fn apply_collection_policy_from_settings(
    tool: &Tool,
    settings: anyhow::Result<Settings>,
    reconcile: impl FnOnce(bool) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    apply_collection_policy_from_settings_with_retirement(tool, settings, reconcile, || {
        antigravity::set_enabled(false);
    })
}

fn apply_collection_policy_from_settings_with_retirement(
    tool: &Tool,
    settings: anyhow::Result<Settings>,
    reconcile: impl FnOnce(bool) -> anyhow::Result<()>,
    retire_antigravity: impl FnOnce(),
) -> anyhow::Result<()> {
    match settings {
        Ok(settings) => reconcile(CollectionPolicy::from_settings(&settings).enabled(tool)),
        Err(error) if matches!(tool, Tool::Codex | Tool::Grok) => match reconcile(false) {
            Ok(()) => Err(error),
            Err(disable_error) => Err(disable_error.context("could not disable collection policy")),
        },
        Err(error) if *tool == Tool::Antigravity => {
            // Retire memory admission without invoking a settings/binding writer.
            retire_antigravity();
            Err(error)
        }
        Err(error) => Err(error),
    }
}

fn reconcile_current_antigravity_cli(
    load_settings: impl FnOnce() -> anyhow::Result<Settings>,
    reconcile: impl FnOnce(bool) -> anyhow::Result<()>,
    publish_failure: impl FnOnce(bool),
) -> anyhow::Result<()> {
    reconcile_current_collection_policy(
        &Tool::Antigravity,
        load_settings,
        reconcile,
        publish_failure,
    )
}

fn reconcile_broker_policy_for(tool: &Tool) -> anyhow::Result<()> {
    reconcile_current_collection_policy(
        tool,
        Settings::try_load,
        |enabled| match tool {
            Tool::Codex => collector::set_codex_app_server_enabled(enabled),
            Tool::Grok => collector::set_grok_acp_enabled(enabled),
            _ => anyhow::bail!("unsupported collection broker"),
        },
        |failed| {
            COLLECTION_POLICY_RECOVERY
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .record(tool, failed, std::time::Instant::now());
        },
    )
}

fn recover_pending_collection_policies_with(
    state: &Mutex<CollectionPolicyRecovery>,
    clock: impl Fn() -> std::time::Instant,
    mut load_settings: impl FnMut() -> anyhow::Result<Settings>,
    mut reconcile: impl FnMut(&Tool, bool) -> anyhow::Result<()>,
    mut publish_failure: impl FnMut(&Tool, bool),
) -> anyhow::Result<()> {
    let due = {
        let state = state.lock().unwrap_or_else(|err| err.into_inner());
        state.any_due(clock())
    };
    if !due {
        return Ok(());
    }
    with_taskbar_settings_read(|_| {
        let mut last_error = None;
        for tool in [&Tool::Claude, &Tool::Codex, &Tool::Grok, &Tool::Antigravity] {
            // Recheck after acquiring the gate: a newer direct apply may have repaired it.
            if !state
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .due(tool, clock())
            {
                continue;
            }
            let result = apply_collection_policy_from_settings(tool, load_settings(), |enabled| {
                reconcile(tool, enabled)
            });
            state.lock().unwrap_or_else(|err| err.into_inner()).record(
                tool,
                result.is_err(),
                clock(),
            );
            publish_failure(tool, result.is_err());
            if let Err(error) = result {
                last_error = Some(error);
            }
        }
        match last_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })
}

fn recover_pending_collection_policies() -> anyhow::Result<()> {
    recover_pending_collection_policies_with(
        &COLLECTION_POLICY_RECOVERY,
        std::time::Instant::now,
        Settings::try_load,
        |tool, enabled| match tool {
            Tool::Claude => {
                apply_claude_statusline_for_release(enabled).map_err(anyhow::Error::msg)
            }
            Tool::Codex => collector::set_codex_app_server_enabled(enabled),
            Tool::Grok => collector::set_grok_acp_enabled(enabled),
            Tool::Antigravity => apply_antigravity_cli_policy_for_release(enabled),
            _ => anyhow::bail!("unsupported collection broker"),
        },
        |tool, failed| {
            if *tool == Tool::Antigravity {
                antigravity_cli::set_connection_failed(failed);
            }
        },
    )
}

async fn recover_pending_collection_policies_off_thread() -> anyhow::Result<()> {
    let due = COLLECTION_POLICY_RECOVERY
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .any_due(std::time::Instant::now());
    if !due {
        return Ok(());
    }
    tauri::async_runtime::spawn_blocking(recover_pending_collection_policies)
        .await
        .map_err(|_| anyhow::anyhow!("collection policy recovery task failed"))?
}

async fn reconcile_broker_policy_off_thread(tool: Tool) -> anyhow::Result<()> {
    tauri::async_runtime::spawn_blocking(move || reconcile_broker_policy_for(&tool))
        .await
        .map_err(|_| anyhow::anyhow!("collection policy task failed"))?
}

fn apply_antigravity_cli_policy_for_release(enabled: bool) -> anyhow::Result<()> {
    antigravity::set_enabled(enabled);
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let bridge = statusline_bridge_path()
        .map_err(|_| anyhow::anyhow!("Antigravity CLI bridge path unavailable"))?;
    // Account reads do not depend on a user's statusline or its in-memory TUI state.
    // Retire only our old connection; stale CLI callbacks can still forward custom output.
    antigravity_cli::binding::reconcile(false, std::path::Path::new(&bridge))
}

fn reconcile_antigravity_cli_for_release() -> Result<(), String> {
    reconcile_current_antigravity_cli(
        Settings::try_load,
        apply_antigravity_cli_policy_for_release,
        |failed| {
            COLLECTION_POLICY_RECOVERY
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .record(&Tool::Antigravity, failed, std::time::Instant::now());
            antigravity_cli::set_connection_failed(failed);
        },
    )
    .map_err(|_| "Antigravity CLI connection could not be updated".to_string())
}

async fn reconcile_antigravity_cli_off_thread() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(reconcile_antigravity_cli_for_release)
        .await
        .map_err(|_| "Antigravity CLI connection task failed".to_string())?
}

fn spawn_antigravity_cli_reconcile() {
    tauri::async_runtime::spawn(async move {
        if reconcile_antigravity_cli_off_thread().await.is_err() {
            eprintln!("[antigravity] CLI connection reconciliation failed");
        }
    });
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
    #[cfg(windows)]
    let instance_event = match single_instance::acquire() {
        Ok(single_instance::AcquireResult::Primary(event)) => event,
        Ok(single_instance::AcquireResult::Secondary) => return,
        Err(err) => {
            eprintln!("[single-instance] startup guard failed: {err}");
            return;
        }
    };

    #[cfg(windows)]
    update::start_update_temp_cleanup();

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_collection_health,
            get_activity,
            refresh_status,
            get_update_status,
            check_for_updates,
            install_update,
            open_release_page,
            get_settings,
            get_system_text_scale,
            save_settings,
            clear_taskbar_layout_profiles,
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
            start_panel_drag
        ])
        .setup(move |app| {
            let scale_app = app.handle().clone();
            app.manage(text_scale::SystemTextScale::start(move |snapshot| {
                let _ = scale_app.emit(text_scale::CHANGED_EVENT, snapshot);
            }));
            app.manage(TaskbarPauseState::default());
            app.manage(TaskbarMenuState::default());
            app.manage(TaskbarRecoveryState::default());
            app.manage(TaskbarDragState::default());
            app.manage(TaskbarStableTopologyState::default());
            app.manage(TaskbarTooltipTextState::default());
            app.manage(TaskbarContentLayoutState::default());
            app.manage(TaskbarWindowState::default());
            app.manage(TaskbarShutdownState::default());
            app.manage(QuitPendingState::default());
            app.manage(SettingsSideEffectRetryState::default());
            #[cfg(windows)]
            register_taskbar_window_handles(app);
            #[cfg(windows)]
            spawn_single_instance_listener(app.handle().clone(), instance_event);
            setup_panel_close_hide(app);
            setup_trays(app)?;
            let settings = match Settings::try_load() {
                Ok(loaded) => {
                    #[cfg(windows)]
                    let loaded = match initialize_pending_taskbar_targets(app, &loaded) {
                        Ok(snapshot) => snapshot.settings,
                        Err(err) => {
                            eprintln!("[taskbar] initial left placement failed: {err}");
                            loaded
                        }
                    };
                    Some(loaded)
                }
                Err(err) => {
                    eprintln!("[settings] startup load failed; side effects disabled: {err}");
                    hide_all_taskbar_bars(app);
                    None
                }
            };
            if let Err(error) = reconcile_broker_policy_for(&Tool::Codex) {
                eprintln!("[codex] app-server broker startup failed: {error}");
            }
            antigravity::set_enabled(
                settings
                    .as_ref()
                    .is_some_and(|settings| settings.show_antigravity),
            );
            if let Err(error) = reconcile_broker_policy_for(&Tool::Grok) {
                eprintln!("[grok] ACP broker startup failed: {error}");
            }
            if let Some(settings) = settings.as_ref() {
                set_taskbar_bars_paused(app, settings.taskbar_bars_paused);
                let taskbar_retry = match try_setup_taskbar_dock(app, settings) {
                    Ok(()) => false,
                    Err(err) => {
                        eprintln!("[taskbar] fallback to tray: {err}");
                        true
                    }
                };
                let autostart_retry = match apply_autostart_for_release(app, settings) {
                    Ok(()) => false,
                    Err(err) => {
                        eprintln!("[autostart] startup apply failed: {err}");
                        true
                    }
                };
                retry_settings_side_effects(app.handle().clone(), taskbar_retry, autostart_retry);
            }
            // Current-policy workers record a retry even if the first read failed.
            spawn_claude_statusline_reconcile();
            spawn_antigravity_cli_reconcile();
            let (system_activity, system_activity_shutdown) =
                system_activity::SystemActivityMonitor::start();
            app.manage(system_activity_shutdown);
            spawn_status_loop(app.handle().clone(), system_activity);
            spawn_taskbar_drag_loop(app.handle().clone());
            spawn_taskbar_visibility_loop(app.handle().clone());
            spawn_update_check(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                    request_app_quit(app);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use crate::{
        config::{
            Settings, TaskbarLayoutProfile, TaskbarMonitorMode, TaskbarPlacement, TaskbarTopology,
        },
        model::{AccountLimit, AgentStatus, SessionInfo, Tool},
        render::Palette,
        taskbar,
    };
    #[test]
    fn settings_edits_merge_only_changed_fields_into_the_latest_profile() {
        let baseline = Settings::default();
        let mut requested = baseline.clone();
        requested.theme = "dark".into();
        requested.tool_colors.claude_primary = [1, 2, 3];
        let mut current = baseline.clone();
        current.bar_mode = "compact".into();
        current.indicator_style = "bar".into();
        current.bar_content_gap_px = 3.1;
        current.tool_colors.cursor_primary = [4, 5, 6];
        current.taskbar_bars_paused = true;
        current.codex_taskbar_offset_ratio = 0.8;
        super::record_taskbar_layout_profile(&mut current, &vec!["monitor-b".into()].into());
        let merged = super::merge_settings_edits(&current, Some(&baseline), &requested).unwrap();
        assert_eq!(merged.theme, "dark");
        assert_eq!(merged.bar_mode, "compact");
        assert_eq!(merged.indicator_style, "bar");
        assert_eq!(merged.bar_content_gap_px, 3.1);
        assert_eq!(merged.tool_colors.claude_primary, [1, 2, 3]);
        assert_eq!(merged.tool_colors.cursor_primary, [4, 5, 6]);
        assert!(merged.taskbar_bars_paused);
        assert_eq!(merged.codex_taskbar_offset_ratio, 0.8);
        assert_eq!(
            merged.taskbar_layout_profiles,
            current.taskbar_layout_profiles
        );
    }

    #[test]
    fn scoped_edits_reject_a_changed_topology_but_global_edits_merge() {
        let baseline = Settings::default();
        let mut requested = baseline.clone();
        requested.theme = "dark".into();
        let old_topology = TaskbarTopology::from(vec!["monitor-a".into()]);
        let new_topology = TaskbarTopology::from(vec!["monitor-b".into()]);
        assert!(super::validate_settings_edit_topology(
            &baseline,
            Some(&baseline),
            &requested,
            Some(&old_topology),
            &new_topology
        )
        .is_ok());
        requested.bar_mode = "quad".into();
        assert!(super::validate_settings_edit_topology(
            &baseline,
            Some(&baseline),
            &requested,
            Some(&old_topology),
            &new_topology
        )
        .is_err());
        assert!(super::validate_settings_edit_topology(
            &baseline,
            Some(&baseline),
            &requested,
            Some(&old_topology),
            &TaskbarTopology::default()
        )
        .is_err());
        assert!(super::validate_settings_edit_topology(
            &baseline,
            Some(&baseline),
            &requested,
            Some(&old_topology),
            &old_topology
        )
        .is_ok());
    }

    #[test]
    fn settings_edit_merge_switches_payload_palette_variants_without_stale_tags() {
        let palettes = [
            Palette::Traffic,
            Palette::Mono([1, 2, 3]),
            Palette::Mono([4, 5, 6]),
            Palette::Custom([1, 2, 3], [4, 5, 6], [7, 8, 9]),
            Palette::Custom([9, 8, 7], [6, 5, 4], [3, 2, 1]),
        ];
        for before in palettes {
            for after in palettes {
                let baseline = Settings {
                    palette: before,
                    ..Settings::default()
                };
                let mut current = baseline.clone();
                current.theme = "dark".into();
                let mut requested = baseline.clone();
                requested.palette = after;
                let merged = super::merge_settings_edits(&current, Some(&baseline), &requested)
                    .unwrap_or_else(|error| panic!("{before:?} -> {after:?}: {error}"));
                assert_eq!(merged.palette, after);
                assert_eq!(merged.theme, "dark");
            }
        }
    }

    #[test]
    fn native_tray_labels_follow_explicit_and_system_language() {
        let english = super::tray_menu_labels("en", 0x0412);
        assert_eq!(
            english,
            [
                "Open Juice",
                "Refresh usage",
                "Pause bars",
                "Resume bars",
                "Quit"
            ]
        );
        assert_eq!(super::tray_menu_labels("system", 0x0409), english);
        assert_eq!(
            super::tray_menu_labels("ko", 0x0409),
            super::tray_menu_labels("system", 0x0412)
        );
    }

    #[test]
    fn measured_bar_width_never_rounds_below_content_at_any_supported_dpi() {
        for dpi in [96, 120, 144, 168, 192] {
            for css_length in [36.0, 37.1, 83.2, 105.0, 186.2, 431.7, 1024.0] {
                for zoom in [1.0, 1.1, 1.25, 1.5] {
                    let scale = dpi as f64 / 96.0;
                    let logical = super::taskbar_measured_logical_length(
                        css_length,
                        Some(scale * zoom),
                        scale,
                    )
                    .unwrap();
                    let physical = super::taskbar_physical_length(logical, dpi) as f64;
                    let content = css_length * scale * zoom;
                    assert!(
                        physical >= content,
                        "dpi={dpi}, css={css_length}, zoom={zoom}"
                    );
                    assert!(physical - content < scale + 1.0);
                }
            }
        }
        assert_eq!(super::taskbar_physical_length(37, 120), 47);
        assert_eq!(
            super::taskbar_measured_logical_length(1050.7, Some(2.0), 2.0).unwrap(),
            1051
        );
        assert_eq!(
            super::taskbar_measured_logical_length(2304.0, Some(1.0), 1.0).unwrap(),
            2304
        );
        assert!(super::taskbar_measured_logical_length(2305.0, Some(1.0), 1.0).is_err());
        for shell_dpi in [96, 120, 144, 168, 192] {
            for webview_dpr in [1.0, 1.25, 1.5, 1.75, 2.0] {
                let length = super::taskbar_measured_logical_length(
                    183.3,
                    Some(webview_dpr),
                    shell_dpi as f64 / 96.0,
                )
                .unwrap();
                assert!(
                    super::taskbar_physical_length(length, shell_dpi) as f64 >= 183.3 * webview_dpr
                );
            }
        }
        assert_eq!(
            super::taskbar_measured_logical_length(37.1, None, 1.5).unwrap(),
            38
        );
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, 9.0] {
            assert!(super::taskbar_measured_logical_length(50.0, Some(invalid), 1.0).is_err());
        }
    }

    #[tokio::test]
    async fn stalled_update_download_times_out_and_releases_the_operation() {
        let gate = tokio::sync::Mutex::new(());
        let oversize = tokio::sync::Notify::new();
        let result = {
            let _guard = gate.lock().await;
            super::await_update_download(
                std::future::pending::<Result<Vec<u8>, ()>>(),
                &oversize,
                std::time::Duration::from_millis(20),
            )
            .await
        };
        assert_eq!(result.unwrap_err(), "update download timed out");
        assert!(gate.try_lock().is_ok());
        assert_eq!(
            super::await_update_download(
                std::future::ready(Ok::<_, ()>(vec![1u8])),
                &oversize,
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap(),
            vec![1]
        );
        oversize.notify_one();
        assert_eq!(
            super::await_update_download(
                std::future::pending::<Result<Vec<u8>, ()>>(),
                &oversize,
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap_err(),
            "update package exceeds the size limit"
        );
    }

    #[test]
    fn auth_failure_blocks_old_account_until_successful_collection() {
        use chrono::TimeZone;
        use std::sync::Mutex;
        let now = chrono::Utc.with_ymd_and_hms(2026, 9, 5, 0, 0, 0).unwrap();
        let cache = Mutex::new(None);
        super::cached_status_attempt(&cache, now, 60, true, || {
            Ok(status_for_signature("account-A"))
        });
        assert!(super::cached_status_attempt(&cache, now, 60, true, || Err(
            super::CollectionErrorKind::LoginRequired
        ))
        .is_none());
        assert!(super::cached_status_attempt(&cache, now, 60, true, || Err(
            super::CollectionErrorKind::Transport
        ))
        .is_none());
        assert_eq!(
            super::cached_collection_health(&cache),
            super::CollectionHealth::LoginRequired
        );
        assert!(
            super::cached_status_attempt(&cache, now, 60, false, || panic!("backoff")).is_none()
        );
        let fresh = super::cached_status_attempt(&cache, now, 60, true, || {
            Ok(status_for_signature("account-B"))
        })
        .unwrap();
        assert_eq!(fresh.session_id, "account-B");
        assert_eq!(
            super::cached_collection_health(&cache),
            super::CollectionHealth::Ready
        );
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
            let original = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn forced_taskbar_hover_requires_an_isolated_runtime() {
        assert_eq!(
            super::isolated_forced_taskbar_hover(true, Some("claude")),
            Some("claude")
        );
        assert_eq!(
            super::isolated_forced_taskbar_hover(true, Some("codex")),
            Some("codex")
        );
        assert_eq!(
            super::isolated_forced_taskbar_hover(false, Some("claude")),
            None
        );
        assert_eq!(
            super::isolated_forced_taskbar_hover(true, Some("unknown")),
            None
        );
    }

    #[test]
    fn taskbar_layout_profiles_restore_only_the_exact_monitor_setup() {
        let office = TaskbarTopology::from(vec!["monitor-office".into(), "monitor-laptop".into()]);
        let mobile = TaskbarTopology::from(vec!["monitor-laptop".into()]);
        let mut settings = Settings {
            taskbar_profile_colors_on: true,
            bar_mode: "full".into(),
            palette: Palette::Ocean,
            ..Settings::default()
        };
        super::set_taskbar_target(&mut settings, "claude", "monitor-office", 0.2);
        super::set_taskbar_target(&mut settings, "codex", "monitor-laptop", 0.7);
        assert!(super::record_taskbar_layout_profile(&mut settings, &office));

        super::set_taskbar_target(&mut settings, "claude", "monitor-laptop", 0.1);
        super::set_taskbar_target(&mut settings, "codex", "monitor-laptop", 0.45);
        settings.bar_mode = "compact".into();
        settings.palette = Palette::Forest;
        assert!(super::record_taskbar_layout_profile(&mut settings, &mobile));
        assert_eq!(settings.taskbar_layout_profiles.len(), 2);

        assert!(super::apply_taskbar_layout_profile(&mut settings, &office));
        assert_eq!(settings.claude_taskbar_monitor_key, "monitor-office");
        assert_eq!(settings.claude_taskbar_offset_ratio, 0.2);
        assert_eq!(settings.codex_taskbar_monitor_key, "monitor-laptop");
        assert_eq!(settings.codex_taskbar_offset_ratio, 0.7);
        assert_eq!(settings.bar_mode, "full");
        assert_eq!(settings.palette, Palette::Ocean);

        settings.taskbar_profile_colors_on = false;
        settings.palette = Palette::Sunset;
        assert!(super::apply_taskbar_layout_profile(&mut settings, &mobile));
        assert_eq!(settings.bar_mode, "compact");
        assert_eq!(settings.palette, Palette::Sunset);
        assert!(super::record_taskbar_layout_profile(&mut settings, &mobile));
        assert_eq!(
            settings
                .taskbar_layout_profile(&mobile)
                .unwrap()
                .appearance
                .as_ref()
                .unwrap()
                .palette,
            Palette::Forest
        );

        settings.taskbar_profile_presentation_on = false;
        settings.bar_mode = "quad".into();
        assert!(super::record_taskbar_layout_profile(&mut settings, &mobile));
        assert_eq!(
            settings
                .taskbar_layout_profile(&mobile)
                .unwrap()
                .presentation
                .as_ref()
                .unwrap()
                .bar_mode,
            "compact"
        );
        assert!(super::apply_taskbar_layout_profile(&mut settings, &office));
        assert_eq!(settings.bar_mode, "quad");

        settings.taskbar_profile_presentation_on = true;
        settings.taskbar_profile_colors_on = true;
        assert!(super::apply_taskbar_layout_profile(&mut settings, &mobile));
        assert_eq!(settings.bar_mode, "compact");
        assert_eq!(settings.palette, Palette::Forest);

        let similar = TaskbarTopology::from(vec!["monitor-laptop".into(), "monitor-home".into()]);
        assert!(!super::apply_taskbar_layout_profile(
            &mut settings,
            &similar
        ));
        assert_eq!(settings.claude_taskbar_monitor_key, "monitor-laptop");
    }

    fn display_topology(width: i32, height: i32, dpi: u32) -> TaskbarTopology {
        TaskbarTopology::from_modes(vec![TaskbarMonitorMode {
            monitor_key: "monitor-office".into(),
            width,
            height,
            dpi,
        }])
    }

    #[test]
    fn display_profiles_round_trip_resolution_scale_fonts_colors_and_all_positions() {
        let modes = [
            display_topology(3840, 2160, 144),
            display_topology(1920, 1080, 144), // resolution only
            display_topology(3840, 2160, 96),  // scale only
            display_topology(1920, 1080, 96),  // both
        ];
        let mut settings = Settings {
            taskbar_profile_colors_on: true,
            ..Settings::default()
        };
        for (i, topology) in modes.iter().enumerate() {
            settings.bar_mode = if i % 2 == 0 { "full" } else { "compact" }.into();
            settings.font_mode = if i % 2 == 0 { "system" } else { "pretendard" }.into();
            settings.bar_text_font_size_px = 10.0 + i as f32;
            settings.ring_number_font_size_px = 9.0 + i as f32;
            settings.bar_text_font_weight = 400 + i as i32 * 100;
            settings.tool_colors.claude_primary = [i as u8, 20, 30];
            for (j, tool) in super::TASKBAR_TOOLS.iter().enumerate() {
                super::set_taskbar_target(
                    &mut settings,
                    tool,
                    "monitor-office",
                    (i + j) as f32 / 10.0,
                );
            }
            assert!(super::record_taskbar_layout_profile(
                &mut settings,
                topology
            ));
        }
        // Round-trip the persisted wire representation, not only in-memory structs.
        let json = serde_json::to_string(&settings).unwrap();
        let mut settings: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(settings.taskbar_layout_profiles.len(), 4);
        settings.theme = "dark".into();
        settings.poll_interval_secs = 777;
        for (i, topology) in modes.iter().enumerate().rev() {
            super::reconcile_taskbar_profile_state(&mut settings, topology, &[]);
            assert_eq!(
                settings.bar_mode,
                if i % 2 == 0 { "full" } else { "compact" }
            );
            assert_eq!(
                settings.font_mode,
                if i % 2 == 0 { "system" } else { "pretendard" }
            );
            assert_eq!(settings.bar_text_font_size_px, 10.0 + i as f32);
            assert_eq!(settings.ring_number_font_size_px, 9.0 + i as f32);
            assert_eq!(settings.bar_text_font_weight, 400 + i as i32 * 100);
            assert_eq!(settings.tool_colors.claude_primary, [i as u8, 20, 30]);
            for (j, tool) in super::TASKBAR_TOOLS.iter().enumerate() {
                assert_eq!(
                    super::taskbar_offset_ratio(&settings, tool),
                    (i + j) as f32 / 10.0
                );
            }
            assert_eq!(settings.theme, "dark");
            assert_eq!(settings.poll_interval_secs, 777);
        }
    }

    #[test]
    fn legacy_display_profile_is_copied_without_resetting_fonts_or_old_positions() {
        let legacy = TaskbarTopology::from(vec!["monitor-office".into()]);
        let mut settings = Settings {
            bar_mode: "compact".into(),
            ..Settings::default()
        };
        super::set_taskbar_target(&mut settings, "claude", "monitor-office", 0.2);
        super::record_taskbar_layout_profile(&mut settings, &legacy);
        // Actual old JSON has neither monitor_modes nor presentation.font_mode.
        let mut json = serde_json::to_value(&settings).unwrap();
        json["taskbar_layout_profiles"][0]["presentation"]
            .as_object_mut()
            .unwrap()
            .remove("font_mode");
        let mut settings: Settings = serde_json::from_value(json).unwrap();
        let old_profile = settings.taskbar_layout_profiles[0].clone();
        settings.font_mode = "pretendard".into();
        settings.bar_mode = "full".into();
        let mode = display_topology(3840, 2160, 144);
        assert!(super::reconcile_taskbar_profile_state(
            &mut settings,
            &mode,
            &[]
        ));
        assert_eq!(settings.bar_mode, "compact");
        assert_eq!(settings.font_mode, "pretendard");
        assert_eq!(settings.taskbar_layout_profiles.len(), 2);
        assert_eq!(settings.taskbar_layout_profile(&legacy), Some(&old_profile));
        assert_eq!(
            settings
                .taskbar_layout_profile(&mode)
                .unwrap()
                .presentation
                .as_ref()
                .unwrap()
                .font_mode
                .as_deref(),
            Some("pretendard")
        );
        settings.bar_mode = "quad".into();
        super::set_taskbar_target(&mut settings, "claude", "monitor-office", 0.8);
        super::record_taskbar_layout_profile(&mut settings, &mode);
        assert_eq!(settings.taskbar_layout_profile(&legacy), Some(&old_profile));
        let other = display_topology(1920, 1080, 96);
        super::reconcile_taskbar_profile_state(&mut settings, &other, &[]);
        assert_eq!(settings.claude_taskbar_offset_ratio, 0.2);
        assert_eq!(settings.bar_mode, "compact");
    }

    #[test]
    fn unseen_display_modes_inherit_current_settings_and_respect_disabled_memory_options() {
        let first = display_topology(3840, 2160, 144);
        let second = display_topology(1920, 1080, 96);
        let mut settings = Settings {
            font_mode: "pretendard".into(),
            bar_mode: "compact".into(),
            taskbar_profile_colors_on: true,
            ..Settings::default()
        };
        super::set_taskbar_target(&mut settings, "claude", "monitor-office", 0.4);
        super::reconcile_taskbar_profile_state(&mut settings, &first, &[]);
        assert!(super::reconcile_taskbar_profile_state(
            &mut settings,
            &second,
            &[]
        ));
        assert_eq!(settings.font_mode, "pretendard");
        assert_eq!(settings.claude_taskbar_offset_ratio, 0.4);
        settings.taskbar_profile_presentation_on = false;
        settings.taskbar_profile_colors_on = false;
        settings.font_mode = "system".into();
        settings.bar_mode = "quad".into();
        settings.palette = Palette::Sunset;
        super::record_taskbar_layout_profile(&mut settings, &second);
        super::reconcile_taskbar_profile_state(&mut settings, &first, &[]);
        assert_eq!(settings.font_mode, "system");
        assert_eq!(settings.bar_mode, "quad");
        assert_eq!(settings.palette, Palette::Sunset);
        assert_eq!(
            settings
                .taskbar_layout_profile(&second)
                .unwrap()
                .presentation
                .as_ref()
                .unwrap()
                .font_mode
                .as_deref(),
            Some("pretendard")
        );
        settings.taskbar_layout_memory_on = false;
        let before = serde_json::to_value(&settings).unwrap();
        assert!(!super::reconcile_taskbar_profile_state(
            &mut settings,
            &second,
            &[]
        ));
        assert_eq!(before, serde_json::to_value(settings).unwrap());
    }

    #[test]
    fn stale_display_edits_and_pending_drags_cannot_cross_resolution_or_scale() {
        let original = display_topology(3840, 2160, 144);
        for changed in [
            display_topology(1920, 1080, 144),
            display_topology(3840, 2160, 96),
        ] {
            let baseline = Settings {
                taskbar_profile_colors_on: true,
                ..Settings::default()
            };
            for requested in [
                Settings {
                    font_mode: "pretendard".into(),
                    ..baseline.clone()
                },
                Settings {
                    bar_mode: "compact".into(),
                    ..baseline.clone()
                },
                Settings {
                    bar_text_font_size_px: 25.0,
                    ..baseline.clone()
                },
                Settings {
                    palette: Palette::Forest,
                    ..baseline.clone()
                },
            ] {
                assert!(super::validate_settings_edit_topology(
                    &baseline,
                    Some(&baseline),
                    &requested,
                    Some(&original),
                    &changed
                )
                .is_err());
            }
            let mut settings = baseline;
            super::set_taskbar_target(&mut settings, "claude", "monitor-office", 0.2);
            super::record_taskbar_layout_profile(&mut settings, &original);
            super::record_taskbar_layout_profile(&mut settings, &changed);
            let pending = super::PendingTaskbarProfilePlacement {
                topology: original.clone(),
                tool: "claude",
                placement: TaskbarPlacement {
                    monitor_key: "monitor-office".into(),
                    offset_ratio: 0.9,
                },
            };
            assert!(!super::apply_pending_taskbar_profile_placements(
                &mut settings,
                &changed,
                std::slice::from_ref(&pending)
            ));
            assert_eq!(settings.claude_taskbar_offset_ratio, 0.2);
            assert!(super::apply_pending_taskbar_profile_placements(
                &mut settings,
                &original,
                &[pending]
            ));
            assert_eq!(settings.claude_taskbar_offset_ratio, 0.9);
            assert_eq!(
                settings
                    .taskbar_layout_profile(&changed)
                    .unwrap()
                    .claude
                    .as_ref()
                    .unwrap()
                    .offset_ratio,
                0.2
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn display_mode_changes_require_consecutive_observations() {
        let mut stability = super::TaskbarTopologyStability::default();
        let original = display_topology(3840, 2160, 144);
        let changed = display_topology(3840, 2160, 96);
        for _ in 0..2 {
            assert!(stability.observe(original.clone()).is_none());
        }
        assert_eq!(stability.observe(original.clone()), Some(original.clone()));
        assert!(stability.observe(changed.clone()).is_none());
        assert!(stability.observe(original.clone()).is_none());
        for _ in 0..2 {
            assert!(stability.observe(changed.clone()).is_none());
        }
        assert_eq!(stability.observe(changed.clone()), Some(changed));
    }

    #[test]
    fn disabled_taskbar_layout_memory_does_not_capture_a_dragged_position() {
        let mut settings = Settings {
            taskbar_layout_memory_on: false,
            ..Settings::default()
        };
        super::set_taskbar_target(&mut settings, "claude", "monitor-laptop", 0.3);
        assert!(!super::record_taskbar_layout_profile(
            &mut settings,
            &vec!["monitor-laptop".into()].into()
        ));
        assert!(settings.taskbar_layout_profiles.is_empty());
        assert!(!settings.taskbar_layout_memory_initialized);
    }

    #[test]
    fn initialized_layout_memory_records_a_previously_unknown_stable_topology() {
        let topology = TaskbarTopology::from(vec!["monitor-new".to_string()]);
        let mut settings = Settings {
            taskbar_layout_memory_initialized: true,
            ..Settings::default()
        };
        super::set_taskbar_target(&mut settings, "claude", "monitor-new", 0.3);

        assert!(super::taskbar_layout_profile_reconcile_needed(
            &settings,
            &topology,
            &[]
        ));
        assert!(super::record_taskbar_layout_profile(
            &mut settings,
            &topology
        ));
        assert!(!super::taskbar_layout_profile_reconcile_needed(
            &settings,
            &topology,
            &[]
        ));

        settings.taskbar_layout_memory_on = false;
        assert!(!super::taskbar_layout_profile_reconcile_needed(
            &settings,
            &topology,
            &[]
        ));
    }

    #[test]
    fn partial_taskbar_layout_profile_adds_only_the_missing_current_tool() {
        let topology =
            TaskbarTopology::from(vec!["monitor-office".into(), "monitor-laptop".into()]);
        let mut settings = Settings::default();
        super::set_taskbar_target(&mut settings, "claude", "monitor-office", 0.9);
        super::set_taskbar_target(&mut settings, "codex", "monitor-laptop", 0.7);
        super::set_taskbar_target(&mut settings, "grok", "monitor-office", 0.5);
        super::set_taskbar_target(&mut settings, "cursor", "monitor-laptop", 0.4);
        assert!(
            settings.upsert_taskbar_layout_profile(TaskbarLayoutProfile {
                topology: topology.clone(),
                claude: Some(TaskbarPlacement {
                    monitor_key: "monitor-office".into(),
                    offset_ratio: 0.2,
                }),
                codex: None,
                grok: None,
                cursor: None,
                antigravity: None,
                presentation: None,
                appearance: None,
            })
        );

        assert!(super::complete_taskbar_layout_profile(
            &mut settings,
            &topology
        ));
        let profile = settings.taskbar_layout_profile(&topology).unwrap();
        assert_eq!(profile.claude.as_ref().unwrap().offset_ratio, 0.2);
        assert_eq!(profile.codex.as_ref().unwrap().offset_ratio, 0.7);
        assert_eq!(profile.grok.as_ref().unwrap().offset_ratio, 0.5);
        assert_eq!(profile.cursor.as_ref().unwrap().offset_ratio, 0.4);
        assert!(profile.presentation.is_some());
        assert!(profile.appearance.is_none());

        assert!(super::apply_taskbar_layout_profile(
            &mut settings,
            &topology
        ));
        assert_eq!(settings.claude_taskbar_offset_ratio, 0.2);
        assert_eq!(settings.codex_taskbar_offset_ratio, 0.7);
        assert_eq!(settings.grok_taskbar_offset_ratio, 0.5);
        assert_eq!(settings.cursor_taskbar_offset_ratio, 0.4);
    }

    #[test]
    fn pending_drag_updates_only_its_profile_placement_after_topology_stabilizes() {
        let topology =
            TaskbarTopology::from(vec!["monitor-office".into(), "monitor-laptop".into()]);
        let mut settings = Settings::default();
        assert!(
            settings.upsert_taskbar_layout_profile(TaskbarLayoutProfile {
                topology: topology.clone(),
                claude: Some(TaskbarPlacement {
                    monitor_key: "monitor-office".into(),
                    offset_ratio: 0.2,
                }),
                codex: Some(TaskbarPlacement {
                    monitor_key: "monitor-laptop".into(),
                    offset_ratio: 0.7,
                }),
                grok: None,
                cursor: None,
                antigravity: None,
                presentation: None,
                appearance: None,
            })
        );
        let pending = vec![super::PendingTaskbarProfilePlacement {
            topology: topology.clone(),
            tool: "claude",
            placement: TaskbarPlacement {
                monitor_key: "monitor-laptop".into(),
                offset_ratio: 0.9,
            },
        }];
        super::set_taskbar_target(&mut settings, "claude", "monitor-laptop", 0.9);

        assert!(super::apply_pending_taskbar_profile_placements(
            &mut settings,
            &topology,
            &pending,
        ));
        let profile = settings.taskbar_layout_profile(&topology).unwrap();
        assert_eq!(
            profile.claude.as_ref().unwrap().monitor_key,
            "monitor-laptop"
        );
        assert_eq!(profile.claude.as_ref().unwrap().offset_ratio, 0.9);
        assert_eq!(
            profile.codex.as_ref().unwrap().monitor_key,
            "monitor-laptop"
        );
        assert_eq!(profile.codex.as_ref().unwrap().offset_ratio, 0.7);
        assert_eq!(settings.claude_taskbar_offset_ratio, 0.9);
        assert_eq!(settings.codex_taskbar_offset_ratio, 0.7);

        super::set_taskbar_target(&mut settings, "claude", "monitor-laptop", 0.4);
        assert!(super::apply_pending_taskbar_profile_placements(
            &mut settings,
            &topology,
            &pending,
        ));
        assert_eq!(settings.claude_taskbar_offset_ratio, 0.9);
        assert_eq!(
            settings
                .taskbar_layout_profile(&topology)
                .unwrap()
                .claude
                .as_ref()
                .unwrap()
                .offset_ratio,
            0.9,
        );
    }

    #[test]
    fn pending_drag_blocks_stable_topology_publication_until_it_is_processed() {
        let topology = TaskbarTopology::from(vec!["monitor-office".to_string()]);
        let mut state = super::TaskbarStableTopologyData::default();
        let first = super::PendingTaskbarProfilePlacement {
            topology: topology.clone(),
            tool: "claude",
            placement: TaskbarPlacement {
                monitor_key: "monitor-office".into(),
                offset_ratio: 0.2,
            },
        };
        assert!(super::store_pending_taskbar_profile_placement(
            &mut state,
            first.clone(),
        ));
        assert!(!super::try_publish_stable_taskbar_topology(
            &mut state, &topology,
        ));

        let home_topology = TaskbarTopology::from(vec!["monitor-home".to_string()]);
        let home = super::PendingTaskbarProfilePlacement {
            topology: home_topology.clone(),
            tool: "codex",
            placement: TaskbarPlacement {
                monitor_key: "monitor-home".into(),
                offset_ratio: 0.4,
            },
        };
        assert!(super::store_pending_taskbar_profile_placement(
            &mut state,
            home.clone(),
        ));

        let replacement = super::PendingTaskbarProfilePlacement {
            placement: TaskbarPlacement {
                monitor_key: "monitor-office".into(),
                offset_ratio: 0.8,
            },
            ..first
        };
        assert!(super::store_pending_taskbar_profile_placement(
            &mut state,
            replacement.clone(),
        ));
        assert_eq!(state.pending_placements, vec![home, replacement.clone()]);
        super::clear_pending_taskbar_profile_state(&mut state);
        assert!(state.pending_placements.is_empty());
        assert!(super::try_publish_stable_taskbar_topology(
            &mut state, &topology,
        ));
        assert_eq!(state.topology, topology);
        assert!(!super::store_pending_taskbar_profile_placement(
            &mut state,
            replacement,
        ));
    }

    #[test]
    fn pending_drag_storage_is_bounded_and_refreshes_replaced_entries() {
        let mut state = super::TaskbarStableTopologyData::default();
        for index in 0..super::MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS {
            let monitor_key = format!("monitor-{index}");
            assert!(super::store_pending_taskbar_profile_placement(
                &mut state,
                super::PendingTaskbarProfilePlacement {
                    topology: vec![monitor_key.clone()].into(),
                    tool: "claude",
                    placement: TaskbarPlacement {
                        monitor_key,
                        offset_ratio: index as f32 / 100.0,
                    },
                },
            ));
        }
        assert_eq!(
            state.pending_placements.len(),
            super::MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS
        );

        let refreshed = super::PendingTaskbarProfilePlacement {
            topology: vec!["monitor-0".into()].into(),
            tool: "claude",
            placement: TaskbarPlacement {
                monitor_key: "monitor-0".into(),
                offset_ratio: 0.99,
            },
        };
        assert!(super::store_pending_taskbar_profile_placement(
            &mut state,
            refreshed.clone(),
        ));
        assert_eq!(
            state.pending_placements.len(),
            super::MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS
        );
        assert_eq!(state.pending_placements.last(), Some(&refreshed));

        let newest = super::PendingTaskbarProfilePlacement {
            topology: vec!["monitor-32".into()].into(),
            tool: "codex",
            placement: TaskbarPlacement {
                monitor_key: "monitor-32".into(),
                offset_ratio: 0.5,
            },
        };
        assert!(super::store_pending_taskbar_profile_placement(
            &mut state,
            newest.clone(),
        ));
        assert_eq!(
            state.pending_placements.len(),
            super::MAX_PENDING_TASKBAR_PROFILE_PLACEMENTS
        );
        assert!(!state
            .pending_placements
            .iter()
            .any(|item| item.topology.monitor_keys == ["monitor-1"]));
        assert!(state.pending_placements.contains(&refreshed));
        assert_eq!(state.pending_placements.last(), Some(&newest));
    }

    #[cfg(windows)]
    #[test]
    fn taskbar_topology_republishes_after_interrupted_display_changes() {
        let original = display_topology(3840, 2160, 144);
        for interruption in [
            display_topology(1920, 1080, 144),
            display_topology(3840, 2160, 96),
            TaskbarTopology::default(),
        ] {
            for samples in [1, 2] {
                let mut observer = super::TaskbarTopologyStability::default();
                let mut published = super::TaskbarStableTopologyData::default();
                let mut tick = |topology: &TaskbarTopology| {
                    if published.topology != *topology {
                        assert!(super::try_publish_stable_taskbar_topology(
                            &mut published,
                            &TaskbarTopology::default()
                        ));
                    }
                    if let Some(stable) = observer.observe(topology.clone()) {
                        assert!(super::try_publish_stable_taskbar_topology(
                            &mut published,
                            &stable
                        ));
                    }
                    published.topology.clone()
                };
                for _ in 0..3 {
                    tick(&original);
                }
                assert_eq!(tick(&original), original);
                for _ in 0..samples {
                    assert!(tick(&interruption).is_empty());
                }
                let baseline = Settings::default();
                let requested = Settings {
                    bar_mode: "compact".into(),
                    ..baseline.clone()
                };
                for _ in 0..2 {
                    let unstable = tick(&original);
                    assert!(unstable.is_empty());
                    assert!(super::validate_settings_edit_topology(
                        &baseline,
                        Some(&baseline),
                        &requested,
                        Some(&original),
                        &unstable
                    )
                    .is_err());
                }
                let stable = tick(&original);
                assert_eq!(
                    stable, original,
                    "the returned display must become editable"
                );
                assert!(super::validate_settings_edit_topology(
                    &baseline,
                    Some(&baseline),
                    &requested,
                    Some(&original),
                    &stable
                )
                .is_ok());
                assert_eq!(observer.observe(original.clone()), None);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn taskbar_topology_waits_for_consecutive_samples_after_a_read_failure() {
        let original = display_topology(3840, 2160, 144);
        let changed = display_topology(1920, 1080, 96);
        let mut observer = super::TaskbarTopologyStability::default();
        for _ in 0..3 {
            observer.observe(original.clone());
        }
        assert_eq!(observer.observe(TaskbarTopology::default()), None);
        assert_eq!(observer.observe(original.clone()), None);
        assert_eq!(observer.observe(changed.clone()), None);
        assert_eq!(observer.observe(original.clone()), None);
        assert_eq!(observer.observe(original.clone()), None);
        assert_eq!(observer.observe(original.clone()), Some(original.clone()));
        assert_eq!(observer.observe(changed.clone()), None);
        assert_eq!(observer.observe(changed.clone()), None);
        assert_eq!(observer.observe(changed.clone()), Some(changed.clone()));
        assert_eq!(observer.observe(changed), None);
    }

    #[cfg(windows)]
    #[test]
    fn taskbar_topology_requires_three_consecutive_observations() {
        let mut stability = super::TaskbarTopologyStability::default();
        let laptop = TaskbarTopology::from(vec!["monitor-laptop".into()]);
        assert_eq!(stability.observe(laptop.clone()), None);
        assert_eq!(stability.observe(laptop.clone()), None);
        assert_eq!(stability.observe(laptop.clone()), Some(laptop.clone()));
        assert_eq!(stability.observe(laptop.clone()), None);

        stability.rearm();
        assert_eq!(stability.observe(laptop.clone()), None);
        assert_eq!(stability.observe(laptop.clone()), None);
        assert_eq!(stability.observe(laptop.clone()), Some(laptop.clone()));

        let office = TaskbarTopology::from(vec!["monitor-laptop".into(), "monitor-office".into()]);
        assert_eq!(stability.observe(office.clone()), None);
        assert_eq!(stability.observe(TaskbarTopology::default()), None);
        assert_eq!(stability.observe(office.clone()), None);
        assert_eq!(stability.observe(office.clone()), None);
        assert_eq!(stability.observe(office.clone()), Some(office));
    }

    #[cfg(windows)]
    fn g3207_taskbar_snapshot() -> super::TaskbarDockSnapshot {
        use crate::taskbar::{DockRect, ShellTaskbarWindow};
        use windows::Win32::Foundation::HWND;

        super::TaskbarDockSnapshot {
            taskbars: vec![
                ShellTaskbarWindow {
                    hwnd: HWND(std::ptr::dangling_mut::<core::ffi::c_void>()),
                    dpi: 96,
                    left: 0,
                    top: 1392,
                    right: 3440,
                    bottom: 1440,
                    monitor: DockRect {
                        x: 0,
                        y: 0,
                        width: 3440,
                        height: 1440,
                    },
                    key: "monitor-path:primary".into(),
                    device_key: "device:primary".into(),
                    legacy_key: "monitor:0,0,3440,1440".into(),
                    primary: true,
                },
                ShellTaskbarWindow {
                    hwnd: HWND(2usize as *mut core::ffi::c_void),
                    dpi: 144,
                    left: 3440,
                    top: 1392,
                    right: 6000,
                    bottom: 1440,
                    monitor: DockRect {
                        x: 3440,
                        y: 0,
                        width: 2560,
                        height: 1440,
                    },
                    key: "monitor-path:secondary".into(),
                    device_key: "device:secondary".into(),
                    legacy_key: "monitor:3440,0,2560,1440".into(),
                    primary: false,
                },
            ],
            monitor_states: Default::default(),
        }
    }

    #[cfg(windows)]
    fn g3207_observe_native_targets(
        observer: &mut super::TaskbarTopologyStability,
        snapshot: &super::TaskbarDockSnapshot,
        settings: &Settings,
        targets: &mut Vec<String>,
    ) {
        let topology = super::taskbar_profile_topology_keys(snapshot);
        observer.observe(topology.clone());
        if observer.allows_native_layout(&topology) {
            *targets = super::TASKBAR_TOOLS
                .into_iter()
                .map(|tool| {
                    super::taskbar_from_snapshot(
                        snapshot,
                        super::taskbar_monitor_key(settings, tool),
                    )
                    .unwrap()
                    .key
                    .clone()
                })
                .collect();
        }
    }

    #[cfg(windows)]
    #[test]
    fn g3207_partial_snapshot_never_relocates_five_bars_or_changes_saved_keys() {
        for missing_samples in [1, 2] {
            let complete = g3207_taskbar_snapshot();
            let mut partial = g3207_taskbar_snapshot();
            partial.taskbars.pop();
            let mut settings = Settings {
                taskbar_layout_memory_on: false,
                ..Settings::default()
            };
            for tool in super::TASKBAR_TOOLS {
                super::set_taskbar_target(&mut settings, tool, "monitor-path:secondary", 0.5);
            }
            let saved = serde_json::to_value(&settings).unwrap();
            let mut observer = super::TaskbarTopologyStability::default();
            let mut targets = Vec::new();
            for _ in 0..3 {
                g3207_observe_native_targets(&mut observer, &complete, &settings, &mut targets);
            }
            let original = vec!["monitor-path:secondary".to_string(); 5];
            assert_eq!(targets, original);
            // The raw selector really falls back; the observation must not reach native apply.
            assert_eq!(
                super::taskbar_from_snapshot(&partial, "monitor-path:secondary")
                    .unwrap()
                    .key,
                "monitor-path:primary"
            );
            for _ in 0..missing_samples {
                g3207_observe_native_targets(&mut observer, &partial, &settings, &mut targets);
                assert_eq!(
                    targets, original,
                    "unconfirmed partial snapshot moved every bar"
                );
            }
            for _ in 0..3 {
                g3207_observe_native_targets(&mut observer, &complete, &settings, &mut targets);
                assert_eq!(targets, original);
            }
            assert_eq!(serde_json::to_value(&settings).unwrap(), saved);
        }
    }

    #[cfg(windows)]
    #[test]
    fn g3207_temporary_monitor_path_lookup_failure_keeps_native_targets() {
        let complete = g3207_taskbar_snapshot();
        let mut aliases = g3207_taskbar_snapshot();
        for taskbar in &mut aliases.taskbars {
            taskbar.key = taskbar.device_key.clone();
        }
        let mut settings = Settings::default();
        for tool in super::TASKBAR_TOOLS {
            super::set_taskbar_target(&mut settings, tool, "monitor-path:secondary", 0.5);
        }
        let mut observer = super::TaskbarTopologyStability::default();
        let mut targets = Vec::new();
        for _ in 0..3 {
            g3207_observe_native_targets(&mut observer, &complete, &settings, &mut targets);
        }
        let original = targets.clone();
        for _ in 0..2 {
            g3207_observe_native_targets(&mut observer, &aliases, &settings, &mut targets);
            assert_eq!(targets, original, "temporary key fallback moved every bar");
        }
        for _ in 0..3 {
            g3207_observe_native_targets(&mut observer, &complete, &settings, &mut targets);
            assert_eq!(targets, original);
        }
    }

    #[cfg(windows)]
    #[test]
    fn g3207_confirmed_monitor_disconnect_retains_primary_fallback() {
        let complete = g3207_taskbar_snapshot();
        let mut disconnected = g3207_taskbar_snapshot();
        disconnected.taskbars.pop();
        let mut settings = Settings::default();
        super::set_taskbar_target(&mut settings, "claude", "monitor-path:secondary", 0.5);
        let mut observer = super::TaskbarTopologyStability::default();
        let mut targets = Vec::new();
        for _ in 0..3 {
            g3207_observe_native_targets(&mut observer, &complete, &settings, &mut targets);
        }
        let original = targets.clone();
        for _ in 0..2 {
            g3207_observe_native_targets(&mut observer, &disconnected, &settings, &mut targets);
            assert_eq!(targets, original);
        }
        g3207_observe_native_targets(&mut observer, &disconnected, &settings, &mut targets);
        assert_eq!(targets, vec!["monitor-path:primary".to_string(); 5]);
        assert_eq!(
            settings.claude_taskbar_monitor_key,
            "monitor-path:secondary"
        );
    }

    #[cfg(windows)]
    #[test]
    fn g3207_start_menu_cover_reapplies_same_monitor_without_waiting_again() {
        let snapshot = g3207_taskbar_snapshot();
        let topology = super::taskbar_profile_topology_keys(&snapshot);
        let mut observer = super::TaskbarTopologyStability::default();
        for _ in 0..3 {
            observer.observe(topology.clone());
        }
        assert!(super::taskbar_observation_requires_reapply(
            true, true, true, true
        ));
        assert_eq!(observer.observe(topology.clone()), None);
        assert!(observer.allows_native_layout(&topology));
        for tool in super::TASKBAR_TOOLS {
            let mut settings = Settings::default();
            super::set_taskbar_target(&mut settings, tool, "monitor-path:secondary", 0.5);
            assert_eq!(
                super::taskbar_from_snapshot(
                    &snapshot,
                    super::taskbar_monitor_key(&settings, tool)
                )
                .unwrap()
                .key,
                "monitor-path:secondary"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn g3207_ipc_layout_cannot_use_unconfirmed_monitor_fallback() {
        let complete = g3207_taskbar_snapshot();
        let mut partial = g3207_taskbar_snapshot();
        partial.taskbars.pop();
        assert!(
            super::taskbar_from_snapshot_for_layout(&partial, "monitor-path:secondary", false)
                .is_err()
        );
        assert_eq!(
            super::taskbar_from_snapshot_for_layout(&partial, "monitor-path:secondary", true)
                .unwrap()
                .key,
            "monitor-path:primary"
        );
        assert_eq!(
            super::taskbar_from_snapshot_for_layout(&complete, "monitor-path:secondary", false)
                .unwrap()
                .key,
            "monitor-path:secondary"
        );
        assert_eq!(
            super::taskbar_from_snapshot_for_layout(&complete, "device:secondary", false)
                .unwrap()
                .key,
            "monitor-path:secondary"
        );
        assert_eq!(
            super::taskbar_from_snapshot_for_layout(&complete, "monitor:3440,0,2560,1440", false)
                .unwrap()
                .key,
            "monitor-path:secondary"
        );
        assert_eq!(
            super::taskbar_from_snapshot_for_layout(&complete, "", false)
                .unwrap()
                .key,
            "monitor-path:primary"
        );
    }

    #[cfg(windows)]
    #[test]
    fn g3207_invalid_topology_rearms_native_placement_guard() {
        let snapshot = g3207_taskbar_snapshot();
        let topology = super::taskbar_profile_topology_keys(&snapshot);
        let mut observer = super::TaskbarTopologyStability::default();
        assert!(!observer.allows_native_layout(&topology));
        for _ in 0..3 {
            observer.observe(topology.clone());
        }
        assert!(observer.allows_native_layout(&topology));
        observer.observe(Default::default());
        assert!(!observer.allows_native_layout(&topology));
        for _ in 0..2 {
            observer.observe(topology.clone());
            assert!(!observer.allows_native_layout(&topology));
        }
        observer.observe(topology.clone());
        assert!(observer.allows_native_layout(&topology));
    }

    #[cfg(windows)]
    #[test]
    fn g3207_fallback_commit_rejects_invalidated_or_replaced_publication() {
        let snapshot = g3207_taskbar_snapshot();
        let expected = super::taskbar_profile_topology_keys(&snapshot);
        let mut disconnected = g3207_taskbar_snapshot();
        disconnected.taskbars.pop();
        for current in [
            Default::default(),
            super::taskbar_profile_topology_keys(&disconnected),
        ] {
            let gate = std::sync::Mutex::new(());
            let mut applied = false;
            assert!(super::with_confirmed_taskbar_fallback(
                &gate,
                &expected,
                || current,
                || {
                    applied = true;
                    Ok(())
                }
            )
            .is_err());
            assert!(!applied);
        }
    }

    #[cfg(windows)]
    #[test]
    fn g3207_fallback_commit_keeps_publication_gate_until_native_actions_finish() {
        let snapshot = g3207_taskbar_snapshot();
        let expected = super::taskbar_profile_topology_keys(&snapshot);
        let gate = std::sync::Mutex::new(());
        super::with_confirmed_taskbar_fallback(
            &gate,
            &expected,
            || {
                assert!(matches!(
                    gate.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
                expected.clone()
            },
            || {
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            assert!(matches!(
                                gate.try_lock(),
                                Err(std::sync::TryLockError::WouldBlock)
                            ));
                        })
                        .join()
                        .unwrap();
                });
                Ok(())
            },
        )
        .unwrap();
        assert!(gate.try_lock().is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn g3207_fallback_commit_profile_contention_fails_without_waiting() {
        let snapshot = g3207_taskbar_snapshot();
        let topology = super::taskbar_profile_topology_keys(&snapshot);
        let gate = std::sync::Mutex::new(());
        let _held = gate.lock().unwrap();
        assert!(super::with_confirmed_taskbar_fallback(
            &gate,
            &topology,
            || panic!("must not inspect topology while publication is busy"),
            || panic!("must not commit while publication is busy")
        )
        .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn g3207_start_button_outside_bar_does_not_begin_drag_at_mixed_dpi() {
        use crate::taskbar::DockRect;
        for x in [-2560, 3440] {
            for dpi in [96, 144, 192] {
                let taskbar = DockRect {
                    x,
                    y: 1392,
                    width: 2560,
                    height: 48,
                };
                let length = super::taskbar_physical_length(260, dpi);
                let bar = crate::taskbar::dock_rect_for_taskbar_at_offset(
                    x,
                    1392,
                    x + 2560,
                    1440,
                    length,
                    0.5,
                )
                .unwrap();
                assert!(!super::taskbar_drag_candidate_precheck(
                    (taskbar.x + 60, 1416),
                    bar,
                    false,
                    false,
                    true,
                    true
                ));
                assert!(super::taskbar_drag_candidate_precheck(
                    (bar.x + length / 2, 1416),
                    bar,
                    false,
                    false,
                    true,
                    true
                ));
            }
        }
    }

    #[test]
    fn taskbar_layout_contention_fails_without_waiting() {
        let gate = std::sync::Mutex::new(());
        let held = gate.lock().unwrap();

        let error = super::try_taskbar_layout_gate(&gate).unwrap_err();
        assert_eq!(
            error.to_string(),
            "taskbar layout update already in progress"
        );

        drop(held);
        assert!(super::try_taskbar_layout_gate(&gate).is_ok());
    }

    #[test]
    fn side_effect_retry_exhaustion_never_clears_a_newer_request() {
        let pending = std::sync::atomic::AtomicBool::new(true);
        let requests = std::sync::atomic::AtomicU64::new(1);
        assert!(super::clear_exhausted_retry(&pending, &requests, Some(1)));
        assert!(!pending.load(std::sync::atomic::Ordering::Acquire));

        pending.store(true, std::sync::atomic::Ordering::Release);
        requests.store(2, std::sync::atomic::Ordering::Release);
        assert!(!super::clear_exhausted_retry(&pending, &requests, Some(1)));
        assert!(pending.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn force_refresh_admission_allows_only_one_blocking_waiter() {
        let first = super::try_begin_force_refresh().expect("first refresh must be admitted");
        assert!(super::try_begin_force_refresh().is_none());
        drop(first);
        assert!(super::try_begin_force_refresh().is_some());
    }

    #[test]
    fn disabled_tools_ignore_normal_and_forced_collection() {
        let settings = Settings {
            show_claude: false,
            show_codex: false,
            claude_account_auto_collect_on: true,
            ..Settings::default()
        };
        assert_eq!(
            super::collection_plan(&settings, true, true, true, true),
            super::CollectionPlan {
                claude_status: false,
                claude_account: false,
                codex: false,
                grok: false,
                cursor: false,
                force_claude_account: false,
                force_codex_account: false,
                force_grok_billing: false,
                force_cursor_usage: false,
            }
        );

        let settings = Settings {
            show_claude: true,
            show_codex: false,
            claude_account_auto_collect_on: false,
            ..Settings::default()
        };
        assert_eq!(
            super::collection_plan(&settings, false, false, false, false),
            super::CollectionPlan {
                claude_status: true,
                claude_account: false,
                codex: false,
                grok: false,
                cursor: false,
                force_claude_account: false,
                force_codex_account: false,
                force_grok_billing: false,
                force_cursor_usage: false,
            }
        );
        assert!(super::collection_plan(&settings, false, true, false, false).claude_account);

        let grok = Settings {
            show_claude: false,
            show_codex: false,
            show_grok: true,
            ..Settings::default()
        };
        let plan = super::collection_plan(&grok, false, false, true, false);
        assert!(plan.grok);
        assert!(plan.force_grok_billing);

        let cursor = Settings {
            show_claude: false,
            show_codex: false,
            show_cursor: true,
            ..Settings::default()
        };
        let plan = super::collection_plan(&cursor, false, false, false, true);
        assert!(plan.cursor);
        assert!(plan.force_cursor_usage);
    }

    #[test]
    fn three_tool_collection_reserves_a_final_grok_budget() {
        let settings = Settings {
            show_claude: true,
            show_codex: true,
            show_grok: true,
            claude_account_auto_collect_on: true,
            ..Settings::default()
        };
        let plan = super::collection_plan(&settings, false, false, false, false);
        let final_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let deadlines = super::collection_deadlines(plan, final_deadline);

        assert_eq!(deadlines.codex_reserve, std::time::Duration::from_secs(4));
        assert!(
            final_deadline.saturating_duration_since(deadlines.claude)
                >= std::time::Duration::from_secs(2)
        );
        assert!(
            final_deadline.saturating_duration_since(deadlines.rollout)
                >= std::time::Duration::from_secs(9)
        );
    }

    #[test]
    fn cursor_lane_preserves_existing_provider_deadlines() {
        let settings = Settings {
            show_cursor: true,
            show_grok: true,
            ..Settings::default()
        };
        let plan = super::collection_plan(&settings, false, false, false, false);
        let final_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let with_cursor = super::collection_deadlines(plan, final_deadline);
        let without_cursor = super::collection_deadlines(
            super::CollectionPlan {
                cursor: false,
                force_cursor_usage: false,
                ..plan
            },
            final_deadline,
        );

        assert_eq!(with_cursor, without_cursor);
        assert_eq!(with_cursor.codex_reserve, std::time::Duration::from_secs(4));
        assert_eq!(with_cursor.grok, final_deadline);
        assert_eq!(super::COLLECTION_REFRESH_DEADLINE_SECS, 15);
        assert_eq!(super::CURSOR_USAGE_TIMEOUT_SECS, 20);
    }

    #[test]
    fn cursor_freshness_never_expires_before_its_five_minute_cache() {
        let settings = Settings {
            stale_after_secs: 90,
            ..Settings::default()
        };
        assert_eq!(super::cursor_stale_after_secs(&settings), 330);

        let customized = Settings {
            stale_after_secs: 600,
            ..Settings::default()
        };
        assert_eq!(super::cursor_stale_after_secs(&customized), 600);
    }

    #[test]
    fn cursor_first_observed_account_survives_all_failed_collection_paths() {
        use super::CollectionErrorKind as E;
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            team_id: Some(12),
        };
        let settings = Settings::default();
        let now = chrono::Utc::now();
        for initial in 0..3 {
            for error in [
                E::Deadline,
                E::Transport,
                E::Parse,
                E::LoginRequired,
                E::SourceChanged,
            ] {
                let cache = std::sync::Mutex::new(None);
                match initial {
                    0 => {}
                    1 => {
                        super::cache_cursor_status(
                            &cache,
                            now,
                            Some(status_for_signature("legacy-a")),
                            Some(super::CursorStatusSource::Agent(None)),
                            None,
                            &settings,
                        );
                    }
                    _ => {
                        super::cache_cursor_status(
                            &cache,
                            now,
                            None,
                            None,
                            Some(E::Unavailable),
                            &settings,
                        );
                    }
                }
                super::remember_cursor_account_scope(&cache, now, scope);
                assert!(cache.lock().unwrap().as_ref().unwrap().last_good.is_none());
                super::cache_cursor_status(&cache, now, None, None, Some(error), &settings);
                let known = cache.lock().unwrap().as_ref().unwrap().known_scope;
                assert_eq!(known, Some(scope));
                let result = super::resolve_cursor_dashboard_first(
                    Err((E::Unavailable, None)),
                    || panic!("observed credentials must block legacy fallback after failure"),
                    || panic!("blocked fallback must not read credentials again"),
                    known,
                );
                assert!(matches!(result, Err(E::SourceChanged)));
            }
        }
    }

    #[test]
    fn cursor_observed_account_preserves_known_identity_until_verified_success() {
        let a = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let b = crate::cursor_dashboard::AccountScope { user_id: 8, ..a };
        let now = chrono::Utc::now();
        let settings = Settings::default();
        for source in [
            super::CursorStatusSource::Dashboard(a),
            super::CursorStatusSource::Agent(Some(a)),
        ] {
            let cache = std::sync::Mutex::new(None);
            super::cache_cursor_status(
                &cache,
                now,
                Some(status_for_signature("account-a")),
                Some(source),
                None,
                &settings,
            );
            super::remember_cursor_account_scope(&cache, now, a);
            assert_eq!(
                cache
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .last_good
                    .as_ref()
                    .unwrap()
                    .session_id,
                "account-a"
            );
            super::remember_cursor_account_scope(&cache, now, b);
            {
                let cached = cache.lock().unwrap();
                let cached = cached.as_ref().unwrap();
                assert_eq!(cached.known_scope, Some(a));
                assert!(cached.last_good.is_none() && cached.source.is_none());
            }
            super::cache_cursor_status(
                &cache,
                now,
                Some(status_for_signature("account-b")),
                Some(super::CursorStatusSource::Dashboard(b)),
                None,
                &settings,
            );
            assert_eq!(cache.lock().unwrap().as_ref().unwrap().known_scope, Some(b));
        }
    }

    #[test]
    fn cursor_unscoped_cache_yields_to_new_credentials_and_blocks_legacy_revival() {
        use super::CollectionErrorKind as E;
        let cache = std::sync::Mutex::new(None);
        let settings = Settings::default();
        let now = chrono::Utc::now();
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            team_id: Some(12),
        };
        super::cache_cursor_status(
            &cache,
            now,
            Some(status_for_signature("legacy-account-a")),
            Some(super::CursorStatusSource::Agent(None)),
            None,
            &settings,
        );
        let mut cached = cache.lock().unwrap().clone().unwrap();
        assert!(!super::revalidate_cursor_cached_source(
            &mut cached,
            Ok(scope)
        ));
        assert!(cached.last_good.is_none() && cached.source.is_none());
        assert_eq!(cached.error, Some(E::SourceChanged));
        assert_eq!(cached.known_scope, Some(scope));
        for error in [E::Unavailable, E::LoginRequired, E::Deadline, E::Parse] {
            let result = super::resolve_cursor_dashboard_first(
                Err((error.clone(), None)),
                || panic!("new credentials must block unverified legacy fallback"),
                || panic!("blocked fallback must not read credentials again"),
                cached.known_scope,
            );
            let expected = if error == E::LoginRequired {
                E::LoginRequired
            } else {
                E::SourceChanged
            };
            assert!(matches!(result, Err(value) if value == expected));
        }
    }

    #[test]
    fn cursor_unscoped_cache_remains_valid_without_available_credentials() {
        use super::CollectionErrorKind as E;
        let settings = Settings::default();
        let now = chrono::Utc::now();
        for error in [E::Unavailable, E::LoginRequired] {
            let cache = std::sync::Mutex::new(None);
            super::cache_cursor_status(
                &cache,
                now,
                Some(status_for_signature("cli-only")),
                Some(super::CursorStatusSource::Agent(None)),
                None,
                &settings,
            );
            let mut cached = cache.lock().unwrap().clone().unwrap();
            assert!(super::revalidate_cursor_cached_source(
                &mut cached,
                Err(error)
            ));
            assert_eq!(cached.last_good.unwrap().session_id, "cli-only");
            assert!(matches!(
                cached.source,
                Some(super::CursorStatusSource::Agent(None))
            ));
            assert!(cached.known_scope.is_none() && cached.error.is_none());
        }
    }

    #[test]
    fn cursor_unscoped_cache_does_not_treat_uncertain_credentials_as_absent() {
        use super::CollectionErrorKind as E;
        for error in [E::Deadline, E::Parse, E::Transport, E::SourceChanged] {
            let cache = std::sync::Mutex::new(None);
            super::cache_cursor_status(
                &cache,
                chrono::Utc::now(),
                Some(status_for_signature("cli-only")),
                Some(super::CursorStatusSource::Agent(None)),
                None,
                &Settings::default(),
            );
            let mut cached = cache.lock().unwrap().clone().unwrap();
            assert!(!super::revalidate_cursor_cached_source(
                &mut cached,
                Err(error)
            ));
            assert!(cached.last_good.is_none() && cached.source.is_none());
            assert_eq!(cached.error, Some(E::SourceChanged));
        }
    }

    #[test]
    fn cursor_scoped_cache_revalidation_preserves_existing_identity_protection() {
        let a = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let b = crate::cursor_dashboard::AccountScope { user_id: 8, ..a };
        for source in [
            super::CursorStatusSource::Dashboard(a),
            super::CursorStatusSource::Agent(Some(a)),
        ] {
            let cache = std::sync::Mutex::new(None);
            super::cache_cursor_status(
                &cache,
                chrono::Utc::now(),
                Some(status_for_signature("account-a")),
                Some(source),
                None,
                &Settings::default(),
            );
            let mut cached = cache.lock().unwrap().clone().unwrap();
            assert!(super::revalidate_cursor_cached_source(&mut cached, Ok(a)));
            assert_eq!(cached.last_good.as_ref().unwrap().session_id, "account-a");
            assert!(!super::revalidate_cursor_cached_source(&mut cached, Ok(b)));
            assert!(cached.last_good.is_none() && cached.source.is_none());
            assert_eq!(cached.known_scope, Some(a));
        }
    }

    #[test]
    fn cursor_dashboard_success_never_starts_the_agent_fallback() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = AtomicUsize::new(0);
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let result = super::resolve_cursor_dashboard_first(
            Ok((status_for_signature("dashboard"), scope)),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(status_for_signature("agent"))
            },
            || panic!("successful Dashboard response already validated"),
            None,
        )
        .unwrap();
        assert!(matches!(
            result.1,
            super::CursorStatusSource::Dashboard(value) if value == scope
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let transport = super::resolve_cursor_dashboard_first(
            Err((super::CollectionErrorKind::Transport, Some(scope))),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(status_for_signature("must-not-run"))
            },
            || panic!("transport error must not start a fallback"),
            None,
        );
        assert!(matches!(
            transport,
            Err(super::CollectionErrorKind::Transport)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let login = super::resolve_cursor_dashboard_first(
            Err((super::CollectionErrorKind::LoginRequired, None)),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(status_for_signature("agent"))
            },
            || panic!("legacy fallback has no Dashboard scope to revalidate"),
            None,
        )
        .unwrap();
        assert!(matches!(login.1, super::CursorStatusSource::Agent(None)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cursor_response_identity_changes_clear_cache_and_block_fallback() {
        use super::CollectionErrorKind as E;
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: Some(11),
        };
        let changed_user = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            ..scope
        };
        let changed_team = crate::cursor_dashboard::AccountScope {
            team_id: Some(12),
            ..scope
        };
        let settings = Settings::default();
        let now = chrono::Utc::now();
        for current in [
            Ok(changed_user),
            Ok(changed_team),
            Err(E::LoginRequired),
            Err(E::Unavailable),
            Err(E::Deadline),
            Err(E::Parse),
        ] {
            for response in [
                Ok(status_for_signature("old-account-response")),
                Err(E::Transport),
                Err(E::Parse),
                Err(E::LoginRequired),
            ] {
                let cache = std::sync::Mutex::new(None);
                assert!(super::cache_cursor_status(
                    &cache,
                    now,
                    Some(status_for_signature("old-account-cached")),
                    Some(super::CursorStatusSource::Dashboard(scope)),
                    None,
                    &settings,
                )
                .is_some());
                let expected_error = if current == Err(E::LoginRequired) {
                    E::LoginRequired
                } else {
                    E::SourceChanged
                };
                let checked =
                    super::validate_cursor_dashboard_response(scope, response, || current.clone())
                        .map(|status| (status, scope));
                let result = super::resolve_cursor_dashboard_first(
                    checked,
                    || panic!("changed or signed-out account must not start legacy fallback"),
                    || panic!("blocked fallback must not revalidate again"),
                    Some(scope),
                );
                let error = result.err().unwrap();
                assert_eq!(error, expected_error);
                assert!(super::cache_cursor_status(
                    &cache,
                    now,
                    None,
                    None,
                    Some(error),
                    &settings,
                )
                .is_none());
                let guard = cache.lock().unwrap();
                let cached = guard.as_ref().unwrap();
                assert!(cached.last_good.is_none());
                assert!(cached.source.is_none());
                assert_eq!(cached.error, Some(expected_error));
            }
        }
        assert_eq!(
            super::dashboard_collection_error(
                crate::cursor_dashboard::DashboardErrorKind::ScopeChanged
            ),
            E::SourceChanged
        );
    }

    #[test]
    fn cursor_cli_fallback_cannot_label_another_account_with_gui_scope() {
        use super::CollectionErrorKind as E;
        let gui = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            team_id: None,
        };
        let cli = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let mut captures = 0;
        let result = super::resolve_cursor_dashboard_first(
            Err((E::Parse, Some(gui))),
            || {
                super::ensure_cursor_cli_scope(gui, cli)?;
                captures += 1;
                Ok(status_for_signature("cli-account-a"))
            },
            || Ok(gui),
            Some(gui),
        );
        assert!(matches!(result, Err(E::SourceChanged)));
        assert_eq!(captures, 0);
        assert!(super::ensure_cursor_cli_scope(gui, gui).is_ok());
        assert!(matches!(
            super::ensure_cursor_cli_scope(
                gui,
                crate::cursor_dashboard::AccountScope {
                    team_id: Some(42),
                    ..gui
                }
            ),
            Err(E::SourceChanged)
        ));
    }

    #[test]
    fn cursor_known_scope_survives_repeated_failed_queries_and_recovers() {
        use super::CollectionErrorKind as E;
        let cache = std::sync::Mutex::new(None);
        let settings = Settings::default();
        let now = chrono::Utc::now();
        let a = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: Some(11),
        };
        let b = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            team_id: None,
        };
        assert!(super::cache_cursor_status(
            &cache,
            now,
            Some(status_for_signature("account-a")),
            Some(super::CursorStatusSource::Dashboard(a)),
            None,
            &settings
        )
        .is_some());
        for error in [E::SourceChanged, E::LoginRequired, E::Unavailable, E::Parse] {
            assert!(
                super::cache_cursor_status(&cache, now, None, None, Some(error), &settings)
                    .is_none()
            );
            let prior = cache.lock().unwrap().as_ref().unwrap().known_scope;
            assert_eq!(prior, Some(a));
            for elapsed in [1, 3600] {
                let result = super::resolve_cursor_dashboard_first(
                    Err((E::Unavailable, None)),
                    || panic!("failed known credentials must not enable unverified fallback"),
                    || panic!("no fallback should require this reader"),
                    prior,
                );
                assert!(matches!(result, Err(E::SourceChanged)));
                super::cache_cursor_status(
                    &cache,
                    now + chrono::Duration::seconds(elapsed),
                    None,
                    None,
                    Some(E::SourceChanged),
                    &settings,
                );
                let cached = cache.lock().unwrap();
                let cached = cached.as_ref().unwrap();
                assert_eq!(cached.known_scope, Some(a));
                assert!(cached.last_good.is_none() && cached.source.is_none());
            }
        }
        let (status, source) = super::resolve_cursor_dashboard_first(
            Ok((status_for_signature("account-b"), b)),
            || panic!("verified Dashboard should not fallback"),
            || panic!("response was already checked"),
            Some(a),
        )
        .unwrap();
        let fresh =
            super::cache_cursor_status(&cache, now, Some(status), Some(source), None, &settings)
                .unwrap();
        assert_eq!(fresh.session_id, "account-b");
        assert_eq!(cache.lock().unwrap().as_ref().unwrap().known_scope, Some(b));
        super::cache_cursor_status(&cache, now, None, None, Some(E::LoginRequired), &settings);
        assert_eq!(cache.lock().unwrap().as_ref().unwrap().known_scope, Some(b));
    }

    #[test]
    fn cursor_first_cli_only_fallback_remains_available_without_known_scope() {
        use super::CollectionErrorKind as E;
        let cache = std::sync::Mutex::new(None);
        let now = chrono::Utc::now();
        let settings = Settings::default();
        super::cache_cursor_status(&cache, now, None, None, Some(E::Unavailable), &settings);
        let prior = cache.lock().unwrap().as_ref().unwrap().known_scope;
        assert_eq!(prior, None);
        let (status, source) = super::resolve_cursor_dashboard_first(
            Err((E::Unavailable, None)),
            || Ok(status_for_signature("cli-only")),
            || panic!("legacy-only has no identity reader"),
            prior,
        )
        .unwrap();
        assert!(super::cache_cursor_status(
            &cache,
            now,
            Some(status),
            Some(source),
            None,
            &settings
        )
        .is_some());
        assert_eq!(cache.lock().unwrap().as_ref().unwrap().known_scope, None);
    }

    #[test]
    fn cursor_revalidation_uses_remaining_outer_budget_after_http_timeout() {
        use super::CollectionErrorKind as E;
        let now = std::time::Instant::now();
        let expired_http = now - std::time::Duration::from_millis(1);
        let outer = now + std::time::Duration::from_secs(10);
        let check = super::cursor_scope_revalidation_deadline(outer, now);
        assert!(expired_http < now && check > now);
        assert_eq!(check, now + std::time::Duration::from_secs(1));
        let short_outer = now + std::time::Duration::from_millis(200);
        assert_eq!(
            super::cursor_scope_revalidation_deadline(short_outer, now),
            short_outer
        );
        assert_eq!(
            super::cursor_scope_revalidation_deadline(expired_http, now),
            expired_http
        );
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let result = super::validate_cursor_dashboard_response::<AgentStatus>(
            scope,
            Err(E::Deadline),
            || {
                assert!(check > now);
                Ok(scope)
            },
        );
        assert!(matches!(result, Err((E::Deadline, Some(actual))) if actual == scope));
        let changed = super::validate_cursor_dashboard_response::<AgentStatus>(
            scope,
            Err(E::Deadline),
            || {
                Ok(crate::cursor_dashboard::AccountScope {
                    user_id: 8,
                    ..scope
                })
            },
        );
        assert!(matches!(changed, Err((E::SourceChanged, _))));
    }

    #[test]
    fn cursor_response_revalidation_preserves_same_scope_success_and_errors() {
        use super::CollectionErrorKind as E;
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let mut calls = 0;
        let good = super::validate_cursor_dashboard_response(
            scope,
            Ok(status_for_signature("same-account")),
            || {
                calls += 1;
                Ok(scope)
            },
        )
        .unwrap();
        assert_eq!(good.session_id, "same-account");
        assert_eq!(calls, 1);
        for error in [E::Transport, E::Deadline, E::Parse, E::LoginRequired] {
            let result = super::validate_cursor_dashboard_response::<AgentStatus>(
                scope,
                Err(error.clone()),
                || Ok(scope),
            );
            assert!(matches!(result, Err((actual, Some(actual_scope)))
                if actual == error && actual_scope == scope));
        }
        let result = super::resolve_cursor_dashboard_first(
            Err((E::LoginRequired, Some(scope))),
            || panic!("401 for a known account must not resurrect a legacy account"),
            || panic!("known-account auth failure blocks fallback"),
            Some(scope),
        );
        assert!(matches!(result, Err(E::LoginRequired)));
        for error in [E::LoginRequired, E::Unavailable, E::Parse, E::Deadline] {
            let result = super::resolve_cursor_dashboard_first(
                Err((error.clone(), None)),
                || panic!("lost known credentials must not resurrect a legacy account"),
                || panic!("lost known credentials block fallback"),
                Some(scope),
            );
            let expected = if error == E::LoginRequired {
                E::LoginRequired
            } else {
                E::SourceChanged
            };
            assert!(matches!(result, Err(actual) if actual == expected));
        }
    }

    #[test]
    fn cursor_fallback_revalidates_known_scope_before_returning_values() {
        use super::CollectionErrorKind as E;
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let other = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            ..scope
        };
        for after in [
            Ok(scope),
            Ok(other),
            Err(E::LoginRequired),
            Err(E::Unavailable),
        ] {
            let current = std::cell::RefCell::new(Ok(scope));
            let mut calls = 0;
            let result = super::resolve_cursor_dashboard_first(
                Err((E::Parse, Some(scope))),
                || {
                    calls += 1;
                    *current.borrow_mut() = after.clone();
                    Ok(status_for_signature("legacy-account"))
                },
                || current.borrow().clone(),
                Some(scope),
            );
            assert_eq!(calls, 1);
            match after {
                Ok(value) if value == scope => {
                    let (status, source) = result.unwrap();
                    assert_eq!(status.session_id, "legacy-account");
                    assert!(
                        matches!(source, super::CursorStatusSource::Agent(Some(value))
                        if value == scope)
                    );
                }
                Err(E::LoginRequired) => assert!(matches!(result, Err(E::LoginRequired))),
                _ => assert!(matches!(result, Err(E::SourceChanged))),
            }
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "uses the locally logged-in Cursor GUI account or Agent fallback"]
    fn live_cursor_provider_collects_two_monthly_pools_without_a_prompt() {
        *super::CURSOR_USAGE_CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let settings = Settings {
            show_claude: false,
            show_codex: false,
            show_grok: false,
            show_cursor: true,
            ..Settings::default()
        };
        let statuses =
            super::collect_representatives_with_options(&settings, false, false, false, true);
        assert_eq!(statuses.len(), 1);
        let status = &statuses[0];
        assert_eq!(status.tool, Tool::Cursor);
        assert_eq!(status.primary.as_ref().unwrap().label, "cursor_models");
        assert_eq!(status.secondary.as_ref().unwrap().label, "other_models");
        assert_eq!(
            super::collection_health_snapshot().cursor,
            super::CollectionHealth::Ready
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "uses the locally logged-in Cursor GUI account"]
    fn live_cursor_gui_fallback_works_without_a_cursor_agent_runtime() {
        let isolated = std::env::temp_dir().join(format!(
            "agent-juice-cursor-gui-fallback-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&isolated).unwrap();
        let environment = EnvVarGuard::set("LOCALAPPDATA", isolated.as_os_str());
        *super::CURSOR_USAGE_CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let settings = Settings {
            show_cursor: true,
            ..Settings::default()
        };
        let status = super::collect_cursor_usage_status(
            &settings,
            "LOCAL",
            chrono::Utc::now(),
            true,
            std::time::Instant::now() + std::time::Duration::from_secs(8),
        )
        .unwrap();
        drop(environment);
        std::fs::remove_dir_all(isolated).unwrap();
        assert_eq!(status.session_id, "cursor-gui-usage");
        assert_eq!(status.tool, Tool::Cursor);
    }

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
        assert_eq!(super::taskbar_bar_label("grok"), Some("bar-grok"));
        assert_eq!(super::taskbar_bar_label("cursor"), Some("bar-cursor"));
        assert!(super::should_show_taskbar_bar(&settings, "claude"));
        assert!(super::should_show_taskbar_bar(&settings, "codex"));
        assert!(!super::should_show_taskbar_bar(&settings, "grok"));
        assert!(!super::should_show_taskbar_bar(&settings, "cursor"));
        assert!(super::should_show_taskbar_bar_with_fullscreen(
            &settings, "claude", true
        ));
        settings.fullscreen_hide_on = true;
        assert!(!super::should_show_taskbar_bar_with_fullscreen(
            &settings, "claude", true
        ));
        assert!(!super::should_show_taskbar_bar_with_fullscreen(
            &settings, "codex", true
        ));
        assert!(super::should_show_taskbar_bar_with_window_state(
            &settings, "codex", false, true
        ));
        settings.taskbar_bars_paused = true;
        assert!(!super::should_show_taskbar_bar_with_pause(
            &settings,
            "codex",
            false,
            false,
            settings.taskbar_bars_paused,
        ));
        settings.taskbar_bars_paused = false;
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
        settings.show_grok = true;
        assert_eq!(super::taskbar_dock_width(&settings, "grok"), Some(45));
        settings.show_cursor = true;
        assert_eq!(super::taskbar_dock_width(&settings, "cursor"), Some(96));
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
    fn measured_taskbar_content_width_is_scoped_to_its_mode() {
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
        let dual_layout = super::TaskbarContentLayout {
            mode: "dual".into(),
            width: 92,
            ratio: Some(0.42),
        };
        assert_eq!(
            super::taskbar_content_width_for_mode("dual", Some(&dual_layout)),
            Some(92)
        );
        assert_eq!(super::taskbar_content_width_for_mode("full", None), None);
    }

    #[test]
    fn content_width_retries_after_target_initialization_and_stale_hwnd_size() {
        use super::TaskbarContentWidthDecision::{AlreadyApplied, Apply, RetryAfterTarget};

        assert_eq!(
            super::taskbar_content_width_decision(false, false, false),
            RetryAfterTarget
        );
        assert_eq!(
            super::taskbar_content_width_decision(true, true, false),
            Apply
        );
        assert_eq!(
            super::taskbar_content_width_decision(true, true, true),
            AlreadyApplied
        );
        assert_eq!(
            super::taskbar_content_width_decision(true, false, true),
            Apply
        );
    }

    #[test]
    fn restored_profile_ratio_replaces_the_cached_content_layout_ratio() {
        let mut layout = Some(super::TaskbarContentLayout {
            mode: "full".into(),
            width: 188,
            ratio: Some(0.75),
        });

        super::update_taskbar_content_layout_ratio(&mut layout, 0.2);
        assert_eq!(layout.as_ref().unwrap().ratio, Some(0.2));
        super::update_taskbar_content_layout_ratio(&mut layout, 2.0);
        assert_eq!(layout.as_ref().unwrap().ratio, Some(1.0));
    }

    #[test]
    fn taskbar_coverage_scan_skips_paused_or_fully_disabled_bars() {
        let mut settings = Settings::default();
        assert!(!super::should_scan_taskbar_coverage(&settings, false));
        settings.fullscreen_hide_on = true;
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

        assert_eq!(super::taskbar_offset_ratio(&settings, "claude"), 0.0);
        assert_eq!(super::taskbar_offset_ratio(&settings, "codex"), 0.0);
        assert_eq!(super::taskbar_offset_ratio(&settings, "grok"), 0.0);
        assert_eq!(super::taskbar_offset_ratio(&settings, "cursor"), 0.0);

        super::set_taskbar_offset_ratio(&mut settings, "claude", 0.2);
        super::set_taskbar_offset_ratio(&mut settings, "codex", 0.8);
        super::set_taskbar_offset_ratio(&mut settings, "grok", 0.6);
        super::set_taskbar_offset_ratio(&mut settings, "cursor", 0.4);

        assert_eq!(settings.claude_taskbar_offset_ratio, 0.2);
        assert_eq!(settings.codex_taskbar_offset_ratio, 0.8);
        assert_eq!(settings.grok_taskbar_offset_ratio, 0.6);
        assert_eq!(settings.cursor_taskbar_offset_ratio, 0.4);
    }

    #[test]
    fn initial_taskbar_stack_starts_at_the_leading_edge_without_overlap() {
        let settings = Settings::default();
        let horizontal = taskbar::DockRect {
            x: 0,
            y: 1040,
            width: 1920,
            height: 40,
        };
        let ratios = super::pending_taskbar_target_ratios(
            &settings,
            horizontal,
            [Some(320), Some(280), None, None, None],
        )
        .unwrap();
        let claude =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 320, ratios[0].unwrap())
                .unwrap();
        let codex =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 280, ratios[1].unwrap())
                .unwrap();

        assert_eq!(claude.x, 0);
        assert_eq!(codex.x, claude.x + claude.width);

        let scaled = super::pending_taskbar_target_ratios(
            &settings,
            horizontal,
            [Some(480), Some(420), None, None, None],
        )
        .unwrap();
        let scaled_claude =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 480, scaled[0].unwrap())
                .unwrap();
        let scaled_codex =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 420, scaled[1].unwrap())
                .unwrap();
        assert_eq!(scaled_codex.x, scaled_claude.x + scaled_claude.width);

        let three_settings = Settings {
            show_grok: true,
            ..Settings::default()
        };
        let three = super::pending_taskbar_target_ratios(
            &three_settings,
            horizontal,
            [Some(320), Some(280), Some(240), None, None],
        )
        .unwrap();
        let grok =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 240, three[2].unwrap())
                .unwrap();
        assert_eq!(grok.x, 600);

        let four_settings = Settings {
            show_grok: true,
            show_cursor: true,
            ..Settings::default()
        };
        let four = super::pending_taskbar_target_ratios(
            &four_settings,
            horizontal,
            [Some(320), Some(280), Some(240), Some(220), None],
        )
        .unwrap();
        let cursor =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 220, four[3].unwrap())
                .unwrap();
        assert_eq!(cursor.x, 840);
        let five_settings = Settings {
            show_antigravity: true,
            ..four_settings
        };
        let five = super::pending_taskbar_target_ratios(
            &five_settings,
            horizontal,
            [Some(320), Some(280), Some(240), Some(220), Some(200)],
        )
        .unwrap();
        let antigravity =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 200, five[4].unwrap())
                .unwrap();
        assert_eq!(antigravity.x, 1060);
    }

    #[cfg(windows)]
    #[test]
    fn runtime_overlap_resolution_is_optional_and_scoped_to_one_taskbar() {
        let taskbar_rect = taskbar::DockRect {
            x: 0,
            y: 1040,
            width: 1000,
            height: 40,
        };
        let claude = taskbar::DockRect {
            x: 100,
            y: 1040,
            width: 250,
            height: 40,
        };
        let codex = taskbar::DockRect {
            x: 300,
            y: 1040,
            width: 200,
            height: 40,
        };

        assert_eq!(
            super::resolve_taskbar_position_pair(false, 1, taskbar_rect, claude, 1, codex),
            (claude, codex)
        );
        assert_eq!(
            super::resolve_taskbar_position_pair(true, 1, taskbar_rect, claude, 2, codex),
            (claude, codex)
        );
        let (_, resolved_codex) =
            super::resolve_taskbar_position_pair(true, 1, taskbar_rect, claude, 1, codex);
        assert_eq!(resolved_codex.x, 350);

        let mut three = [
            claude,
            codex,
            taskbar::DockRect {
                x: 420,
                y: 1040,
                width: 180,
                height: 40,
            },
        ];
        taskbar::resolve_taskbar_overlaps(taskbar_rect, &mut three);
        assert_eq!(three[1].x, three[0].x + three[0].width);
        assert_eq!(three[2].x, three[1].x + three[1].width);

        let mut trailing = [
            taskbar::DockRect { x: 700, ..claude },
            taskbar::DockRect { x: 750, ..codex },
            taskbar::DockRect {
                x: 800,
                y: 1040,
                width: 180,
                height: 40,
            },
        ];
        taskbar::resolve_taskbar_overlaps(taskbar_rect, &mut trailing);
        assert_eq!(trailing[0].x, 370);
        assert_eq!(trailing[1].x, 620);
        assert_eq!(trailing[2].x, 820);
    }

    #[test]
    fn initial_taskbar_stack_handles_vertical_and_single_tool_layouts() {
        let settings = Settings::default();
        let vertical = taskbar::DockRect {
            x: 0,
            y: 0,
            width: 48,
            height: 1080,
        };
        let stacked = super::pending_taskbar_target_ratios(
            &settings,
            vertical,
            [Some(220), Some(180), None, None, None],
        )
        .unwrap();
        let claude =
            taskbar::dock_rect_for_taskbar_at_offset(0, 0, 48, 1080, 220, stacked[0].unwrap())
                .unwrap();
        let codex =
            taskbar::dock_rect_for_taskbar_at_offset(0, 0, 48, 1080, 180, stacked[1].unwrap())
                .unwrap();
        let codex_only = super::pending_taskbar_target_ratios(
            &settings,
            vertical,
            [None, Some(180), None, None, None],
        )
        .unwrap();

        assert_eq!(claude.y, 0);
        assert_eq!(codex.y, claude.y + claude.height);
        assert_eq!(codex_only[0], None);
        assert_eq!(codex_only[1], Some(0.0));
    }

    #[test]
    fn initial_taskbar_stack_never_overwrites_an_existing_position() {
        let settings = Settings {
            claude_taskbar_target_initialized: true,
            codex_taskbar_target_initialized: true,
            ..Settings::default()
        };

        assert!(super::pending_taskbar_target_ratios(
            &settings,
            taskbar::DockRect {
                x: 0,
                y: 1040,
                width: 1920,
                height: 40,
            },
            [Some(320), Some(280), None, None, None],
        )
        .is_none());
    }

    #[test]
    fn enabling_a_hidden_tool_uses_the_first_gap_without_moving_the_existing_tool() {
        let taskbar_rect = taskbar::DockRect {
            x: 0,
            y: 1040,
            width: 1920,
            height: 40,
        };
        let mut settings = Settings {
            show_claude: false,
            ..Settings::default()
        };
        let codex_only = super::pending_taskbar_target_ratios(
            &settings,
            taskbar_rect,
            [None, Some(280), None, None, None],
        )
        .unwrap();
        super::set_taskbar_target(
            &mut settings,
            "codex",
            "monitor:primary",
            codex_only[1].unwrap(),
        );
        assert!(!settings.claude_taskbar_target_initialized);
        assert!(settings.codex_taskbar_target_initialized);

        settings.show_claude = true;
        let enabled = super::pending_taskbar_target_ratios(
            &settings,
            taskbar_rect,
            [Some(320), Some(280), None, None, None],
        )
        .unwrap();
        let claude =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 320, enabled[0].unwrap())
                .unwrap();
        let codex = taskbar::dock_rect_for_taskbar_at_offset(
            0,
            1040,
            1920,
            1080,
            280,
            settings.codex_taskbar_offset_ratio,
        )
        .unwrap();

        assert_eq!(codex.x, 0);
        assert_eq!(claude.x, codex.x + codex.width);
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
    fn taskbar_layout_resize_uses_the_canonical_ratio_instead_of_the_shifted_hwnd() {
        let taskbar_rect = taskbar::DockRect {
            x: 0,
            y: 1040,
            width: 1000,
            height: 40,
        };
        let canonical_ratio = 300.0 / 800.0;
        let resized_ratio = super::taskbar_ratio_preserving_layout_leading_edge(
            taskbar_rect,
            200,
            canonical_ratio,
            300,
        )
        .unwrap();
        let resized =
            taskbar::dock_rect_for_taskbar_at_offset(0, 1040, 1000, 1080, 300, resized_ratio)
                .unwrap();

        assert_eq!(resized.x, 300);
        assert_ne!(resized.x, 350);
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

        let activity_guard = super::try_begin_activity_refresh().expect("first refresh starts");
        assert!(super::try_begin_activity_refresh().is_none());
        drop(activity_guard);
        assert!(super::try_begin_activity_refresh().is_some());

        let (cursor_guard, generation) =
            super::try_begin_cursor_activity_refresh().expect("first Cursor refresh starts");
        assert!(super::try_begin_cursor_activity_refresh().is_none());
        drop(cursor_guard);
        let (_, next_generation) =
            super::try_begin_cursor_activity_refresh().expect("Cursor refresh restarts");
        assert!(next_generation > generation);
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
    fn unavailable_codex_account_preserves_rollout_approximate_limits() {
        let mut rollout = status_for_signature("rollout-fallback");
        rollout.tool = Tool::Codex;
        rollout.approx = true;
        rollout.primary = Some(AccountLimit {
            label: "5h".into(),
            used_percent: Some(18.0),
            resets_at: Some("rollout-5h-reset".into()),
        });
        rollout.secondary = Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(42.0),
            resets_at: Some("rollout-week-reset".into()),
        });

        let merged = super::merge_codex_account_status(Some(rollout), None).unwrap();

        assert_eq!(merged.session_id, "rollout-fallback");
        assert_eq!(merged.primary.unwrap().used_percent, Some(18.0));
        assert_eq!(merged.secondary.unwrap().used_percent, Some(42.0));
        assert!(merged.approx);
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
    fn claude_desktop_values_never_merge_an_unrelated_statusline_or_survive_failure() {
        use crate::model;
        use chrono::Utc;
        use std::sync::Mutex;
        let mut desktop = status_for_signature("claude-desktop-usage");
        desktop.tool = Tool::Claude;
        desktop.primary = Some(model::AccountLimit {
            label: "5h".into(),
            used_percent: Some(20.0),
            resets_at: None,
        });
        desktop.secondary = None;
        desktop.session.active = false;
        let mut statusline = desktop.clone();
        statusline.session_id = "another-account".into();
        statusline.session.active = true;
        statusline.secondary = Some(model::AccountLimit {
            label: "week".into(),
            used_percent: Some(99.0),
            resets_at: None,
        });
        statusline.cost_estimate_usd = Some(123.0);
        let merged =
            super::merge_claude_usage_status(Some(statusline), Some(desktop.clone())).unwrap();
        assert_eq!(merged.session_id, "claude-desktop-usage");
        assert!(merged.secondary.is_none());
        assert_eq!(merged.cost_estimate_usd, desktop.cost_estimate_usd);
        let cache = Mutex::new(None);
        let now = Utc::now();
        super::cached_status_attempt(&cache, now, 60, true, || Ok(desktop));
        let result = super::cached_status_attempt(&cache, now, 60, true, || {
            Err(super::CollectionErrorKind::Unavailable)
        });
        assert!(super::discard_failed_desktop_status(&cache, result).is_none());
        assert!(cache.lock().unwrap().as_ref().unwrap().last_good.is_none());
    }

    #[test]
    fn desktop_transient_failure_keeps_verified_last_good() {
        let cache = std::sync::Mutex::new(None);
        let now = chrono::Utc::now();
        let good = status_for_signature("claude-desktop-usage");
        super::cached_status_attempt(&cache, now, 60, true, || Ok(good));
        let status = super::cached_status_attempt(&cache, now, 60, true, || {
            Err(super::CollectionErrorKind::Transport)
        });
        assert!(super::discard_failed_desktop_status(&cache, status).is_some());
    }

    #[test]
    fn claude_automatic_account_queries_wait_five_minutes() {
        assert_eq!(super::CLAUDE_USAGE_CACHE_MIN_SECS, 300);
        let cache = std::sync::Mutex::new(None);
        let now = chrono::Utc::now();
        let interval = super::CLAUDE_USAGE_CACHE_MIN_SECS;
        super::cached_status_attempt(&cache, now, interval, false, || {
            Ok(status_for_signature("claude-desktop-usage"))
        });
        for seconds in [60, 120, 299] {
            assert!(super::cached_status_attempt(
                &cache,
                now + chrono::Duration::seconds(seconds),
                interval,
                false,
                || panic!("automatic query ran too soon")
            )
            .is_some());
        }
        let mut called = false;
        super::cached_status_attempt(
            &cache,
            now + chrono::Duration::seconds(300),
            interval,
            false,
            || {
                called = true;
                Ok(status_for_signature("claude-desktop-usage"))
            },
        );
        assert!(called);
        for force in [false, true] {
            assert!(!super::claude_legacy_fallback_allowed(
                &super::CollectionErrorKind::RateLimited,
                force
            ));
        }
    }

    #[test]
    fn claude_rate_limit_preserves_values_and_manual_refresh_honors_cooldown() {
        use super::CollectionErrorKind as E;
        let cache = std::sync::Mutex::new(None);
        let now = chrono::Utc::now();
        let good = status_for_signature("claude-desktop-usage");
        let captured = good.captured_at.clone();
        super::cached_status_attempt(&cache, now, 60, true, || Ok(good));
        let status = super::cached_status_attempt(&cache, now, 60, true, || Err(E::RateLimited));
        let kept = super::discard_failed_desktop_status(&cache, status).unwrap();
        assert_eq!(kept.captured_at, captured);
        assert_eq!(
            super::claude_collection_health(&cache),
            super::CollectionHealth::RateLimited
        );
        assert!(super::cached_status_attempt(
            &cache,
            now + chrono::Duration::seconds(1),
            60,
            true,
            || panic!("cooldown bypassed")
        )
        .is_some());
        let retry_at = cache.lock().unwrap().as_ref().unwrap().retry_at;
        assert!(retry_at >= now + chrono::Duration::seconds(120));
        assert!(
            super::cached_status_attempt(&cache, retry_at, 60, true, || Ok(status_for_signature(
                "claude-desktop-usage"
            )))
            .is_some()
        );
        assert_eq!(
            super::claude_collection_health(&cache),
            super::CollectionHealth::Ready
        );
    }

    #[test]
    fn claude_error_health_and_retention_are_distinct() {
        use super::{CollectionErrorKind as E, CollectionHealth as H};
        assert_eq!(
            super::claude_collection_health(&std::sync::Mutex::new(None)),
            H::Idle
        );
        for (error, health, retain) in [
            (E::RateLimited, H::RateLimited, true),
            (E::Transport, H::TransientError, true),
            (E::Deadline, H::TransientError, true),
            (E::Parse, H::ParseError, true),
            (E::LoginRequired, H::LoginRequired, false),
            (E::CredentialsDecode, H::CredentialsError, false),
            (E::Unavailable, H::Unavailable, false),
            (E::SourceChanged, H::SourceChanged, false),
        ] {
            let cache = std::sync::Mutex::new(None);
            let now = chrono::Utc::now();
            super::cached_status_attempt(&cache, now, 60, true, || {
                Ok(status_for_signature("claude-desktop-usage"))
            });
            let status = super::cached_status_attempt(&cache, now, 60, true, || Err(error));
            assert_eq!(
                super::discard_failed_desktop_status(&cache, status).is_some(),
                retain
            );
            assert_eq!(super::claude_collection_health(&cache), health);
        }
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
            now + Duration::seconds(302)
        );
        assert_eq!(
            cached.as_ref().unwrap().error,
            Some(super::CollectionErrorKind::Parse)
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn provider_failure_backoff_is_class_aware_exponential_and_bounded() {
        assert_eq!(
            super::collection_retry_delay_secs(&super::CollectionErrorKind::Transport, 60, 1),
            60
        );
        assert_eq!(
            super::collection_retry_delay_secs(&super::CollectionErrorKind::Transport, 60, 3),
            240
        );
        assert_eq!(
            super::collection_retry_delay_secs(&super::CollectionErrorKind::LoginRequired, 60, 1),
            300
        );
        assert_eq!(
            super::collection_retry_delay_secs(&super::CollectionErrorKind::Unavailable, 300, 20),
            super::COLLECTION_MAX_BACKOFF_SECS
        );
    }

    #[test]
    fn collection_health_prioritizes_login_failure_over_last_good_status() {
        use chrono::{TimeZone, Utc};
        use std::sync::Mutex;

        let now = Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).unwrap();
        let cache = Mutex::new(Some(super::CachedStatusAttempt {
            attempted_at: now,
            last_good: Some(status_for_signature("last-good")),
            error: Some(super::CollectionErrorKind::LoginRequired),
            retry_at: now,
            consecutive_failures: 1,
        }));

        assert_eq!(
            super::cached_collection_health(&cache),
            super::CollectionHealth::LoginRequired
        );
    }

    #[test]
    fn collection_health_recovers_after_a_successful_retry() {
        use chrono::{TimeZone, Utc};
        use std::sync::Mutex;

        let now = Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).unwrap();
        let cache = Mutex::new(Some(super::CachedStatusAttempt {
            attempted_at: now,
            last_good: None,
            error: Some(super::CollectionErrorKind::LoginRequired),
            retry_at: now,
            consecutive_failures: 1,
        }));
        let result = super::cached_status_attempt(&cache, now, 30, true, || {
            Ok(status_for_signature("recovered"))
        });

        assert_eq!(result.unwrap().session_id, "recovered");
        assert_eq!(
            super::cached_collection_health(&cache),
            super::CollectionHealth::Ready
        );
    }

    #[test]
    fn collection_error_classification_is_symmetric_across_providers() {
        for message in [
            "Claude OAuth access token unavailable",
            "Codex app-server HTTP 401 unauthorized",
            "Grok ACP invalid_grant RefreshTokenRejected",
        ] {
            assert_eq!(
                super::classify_collection_error(&anyhow::anyhow!(message)),
                super::CollectionErrorKind::LoginRequired
            );
        }
        assert_eq!(
            super::classify_collection_error(&anyhow::anyhow!("Grok executable was not found")),
            super::CollectionErrorKind::Unavailable
        );
        assert_eq!(
            super::classify_collection_error(&anyhow::anyhow!("Grok Build executable unavailable")),
            super::CollectionErrorKind::Unavailable
        );
        assert_eq!(
            super::classify_collection_error(&anyhow::anyhow!(
                "Codex app-server command unavailable by collection policy"
            )),
            super::CollectionErrorKind::Unavailable
        );
        assert_eq!(
            super::classify_collection_error(&anyhow::anyhow!("Codex app-server timed out")),
            super::CollectionErrorKind::Transport
        );
        assert_eq!(
            super::classify_collection_error(&anyhow::anyhow!(
                "Codex app-server retry backoff active"
            )),
            super::CollectionErrorKind::Transport
        );
    }

    #[test]
    fn claude_fallback_preserves_oauth_error_when_usage_output_is_not_supported() {
        let raw = r#"{"type":"result","result":"Usage: 0 input, 0 output"}"#;

        for error in [
            super::CollectionErrorKind::LoginRequired,
            super::CollectionErrorKind::Transport,
            super::CollectionErrorKind::Parse,
            super::CollectionErrorKind::Unavailable,
        ] {
            let result = super::parse_claude_fallback_usage(
                raw,
                "LOCAL",
                "2026-08-15T00:00:00Z",
                error.clone(),
            );
            assert_eq!(result.unwrap_err(), error);
        }
    }

    #[test]
    fn claude_login_failure_never_enters_the_legacy_cli_fallback() {
        assert!(!super::claude_legacy_fallback_allowed(
            &super::CollectionErrorKind::LoginRequired,
            false
        ));
        assert!(!super::claude_legacy_fallback_allowed(
            &super::CollectionErrorKind::LoginRequired,
            true
        ));
        assert!(super::claude_legacy_fallback_allowed(
            &super::CollectionErrorKind::Parse,
            false
        ));
        assert!(!super::claude_legacy_fallback_allowed(
            &super::CollectionErrorKind::Transport,
            false
        ));
        assert!(super::claude_legacy_fallback_allowed(
            &super::CollectionErrorKind::Transport,
            true
        ));

        let status = super::parse_claude_fallback_usage(
            r#"{"result":"Current session: 12%\nCurrent week (all models): 34%"}"#,
            "LOCAL",
            "2026-08-15T00:00:00Z",
            super::CollectionErrorKind::Parse,
        )
        .unwrap();

        assert_eq!(status.session_id, "claude-usage");
        assert_eq!(status.primary.unwrap().used_percent, Some(12.0));
        assert_eq!(status.secondary.unwrap().used_percent, Some(34.0));
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
            super::SettingsSnapshot {
                settings: settings.clone(),
                settings_revision: "7".into(),
            },
            Err("taskbar unavailable".into()),
            Err("registry denied".into()),
        );

        assert_eq!(report.settings.settings.bar_mode, settings.bar_mode);
        assert_eq!(report.settings.settings.show_claude, settings.show_claude);
        assert_eq!(report.settings.settings.show_codex, settings.show_codex);
        assert_eq!(report.settings.settings_revision, "7");
        assert!(!report.taskbar_applied);
        assert!(!report.autostart_applied);
        assert_eq!(report.warnings.len(), 2);
        assert!(report.warnings[0].contains("taskbar unavailable"));
        assert!(report.warnings[1].contains("registry denied"));
    }

    #[test]
    fn settings_publication_revisions_cover_commits_and_external_content_changes() {
        let mut state = super::SettingsPublicationState::default();
        let first = Settings::default();
        let a = super::settings_snapshot_in_state(&mut state, first.clone(), 3).unwrap();
        assert_eq!(a.settings_revision, "1");
        let repeated = super::settings_snapshot_in_state(&mut state, first.clone(), 3).unwrap();
        assert_eq!(repeated.settings_revision, a.settings_revision);
        let identical_commit =
            super::settings_snapshot_in_state(&mut state, first.clone(), 4).unwrap();
        assert_eq!(identical_commit.settings_revision, "2");
        let mut changed = first;
        changed.bar_mode = "compact".into();
        let external = super::settings_snapshot_in_state(&mut state, changed.clone(), 4).unwrap();
        assert_eq!(external.settings_revision, "3");
        let final_read = super::settings_snapshot_in_state(&mut state, changed, 4).unwrap();
        assert_eq!(final_read.settings_revision, external.settings_revision);
    }

    #[test]
    fn settings_publication_flat_snapshot_and_nested_reply_keep_decimal_u64() {
        let mut state = super::SettingsPublicationState {
            revision: u64::MAX - 1,
            ..Default::default()
        };
        let snapshot =
            super::settings_snapshot_in_state(&mut state, Settings::default(), 1).unwrap();
        let flat = serde_json::to_value(&snapshot).unwrap();
        assert!(flat.get("settings").is_none());
        assert_eq!(flat["bar_mode"], "full");
        assert_eq!(flat["settings_revision"], u64::MAX.to_string());
        let report = super::settings_apply_report(snapshot, Ok(()), Ok(()));
        let reply = serde_json::to_value(&report).unwrap();
        assert_eq!(reply["settings"]["bar_mode"], flat["bar_mode"]);
        assert_eq!(
            reply["settings"]["settings_revision"],
            flat["settings_revision"]
        );
    }

    #[test]
    fn settings_publication_revision_exhaustion_preserves_the_last_snapshot() {
        let settings = Settings::default();
        let mut state = super::SettingsPublicationState {
            revision: u64::MAX,
            generation: 2,
            settings: Some(settings.clone()),
        };
        assert!(super::settings_snapshot_in_state(&mut state, settings.clone(), 3).is_err());
        assert_eq!(state.revision, u64::MAX);
        assert_eq!(state.generation, 2);
        assert_eq!(state.settings.as_ref(), Some(&settings));
        assert_eq!(
            super::settings_snapshot_in_state(&mut state, settings, 2)
                .unwrap()
                .settings_revision,
            u64::MAX.to_string()
        );
    }

    #[test]
    fn autostart_side_effect_runs_only_when_the_setting_changes() {
        let current = Settings::default();
        let mut requested = current.clone();

        assert!(!super::autostart_setting_changed(&current, &requested));
        requested.autostart_on = !current.autostart_on;
        assert!(super::autostart_setting_changed(&current, &requested));
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
    fn taskbar_style_contract_requires_all_flags_and_no_cross_process_owner() {
        use windows::Win32::UI::WindowsAndMessaging::{
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        };
        let ex_style =
            WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize | WS_EX_TOPMOST.0 as isize;
        let style = WS_POPUP.0 as isize;
        assert!(super::bar_overlay_contract_matches(ex_style, style, 0));
        assert!(!super::bar_overlay_contract_matches(
            ex_style & !(WS_EX_NOACTIVATE.0 as isize),
            style,
            0,
        ));
        assert!(!super::bar_overlay_contract_matches(ex_style, style, 42));
    }

    #[cfg(windows)]
    #[test]
    fn taskbar_z_order_recovery_detects_shell_overlays_and_ignores_juice_bars() {
        use windows::Win32::Foundation::HWND;

        let hwnd = |value: usize| HWND(value as *mut core::ffi::c_void);
        let bar = hwnd(10);
        let taskbar = hwnd(20);
        let other_bar = hwnd(30);
        let juice_bars = [bar, other_bar];

        assert!(!super::taskbar_bar_hit_is_cover(bar, bar, bar, &juice_bars,));
        assert!(super::taskbar_bar_hit_is_cover(
            bar,
            hwnd(21),
            taskbar,
            &juice_bars,
        ));
        assert!(super::taskbar_bar_hit_is_cover(
            bar,
            hwnd(41),
            hwnd(40),
            &juice_bars,
        ));
        assert!(!super::taskbar_bar_hit_is_cover(
            bar,
            hwnd(31),
            other_bar,
            &juice_bars,
        ));

        assert!(!super::taskbar_observation_requires_reapply(
            true, true, false, true
        ));
        assert!(super::taskbar_observation_requires_reapply(
            true, true, true, true
        ));
        assert!(super::taskbar_observation_requires_reapply(
            true, true, false, false
        ));
        assert!(super::taskbar_observation_requires_reapply(
            true, false, false, true
        ));
        assert!(super::taskbar_observation_requires_reapply(
            false, true, false, true
        ));
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
    fn broker_policy_recovery_reenables_after_transient_read_error() {
        for tool in [Tool::Codex, Tool::Grok] {
            let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
            let now = std::time::Instant::now();
            let mut applied = Vec::new();
            assert!(super::reconcile_current_collection_policy(
                &tool,
                || anyhow::bail!("fixture settings unavailable"),
                |enabled| {
                    applied.push(enabled);
                    Ok(())
                },
                |failed| state.lock().unwrap().record(&tool, failed, now),
            )
            .is_err());
            assert_eq!(applied, [false]);
            super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(1),
                || Ok(collection_test_settings(&tool, true)),
                |actual_tool, enabled| {
                    assert_eq!(actual_tool, &tool);
                    applied.push(enabled);
                    Ok(())
                },
                |_, _| {},
            )
            .unwrap();
            assert_eq!(applied, [false, true]);
            assert!(!state.lock().unwrap().entries[super::collection_tool_index(&tool)].pending);
        }
    }

    #[test]
    fn broker_policy_recovery_obeys_latest_off_and_skips_healthy_policies() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Codex, true, now);
        let mut applied = Vec::new();
        super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(1),
            || Ok(collection_test_settings(&Tool::Codex, false)),
            |tool, enabled| {
                assert_eq!(tool, &Tool::Codex);
                applied.push(enabled);
                Ok(())
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(applied, [false]);
        super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(500),
            || panic!("healthy policy must not reload settings"),
            |_, _| panic!("healthy policy must not be rewritten"),
            |_, _| panic!("healthy policy must not republish health"),
        )
        .unwrap();
    }

    #[test]
    fn broker_policy_recovery_keeps_disabled_on_read_errors_with_bounded_backoff() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let mut now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Codex, true, now);
        for expected in [1, 2, 4, 8, 16, 32, 60, 60] {
            let deadline = state.lock().unwrap().entries
                [super::collection_tool_index(&Tool::Codex)]
            .retry_at
            .unwrap();
            assert_eq!(deadline.duration_since(now).as_secs(), expected);
            super::recover_pending_collection_policies_with(
                &state,
                || deadline - std::time::Duration::from_millis(1),
                || panic!("backoff must defer settings IO"),
                |_, _| panic!("backoff must defer setter"),
                |_, _| panic!("backoff must defer health publication"),
            )
            .unwrap();
            now = deadline;
            assert!(super::recover_pending_collection_policies_with(
                &state,
                || now,
                || anyhow::bail!("fixture settings unavailable"),
                |_, enabled| {
                    assert!(!enabled);
                    Ok(())
                },
                |_, _| {},
            )
            .is_err());
        }
    }

    #[test]
    fn broker_policy_recovery_direct_success_cancels_pending_retry() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Grok, true, now);
        super::reconcile_current_collection_policy(
            &Tool::Grok,
            || Ok(collection_test_settings(&Tool::Grok, true)),
            |enabled| {
                assert!(enabled);
                Ok(())
            },
            |failed| state.lock().unwrap().record(&Tool::Grok, failed, now),
        )
        .unwrap();
        super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(2),
            || panic!("direct success already repaired the policy"),
            |_, _| panic!("duplicate apply"),
            |_, _| panic!("duplicate health publication"),
        )
        .unwrap();
    }

    #[test]
    fn broker_policy_recovery_rechecks_pending_after_waiting_for_settings_gate() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        let state = Arc::new(std::sync::Mutex::new(
            super::CollectionPolicyRecovery::default(),
        ));
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Codex, true, now);
        let (started, ready) = mpsc::channel();
        let worker_state = state.clone();
        let worker = super::with_taskbar_settings_read(|_| {
            let worker = std::thread::spawn(move || {
                let first = AtomicBool::new(true);
                super::recover_pending_collection_policies_with(
                    &worker_state,
                    || {
                        if first.swap(false, Ordering::SeqCst) {
                            started.send(()).unwrap();
                        }
                        now + std::time::Duration::from_secs(1)
                    },
                    || panic!("a newer direct apply cleared the pending entry"),
                    |_, _| panic!("stale retry"),
                    |_, _| panic!("stale health publication"),
                )
            });
            ready
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            state.lock().unwrap().record(&Tool::Codex, false, now);
            Ok(worker)
        })
        .unwrap();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn broker_policy_recovery_concurrent_callers_apply_once() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        };
        let state = Arc::new(std::sync::Mutex::new(
            super::CollectionPolicyRecovery::default(),
        ));
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Codex, true, now);
        let calls = Arc::new(AtomicUsize::new(0));
        let ready = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let state = state.clone();
            let calls = calls.clone();
            let ready = ready.clone();
            workers.push(std::thread::spawn(move || {
                ready.wait();
                super::recover_pending_collection_policies_with(
                    &state,
                    || now + std::time::Duration::from_secs(1),
                    || Ok(collection_test_settings(&Tool::Codex, true)),
                    |_, enabled| {
                        assert!(enabled);
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                    |_, _| {},
                )
            }));
        }
        ready.wait();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn broker_policy_recovery_one_failure_does_not_starve_other_pending_tool() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Codex, true, now);
        state.lock().unwrap().record(&Tool::Grok, true, now);
        let mut calls = Vec::new();
        assert!(super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(1),
            || Ok(Settings {
                show_codex: true,
                show_grok: true,
                ..Settings::default()
            }),
            |tool, enabled| {
                assert!(enabled);
                calls.push(tool.clone());
                if *tool == Tool::Codex {
                    anyhow::bail!("fixture setter unavailable")
                }
                Ok(())
            },
            |_, _| {},
        )
        .is_err());
        assert_eq!(calls, [Tool::Codex, Tool::Grok]);
        let state = state.lock().unwrap();
        assert!(state.entries[super::collection_tool_index(&Tool::Codex)].pending);
        assert!(!state.entries[super::collection_tool_index(&Tool::Grok)].pending);
    }

    #[test]
    fn cli_policy_recovery_repairs_failed_enable_and_restore_from_current_settings() {
        for enabled in [true, false] {
            let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
            let now = std::time::Instant::now();
            let mut attempts = Vec::new();
            let mut health = Vec::new();
            assert!(super::reconcile_current_antigravity_cli(
                || Ok(collection_test_settings(&Tool::Antigravity, enabled)),
                |current| {
                    attempts.push(current);
                    anyhow::bail!("fixture CLI settings locked")
                },
                |failed| {
                    state
                        .lock()
                        .unwrap()
                        .record(&Tool::Antigravity, failed, now);
                    health.push(failed);
                },
            )
            .is_err());
            super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(1),
                || Ok(collection_test_settings(&Tool::Antigravity, enabled)),
                |tool, current| {
                    assert_eq!(tool, &Tool::Antigravity);
                    attempts.push(current);
                    Ok(())
                },
                |tool, failed| {
                    assert_eq!(tool, &Tool::Antigravity);
                    health.push(failed);
                },
            )
            .unwrap();
            assert_eq!(attempts, [enabled, enabled]);
            assert_eq!(health, [true, false]);
            super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(500),
                || panic!("healthy CLI must not reload settings"),
                |_, _| panic!("healthy CLI must not rewrite the binding"),
                |_, _| panic!("healthy CLI must not republish failure"),
            )
            .unwrap();
        }
    }

    #[test]
    fn cli_policy_recovery_starts_after_initial_settings_read_failure() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let now = std::time::Instant::now();
        let mut enabled = false;
        assert!(super::reconcile_current_antigravity_cli(
            || anyhow::bail!("fixture startup settings locked"),
            |_| panic!("unknown startup policy must not touch CLI files"),
            |failed| state
                .lock()
                .unwrap()
                .record(&Tool::Antigravity, failed, now),
        )
        .is_err());
        assert!(
            state.lock().unwrap().entries[super::collection_tool_index(&Tool::Antigravity)].pending
        );
        super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(1),
            || Ok(collection_test_settings(&Tool::Antigravity, true)),
            |tool, current| {
                assert_eq!(tool, &Tool::Antigravity);
                enabled = current;
                Ok(())
            },
            |_, failed| assert!(!failed),
        )
        .unwrap();
        assert!(enabled);
        assert!(
            !state.lock().unwrap().entries[super::collection_tool_index(&Tool::Antigravity)]
                .pending
        );
    }

    #[test]
    fn cli_policy_recovery_obeys_latest_toggle_and_preserves_binding_on_read_failure() {
        for enabled in [true, false] {
            let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
            let now = std::time::Instant::now();
            state.lock().unwrap().record(&Tool::Antigravity, true, now);
            let mut health = Vec::new();
            assert!(super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(1),
                || anyhow::bail!("fixture settings unreadable"),
                |_, _| panic!("unknown policy must not alter CLI files"),
                |tool, failed| {
                    assert_eq!(tool, &Tool::Antigravity);
                    health.push(failed);
                },
            )
            .is_err());
            let mut applied = Vec::new();
            super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(3),
                || Ok(collection_test_settings(&Tool::Antigravity, enabled)),
                |tool, current| {
                    assert_eq!(tool, &Tool::Antigravity);
                    applied.push(current);
                    Ok(())
                },
                |_, failed| health.push(failed),
            )
            .unwrap();
            assert_eq!(applied, [enabled]);
            assert_eq!(health, [true, false]);
        }
    }

    #[test]
    fn cli_policy_recovery_respects_backoff_and_bounded_failures() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let mut now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Antigravity, true, now);
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            let due = state.lock().unwrap().entries
                [super::collection_tool_index(&Tool::Antigravity)]
            .retry_at
            .unwrap();
            assert_eq!(due.duration_since(now).as_secs(), seconds);
            super::recover_pending_collection_policies_with(
                &state,
                || due - std::time::Duration::from_millis(1),
                || panic!("CLI backoff must defer IO"),
                |_, _| panic!("CLI backoff must defer binding work"),
                |_, _| panic!("CLI backoff must defer publication"),
            )
            .unwrap();
            now = due;
            assert!(super::recover_pending_collection_policies_with(
                &state,
                || now,
                || Ok(collection_test_settings(&Tool::Antigravity, true)),
                |_, _| anyhow::bail!("fixture binding remains unavailable"),
                |_, failed| assert!(failed),
            )
            .is_err());
        }
    }

    #[test]
    fn cli_policy_recovery_rechecks_after_direct_success_and_publishes_under_gate() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        let state = Arc::new(std::sync::Mutex::new(
            super::CollectionPolicyRecovery::default(),
        ));
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Antigravity, true, now);
        let (started, ready) = mpsc::channel();
        let worker_state = state.clone();
        let worker = super::with_taskbar_settings_read(|_| {
            let worker = std::thread::spawn(move || {
                let first = AtomicBool::new(true);
                super::recover_pending_collection_policies_with(
                    &worker_state,
                    || {
                        if first.swap(false, Ordering::SeqCst) {
                            started.send(()).unwrap();
                        }
                        now + std::time::Duration::from_secs(1)
                    },
                    || panic!("direct success retired pending work"),
                    |_, _| panic!("stale CLI apply"),
                    |_, _| panic!("stale CLI failure publication"),
                )
            });
            ready
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            state.lock().unwrap().record(&Tool::Antigravity, false, now);
            Ok(worker)
        })
        .unwrap();
        worker.join().unwrap().unwrap();
        state.lock().unwrap().record(&Tool::Antigravity, true, now);
        super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(1),
            || Ok(collection_test_settings(&Tool::Antigravity, false)),
            |_, enabled| {
                assert!(!enabled);
                Ok(())
            },
            |_, failed| {
                assert!(!failed);
                assert!(super::TASKBAR_SETTINGS_WRITE_GATE.try_lock().is_err());
                assert!(
                    !state.lock().unwrap().entries
                        [super::collection_tool_index(&Tool::Antigravity)]
                    .pending
                );
            },
        )
        .unwrap();
    }

    #[test]
    fn cli_policy_recovery_failure_does_not_starve_other_pending_tools() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Codex, true, now);
        state.lock().unwrap().record(&Tool::Antigravity, true, now);
        let mut health = Vec::new();
        assert!(super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(1),
            || Ok(Settings {
                show_codex: true,
                show_antigravity: true,
                ..Settings::default()
            }),
            |tool, enabled| {
                assert!(enabled);
                if *tool == Tool::Antigravity {
                    anyhow::bail!("fixture CLI binding locked");
                }
                assert_eq!(tool, &Tool::Codex);
                Ok(())
            },
            |tool, failed| health.push((tool.clone(), failed)),
        )
        .is_err());
        assert_eq!(health, [(Tool::Codex, false), (Tool::Antigravity, true)]);
        assert!(!state.lock().unwrap().entries[super::collection_tool_index(&Tool::Codex)].pending);
        assert!(
            state.lock().unwrap().entries[super::collection_tool_index(&Tool::Antigravity)].pending
        );
    }

    #[cfg(windows)]
    #[test]
    fn cli_policy_recovery_retries_real_locked_binding_without_losing_original_settings() {
        use crate::antigravity_cli::binding;
        use std::os::windows::fs::OpenOptionsExt;
        for enabled in [true, false] {
            let root = std::env::temp_dir().join(format!(
                "juice-cli-policy-retry-{}-{}-{enabled}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap(),
            ));
            let home = root.join("home");
            let data = root.join("data");
            let path = home.join(".gemini/antigravity-cli/settings.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = serde_json::json!({
                "statusLine":{"type":"command","command":"echo fixture-original","padding":3},
                "other":{"unchanged":true}
            });
            std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
            let helper = root.join("fixture.exe");
            std::fs::write(&helper, b"never executed").unwrap();
            if !enabled {
                binding::install_at(&home, &data, &helper).unwrap();
            }
            let locked = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            let apply = |enabled| -> anyhow::Result<()> {
                if enabled {
                    binding::install_at(&home, &data, &helper).map(|_| ())
                } else {
                    binding::restore_at(&home, &data)
                }
            };
            let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
            let now = std::time::Instant::now();
            assert!(super::reconcile_current_antigravity_cli(
                || Ok(collection_test_settings(&Tool::Antigravity, enabled)),
                apply,
                |failed| state
                    .lock()
                    .unwrap()
                    .record(&Tool::Antigravity, failed, now),
            )
            .is_err());
            drop(locked);
            super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(1),
                || Ok(collection_test_settings(&Tool::Antigravity, enabled)),
                |tool, enabled| {
                    assert_eq!(tool, &Tool::Antigravity);
                    apply(enabled)
                },
                |_, failed| assert!(!failed),
            )
            .unwrap();
            assert_eq!(
                binding::load_owned_at(&home, &data).unwrap().is_some(),
                enabled
            );
            let actual: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(actual["other"], original["other"]);
            binding::restore_at(&home, &data).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap())
                    .unwrap(),
                original
            );
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn taskbar_content_layout_rollback_preserves_a_new_position() {
        let original = super::TaskbarContentLayout {
            mode: "full".into(),
            width: 100,
            ratio: Some(0.8),
        };
        let slot = std::sync::Mutex::new(super::TaskbarContentLayoutSlot {
            revision: 0,
            layout: Some(original.clone()),
        });
        let write = super::begin_taskbar_content_layout_in_slot(
            &slot,
            0,
            super::TaskbarContentLayout {
                width: 150,
                ..original
            },
        )
        .unwrap();
        super::set_taskbar_content_layout_ratio_in_slot(&slot, 0.2);
        assert!(!super::rollback_taskbar_content_layout_in_slot(
            &slot, write
        ));
        let actual = slot.lock().unwrap().layout.clone().unwrap();
        assert_eq!(actual.width, 150);
        assert_eq!(actual.ratio, Some(0.2));
    }

    #[test]
    fn taskbar_content_layout_rollback_preserves_newer_and_identical_measurements() {
        for newer_width in [150, 240] {
            let original = super::TaskbarContentLayout {
                mode: "full".into(),
                width: 100,
                ratio: Some(0.8),
            };
            let candidate = super::TaskbarContentLayout {
                width: 150,
                ..original.clone()
            };
            let slot = std::sync::Mutex::new(super::TaskbarContentLayoutSlot {
                revision: 0,
                layout: Some(original),
            });
            let old =
                super::begin_taskbar_content_layout_in_slot(&slot, 0, candidate.clone()).unwrap();
            let next = super::TaskbarContentLayout {
                width: newer_width,
                ..candidate
            };
            let _new =
                super::begin_taskbar_content_layout_in_slot(&slot, old.revision, next.clone())
                    .unwrap();
            assert!(!super::rollback_taskbar_content_layout_in_slot(&slot, old));
            assert_eq!(slot.lock().unwrap().layout, Some(next));
        }
    }

    #[test]
    fn taskbar_content_layout_stale_plan_cannot_replace_a_new_position() {
        let slot = std::sync::Mutex::new(super::TaskbarContentLayoutSlot {
            revision: 8,
            layout: Some(super::TaskbarContentLayout {
                mode: "full".into(),
                width: 100,
                ratio: Some(0.8),
            }),
        });
        super::set_taskbar_content_layout_ratio_in_slot(&slot, 0.2);
        let stale = super::begin_taskbar_content_layout_in_slot(
            &slot,
            8,
            super::TaskbarContentLayout {
                mode: "full".into(),
                width: 150,
                ratio: Some(0.8),
            },
        );
        assert!(stale.is_err());
        let current = slot.lock().unwrap();
        assert_eq!(current.revision, 9);
        assert_eq!(current.layout.as_ref().unwrap().width, 100);
        assert_eq!(current.layout.as_ref().unwrap().ratio, Some(0.2));
    }

    #[test]
    fn taskbar_content_layout_normal_rollback_restores_the_raw_previous_mode() {
        for previous in [
            None,
            Some(super::TaskbarContentLayout {
                mode: "compact".into(),
                width: 80,
                ratio: Some(0.3),
            }),
        ] {
            let slot = std::sync::Mutex::new(super::TaskbarContentLayoutSlot {
                revision: 4,
                layout: previous.clone(),
            });
            let write = super::begin_taskbar_content_layout_in_slot(
                &slot,
                4,
                super::TaskbarContentLayout {
                    mode: "full".into(),
                    width: 150,
                    ratio: Some(0.8),
                },
            )
            .unwrap();
            assert!(
                slot.try_lock().is_ok(),
                "native placement must not inherit a cache lock"
            );
            assert!(super::rollback_taskbar_content_layout_in_slot(&slot, write));
            let current = slot.lock().unwrap();
            assert_eq!(current.revision, 6);
            assert_eq!(current.layout, previous);
        }
    }

    #[test]
    fn taskbar_content_layout_identical_ratio_publication_invalidates_old_rollback() {
        let candidate = super::TaskbarContentLayout {
            mode: "full".into(),
            width: 150,
            ratio: Some(0.8),
        };
        let slot = std::sync::Mutex::new(super::TaskbarContentLayoutSlot::default());
        let write =
            super::begin_taskbar_content_layout_in_slot(&slot, 0, candidate.clone()).unwrap();
        super::set_taskbar_content_layout_ratio_in_slot(&slot, 0.8);
        assert!(!super::rollback_taskbar_content_layout_in_slot(
            &slot, write
        ));
        assert_eq!(slot.lock().unwrap().layout, Some(candidate));
    }

    #[test]
    fn taskbar_content_layout_acknowledged_measurement_invalidates_old_rollback() {
        let candidate = super::TaskbarContentLayout {
            mode: "full".into(),
            width: 150,
            ratio: Some(0.8),
        };
        let slot = std::sync::Mutex::new(super::TaskbarContentLayoutSlot::default());
        let write =
            super::begin_taskbar_content_layout_in_slot(&slot, 0, candidate.clone()).unwrap();
        super::acknowledge_taskbar_content_layout_in_slot(&slot, write.revision).unwrap();
        assert!(!super::rollback_taskbar_content_layout_in_slot(
            &slot, write
        ));
        assert_eq!(slot.lock().unwrap().layout, Some(candidate));
        assert!(super::acknowledge_taskbar_content_layout_in_slot(&slot, 1).is_err());
        assert_eq!(slot.lock().unwrap().revision, 2);
    }

    #[test]
    fn taskbar_menu_own_failure_closes_the_current_request() {
        for open in [false, true] {
            let slot = std::sync::Mutex::new(super::TaskbarMenuSlot::default());
            let write = super::set_taskbar_menu_layout_in_slot(&slot, open, Some(0.8));
            assert!(super::close_taskbar_menu_layout_in_slot_if_current(
                &slot, write
            ));
            let current = slot.lock().unwrap();
            assert_eq!(current.layout, super::TaskbarMenuLayout::default());
            assert_eq!(current.revision, 2);
        }
    }

    #[test]
    fn taskbar_menu_new_identical_open_and_aba_survive_old_cleanup() {
        for aba in [false, true] {
            let slot = std::sync::Mutex::new(super::TaskbarMenuSlot::default());
            let old = super::set_taskbar_menu_layout_in_slot(&slot, true, Some(0.8));
            if aba {
                super::set_taskbar_menu_layout_in_slot(&slot, false, None);
            }
            super::set_taskbar_menu_layout_in_slot(&slot, true, Some(0.8));
            let before = slot.lock().unwrap().revision;
            assert!(!super::close_taskbar_menu_layout_in_slot_if_current(
                &slot, old
            ));
            let current = slot.lock().unwrap();
            assert_eq!(current.revision, before);
            assert!(current.layout.open);
            assert_eq!(current.layout.ratio, Some(0.8));
        }
    }

    #[test]
    fn taskbar_menu_each_slot_versions_immediate_closed_publications() {
        let state = super::TaskbarMenuState::default();
        for tool in super::TASKBAR_TOOLS {
            let slot = super::taskbar_menu_slot(&state, tool).unwrap();
            let old = super::set_taskbar_menu_layout_in_slot(slot, true, Some(0.8));
            super::set_taskbar_menu_layout_in_slot(slot, false, None);
            super::set_taskbar_menu_layout_in_slot(slot, false, None);
            assert!(!super::close_taskbar_menu_layout_in_slot_if_current(
                slot, old
            ));
            let current = slot.lock().unwrap();
            assert_eq!(current.revision, 3);
            assert_eq!(current.layout, super::TaskbarMenuLayout::default());
        }
    }

    #[test]
    fn taskbar_menu_deferred_cleanup_owns_only_its_observed_revision() {
        let slot = std::sync::Mutex::new(super::TaskbarMenuSlot::default());
        super::set_taskbar_menu_layout_in_slot(&slot, true, Some(0.8));
        let pending = super::TaskbarMenuWrite {
            revision: slot.lock().unwrap().revision,
        };
        super::set_taskbar_menu_layout_in_slot(&slot, true, Some(0.3));
        assert!(!super::close_taskbar_menu_layout_in_slot_if_current(
            &slot, pending
        ));
        assert_eq!(slot.lock().unwrap().layout.ratio, Some(0.3));
        let current = super::TaskbarMenuWrite {
            revision: slot.lock().unwrap().revision,
        };
        assert!(super::close_taskbar_menu_layout_in_slot_if_current(
            &slot, current
        ));
        assert!(!slot.lock().unwrap().layout.open);
    }

    #[test]
    fn taskbar_menu_publication_releases_the_slot_before_native_work() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(super::TaskbarMenuSlot::default()));
        let old = super::set_taskbar_menu_layout_in_slot(&slot, true, Some(0.8));
        assert!(slot.try_lock().is_ok());
        let newer_slot = slot.clone();
        std::thread::spawn(move || {
            super::set_taskbar_menu_layout_in_slot(&newer_slot, true, Some(0.5));
        })
        .join()
        .unwrap();
        assert!(!super::close_taskbar_menu_layout_in_slot_if_current(
            &slot, old
        ));
        assert_eq!(slot.lock().unwrap().layout.ratio, Some(0.5));
    }

    #[test]
    fn taskbar_menu_publication_keeps_existing_ratio_clamping() {
        let slot = std::sync::Mutex::new(super::TaskbarMenuSlot::default());
        for (input, expected) in [
            (Some(-1.0), Some(0.0)),
            (Some(2.0), Some(1.0)),
            (None, None),
        ] {
            super::set_taskbar_menu_layout_in_slot(&slot, true, input);
            assert_eq!(slot.lock().unwrap().layout.ratio, expected);
        }
    }

    #[cfg(windows)]
    #[test]
    fn taskbar_content_layout_native_plan_rejects_retired_and_missing_state_revisions() {
        let plan = [("claude", Some(4)), ("codex", Some(8)), ("cursor", None)];
        let current = |tool: &str| match tool {
            "claude" => Some(4),
            "codex" => Some(8),
            _ => None,
        };
        super::validate_taskbar_content_layout_plan(&plan, current).unwrap();
        for changed in ["claude", "codex", "cursor"] {
            assert!(super::validate_taskbar_content_layout_plan(&plan, |tool| {
                if tool == changed {
                    Some(99)
                } else {
                    current(tool)
                }
            })
            .is_err());
        }
        assert!(super::validate_taskbar_content_layout_plan(&plan, |_| None).is_err());
    }

    #[test]
    fn claude_policy_recovery_repairs_startup_read_and_binding_failures() {
        for enabled in [true, false] {
            for read_failed in [true, false] {
                let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
                let now = std::time::Instant::now();
                let mut attempts = Vec::new();
                let result = super::reconcile_current_collection_policy(
                    &Tool::Claude,
                    || {
                        if read_failed {
                            anyhow::bail!("fixture startup settings locked");
                        }
                        Ok(collection_test_settings(&Tool::Claude, enabled))
                    },
                    |current| {
                        attempts.push(current);
                        anyhow::bail!("fixture Claude settings locked")
                    },
                    |failed| state.lock().unwrap().record(&Tool::Claude, failed, now),
                );
                assert!(result.is_err());
                assert_eq!(attempts.len(), usize::from(!read_failed));
                super::recover_pending_collection_policies_with(
                    &state,
                    || now + std::time::Duration::from_secs(1),
                    || Ok(collection_test_settings(&Tool::Claude, enabled)),
                    |tool, current| {
                        assert_eq!(tool, &Tool::Claude);
                        attempts.push(current);
                        Ok(())
                    },
                    |tool, failed| {
                        assert_eq!(tool, &Tool::Claude);
                        assert!(!failed);
                    },
                )
                .unwrap();
                assert_eq!(attempts.last(), Some(&enabled));
                assert_eq!(attempts.len(), 1 + usize::from(!read_failed));
                super::recover_pending_collection_policies_with(
                    &state,
                    || now + std::time::Duration::from_secs(500),
                    || panic!("healthy Claude must not reload settings"),
                    |_, _| panic!("healthy Claude must not rewrite its statusline"),
                    |_, _| panic!("healthy Claude must not publish failure"),
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn claude_policy_recovery_preserves_unknown_files_and_uses_latest_toggle() {
        for enabled in [true, false] {
            let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
            let now = std::time::Instant::now();
            state.lock().unwrap().record(&Tool::Claude, true, now);
            assert!(super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(1),
                || anyhow::bail!("fixture unreadable settings"),
                |_, _| panic!("unknown Claude policy must not alter files"),
                |tool, failed| {
                    assert_eq!(tool, &Tool::Claude);
                    assert!(failed);
                },
            )
            .is_err());
            let mut applied = None;
            super::recover_pending_collection_policies_with(
                &state,
                || now + std::time::Duration::from_secs(3),
                || Ok(collection_test_settings(&Tool::Claude, enabled)),
                |tool, current| {
                    assert_eq!(tool, &Tool::Claude);
                    assert!(super::TASKBAR_SETTINGS_WRITE_GATE.try_lock().is_err());
                    applied = Some(current);
                    Ok(())
                },
                |_, failed| assert!(!failed),
            )
            .unwrap();
            assert_eq!(applied, Some(enabled));
        }
    }

    #[test]
    fn claude_policy_recovery_backoff_and_direct_success_defer_unneeded_io() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let mut now = std::time::Instant::now();
        state.lock().unwrap().record(&Tool::Claude, true, now);
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            let due = state.lock().unwrap().entries[super::collection_tool_index(&Tool::Claude)]
                .retry_at
                .expect("failed Claude policy must have a retry deadline");
            assert_eq!(due.duration_since(now).as_secs(), seconds);
            super::recover_pending_collection_policies_with(
                &state,
                || due - std::time::Duration::from_millis(1),
                || panic!("backoff must defer Claude settings IO"),
                |_, _| panic!("backoff must defer Claude binding IO"),
                |_, _| panic!("backoff must defer Claude health publication"),
            )
            .unwrap();
            now = due;
            assert!(super::recover_pending_collection_policies_with(
                &state,
                || now,
                || Ok(collection_test_settings(&Tool::Claude, true)),
                |_, _| anyhow::bail!("fixture file remains locked"),
                |_, failed| assert!(failed),
            )
            .is_err());
        }
        state.lock().unwrap().record(&Tool::Claude, false, now);
        super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(500),
            || panic!("direct success must retire pending Claude work"),
            |_, _| panic!("direct success must skip repeated Claude apply"),
            |_, _| panic!("direct success must skip failure publication"),
        )
        .unwrap();
    }

    #[test]
    fn claude_policy_recovery_failure_does_not_starve_other_pending_tools() {
        let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
        let now = std::time::Instant::now();
        for tool in [Tool::Claude, Tool::Codex, Tool::Grok, Tool::Antigravity] {
            state.lock().unwrap().record(&tool, true, now);
        }
        let mut calls = Vec::new();
        assert!(super::recover_pending_collection_policies_with(
            &state,
            || now + std::time::Duration::from_secs(1),
            || Ok(Settings::default()),
            |tool, _| {
                calls.push(tool.clone());
                if *tool == Tool::Claude {
                    anyhow::bail!("fixture Claude binding failed");
                }
                Ok(())
            },
            |_, _| {},
        )
        .is_err());
        assert_eq!(
            calls,
            [Tool::Claude, Tool::Codex, Tool::Grok, Tool::Antigravity]
        );
        assert!(state.lock().unwrap().entries[super::collection_tool_index(&Tool::Claude)].pending);
    }

    #[cfg(windows)]
    #[test]
    fn claude_policy_recovery_retries_real_file_lock_and_preserves_custom_command() {
        use std::os::windows::fs::OpenOptionsExt;
        for enabled in [true, false] {
            for user_changed in [true, false] {
                let (root, home, data, _) = collection_transaction_fixture();
                let path = home.join(".claude/settings.json");
                let original = std::fs::read(&path).unwrap();
                if !enabled || user_changed {
                    apply_fixture_claude(&home, &data, true).unwrap();
                }
                let locked = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&path)
                    .unwrap();
                let state = std::sync::Mutex::new(super::CollectionPolicyRecovery::default());
                let now = std::time::Instant::now();
                assert!(super::reconcile_current_collection_policy(
                    &Tool::Claude,
                    || Ok(collection_test_settings(&Tool::Claude, enabled)),
                    |current| apply_fixture_claude(&home, &data, current),
                    |failed| state.lock().unwrap().record(&Tool::Claude, failed, now),
                )
                .is_err());
                drop(locked);
                let changed =
                    br#"{"statusLine":{"command":"echo fixture-new-custom"},"other":"keep"}"#;
                if user_changed {
                    std::fs::write(&path, changed).unwrap();
                }
                let result = super::recover_pending_collection_policies_with(
                    &state,
                    || now + std::time::Duration::from_secs(1),
                    || Ok(collection_test_settings(&Tool::Claude, enabled)),
                    |tool, current| {
                        assert_eq!(tool, &Tool::Claude);
                        apply_fixture_claude(&home, &data, current)
                    },
                    |_, _| {},
                );
                if user_changed {
                    assert!(result.is_err());
                    assert_eq!(std::fs::read(&path).unwrap(), changed);
                    assert!(
                        state.lock().unwrap().entries[super::collection_tool_index(&Tool::Claude)]
                            .pending
                    );
                } else {
                    result.unwrap();
                    let actual: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    assert_eq!(actual["other"], "keep");
                    if enabled {
                        assert!(actual["statusLine"]["command"]
                            .as_str()
                            .unwrap()
                            .contains("agentjuice-statusline.exe"));
                    } else {
                        assert_eq!(
                            actual,
                            serde_json::from_slice::<serde_json::Value>(&original).unwrap()
                        );
                    }
                    apply_fixture_claude(&home, &data, false).unwrap();
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap())
                            .unwrap(),
                        serde_json::from_slice::<serde_json::Value>(&original).unwrap()
                    );
                }
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }

    fn collection_test_settings(tool: &Tool, enabled: bool) -> Settings {
        let mut settings = Settings {
            show_claude: false,
            show_codex: false,
            show_grok: false,
            show_cursor: false,
            show_antigravity: false,
            ..Settings::default()
        };
        match tool {
            Tool::Claude => settings.show_claude = enabled,
            Tool::Codex => settings.show_codex = enabled,
            Tool::Grok => settings.show_grok = enabled,
            Tool::Cursor => settings.show_cursor = enabled,
            Tool::Antigravity => settings.show_antigravity = enabled,
        }
        settings
    }

    #[test]
    fn collection_policy_commit_tracks_every_tool_in_both_directions() {
        for tool in [
            Tool::Claude,
            Tool::Codex,
            Tool::Grok,
            Tool::Cursor,
            Tool::Antigravity,
        ] {
            for target in [true, false] {
                let before = collection_test_settings(&tool, !target);
                let requested = collection_test_settings(&tool, target);
                let preflight =
                    super::merge_settings_edits(&requested, Some(&before), &requested).unwrap();
                assert_eq!(
                    super::CollectionPolicy::from_settings(&preflight).enabled(&tool),
                    target
                );
                let mut current = before.clone();
                let changes =
                    super::mutate_settings_with_collection_changes(&mut current, |current| {
                        *current = super::merge_settings_edits(current, Some(&before), &requested)?;
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(changes.transition(&tool), Some(target));
                assert_eq!(changes.enabled_now(&tool), target);
                assert!(changes.any());
                let mut applied = None;
                super::reconcile_current_collection_policy(
                    &tool,
                    || Ok(current.clone()),
                    |enabled| {
                        applied = Some(enabled);
                        Ok(())
                    },
                    |_| {},
                )
                .unwrap();
                assert_eq!(applied, Some(target));
                let noop = super::mutate_settings_with_collection_changes(&mut current, |_| Ok(()))
                    .unwrap();
                assert!(!noop.any());
            }
        }
    }

    #[test]
    fn collection_policy_delayed_jobs_read_latest_settings_for_all_tools() {
        use std::sync::{Arc, Barrier, Mutex};
        for tool in [
            Tool::Claude,
            Tool::Codex,
            Tool::Grok,
            Tool::Cursor,
            Tool::Antigravity,
        ] {
            for latest in [true, false] {
                let current = Arc::new(Mutex::new(collection_test_settings(&tool, !latest)));
                let ready = Arc::new(Barrier::new(2));
                let release = Arc::new(Barrier::new(2));
                let worker_current = current.clone();
                let worker_ready = ready.clone();
                let worker_release = release.clone();
                let worker_tool = tool.clone();
                let worker = std::thread::spawn(move || {
                    worker_ready.wait();
                    worker_release.wait();
                    let mut applied = None;
                    super::reconcile_current_collection_policy(
                        &worker_tool,
                        || Ok(worker_current.lock().unwrap().clone()),
                        |enabled| {
                            assert!(matches!(
                                super::TASKBAR_SETTINGS_WRITE_GATE.try_lock(),
                                Err(std::sync::TryLockError::WouldBlock)
                            ));
                            applied = Some(enabled);
                            Ok(())
                        },
                        |_| {},
                    )
                    .unwrap();
                    applied
                });
                ready.wait();
                super::with_taskbar_settings_read(|_| {
                    *current.lock().unwrap() = collection_test_settings(&tool, latest);
                    Ok(())
                })
                .unwrap();
                release.wait();
                assert_eq!(worker.join().unwrap(), Some(latest));
            }
        }
    }

    #[test]
    fn collection_policy_settings_failure_disables_brokers_but_leaves_files_untouched() {
        for tool in [Tool::Claude, Tool::Codex, Tool::Grok, Tool::Antigravity] {
            let mut applied = None;
            let mut failed = false;
            let result = super::reconcile_current_collection_policy(
                &tool,
                || anyhow::bail!("fixture settings read failed"),
                |enabled| {
                    applied = Some(enabled);
                    Ok(())
                },
                |error| failed = error,
            );
            assert!(result.is_err());
            assert!(failed);
            assert_eq!(
                applied,
                if matches!(tool, Tool::Codex | Tool::Grok) {
                    Some(false)
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn antigravity_policy_read_failure_blocks_old_on_snapshot_without_writer() {
        use std::cell::Cell;

        let old_settings = collection_test_settings(&Tool::Antigravity, true);
        let admitted = Cell::new(true);
        let retire_calls = Cell::new(0);
        let headless_calls = Cell::new(0);
        let result = super::apply_collection_policy_from_settings_with_retirement(
            &Tool::Antigravity,
            Err(anyhow::anyhow!("synthetic settings read failed")),
            |_| panic!("read failure must not invoke a binding/settings writer"),
            || {
                admitted.set(false);
                retire_calls.set(retire_calls.get() + 1);
            },
        );
        assert!(result.is_err());
        assert_eq!(retire_calls.get(), 1);
        assert!(!admitted.get());
        for _force in [false, true] {
            if old_settings.show_antigravity && admitted.get() {
                headless_calls.set(headless_calls.get() + 1);
            }
        }
        assert_eq!(headless_calls.get(), 0);
    }

    #[test]
    fn antigravity_policy_latest_success_recovers_only_its_current_admission() {
        use std::cell::Cell;

        for latest in [false, true] {
            let admitted = Cell::new(true);
            let writes = Cell::new(0);
            let headless_calls = Cell::new(0);
            assert!(
                super::apply_collection_policy_from_settings_with_retirement(
                    &Tool::Antigravity,
                    Err(anyhow::anyhow!("synthetic settings read failed")),
                    |_| panic!("read failure must leave original statusline unchanged"),
                    || admitted.set(false),
                )
                .is_err()
            );
            super::apply_collection_policy_from_settings_with_retirement(
                &Tool::Antigravity,
                Ok(collection_test_settings(&Tool::Antigravity, latest)),
                |enabled| {
                    admitted.set(enabled);
                    writes.set(writes.get() + 1);
                    Ok(())
                },
                || panic!("a valid current policy must use normal apply"),
            )
            .unwrap();
            assert_eq!(writes.get(), 1);
            assert_eq!(admitted.get(), latest);
            if admitted.get() {
                headless_calls.set(headless_calls.get() + 1);
            }
            assert_eq!(headless_calls.get(), usize::from(latest));
        }
    }

    #[test]
    fn antigravity_policy_valid_read_restore_failure_does_not_retire_admission() {
        use std::cell::Cell;

        for latest in [false, true] {
            let admitted = Cell::new(!latest);
            let writes = Cell::new(0);
            let result = super::apply_collection_policy_from_settings_with_retirement(
                &Tool::Antigravity,
                Ok(collection_test_settings(&Tool::Antigravity, latest)),
                |enabled| {
                    admitted.set(enabled);
                    writes.set(writes.get() + 1);
                    anyhow::bail!("synthetic original statusline restore failed")
                },
                || panic!("binding failure is not a current-policy read failure"),
            );
            assert!(result.is_err());
            assert_eq!(writes.get(), 1);
            assert_eq!(admitted.get(), latest);
        }
    }

    #[test]
    fn antigravity_policy_read_retirement_preserves_other_tool_error_contracts() {
        use std::cell::Cell;

        for tool in [Tool::Claude, Tool::Codex, Tool::Grok, Tool::Cursor] {
            let applied = Cell::new(None);
            let result = super::apply_collection_policy_from_settings_with_retirement(
                &tool,
                Err(anyhow::anyhow!("synthetic settings read failed")),
                |enabled| {
                    applied.set(Some(enabled));
                    Ok(())
                },
                || panic!("other tools must not retire Antigravity admission"),
            );
            assert_eq!(
                result.unwrap_err().to_string(),
                "synthetic settings read failed"
            );
            let broker = matches!(tool, Tool::Codex | Tool::Grok);
            assert_eq!(applied.get(), broker.then_some(false));
            super::apply_collection_policy_from_settings_with_retirement(
                &tool,
                Ok(collection_test_settings(&tool, true)),
                |enabled| {
                    applied.set(Some(enabled));
                    Ok(())
                },
                || panic!("successful other-tool apply must not retire Antigravity"),
            )
            .unwrap();
            assert_eq!(applied.get(), Some(true));
        }
        for tool in [Tool::Codex, Tool::Grok] {
            let error = super::apply_collection_policy_from_settings_with_retirement(
                &tool,
                Err(anyhow::anyhow!("synthetic settings read failed")),
                |_| anyhow::bail!("synthetic broker disable failed"),
                || panic!("broker errors must not retire Antigravity admission"),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "could not disable collection policy");
            assert_eq!(
                error.root_cause().to_string(),
                "synthetic broker disable failed"
            );
        }
    }

    #[test]
    fn collection_policy_cursor_range_change_uses_final_settings_without_rebinding() {
        let before = collection_test_settings(&Tool::Cursor, true);
        let mut after = before.clone();
        after.activity_weeks = if before.activity_weeks == 4 { 12 } else { 4 };
        let changes = super::CollectionChanges::between(
            super::CollectionPolicy::from_settings(&before),
            super::CollectionPolicy::from_settings(&after),
        );
        assert!(changes.cursor_activity_range_changed);
        assert!(!changes.any());
        after.show_cursor = false;
        let disabled = super::CollectionChanges::between(
            super::CollectionPolicy::from_settings(&before),
            super::CollectionPolicy::from_settings(&after),
        );
        assert!(!disabled.cursor_activity_range_changed);
        assert_eq!(disabled.transition(&Tool::Cursor), Some(false));
    }

    fn collection_transaction_fixture() -> (
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let root = std::env::temp_dir().join(format!(
            "juice-policy-transaction-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let home = root.join("home");
        let data = root.join("data");
        let path = data.join("settings.json");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(home.join(".claude/settings.json"),
            br#"{"statusLine":{"type":"command","command":"echo original","extra":"keep"},"other":"keep"}"#).unwrap();
        collection_test_settings(&Tool::Claude, false)
            .save_to(&path)
            .unwrap();
        (root, home, data, path)
    }

    fn apply_fixture_claude(
        home: &std::path::Path,
        data: &std::path::Path,
        enabled: bool,
    ) -> anyhow::Result<()> {
        if enabled {
            Settings::install_statusline_wrap_at(home, data, "C:/Juice/agentjuice-statusline.exe")
        } else {
            Settings::restore_statusline_if_installed_at(home, data)
        }
    }

    #[test]
    fn collection_transaction_success_preserves_original_claude_subtree() {
        let (root, home, data, path) = collection_transaction_fixture();
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join(".claude/settings.json")).unwrap())
                .unwrap();
        for enabled in [true, false] {
            let (saved, changes) = super::persist_settings_with_collection_policy(
                |edit| Settings::try_update_at(&path, edit),
                |current| {
                    current.show_claude = enabled;
                    Ok(())
                },
                || Settings::try_load_from(&path),
                |enabled| apply_fixture_claude(&home, &data, enabled),
            )
            .unwrap();
            assert_eq!(saved.show_claude, enabled);
            assert_eq!(changes.transition(&Tool::Claude), Some(enabled));
        }
        let restored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join(".claude/settings.json")).unwrap())
                .unwrap();
        assert_eq!(restored, original);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collection_transaction_failed_write_restores_persisted_claude_policy() {
        for desired in [true, false] {
            let (root, home, data, path) = collection_transaction_fixture();
            Settings::update_at(&path, |s| s.show_claude = !desired).unwrap();
            apply_fixture_claude(&home, &data, !desired).unwrap();
            let before = std::fs::read(&path).unwrap();
            let original: serde_json::Value =
                serde_json::from_slice(&std::fs::read(home.join(".claude/settings.json")).unwrap())
                    .unwrap();
            let mut applied = Vec::new();
            let result = super::persist_settings_with_collection_policy(
                |edit| {
                    let mut current = Settings::try_load_from(&path)?;
                    edit(&mut current)?;
                    anyhow::bail!("fixture storage write failed")
                },
                |s| {
                    s.show_claude = desired;
                    Ok(())
                },
                || Settings::try_load_from(&path),
                |enabled| {
                    applied.push(enabled);
                    apply_fixture_claude(&home, &data, enabled)
                },
            );
            assert!(result.is_err());
            assert_eq!(applied, [desired, !desired]);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let restored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(home.join(".claude/settings.json")).unwrap())
                    .unwrap();
            assert_eq!(restored, original);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn collection_transaction_rollback_reads_latest_persisted_state_not_old_baseline() {
        for desired in [true, false] {
            let (root, _home, _data, path) = collection_transaction_fixture();
            Settings::update_at(&path, |s| s.show_claude = !desired).unwrap();
            let mut applied = Vec::new();
            let result = super::persist_settings_with_collection_policy(
                |edit| {
                    let mut current = Settings::try_load_from(&path)?;
                    edit(&mut current)?;
                    // Simulate an external successful write before the failed caller recovers.
                    Settings::update_at(&path, |s| s.show_claude = desired)?;
                    anyhow::bail!("fixture stale caller failed")
                },
                |s| {
                    s.show_claude = desired;
                    Ok(())
                },
                || Settings::try_load_from(&path),
                |enabled| {
                    applied.push(enabled);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(applied, [desired, desired]);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn collection_transaction_failed_mutation_never_changes_claude_files() {
        let mut applied = false;
        let result = super::persist_settings_with_collection_policy(
            |edit| {
                let mut current = Settings::default();
                edit(&mut current)?;
                Ok(current)
            },
            |_| anyhow::bail!("fixture validation failed"),
            || panic!("rollback must not read settings before any side effect"),
            |_| {
                applied = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!applied);
    }

    #[test]
    fn collection_transaction_rollback_rejects_changed_user_statusline() {
        let (root, home, data, path) = collection_transaction_fixture();
        let user_change = br#"{"statusLine":{"command":"echo new-user"},"other":"new-user"}"#;
        let result = super::persist_settings_with_collection_policy(
            |edit| {
                let mut current = Settings::try_load_from(&path)?;
                edit(&mut current)?;
                std::fs::write(home.join(".claude/settings.json"), user_change)?;
                anyhow::bail!("fixture storage write failed")
            },
            |s| {
                s.show_claude = true;
                Ok(())
            },
            || Settings::try_load_from(&path),
            |enabled| apply_fixture_claude(&home, &data, enabled),
        );
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("rollback failed"));
        assert_eq!(
            std::fs::read(home.join(".claude/settings.json")).unwrap(),
            user_change
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collection_transaction_apply_failure_rejects_save_and_repairs_original_policy() {
        let (root, home, data, path) = collection_transaction_fixture();
        let before = std::fs::read(&path).unwrap();
        let mut applied = Vec::new();
        let result = super::persist_settings_with_collection_policy(
            |edit| Settings::try_update_at(&path, edit),
            |current| {
                current.show_claude = true;
                Ok(())
            },
            || Settings::try_load_from(&path),
            |enabled| {
                applied.push(enabled);
                if enabled {
                    anyhow::bail!("fixture connection failed")
                }
                apply_fixture_claude(&home, &data, enabled)
            },
        );
        assert!(result.is_err());
        assert_eq!(applied, [true, false]);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collection_transaction_keeps_settings_gate_through_write_failure_and_recovery() {
        let held = || {
            assert!(matches!(
                super::TASKBAR_SETTINGS_WRITE_GATE.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ))
        };
        let mut applied = Vec::new();
        let result = super::with_taskbar_settings_read(|_| {
            super::persist_settings_with_collection_policy(
                |edit| {
                    held();
                    let mut current = collection_test_settings(&Tool::Claude, false);
                    edit(&mut current)?;
                    anyhow::bail!("fixture write failed")
                },
                |current| {
                    current.show_claude = true;
                    Ok(())
                },
                || {
                    held();
                    Ok(collection_test_settings(&Tool::Claude, false))
                },
                |enabled| {
                    held();
                    applied.push(enabled);
                    Ok(())
                },
            )
        });
        assert!(result.is_err());
        assert_eq!(applied, [true, false]);
    }

    #[test]
    fn antigravity_commit_transition_follows_current_merge_in_both_directions() {
        for target in [true, false] {
            let edit_baseline = Settings {
                show_antigravity: !target,
                ..Settings::default()
            };
            let edit_request = Settings {
                show_antigravity: target,
                ..Settings::default()
            };
            let preflight = edit_request.clone();
            let planned =
                super::merge_settings_edits(&preflight, Some(&edit_baseline), &edit_request)
                    .unwrap();
            assert_eq!(planned.show_antigravity, preflight.show_antigravity);
            let mut current = edit_baseline.clone();
            let transition =
                super::mutate_settings_with_collection_changes(&mut current, |current| {
                    *current =
                        super::merge_settings_edits(current, Some(&edit_baseline), &edit_request)?;
                    Ok(())
                })
                .unwrap()
                .transition(&Tool::Antigravity);
            assert_eq!(transition, Some(target));
            assert_eq!(transition == Some(true), target);
            assert_eq!(current.show_antigravity, target);
            let mut applied = None;
            if transition.is_some() {
                super::reconcile_current_antigravity_cli(
                    || Ok(current.clone()),
                    |enabled| {
                        applied = Some(enabled);
                        Ok(())
                    },
                    |failed| assert!(!failed),
                )
                .unwrap();
            }
            assert_eq!(applied, Some(target));
        }
    }

    #[test]
    fn antigravity_commit_noop_skips_binding_changes() {
        for target in [true, false] {
            let edit_baseline = Settings {
                show_antigravity: !target,
                ..Settings::default()
            };
            let edit_request = Settings {
                show_antigravity: target,
                ..Settings::default()
            };
            let mut current = edit_request.clone();
            let transition =
                super::mutate_settings_with_collection_changes(&mut current, |current| {
                    *current =
                        super::merge_settings_edits(current, Some(&edit_baseline), &edit_request)?;
                    Ok(())
                })
                .unwrap()
                .transition(&Tool::Antigravity);
            assert_eq!(transition, None);
            assert_eq!(current.show_antigravity, target);
        }
    }

    #[test]
    fn antigravity_commit_unrelated_edit_preserves_concurrent_toggle_without_rebinding() {
        for enabled in [true, false] {
            let edit_baseline = Settings {
                show_antigravity: !enabled,
                ..Settings::default()
            };
            let edit_request = Settings {
                theme: "dark".into(),
                ..edit_baseline.clone()
            };
            let mut current = Settings {
                show_antigravity: enabled,
                ..edit_baseline.clone()
            };
            let transition =
                super::mutate_settings_with_collection_changes(&mut current, |current| {
                    *current =
                        super::merge_settings_edits(current, Some(&edit_baseline), &edit_request)?;
                    Ok(())
                })
                .unwrap()
                .transition(&Tool::Antigravity);
            assert_eq!(transition, None);
            assert_eq!(current.show_antigravity, enabled);
            assert_eq!(current.theme, "dark");
        }
    }

    #[test]
    fn antigravity_commit_metadata_matches_persisted_toggle_after_intervening_save() {
        for target in [true, false] {
            let root = std::env::temp_dir().join(format!(
                "juice-antigravity-commit-{}-{}-{target}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("settings.json");
            let edit_baseline = Settings {
                show_antigravity: !target,
                ..Settings::default()
            };
            let edit_request = Settings {
                show_antigravity: target,
                ..Settings::default()
            };
            edit_request.save_to(&path).unwrap();
            let preflight = Settings::try_load_from(&path).unwrap();
            assert_eq!(preflight.show_antigravity, target);
            Settings::update_at(&path, |current| current.show_antigravity = !target).unwrap();
            let mut transition = None;
            let saved = Settings::try_update_at(&path, |current| {
                transition = super::mutate_settings_with_collection_changes(current, |current| {
                    *current =
                        super::merge_settings_edits(current, Some(&edit_baseline), &edit_request)?;
                    Ok(())
                })?
                .transition(&Tool::Antigravity);
                Ok(())
            })
            .unwrap();
            assert_eq!(transition, Some(target));
            assert_eq!(saved.show_antigravity, target);
            assert_eq!(
                Settings::try_load_from(&path).unwrap().show_antigravity,
                target
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn antigravity_commit_failed_mutation_does_not_publish_transition_or_save() {
        let root = std::env::temp_dir().join(format!(
            "juice-antigravity-commit-error-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        let initial = Settings {
            show_antigravity: false,
            ..Settings::default()
        };
        initial.save_to(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let mut transition = None;
        let result = Settings::try_update_at(&path, |current| {
            transition = super::mutate_settings_with_collection_changes(current, |current| {
                current.show_antigravity = true;
                anyhow::bail!("fixture mutation failed")
            })?
            .transition(&Tool::Antigravity);
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(transition, None);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delayed_antigravity_cli_reconciliation_uses_current_setting_in_both_directions() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Barrier,
        };
        for latest in [true, false] {
            let current = Arc::new(AtomicBool::new(!latest));
            let applied = Arc::new(AtomicBool::new(!latest));
            let failed = Arc::new(AtomicBool::new(true));
            let ready = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let worker_current = current.clone();
            let worker_applied = applied.clone();
            let worker_failed = failed.clone();
            let worker_ready = ready.clone();
            let worker_release = release.clone();
            let worker = std::thread::spawn(move || {
                let old_startup_setting = worker_current.load(Ordering::SeqCst);
                worker_ready.wait();
                worker_release.wait();
                super::reconcile_current_antigravity_cli(
                    || {
                        let setting = worker_current.load(Ordering::SeqCst);
                        assert_ne!(setting, old_startup_setting);
                        Ok(Settings {
                            show_antigravity: setting,
                            ..Settings::default()
                        })
                    },
                    |enabled| {
                        worker_applied.store(enabled, Ordering::SeqCst);
                        Ok(())
                    },
                    |error| worker_failed.store(error, Ordering::SeqCst),
                )
            });
            ready.wait();
            super::with_taskbar_settings_read(|_| {
                current.store(latest, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
            super::reconcile_current_antigravity_cli(
                || {
                    Ok(Settings {
                        show_antigravity: latest,
                        ..Settings::default()
                    })
                },
                |enabled| {
                    applied.store(enabled, Ordering::SeqCst);
                    Ok(())
                },
                |error| failed.store(error, Ordering::SeqCst),
            )
            .unwrap();
            release.wait();
            worker.join().unwrap().unwrap();
            assert_eq!(applied.load(Ordering::SeqCst), latest);
            assert!(!failed.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn antigravity_cli_reconciliation_holds_settings_gate_through_health_publication() {
        let mut stages = Vec::new();
        let gate_is_held = || {
            assert!(matches!(
                super::TASKBAR_SETTINGS_WRITE_GATE.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ))
        };
        super::reconcile_current_antigravity_cli(
            || {
                gate_is_held();
                Ok(Settings::default())
            },
            |_| {
                gate_is_held();
                Ok(())
            },
            |failed| {
                gate_is_held();
                stages.push(failed);
            },
        )
        .unwrap();
        assert_eq!(stages, [false]);
    }

    #[test]
    fn antigravity_cli_reconciliation_settings_failure_does_not_change_binding() {
        let mut called = false;
        let mut failed = false;
        let result = super::reconcile_current_antigravity_cli(
            || anyhow::bail!("fixture settings unavailable"),
            |_| {
                called = true;
                Ok(())
            },
            |error| failed = error,
        );
        assert!(result.is_err());
        assert!(!called);
        assert!(failed);
    }

    #[test]
    fn antigravity_cli_reconciliation_clears_previous_error_only_after_success() {
        let mut failed = false;
        assert!(super::reconcile_current_antigravity_cli(
            || Ok(Settings::default()),
            |_| anyhow::bail!("fixture binding unavailable"),
            |error| failed = error,
        )
        .is_err());
        assert!(failed);
        super::reconcile_current_antigravity_cli(
            || Ok(Settings::default()),
            |_| Ok(()),
            |error| failed = error,
        )
        .unwrap();
        assert!(!failed);
    }

    #[test]
    fn late_cursor_callback_preserves_a_tool_enabled_while_waiting() {
        use std::sync::{mpsc, Arc, Barrier, Mutex};
        let initial_settings = Settings {
            show_claude: false,
            show_codex: false,
            show_grok: false,
            show_cursor: true,
            show_antigravity: false,
            ..Settings::default()
        };
        let settings = Arc::new(Mutex::new(initial_settings.clone()));
        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let cache = Arc::new(Mutex::new(None));
        let mut cursor = status_for_signature("late-cursor");
        cursor.tool = Tool::Cursor;
        super::cache_cursor_status(
            &cache,
            chrono::Utc::now(),
            Some(cursor.clone()),
            Some(super::CursorStatusSource::Agent(None)),
            None,
            &initial_settings,
        );
        let release = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = mpsc::channel();
        let (sender, receiver) = mpsc::sync_channel(0);
        let worker_settings = settings.clone();
        let worker_coordinator = coordinator.clone();
        let worker_release = release.clone();
        let worker = std::thread::spawn(move || {
            assert!(!worker_settings.lock().unwrap().show_grok);
            started_tx.send(()).unwrap();
            worker_release.wait();
            let mut published = None;
            super::deliver_cursor_result(
                sender,
                super::CursorCollectionResult {
                    statuses: vec![cursor],
                    attempted: true,
                },
                |result| {
                    super::publish_late_cursor_result(
                        result,
                        || Ok(worker_settings.lock().unwrap().clone()),
                        || worker_coordinator.last_result(),
                        &cache,
                        |snapshot| {
                            assert!(matches!(
                                super::TASKBAR_SETTINGS_WRITE_GATE.try_lock(),
                                Err(std::sync::TryLockError::WouldBlock)
                            ));
                            published = Some(snapshot);
                        },
                    )
                    .unwrap();
                },
            );
            published.unwrap()
        });
        started_rx.recv().unwrap();
        // The original waiter has timed out before a newer settings/collection pass.
        drop(receiver);
        super::with_taskbar_settings_read(|_| {
            settings.lock().unwrap().show_grok = true;
            Ok(())
        })
        .unwrap();
        let mut grok = status_for_signature("newly-enabled-grok");
        grok.tool = Tool::Grok;
        let disabled = status_for_signature("disabled-claude");
        coordinator.run(false, || vec![grok.clone(), disabled]);
        assert!(
            super::filter_enabled_statuses(coordinator.last_result(), &initial_settings).is_empty()
        );
        release.wait();
        let published = worker.join().unwrap();
        assert_eq!(published.len(), 2);
        assert_eq!(published[0], grok);
        assert_eq!(published[1].session_id, "late-cursor");
    }

    #[test]
    fn on_time_cursor_followup_publication_and_reply_use_current_settings_and_cache() {
        let old_settings = Settings {
            show_claude: false,
            show_codex: false,
            show_grok: false,
            show_cursor: true,
            show_antigravity: false,
            ..Settings::default()
        };
        let mut cursor = status_for_signature("on-time-cursor");
        cursor.tool = Tool::Cursor;
        let cache = std::sync::Mutex::new(None);
        super::cache_cursor_status(
            &cache,
            chrono::Utc::now(),
            Some(cursor.clone()),
            Some(super::CursorStatusSource::Agent(None)),
            None,
            &old_settings,
        );
        let mut grok = status_for_signature("new-current-grok");
        grok.tool = Tool::Grok;
        let latest = vec![grok.clone(), cursor.clone()];
        let stale_reply = super::filter_enabled_statuses(latest.clone(), &old_settings);
        assert_eq!(stale_reply, vec![cursor.clone()]);
        let current_settings = Settings {
            show_grok: true,
            ..old_settings
        };
        let mut first_emitted = None;
        super::publish_late_cursor_result(
            super::CursorCollectionResult {
                attempted: true,
                statuses: stale_reply,
            },
            || Ok(current_settings.clone()),
            || latest.clone(),
            &cache,
            |snapshot| first_emitted = Some(snapshot),
        )
        .unwrap();
        let reply = super::consume_current_collection_snapshot(
            || Ok(current_settings.clone()),
            || latest.clone(),
            &cache,
            |snapshot| snapshot,
        )
        .unwrap();
        let followup = super::consume_current_collection_snapshot(
            || Ok(current_settings),
            || latest.clone(),
            &cache,
            |snapshot| {
                assert!(matches!(
                    super::TASKBAR_SETTINGS_WRITE_GATE.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
                snapshot
            },
        )
        .unwrap();
        assert_eq!(first_emitted.unwrap(), latest);
        assert_eq!(reply, latest);
        assert_eq!(followup, latest);
    }

    #[test]
    fn final_collection_boundary_filters_disabled_tools_and_retires_failed_reads() {
        let mut grok = status_for_signature("disabled-grok");
        grok.tool = Tool::Grok;
        let cache = std::sync::Mutex::new(None);
        let snapshot = super::consume_current_collection_snapshot(
            || {
                Ok(Settings {
                    show_grok: false,
                    ..Settings::default()
                })
            },
            || vec![grok],
            &cache,
            |snapshot| snapshot,
        )
        .unwrap();
        assert!(snapshot.is_empty());
        let error = super::consume_current_collection_snapshot(
            || anyhow::bail!("synthetic settings read failure"),
            || panic!("unreadable settings must not read collection"),
            &cache,
            |_| panic!("unreadable settings must not publish"),
        );
        assert!(error.is_err());
        let error = super::consume_current_collection_snapshot(
            || Ok(Settings::default()),
            || {
                super::mark_taskbar_settings_changed();
                Vec::new()
            },
            &cache,
            |_| panic!("changed generation must not publish"),
        );
        assert!(error.is_err());
    }

    #[test]
    fn late_cursor_callback_skips_disabled_unattempted_or_unreadable_settings() {
        for (attempted, enabled, load_fails) in [
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let (sender, receiver) = std::sync::mpsc::sync_channel(0);
            drop(receiver);
            let mut settings_reads = 0;
            let mut outcome = None;
            super::deliver_cursor_result(
                sender,
                super::CursorCollectionResult {
                    statuses: vec![status_for_signature("late")],
                    attempted,
                },
                |result| {
                    outcome = Some(super::publish_late_cursor_result(
                        result,
                        || {
                            settings_reads += 1;
                            if load_fails {
                                anyhow::bail!("synthetic settings read failure");
                            }
                            Ok(Settings {
                                show_cursor: enabled,
                                ..Settings::default()
                            })
                        },
                        || panic!("retired callbacks must not read coordinator state"),
                        &std::sync::Mutex::new(None),
                        |_| panic!("retired callbacks must not publish a snapshot"),
                    ));
                },
            );
            assert_eq!(settings_reads, usize::from(attempted));
            assert_eq!(outcome.unwrap().is_err(), load_fails);
        }
    }

    #[test]
    fn late_cursor_callback_rechecks_settings_generation_before_publishing() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        drop(receiver);
        super::deliver_cursor_result(
            sender,
            super::CursorCollectionResult {
                statuses: Vec::new(),
                attempted: true,
            },
            |result| {
                super::publish_late_cursor_result(
                    result,
                    || {
                        Ok(Settings {
                            show_cursor: true,
                            ..Settings::default()
                        })
                    },
                    || {
                        super::mark_taskbar_settings_changed();
                        Vec::new()
                    },
                    &std::sync::Mutex::new(None),
                    |_| panic!("a changed settings generation must retire the snapshot"),
                )
                .unwrap();
            },
        );
    }

    #[test]
    fn late_cursor_callback_keeps_provider_current_filter_after_recovery_or_invalidation() {
        for invalidated in [false, true] {
            let settings = Settings {
                show_grok: true,
                show_cursor: true,
                ..Settings::default()
            };
            let cache = std::sync::Mutex::new(None);
            let mut old = status_for_signature("old-cursor");
            old.tool = Tool::Cursor;
            let mut fresh = old.clone();
            fresh.session_id = "current-cursor".into();
            // A retired result must not displace the provider's current sample,
            // even if its timestamp sorts after the recovered account's sample.
            old.captured_at = "2026-07-08T00:00:00Z".into();
            super::cache_cursor_status(
                &cache,
                chrono::Utc::now(),
                Some(fresh.clone()),
                Some(super::CursorStatusSource::Agent(None)),
                None,
                &settings,
            );
            if invalidated {
                super::cache_cursor_status(
                    &cache,
                    chrono::Utc::now(),
                    None,
                    None,
                    Some(super::CollectionErrorKind::SourceChanged),
                    &settings,
                );
            }
            let mut grok = status_for_signature("current-grok");
            grok.tool = Tool::Grok;
            let (sender, receiver) = std::sync::mpsc::sync_channel(0);
            drop(receiver);
            let mut published = None;
            super::deliver_cursor_result(
                sender,
                super::CursorCollectionResult {
                    statuses: vec![old],
                    attempted: true,
                },
                |result| {
                    super::publish_late_cursor_result(
                        result,
                        || Ok(settings),
                        || vec![grok.clone(), fresh.clone()],
                        &cache,
                        |snapshot| published = Some(snapshot),
                    )
                    .unwrap();
                },
            );
            let expected = if invalidated {
                vec![grok]
            } else {
                vec![grok, fresh]
            };
            assert_eq!(published.unwrap(), expected);
        }
    }

    #[test]
    fn cursor_invalidated_provider_cannot_resurrect_inflight_timeout_or_late_values() {
        use std::sync::{mpsc, Arc, Barrier};
        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let cache = Arc::new(std::sync::Mutex::new(None));
        let settings = Settings::default();
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let mut old = status_for_signature("old-account");
        old.tool = Tool::Cursor;
        super::cache_cursor_status(
            &cache,
            chrono::Utc::now(),
            Some(old.clone()),
            Some(super::CursorStatusSource::Dashboard(scope)),
            None,
            &settings,
        );
        coordinator.run_if_idle_with_flag(|| vec![old.clone()]);
        let release = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = mpsc::channel();
        let worker_coordinator = coordinator.clone();
        let worker_cache = cache.clone();
        let worker_release = release.clone();
        let worker = std::thread::spawn(move || {
            worker_coordinator.run_if_idle_with_flag(|| {
                super::cache_cursor_status(
                    &worker_cache,
                    chrono::Utc::now(),
                    None,
                    None,
                    Some(super::CollectionErrorKind::SourceChanged),
                    &settings,
                );
                started_tx.send(()).unwrap();
                worker_release.wait();
                Vec::new()
            })
        });
        started_rx.recv().unwrap();
        let (stale, attempted) =
            coordinator.run_if_idle_with_flag(|| panic!("must reuse in-flight lane"));
        assert!(!attempted && !stale.is_empty());
        assert!(super::filter_current_cursor_samples(stale, &cache).is_empty());
        assert!(super::filter_current_cursor_samples(coordinator.last_result(), &cache).is_empty());
        let (sender, receiver) = mpsc::sync_channel(0);
        drop(receiver);
        let mut late = None;
        super::deliver_cursor_result(sender, vec![old.clone()], |values| {
            late = Some(super::filter_current_cursor_samples(values, &cache));
        });
        assert!(late.unwrap().is_empty());
        let other = status_for_signature("other-tool");
        assert_eq!(
            super::filter_current_cursor_samples(vec![old, other.clone()], &cache),
            vec![other]
        );
        release.wait();
        worker.join().unwrap();
    }

    #[test]
    fn cursor_current_samples_preserve_age_and_drop_previous_account_after_recovery() {
        let cache = std::sync::Mutex::new(None);
        let settings = Settings::default();
        let scope = crate::cursor_dashboard::AccountScope {
            user_id: 7,
            team_id: None,
        };
        let mut old = status_for_signature("old-account");
        old.tool = Tool::Cursor;
        super::cache_cursor_status(
            &cache,
            chrono::Utc::now(),
            Some(old.clone()),
            Some(super::CursorStatusSource::Dashboard(scope)),
            Some(super::CollectionErrorKind::Transport),
            &settings,
        );
        let mut aged = old.clone();
        aged.session.active = false;
        assert_eq!(
            super::filter_current_cursor_samples(vec![aged.clone()], &cache),
            vec![aged]
        );
        let mut fresh = old.clone();
        fresh.session_id = "new-account".into();
        let new_scope = crate::cursor_dashboard::AccountScope {
            user_id: 8,
            ..scope
        };
        super::cache_cursor_status(
            &cache,
            chrono::Utc::now(),
            Some(fresh.clone()),
            Some(super::CursorStatusSource::Dashboard(new_scope)),
            None,
            &settings,
        );
        assert_eq!(
            super::filter_current_cursor_samples(vec![old, fresh.clone()], &cache),
            vec![fresh]
        );
    }

    #[test]
    fn cursor_collection_lane_returns_last_good_without_joining_an_inflight_attempt() {
        use std::sync::{mpsc, Arc, Barrier};

        let coordinator = Arc::new(super::CollectionCoordinator::default());
        let (initial, initial_attempted) =
            coordinator.run_if_idle_with_flag(|| vec![status_for_signature("cursor-last")]);
        assert!(initial_attempted);
        assert_eq!(initial[0].session_id, "cursor-last");

        let release = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = mpsc::channel();
        let worker_coordinator = Arc::clone(&coordinator);
        let worker_release = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            worker_coordinator.run_if_idle_with_flag(|| {
                started_tx.send(()).unwrap();
                worker_release.wait();
                vec![status_for_signature("cursor-fresh")]
            })
        });
        started_rx.recv().unwrap();

        let started = std::time::Instant::now();
        let (duplicate, duplicate_attempted) =
            coordinator.run_if_idle_with_flag(|| panic!("must not run"));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(!duplicate_attempted);
        assert_eq!(duplicate[0].session_id, "cursor-last");

        release.wait();
        let (fresh, fresh_attempted) = worker.join().unwrap();
        assert!(fresh_attempted);
        assert_eq!(fresh[0].session_id, "cursor-fresh");
    }

    #[test]
    fn cursor_result_uses_late_callback_only_after_the_waiter_is_gone() {
        let (late_sender, late_receiver) = std::sync::mpsc::sync_channel(0);
        drop(late_receiver);
        let mut late = None;
        super::deliver_cursor_result(late_sender, "late", |value| late = Some(value));
        assert_eq!(late, Some("late"));

        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        let received = std::thread::spawn(move || receiver.recv().unwrap());
        let mut unexpected_late = false;
        super::deliver_cursor_result(sender, "on-time", |_| unexpected_late = true);
        assert_eq!(received.join().unwrap(), "on-time");
        assert!(!unexpected_late);
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
        let initial = coordinator.run(false, || vec![status_for_signature("last-good")]);
        assert_eq!(initial[0].session_id, "last-good");
        let release = Arc::new(Barrier::new(2));
        let (started_tx, started_rx) = mpsc::channel();
        let panic_coordinator = Arc::clone(&coordinator);
        let panic_release = Arc::clone(&release);
        let panicking = std::thread::spawn(move || {
            std::panic::catch_unwind(|| {
                panic_coordinator.run(true, || {
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
                waiter_coordinator.run(true, || vec![status_for_signature("must-not-run")]);
            done_tx.send(result).unwrap();
        });
        while coordinator.state.lock().unwrap().joined_waiters == 0 {
            std::thread::yield_now();
        }
        release.wait();

        assert_eq!(
            panicking.join().unwrap().unwrap()[0].session_id,
            "last-good"
        );
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()[0]
                .session_id,
            "last-good"
        );
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
    fn panel_save_always_preserves_concurrent_layout_profile_state() {
        let topology = TaskbarTopology::from(vec!["monitor-office".to_string()]);
        let mut baseline = Settings::default();
        super::set_taskbar_target(&mut baseline, "claude", "monitor-office", 0.2);

        let mut current = baseline.clone();
        assert!(super::record_taskbar_layout_profile(
            &mut current,
            &topology
        ));
        let mut requested = baseline.clone();
        requested.theme = "dark".into();
        requested.claude_taskbar_offset_ratio = 0.35;

        super::preserve_concurrent_taskbar_state(&current, &baseline, &mut requested, false);

        assert_eq!(requested.theme, "dark");
        assert_eq!(requested.claude_taskbar_offset_ratio, 0.35);
        assert!(requested.taskbar_layout_memory_initialized);
        assert_eq!(
            requested.taskbar_layout_profiles,
            current.taskbar_layout_profiles
        );

        requested.bar_mode = "compact".into();
        assert!(super::record_taskbar_layout_profile(
            &mut requested,
            &topology
        ));
        let saved_profile = requested.taskbar_layout_profile(&topology).unwrap();
        assert_eq!(
            saved_profile.presentation.as_ref().unwrap().bar_mode,
            "compact"
        );
        assert!(saved_profile.appearance.is_none());

        let profile_baseline = current.clone();
        let mut cleared = profile_baseline.clone();
        cleared.taskbar_layout_profiles.clear();
        let mut stale_requested = profile_baseline.clone();
        stale_requested.theme = "light".into();
        super::preserve_concurrent_taskbar_state(
            &cleared,
            &profile_baseline,
            &mut stale_requested,
            false,
        );
        assert_eq!(stale_requested.theme, "light");
        assert!(stale_requested.taskbar_layout_profiles.is_empty());

        super::set_taskbar_target(&mut current, "claude", "monitor-office", 0.8);
        super::preserve_concurrent_taskbar_state(&current, &baseline, &mut requested, false);
        assert_eq!(requested.claude_taskbar_offset_ratio, 0.8);
    }

    #[test]
    fn panel_save_preserves_the_latest_backend_taskbar_pause_state() {
        let baseline = Settings {
            taskbar_bars_paused: true,
            ..Settings::default()
        };

        let current = baseline.clone();
        let mut stale_panel_request = Settings::default();
        super::preserve_concurrent_taskbar_state(
            &current,
            &baseline,
            &mut stale_panel_request,
            false,
        );
        assert!(stale_panel_request.taskbar_bars_paused);

        let mut resumed = baseline.clone();
        resumed.taskbar_bars_paused = false;
        let mut stale_paused_request = baseline.clone();
        super::preserve_concurrent_taskbar_state(
            &resumed,
            &baseline,
            &mut stale_paused_request,
            false,
        );
        assert!(!stale_paused_request.taskbar_bars_paused);
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
