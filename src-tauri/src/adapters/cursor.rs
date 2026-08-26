use chrono::NaiveDate;

use crate::model::{
    normalized_percent, AccountLimit, AgentStatus, SessionInfo, Tool, SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CursorUsageSnapshot {
    pub plan_name: String,
    pub cursor_models_used_percent: f32,
    pub other_models_used_percent: f32,
    pub reset_date: Option<String>,
}

fn percent_after_label(raw: &str, label: &str) -> Option<f32> {
    raw.rmatch_indices(label).find_map(|(index, _)| {
        let tail = raw.get(index + label.len()..)?;
        let tail = tail.lines().next().unwrap_or(tail);
        let percent_index = tail.find('%')?;
        if !tail
            .get(percent_index + 1..)?
            .trim_start()
            .starts_with("used")
        {
            return None;
        }
        let number = tail.get(..percent_index)?.split_whitespace().next()?;
        number.parse::<f64>().ok().and_then(normalized_percent)
    })
}

fn percent_after_labels(raw: &str, labels: &[&str]) -> Option<f32> {
    labels
        .iter()
        .find_map(|label| percent_after_label(raw, label))
}

fn plan_name(raw: &str) -> String {
    raw.lines()
        .filter_map(|line| line.split_once("Usage • ").map(|(_, tail)| tail))
        .filter_map(|tail| tail.split("Resets").next())
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or("Cursor")
        .to_string()
}

fn reset_label(raw: &str) -> Option<&str> {
    raw.lines()
        .filter_map(|line| line.rsplit_once("Resets ").map(|(_, tail)| tail.trim()))
        .find(|value| !value.is_empty())
}

fn normalized_month_day(month: u32, day: u32) -> Option<String> {
    NaiveDate::from_ymd_opt(2000, month, day)?;
    Some(format!("{month:02}-{day:02}"))
}

fn parse_marked_numeric_date(label: &str) -> Option<String> {
    for (month_marker, day_marker) in [("월", "일"), ("月", "日")] {
        let Some((month_text, tail)) = label.split_once(month_marker) else {
            continue;
        };
        let Some((day_text, _)) = tail.split_once(day_marker) else {
            continue;
        };
        let month = month_text.split_whitespace().last()?.parse::<u32>().ok()?;
        let day = day_text.trim().parse::<u32>().ok()?;
        return normalized_month_day(month, day);
    }
    None
}

fn parse_english_date(label: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let normalized = label.to_ascii_lowercase().replace([',', '.', '-'], " ");
    let parts = normalized.split_whitespace().collect::<Vec<_>>();
    let (month_index, month) = parts.iter().enumerate().find_map(|(index, part)| {
        let short = part.get(..part.len().min(3))?;
        MONTHS
            .iter()
            .position(|month| *month == short)
            .map(|month| (index, month as u32 + 1))
    })?;
    let day = parts
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != month_index)
        .find_map(|(_, part)| {
            part.parse::<u32>()
                .ok()
                .filter(|value| (1..=31).contains(value))
        })?;
    normalized_month_day(month, day)
}

fn parse_reset_date(label: &str) -> Option<String> {
    parse_marked_numeric_date(label).or_else(|| parse_english_date(label))
}

pub fn parse_usage_output(raw: &str) -> anyhow::Result<CursorUsageSnapshot> {
    let cursor_models_used_percent = percent_after_labels(raw, &["Auto", "Cursor Models"])
        .ok_or_else(|| anyhow::anyhow!("missing Cursor Auto usage"))?;
    let other_models_used_percent = percent_after_labels(raw, &["API", "Other Models"])
        .ok_or_else(|| anyhow::anyhow!("missing Cursor API usage"))?;
    Ok(CursorUsageSnapshot {
        plan_name: plan_name(raw),
        cursor_models_used_percent,
        other_models_used_percent,
        reset_date: reset_label(raw).and_then(parse_reset_date),
    })
}

pub fn parse_usage_status(
    raw: &str,
    pc_id: &str,
    captured_at: &str,
) -> anyhow::Result<AgentStatus> {
    let snapshot = parse_usage_output(raw)?;
    Ok(status_from_values(
        pc_id,
        captured_at,
        "cursor-agent-usage",
        snapshot.cursor_models_used_percent,
        snapshot.other_models_used_percent,
        snapshot.reset_date,
    ))
}

