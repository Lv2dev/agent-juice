use serde_json::Value;

use crate::model::*;

fn canonical_rfc3339(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(normalized_rfc3339)
}

fn period_label(value: &str) -> Option<&'static str> {
    match value.to_ascii_uppercase().as_str() {
        "USAGE_PERIOD_TYPE_WEEKLY" | "WEEKLY" => Some("week"),
        "USAGE_PERIOD_TYPE_MONTHLY" | "MONTHLY" => Some("month"),
        _ => None,
    }
}

fn preferred_limit(config: &Value) -> anyhow::Result<Option<AccountLimit>> {
    let Some(period) = config.get("currentPeriod") else {
        return Ok(None);
    };
    let period = period
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid Grok billing period"))?;
    let label = period
        .get("type")
        .and_then(Value::as_str)
        .and_then(period_label)
        .ok_or_else(|| anyhow::anyhow!("unsupported Grok billing period"))?;
    canonical_rfc3339(period.get("start"))
        .ok_or_else(|| anyhow::anyhow!("invalid Grok billing period start"))?;
    let resets_at = canonical_rfc3339(period.get("end"))
        .ok_or_else(|| anyhow::anyhow!("invalid Grok billing period end"))?;

    let used_percent = match config.get("creditUsagePercent") {
        Some(value) => value
            .as_f64()
            .and_then(normalized_percent)
            .ok_or_else(|| anyhow::anyhow!("invalid Grok credit usage percent"))?,
        None => 0.0,
    };
    Ok(Some(AccountLimit {
        label: label.into(),
        used_percent: Some(used_percent),
        resets_at: Some(resets_at),
    }))
}

fn legacy_limit(config: &Value) -> anyhow::Result<AccountLimit> {
    canonical_rfc3339(config.get("billingPeriodStart"))
        .ok_or_else(|| anyhow::anyhow!("invalid legacy Grok billing period start"))?;
    let resets_at = canonical_rfc3339(config.get("billingPeriodEnd"))
        .ok_or_else(|| anyhow::anyhow!("invalid legacy Grok billing period end"))?;
    let monthly_limit = config
        .pointer("/monthlyLimit/val")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow::anyhow!("invalid legacy Grok monthly limit"))?;
    let used = config
        .pointer("/used/val")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| anyhow::anyhow!("invalid legacy Grok used credits"))?;
    let used_percent = normalized_percent(used as f64 / monthly_limit as f64 * 100.0)
        .ok_or_else(|| anyhow::anyhow!("invalid legacy Grok usage percent"))?;

    Ok(AccountLimit {
        label: "month".into(),
        used_percent: Some(used_percent),
        resets_at: Some(resets_at),
    })
}

pub fn parse_billing_response(
    raw: &str,
    pc_id: &str,
    captured_at: &str,
) -> anyhow::Result<AgentStatus> {
    let root: Value = serde_json::from_str(raw)?;
    if root.get("error").is_some() {
        anyhow::bail!("Grok billing API returned an error");
    }
    let result = root.get("result").unwrap_or(&root);
    let config = result
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("missing Grok billing config"))?;
    let config = Value::Object(config.clone());
    let primary = match preferred_limit(&config)? {
        Some(limit) => limit,
        None => legacy_limit(&config)?,
    };

    Ok(AgentStatus {
        schema_version: SCHEMA_VERSION.into(),
        pc_id: pc_id.into(),
        tool: Tool::Grok,
        session_id: "grok-acp-billing".into(),
        captured_at: captured_at.into(),
        primary: Some(primary),
        secondary: None,
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
    fn parses_weekly_period_and_proto3_omitted_zero_percent() {
        let status = parse_billing_response(
            r#"{"jsonrpc":"2.0","id":2,"result":{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-08-10T00:00:00Z","end":"2026-08-17T00:00:00Z"}}}}"#,
            "LOCAL",
            "2026-08-13T00:00:00Z",
        )
        .unwrap();

        assert_eq!(status.tool, Tool::Grok);
        assert!(!status.approx);
        let limit = status.primary.unwrap();
        assert_eq!(limit.label, "week");
        assert_eq!(limit.used_percent, Some(0.0));
        assert_eq!(limit.resets_at.as_deref(), Some("2026-08-17T00:00:00Z"));
        assert!(status.secondary.is_none());
    }

    #[test]
    fn parses_monthly_period_and_usage_percent() {
        let status = parse_billing_response(
            r#"{"result":{"config":{"creditUsagePercent":42.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY","start":"2026-08-01T00:00:00Z","end":"2026-09-01T00:00:00Z"}}}}"#,
            "LOCAL",
            "2026-08-13T00:00:00Z",
        )
        .unwrap();

        let limit = status.primary.unwrap();
        assert_eq!(limit.label, "month");
        assert_eq!(limit.used_percent, Some(42.5));
    }

    #[test]
    fn falls_back_to_legacy_monthly_credit_fields() {
        let status = parse_billing_response(
            r#"{"result":{"config":{"monthlyLimit":{"val":2000},"used":{"val":500},"billingPeriodStart":"2026-08-01T00:00:00Z","billingPeriodEnd":"2026-09-01T00:00:00Z"}}}"#,
            "LOCAL",
            "2026-08-13T00:00:00Z",
        )
        .unwrap();

        let limit = status.primary.unwrap();
        assert_eq!(limit.label, "month");
        assert_eq!(limit.used_percent, Some(25.0));
    }

    #[test]
    fn rejects_invalid_or_unsupported_billing_shapes() {
        for raw in [
            r#"{"error":{"code":-32603}}"#,
            r#"{"result":{"config":null}}"#,
            r#"{"result":{"config":{"creditUsagePercent":101,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-08-10T00:00:00Z","end":"2026-08-17T00:00:00Z"}}}}"#,
            r#"{"result":{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_DAILY","start":"2026-08-10T00:00:00Z","end":"2026-08-17T00:00:00Z"}}}}"#,
            r#"{"result":{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"invalid","end":"2026-08-17T00:00:00Z"}}}}"#,
        ] {
            assert!(parse_billing_response(raw, "LOCAL", "2026-08-13T00:00:00Z").is_err());
        }
    }
}
