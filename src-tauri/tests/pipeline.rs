use agent_juice::{
    collect_all_from, collect_representatives_from,
    config::Settings,
    latest_per_tool,
    model::{AgentStatus, Tool},
};
use chrono::{TimeZone, Utc};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-juice-pipeline-test-{}-{suffix}-{sequence}",
        std::process::id(),
    ))
}

#[test]
fn temp_fixture_paths_are_unique_across_parallel_calls() {
    let workers: Vec<_> = (0..32)
        .map(|_| std::thread::spawn(unique_temp_dir))
        .collect();
    let paths: std::collections::HashSet<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();

    assert_eq!(paths.len(), 32);
}

#[test]
fn collect_all_reads_all_forward_and_rollout_sessions_and_derives_active() {
    let root = unique_temp_dir();
    let data_dir = root.join("data");
    let sessions_dir = root.join("sessions").join("2026").join("07").join("07");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&sessions_dir).unwrap();

    fs::write(
        data_dir.join("claude_last.s1.json"),
        r#"{"session_id":"s1","context_window":{"used_percentage":63},"rate_limits":{"five_hour":{"used_percentage":88},"seven_day":{"used_percentage":41}}}"#,
    )
    .unwrap();
    fs::write(
        data_dir.join("claude_last.s2.json"),
        r#"{"session_id":"s2","context_window":{"used_percentage":12}}"#,
    )
    .unwrap();
    fs::write(data_dir.join("claude_last_ignored.json"), "{}").unwrap();

    fs::write(
        sessions_dir.join("rollout-2026-07-07-codex-old.jsonl"),
        concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":50},"model_context_window":100},"rate_limits":{"primary":{"used_percent":10,"window_minutes":300}}}}"#,
            "\n",
            r#"{"timestamp":"2026-07-07T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":20},"model_context_window":100},"rate_limits":{"primary":{"used_percent":20,"window_minutes":300}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let settings = Settings {
        stale_after_secs: 90,
        ..Settings::default()
    };
    let now = Utc.with_ymd_and_hms(2026, 7, 7, 0, 2, 0).unwrap();
    let statuses = collect_all_from(
        &settings,
        Some(&data_dir),
        Some(root.join("sessions").as_path()),
        now,
    );

    let mut keys: Vec<_> = statuses
        .iter()
        .map(|status| (status.tool.clone(), status.session_id.clone()))
        .collect();
    keys.sort_by(|a, b| format!("{:?}{:?}", a.0, a.1).cmp(&format!("{:?}{:?}", b.0, b.1)));

    assert_eq!(statuses.len(), 3);
    assert!(keys.contains(&(Tool::Claude, "s1".to_string())));
    assert!(keys.contains(&(Tool::Claude, "s2".to_string())));
    assert!(keys.contains(&(Tool::Codex, "rollout-2026-07-07-codex-old".to_string())));

    let stale_codex = statuses
        .iter()
        .find(|status| status.tool == Tool::Codex)
        .unwrap();
    assert_eq!(stale_codex.session.context_used_percent, Some(20.0));
    assert!(!stale_codex.session.active);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn collect_representatives_reads_only_latest_status_per_tool() {
    let root = unique_temp_dir();
    let data_dir = root.join("data");
    let sessions_dir = root.join("sessions").join("2026").join("07").join("07");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&sessions_dir).unwrap();

    fs::write(
        data_dir.join("claude_last.old.json"),
        r#"{"session_id":"old","context_window":{"used_percentage":10},"rate_limits":{"five_hour":{"used_percentage":10}}}"#,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        data_dir.join("claude_last.new.json"),
        r#"{"session_id":"new","context_window":{"used_percentage":70},"rate_limits":{"five_hour":{"used_percentage":70}}}"#,
    )
    .unwrap();

    fs::write(
        sessions_dir.join("rollout-older.jsonl"),
        r#"{"timestamp":"2026-07-07T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10},"model_context_window":100},"rate_limits":{"primary":{"used_percent":10,"window_minutes":300}}}}"#,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        sessions_dir.join("rollout-newer.jsonl"),
        r#"{"timestamp":"2026-07-07T00:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":42},"model_context_window":100},"rate_limits":{"primary":{"used_percent":42,"window_minutes":300}}}}"#,
    )
    .unwrap();

    let settings = Settings {
        stale_after_secs: 90,
        ..Settings::default()
    };
    let now = Utc.with_ymd_and_hms(2026, 7, 7, 0, 1, 30).unwrap();
    let statuses = collect_representatives_from(
        &settings,
        Some(&data_dir),
        Some(root.join("sessions").as_path()),
        now,
    );

    assert_eq!(statuses.len(), 2);
    assert!(statuses
        .iter()
        .any(|status| status.tool == Tool::Claude && status.session_id == "new"));
    let codex = statuses
        .iter()
        .find(|status| status.tool == Tool::Codex)
        .unwrap();
    assert_eq!(codex.session_id, "rollout-newer");
    assert_eq!(
        codex.primary.as_ref().and_then(|limit| limit.used_percent),
        Some(42.0)
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn disabled_tools_are_excluded_from_all_collection_paths() {
    let root = unique_temp_dir();
    let data_dir = root.join("data");
    let sessions_dir = root.join("sessions").join("2026").join("07").join("07");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        data_dir.join("claude_last.enabled.json"),
        r#"{"session_id":"claude","rate_limits":{"five_hour":{"used_percentage":25}}}"#,
    )
    .unwrap();
    fs::write(
        sessions_dir.join("rollout-enabled.jsonl"),
        r#"{"timestamp":"2026-07-07T00:01:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":40,"window_minutes":300}}}}"#,
    )
    .unwrap();
    let now = Utc.with_ymd_and_hms(2026, 7, 7, 0, 1, 30).unwrap();

    let codex_only = Settings {
        show_claude: false,
        show_codex: true,
        ..Settings::default()
    };
    for statuses in [
        collect_all_from(
            &codex_only,
            Some(&data_dir),
            Some(root.join("sessions").as_path()),
            now,
        ),
        collect_representatives_from(
            &codex_only,
            Some(&data_dir),
            Some(root.join("sessions").as_path()),
            now,
        ),
    ] {
        assert!(!statuses.is_empty());
        assert!(statuses.iter().all(|status| status.tool == Tool::Codex));
    }

    let claude_only = Settings {
        show_claude: true,
        show_codex: false,
        ..Settings::default()
    };
    let statuses = collect_representatives_from(
        &claude_only,
        Some(&data_dir),
        Some(root.join("sessions").as_path()),
        now,
    );
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].tool, Tool::Claude);

    let none = Settings {
        show_claude: false,
        show_codex: false,
        ..Settings::default()
    };
    assert!(collect_all_from(
        &none,
        Some(&data_dir),
        Some(root.join("sessions").as_path()),
        now,
    )
    .is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn collect_representatives_backtracks_to_recent_codex_rollout_with_token_count() {
    let root = unique_temp_dir();
    let sessions_dir = root.join("sessions").join("2026").join("07").join("08");
    fs::create_dir_all(&sessions_dir).unwrap();

    fs::write(
        sessions_dir.join("rollout-valid.jsonl"),
        r#"{"timestamp":"2026-07-08T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":37},"model_context_window":100},"rate_limits":{"primary":{"used_percent":37,"window_minutes":300}}}}"#,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        sessions_dir.join("rollout-newest-without-token-count.jsonl"),
        r#"{"timestamp":"2026-07-08T00:01:00Z","type":"event_msg","payload":{"type":"other"}}"#,
    )
    .unwrap();

    let settings = Settings {
        stale_after_secs: 90,
        ..Settings::default()
    };
    let now = Utc.with_ymd_and_hms(2026, 7, 8, 0, 1, 30).unwrap();
    let statuses =
        collect_representatives_from(&settings, None, Some(root.join("sessions").as_path()), now);

    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].tool, Tool::Codex);
    assert_eq!(statuses[0].session_id, "rollout-valid");
    assert_eq!(
        statuses[0]
            .primary
            .as_ref()
            .and_then(|limit| limit.used_percent),
        Some(37.0)
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn latest_per_tool_picks_newest_status_for_each_tool() {
    let older_claude = status(Tool::Claude, "old", "2026-07-07T00:00:00Z");
    let newer_claude = status(Tool::Claude, "new", "2026-07-07T00:03:00Z");
    let codex = status(Tool::Codex, "codex", "2026-07-07T00:01:00Z");

    let mut reps = latest_per_tool(&[older_claude, newer_claude, codex]);
    reps.sort_by(|a, b| format!("{:?}", a.tool).cmp(&format!("{:?}", b.tool)));

    assert_eq!(reps.len(), 2);
    assert!(reps.iter().any(|status| status.session_id == "new"));
    assert!(reps.iter().any(|status| status.session_id == "codex"));
}

#[test]
fn latest_per_tool_prefers_valid_timestamps_over_invalid_strings() {
    let invalid = status(Tool::Codex, "bad", "zzzz-invalid");
    let valid = status(Tool::Codex, "good", "2026-07-07T00:01:00Z");

    let reps = latest_per_tool(&[valid, invalid]);

    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].session_id, "good");
}

#[test]
fn future_captured_at_is_not_active() {
    let root = unique_temp_dir();
    let sessions_dir = root.join("sessions").join("2026").join("07").join("07");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("rollout-2026-07-07-future.jsonl"),
        r#"{"timestamp":"2026-07-07T00:05:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":20},"model_context_window":100}}}"#,
    )
    .unwrap();

    let settings = Settings {
        stale_after_secs: 90,
        ..Settings::default()
    };
    let now = Utc.with_ymd_and_hms(2026, 7, 7, 0, 2, 0).unwrap();
    let statuses = collect_all_from(&settings, None, Some(root.join("sessions").as_path()), now);

    assert_eq!(statuses.len(), 1);
    assert!(!statuses[0].session.active);

    fs::remove_dir_all(root).unwrap();
}

fn status(tool: Tool, session_id: &str, captured_at: &str) -> AgentStatus {
    AgentStatus {
        schema_version: "agent_status.v1".into(),
        pc_id: "PC".into(),
        tool,
        session_id: session_id.into(),
        captured_at: captured_at.into(),
        primary: None,
        secondary: None,
        session: agent_juice::model::SessionInfo {
            active: true,
            context_used_percent: None,
        },
        cost_estimate_usd: None,
        approx: true,
    }
}
