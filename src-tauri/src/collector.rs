use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use serde_json::Value;

const TAIL_CHUNK_BYTES: u64 = 64 * 1024;
const CODEX_ACCOUNT_RESPONSE_ID: i64 = 2;
const PROCESS_CLEANUP_GRACE: Duration = Duration::from_millis(500);
const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_ERROR_BYTES: usize = 64 * 1024;
const MAX_COMMAND_LINE_BYTES: usize = 256 * 1024;
#[cfg(windows)]
const TASKKILL_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_ROLLOUT_DEPTH: usize = 4;
const MAX_ROLLOUT_CANDIDATES: usize = 16_384;
pub const MAX_ROLLOUT_TAIL_BYTES: u64 = 4 * 1024 * 1024;
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

    fn terminate(&self) -> bool {
        use windows::Win32::System::JobObjects::TerminateJobObject;
        unsafe { TerminateJobObject(self.job, 1) }.is_ok()
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

    fn terminate(&self) -> bool {
        false
    }
}

fn wait_for_exit_until(child: &mut Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

fn terminate_process_tree_until(child: &mut Child, tree: Option<&ProcessTree>, deadline: Instant) {
    let job_terminated = tree.is_some_and(ProcessTree::terminate);
    #[cfg(not(windows))]
    let _ = job_terminated;

    #[cfg(windows)]
    if !job_terminated && child.try_wait().ok().flatten().is_none() {
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
        if let Ok(mut helper) = taskkill.spawn() {
            let helper_deadline = deadline.min(Instant::now() + TASKKILL_TIMEOUT);
            if !wait_for_exit_until(&mut helper, helper_deadline) {
                let _ = helper.kill();
                let _ = wait_for_exit_until(&mut helper, deadline);
            }
        }
    }

    let _ = child.kill();
    let _ = wait_for_exit_until(child, deadline);
}

fn read_bounded_to_end(
    mut reader: impl Read,
    max_bytes: usize,
    label: &str,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    let mut overflowed = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let available = max_bytes.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(available)]);
        overflowed |= read > available;
    }
    if overflowed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} exceeded {max_bytes} bytes"),
        ));
    }
    Ok(output)
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    total_bytes: &mut usize,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&line).into_owned()))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(buffer.len(), |index| index + 1);
        if total_bytes.saturating_add(take) > MAX_COMMAND_OUTPUT_BYTES
            || line.len().saturating_add(take) > MAX_COMMAND_LINE_BYTES
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "command stdout exceeded its line or total byte limit",
            ));
        }
        line.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        *total_bytes += take;
        if newline.is_some() {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

fn is_rollout(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

fn is_link_or_reparse(entry: &std::fs::DirEntry, file_type: std::fs::FileType) -> bool {
    if file_type.is_symlink() {
        return true;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

        std::fs::symlink_metadata(entry.path())
            .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .unwrap_or(true)
    }

    #[cfg(not(windows))]
    false
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RolloutScanOutcome {
    Complete,
    Deadline,
    CandidateLimit,
    IoError,
}

fn walk(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    deadline: Option<Instant>,
) -> RolloutScanOutcome {
    if deadline_expired(deadline) {
        return RolloutScanOutcome::Deadline;
    }
    if depth > MAX_ROLLOUT_DEPTH {
        return RolloutScanOutcome::Complete;
    }

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return RolloutScanOutcome::IoError;
    };
    let mut entries = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else {
            return RolloutScanOutcome::IoError;
        };
        entries.push(entry);
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));

    for entry in entries {
        if deadline_expired(deadline) {
            return RolloutScanOutcome::Deadline;
        }
        if out.len() >= MAX_ROLLOUT_CANDIDATES {
            return RolloutScanOutcome::CandidateLimit;
        }
        let Ok(file_type) = entry.file_type() else {
            return RolloutScanOutcome::IoError;
        };
        if is_link_or_reparse(&entry, file_type) {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            let outcome = walk(&path, depth + 1, out, deadline);
            if outcome != RolloutScanOutcome::Complete {
                return outcome;
            }
        } else if file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_rollout)
        {
            out.push(path);
        }
    }
    RolloutScanOutcome::Complete
}

pub fn list_rollouts(sessions_dir: &Path) -> Vec<PathBuf> {
    list_rollouts_with_deadline(sessions_dir, None).0
}

fn list_rollouts_with_deadline(
    sessions_dir: &Path,
    deadline: Option<Instant>,
) -> (Vec<PathBuf>, RolloutScanOutcome) {
    let mut rollouts = Vec::new();
    let outcome = walk(sessions_dir, 0, &mut rollouts, deadline);
    (rollouts, outcome)
}

