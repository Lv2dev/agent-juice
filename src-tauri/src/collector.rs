use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};

use serde_json::Value;

const TAIL_CHUNK_BYTES: u64 = 64 * 1024;
const CODEX_ACCOUNT_RESPONSE_ID: i64 = 2;
static CLAUDE_USER_AGENT: OnceLock<String> = OnceLock::new();

#[cfg(windows)]
struct ProcessTree {
    job: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &Child) -> anyhow::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::{
            core::PCWSTR,
            Win32::{
                Foundation::HANDLE,
                System::JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
            },
        };

        let job = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
        let tree = Self { job };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                tree.job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )?;
            AssignProcessToJobObject(tree.job, HANDLE(child.as_raw_handle()))?;
        }
        Ok(tree)
    }

    fn terminate(&self) {
        use windows::Win32::System::JobObjects::TerminateJobObject;
        let _ = unsafe { TerminateJobObject(self.job, 1) };
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.job) };
    }
}

#[cfg(not(windows))]
struct ProcessTree;

#[cfg(not(windows))]
impl ProcessTree {
    fn attach(_child: &Child) -> anyhow::Result<Self> {
        Ok(Self)
    }

    fn terminate(&self) {}
}

fn terminate_process_tree(child: &mut Child, tree: &ProcessTree) {
    #[cfg(windows)]
    if child.try_wait().ok().flatten().is_none() {
        use std::os::windows::process::CommandExt;

        let taskkill_path = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("taskkill.exe");
        let mut taskkill = Command::new(taskkill_path);
        taskkill
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000);
        let _ = taskkill.status();
    }

    tree.terminate();
    let _ = child.kill();
    let _ = child.wait();
}

fn is_rollout(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_rollout)
            {
                out.push(path);
            }
        }
    }
}

pub fn list_rollouts(sessions_dir: &Path) -> Vec<PathBuf> {
    let mut rollouts = Vec::new();
    walk(sessions_dir, &mut rollouts);
    rollouts
}

pub fn latest_rollout(sessions_dir: &Path) -> Option<PathBuf> {
    recent_rollouts(sessions_dir, 1).into_iter().next()
}

pub fn recent_rollouts(sessions_dir: &Path, limit: usize) -> Vec<PathBuf> {
    if limit == 0 {
        return Vec::new();
    }

    let mut rollouts: Vec<_> = list_rollouts(sessions_dir)
        .into_iter()
        .filter_map(|path| {
            std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(|modified| (modified, path))
        })
        .collect();
    rollouts.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    rollouts
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect()
}

pub fn session_id_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_default()
}

pub fn last_token_count_line(contents: &str) -> Option<String> {
    contents
        .lines()
        .rev()
        .find(|line| is_token_count_event(line))
        .map(str::to_string)
}

pub fn last_token_count_line_from_file(path: &Path) -> anyhow::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    let mut position = file.metadata()?.len();
    let mut carry = String::new();

    while position > 0 {
        let read_len = position.min(TAIL_CHUNK_BYTES);
        position -= read_len;

        file.seek(SeekFrom::Start(position))?;
        let mut buffer = vec![0; read_len as usize];
        file.read_exact(&mut buffer)?;

        let mut combined = String::from_utf8_lossy(&buffer).into_owned();
        combined.push_str(&carry);

        let mut lines: Vec<&str> = combined.lines().collect();
        if position > 0 {
            carry = lines.first().copied().unwrap_or_default().to_string();
            if !lines.is_empty() {
                lines.remove(0);
            }
        }

        if let Some(line) = lines.iter().rev().find(|line| is_token_count_event(line)) {
            return Ok(Some((*line).to_string()));
        }
    }

    Ok(None)
}

fn is_token_count_event(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };

    value.get("type").and_then(Value::as_str) == Some("event_msg")
        && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
}

pub fn codex_account_rate_limits_response(timeout: Duration) -> anyhow::Result<String> {
    codex_account_rate_limits_response_with_command(codex_app_server_command(), timeout)
}

fn codex_account_rate_limits_response_with_command(
    mut command: Command,
    timeout: Duration,
) -> anyhow::Result<String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };

    let result = (|| {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stdout unavailable"))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut sink = Vec::new();
                let _ = reader.read_to_end(&mut sink);
            });
        }

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stdin unavailable"))?;
        let requests = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "agent-juice",
                        "title": "Juice",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": CODEX_ACCOUNT_RESPONSE_ID,
                "method": "account/rateLimits/read",
                "params": {}
            }),
        ];
        for request in requests {
            writeln!(stdin, "{}", request)?;
        }
        stdin.flush()?;

        let deadline = Instant::now() + timeout;
        let mut outcome = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait_for = remaining.min(Duration::from_millis(100));
            match rx.recv_timeout(wait_for) {
                Ok(Ok(line)) => {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if value.get("id").and_then(Value::as_i64) != Some(CODEX_ACCOUNT_RESPONSE_ID) {
                        continue;
                    }
                    outcome = Some(if value.get("error").is_some() {
                        Err(anyhow::anyhow!("codex account API returned an error"))
                    } else {
                        Ok(line)
                    });
                    break;
                }
                Ok(Err(err)) => {
                    outcome = Some(Err(anyhow::anyhow!(err)));
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        drop(stdin);
        outcome.unwrap_or_else(|| Err(anyhow::anyhow!("codex account API timed out")))
    })();

    terminate_process_tree(&mut child, &tree);
    result
}

