use chrono::{TimeZone, Utc};
use serde::Deserialize;

use crate::model::*;

#[derive(Deserialize)]
struct ContextWindow {
    used_percentage: Option<f32>,
}

#[derive(Deserialize)]
struct LimitWindow {
    used_percentage: Option<f32>,
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
struct RateLimits {
    five_hour: Option<LimitWindow>,
    seven_day: Option<LimitWindow>,
}

#[derive(Deserialize)]
struct Cost {
    total_cost_usd: Option<f32>,
}

#[derive(Deserialize)]
struct StatusLine {
    session_id: Option<String>,
    context_window: Option<ContextWindow>,
    rate_limits: Option<RateLimits>,
    cost: Option<Cost>,
}

#[derive(Deserialize)]
struct UsageResult {
    result: Option<String>,
}

#[derive(Deserialize)]
struct OauthUsageWindow {
    utilization: Option<f32>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct OauthUsage {
    five_hour: Option<OauthUsageWindow>,
    seven_day: Option<OauthUsageWindow>,
}

fn iso(ts: Option<i64>) -> Option<String> {
    ts.and_then(|t| Utc.timestamp_opt(t, 0).single())
        .map(|d| d.to_rfc3339())
}

fn limit(label: &str, window: Option<LimitWindow>) -> Option<AccountLimit> {
    window.map(|window| AccountLimit {
        label: label.into(),
        used_percent: window.used_percentage,
        resets_at: iso(window.resets_at),
    })
}

pub fn parse(json: &str, pc_id: &str, captured_at: &str) -> anyhow::Result<AgentStatus> {
    let status: StatusLine = serde_json::from_str(json)?;
    let (primary, secondary) = match status.rate_limits {
        Some(rate_limits) => (
            limit("5h", rate_limits.five_hour),
            limit("week", rate_limits.seven_day),
        ),
        None => (None, None),
    };

    Ok(AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Claude,
        session_id: status.session_id.unwrap_or_default(),
        captured_at: captured_at.into(),
        primary,
        secondary,
        session: SessionInfo {
            active: true,
            context_used_percent: status.context_window.and_then(|c| c.used_percentage),
        },
        cost_estimate_usd: status.cost.and_then(|c| c.total_cost_usd),
        approx: true,
    })
}

fn usage_text(raw: &str) -> String {
    serde_json::from_str::<UsageResult>(raw)
        .ok()
        .and_then(|usage| usage.result)
        .unwrap_or_else(|| raw.to_string())
}

fn percent_after_prefix(text: &str, prefix: &str) -> Option<f32> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(prefix)?;
        let percent = rest.split('%').next()?.trim();
        percent.parse::<f32>().ok()
    })
}

fn current_session_percent(text: &str) -> Option<f32> {
    percent_after_prefix(text, "Current session:")
}

fn current_week_percent(text: &str) -> Option<f32> {
    percent_after_prefix(text, "Current week (all models):")
}

pub fn parse_usage_output(
    raw: &str,
    pc_id: &str,
    captured_at: &str,
) -> anyhow::Result<AgentStatus> {
    let text = usage_text(raw);
    let session_percent = current_session_percent(&text);
    let week_percent = current_week_percent(&text)
        .ok_or_else(|| anyhow::anyhow!("Claude usage output has no current week percent"))?;

    Ok(AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Claude,
        session_id: "claude-usage".into(),
        captured_at: captured_at.into(),
        primary: session_percent.map(|used_percent| AccountLimit {
            label: "5h".into(),
            used_percent: Some(used_percent),
            resets_at: None,
        }),
        secondary: Some(AccountLimit {
            label: "week".into(),
            used_percent: Some(week_percent),
            resets_at: None,
        }),
        session: SessionInfo {
            active: true,
            context_used_percent: None,
        },
        cost_estimate_usd: None,
        approx: true,
    })
}

fn oauth_limit(label: &str, window: Option<OauthUsageWindow>) -> Option<AccountLimit> {
    let window = window?;
    let used_percent = window
        .utilization
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value));
    Some(AccountLimit {
        label: label.into(),
        used_percent,
        resets_at: window.resets_at,
    })
}

pub fn parse_oauth_usage_response(
    raw: &str,
    pc_id: &str,
    captured_at: &str,
) -> anyhow::Result<AgentStatus> {
    let usage: OauthUsage = serde_json::from_str(raw)?;
    let primary = oauth_limit("5h", usage.five_hour);
    let secondary = oauth_limit("week", usage.seven_day);
    if primary
        .as_ref()
        .and_then(|limit| limit.used_percent)
        .is_none()
        && secondary
            .as_ref()
            .and_then(|limit| limit.used_percent)
            .is_none()
    {
        anyhow::bail!("Claude OAuth usage response has no supported utilization");
    }

    Ok(AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Claude,
        session_id: "claude-oauth-usage".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_usage_parser_reads_exact_five_hour_and_weekly_limits() {
        let status = parse_oauth_usage_response(
            r#"{
                "five_hour":{"utilization":78,"resets_at":"2026-07-10T04:50:00Z"},
                "seven_day":{"utilization":10,"resets_at":"2026-07-16T12:00:00Z"},
                "seven_day_sonnet":null
            }"#,
            "LOCAL",
            "2026-07-10T03:00:00Z",
        )
        .unwrap();

        assert_eq!(status.session_id, "claude-oauth-usage");
        assert_eq!(status.primary.unwrap().used_percent, Some(78.0));
        let weekly = status.secondary.unwrap();
        assert_eq!(weekly.used_percent, Some(10.0));
        assert_eq!(weekly.resets_at.as_deref(), Some("2026-07-16T12:00:00Z"));
        assert!(!status.approx);
    }

    #[test]
    fn oauth_usage_parser_rejects_missing_or_invalid_utilization() {
        assert!(parse_oauth_usage_response(
            r#"{"five_hour":{"utilization":101},"seven_day":null}"#,
            "LOCAL",
            "2026-07-10T03:00:00Z",
        )
        .is_err());
    }
}
