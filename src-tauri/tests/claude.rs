use agent_juice::adapters::claude;

#[test]
fn parses_full_and_missing_ratelimits() {
    let full = r#"{"session_id":"abc","context_window":{"used_percentage":63},
      "rate_limits":{"five_hour":{"used_percentage":88,"resets_at":1783330000},
                     "seven_day":{"used_percentage":41,"resets_at":1783500000}},
      "cost":{"total_cost_usd":0.56}}"#;
    let s = claude::parse(full, "PC1", "2026-07-06T14:52:00+09:00").unwrap();
    assert_eq!(s.session_id, "abc");
    assert_eq!(s.session.context_used_percent, Some(63.0));
    assert_eq!(s.primary.as_ref().unwrap().used_percent, Some(88.0));
    assert_eq!(s.secondary.as_ref().unwrap().label, "week");
    assert_eq!(s.cost_estimate_usd, Some(0.56));

    let bare = r#"{"context_window":{"used_percentage":10}}"#;
    let s2 = claude::parse(bare, "PC1", "t").unwrap();
    assert!(s2.primary.is_none() && s2.secondary.is_none());
    assert_eq!(s2.session_id, "");
    assert!(s2.approx);
}

#[test]
fn parses_usage_current_session_as_primary_and_current_week_as_secondary() {
    let raw = r#"{"type":"result","result":"Claude Code usage\n\nCurrent session: 12% used · resets Jul 10, 3:49am (Asia/Seoul)\nCurrent week (all models): 38% used · resets Jul 16, 8:59pm (Asia/Seoul)\nCurrent week (Opus): 0% used\n","num_turns":0,"total_cost_usd":0.0}"#;

    let status = claude::parse_usage_output(raw, "PC1", "2026-07-09T12:00:00Z").unwrap();

    assert_eq!(status.session_id, "claude-usage");
    assert_eq!(status.primary.as_ref().unwrap().label, "5h");
    assert_eq!(status.primary.as_ref().unwrap().used_percent, Some(12.0));
    assert!(status.primary.as_ref().unwrap().resets_at.is_none());
    assert_eq!(status.secondary.as_ref().unwrap().label, "week");
    assert_eq!(status.secondary.as_ref().unwrap().used_percent, Some(38.0));
    assert!(status.secondary.as_ref().unwrap().resets_at.is_none());
    assert_eq!(status.cost_estimate_usd, None);
    assert!(status.session.active);
    assert!(status.approx);
}

#[test]
fn rejects_usage_output_without_current_week_percent() {
    let raw = r#"{"type":"result","result":"Claude Code usage\n\nNo usage limit information yet\n","num_turns":0,"total_cost_usd":0.0}"#;

    assert!(claude::parse_usage_output(raw, "PC1", "2026-07-09T12:00:00Z").is_err());
}
