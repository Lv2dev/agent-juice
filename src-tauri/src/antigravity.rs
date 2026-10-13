//! Read-only Desktop quotas and the CLI's built-in zero-token quota command.
use crate::model::{AccountLimit, AgentStatus, SessionInfo, Tool};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Instant;

pub const COLLECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

#[derive(Default)]
struct Cache {
    enabled: bool,
    suspended: bool,
    generation: u64,
    busy: bool,
    retry_at: Option<Instant>,
    last_started_at: Option<Instant>,
    force_retry_at: Option<Instant>,
    failures: u32,
    result: Option<Result<AgentStatus, Error>>,
}
static CACHE: Mutex<Cache> = Mutex::new(Cache {
    enabled: false,
    suspended: false,
    generation: 0,
    busy: false,
    retry_at: None,
    last_started_at: None,
    force_retry_at: None,
    failures: 0,
    result: None,
});

impl Cache {
    fn set_enabled(&mut self, enabled: bool) -> bool {
        if self.enabled == enabled && !self.suspended {
            return false;
        }
        if self.enabled != enabled {
            self.retry_at = None;
            self.last_started_at = None;
            self.force_retry_at = None;
            self.failures = 0;
        }
        self.enabled = enabled;
        self.suspended = false;
        self.generation = self.generation.wrapping_add(1);
        self.result = None;
        true
    }

    fn suspend_for_settings_failure(&mut self) -> bool {
        if self.suspended {
            return false;
        }
        self.suspended = true;
        self.generation = self.generation.wrapping_add(1);
        self.result = None;
        true
    }
}

pub fn set_enabled(enabled: bool) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.set_enabled(enabled) {
        crate::antigravity_cli::clear_cached_source();
    }
}

pub fn suspend_for_settings_failure() {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.suspend_for_settings_failure() {
        crate::antigravity_cli::clear_cached_source();
    }
}

pub fn cached() -> Option<AgentStatus> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .filter(|status| crate::antigravity_cli::cached_source_alive(status))
        .cloned()
}

pub fn last_error() -> Option<Error> {
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match cache.result.as_ref()? {
        Ok(status) if !crate::antigravity_cli::cached_source_alive(status) => {
            Some(Error::AppRequired)
        }
        Ok(_) => None,
        Err(error) => Some(*error),
    }
}

/// Consumes quota and error from one current completion and source-liveness sample.
///
/// The callback runs synchronously with CACHE locked through its return. It must
/// not re-enter CACHE (including cached/last_error/refresh/setters), query the CLI
/// source again, or perform runtime/provider I/O. Keep it short and do not acquire
/// locks that another CACHE caller can hold. Deferred work returned by the callback
/// is not protected; final payload construction/consumption belongs inside it.
/// External source lifetime is sampled, not locked for the callback's duration.
pub(crate) fn with_current_snapshot<T>(
    consume: impl FnOnce(Option<AgentStatus>, Option<Error>) -> T,
) -> T {
    with_current_cache_snapshot(&CACHE, crate::antigravity_cli::cached_source_alive, consume)
}

fn with_current_cache_snapshot<T>(
    state: &Mutex<Cache>,
    source_alive: impl FnOnce(&AgentStatus) -> bool,
    consume: impl FnOnce(Option<AgentStatus>, Option<Error>) -> T,
) -> T {
    let cache = state.lock().unwrap_or_else(|e| e.into_inner());
    let (status, error) = if !cache.enabled || cache.suspended {
        (None, None)
    } else {
        match cache.result.as_ref() {
            Some(Ok(status)) if source_alive(status) => (Some(status.clone()), None),
            Some(Ok(_)) => (None, Some(Error::AppRequired)),
            Some(Err(error)) => (None, Some(*error)),
            None => (None, None),
        }
    };
    let consumed = consume(status, error);
    drop(cache);
    consumed
}

pub fn refresh(force: bool, deadline: Instant) -> Option<AgentStatus> {
    refresh_cache(&CACHE, force, |admitted| {
        collect(
            &gethostname::gethostname().to_string_lossy(),
            &chrono::Utc::now().to_rfc3339(),
            deadline,
            admitted,
        )
    })
}

