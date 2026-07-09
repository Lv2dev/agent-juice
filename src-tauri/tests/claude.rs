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
