//! Read-only Desktop quotas and the CLI's built-in zero-token quota command.
use crate::model::{AccountLimit, AgentStatus, SessionInfo, Tool};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Instant;

pub const COLLECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

#[derive(Default)]
struct Cache {
    enabled: bool,
    generation: u64,
    busy: bool,
    retry_at: Option<Instant>,
    last_started_at: Option<Instant>,
    failures: u32,
    result: Option<Result<AgentStatus, Error>>,
}
static CACHE: Mutex<Cache> = Mutex::new(Cache {
    enabled: false,
    generation: 0,
    busy: false,
    retry_at: None,
    last_started_at: None,
    failures: 0,
    result: None,
});

pub fn set_enabled(enabled: bool) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.enabled == enabled {
        return;
    }
    cache.enabled = enabled;
    cache.generation = cache.generation.wrapping_add(1);
    cache.result = None;
    cache.retry_at = None;
    cache.last_started_at = None;
    cache.failures = 0;
    crate::antigravity_cli::clear_cached_source();
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

pub fn refresh(force: bool, deadline: Instant) -> Option<AgentStatus> {
    refresh_cache(&CACHE, force, || {
        collect(
            &gethostname::gethostname().to_string_lossy(),
            &chrono::Utc::now().to_rfc3339(),
            deadline,
        )
    })
}

fn refresh_cache(
    state: &Mutex<Cache>,
    force: bool,
    fetch: impl FnOnce() -> Result<AgentStatus, Error>,
) -> Option<AgentStatus> {
    let generation = {
        let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.enabled {
            return None;
        }
        let now = Instant::now();
        let force_throttled = force
            && cache.result.as_ref().is_some_and(|result| {
                matches!(result, Err(Error::CliLoginRequired | Error::CliUnavailable))
                    || result.as_ref().is_ok_and(|status| {
                        status.session_id == crate::antigravity_cli::headless::SESSION_ID
                    })
            })
            && cache.last_started_at.is_some_and(|at| {
                now.saturating_duration_since(at) < std::time::Duration::from_secs(30)
            });
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
        cache.generation
    };
    let result = fetch();
    let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
    cache.busy = false;
    if cache.generation != generation {
        return None;
    }
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
    cache.retry_at = Some(Instant::now() + std::time::Duration::from_secs(delay));
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

pub fn collect(pc_id: &str, captured_at: &str, deadline: Instant) -> Result<AgentStatus, Error> {
    #[cfg(windows)]
    {
        resolve_collection_source(
            crate::antigravity_cli::headless::collect(pc_id, captured_at, deadline).map_err(
                |error| match error {
                    crate::antigravity_cli::CaptureError::AppRequired => Error::AppRequired,
                    crate::antigravity_cli::CaptureError::LoginRequired => Error::CliLoginRequired,
                    crate::antigravity_cli::CaptureError::Unavailable => Error::CliUnavailable,
                },
            ),
            || native::collect(pc_id, captured_at, deadline),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (pc_id, captured_at, deadline);
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
        let status = || {
            parse_status(
                &fixture(json!([{"bucketId":"gemini-5h","remainingFraction":0.5}])),
                "PC",
                "now",
            )
        };
        assert!(refresh_cache(&state, false, status).is_some());
        assert!(refresh_cache(&state, false, || panic!(
            "cache must avoid a second request"
        ))
        .is_some());
        assert!(refresh_cache(&state, true, || Err(Error::AppRequired)).is_none());
        assert!(state.lock().unwrap().result.as_ref().unwrap().is_err());
        assert!(refresh_cache(&state, false, || panic!("missing app cooldown")).is_none());
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
            assert!(refresh_cache(&state, false, || panic!("cached path only")).is_none());
        }
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        assert!(refresh_cache(&state, true, || Ok(closed)).is_none());
    }

    #[test]
    fn headless_account_reads_use_five_minute_cache_and_throttle_repeat_force() {
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        let result = || {
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
        let cached = refresh_cache(&state, true, || {
            panic!("rapid headless force must be throttled")
        })
        .unwrap();
        assert_eq!(cached.captured_at, "fixed-capture");
        {
            let mut cache = state.lock().unwrap();
            cache.last_started_at = Some(Instant::now() - std::time::Duration::from_secs(31));
        }
        assert!(refresh_cache(&state, true, || Err(Error::CliLoginRequired)).is_none());
        assert!(state.lock().unwrap().result.as_ref().unwrap().is_err());
        assert!(refresh_cache(&state, true, || panic!(
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
            let result = || {
                parse_status(
                    &fixture(json!([
                        {"bucketId":"gemini-5h","remainingFraction":0.5}
                    ])),
                    "PC",
                    "now",
                )
            };
            assert!(refresh_cache(&state, true, result).is_some());
            assert!(refresh_cache(&state, true, || Err(error)).is_none());
            assert!(refresh_cache(&state, false, || panic!("backoff")).is_none());
            assert!(refresh_cache(&state, true, result).is_some());
        }
    }

    #[test]
    fn disabled_and_concurrent_collection_never_start_a_request() {
        let state = Mutex::new(Cache::default());
        assert!(refresh_cache(&state, true, || panic!("disabled")).is_none());
        state.lock().unwrap().enabled = true;
        refresh_cache(&state, true, || {
            assert!(refresh_cache(&state, true, || panic!("duplicate request")).is_none());
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