fn refresh_cache(
    state: &Mutex<Cache>,
    force: bool,
    fetch: impl FnOnce(&dyn Fn() -> bool) -> Result<AgentStatus, Error>,
) -> Option<AgentStatus> {
    let (generation, started_at) = {
        let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.enabled || cache.suspended {
            return None;
        }
        let now = Instant::now();
        let force_throttled = force && cache.force_retry_at.is_some_and(|at| now < at);
        if cache.busy || force_throttled || (!force && cache.retry_at.is_some_and(|at| now < at)) {
            return cache
                .result
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .filter(|status| crate::antigravity_cli::cached_source_alive(status))
                .cloned();
        }
        cache.busy = true;
        cache.last_started_at = Some(now);
        (cache.generation, now)
    };
    let admitted = || {
        let cache = state.lock().unwrap_or_else(|e| e.into_inner());
        cache.enabled && !cache.suspended && cache.generation == generation
    };
    let result = fetch(&admitted);
    let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
    cache.busy = false;
    let retired = cache.generation != generation;
    cache.failures = if result.is_ok() {
        0
    } else {
        cache.failures.saturating_add(1)
    };
    let delay = if result
        .as_ref()
        .is_ok_and(|status| status.session_id == crate::antigravity_cli::headless::SESSION_ID)
    {
        crate::antigravity_cli::headless::MIN_REFRESH_INTERVAL.as_secs()
    } else if matches!(result, Err(Error::CliUnavailable)) {
        300u64.saturating_mul(1 << cache.failures.saturating_sub(1).min(3))
    } else if matches!(result, Err(Error::CliLoginRequired)) {
        300
    } else if matches!(result, Err(Error::Unavailable)) {
        60u64.saturating_mul(1 << cache.failures.saturating_sub(1).min(4))
    } else {
        60
    };
    let completed_at = Instant::now();
    let retry_at = completed_at + std::time::Duration::from_secs(delay);
    // Retired account data stays discarded, but its external attempt still counts.
    cache.retry_at = Some(if retired {
        cache.retry_at.map_or(retry_at, |at| at.max(retry_at))
    } else {
        retry_at
    });
    cache.last_started_at = Some(started_at);
    cache.force_retry_at = if retired
        || matches!(result, Err(Error::CliLoginRequired | Error::CliUnavailable))
        || result
            .as_ref()
            .is_ok_and(|status| status.session_id == crate::antigravity_cli::headless::SESSION_ID)
    {
        Some(
            if retired {
                completed_at
            } else {
                cache.last_started_at.unwrap_or(started_at)
            } + std::time::Duration::from_secs(30),
        )
    } else {
        None
    };
    if retired {
        return None;
    }
    // No persisted or last-good account data across errors, closure or a toggle.
    cache.result = Some(result);
    cache
        .result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .filter(|status| crate::antigravity_cli::cached_source_alive(status))
        .cloned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AppRequired,
    LoginRequired,
    Unavailable,
    CliLoginRequired,
    CliUnavailable,
}

fn authenticated(body: &[u8]) -> Result<bool, Error> {
    #[derive(Deserialize)]
    struct AuthResponse {
        #[serde(rename = "authResult")]
        result: AuthResult,
    }
    #[derive(Deserialize)]
    struct AuthResult {
        #[serde(default, rename = "hasValidAuth")]
        valid: bool,
    }
    if body.len() > 1024 * 1024 {
        return Err(Error::Unavailable);
    }
    serde_json::from_slice::<AuthResponse>(body)
        .map(|response| response.result.valid)
        .map_err(|_| Error::Unavailable)
}

#[derive(Deserialize)]
struct Response {
    response: Option<QuotaSummary>,
}

#[derive(Deserialize)]
struct QuotaSummary {
    #[serde(default)]
    groups: Vec<QuotaGroup>,
    #[serde(default)]
    buckets: Vec<Quota>,
}

#[derive(Deserialize)]
struct QuotaGroup {
    #[serde(default)]
    buckets: Vec<Quota>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quota {
    #[serde(default)]
    bucket_id: String,
    #[serde(default, deserialize_with = "remaining_fraction")]
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

fn remaining_fraction<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    f64::deserialize(deserializer).map(Some)
}

pub fn parse_status(body: &[u8], pc_id: &str, captured_at: &str) -> Result<AgentStatus, Error> {
    if body.len() > 1024 * 1024 {
        return Err(Error::Unavailable);
    }
    let response: Response = serde_json::from_slice(body).map_err(|_| Error::Unavailable)?;
    let summary = response.response.ok_or(Error::Unavailable)?;
    if summary.groups.len() > 64
        || summary.buckets.len()
            + summary
                .groups
                .iter()
                .map(|group| group.buckets.len())
                .sum::<usize>()
            > 256
    {
        return Err(Error::Unavailable);
    }
    let mut pools: [Option<Quota>; 2] = [None, None];
    for mut quota in summary
        .buckets
        .into_iter()
        .chain(summary.groups.into_iter().flat_map(|group| group.buckets))
    {
        let index = match quota.bucket_id.as_str() {
            "gemini-5h" => 0,
            "gemini-weekly" => 1,
            _ => continue,
        };
        // Proto3 omits a scalar zero. Require a reset to distinguish an exhausted
        // quota from an empty/default quota object supplied by the GUI.
        let remaining = quota
            .remaining_fraction
            .or_else(|| quota.reset_time.as_ref().map(|_| 0.0))
            .ok_or(Error::Unavailable)?;
        quota.remaining_fraction = Some(remaining);
        if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
            return Err(Error::Unavailable);
        }
        if let Some(reset) = &quota.reset_time {
            chrono::DateTime::parse_from_rfc3339(reset).map_err(|_| Error::Unavailable)?;
        }
        if pools[index].is_some() {
            return Err(Error::Unavailable);
        }
        pools[index] = Some(quota);
    }
    if pools.iter().all(Option::is_none) {
        return Err(Error::Unavailable);
    }
    let limit = |quota: Option<Quota>, label: &str| {
        quota.map(|q| AccountLimit {
            label: label.into(),
            used_percent: q.remaining_fraction.map(|r| ((1.0 - r) * 100.0) as f32),
            resets_at: q.reset_time,
        })
    };
    let [primary, secondary] = pools;
    Ok(AgentStatus {
        schema_version: "agent_status.v1".into(),
        pc_id: pc_id.into(),
        tool: Tool::Antigravity,
        session_id: "antigravity-gui".into(),
        captured_at: captured_at.into(),
        primary: limit(primary, "5h"),
        secondary: limit(secondary, "week"),
        session: SessionInfo {
            active: true,
            context_used_percent: None,
        },
        cost_estimate_usd: None,
        approx: false,
    })
}