pub fn latest_rollout(sessions_dir: &Path) -> Option<PathBuf> {
    recent_rollouts(sessions_dir, 1).into_iter().next()
}

pub fn recent_rollouts(sessions_dir: &Path, limit: usize) -> Vec<PathBuf> {
    recent_rollouts_with_deadline(sessions_dir, limit, None).0
}

fn recent_rollouts_with_deadline(
    sessions_dir: &Path,
    limit: usize,
    deadline: Option<Instant>,
) -> (Vec<PathBuf>, RolloutScanOutcome) {
    if limit == 0 {
        return (Vec::new(), RolloutScanOutcome::Complete);
    }

    let (rollouts, outcome) = list_rollouts_with_deadline(sessions_dir, deadline);
    if outcome != RolloutScanOutcome::Complete || deadline_expired(deadline) {
        return (rollouts, outcome);
    }
    let mut candidates = Vec::with_capacity(rollouts.len());
    for path in rollouts {
        let Ok(modified) = std::fs::metadata(&path).and_then(|metadata| metadata.modified()) else {
            return (Vec::new(), RolloutScanOutcome::IoError);
        };
        candidates.push((modified, path));
    }
    let mut rollouts = candidates;
    rollouts.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let rollouts = rollouts
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect();
    (rollouts, RolloutScanOutcome::Complete)
}

#[derive(Default)]
pub struct RolloutCache {
    entry: Option<RolloutCacheEntry>,
}

struct RolloutCacheEntry {
    sessions_dir: PathBuf,
    scanned_limit: usize,
    scanned_at: Instant,
    directory_mtime: Option<SystemTime>,
    newest_candidate_mtime: Option<SystemTime>,
    candidates: Vec<PathBuf>,
}

fn modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn newest_modified(paths: &[PathBuf]) -> Option<SystemTime> {
    paths.iter().filter_map(|path| modified_at(path)).max()
}

impl RolloutCache {
    pub fn recent(
        &mut self,
        sessions_dir: &Path,
        limit: usize,
        force: bool,
        max_age: Duration,
        now: Instant,
    ) -> Vec<PathBuf> {
        self.recent_with_deadline(sessions_dir, limit, force, max_age, now, None)
    }

    pub fn recent_with_deadline(
        &mut self,
        sessions_dir: &Path,
        limit: usize,
        force: bool,
        max_age: Duration,
        now: Instant,
        deadline: Option<Instant>,
    ) -> Vec<PathBuf> {
        if limit == 0 {
            return Vec::new();
        }

        let directory_mtime = modified_at(sessions_dir);
        let should_scan = force
            || self.entry.as_ref().is_none_or(|entry| {
                entry.sessions_dir != sessions_dir
                    || entry.scanned_limit < limit
                    || now.saturating_duration_since(entry.scanned_at) >= max_age
                    || entry.directory_mtime != directory_mtime
                    || entry.newest_candidate_mtime != newest_modified(&entry.candidates)
            });

        if should_scan {
            let (candidates, outcome) =
                recent_rollouts_with_deadline(sessions_dir, limit, deadline);
            if outcome == RolloutScanOutcome::Complete {
                self.entry = Some(RolloutCacheEntry {
                    sessions_dir: sessions_dir.to_path_buf(),
                    scanned_limit: limit,
                    scanned_at: now,
                    directory_mtime,
                    newest_candidate_mtime: newest_modified(&candidates),
                    candidates,
                });
            }
        }

        self.entry
            .as_ref()
            .map(|entry| entry.candidates.iter().take(limit).cloned().collect())
            .unwrap_or_default()
    }
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
    last_token_count_line_from_file_bounded(path, None, None)
}

pub fn last_token_count_line_from_file_until(
    path: &Path,
    deadline: Instant,
    max_bytes: u64,
) -> anyhow::Result<Option<String>> {
    last_token_count_line_from_file_bounded(path, Some(deadline), Some(max_bytes))
}

