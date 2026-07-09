use agent_juice::model::*;

#[test]
fn agent_status_roundtrips_and_nullsafe() {
    let json = r#"{"schema_version":"agent_status.v1","pc_id":"PC1","tool":"claude",
      "session_id":"s-abc","captured_at":"2026-07-06T14:52:00+09:00",
      "primary":{"label":"5h","used_percent":88.0,"resets_at":"2026-07-06T16:04:00+09:00"},
      "secondary":null,
      "session":{"active":true,"context_used_percent":63.0},
      "cost_estimate_usd":0.56,"approx":true}"#;
    let s: AgentStatus = serde_json::from_str(json).unwrap();
    assert_eq!(s.tool, Tool::Claude);
    assert_eq!(s.session_id, "s-abc");
    assert_eq!(s.primary.as_ref().unwrap().used_percent, Some(88.0));
    assert!(s.secondary.is_none());
    let back = serde_json::to_string(&s).unwrap();
    assert!(back.contains("agent_status.v1"));

    let legacy = r#"{"schema_version":"agent_status.v1","pc_id":"PC1","tool":"codex",
      "captured_at":"t","primary":null,"secondary":null,
      "session":{"active":false,"context_used_percent":null},"cost_estimate_usd":null,"approx":true}"#;
    let l: AgentStatus = serde_json::from_str(legacy).unwrap();
    assert_eq!(l.session_id, "");
}
