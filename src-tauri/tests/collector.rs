use agent_juice::collector::*;
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn unique_temp_dir() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-juice-collector-test-{}-{suffix}",
        std::process::id()
    ))
}

#[test]
fn picks_last_token_count_line() {
    let jsonl = concat!(
        r#"{"type":"response_item","x":1}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"other"}}"#,
        "\n"
    );
    let line = last_token_count_line(jsonl).unwrap();
    assert!(line.contains("token_count") && line.contains("258400"));
}

#[test]
fn token_count_line_skips_malformed_and_non_token_matches() {
    let jsonl = concat!(
        r#"{"timestamp":"2026-07-07T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"message","text":"mentions token_count but is not usage"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"token_count","info":"#,
        "\n"
    );

    let line = last_token_count_line(jsonl).unwrap();

    assert!(line.contains("258400"));
    assert!(line.contains(r#""type":"token_count""#));
}

#[test]
fn reads_last_token_count_line_from_file_tail() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let path = root.join("rollout-2026-07-08-tail.jsonl");
    fs::write(
        &path,
        concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":100}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"message","text":"not usage"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":"#,
            "\n"
        ),
    )
    .unwrap();

    let line = last_token_count_line_from_file(&path).unwrap().unwrap();

    assert!(line.contains("258400"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recursively_lists_rollouts_and_picks_latest() {
    let root = unique_temp_dir();
    let day = root.join("2026").join("07").join("07");
    fs::create_dir_all(&day).unwrap();

    let older = day.join("rollout-2026-07-07-old.jsonl");
    let ignored = day.join("notes.jsonl");
    fs::write(&older, "{}\n").unwrap();
    fs::write(&ignored, "{}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));

    let newer = day.join("rollout-2026-07-07-new.jsonl");
    fs::write(&newer, "{}\n").unwrap();

    let mut names: Vec<_> = list_rollouts(&root)
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "rollout-2026-07-07-new.jsonl".to_string(),
            "rollout-2026-07-07-old.jsonl".to_string()
        ]
    );
    assert_eq!(latest_rollout(&root).unwrap(), newer);

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn recent_rollouts_are_mtime_desc_and_limited() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();

    let oldest = root.join("rollout-oldest.jsonl");
    fs::write(&oldest, "{}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let middle = root.join("rollout-middle.jsonl");
    fs::write(&middle, "{}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let newest = root.join("rollout-newest.jsonl");
    fs::write(&newest, "{}\n").unwrap();

    let names: Vec<_> = recent_rollouts(&root, 2)
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();

    assert_eq!(
        names,
        vec![
            "rollout-newest.jsonl".to_string(),
            "rollout-middle.jsonl".to_string()
        ]
    );

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn derives_session_id_from_rollout_stem() {
    let path = Path::new("sessions/2026/07/07/rollout-2026-07-07-session-uuid.jsonl");

    assert_eq!(
        session_id_of(path),
        "rollout-2026-07-07-session-uuid".to_string()
    );
}