fn last_token_count_line_from_file_bounded(
    path: &Path,
    deadline: Option<Instant>,
    max_bytes: Option<u64>,
) -> anyhow::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    let mut position = file.metadata()?.len();
    let mut carry = String::new();
    let mut scanned = 0u64;

    while position > 0 {
        if deadline_expired(deadline) {
            anyhow::bail!("rollout tail scan deadline exceeded");
        }
        if max_bytes.is_some_and(|max_bytes| scanned >= max_bytes) {
            return Ok(None);
        }
        let read_len = position.min(TAIL_CHUNK_BYTES);
        let read_len = max_bytes
            .map(|max_bytes| read_len.min(max_bytes.saturating_sub(scanned)))
            .unwrap_or(read_len);
        if read_len == 0 {
            return Ok(None);
        }
        position -= read_len;
        scanned = scanned.saturating_add(read_len);

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
    let deadline = Instant::now() + timeout;
    let hard_deadline = deadline + PROCESS_CLEANUP_GRACE;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(err) => {
            terminate_process_tree_until(&mut child, None, hard_deadline);
            return Err(err);
        }
    };

    let result = (|| {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stdout unavailable"))?;
        let (tx, rx) = mpsc::sync_channel(16);
        let (stdout_done_tx, stdout_done_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut total_bytes = 0;
            loop {
                match read_bounded_line(&mut reader, &mut total_bytes) {
                    Ok(Some(line)) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        let _ = tx.send(Err(err));
                        break;
                    }
                }
            }
            let _ = stdout_done_tx.send(());
        });

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stderr unavailable"))?;
        let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = read_bounded_to_end(
                BufReader::new(stderr),
                MAX_COMMAND_ERROR_BYTES,
                "codex app-server stderr",
            );
            let _ = stderr_tx.send(result);
        });

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
        terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
        drop(rx);
        let remaining = hard_deadline.saturating_duration_since(Instant::now());
        stdout_done_rx
            .recv_timeout(remaining)
            .map_err(|_| anyhow::anyhow!("codex app-server stdout did not close"))?;
        let remaining = hard_deadline.saturating_duration_since(Instant::now());
        stderr_rx
            .recv_timeout(remaining)
            .map_err(|_| anyhow::anyhow!("codex app-server stderr did not close"))??;
        outcome.unwrap_or_else(|| Err(anyhow::anyhow!("codex account API timed out")))
    })();
    terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
    result
}

pub fn claude_usage_output(timeout: Duration) -> anyhow::Result<String> {
    claude_usage_output_with_command(claude_usage_command(), timeout)
}

pub fn claude_oauth_usage_response(timeout: Duration) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
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
    let remaining = remaining_until(deadline, "Claude OAuth usage")?;
    let user_agent = claude_user_agent(remaining)?;
    let remaining = remaining_until(deadline, "Claude OAuth usage")?;
    let config = claude_oauth_curl_config(token, user_agent, remaining);
    command_output_with_input(
        claude_oauth_curl_command(),
        Some(config.as_bytes()),
        remaining,
        "Claude OAuth usage",
    )
}

fn remaining_until(deadline: Instant, label: &str) -> anyhow::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        anyhow::bail!("{label} deadline exceeded");
    }
    Ok(remaining)
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