pub fn dashboard_usage_status(
    pc_id: &str,
    captured_at: &str,
    cursor_models_used_percent: f32,
    other_models_used_percent: f32,
    reset_at: String,
) -> anyhow::Result<AgentStatus> {
    let cursor_models_used_percent = normalized_percent(cursor_models_used_percent)
        .ok_or_else(|| anyhow::anyhow!("invalid Cursor Models usage"))?;
    let other_models_used_percent = normalized_percent(other_models_used_percent)
        .ok_or_else(|| anyhow::anyhow!("invalid Cursor Other Models usage"))?;
    Ok(status_from_values(
        pc_id,
        captured_at,
        "cursor-gui-usage",
        cursor_models_used_percent,
        other_models_used_percent,
        Some(reset_at),
    ))
}

fn status_from_values(
    pc_id: &str,
    captured_at: &str,
    session_id: &str,
    cursor_models_used_percent: f32,
    other_models_used_percent: f32,
    reset_at: Option<String>,
) -> AgentStatus {
    AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Cursor,
        session_id: session_id.into(),
        captured_at: captured_at.into(),
        primary: Some(AccountLimit {
            label: "cursor_models".into(),
            used_percent: Some(cursor_models_used_percent),
            resets_at: reset_at.clone(),
        }),
        secondary: Some(AccountLimit {
            label: "other_models".into(),
            used_percent: Some(other_models_used_percent),
            resets_at: reset_at,
        }),
        session: SessionInfo {
            active: true,
            context_used_percent: None,
        },
        cost_estimate_usd: None,
        approx: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pro_plus_auto_api_and_korean_reset_date() {
        let output = r#"
 Usage • Pro+                                                 Resets 9월 21일
 Monthly plan and on-demand usage
 Category        Current             Usage
 Included        1% used
   Auto          1% used
   API           0% used
 On-Demand       Disabled
 Esc to close
"#;
        let snapshot = parse_usage_output(output).unwrap();
        assert_eq!(snapshot.plan_name, "Pro+");
        assert_eq!(snapshot.cursor_models_used_percent, 1.0);
        assert_eq!(snapshot.other_models_used_percent, 0.0);
        assert_eq!(snapshot.reset_date.as_deref(), Some("09-21"));

        let status = parse_usage_status(output, "LOCAL", "2026-08-21T00:00:00Z").unwrap();
        assert_eq!(status.tool, Tool::Cursor);
        assert_eq!(status.primary.unwrap().label, "cursor_models");
        assert_eq!(status.secondary.unwrap().label, "other_models");
    }

    #[test]
    fn parses_english_reset_without_inventing_a_year() {
        let output = "Usage • Pro+ Resets Jan 4\nAuto 9.5% used\nAPI 12% used";
        let snapshot = parse_usage_output(output).unwrap();
        assert_eq!(snapshot.cursor_models_used_percent, 9.5);
        assert_eq!(snapshot.other_models_used_percent, 12.0);
        assert_eq!(snapshot.reset_date.as_deref(), Some("01-04"));
    }

    #[test]
    fn accepts_future_pool_labels_without_changing_the_data_contract() {
        let output = "Usage • Pro+ Resets Sep 21\nCursor Models 2% used\nOther Models 3% used";
        let snapshot = parse_usage_output(output).unwrap();
        assert_eq!(snapshot.cursor_models_used_percent, 2.0);
        assert_eq!(snapshot.other_models_used_percent, 3.0);
    }

    #[test]
    fn keeps_usage_when_a_localized_reset_is_unknown() {
        let output = "Usage • Pro+ Resets 21/09\nAuto 1% used\nAPI 0% used";
        let snapshot = parse_usage_output(output).unwrap();
        assert_eq!(snapshot.reset_date, None);
    }

    #[test]
    fn rejects_missing_or_out_of_range_pool_usage() {
        for output in [
            "Usage • Pro+\nAuto 1% used",
            "Usage • Pro+\nAuto 101% used\nAPI 0% used",
            "Usage • Pro+\nAuto 1% used\nAPI -1% used",
        ] {
            assert!(parse_usage_output(output).is_err());
        }
    }

    #[test]
    fn builds_exact_dashboard_status_with_full_reset_timestamp() {
        let status = dashboard_usage_status(
            "LOCAL",
            "2026-08-25T00:00:00Z",
            12.5,
            3.0,
            "2026-09-21T00:00:00Z".into(),
        )
        .unwrap();
        assert_eq!(status.session_id, "cursor-gui-usage");
        assert_eq!(status.primary.unwrap().used_percent, Some(12.5));
        assert_eq!(
            status.secondary.unwrap().resets_at.as_deref(),
            Some("2026-09-21T00:00:00Z")
        );
    }
}