pub fn collect(
    pc_id: &str,
    captured_at: &str,
    deadline: Instant,
    admitted: &dyn Fn() -> bool,
) -> Result<AgentStatus, Error> {
    #[cfg(windows)]
    {
        resolve_collection_source(
            crate::antigravity_cli::headless::collect(pc_id, captured_at, deadline, admitted)
                .map_err(|error| match error {
                    crate::antigravity_cli::CaptureError::AppRequired => Error::AppRequired,
                    crate::antigravity_cli::CaptureError::LoginRequired => Error::CliLoginRequired,
                    crate::antigravity_cli::CaptureError::Unavailable => Error::CliUnavailable,
                }),
            || {
                if !admitted() {
                    return Err(Error::Unavailable);
                }
                native::collect(pc_id, captured_at, deadline, admitted)
            },
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (pc_id, captured_at, deadline, admitted);
        Err(Error::AppRequired)
    }
}

#[cfg(any(windows, test))]
fn resolve_collection_source(
    cli: Result<AgentStatus, Error>,
    desktop: impl FnOnce() -> Result<AgentStatus, Error>,
) -> Result<AgentStatus, Error> {
    match cli {
        // An installed CLI owns this read, including auth/transport failures.
        // Switching on those errors could silently display a different account.
        Err(Error::AppRequired) => desktop(),
        result => result,
    }
}

#[cfg(windows)]
mod native;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_takes_priority_without_querying_desktop_and_keeps_input_timestamp() {
        let mut cli = parse_status(
            &fixture(serde_json::json!([{"bucketId":"gemini-weekly","remainingFraction":0.5}])),
            "pc",
            "2026-10-06T00:00:00Z",
        )
        .unwrap();
        cli.session_id = crate::antigravity_cli::headless::SESSION_ID.into();
        cli.session.active = false;
        let selected = resolve_collection_source(Ok(cli), || {
            panic!("installed CLI must not depend on Desktop auth, transport or lifetime")
        })
        .unwrap();
        assert_eq!(
            selected.session_id,
            crate::antigravity_cli::headless::SESSION_ID
        );
        assert_eq!(selected.captured_at, "2026-10-06T00:00:00Z");
        assert!(!selected.session.active);
        assert!(selected.primary.is_none());
    }

    #[test]
    fn cli_failures_never_switch_to_a_desktop_account() {
        for error in [Error::CliLoginRequired, Error::CliUnavailable] {
            assert_eq!(
                resolve_collection_source(Err(error), || panic!(
                    "CLI source must remain exclusive"
                ))
                .err(),
                Some(error)
            );
        }
    }

    #[test]
    fn desktop_is_collected_only_when_cli_is_not_installed() {
        let gui = parse_status(
            &fixture(serde_json::json!([{"bucketId":"gemini-5h","remainingFraction":1.0}])),
            "pc",
            "2026-10-06T00:00:00Z",
        )
        .unwrap();
        let calls = std::cell::Cell::new(0);
        let selected = resolve_collection_source(Err(Error::AppRequired), || {
            calls.set(calls.get() + 1);
            Ok(gui)
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(selected.session_id, "antigravity-gui");
        assert_eq!(selected.captured_at, "2026-10-06T00:00:00Z");
        assert!(selected.secondary.is_none());
        for error in [Error::AppRequired, Error::LoginRequired, Error::Unavailable] {
            assert_eq!(
                resolve_collection_source(Err(Error::AppRequired), || Err(error)).err(),
                Some(error)
            );
        }
    }

    #[test]
    #[ignore = "requires explicit JUICE_TEST_ANTIGRAVITY_HEADLESS=1 and the existing CLI login"]
    fn live_collection_prefers_cli_without_a_desktop_dependency() {
        assert_eq!(
            std::env::var("JUICE_TEST_ANTIGRAVITY_HEADLESS").as_deref(),
            Ok("1")
        );
        let captured = chrono::Utc::now().to_rfc3339();
        let status = collect(
            "live-fixture",
            &captured,
            Instant::now() + COLLECTION_TIMEOUT,
            &|| true,
        )
        .expect("CLI-first read-only quota through the actual collection entry point");
        assert_eq!(
            status.session_id,
            crate::antigravity_cli::headless::SESSION_ID
        );
        assert_eq!(status.captured_at, captured);
        assert!(!status.approx);
        assert!(status.primary.is_some() || status.secondary.is_some());
        assert!(status.session.context_used_percent.is_none());
    }
    use serde_json::json;

    #[test]
    fn quota_summary_selects_gemini_periods_not_third_party_pools() {
        let raw = serde_json::to_vec(&json!({"response":{"groups":[{"buckets":[
            {"bucketId":"gemini-weekly","remainingFraction":0.90,"resetTime":"2026-10-04T05:13:15Z"},
            {"bucketId":"3p-5h","remainingFraction":1.0},
            {"bucketId":"gemini-5h","remainingFraction":0.42,"resetTime":"2026-09-28T06:12:05Z"}
        ]}]}})).unwrap();
        let status = parse_status(&raw, "PC", "2026-09-28T03:00:00Z").unwrap();
        let primary = status.primary.unwrap();
        let secondary = status.secondary.unwrap();
        assert_eq!(primary.label, "5h");
        assert_eq!(primary.used_percent, Some(58.0));
        assert_eq!(primary.resets_at.as_deref(), Some("2026-09-28T06:12:05Z"));
        assert_eq!(secondary.label, "week");
        assert_eq!(secondary.used_percent, Some(10.0));
        assert_eq!(secondary.resets_at.as_deref(), Some("2026-10-04T05:13:15Z"));
    }

    fn fixture(buckets: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"response":{"groups":[{"buckets":buckets}]}})).unwrap()
    }

    #[test]
    fn periods_use_ids_not_display_names_or_response_order() {
        let raw = fixture(json!([
            {"bucketId":"3p-weekly","displayName":"Gemini","remainingFraction":0.0},
            {"bucketId":"gemini-weekly","displayName":"Localized weekly","remainingFraction":0.90},
            {"bucketId":"gemini-5h","displayName":"Localized five hour","remainingFraction":0.75}
        ]));
        let status = parse_status(&raw, "PC", "2026-09-27T09:00:00Z").unwrap();
        assert_eq!(status.tool, Tool::Antigravity);
        assert_eq!(status.primary.as_ref().unwrap().used_percent, Some(25.0));
        assert_eq!(status.secondary.as_ref().unwrap().used_percent, Some(10.0));
        assert_eq!(status.primary.unwrap().label, "5h");
        assert!(status.session.context_used_percent.is_none());
    }

    #[test]
    fn absent_period_stays_absent_and_zero_is_valid() {
        let raw = fixture(json!([{"bucketId":"gemini-5h","remainingFraction":0.0}]));
        let status = parse_status(&raw, "PC", "now").unwrap();
        assert_eq!(status.primary.unwrap().used_percent, Some(100.0));
        assert!(status.secondary.is_none());
        let omitted_zero =
            fixture(json!([{"bucketId":"gemini-5h","resetTime":"2026-09-27T10:00:00Z"}]));
        let weekly = parse_status(
            &fixture(json!([
                {"bucketId":"gemini-weekly","remainingFraction":1.0}
            ])),
            "PC",
            "now",
        )
        .unwrap();
        assert!(weekly.primary.is_none());
        assert_eq!(weekly.secondary.unwrap().used_percent, Some(0.0));
        assert_eq!(
            parse_status(&omitted_zero, "PC", "now")
                .unwrap()
                .primary
                .unwrap()
                .used_percent,
            Some(100.0)
        );
    }

    #[test]
    fn missing_invalid_unknown_and_conflicting_values_fail_closed() {
        for buckets in [
            json!([]),
            json!([{"bucketId":"gemini-unknown","remainingFraction":0.5}]),
            json!([{"bucketId":"3p-5h","remainingFraction":0.5}]),
            json!([{"bucketId":"gemini-5h"}]),
            json!([{"bucketId":"gemini-5h","remainingFraction":1.5}]),
            json!([{"bucketId":"gemini-5h","remainingFraction":-0.1}]),
            json!([{"bucketId":"gemini-5h","remainingFraction":"0.5"}]),
            json!([{"bucketId":"gemini-5h","remainingFraction":null,"resetTime":"2026-09-27T10:00:00Z"}]),
            json!([{"bucketId":"gemini-5h","remainingFraction":0.5,"resetTime":"bad"}]),
            json!([{"bucketId":"gemini-5h","remainingFraction":0.5}, {"bucketId":"gemini-5h","remainingFraction":0.6}]),
            json!([{"bucketId":"gemini-weekly","remainingFraction":0.5}, {"bucketId":"gemini-weekly","remainingFraction":0.5}]),
        ] {
            assert!(parse_status(&fixture(buckets), "PC", "now").is_err());
        }
        for body in [br#"{"response":null}"#.as_slice(), br#"{}"#,
            br#"{"userStatus":{"cascadeModelConfigData":{"clientModelConfigs":[{"label":"Gemini Pro","quotaInfo":{"remainingFraction":0.9}}]}}}"#] {
            assert!(parse_status(body, "PC", "now").is_err());
        }
    }

    #[test]
    fn auth_absence_is_not_confused_with_a_broken_response() {
        assert_eq!(
            authenticated(br#"{"authResult":{"hasValidAuth":true}}"#),
            Ok(true)
        );
        assert_eq!(authenticated(br#"{"authResult":{}}"#), Ok(false));
        assert_eq!(
            authenticated(br#"{"authResult":{"hasValidAuth":false}}"#),
            Ok(false)
        );
        assert_eq!(authenticated(br#"{}"#), Err(Error::Unavailable));
        assert_eq!(
            authenticated(br#"{"authResult":{"hasValidAuth":"false"}}"#),
            Err(Error::Unavailable)
        );
    }

    #[test]
    fn summary_supports_flat_buckets_but_rejects_cross_group_duplicates_and_excess() {
        let bucket = json!({"bucketId":"gemini-5h","remainingFraction":0.8});
        let body = serde_json::to_vec(&json!({"response":{"buckets":[bucket.clone()]}})).unwrap();
        let status = parse_status(&body, "PC", "now").unwrap();
        assert!(status.primary.is_some());
        assert!(status.secondary.is_none());
        let duplicate = serde_json::to_vec(&json!({"response":{
            "buckets":[bucket.clone()],"groups":[{"buckets":[bucket.clone()]}]
        }}))
        .unwrap();
        assert!(parse_status(&duplicate, "PC", "now").is_err());
        let duplicate_groups = serde_json::to_vec(&json!({"response":{"groups":[
            {"buckets":[bucket.clone()]},{"buckets":[bucket.clone()]}
        ]}}))
        .unwrap();
        assert!(parse_status(&duplicate_groups, "PC", "now").is_err());
        assert!(parse_status(&fixture(json!(vec![bucket; 257])), "PC", "now").is_err());
        let groups =
            serde_json::to_vec(&json!({"response":{"groups":vec![json!({});65]}})).unwrap();
        assert!(parse_status(&groups, "PC", "now").is_err());
        assert!(parse_status(&vec![b' '; 1024 * 1024 + 1], "PC", "now").is_err());
    }

    #[test]
    fn cache_bounds_polling_and_clears_data_after_failed_force_refresh() {
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        let status = |_: &dyn Fn() -> bool| {
            parse_status(
                &fixture(json!([{"bucketId":"gemini-5h","remainingFraction":0.5}])),
                "PC",
                "now",
            )
        };
        assert!(refresh_cache(&state, false, status).is_some());
        assert!(refresh_cache(&state, false, |_| panic!(
            "cache must avoid a second request"
        ))
        .is_some());
        assert!(refresh_cache(&state, true, |_| Err(Error::AppRequired)).is_none());
        assert!(state.lock().unwrap().result.as_ref().unwrap().is_err());
        assert!(refresh_cache(&state, false, |_| panic!("missing app cooldown")).is_none());
    }

    #[test]
    fn closed_cli_source_is_not_returned_during_busy_backoff_or_publication() {
        let mut closed = parse_status(
            &fixture(json!([{"bucketId":"gemini-5h","remainingFraction":0.5}])),
            "PC",
            "now",
        )
        .unwrap();
        closed.session_id = "antigravity-cli:4294967295:0".into();
        for busy in [true, false] {
            let state = Mutex::new(Cache {
                enabled: true,
                busy,
                retry_at: Some(Instant::now() + std::time::Duration::from_secs(60)),
                result: Some(Ok(closed.clone())),
                ..Default::default()
            });
            assert!(refresh_cache(&state, false, |_| panic!("cached path only")).is_none());
        }
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        assert!(refresh_cache(&state, true, |_| Ok(closed)).is_none());
    }

    #[test]
    fn headless_account_reads_use_five_minute_cache_and_throttle_repeat_force() {
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        let result = |_: &dyn Fn() -> bool| {
            let mut status = parse_status(
                &fixture(json!([{"bucketId":"gemini-5h","remainingFraction":0.25}])),
                "PC",
                "fixed-capture",
            )?;
            status.session_id = crate::antigravity_cli::headless::SESSION_ID.into();
            Ok(status)
        };
        assert!(refresh_cache(&state, false, result).is_some());
        {
            let cache = state.lock().unwrap();
            assert!(
                cache
                    .retry_at
                    .unwrap()
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    >= 299
            );
        }
        let cached = refresh_cache(&state, true, |_| {
            panic!("rapid headless force must be throttled")
        })
        .unwrap();
        assert_eq!(cached.captured_at, "fixed-capture");
        {
            let mut cache = state.lock().unwrap();
            cache.last_started_at = Some(Instant::now() - std::time::Duration::from_secs(31));
            cache.force_retry_at = Some(Instant::now() - std::time::Duration::from_secs(1));
        }
        assert!(refresh_cache(&state, true, |_| Err(Error::CliLoginRequired)).is_none());
        assert!(state.lock().unwrap().result.as_ref().unwrap().is_err());
        assert!(refresh_cache(&state, true, |_| panic!(
            "failed headless force is also throttled"
        ))
        .is_none());
    }

    #[test]
    fn failed_summary_never_restores_old_quota_on_transport_parse_or_auth_errors() {
        for error in [Error::Unavailable, Error::LoginRequired, Error::AppRequired] {
            let state = Mutex::new(Cache {
                enabled: true,
                ..Default::default()
            });
            let result = |_: &dyn Fn() -> bool| {
                parse_status(
                    &fixture(json!([
                        {"bucketId":"gemini-5h","remainingFraction":0.5}
                    ])),
                    "PC",
                    "now",
                )
            };
            assert!(refresh_cache(&state, true, result).is_some());
            assert!(refresh_cache(&state, true, |_| Err(error)).is_none());
            assert!(refresh_cache(&state, false, |_| panic!("backoff")).is_none());
            assert!(refresh_cache(&state, true, result).is_some());
        }
    }

    fn headless_fixture() -> AgentStatus {
        let mut status = parse_status(
            &fixture(json!([{"bucketId":"gemini-5h","remainingFraction":0.5}])),
            "PC",
            "fixed-capture",
        )
        .unwrap();
        status.session_id = crate::antigravity_cli::headless::SESSION_ID.into();
        status
    }

    fn snapshot_fixture(used_percent: f64, captured_at: &str) -> AgentStatus {
        parse_status(
            &fixture(json!([{
                "bucketId": "gemini-5h",
                "remainingFraction": 1.0 - used_percent / 100.0
            }])),
            "PC",
            captured_at,
        )
        .unwrap()
    }

    fn snapshot_after_cursor_wait(
        update: impl FnOnce(&Mutex<Cache>),
    ) -> (Vec<AgentStatus>, Option<Error>) {
        let old = snapshot_fixture(10.0, "old-A");
        let state = Mutex::new(Cache {
            enabled: true,
            result: Some(Ok(old.clone())),
            ..Default::default()
        });
        let cursor = Mutex::new(());
        let cursor_guard = cursor.lock().unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let pending = scope.spawn(|| {
                let mut snapshot = vec![old];
                waiting_tx.send(()).unwrap();
                let _cursor_guard = cursor.lock().unwrap();
                with_current_cache_snapshot(
                    &state,
                    |_| true,
                    |current, error| {
                        snapshot.retain(|status| status.tool != Tool::Antigravity);
                        snapshot.extend(current);
                        (snapshot, error)
                    },
                )
            });
            waiting_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            update(&state);
            drop(cursor_guard);
            pending.join().unwrap()
        })
    }

    #[test]
    fn current_snapshot_replaces_old_clone_after_new_success_during_cursor_wait() {
        let new = snapshot_fixture(90.0, "new-B");
        let (published, error) = snapshot_after_cursor_wait(|state| {
            assert_eq!(
                refresh_cache(state, true, |_| Ok(new.clone())),
                Some(new.clone())
            );
        });
        assert_eq!(published, vec![new]);
        assert_eq!(error, None);
    }

    #[test]
    fn current_snapshot_discards_old_clone_after_new_error_during_cursor_wait() {
        for expected in [
            Error::AppRequired,
            Error::LoginRequired,
            Error::Unavailable,
            Error::CliLoginRequired,
            Error::CliUnavailable,
        ] {
            let (published, error) = snapshot_after_cursor_wait(|state| {
                assert!(refresh_cache(state, true, |_| Err(expected)).is_none());
            });
            assert!(published.is_empty());
            assert_eq!(error, Some(expected));
        }
    }

    #[test]
    fn current_snapshot_discards_old_clone_after_policy_change_during_cursor_wait() {
        for suspended in [false, true] {
            for recovered in [false, true] {
                let (published, error) = snapshot_after_cursor_wait(|state| {
                    let mut cache = state.lock().unwrap();
                    assert!(if suspended {
                        cache.suspend_for_settings_failure()
                    } else {
                        cache.set_enabled(false)
                    });
                    if recovered {
                        assert!(cache.set_enabled(true));
                    }
                });
                assert!(published.is_empty());
                assert_eq!(error, None);
            }
        }
    }

    #[test]
    fn current_snapshot_samples_source_once_for_both_quota_and_error() {
        let source_alive = std::cell::Cell::new(true);
        let calls = std::cell::Cell::new(0);
        let status = snapshot_fixture(10.0, "source-A");
        let state = Mutex::new(Cache {
            enabled: true,
            result: Some(Ok(status.clone())),
            ..Default::default()
        });
        let observed = with_current_cache_snapshot(
            &state,
            |current| {
                assert!(
                    state.try_lock().is_err(),
                    "sample must hold completion lock"
                );
                assert_eq!(current, &status);
                calls.set(calls.get() + 1);
                source_alive.get()
            },
            |quota, error| {
                source_alive.set(false);
                (quota, error)
            },
        );
        assert_eq!(observed, (Some(status.clone()), None));
        assert_eq!(calls.get(), 1);
        let stale = with_current_cache_snapshot(
            &state,
            |_| {
                calls.set(calls.get() + 1);
                source_alive.get()
            },
            |quota, error| (quota, error),
        );
        assert_eq!(stale, (None, Some(Error::AppRequired)));
        assert_eq!(calls.get(), 2);
        assert_eq!(state.lock().unwrap().result, Some(Ok(status)));
    }

    #[test]
    fn current_snapshot_keeps_empty_disabled_suspended_and_error_contracts() {
        for result in [
            None,
            Some(Ok(headless_fixture())),
            Some(Err(Error::AppRequired)),
            Some(Err(Error::LoginRequired)),
            Some(Err(Error::Unavailable)),
            Some(Err(Error::CliLoginRequired)),
            Some(Err(Error::CliUnavailable)),
        ] {
            for (enabled, suspended) in [(false, false), (false, true), (true, true)] {
                let state = Mutex::new(Cache {
                    enabled,
                    suspended,
                    result: result.clone(),
                    ..Default::default()
                });
                assert_eq!(
                    with_current_cache_snapshot(
                        &state,
                        |_| panic!("disabled/suspended must not query source"),
                        |quota, error| (quota, error)
                    ),
                    (None, None)
                );
            }
            if result.as_ref().is_some_and(Result::is_ok) {
                continue;
            }
            let expected = result
                .as_ref()
                .and_then(|value| value.as_ref().err())
                .copied();
            let state = Mutex::new(Cache {
                enabled: true,
                result,
                ..Default::default()
            });
            assert_eq!(
                with_current_cache_snapshot(
                    &state,
                    |_| panic!("empty/error has no source to query"),
                    |quota, error| (quota, error)
                ),
                (None, expected)
            );
        }
    }

    #[test]
    fn current_snapshot_preserves_headless_desktop_and_closed_cli_source_rules() {
        let desktop = snapshot_fixture(10.0, "desktop");
        let mut closed = desktop.clone();
        closed.session_id = "antigravity-cli:4294967295:0".into();
        for status in [desktop, headless_fixture(), closed] {
            let is_closed = status.session_id.starts_with("antigravity-cli:");
            let state = Mutex::new(Cache {
                enabled: true,
                busy: true,
                retry_at: Some(Instant::now() + std::time::Duration::from_secs(300)),
                result: Some(Ok(status.clone())),
                ..Default::default()
            });
            assert_eq!(
                with_current_cache_snapshot(
                    &state,
                    crate::antigravity_cli::cached_source_alive,
                    |quota, error| (quota, error)
                ),
                if is_closed {
                    (None, Some(Error::AppRequired))
                } else {
                    (Some(status), None)
                }
            );
        }
    }

    #[test]
    fn current_snapshot_holds_completion_lock_through_final_consumption() {
        for change in 0..4 {
            let old = snapshot_fixture(10.0, "old-A");
            let new = snapshot_fixture(90.0, "new-B");
            let state = Mutex::new(Cache {
                enabled: true,
                result: Some(Ok(old.clone())),
                ..Default::default()
            });
            let (start_tx, start_rx) = std::sync::mpsc::sync_channel(0);
            let (blocked_tx, blocked_rx) = std::sync::mpsc::sync_channel(0);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                let state_ref = &state;
                let new_ref = &new;
                let updater = scope.spawn(move || {
                    start_rx
                        .recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                    assert!(matches!(
                        state_ref.try_lock(),
                        Err(std::sync::TryLockError::WouldBlock)
                    ));
                    blocked_tx.send(()).unwrap();
                    match change {
                        0 => {
                            refresh_cache(state_ref, true, |_| Ok(new_ref.clone()));
                        }
                        1 => {
                            refresh_cache(state_ref, true, |_| Err(Error::CliUnavailable));
                        }
                        2 => {
                            state_ref.lock().unwrap().set_enabled(false);
                        }
                        _ => {
                            state_ref.lock().unwrap().suspend_for_settings_failure();
                        }
                    }
                    done_tx.send(()).unwrap();
                });
                let consumed = with_current_cache_snapshot(
                    &state,
                    |_| true,
                    |quota, error| {
                        start_tx.send(()).unwrap();
                        blocked_rx
                            .recv_timeout(std::time::Duration::from_secs(3))
                            .unwrap();
                        let payload = serde_json::to_vec(&quota).unwrap();
                        let health = error;
                        assert!(matches!(
                            state.try_lock(),
                            Err(std::sync::TryLockError::WouldBlock)
                        ));
                        assert!(matches!(
                            done_rx.try_recv(),
                            Err(std::sync::mpsc::TryRecvError::Empty)
                        ));
                        (payload, health)
                    },
                );
                updater.join().unwrap();
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
                assert_eq!(consumed, (serde_json::to_vec(&Some(old)).unwrap(), None));
            });
            assert_eq!(
                with_current_cache_snapshot(&state, |_| true, |quota, error| (quota, error)),
                match change {
                    0 => (Some(new), None),
                    1 => (None, Some(Error::CliUnavailable)),
                    _ => (None, None),
                }
            );
        }
    }

    #[test]
    fn settings_suspension_preserves_throttles_but_never_restores_account_data() {
        for result in [
            Ok(headless_fixture()),
            Err(Error::CliUnavailable),
            Err(Error::CliLoginRequired),
        ] {
            let state = Mutex::new(Cache {
                enabled: true,
                ..Default::default()
            });
            refresh_cache(&state, false, |_| result);
            let throttle = |cache: &Cache| {
                (
                    cache.retry_at,
                    cache.last_started_at,
                    cache.force_retry_at,
                    cache.failures,
                )
            };
            let before = {
                let mut cache = state.lock().unwrap();
                let before = throttle(&cache);
                assert!(cache.suspend_for_settings_failure());
                let generation = cache.generation;
                assert!(!cache.suspend_for_settings_failure());
                assert_eq!(cache.generation, generation);
                assert!(cache.enabled && cache.suspended);
                assert!(cache.result.is_none());
                assert_eq!(throttle(&cache), before);
                before
            };
            assert!(refresh_cache(&state, true, |_| panic!("suspended")).is_none());
            {
                let mut cache = state.lock().unwrap();
                assert!(cache.set_enabled(true));
                assert!(!cache.suspended);
                assert!(cache.result.is_none());
                assert_eq!(throttle(&cache), before);
            }
            for force in [false, true] {
                assert!(refresh_cache(&state, force, |_| panic!("preserved throttle")).is_none());
            }
            {
                let mut cache = state.lock().unwrap();
                cache.force_retry_at = Some(Instant::now() - std::time::Duration::from_secs(1));
            }
            assert!(
                refresh_cache(&state, false, |_| panic!("automatic retry still pending")).is_none()
            );
            assert!(refresh_cache(&state, true, |_| Ok(headless_fixture())).is_some());
        }
    }

    #[test]
    fn explicit_off_on_still_resets_completed_attempt_throttles() {
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        assert!(refresh_cache(&state, false, |_| Err(Error::CliUnavailable)).is_none());
        {
            let mut cache = state.lock().unwrap();
            assert!(cache.suspend_for_settings_failure());
            assert!(cache.set_enabled(false));
            assert!(cache.set_enabled(true));
            assert!(!cache.suspended);
            assert!(cache.retry_at.is_none());
            assert!(cache.last_started_at.is_none());
            assert!(cache.force_retry_at.is_none());
            assert_eq!(cache.failures, 0);
        }
        assert!(refresh_cache(&state, false, |_| Ok(headless_fixture())).is_some());
    }

    #[test]
    fn retired_admission_and_completion_cannot_revive_or_bypass_throttles() {
        for temporary in [false, true] {
            for result in [Ok(headless_fixture()), Err(Error::CliUnavailable)] {
                let state = Mutex::new(Cache {
                    enabled: true,
                    ..Default::default()
                });
                assert!(refresh_cache(&state, false, |admitted| {
                    assert!(admitted());
                    {
                        let mut cache = state.lock().unwrap();
                        if temporary {
                            assert!(cache.suspend_for_settings_failure());
                        } else {
                            assert!(cache.set_enabled(false));
                        }
                        assert!(cache.set_enabled(true));
                    }
                    assert!(
                        !admitted(),
                        "old admission must stay retired after recovery"
                    );
                    assert!(
                        refresh_cache(&state, true, |_| panic!("in-flight duplicate")).is_none()
                    );
                    result
                })
                .is_none());
                {
                    let cache = state.lock().unwrap();
                    assert!(!cache.busy);
                    assert!(cache.result.is_none());
                    assert!(cache.last_started_at.is_some());
                    assert!(
                        cache
                            .retry_at
                            .unwrap()
                            .saturating_duration_since(Instant::now())
                            .as_secs()
                            >= 299
                    );
                    assert!(
                        cache
                            .force_retry_at
                            .unwrap()
                            .saturating_duration_since(Instant::now())
                            .as_secs()
                            >= 29
                    );
                }
                for force in [false, true] {
                    assert!(
                        refresh_cache(&state, force, |_| panic!("retired attempt throttle"))
                            .is_none()
                    );
                }
            }
        }
    }

    #[test]
    fn retired_failure_keeps_escalation_and_does_not_shorten_existing_backoff() {
        let retry_at = Instant::now() + std::time::Duration::from_secs(6000);
        let state = Mutex::new(Cache {
            enabled: true,
            failures: 3,
            retry_at: Some(retry_at),
            ..Default::default()
        });
        assert!(refresh_cache(&state, true, |admitted| {
            assert!(admitted());
            let mut cache = state.lock().unwrap();
            cache.suspend_for_settings_failure();
            cache.set_enabled(true);
            Err(Error::CliUnavailable)
        })
        .is_none());
        let cache = state.lock().unwrap();
        assert_eq!(cache.failures, 4);
        assert_eq!(cache.retry_at, Some(retry_at));
        assert!(cache.result.is_none());
    }

    #[test]
    fn disabled_and_concurrent_collection_never_start_a_request() {
        let state = Mutex::new(Cache::default());
        assert!(refresh_cache(&state, true, |_| panic!("disabled")).is_none());
        state.lock().unwrap().enabled = true;
        refresh_cache(&state, true, |_| {
            assert!(refresh_cache(&state, true, |_| panic!("duplicate request")).is_none());
            let mut cache = state.lock().unwrap();
            cache.enabled = false;
            cache.generation += 1;
            Err(Error::LoginRequired)
        });
        assert!(
            state.lock().unwrap().result.is_none(),
            "disabled generation must not commit"
        );
        assert!(!state.lock().unwrap().busy);
    }
}
