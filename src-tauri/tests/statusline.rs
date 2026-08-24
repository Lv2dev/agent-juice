use agent_juice::{config::Settings, statusline};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    temp_dir_with_suffix(suffix)
}

fn temp_dir_with_suffix(suffix: u128) -> std::path::PathBuf {
    let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-juice-statusline-test-{}-{suffix}-{sequence}",
        std::process::id()
    ))
}

#[test]
fn temp_fixture_paths_are_unique_when_clock_values_repeat() {
    let workers: Vec<_> = (0..32)
        .map(|_| std::thread::spawn(|| temp_dir_with_suffix(0)))
        .collect();
    let paths: std::collections::HashSet<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();

    assert_eq!(paths.len(), 32);
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
fn ignores_original_wrap_without_starting_a_child_process() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("original-ran.txt");
    let original = if cfg!(windows) {
        format!("echo original>{}", marker.display())
    } else {
        format!("echo original > {}", marker.display())
    };
    fs::write(dir.join("wrap.json"), &original).unwrap();
    fs::write(
        dir.join("wrap-meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "managed_command": "fixture-managed",
            "original_command": original,
        }))
        .unwrap(),
    )
    .unwrap();
    let input = r#"{"session_id":"t","context_window":{"used_percentage":63}}"#;

    let output = statusline::run_with_dir(input, &dir);

    assert_eq!(String::from_utf8(output).unwrap(), "ctx 63%\n");
    assert!(dir.join("claude_last.t.json").exists());
    assert!(!marker.exists());

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
fn session_state_retention_is_bounded_and_keeps_current_write() {
    let dir = unique_temp_dir();
    for index in 0..(statusline::MAX_CLAUDE_SESSION_FILES + 8) {
        let input = serde_json::json!({
            "session_id": format!("session-{index:03}"),
            "context_window": {"used_percentage": index % 100},
        });
        statusline::run_with_dir(&input.to_string(), &dir);
    }

    let retained = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("claude_last.") && name.ends_with(".json")
        })
        .count();
    assert_eq!(retained, statusline::MAX_CLAUDE_SESSION_FILES);
    assert!(dir
        .join(format!(
            "claude_last.session-{:03}.json",
            statusline::MAX_CLAUDE_SESSION_FILES + 7
        ))
        .exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn statusline_binary_bounds_oversized_stdin_without_losing_juice_state() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("wrap.json"), "cat").unwrap();
    let prefix = br#"{"session_id":"oversized","context_window":{"used_percentage":63}}"#;
    let mut input = Vec::with_capacity(statusline::MAX_STATUSLINE_INPUT_BYTES + 128);
    input.extend_from_slice(prefix);
    input.resize(statusline::MAX_STATUSLINE_INPUT_BYTES + 128, b' ');

    let mut child = Command::new(env!("CARGO_BIN_EXE_agentjuice-statusline"))
        .env("AGENT_JUICE_DATA_DIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(&input);
    drop(stdin);
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "ctx 63%\n");
    let subset: Value =
        serde_json::from_slice(&fs::read(dir.join("claude_last.oversized.json")).unwrap()).unwrap();
    assert_eq!(subset["session_id"], "oversized");
    assert_eq!(subset["context_window"]["used_percentage"], 63.0);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn statusline_binary_does_not_collect_when_claude_is_disabled() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    Settings {
        show_claude: false,
        ..Settings::default()
    }
    .save_to(&dir.join("settings.json"))
    .unwrap();
    let input = br#"{"session_id":"disabled","context_window":{"used_percentage":63}}"#;

    let mut child = Command::new(env!("CARGO_BIN_EXE_agentjuice-statusline"))
        .env("AGENT_JUICE_DATA_DIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!dir.join("claude_last.disabled.json").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn restore_owned_statusline_cli_uses_exit_status_without_stdout() {
    let root = unique_temp_dir();
    let home = root.join("home");
    let data_dir = root.join("data");
    let settings_path = home.join(".claude").join("settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(
        &settings_path,
        r#"{"statusLine":{"type":"command","command":"old","extra":true},"keep":true}"#,
    )
    .unwrap();
    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();

    let success = Command::new(env!("CARGO_BIN_EXE_agentjuice-statusline"))
        .arg("--restore-owned-statusline")
        .env("AGENT_JUICE_CLAUDE_HOME", &home)
        .env("AGENT_JUICE_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(success.status.success());
    assert!(success.stdout.is_empty());
    let restored: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(restored["statusLine"]["command"], "old");
    assert_eq!(restored["statusLine"]["extra"], true);

    Settings::install_statusline_wrap_at(&home, &data_dir, r"C:\Juice\agentjuice-statusline.exe")
        .unwrap();
    let mut changed: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    changed["statusLine"]["userChanged"] = serde_json::json!(true);
    fs::write(&settings_path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();

    let refused = Command::new(env!("CARGO_BIN_EXE_agentjuice-statusline"))
        .arg("--restore-owned-statusline")
        .env("AGENT_JUICE_CLAUDE_HOME", &home)
        .env("AGENT_JUICE_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    let unchanged: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(unchanged, changed);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn statusline_source_cannot_spawn_original_commands() {
    let source = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/statusline.rs"),
    )
    .unwrap();

    assert!(!source.contains("std::process::Command"));
    assert!(!source.contains("Command::new"));
    assert!(!source.contains("run_original"));
    assert!(!source.contains("wrap.json"));
    assert!(!source.contains("wrap-meta.json"));
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

#[test]
fn concurrent_statusline_forwards_leave_valid_json_without_temp_files() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let dir = std::sync::Arc::new(dir);
    let mut workers = Vec::new();
    for index in 0..16 {
        let dir = std::sync::Arc::clone(&dir);
        workers.push(std::thread::spawn(move || {
            let input = serde_json::json!({
                "session_id": "shared-session",
                "context_window": {"used_percentage": index},
                "rate_limits": null,
                "cost": null
            });
            statusline::run_with_dir(&input.to_string(), &dir);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }

    let target = dir.join("claude_last.shared-session.json");
    let value: Value = serde_json::from_slice(&fs::read(target).unwrap()).unwrap();
    assert!(value["context_window"]["used_percentage"].is_number());
    let leftovers: Vec<_> = fs::read_dir(&*dir)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".aj-tmp"))
        .collect();
    assert!(leftovers.is_empty());

    fs::remove_dir_all(&*dir).unwrap();
}
