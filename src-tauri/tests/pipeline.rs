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
    let grok = status(Tool::Grok, "grok", "2026-07-07T00:02:00Z");
    let cursor = status(Tool::Cursor, "cursor", "2026-07-07T00:04:00Z");
    let antigravity = status(Tool::Antigravity, "antigravity", "2026-07-07T00:05:00Z");

    let mut reps = latest_per_tool(&[older_claude, newer_claude, codex, grok, cursor, antigravity]);
    reps.sort_by(|a, b| format!("{:?}", a.tool).cmp(&format!("{:?}", b.tool)));

    assert_eq!(reps.len(), 5);
    assert!(reps.iter().any(|status| status.session_id == "new"));
    assert!(reps.iter().any(|status| status.session_id == "codex"));
    assert!(reps.iter().any(|status| status.session_id == "grok"));
    assert!(reps.iter().any(|status| status.session_id == "cursor"));
    assert!(reps.iter().any(|status| status.session_id == "antigravity"));
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

#[test]
fn nsis_preinstall_checks_running_app_before_quarantining_statusline() {
    let hooks = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows/hooks.nsh"),
    )
    .unwrap();
    let preinstall = hooks
        .split("!macro NSIS_HOOK_PREINSTALL")
        .nth(1)
        .unwrap()
        .split("!macroend")
        .next()
        .unwrap();
    let check = preinstall
        .find("!insertmacro CheckIfAppIsRunning")
        .expect("use Tauri's running-app check before touching the bridge");
    assert!(check < preinstall.find("Delete ").unwrap());
    assert!(check < preinstall.find("Rename ").unwrap());

    let failure = hooks
        .split("Function .onInstFailed")
        .nth(1)
        .unwrap()
        .split("FunctionEnd")
        .next()
        .unwrap();
    let restore = failure.find("kernel32::MoveFileExW").unwrap();
    assert!(
        failure
            .find("StrCmp $JuiceStatuslineQuarantined 1")
            .unwrap()
            < restore
    );
    assert!(
        failure
            .find("Call JuiceStatuslineMatchesUpdatePayload")
            .unwrap()
            < restore
    );
    assert!(failure
        .find("IfFileExists \"$INSTDIR\\agentjuice-statusline.juice-update-old.exe\" 0 juice_statusline_restore_done")
        .unwrap()
        < restore);
    assert!(!failure.contains("Delete "));
    assert!(
        preinstall
            .find("juice-statusline-update-reference.exe")
            .unwrap()
            < preinstall.find("Rename ").unwrap()
    );
}

#[cfg(windows)]
fn compile_nsis_fixture(
    compiler: &std::path::Path,
    script: &str,
    directory: &std::path::Path,
    trace: bool,
) -> std::process::Output {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut command = Command::new(compiler);
    command.args(["/INPUTCHARSET", "UTF8", "/NOCD", "/V2"]);
    if trace {
        command.arg("/V4");
    }
    let mut child = command
        .arg("-")
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "NSIS compile failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[cfg(windows)]
fn nsis_fixture_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .replace('$', "$$")
        .replace('"', "$\\\"")
}

