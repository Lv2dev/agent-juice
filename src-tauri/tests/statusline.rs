use agent_juice::{config::Settings, statusline};
use serde_json::Value;
#[cfg(windows)]
use std::time::{Duration, Instant};
use std::{
    fs,
    process::Command,
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

#[cfg(windows)]
#[test]
fn original_command_timeout_returns_fallback_and_terminates_process_tree() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let command_path = dir.join("slow-tree.cmd");
    let marker_path = dir.join("leaked-child.txt");
    let command = format!(
        "@echo off\r\nstart \"\" /b cmd.exe /D /C \"ping.exe 127.0.0.1 -n 5 >nul & echo leaked>{}\"\r\nping.exe 127.0.0.1 -n 30 >nul\r\n",
        marker_path.display()
    );
    fs::write(&command_path, command).unwrap();
    let original = command_path.to_string_lossy().to_string();
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
    let input = r#"{"session_id":"timeout","context_window":{"used_percentage":63}}"#;

    let started = Instant::now();
    let output = statusline::run_with_dir(input, &dir);
    let elapsed = started.elapsed();

    assert_eq!(String::from_utf8(output).unwrap(), "ctx 63%\n");
    assert!(elapsed >= Duration::from_millis(1_700));
    assert!(elapsed < Duration::from_secs(5));

    let recovered = "echo recovered";
    fs::write(dir.join("wrap.json"), recovered).unwrap();
    fs::write(
        dir.join("wrap-meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "managed_command": "fixture-managed",
            "original_command": recovered,
        }))
        .unwrap(),
    )
    .unwrap();
    let next_started = Instant::now();
    let next = statusline::run_with_dir(input, &dir);
    assert!(next_started.elapsed() < Duration::from_secs(1));
    assert_eq!(String::from_utf8(next).unwrap().trim(), "recovered");

    std::thread::sleep(Duration::from_secs(4));
    assert!(!marker_path.exists());

    fs::remove_dir_all(dir).unwrap();
}

#[cfg(windows)]
#[test]
fn original_command_timeout_includes_a_blocked_large_stdin_write() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let command_path = dir.join("no-stdin.cmd");
    fs::write(
        &command_path,
        "@echo off\r\nping.exe 127.0.0.1 -n 30 >nul\r\n",
    )
    .unwrap();
    let original = command_path.to_string_lossy().to_string();
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

    let input = "x".repeat(512 * 1024);
    let started = Instant::now();
    let output = statusline::run_with_dir(&input, &dir);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(String::from_utf8(output).unwrap(), "agent-juice\n");

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