pub fn claude_usage_output(timeout: Duration) -> anyhow::Result<String> {
    claude_usage_output_with_command(claude_usage_command(), timeout)
}

pub fn claude_oauth_usage_response(timeout: Duration) -> anyhow::Result<String> {
    let credentials_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".claude")
        .join(".credentials.json");
    let credentials: Value = serde_json::from_str(&std::fs::read_to_string(credentials_path)?)?;
    let token = credentials
        .pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        .ok_or_else(|| anyhow::anyhow!("Claude OAuth access token unavailable"))?;
    let user_agent = claude_user_agent(timeout)?;
    let config = claude_oauth_curl_config(token, user_agent, timeout);
    command_output_with_input(
        claude_oauth_curl_command(),
        Some(config.as_bytes()),
        timeout,
        "Claude OAuth usage",
    )
}

fn claude_user_agent(timeout: Duration) -> anyhow::Result<&'static str> {
    if let Some(user_agent) = CLAUDE_USER_AGENT.get() {
        return Ok(user_agent);
    }

    let output =
        command_output_with_input(claude_version_command(), None, timeout, "Claude version")?;
    let version = output
        .split_whitespace()
        .next()
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-'))
        })
        .ok_or_else(|| anyhow::anyhow!("Claude version output was not recognized"))?;
    let _ = CLAUDE_USER_AGENT.set(format!("claude-code/{version}"));
    CLAUDE_USER_AGENT
        .get()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("Claude user agent unavailable"))
}

fn claude_oauth_curl_config(token: &str, user_agent: &str, timeout: Duration) -> String {
    format!(
        "silent\nshow-error\nfail-with-body\nmax-time = \"{}\"\nurl = \"https://api.anthropic.com/api/oauth/usage\"\nheader = \"Authorization: Bearer {}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Content-Type: application/json\"\nuser-agent = \"{}\"\nheader = \"x-app: cli\"\n",
        timeout.as_secs().max(1),
        token,
        user_agent,
    )
}

fn claude_usage_output_with_command(command: Command, timeout: Duration) -> anyhow::Result<String> {
    command_output_with_input(command, None, timeout, "Claude usage")
}

fn command_output_with_input(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    label: &str,
) -> anyhow::Result<String> {
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("{label} stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("{label} stderr unavailable"))?;

    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("{label} stdin unavailable"))?;
        stdin.write_all(input)?;
        stdin.flush()?;
    }
    let (stdout_tx, stdout_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = BufReader::new(stdout)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = stdout_tx.send(result);
    });
    let (stderr_tx, stderr_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = BufReader::new(stderr)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = stderr_tx.send(result);
    });
    let deadline = Instant::now() + timeout;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }

        if Instant::now() >= deadline {
            break None;
        }

        std::thread::sleep(Duration::from_millis(50));
    };

    if status.is_none() {
        terminate_process_tree(&mut child, &tree);
    }
    let stdout = stdout_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| anyhow::anyhow!("Claude usage stdout did not close"))??;
    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| anyhow::anyhow!("Claude usage stderr did not close"))??;

    let Some(status) = status else {
        anyhow::bail!("{label} command timed out");
    };
    if status.success() {
        return Ok(String::from_utf8_lossy(&stdout).into_owned());
    }

    let message = String::from_utf8_lossy(&stderr);
    anyhow::bail!("{label} command failed: {}", message.trim());
}

fn codex_app_server_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new("cmd");
        command.args(["/C", "codex", "app-server", "--listen", "stdio://"]);
        command.creation_flags(0x08000000);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("codex");
        command.args(["app-server", "--listen", "stdio://"]);
        command
    }
}

fn claude_usage_command() -> Command {
    const ARGS: [&str; 10] = [
        "-p",
        "/usage",
        "--max-budget-usd",
        "0.000001",
        "--no-session-persistence",
        "--permission-mode",
        "dontAsk",
        "--output-format",
        "json",
        "--tools",
    ];

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new("cmd");
        command.args(["/C", "claude"]);
        command.args(ARGS);
        command.arg("");
        command.creation_flags(0x08000000);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("claude");
        command.args(ARGS);
        command.arg("");
        command
    }
}

fn claude_oauth_curl_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let curl_path = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("curl.exe");
        let mut command = Command::new(curl_path);
        command.args(["-q", "--config", "-"]);
        command.creation_flags(0x08000000);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("curl");
        command.args(["-q", "--config", "-"]);
        command
    }
}