#[cfg(windows)]
#[test]
#[ignore = "requires JUICE_NSIS_COMPILER; executes only isolated file-operation fixtures"]
fn nsis_cancel_fixture_preserves_canonical_statusline() {
    use std::os::windows::process::CommandExt;

    let compiler = std::path::PathBuf::from(std::env::var_os("JUICE_NSIS_COMPILER").unwrap());
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let hooks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows/hooks.nsh");
    let payload_dir = root.join("payload");
    fs::create_dir_all(&payload_dir).unwrap();
    let mut new_payload = vec![0x5a; 65536 + 129];
    new_payload[..2].copy_from_slice(b"MZ");
    new_payload[65536] = 0x7f;
    let mut corrupt_payload = new_payload.clone();
    *corrupt_payload.last_mut().unwrap() ^= 1;
    let payload = payload_dir.join("agentjuice-statusline.exe");
    fs::write(&payload, &new_payload).unwrap();
    for (name, cancel_on, replacement, remove_backup, succeed, remove_reference) in [
        ("cancel-before-quarantine", 1, None, false, false, false),
        ("cancel-after-quarantine", 2, None, false, false, false),
        (
            "failure-after-new-bridge",
            0,
            Some(new_payload.as_slice()),
            false,
            false,
            false,
        ),
        (
            "successful-install",
            0,
            Some(new_payload.as_slice()),
            false,
            true,
            false,
        ),
        (
            "failure-after-zero-byte-bridge",
            0,
            Some(&[][..]),
            false,
            false,
            false,
        ),
        (
            "failure-after-truncated-mz-bridge",
            0,
            Some(&new_payload[..128]),
            false,
            false,
            false,
        ),
        (
            "failure-after-same-size-corrupt-bridge",
            0,
            Some(corrupt_payload.as_slice()),
            false,
            false,
            false,
        ),
        (
            "missing-backup-zero-byte-bridge",
            0,
            Some(&[][..]),
            true,
            false,
            false,
        ),
        (
            "missing-backup-valid-new-bridge",
            0,
            Some(new_payload.as_slice()),
            true,
            false,
            false,
        ),
        ("missing-backup-missing-bridge", 0, None, true, false, false),
        (
            "unreadable-reference-zero-byte-bridge",
            0,
            Some(&[][..]),
            false,
            false,
            true,
        ),
    ] {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        let canonical = directory.join("agentjuice-statusline.exe");
        let quarantine = directory.join("agentjuice-statusline.juice-update-old.exe");
        let executable = directory.join("fixture.exe");
        fs::write(&canonical, b"old-bridge-fixture").unwrap();
        let mut finish = String::new();
        if let Some(bytes) = replacement {
            let extracted = directory.join("extracted-fixture.bin");
            fs::write(&extracted, bytes).unwrap();
            finish.push_str(&format!(
                "File \"/oname=agentjuice-statusline.exe\" \"{}\"\n",
                nsis_fixture_path(&extracted)
            ));
        }
        if remove_backup {
            finish.push_str("Delete \"$INSTDIR\\agentjuice-statusline.juice-update-old.exe\"\n");
        }
        if remove_reference {
            finish.push_str("Delete \"$PLUGINSDIR\\juice-statusline-update-reference.exe\"\n");
        }
        if succeed {
            finish.push_str("!insertmacro NSIS_HOOK_POSTINSTALL");
        } else if cancel_on == 0 {
            finish.push_str("Abort \"fixture extraction failure\"");
        }
        // The real hook runs unchanged; the app check never finds or kills any process.
        let script = format!(
            r#"Unicode true
Name "Juice cancellation fixture"
OutFile "{}"
InstallDir "{}"
RequestExecutionLevel user
SilentInstall silent
AutoCloseWindow true
!define MAINBINARYNAME "fixture-not-an-app"
!define MAINBINARYSRCPATH "{}"
!define PRODUCTNAME "Juice cancellation fixture"
Var FixtureCheckCount
!macro CheckIfAppIsRunning executableName productName
  Call FixtureCheckRunningApp
!macroend
!include "{}"
Function FixtureCheckRunningApp
  IntOp $FixtureCheckCount $FixtureCheckCount + 1
  IntCmp $FixtureCheckCount {cancel_on} fixture_cancel fixture_check_done fixture_check_done
  fixture_cancel:
    Abort "fixture cancellation"
  fixture_check_done:
FunctionEnd
Section
  SetOutPath $INSTDIR
  !insertmacro NSIS_HOOK_PREINSTALL
  !insertmacro CheckIfAppIsRunning "${{MAINBINARYNAME}}.exe" "${{PRODUCTNAME}}"
  {finish}
SectionEnd
"#,
            nsis_fixture_path(&executable),
            nsis_fixture_path(&directory),
            nsis_fixture_path(&payload_dir.join("fixture-not-an-app.exe")),
            nsis_fixture_path(&hooks)
        );
        compile_nsis_fixture(&compiler, &script, &directory, false);
        let status = std::process::Command::new(&executable)
            .arg("/S")
            .creation_flags(0x08000000)
            .status()
            .unwrap();
        assert_eq!(status.success(), succeed, "{name}");
        let valid_new = replacement == Some(new_payload.as_slice());
        let expected = if valid_new || remove_backup || remove_reference || succeed {
            replacement
        } else {
            Some(b"old-bridge-fixture".as_slice())
        };
        assert_eq!(fs::read(&canonical).ok().as_deref(), expected, "{name}");
        let expected_backup = !remove_backup && !succeed && (valid_new || remove_reference);
        assert_eq!(quarantine.exists(), expected_backup, "{name}");
        if expected_backup {
            assert_eq!(
                fs::read(&quarantine).unwrap(),
                b"old-bridge-fixture",
                "{name}"
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
#[ignore = "requires JUICE_NSIS_COMPILER and JUICE_NSIS_GENERATED_SCRIPT; build only, no install"]
fn nsis_generated_installer_checks_before_quarantine_and_compiles() {
    let compiler = std::path::PathBuf::from(std::env::var_os("JUICE_NSIS_COMPILER").unwrap());
    let generated =
        std::path::PathBuf::from(std::env::var_os("JUICE_NSIS_GENERATED_SCRIPT").unwrap());
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let script = fs::read_to_string(&generated).unwrap();
    assert!(script.contains("!define OUTFILE \"nsis-output.exe\""));
    let script = script.replace(
        "!define OUTFILE \"nsis-output.exe\"",
        &format!(
            "!define OUTFILE \"{}\"",
            nsis_fixture_path(&root.join("bundle.exe"))
        ),
    );
    let output = compile_nsis_fixture(&compiler, &script, generated.parent().unwrap(), true);
    let expanded = String::from_utf8(output.stdout).unwrap();
    let install = expanded
        .split("Section: \"Install\"")
        .nth(1)
        .unwrap()
        .split("SectionEnd")
        .next()
        .unwrap();
    let checks: Vec<_> = install
        .match_indices("Plugin command: FindProcessCurrentUser agent-juice.exe")
        .collect();
    assert_eq!(
        checks.len(),
        2,
        "the hook and Tauri each check the running app"
    );
    let quarantine = install
        .find("Rename: $INSTDIR\\agentjuice-statusline.exe")
        .unwrap();
    assert!(checks[0].0 < quarantine && quarantine < checks[1].0);
    assert!(root.join("bundle.exe").is_file());
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
