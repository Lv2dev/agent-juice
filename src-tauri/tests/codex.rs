use agent_juice::adapters::codex;

#[test]
fn parses_real_envelope_and_computes_context() {
    let ev = r#"{"timestamp":"2026-07-02T02:17:30.218Z","type":"event_msg","payload":{
      "type":"token_count",
      "info":{"total_token_usage":{"total_tokens":227384},
              "last_token_usage":{"input_tokens":53567,"total_tokens":54402},
              "model_context_window":258400},
      "rate_limits":{"primary":{"used_percent":7.0,"window_minutes":300,"resets_at":1782969938},
                     "secondary":{"used_percent":7.0,"window_minutes":10080,"resets_at":1783388932}}}}"#;
    let s = codex::parse_token_count(ev, "PC1", "sess-uuid", "2026-07-06T14:52:00+09:00").unwrap();
    assert_eq!(s.session_id, "sess-uuid");
    assert!(s.captured_at.starts_with("2026-07-02"));
    assert!((s.session.context_used_percent.unwrap() - 20.73).abs() < 0.1);
    assert_eq!(s.primary.as_ref().unwrap().used_percent, Some(7.0));
    assert_eq!(s.primary.as_ref().unwrap().label, "5h");
    assert_eq!(s.secondary.as_ref().unwrap().label, "week");
    assert!(s.primary.as_ref().unwrap().resets_at.is_some());

    let bad = r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100},"model_context_window":0}}}"#;
    let s2 = codex::parse_token_count(bad, "PC1", "sid", "t").unwrap();
    assert!(s2.session.context_used_percent.is_none());
    assert!(s2.primary.is_none());
    assert_eq!(s2.captured_at, "t");
}

#[test]
fn rejects_non_token_count_payloads() {
    let ev = r#"{"timestamp":"2026-07-02T02:17:30.218Z","type":"event_msg","payload":{"type":"message","text":"token_count only in text"}}"#;

    assert!(codex::parse_token_count(ev, "PC1", "sid", "fallback").is_err());
}

#[test]
fn invalid_timestamp_falls_back_to_file_time() {
    let ev = r#"{"timestamp":"not-a-date","type":"event_msg","payload":{
      "type":"token_count",
      "info":{"last_token_usage":{"input_tokens":10},"model_context_window":100}
    }}"#;

    let s = codex::parse_token_count(ev, "PC1", "sid", "2026-07-07T00:00:00Z").unwrap();

    assert_eq!(s.captured_at, "2026-07-07T00:00:00Z");
}
