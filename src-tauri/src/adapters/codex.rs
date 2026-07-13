use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::model::*;

fn iso(ts: Option<i64>) -> Option<String> {
    ts.and_then(|t| Utc.timestamp_opt(t, 0).single())
        .map(|d| d.to_rfc3339())
}

fn iso_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Number(number) => iso(number
            .as_i64()
            .or_else(|| number.as_f64().map(|v| v as i64))),
        Value::String(raw) => canonical_rfc3339(raw),
        _ => None,
    }
}

fn limit(rate_limits: &Value, key: &str, fallback: &str) -> Option<AccountLimit> {
    let window = rate_limits.get(key)?.as_object()?;
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
            .and_then(normalized_percent),
        resets_at: iso(window.get("resets_at").and_then(|value| value.as_i64())),
    })
}

fn semantic_rollout_limits(rate_limits: &Value) -> (Option<AccountLimit>, Option<AccountLimit>) {
    let mut five_hour = None;
    let mut weekly = None;

    for (key, fallback) in [("primary", "5h"), ("secondary", "week")] {
        let Some(limit) = limit(rate_limits, key, fallback) else {
            continue;
        };
        let target = if limit.label == "5h" {
            &mut five_hour
        } else {
            &mut weekly
        };
        if target.is_none() {
            *target = Some(limit);
        }
    }

    (five_hour, weekly)
}

fn account_limit(rate_limits: &Value, key: &str) -> anyhow::Result<Option<AccountLimit>> {
    let Some(value) = rate_limits.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let window = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid {key} codex limit"))?;
    let window_minutes = window
        .get("windowDurationMins")
        .or_else(|| window.get("window_minutes"))
        .and_then(|value| value.as_i64())
        .ok_or_else(|| anyhow::anyhow!("invalid {key} codex limit duration"))?;
    let label = match window_minutes {
        300 => "5h".to_string(),
        10_080 => "week".to_string(),
        _ => anyhow::bail!("unsupported {key} codex limit duration"),
    };
    let used_percent = window
        .get("usedPercent")
        .or_else(|| window.get("used_percent"))
        .and_then(|value| value.as_f64())
        .and_then(normalized_percent)
        .ok_or_else(|| anyhow::anyhow!("invalid {key} codex limit percent"))?;

    Ok(Some(AccountLimit {
        label,
        used_percent: Some(used_percent),
        resets_at: iso_value(window.get("resetsAt").or_else(|| window.get("resets_at"))),
    }))
}

fn semantic_account_limits(
    rate_limits: &Value,
) -> anyhow::Result<(Option<AccountLimit>, Option<AccountLimit>)> {
    let mut five_hour = None;
    let mut weekly = None;

    for key in ["primary", "secondary"] {
        let Some(limit) = account_limit(rate_limits, key)? else {
            continue;
        };
        let target = match limit.label.as_str() {
            "5h" => &mut five_hour,
            "week" => &mut weekly,
            _ => unreachable!("account_limit only returns known semantic labels"),
        };
        if target.replace(limit).is_some() {
            anyhow::bail!("duplicate codex account limit duration");
        }
    }

    if five_hour.is_none() && weekly.is_none() {
        anyhow::bail!("missing valid codex account limits");
    }
    Ok((five_hour, weekly))
}

fn account_rate_limits(root: &Value) -> Option<&Value> {
    let result = root.get("result").unwrap_or(root);
    if let Some(codex) = result
        .pointer("/rateLimitsByLimitId/codex")
        .filter(|value| value.is_object())
    {
        return Some(codex);
    }

    if let Some(by_id) = result
        .get("rateLimitsByLimitId")
        .and_then(|value| value.as_object())
    {
        if let Some((_, limits)) = by_id.iter().find(|(key, value)| {
            key.starts_with("codex")
                && value.is_object()
                && (value.get("primary").is_some() || value.get("secondary").is_some())
        }) {
            return Some(limits);
        }
    }

    result.get("rateLimits").filter(|value| value.is_object())
}

pub fn parse_account_rate_limits_response(
    json: &str,
    pc_id: &str,
    captured_at: &str,
) -> anyhow::Result<AgentStatus> {
    let root: Value = serde_json::from_str(json)?;
    if root.get("error").is_some() {
        anyhow::bail!("codex account API returned an error");
    }

    let rate_limits =
        account_rate_limits(&root).ok_or_else(|| anyhow::anyhow!("missing codex rate limits"))?;
    let (primary, secondary) = semantic_account_limits(rate_limits)?;

    Ok(AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Codex,
        session_id: "app-server-account".into(),
        captured_at: captured_at.into(),
        primary,
        secondary,
        session: SessionInfo {
            active: true,
            context_used_percent: None,
        },
        cost_estimate_usd: None,
        approx: false,
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
        .and_then(canonical_rfc3339)
        .unwrap_or_else(|| captured_at.to_string());

    let context_used_percent = {
        let input_tokens = payload
            .pointer("/info/last_token_usage/input_tokens")
            .and_then(|value| value.as_f64());
        let context_window = payload
            .pointer("/info/model_context_window")
            .and_then(|value| value.as_f64());

        match (input_tokens, context_window) {
            (Some(input_tokens), Some(context_window))
                if input_tokens.is_finite()
                    && input_tokens >= 0.0
                    && context_window.is_finite()
                    && context_window > 0.0 =>
            {
                normalized_percent(input_tokens / context_window * 100.0)
            }
            _ => None,
        }
    };

    let (primary, secondary) = match payload.get("rate_limits") {
        Some(rate_limits) => semantic_rollout_limits(rate_limits),
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

fn canonical_rfc3339(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollout_parser_drops_invalid_percent_fields_and_timestamp() {
        let status = parse_token_count(
            r#"{
                "type":"event_msg",
                "timestamp":"not-a-time",
                "payload":{
                    "type":"token_count",
                    "info":{"last_token_usage":{"input_tokens":200},"model_context_window":100},
                    "rate_limits":{
                        "primary":{"used_percent":-1,"window_minutes":300},
                        "secondary":{"used_percent":41,"window_minutes":10080}
                    }
                }
            }"#,
            "LOCAL",
            "session",
            "2026-07-10T03:00:00Z",
        )
        .unwrap();

        assert_eq!(status.captured_at, "2026-07-10T03:00:00Z");
        assert_eq!(status.session.context_used_percent, None);
        assert_eq!(status.primary.unwrap().used_percent, None);
        assert_eq!(status.secondary.unwrap().used_percent, Some(41.0));
    }
}
