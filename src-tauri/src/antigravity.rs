//! Read-only access to a running Antigravity desktop. Never starts the provider.
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
    failures: u32,
    result: Option<Result<AgentStatus, Error>>,
}
static CACHE: Mutex<Cache> = Mutex::new(Cache {
    enabled: false,
    generation: 0,
    busy: false,
    retry_at: None,
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
    cache.failures = 0;
}

pub fn cached() -> Option<AgentStatus> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .cloned()
}

pub fn last_error() -> Option<Error> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .result
        .as_ref()
        .and_then(|r| r.as_ref().err())
        .copied()
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
        if cache.busy || (!force && cache.retry_at.is_some_and(|at| Instant::now() < at)) {
            return cache.result.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        }
        cache.busy = true;
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
    let delay = if matches!(result, Err(Error::Unavailable)) {
        60u64.saturating_mul(1 << cache.failures.saturating_sub(1).min(4))
    } else {
        60
    };
    cache.retry_at = Some(Instant::now() + std::time::Duration::from_secs(delay));
    // No persisted or last-good account data across errors, closure or a toggle.
    cache.result = Some(result);
    cache.result.as_ref().and_then(|r| r.as_ref().ok()).cloned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AppRequired,
    LoginRequired,
    Unavailable,
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
#[serde(rename_all = "camelCase")]
struct Response {
    user_status: Option<UserStatus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStatus {
    cascade_model_config_data: Option<ModelConfig>,
    plan_status: Option<PlanStatus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanStatus {
    plan_info: Option<PlanInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanInfo {
    teams_tier: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelConfig {
    #[serde(default)]
    client_model_configs: Vec<ModelQuota>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelQuota {
    label: String,
    quota_info: Option<Quota>,
    #[serde(default)]
    allowed_tiers: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quota {
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

pub fn parse_status(body: &[u8], pc_id: &str, captured_at: &str) -> Result<AgentStatus, Error> {
    if body.len() > 1024 * 1024 {
        return Err(Error::Unavailable);
    }
    let response: Response = serde_json::from_slice(body).map_err(|_| Error::Unavailable)?;
    let status = response.user_status.ok_or(Error::Unavailable)?;
    let tier = status
        .plan_status
        .and_then(|plan| plan.plan_info)
        .and_then(|plan| plan.teams_tier);
    let models = status
        .cascade_model_config_data
        .ok_or(Error::Unavailable)?
        .client_model_configs;
    if models.len() > 256 {
        return Err(Error::Unavailable);
    }
    let mut pools: [Option<Quota>; 2] = [None, None];
    for model in models {
        if !model.allowed_tiers.is_empty()
            && !tier
                .as_ref()
                .is_some_and(|tier| model.allowed_tiers.contains(tier))
        {
            continue;
        }
        let index = if model.label.starts_with("Gemini ") {
            0
        } else if model.label.starts_with("Claude ") || model.label.starts_with("GPT-OSS ") {
            1
        } else {
            continue;
        };
        let Some(mut quota) = model.quota_info else {
            continue;
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
        if let Some(previous) = &pools[index] {
            if previous.remaining_fraction != quota.remaining_fraction
                || previous.reset_time != quota.reset_time
            {
                return Err(Error::Unavailable);
            }
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
        primary: limit(primary, "gemini_models"),
        secondary: limit(secondary, "claude_gpt_models"),
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
        native::collect(pc_id, captured_at, deadline)
    }
    #[cfg(not(windows))]
    {
        let _ = (pc_id, captured_at, deadline);
        Err(Error::AppRequired)
    }
}

#[cfg(windows)]
mod native;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(models: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(
            &json!({"userStatus":{"cascadeModelConfigData":{"clientModelConfigs":models}}}),
        )
        .unwrap()
    }

    #[test]
    fn two_pools_are_remaining_fractions_not_five_hour_week_slots() {
        let raw = fixture(json!([
            {"label":"Gemini Pro","quotaInfo":{"remainingFraction":0.75,"resetTime":"2026-09-27T10:00:00Z"}},
            {"label":"Gemini Flash","quotaInfo":{"remainingFraction":0.75,"resetTime":"2026-09-27T10:00:00Z"}},
            {"label":"Claude Opus","quotaInfo":{"remainingFraction":1.0}}
        ]));
        let status = parse_status(&raw, "PC", "2026-09-27T09:00:00Z").unwrap();
        assert_eq!(status.tool, Tool::Antigravity);
        assert_eq!(status.primary.as_ref().unwrap().used_percent, Some(25.0));
        assert_eq!(status.secondary.as_ref().unwrap().used_percent, Some(0.0));
        assert_eq!(status.primary.unwrap().label, "gemini_models");
        assert!(status.session.context_used_percent.is_none());
    }

    #[test]
    fn absent_pool_stays_absent_and_zero_is_valid() {
        let raw = fixture(json!([{"label":"Gemini Pro","quotaInfo":{"remainingFraction":0.0}}]));
        let status = parse_status(&raw, "PC", "now").unwrap();
        assert_eq!(status.primary.unwrap().used_percent, Some(100.0));
        assert!(status.secondary.is_none());
        let omitted_zero = fixture(
            json!([{"label":"Gemini Pro","quotaInfo":{"resetTime":"2026-09-27T10:00:00Z"}}]),
        );
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
        for models in [
            json!([]),
            json!([{"label":"New Model","quotaInfo":{"remainingFraction":0.5}}]),
            json!([{"label":"Gemini Pro","quotaInfo":{}}]),
            json!([{"label":"Gemini Pro","quotaInfo":{"remainingFraction":1.5}}]),
            json!([{"label":"Gemini Pro","quotaInfo":{"remainingFraction":0.5,"resetTime":"bad"}}]),
            json!([{"label":"Gemini Pro","quotaInfo":{"remainingFraction":0.5}}, {"label":"Gemini Flash","quotaInfo":{"remainingFraction":0.6}}]),
        ] {
            assert!(parse_status(&fixture(models), "PC", "now").is_err());
        }
        assert!(parse_status(br#"{"userStatus":null}"#, "PC", "now").is_err());
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
    fn explicitly_restricted_models_are_not_presented_as_available_quota() {
        let body = serde_json::to_vec(&json!({"userStatus": {
            "planStatus":{"planInfo":{"teamsTier":"TEAMS_TIER_PRO"}},
            "cascadeModelConfigData":{"clientModelConfigs":[
                {"label":"Gemini Pro","allowedTiers":["TEAMS_TIER_PRO"],"quotaInfo":{"remainingFraction":0.8}},
                {"label":"Claude Opus","allowedTiers":["TEAMS_TIER_PRO_ULTIMATE"],"quotaInfo":{"remainingFraction":1.0}}
            ]}
        }})).unwrap();
        let status = parse_status(&body, "PC", "now").unwrap();
        assert!(status.primary.is_some());
        assert!(status.secondary.is_none());
    }

    #[test]
    fn cache_bounds_polling_and_clears_data_after_failed_force_refresh() {
        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        let status = || {
            parse_status(
                &fixture(json!([{"label":"Gemini Pro","quotaInfo":{"remainingFraction":0.5}}])),
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