fn claude_version_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new("cmd");
        command.args(["/C", "claude", "--version"]);
        command.creation_flags(0x08000000);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("claude");
        command.arg("--version");
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::BufRead,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    fn test_process(exact_test: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", exact_test, "--nocapture"]);
        command
    }

    #[test]
    fn fake_stdin_reader_child() {
        if std::env::var_os("AGENT_JUICE_FAKE_STDIN_READER").is_none() {
            return;
        }

        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        println!("AJ_STDIN_LEN:{}", input.len());
    }

    #[test]
    fn oauth_secret_is_sent_over_stdin_instead_of_process_arguments() {
        let token = "fixture-secret";
        let config = claude_oauth_curl_config(token, "claude-code/2.1.205", Duration::from_secs(5));
        let command_line = format!("{:?}", claude_oauth_curl_command());
        assert!(!command_line.contains(token));

        let mut command = test_process("collector::tests::fake_stdin_reader_child");
        command.env("AGENT_JUICE_FAKE_STDIN_READER", "1");
        let output = command_output_with_input(
            command,
            Some(config.as_bytes()),
            Duration::from_secs(2),
            "test stdin",
        )
        .unwrap();
        let length = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("AJ_STDIN_LEN:"))
            .and_then(|value| value.parse::<usize>().ok());
        assert_eq!(length, Some(config.len()));
    }

    #[test]
    #[ignore = "requires a locally installed and logged-in Claude Code"]
    fn live_claude_oauth_usage_round_trip() {
        let response = claude_oauth_usage_response(Duration::from_secs(5)).unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert!(value.pointer("/five_hour/utilization").is_some());
        assert!(value.pointer("/seven_day/utilization").is_some());
    }

    #[test]
    fn fake_app_server_child() {
        if std::env::var_os("AGENT_JUICE_FAKE_APP_SERVER").is_none() {
            return;
        }

        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        for _ in 0..3 {
            let mut request = String::new();
            assert!(reader.read_line(&mut request).unwrap() > 0);
        }
        drop(reader);

        let eof_seen = Arc::new(AtomicBool::new(false));
        let eof_from_reader = Arc::clone(&eof_seen);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            let mut next = String::new();
            if reader.read_line(&mut next).unwrap_or_default() == 0 {
                eof_from_reader.store(true, Ordering::SeqCst);
            }
        });

        std::thread::sleep(Duration::from_millis(200));
        if !eof_seen.load(Ordering::SeqCst) {
            println!(
                "{}",
                serde_json::json!({
                    "id": CODEX_ACCOUNT_RESPONSE_ID,
                    "result": {
                        "rateLimits": {
                            "primary": {"usedPercent": 12, "windowDurationMins": 300},
                            "secondary": {"usedPercent": 34, "windowDurationMins": 10080}
                        }
                    }
                })
            );
            std::io::stdout().flush().unwrap();
        }
        std::process::exit(0);
    }

    #[test]
    fn codex_app_server_keeps_stdin_open_until_the_response_arrives() {
        let mut command = test_process("collector::tests::fake_app_server_child");
        command.env("AGENT_JUICE_FAKE_APP_SERVER", "1");

        let response =
            codex_account_rate_limits_response_with_command(command, Duration::from_secs(2))
                .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(value.get("id").and_then(Value::as_i64), Some(2));
    }

    #[test]
    #[ignore = "requires a locally installed and logged-in Codex CLI"]
    fn live_codex_app_server_round_trip() {
        let response = codex_account_rate_limits_response(Duration::from_secs(5)).unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(value.get("id").and_then(Value::as_i64), Some(2));
        assert!(value.pointer("/result/rateLimits").is_some());
    }

    #[cfg(windows)]
    #[test]
    fn process_tree_parent_child() {
        if std::env::var_os("AGENT_JUICE_PROCESS_TREE_PARENT").is_none() {
            return;
        }

        let marker = std::env::var("AGENT_JUICE_PROCESS_TREE_MARKER").unwrap();
        let mut command = test_process("collector::tests::process_tree_delayed_marker_child");
        command.env("AGENT_JUICE_PROCESS_TREE_MARKER", marker);
        command.env("AGENT_JUICE_PROCESS_TREE_DELAYED", "1");
        let mut child = command.spawn().unwrap();
        child.wait().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn process_tree_delayed_marker_child() {
        if std::env::var_os("AGENT_JUICE_PROCESS_TREE_DELAYED").is_none() {
            return;
        }

        std::thread::sleep(Duration::from_millis(1_200));
        let marker = std::env::var("AGENT_JUICE_PROCESS_TREE_MARKER").unwrap();
        std::fs::write(marker, b"survived").unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn timeout_terminates_the_entire_process_tree() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-process-tree-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("descendant-survived.txt");
        let mut command = test_process("collector::tests::process_tree_parent_child");
        command.env("AGENT_JUICE_PROCESS_TREE_PARENT", "1");
        command.env("AGENT_JUICE_PROCESS_TREE_MARKER", &marker);

        let result = claude_usage_output_with_command(command, Duration::from_millis(200));
        assert!(result.is_err());
        std::thread::sleep(Duration::from_millis(1_400));
        assert!(!marker.exists(), "descendant survived the timeout");

        let _ = std::fs::remove_dir_all(root);
    }
}
