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

#[test]
fn parses_codex_account_rate_limits_and_prefers_codex_limit_id() {
    let response = r#"{"id":2,"result":{
      "rateLimits":{"primary":{"usedPercent":99,"windowDurationMins":300,"resetsAt":1782969938}},
      "rateLimitsByLimitId":{
        "codex":{"primary":{"usedPercent":5,"windowDurationMins":300,"resetsAt":1782969938},
                 "secondary":{"usedPercent":35,"windowDurationMins":10080,"resetsAt":1783388932}},
        "codex_bengalfox":{"primary":{"usedPercent":77,"windowDurationMins":300}}
      }
    }}"#;

    let status =
        codex::parse_account_rate_limits_response(response, "PC1", "2026-07-09T00:00:00Z").unwrap();

    assert_eq!(status.session_id, "app-server-account");
    assert!(!status.approx);
    assert_eq!(status.primary.as_ref().unwrap().label, "5h");
    assert_eq!(status.primary.as_ref().unwrap().used_percent, Some(5.0));
    assert_eq!(status.secondary.as_ref().unwrap().label, "week");
    assert_eq!(status.secondary.as_ref().unwrap().used_percent, Some(35.0));
    assert!(status.primary.as_ref().unwrap().resets_at.is_some());
}

#[test]
fn codex_account_rate_limits_fallback_to_result_rate_limits() {
    let response = r#"{"id":2,"result":{
      "rateLimits":{"primary":{"usedPercent":11,"windowDurationMins":300,"resetsAt":"2026-07-09T00:00:00Z"},
                    "secondary":{"usedPercent":22,"windowDurationMins":10080}}
    }}"#;

    let status =
        codex::parse_account_rate_limits_response(response, "PC1", "2026-07-09T00:00:00Z").unwrap();

    assert_eq!(status.primary.as_ref().unwrap().used_percent, Some(11.0));
    assert_eq!(
        status.primary.as_ref().unwrap().resets_at.as_deref(),
        Some("2026-07-09T00:00:00+00:00")
    );
    assert_eq!(status.secondary.as_ref().unwrap().used_percent, Some(22.0));
}

#[test]
fn codex_account_rate_limits_rejects_missing_limits_or_errors() {
    assert!(codex::parse_account_rate_limits_response(
        r#"{"id":2,"result":{"rateLimitsByLimitId":{}}}"#,
        "PC1",
        "2026-07-09T00:00:00Z"
    )
    .is_err());
    assert!(codex::parse_account_rate_limits_response(
        r#"{"id":2,"error":{"message":"not authenticated"}}"#,
        "PC1",
        "2026-07-09T00:00:00Z"
    )
    .is_err());
}

#[test]
fn codex_account_rate_limits_rejects_incomplete_or_invalid_windows() {
    let invalid_responses = [
        r#"{"id":2,"result":{"rateLimits":{"primary":null,"secondary":{"usedPercent":22,"windowDurationMins":10080}}}}"#,
        r#"{"id":2,"result":{"rateLimits":{"primary":{"windowDurationMins":300},"secondary":{"usedPercent":22,"windowDurationMins":10080}}}}"#,
        r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":"11","windowDurationMins":300},"secondary":{"usedPercent":22,"windowDurationMins":10080}}}}"#,
        r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":101,"windowDurationMins":300},"secondary":{"usedPercent":22,"windowDurationMins":10080}}}}"#,
        r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":11,"windowDurationMins":60},"secondary":{"usedPercent":22,"windowDurationMins":10080}}}}"#,
        r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":11,"windowDurationMins":300},"secondary":{"usedPercent":22,"windowDurationMins":1440}}}}"#,
        r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":11,"windowDurationMins":300}}}}"#,
    ];

    for response in invalid_responses {
        assert!(
            codex::parse_account_rate_limits_response(response, "PC1", "2026-07-09T00:00:00Z")
                .is_err(),
            "invalid response was accepted: {response}"
        );
    }
}