pub(crate) fn command_output_with_input(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    label: &str,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    let hard_deadline = deadline + PROCESS_CLEANUP_GRACE;
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
            terminate_process_tree_until(&mut child, None, hard_deadline);
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

    let input_rx = if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("{label} stdin unavailable"))?;
        let input = input.to_vec();
        let (input_tx, input_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = stdin.write_all(&input).and_then(|_| stdin.flush());
            let _ = input_tx.send(result);
        });
        Some(input_rx)
    } else {
        None
    };
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = read_bounded_to_end(
            BufReader::new(stdout),
            MAX_COMMAND_OUTPUT_BYTES,
            "command stdout",
        );
        let _ = stdout_tx.send(result);
    });
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = read_bounded_to_end(
            BufReader::new(stderr),
            MAX_COMMAND_ERROR_BYTES,
            "command stderr",
        );
        let _ = stderr_tx.send(result);
    });

    let mut wait_error = None;
    let status = loop {
        if let Some(input_rx) = input_rx.as_ref() {
            if let Ok(Err(err)) = input_rx.try_recv() {
                wait_error = Some(anyhow::Error::from(err));
                break None;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => {
                wait_error = Some(anyhow::Error::from(err));
                break None;
            }
        }

        if Instant::now() >= deadline {
            break None;
        }

        std::thread::sleep(Duration::from_millis(50));
    };

    terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
    let remaining = hard_deadline.saturating_duration_since(Instant::now());
    let stdout = stdout_rx
        .recv_timeout(remaining)
        .map_err(|_| anyhow::anyhow!("{label} stdout did not close"))??;
    let remaining = hard_deadline.saturating_duration_since(Instant::now());
    let stderr = stderr_rx
        .recv_timeout(remaining)
        .map_err(|_| anyhow::anyhow!("{label} stderr did not close"))??;

    if let Some(error) = wait_error {
        return Err(error);
    }

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
    fn fake_quick_child() {
        if std::env::var_os("AGENT_JUICE_FAKE_QUICK").is_some() {
            println!("recovered");
        }
    }

    #[test]
    fn fake_oversized_output_child() {
        if std::env::var_os("AGENT_JUICE_FAKE_OVERSIZED_STDOUT").is_some() {
            std::io::stdout()
                .write_all(&vec![b'x'; MAX_COMMAND_OUTPUT_BYTES + 1])
                .unwrap();
        }
        if std::env::var_os("AGENT_JUICE_FAKE_OVERSIZED_STDERR").is_some() {
            std::io::stderr()
                .write_all(&vec![b'x'; MAX_COMMAND_ERROR_BYTES + 1])
                .unwrap();
        }
        if std::env::var_os("AGENT_JUICE_FAKE_OVERSIZED_LINE").is_some() {
            let stdin = std::io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            for _ in 0..3 {
                let mut request = String::new();
                assert!(reader.read_line(&mut request).unwrap() > 0);
            }
            std::io::stdout()
                .write_all(&vec![b'x'; MAX_COMMAND_LINE_BYTES + 1])
                .unwrap();
            std::io::stdout().flush().unwrap();
        }
    }

    #[test]
    fn fake_slow_cleanup_helper() {
        if std::env::var_os("AGENT_JUICE_FAKE_SLOW_CLEANUP").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn cleanup_helper_wait_is_bounded() {
        let mut command = test_process("collector::tests::fake_slow_cleanup_helper");
        command.env("AGENT_JUICE_FAKE_SLOW_CLEANUP", "1");
        let mut helper = command.spawn().unwrap();
        let started = Instant::now();

        assert!(!wait_for_exit_until(
            &mut helper,
            Instant::now() + Duration::from_millis(100)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));

        let _ = helper.kill();
        assert!(wait_for_exit_until(
            &mut helper,
            Instant::now() + Duration::from_secs(1)
        ));
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
    fn command_output_rejects_oversized_stdout_and_stderr() {
        for variable in [
            "AGENT_JUICE_FAKE_OVERSIZED_STDOUT",
            "AGENT_JUICE_FAKE_OVERSIZED_STDERR",
        ] {
            let mut command = test_process("collector::tests::fake_oversized_output_child");
            command.env(variable, "1");
            let started = Instant::now();
            assert!(command_output_with_input(
                command,
                None,
                Duration::from_secs(2),
                "oversized fixture",
            )
            .is_err());
            assert!(started.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn app_server_rejects_an_oversized_line_and_next_command_recovers() {
        let mut command = test_process("collector::tests::fake_oversized_output_child");
        command.env("AGENT_JUICE_FAKE_OVERSIZED_LINE", "1");
        let started = Instant::now();
        assert!(
            codex_account_rate_limits_response_with_command(command, Duration::from_secs(2))
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(3));

        let mut next = test_process("collector::tests::fake_quick_child");
        next.env("AGENT_JUICE_FAKE_QUICK", "1");
        let recovered =
            command_output_with_input(next, None, Duration::from_secs(1), "recovery fixture")
                .unwrap();
        assert!(recovered.contains("recovered"));
    }

    #[test]
    #[ignore = "requires a locally installed and logged-in Codex CLI"]
    fn live_codex_app_server_round_trip() {
        let response = codex_account_rate_limits_response(Duration::from_secs(5)).unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        let status = crate::adapters::codex::parse_account_rate_limits_response(
            &response,
            "LIVE",
            "2026-07-13T00:00:00Z",
        )
        .unwrap();

        assert_eq!(value.get("id").and_then(Value::as_i64), Some(2));
        assert!(value.pointer("/result/rateLimits").is_some());
        assert!(status.primary.is_some() || status.secondary.is_some());
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

        let started = Instant::now();
        let result = claude_usage_output_with_command(command, Duration::from_millis(200));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut next = test_process("collector::tests::fake_quick_child");
        next.env("AGENT_JUICE_FAKE_QUICK", "1");
        let recovered = claude_usage_output_with_command(next, Duration::from_secs(1)).unwrap();
        assert!(recovered.contains("recovered"));

        std::thread::sleep(Duration::from_millis(1_400));
        assert!(!marker.exists(), "descendant survived the timeout");

        let _ = std::fs::remove_dir_all(root);
    }
}
