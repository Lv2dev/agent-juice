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
