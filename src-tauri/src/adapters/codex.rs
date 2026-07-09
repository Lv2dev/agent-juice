use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::model::*;

fn iso(ts: Option<i64>) -> Option<String> {
    ts.and_then(|t| Utc.timestamp_opt(t, 0).single())
        .map(|d| d.to_rfc3339())
}

fn limit(rate_limits: &Value, key: &str, fallback: &str) -> Option<AccountLimit> {
    let window = rate_limits.get(key)?;
    let window_minutes = window
        .get("window_minutes")
        .and_then(|value| value.as_i64());
    let label = match window_minutes {
        Some(minutes) if minutes <= 600 => "5h".to_string(),
        Some(_) => "week".to_string(),
        None => fallback.to_string(),
    };

    Some(AccountLimit {
        label,
        used_percent: window
            .get("used_percent")
            .and_then(|value| value.as_f64())
            .map(|value| value as f32),
        resets_at: iso(window.get("resets_at").and_then(|value| value.as_i64())),
    })
}

pub fn parse_token_count(
    json: &str,
    pc_id: &str,
    session_id: &str,
    captured_at: &str,
) -> anyhow::Result<AgentStatus> {
    let root: Value = serde_json::from_str(json)?;
    if root.get("type").and_then(Value::as_str) != Some("event_msg") {
        anyhow::bail!("not an event_msg envelope");
    }
    let payload = root
        .get("payload")
        .ok_or_else(|| anyhow::anyhow!("missing payload"))?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        anyhow::bail!("not a token_count payload");
    }
    let captured = root
        .get("timestamp")
        .and_then(|value| value.as_str())
        .and_then(valid_rfc3339)
        .unwrap_or_else(|| captured_at.to_string());

    let context_used_percent = {
        let input_tokens = payload
            .pointer("/info/last_token_usage/input_tokens")
            .and_then(|value| value.as_f64());
        let context_window = payload
            .pointer("/info/model_context_window")
            .and_then(|value| value.as_f64());

        match (input_tokens, context_window) {
            (Some(input_tokens), Some(context_window)) if context_window > 0.0 => {
                Some((input_tokens / context_window * 100.0) as f32)
            }
            _ => None,
        }
    };

    let (primary, secondary) = match payload.get("rate_limits") {
        Some(rate_limits) => (
            limit(rate_limits, "primary", "5h"),
            limit(rate_limits, "secondary", "week"),
        ),
        None => (None, None),
    };

    Ok(AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Codex,
        session_id: session_id.into(),
        captured_at: captured,
        primary,
        secondary,
        session: SessionInfo {
            active: true,
            context_used_percent,
        },
        cost_estimate_usd: None,
        approx: true,
    })
}

fn valid_rfc3339(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc).to_rfc3339())
}
