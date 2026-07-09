use agent_juice::statusline;
use serde_json::Value;
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn unique_temp_dir() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-juice-statusline-test-{}-{suffix}",
        std::process::id()
    ))
}

#[test]
fn forwards_subset_to_session_file_and_falls_back_to_context_line() {
    let dir = unique_temp_dir();
    let input = r#"{"session_id":"s/1:bad","context_window":{"used_percentage":63},"rate_limits":{"five_hour":{"used_percentage":11}},"cost":{"total_cost_usd":0.12},"cwd":"C:/secret","transcript_path":"secret.jsonl","prompt":"hidden"}"#;

    let output = statusline::run_with_dir(input, &dir);

    assert_eq!(String::from_utf8(output).unwrap(), "ctx 63%\n");
    let written = fs::read_to_string(dir.join("claude_last.s1bad.json")).unwrap();
    let subset: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(subset["session_id"], "s/1:bad");
    assert_eq!(subset["context_window"]["used_percentage"], 63.0);
    assert_eq!(subset["rate_limits"]["five_hour"]["used_percentage"], 11.0);
    assert_eq!(subset["cost"]["total_cost_usd"], 0.12);
    assert!(subset.get("cwd").is_none());
    assert!(subset.get("transcript_path").is_none());
    assert!(subset.get("prompt").is_none());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runs_original_cat_wrap_when_available() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("wrap.json"), "cat").unwrap();
    let input = r#"{"session_id":"t","context_window":{"used_percentage":63}}"#;

    let output = statusline::run_with_dir(input, &dir);

    assert_eq!(String::from_utf8(output).unwrap(), input);
    assert!(dir.join("claude_last.t.json").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ignores_wrap_when_metadata_does_not_match() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("wrap.json"), "echo changed").unwrap();
    fs::write(
        dir.join("wrap-meta.json"),
        r#"{"managed_command":"\"C:/Juice/agentjuice-statusline.exe\"","original_command":"echo original"}"#,
    )
    .unwrap();
    let input = r#"{"session_id":"t","context_window":{"used_percentage":63}}"#;

    let output = statusline::run_with_dir(input, &dir);

    assert_eq!(String::from_utf8(output).unwrap(), "ctx 63%\n");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn oversized_original_wrap_is_ignored() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("wrap.json"), "x".repeat(8192)).unwrap();
    let input = r#"{"session_id":"t","context_window":{"used_percentage":63}}"#;

    let output = statusline::run_with_dir(input, &dir);

    assert_eq!(String::from_utf8(output).unwrap(), "ctx 63%\n");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn statusline_uses_single_original_command_runner() {
    let source = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/statusline.rs"),
    )
    .unwrap();

    assert!(source.contains("(\"cmd\", &[\"/C\"])") || source.contains("(\"sh\", &[\"-c\"])"));
    assert!(!source.contains("\"powershell\""));
    assert!(!source.contains("\"pwsh\""));
    assert!(!source.contains("\"bash\""));
}

#[test]
fn statusline_forward_replace_is_atomic_on_windows() {
    let source = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/statusline.rs"),
    )
    .unwrap();

    assert!(source.contains("ReplaceFileW") || !cfg!(windows));
    assert!(!source.contains("fs::remove_file(path)?"));
}
