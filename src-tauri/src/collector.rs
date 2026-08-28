use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime},
};

use crate::http_transport::{self, HttpErrorKind, HttpMethod};
use serde_json::Value;

const TAIL_CHUNK_BYTES: u64 = 64 * 1024;
#[cfg(test)]
const CODEX_ACCOUNT_RESPONSE_ID: i64 = 2;
#[cfg(test)]
const CODEX_USAGE_RESPONSE_ID: i64 = 3;
#[cfg(test)]
const GROK_BILLING_RESPONSE_ID: i64 = 2;
const PROCESS_CLEANUP_GRACE: Duration = Duration::from_millis(500);
const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_ERROR_BYTES: usize = 64 * 1024;
const MAX_COMMAND_LINE_BYTES: usize = 256 * 1024;
const CLAUDE_OAUTH_RESPONSE_CAP: usize = 256 * 1024;
const CLAUDE_OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_BROKER_QUEUE_CAPACITY: usize = 8;
const CODEX_BROKER_STDOUT_CAPACITY: usize = 32;
const CODEX_BROKER_MAX_SKIPPED_MESSAGES: usize = 64;
const CODEX_BROKER_IDLE_POLL: Duration = Duration::from_millis(250);
const CODEX_BROKER_MIN_BACKOFF: Duration = Duration::from_secs(30);
const CODEX_BROKER_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
const CODEX_BROKER_CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const TASKKILL_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_ROLLOUT_DEPTH: usize = 4;
const MAX_ROLLOUT_CANDIDATES: usize = 16_384;
const MAX_ROLLOUT_ENTRIES: usize = 65_536;
#[cfg(windows)]
const MAX_CODEX_DESKTOP_BIN_ENTRIES: usize = 64;
#[cfg(windows)]
const CODEX_RUNTIME_DIRECTORY_NAME_LEN: usize = 16;
pub const MAX_ROLLOUT_TAIL_BYTES: u64 = 4 * 1024 * 1024;
static CLAUDE_USER_AGENT: OnceLock<String> = OnceLock::new();
// One worker owns the Codex child and serializes every account RPC.
static CODEX_APP_SERVER_BROKER: OnceLock<Result<CodexAppServerBroker, String>> = OnceLock::new();
// Grok uses the same bounded lifecycle engine with its own ACP handshake and child.
static GROK_ACP_BROKER: OnceLock<Result<CodexAppServerBroker, String>> = OnceLock::new();

pub fn text_requires_login(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    [
        "invalid_grant",
        "refreshtokenrejected",
        "refresh token rejected",
        "authentication required",
        "login required",
        "not authenticated",
        "not logged in",
        "please log in",
        "please login",
        "sign in required",
        "no auth credentials",
        "credentials unavailable",
        "oauth access token unavailable",
        "http 401",
        "status 401",
        "error: 401",
        "401 unauthorized",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

pub fn error_requires_login(error: &anyhow::Error) -> bool {
    text_requires_login(&format!("{error:#}"))
}

#[cfg(windows)]
struct ProcessTree {
    job: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessTree {
    fn create() -> anyhow::Result<Self> {
        use windows::{
            core::PCWSTR,
            Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
        }
        Ok(tree)
    }

    fn assign(&self, child: &Child) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::AssignProcessToJobObject};

        unsafe { AssignProcessToJobObject(self.job, HANDLE(child.as_raw_handle()))? };
        Ok(())
    }

    fn resume(child: &Child) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;

        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtResumeProcess(process_handle: *mut std::ffi::c_void) -> i32;
        }

        let status = unsafe { NtResumeProcess(child.as_raw_handle()) };
        if status < 0 {
            anyhow::bail!("NtResumeProcess failed with NTSTATUS 0x{status:08x}");
        }
        Ok(())
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
    fn create() -> anyhow::Result<Self> {
        Ok(Self)
    }

    fn assign(&self, _child: &Child) -> anyhow::Result<()> {
        Ok(())
    }

    fn resume(_child: &Child) -> anyhow::Result<()> {
        Ok(())
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

fn spawn_in_process_tree(
    command: &mut Command,
    cleanup_deadline: Instant,
) -> anyhow::Result<(Child, ProcessTree)> {
    spawn_in_process_tree_with_hook(command, cleanup_deadline, |_| Ok(()))
}

fn spawn_in_process_tree_with_hook(
    command: &mut Command,
    cleanup_deadline: Instant,
    after_suspended_spawn: impl FnOnce(&Child) -> anyhow::Result<()>,
) -> anyhow::Result<(Child, ProcessTree)> {
    let tree = ProcessTree::create()?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

        command.creation_flags(CREATE_NO_WINDOW.0 | CREATE_SUSPENDED.0);
    }

    let mut child = command.spawn()?;
    if let Err(error) = after_suspended_spawn(&child)
        .and_then(|()| tree.assign(&child))
        .and_then(|()| ProcessTree::resume(&child))
    {
        terminate_process_tree_until(&mut child, Some(&tree), cleanup_deadline);
        return Err(error);
    }
    Ok((child, tree))
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
    EntryLimit,
    CandidateLimit,
    IoError,
}

fn walk(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    deadline: Option<Instant>,
    entries_remaining: &mut usize,
) -> RolloutScanOutcome {
    if deadline_expired(deadline) {
        return RolloutScanOutcome::Deadline;
    }
    if depth > MAX_ROLLOUT_DEPTH {
        return RolloutScanOutcome::Complete;
    }

    let Ok(mut read_dir) = std::fs::read_dir(dir) else {
        return RolloutScanOutcome::IoError;
    };
    loop {
        if deadline_expired(deadline) {
            return RolloutScanOutcome::Deadline;
        }
        if *entries_remaining == 0 {
            return RolloutScanOutcome::EntryLimit;
        }
        let Some(entry) = read_dir.next() else {
            return RolloutScanOutcome::Complete;
        };
        *entries_remaining -= 1;
        if deadline_expired(deadline) {
            return RolloutScanOutcome::Deadline;
        }
        let Ok(entry) = entry else {
            return RolloutScanOutcome::IoError;
        };
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
            let outcome = walk(&path, depth + 1, out, deadline, entries_remaining);
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
}

pub fn list_rollouts(sessions_dir: &Path) -> Vec<PathBuf> {
    list_rollouts_with_deadline(sessions_dir, None).0
}

fn list_rollouts_with_deadline(
    sessions_dir: &Path,
    deadline: Option<Instant>,
) -> (Vec<PathBuf>, RolloutScanOutcome) {
    list_rollouts_with_entry_budget(sessions_dir, deadline, MAX_ROLLOUT_ENTRIES)
}

fn list_rollouts_with_entry_budget(
    sessions_dir: &Path,
    deadline: Option<Instant>,
    entry_budget: usize,
) -> (Vec<PathBuf>, RolloutScanOutcome) {
    let mut rollouts = Vec::new();
    let mut entries_remaining = entry_budget;
    let outcome = walk(
        sessions_dir,
        0,
        &mut rollouts,
        deadline,
        &mut entries_remaining,
    );
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
    recent_rollouts_with_entry_budget(sessions_dir, limit, deadline, MAX_ROLLOUT_ENTRIES)
}

fn recent_rollouts_with_entry_budget(
    sessions_dir: &Path,
    limit: usize,
    deadline: Option<Instant>,
    entry_budget: usize,
) -> (Vec<PathBuf>, RolloutScanOutcome) {
    if limit == 0 {
        return (Vec::new(), RolloutScanOutcome::Complete);
    }

    let (rollouts, outcome) = list_rollouts_with_entry_budget(sessions_dir, deadline, entry_budget);
    if outcome != RolloutScanOutcome::Complete || deadline_expired(deadline) {
        return (rollouts, outcome);
    }
    let mut candidates = Vec::with_capacity(rollouts.len());
    for path in rollouts {
        if deadline_expired(deadline) {
            return (Vec::new(), RolloutScanOutcome::Deadline);
        }
        let Ok(modified) = std::fs::metadata(&path).and_then(|metadata| metadata.modified()) else {
            return (Vec::new(), RolloutScanOutcome::IoError);
        };
        candidates.push((modified, path));
    }
    if deadline_expired(deadline) {
        return (Vec::new(), RolloutScanOutcome::Deadline);
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
        self.recent_with_entry_budget(
            sessions_dir,
            limit,
            force,
            max_age,
            now,
            deadline,
            MAX_ROLLOUT_ENTRIES,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn recent_with_entry_budget(
        &mut self,
        sessions_dir: &Path,
        limit: usize,
        force: bool,
        max_age: Duration,
        now: Instant,
        deadline: Option<Instant>,
        entry_budget: usize,
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
                recent_rollouts_with_entry_budget(sessions_dir, limit, deadline, entry_budget);
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
    codex_app_server_broker()?.request("account/rateLimits/read", timeout)
}

pub fn codex_account_usage_response(timeout: Duration) -> anyhow::Result<String> {
    codex_app_server_broker()?.request("account/usage/read", timeout)
}

pub fn set_codex_app_server_enabled(enabled: bool) -> anyhow::Result<()> {
    codex_app_server_broker()?.set_enabled(enabled);
    Ok(())
}

pub fn begin_codex_app_server_shutdown() {
    if let Some(Ok(broker)) = CODEX_APP_SERVER_BROKER.get() {
        broker.begin_shutdown();
    }
}

pub fn set_grok_acp_enabled(enabled: bool) -> anyhow::Result<()> {
    grok_acp_broker()?.set_enabled(enabled);
    Ok(())
}

pub fn begin_grok_acp_shutdown() {
    if let Some(Ok(broker)) = GROK_ACP_BROKER.get() {
        broker.begin_shutdown();
    }
}

fn codex_app_server_broker() -> anyhow::Result<&'static CodexAppServerBroker> {
    match CODEX_APP_SERVER_BROKER.get_or_init(|| {
        CodexAppServerBroker::start(
            BrokerProtocol::Codex,
            true,
            Arc::new(codex_app_server_command),
            CODEX_BROKER_MIN_BACKOFF,
            CODEX_BROKER_MAX_BACKOFF,
        )
    }) {
        Ok(broker) => Ok(broker),
        Err(error) => Err(anyhow::anyhow!(error.clone())),
    }
}

fn grok_acp_broker() -> anyhow::Result<&'static CodexAppServerBroker> {
    match GROK_ACP_BROKER.get_or_init(|| {
        CodexAppServerBroker::start(
            BrokerProtocol::Grok,
            true,
            Arc::new(grok_agent_command),
            CODEX_BROKER_MIN_BACKOFF,
            CODEX_BROKER_MAX_BACKOFF,
        )
    }) {
        Ok(broker) => Ok(broker),
        Err(error) => Err(anyhow::anyhow!(error.clone())),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrokerProtocol {
    Codex,
    Grok,
}

impl BrokerProtocol {
    fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex app-server",
            Self::Grok => "Grok ACP",
        }
    }

    fn worker_name(self) -> &'static str {
        match self {
            Self::Codex => "juice-codex-app-server",
            Self::Grok => "juice-grok-acp",
        }
    }

    fn shutdown_name(self) -> &'static str {
        match self {
            Self::Codex => "juice-codex-shutdown",
            Self::Grok => "juice-grok-shutdown",
        }
    }

    fn reader_name(self, stream: &str) -> String {
        let provider = match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
        };
        format!("juice-{provider}-{stream}")
    }

    fn initialize(self) -> Value {
        match self {
            Self::Codex => serde_json::json!({
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
            Self::Grok => serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {
                        "name": "agent-juice",
                        "title": "Juice",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
        }
    }

    fn sends_initialized(self) -> bool {
        self == Self::Codex
    }
}

type CodexCommandFactory = Arc<dyn Fn() -> anyhow::Result<Command> + Send + Sync + 'static>;

enum CodexBrokerMessage {
    Request {
        method: &'static str,
        deadline: Instant,
        reply: mpsc::SyncSender<Result<String, String>>,
    },
    Reset,
    Shutdown {
        reply: mpsc::SyncSender<()>,
    },
}

struct CodexAppServerBroker {
    sender: mpsc::SyncSender<CodexBrokerMessage>,
    enabled: Arc<AtomicBool>,
    protocol: BrokerProtocol,
    #[cfg(test)]
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl CodexAppServerBroker {
    fn start(
        protocol: BrokerProtocol,
        enabled: bool,
        command_factory: CodexCommandFactory,
        min_backoff: Duration,
        max_backoff: Duration,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(CODEX_BROKER_QUEUE_CAPACITY);
        let enabled = Arc::new(AtomicBool::new(enabled));
        let worker_enabled = Arc::clone(&enabled);
        let worker = std::thread::Builder::new()
            .name(protocol.worker_name().into())
            .spawn(move || {
                CodexBrokerWorker::new(
                    protocol,
                    command_factory,
                    worker_enabled,
                    min_backoff,
                    max_backoff,
                )
                .run(receiver);
            })
            .map_err(|_| {
                format!(
                    "{} command unavailable: broker thread failed",
                    protocol.label()
                )
            })?;
        #[cfg(not(test))]
        drop(worker);
        Ok(Self {
            sender,
            enabled,
            protocol,
            #[cfg(test)]
            worker: Mutex::new(Some(worker)),
        })
    }

    fn request(&self, method: &'static str, timeout: Duration) -> anyhow::Result<String> {
        if !self.enabled.load(Ordering::Acquire) {
            anyhow::bail!(
                "{} command unavailable by collection policy",
                self.protocol.label()
            );
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow::anyhow!("{} timed out", self.protocol.label()))?;
        if timeout.is_zero() {
            anyhow::bail!("{} timed out", self.protocol.label());
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.send_until(
            CodexBrokerMessage::Request {
                method,
                deadline,
                reply,
            },
            deadline,
        )?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("{} timed out", self.protocol.label());
        }
        match response.recv_timeout(remaining) {
            Ok(Ok(raw)) => Ok(raw),
            Ok(Err(error)) => Err(anyhow::anyhow!(error)),
            Err(_) => Err(anyhow::anyhow!("{} timed out", self.protocol.label())),
        }
    }

    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        if !enabled {
            let _ = self.sender.try_send(CodexBrokerMessage::Reset);
        }
    }

    fn begin_shutdown(&self) {
        self.enabled.store(false, Ordering::Release);
        let sender = self.sender.clone();
        let protocol = self.protocol;
        let _ = std::thread::Builder::new()
            .name(protocol.shutdown_name().into())
            .spawn(move || {
                let deadline = Instant::now() + CODEX_BROKER_CONTROL_TIMEOUT;
                let (reply, response) = mpsc::sync_channel(1);
                if send_codex_broker_message_until(
                    &sender,
                    CodexBrokerMessage::Shutdown { reply },
                    deadline,
                    protocol,
                )
                .is_ok()
                {
                    let _ =
                        response.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                }
            });
    }

    #[cfg(test)]
    fn shutdown_for_test(&self) {
        self.enabled.store(false, Ordering::Release);
        let deadline = Instant::now() + CODEX_BROKER_CONTROL_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        let sent = self
            .send_until(CodexBrokerMessage::Shutdown { reply }, deadline)
            .is_ok();
        if sent {
            let _ = response.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
        if sent {
            if let Some(worker) = self
                .worker
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = worker.join();
            }
        }
    }

    fn send_until(&self, message: CodexBrokerMessage, deadline: Instant) -> anyhow::Result<()> {
        send_codex_broker_message_until(&self.sender, message, deadline, self.protocol)
    }
}

fn send_codex_broker_message_until(
    sender: &mpsc::SyncSender<CodexBrokerMessage>,
    mut message: CodexBrokerMessage,
    deadline: Instant,
    protocol: BrokerProtocol,
) -> anyhow::Result<()> {
    loop {
        match sender.try_send(message) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(returned)) if Instant::now() < deadline => {
                message = returned;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(mpsc::TrySendError::Full(_)) => {
                anyhow::bail!("{} timed out", protocol.label())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                anyhow::bail!("{} command unavailable: broker stopped", protocol.label())
            }
        }
    }
}

struct CodexBrokerWorker {
    protocol: BrokerProtocol,
    command_factory: CodexCommandFactory,
    enabled: Arc<AtomicBool>,
    session: Option<CodexAppServerSession>,
    consecutive_failures: u32,
    retry_at: Option<Instant>,
    retry_error: Option<String>,
    min_backoff: Duration,
    max_backoff: Duration,
}

impl CodexBrokerWorker {
    fn new(
        protocol: BrokerProtocol,
        command_factory: CodexCommandFactory,
        enabled: Arc<AtomicBool>,
        min_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self {
            protocol,
            command_factory,
            enabled,
            session: None,
            consecutive_failures: 0,
            retry_at: None,
            retry_error: None,
            min_backoff,
            max_backoff,
        }
    }

    fn run(&mut self, receiver: mpsc::Receiver<CodexBrokerMessage>) {
        loop {
            if let Some(session) = self.session.as_mut() {
                if let Err(error) = session.poll_idle() {
                    self.session.take();
                    self.record_failure(&error);
                }
            }
            match receiver.recv_timeout(CODEX_BROKER_IDLE_POLL) {
                Ok(CodexBrokerMessage::Request {
                    method,
                    deadline,
                    reply,
                }) => {
                    let result = self.handle_request(method, deadline);
                    let _ = reply.send(result);
                }
                Ok(CodexBrokerMessage::Reset) => self.reset(),
                Ok(CodexBrokerMessage::Shutdown { reply }) => {
                    self.reset();
                    let _ = reply.send(());
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.reset();
                    break;
                }
            }
        }
    }

    fn handle_request(
        &mut self,
        method: &'static str,
        deadline: Instant,
    ) -> Result<String, String> {
        if !self.enabled.load(Ordering::Acquire) {
            self.reset();
            return Err(format!(
                "{} command unavailable by collection policy",
                self.protocol.label()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!("{} timed out", self.protocol.label()));
        }
        if self
            .retry_at
            .is_some_and(|retry_at| Instant::now() < retry_at)
        {
            return Err(self
                .retry_error
                .clone()
                .unwrap_or_else(|| format!("{} retry backoff active", self.protocol.label())));
        }
        if self.session.is_none() {
            match CodexAppServerSession::start(
                self.protocol,
                Arc::clone(&self.command_factory),
                Arc::clone(&self.enabled),
                deadline,
            ) {
                Ok(session) => self.session = Some(session),
                Err(error) => {
                    self.record_failure(&error);
                    return Err(error);
                }
            }
        }

        let result = self
            .session
            .as_mut()
            .expect("persistent RPC session must exist after successful start")
            .request(method, deadline);
        match result {
            Ok(raw) => {
                self.consecutive_failures = 0;
                self.retry_at = None;
                self.retry_error = None;
                Ok(raw)
            }
            Err(error) => {
                let disabled = !self.enabled.load(Ordering::Acquire);
                self.session.take();
                if disabled {
                    self.consecutive_failures = 0;
                    self.retry_at = None;
                    self.retry_error = None;
                } else {
                    self.record_failure(&error);
                }
                Err(error)
            }
        }
    }

    fn record_failure(&mut self, error: &str) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let exponent = self.consecutive_failures.saturating_sub(1).min(4);
        let backoff = self
            .min_backoff
            .saturating_mul(1u32 << exponent)
            .min(self.max_backoff);
        self.retry_at = Instant::now().checked_add(backoff);
        let label = self.protocol.label();
        self.retry_error = Some(if text_requires_login(error) {
            format!("{label} authentication required")
        } else if error.to_ascii_lowercase().contains("command unavailable") {
            format!("{label} command unavailable during retry backoff")
        } else {
            format!("{label} retry backoff active")
        });
    }

    fn reset(&mut self) {
        self.session.take();
        self.consecutive_failures = 0;
        self.retry_at = None;
        self.retry_error = None;
    }
}

struct CodexAppServerSession {
    protocol: BrokerProtocol,
    child: Option<Child>,
    tree: Option<ProcessTree>,
    stdin: Option<ChildStdin>,
    stdout: Option<mpsc::Receiver<std::io::Result<String>>>,
    stdout_done: mpsc::Receiver<()>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    stderr_done: mpsc::Receiver<()>,
    enabled: Arc<AtomicBool>,
    next_request_id: i64,
}

impl CodexAppServerSession {
    fn start(
        protocol: BrokerProtocol,
        command_factory: CodexCommandFactory,
        enabled: Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<Self, String> {
        if !enabled.load(Ordering::Acquire) {
            return Err(format!(
                "{} command unavailable by collection policy",
                protocol.label()
            ));
        }
        let mut command =
            command_factory().map_err(|_| format!("{} command unavailable", protocol.label()))?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let hard_deadline = deadline
            .checked_add(PROCESS_CLEANUP_GRACE)
            .unwrap_or(deadline);
        let (mut child, tree) = spawn_in_process_tree(&mut command, hard_deadline)
            .map_err(|_| format!("{} transport failed", protocol.label()))?;
        let setup = (|| {
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| format!("{} stdout unavailable", protocol.label()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| format!("{} stderr unavailable", protocol.label()))?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| format!("{} stdin unavailable", protocol.label()))?;

            let (stdout_sender, stdout_receiver) = mpsc::sync_channel(CODEX_BROKER_STDOUT_CAPACITY);
            let (stdout_done_sender, stdout_done_receiver) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name(protocol.reader_name("stdout"))
                .spawn(move || {
                    let mut reader = BufReader::new(stdout);
                    loop {
                        let mut line_bytes = 0;
                        match read_bounded_line(&mut reader, &mut line_bytes) {
                            Ok(Some(line)) => {
                                if stdout_sender.send(Ok(line)).is_err() {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(error) => {
                                let _ = stdout_sender.send(Err(error));
                                break;
                            }
                        }
                    }
                    let _ = stdout_done_sender.send(());
                })
                .map_err(|_| format!("{} stdout reader unavailable", protocol.label()))?;

            let stderr_tail = Arc::new(Mutex::new(Vec::new()));
            let stderr_reader_tail = Arc::clone(&stderr_tail);
            let (stderr_done_sender, stderr_done_receiver) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name(protocol.reader_name("stderr"))
                .spawn(move || {
                    drain_bounded_tail(BufReader::new(stderr), &stderr_reader_tail);
                    let _ = stderr_done_sender.send(());
                })
                .map_err(|_| format!("{} stderr reader unavailable", protocol.label()))?;
            Ok::<_, String>((
                stdin,
                stdout_receiver,
                stdout_done_receiver,
                stderr_tail,
                stderr_done_receiver,
            ))
        })();
        let (stdin, stdout_receiver, stdout_done_receiver, stderr_tail, stderr_done_receiver) =
            match setup {
                Ok(setup) => setup,
                Err(error) => {
                    terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
                    return Err(error);
                }
            };

        let mut session = Self {
            protocol,
            child: Some(child),
            tree: Some(tree),
            stdin: Some(stdin),
            stdout: Some(stdout_receiver),
            stdout_done: stdout_done_receiver,
            stderr_tail,
            stderr_done: stderr_done_receiver,
            enabled,
            next_request_id: 2,
        };
        let initialize = protocol.initialize();
        session.write_message(&initialize)?;
        session.wait_for_response(1, deadline)?;
        if protocol.sends_initialized() {
            session.write_message(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }))?;
        }
        Ok(session)
    }

    fn request(&mut self, method: &'static str, deadline: Instant) -> Result<String, String> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| format!("{} request id exhausted", self.protocol.label()))?;
        self.write_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": {}
        }))?;
        self.wait_for_response(request_id, deadline)
    }

    fn write_message(&mut self, message: &Value) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| format!("{} stdin unavailable", self.protocol.label()))?;
        writeln!(stdin, "{message}")
            .and_then(|()| stdin.flush())
            .map_err(|_| format!("{} transport failed", self.protocol.label()))
    }

    fn wait_for_response(&mut self, response_id: i64, deadline: Instant) -> Result<String, String> {
        let mut skipped = 0usize;
        loop {
            if !self.enabled.load(Ordering::Acquire) {
                return Err(format!(
                    "{} command unavailable by collection policy",
                    self.protocol.label()
                ));
            }
            if Instant::now() >= deadline {
                return Err(
                    self.failure_with_stderr(&format!("{} timed out", self.protocol.label()))
                );
            }
            if self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some()
            {
                return Err(self
                    .failure_with_stderr(&format!("{} transport failed", self.protocol.label())));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait_for = remaining.min(Duration::from_millis(50));
            let Some(stdout) = self.stdout.as_ref() else {
                return Err(format!("{} stdout unavailable", self.protocol.label()));
            };
            match stdout.recv_timeout(wait_for) {
                Ok(Ok(line)) => {
                    let value = parse_app_server_line(self.protocol, &line)?;
                    if value
                        .as_ref()
                        .and_then(|value| value.get("id"))
                        .and_then(Value::as_i64)
                        != Some(response_id)
                    {
                        skipped = skipped.saturating_add(1);
                        if skipped > CODEX_BROKER_MAX_SKIPPED_MESSAGES {
                            return Err(format!(
                                "{} message limit exceeded",
                                self.protocol.label()
                            ));
                        }
                        continue;
                    }
                    let value = value.expect("matching response id requires a JSON object");
                    if let Some(error) = value.get("error") {
                        if text_requires_login(&error.to_string()) {
                            return Err(format!(
                                "{} authentication required",
                                self.protocol.label()
                            ));
                        }
                        return Err(format!("{} returned an error", self.protocol.label()));
                    }
                    return Ok(line);
                }
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.failure_with_stderr(&format!(
                        "{} transport failed",
                        self.protocol.label()
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
    }

    fn poll_idle(&mut self) -> Result<(), String> {
        if self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .is_some()
        {
            return Err(
                self.failure_with_stderr(&format!("{} transport failed", self.protocol.label()))
            );
        }
        let Some(stdout) = self.stdout.as_ref() else {
            return Err(format!("{} stdout unavailable", self.protocol.label()));
        };
        for _ in 0..CODEX_BROKER_MAX_SKIPPED_MESSAGES {
            match stdout.try_recv() {
                Ok(Ok(line)) => {
                    parse_app_server_line(self.protocol, &line)?;
                }
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(self.failure_with_stderr(&format!(
                        "{} transport failed",
                        self.protocol.label()
                    )));
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
            }
        }
        Err(format!("{} message limit exceeded", self.protocol.label()))
    }

    fn failure_with_stderr(&self, fallback: &str) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if text_requires_login(&String::from_utf8_lossy(&tail)) {
            format!("{} authentication required", self.protocol.label())
        } else {
            fallback.into()
        }
    }
}

fn parse_app_server_line(protocol: BrokerProtocol, line: &str) -> Result<Option<Value>, String> {
    let line = line.trim();
    // Bounded diagnostics are skipped, but malformed JSON-looking frames desync the session.
    if line.is_empty() || !line.starts_with('{') {
        return Ok(None);
    }
    serde_json::from_str::<Value>(line)
        .map(Some)
        .map_err(|_| format!("{} returned malformed JSON", protocol.label()))
}

impl Drop for CodexAppServerSession {
    fn drop(&mut self) {
        self.stdin.take();
        self.stdout.take();
        let deadline = Instant::now() + PROCESS_CLEANUP_GRACE;
        if let Some(child) = self.child.as_mut() {
            terminate_process_tree_until(child, self.tree.as_ref(), deadline);
        }
        let _ = self
            .stdout_done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let _ = self
            .stderr_done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

fn drain_bounded_tail(mut reader: impl Read, tail: &Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 4096];
    while let Ok(read) = reader.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let mut tail = tail.lock().unwrap_or_else(|error| error.into_inner());
        let incoming = &chunk[..read];
        if incoming.len() >= MAX_COMMAND_ERROR_BYTES {
            tail.clear();
            tail.extend_from_slice(&incoming[incoming.len() - MAX_COMMAND_ERROR_BYTES..]);
            continue;
        }
        let overflow = tail
            .len()
            .saturating_add(incoming.len())
            .saturating_sub(MAX_COMMAND_ERROR_BYTES);
        if overflow > 0 {
            let drain = overflow.min(tail.len());
            tail.drain(..drain);
        }
        tail.extend_from_slice(incoming);
    }
}

#[cfg(test)]
fn codex_account_rate_limits_response_with_command(
    command: Command,
    timeout: Duration,
) -> anyhow::Result<String> {
    codex_account_method_response_with_command(
        command,
        timeout,
        "account/rateLimits/read",
        CODEX_ACCOUNT_RESPONSE_ID,
    )
}

#[cfg(test)]
fn codex_account_usage_response_with_command(
    command: Command,
    timeout: Duration,
) -> anyhow::Result<String> {
    codex_account_method_response_with_command(
        command,
        timeout,
        "account/usage/read",
        CODEX_USAGE_RESPONSE_ID,
    )
}

#[cfg(test)]
fn codex_account_method_response_with_command(
    command: Command,
    timeout: Duration,
    method: &str,
    response_id: i64,
) -> anyhow::Result<String> {
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
            "id": response_id,
            "method": method,
            "params": {}
        }),
    ];
    json_rpc_response_with_command(command, &requests, response_id, timeout, "codex app-server")
}

pub fn grok_billing_response(timeout: Duration) -> anyhow::Result<String> {
    grok_acp_broker()?.request("_x.ai/billing", timeout)
}

#[cfg(test)]
fn grok_billing_response_with_command(
    command: Command,
    timeout: Duration,
) -> anyhow::Result<String> {
    let requests = [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {
                    "name": "agent-juice",
                    "title": "Juice",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": GROK_BILLING_RESPONSE_ID,
            "method": "_x.ai/billing",
            "params": {}
        }),
    ];
    json_rpc_response_with_command(
        command,
        &requests,
        GROK_BILLING_RESPONSE_ID,
        timeout,
        "Grok ACP",
    )
}

#[cfg(test)]
fn json_rpc_response_with_command(
    mut command: Command,
    requests: &[Value],
    response_id: i64,
    timeout: Duration,
    label: &str,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    let hard_deadline = deadline + PROCESS_CLEANUP_GRACE;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, tree) = spawn_in_process_tree(&mut command, hard_deadline)?;

    let result = (|| {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("{label} stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("{label} stderr unavailable"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("{label} stdin unavailable"))?;

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

        let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = read_bounded_to_end(
                BufReader::new(stderr),
                MAX_COMMAND_ERROR_BYTES,
                "JSON-RPC stderr",
            );
            let _ = stderr_tx.send(result);
        });

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
                    if value.get("id").and_then(Value::as_i64) != Some(response_id) {
                        continue;
                    }
                    outcome = Some(if let Some(error) = value.get("error") {
                        if text_requires_login(&error.to_string()) {
                            Err(anyhow::anyhow!("{label} authentication required"))
                        } else {
                            Err(anyhow::anyhow!("{label} returned an error"))
                        }
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
            .map_err(|_| anyhow::anyhow!("{label} stdout did not close"))?;
        let remaining = hard_deadline.saturating_duration_since(Instant::now());
        let stderr = stderr_rx
            .recv_timeout(remaining)
            .map_err(|_| anyhow::anyhow!("{label} stderr did not close"))??;
        let outcome = outcome.unwrap_or_else(|| Err(anyhow::anyhow!("{label} timed out")));
        if outcome.is_err() && text_requires_login(&String::from_utf8_lossy(&stderr)) {
            Err(anyhow::anyhow!("{label} authentication required"))
        } else {
            outcome
        }
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
    let credentials = read_claude_oauth_credentials(&credentials_path)?;
    let credentials: Value = serde_json::from_str(&credentials)?;
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
    let user_agent = claude_user_agent(remaining);
    let remaining = remaining_until(deadline, "Claude OAuth usage")?;
    claude_oauth_usage_response_from(
        CLAUDE_OAUTH_USAGE_URL,
        token,
        user_agent,
        Instant::now() + remaining,
    )
}

fn read_claude_oauth_credentials(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("Claude OAuth credentials unavailable")
        } else {
            anyhow::anyhow!("could not read Claude OAuth credentials: {error}")
        }
    })
}

fn remaining_until(deadline: Instant, label: &str) -> anyhow::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        anyhow::bail!("{label} deadline exceeded");
    }
    Ok(remaining)
}

fn claude_user_agent(timeout: Duration) -> &'static str {
    CLAUDE_USER_AGENT
        .get_or_init(|| {
            command_output_with_input(claude_version_command(), None, timeout, "Claude version")
                .ok()
                .and_then(|output| {
                    output
                        .split_whitespace()
                        .next()
                        .filter(|value| {
                            !value.is_empty()
                                && value.bytes().all(|byte| {
                                    byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
                                })
                        })
                        .map(|version| format!("claude-code/{version}"))
                })
                .unwrap_or_else(|| "claude-code/unknown".into())
        })
        .as_str()
}

fn claude_oauth_usage_response_from(
    url: &str,
    token: &str,
    user_agent: &str,
    deadline: Instant,
) -> anyhow::Result<String> {
    let authorization = format!("Bearer {token}");
    let response = http_transport::execute(
        HttpMethod::Get,
        url,
        &[
            ("authorization", authorization.as_str()),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("content-type", "application/json"),
            ("user-agent", user_agent),
            ("x-app", "cli"),
        ],
        None,
        deadline,
        CLAUDE_OAUTH_RESPONSE_CAP,
        "Claude OAuth usage request failed",
    )
    .map_err(|error| match error.kind {
        HttpErrorKind::Deadline => anyhow::anyhow!("Claude OAuth usage timed out"),
        HttpErrorKind::Oversized => {
            anyhow::anyhow!("Claude OAuth usage response exceeded its limit")
        }
        HttpErrorKind::InvalidRequest => {
            anyhow::anyhow!("Claude OAuth usage request was not recognized")
        }
        HttpErrorKind::Transport => anyhow::anyhow!("Claude OAuth usage request failed"),
    })?;
    if matches!(response.status, 401 | 403)
        || text_requires_login(&String::from_utf8_lossy(&response.body))
    {
        anyhow::bail!("Claude OAuth authentication required");
    }
    if !(200..=299).contains(&response.status) {
        anyhow::bail!("Claude OAuth usage request was rejected");
    }
    if !response.content_type.starts_with("application/json") {
        anyhow::bail!("Claude OAuth usage response type was not recognized");
    }
    String::from_utf8(response.body)
        .map_err(|_| anyhow::anyhow!("Claude OAuth usage response was not recognized"))
}

fn claude_usage_output_with_command(command: Command, timeout: Duration) -> anyhow::Result<String> {
    command_output_with_input(command, None, timeout, "Claude usage")
}

pub(crate) fn command_output_with_input(
    command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    label: &str,
) -> anyhow::Result<String> {
    command_output_with_input_caps(
        command,
        input,
        timeout,
        label,
        MAX_COMMAND_OUTPUT_BYTES,
        MAX_COMMAND_ERROR_BYTES,
    )
}

pub(crate) fn command_output_with_input_caps(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    label: &str,
    stdout_cap: usize,
    stderr_cap: usize,
) -> anyhow::Result<String> {
    if stdout_cap == 0 || stderr_cap == 0 {
        anyhow::bail!("{label} output cap unavailable");
    }
    let deadline = Instant::now() + timeout;
    let hard_deadline = deadline + PROCESS_CLEANUP_GRACE;
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, tree) = spawn_in_process_tree(&mut command, hard_deadline)?;
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
        let result = read_bounded_to_end(BufReader::new(stdout), stdout_cap, "command stdout");
        let _ = stdout_tx.send(result);
    });
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = read_bounded_to_end(BufReader::new(stderr), stderr_cap, "command stderr");
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

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowsCodexRuntime {
    Native(PathBuf),
    CommandShim(PathBuf),
}

#[cfg(windows)]
fn windows_metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(windows)]
fn safe_codex_directory(path: &Path) -> anyhow::Result<Option<std::fs::Metadata>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || windows_metadata_is_reparse(&metadata)
    {
        anyhow::bail!("Codex Desktop runtime directory rejected");
    }
    Ok(Some(metadata))
}

#[cfg(windows)]
fn safe_codex_executable(path: &Path) -> anyhow::Result<Option<std::fs::Metadata>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || windows_metadata_is_reparse(&metadata)
    {
        anyhow::bail!("Codex Desktop runtime executable rejected");
    }
    Ok(Some(metadata))
}

#[cfg(windows)]
fn is_codex_runtime_directory_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.len() == CODEX_RUNTIME_DIRECTORY_NAME_LEN
        && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(windows)]
fn find_codex_desktop_executable(local_app_data: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let Some(local_app_data) = local_app_data else {
        return Ok(None);
    };
    let bin = local_app_data.join("OpenAI").join("Codex").join("bin");
    if safe_codex_directory(&bin)?.is_none() {
        return Ok(None);
    }

    let mut latest: Option<(SystemTime, PathBuf)> = None;
    let mut entries_seen = 0usize;
    for entry in std::fs::read_dir(&bin)? {
        entries_seen = entries_seen.saturating_add(1);
        if entries_seen > MAX_CODEX_DESKTOP_BIN_ENTRIES {
            anyhow::bail!("Codex Desktop runtime directory entry limit exceeded");
        }
        let entry = entry?;
        if !is_codex_runtime_directory_name(&entry.file_name()) {
            continue;
        }
        let directory = entry.path();
        if safe_codex_directory(&directory)?.is_none() {
            continue;
        }
        let executable = directory.join("codex.exe");
        let Some(metadata) = safe_codex_executable(&executable)? else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if latest
            .as_ref()
            .is_none_or(|(current, path)| (modified, &executable) > (*current, path))
        {
            latest = Some((modified, executable));
        }
    }

    if let Some((_, executable)) = latest {
        return Ok(Some(executable));
    }
    let fallback = bin.join("codex.exe");
    Ok(safe_codex_executable(&fallback)?.map(|_| fallback))
}

#[cfg(windows)]
fn find_codex_path_runtime(path: Option<&std::ffi::OsStr>) -> Option<WindowsCodexRuntime> {
    const NATIVE_NAMES: [&str; 2] = ["codex.exe", "codex.com"];
    const SHIM_NAMES: [&str; 2] = ["codex.cmd", "codex.bat"];

    for directory in std::env::split_paths(path?) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        for name in NATIVE_NAMES {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(WindowsCodexRuntime::Native(candidate));
            }
        }
        for name in SHIM_NAMES {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(WindowsCodexRuntime::CommandShim(candidate));
            }
        }
    }
    None
}

#[cfg(windows)]
fn resolve_codex_runtime_from(
    local_app_data: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> anyhow::Result<Option<WindowsCodexRuntime>> {
    if let Some(executable) = find_codex_desktop_executable(local_app_data)? {
        return Ok(Some(WindowsCodexRuntime::Native(executable)));
    }
    Ok(find_codex_path_runtime(path))
}

#[cfg(windows)]
fn codex_app_server_command_for(runtime: WindowsCodexRuntime) -> anyhow::Result<Command> {
    use std::os::windows::process::CommandExt;

    let mut command = match runtime {
        WindowsCodexRuntime::Native(executable) => {
            let mut native = Command::new(executable);
            native.args(["app-server", "--listen", "stdio://"]);
            native
        }
        WindowsCodexRuntime::CommandShim(shim) => {
            let shim_text = shim
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Codex command shim path rejected"))?;
            if shim_text
                .chars()
                .any(|character| matches!(character, '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!'))
            {
                anyhow::bail!("Codex command shim path rejected");
            }
            let system_root = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let mut shell = Command::new(system_root.join("System32").join("cmd.exe"));
            shell.args(["/D", "/C"]);
            shell.arg(shim);
            shell.args(["app-server", "--listen", "stdio://"]);
            shell
        }
    };
    command.creation_flags(0x08000000);
    Ok(command)
}

fn codex_app_server_command() -> anyhow::Result<Command> {
    #[cfg(windows)]
    {
        let runtime = resolve_codex_runtime_from(
            dirs::data_local_dir().as_deref(),
            std::env::var_os("PATH").as_deref(),
        )?
        .ok_or_else(|| anyhow::anyhow!("Codex executable unavailable"))?;
        codex_app_server_command_for(runtime)
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("codex");
        command.args(["app-server", "--listen", "stdio://"]);
        Ok(command)
    }
}

fn grok_agent_command() -> anyhow::Result<Command> {
    let executable = find_grok_executable(
        std::env::var_os("PATH").as_deref(),
        dirs::home_dir().as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("Grok Build executable unavailable"))?;
    let mut command = Command::new(executable);
    command.args(["agent", "stdio"]);
    Ok(command)
}

fn find_grok_executable(path: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    #[cfg(windows)]
    const NAMES: [&str; 2] = ["grok.exe", "grok"];
    #[cfg(not(windows))]
    const NAMES: [&str; 1] = ["grok"];

    if let Some(path) = path {
        for directory in std::env::split_paths(path) {
            for name in NAMES {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    let fallback = home?.join(".grok").join("bin").join(NAMES[0]);
    fallback.is_file().then_some(fallback)
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

        let shell = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("cmd.exe");
        let mut command = Command::new(shell);
        command.args(["/D", "/C", "claude"]);
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

fn claude_version_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let shell = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("cmd.exe");
        let mut command = Command::new(shell);
        command.args(["/D", "/C", "claude", "--version"]);
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
        io::{BufRead, Read, Write},
        net::TcpListener,
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            Arc, Barrier,
        },
    };

    static TEST_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn unique_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-juice-{label}-{}-{}",
            std::process::id(),
            TEST_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

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

    #[cfg(windows)]
    #[test]
    fn fake_spawn_marker_child() {
        let Some(marker) = std::env::var_os("AGENT_JUICE_SUSPENDED_SPAWN_MARKER") else {
            return;
        };
        std::fs::write(marker, b"started").unwrap();
    }

    #[test]
    fn rollout_entry_budget_bounds_large_non_rollout_directories() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-rollout-entry-budget-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..128 {
            std::fs::write(root.join(format!("noise-{index:03}.jsonl")), b"{}\n").unwrap();
        }

        let (rollouts, outcome) = list_rollouts_with_entry_budget(&root, None, 16);

        assert!(rollouts.is_empty());
        assert_eq!(outcome, RolloutScanOutcome::EntryLimit);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn entry_budget_incomplete_scan_preserves_cache_and_retries() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-rollout-entry-cache-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let existing = root.join("rollout-existing.jsonl");
        std::fs::write(&existing, b"{}\n").unwrap();
        let now = Instant::now();
        let mut cache = RolloutCache::default();
        assert_eq!(
            cache
                .recent_with_entry_budget(&root, 1, false, Duration::from_secs(60), now, None, 16,),
            vec![existing.clone()]
        );

        std::thread::sleep(Duration::from_millis(20));
        let newest = root.join("rollout-newest.jsonl");
        std::fs::write(&newest, b"{}\n").unwrap();
        let mut noise = Vec::new();
        for index in 0..32 {
            let path = root.join(format!("noise-{index:02}.jsonl"));
            std::fs::write(&path, b"{}\n").unwrap();
            noise.push(path);
        }

        assert_eq!(
            cache.recent_with_entry_budget(
                &root,
                1,
                true,
                Duration::from_secs(60),
                now + Duration::from_secs(1),
                None,
                8,
            ),
            vec![existing]
        );

        for path in noise {
            std::fs::remove_file(path).unwrap();
        }
        assert_eq!(
            cache.recent_with_entry_budget(
                &root,
                1,
                true,
                Duration::from_secs(60),
                now + Duration::from_secs(2),
                None,
                8,
            ),
            vec![newest]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn child_cannot_run_before_job_assignment() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-suspended-spawn-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("started.txt");
        let mut command = test_process("collector::tests::fake_spawn_marker_child");
        command.env("AGENT_JUICE_SUSPENDED_SPAWN_MARKER", &marker);

        let marker_during_attach = marker.clone();
        let (mut child, tree) = spawn_in_process_tree_with_hook(
            &mut command,
            Instant::now() + Duration::from_secs(1),
            move |_| {
                std::thread::sleep(Duration::from_millis(150));
                anyhow::ensure!(
                    !marker_during_attach.exists(),
                    "suspended child ran before Job assignment"
                );
                Ok(())
            },
        )
        .unwrap();

        assert!(wait_for_exit_until(
            &mut child,
            Instant::now() + Duration::from_secs(2)
        ));
        drop(tree);
        assert!(marker.exists());
        std::fs::remove_dir_all(root).unwrap();
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
    fn oauth_secret_is_sent_only_in_the_http_header() {
        let token = "fixture-secret";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let body = br#"{"five_hour":{"utilization":1},"seven_day":{"utilization":2}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
            request
        });
        let output = claude_oauth_usage_response_from(
            &url,
            token,
            "claude-code/2.1.205",
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert!(output.contains("five_hour"));
        let request = worker.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer fixture-secret"));
        assert!(!format!("{:?}", claude_version_command()).contains(token));
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
        let mut account_method = None;
        for _ in 0..3 {
            let mut request = String::new();
            assert!(reader.read_line(&mut request).unwrap() > 0);
            let value: Value = serde_json::from_str(&request).unwrap();
            if value
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| method.starts_with("account/"))
            {
                account_method = value
                    .get("method")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
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
            let response = if account_method.as_deref() == Some("account/usage/read") {
                serde_json::json!({
                    "id": CODEX_USAGE_RESPONSE_ID,
                    "result": {
                        "summary": {
                            "lifetimeTokens": 30,
                            "peakDailyTokens": 20
                        },
                        "dailyUsageBuckets": [
                            {"startDate": "2026-08-25", "tokens": 10},
                            {"startDate": "2026-08-26", "tokens": 20}
                        ]
                    }
                })
            } else {
                serde_json::json!({
                    "id": CODEX_ACCOUNT_RESPONSE_ID,
                    "result": {
                        "rateLimits": {
                            "primary": {"usedPercent": 12, "windowDurationMins": 300},
                            "secondary": {"usedPercent": 34, "windowDurationMins": 10080}
                        }
                    }
                })
            };
            println!("{response}");
            std::io::stdout().flush().unwrap();
        }
        std::process::exit(0);
    }

    #[test]
    fn fake_persistent_app_server_child() {
        if std::env::var_os("AGENT_JUICE_FAKE_PERSISTENT_APP_SERVER").is_none() {
            return;
        }

        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let mut initialize_count = 0u64;
        let mut account_responses = 0u64;
        loop {
            let mut request = String::new();
            if reader.read_line(&mut request).unwrap_or_default() == 0 {
                break;
            }
            let value: Value = serde_json::from_str(&request).unwrap();
            let method = value.get("method").and_then(Value::as_str);
            let id = value.get("id").and_then(Value::as_i64);
            match method {
                Some("initialize") => {
                    if std::env::var_os("AGENT_JUICE_FAKE_GROK_BROKER").is_some() {
                        assert_eq!(
                            value
                                .pointer("/params/protocolVersion")
                                .and_then(Value::as_i64),
                            Some(1)
                        );
                    }
                    initialize_count += 1;
                    println!(
                        "{}",
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"serverInfo": {"name": "fixture"}}
                        })
                    );
                    std::io::stdout().flush().unwrap();
                }
                Some("initialized") => {}
                Some("account/rateLimits/read")
                | Some("account/usage/read")
                | Some("_x.ai/billing") => {
                    if std::env::var_os("AGENT_JUICE_FAKE_BROKER_HANG").is_some() {
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }
                    if std::env::var_os("AGENT_JUICE_FAKE_BROKER_MALFORMED").is_some() {
                        println!("{{malformed");
                        std::io::stdout().flush().unwrap();
                        continue;
                    }
                    if std::env::var_os("AGENT_JUICE_FAKE_BROKER_OVERSIZED").is_some() {
                        std::io::stdout()
                            .write_all(&vec![b'x'; MAX_COMMAND_LINE_BYTES + 1])
                            .unwrap();
                        println!();
                        std::io::stdout().flush().unwrap();
                        continue;
                    }
                    if std::env::var_os("AGENT_JUICE_FAKE_BROKER_AUTH_ERROR").is_some() {
                        println!(
                            "{}",
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {"message": "authentication required"}
                            })
                        );
                        std::io::stdout().flush().unwrap();
                        continue;
                    }
                    account_responses += 1;
                    let result = if method == Some("account/usage/read") {
                        serde_json::json!({
                            "summary": {"lifetimeTokens": 30, "peakDailyTokens": 20},
                            "dailyUsageBuckets": [
                                {"startDate": "2026-08-25", "tokens": 10},
                                {"startDate": "2026-08-26", "tokens": 20}
                            ],
                            "fixturePid": std::process::id(),
                            "initializeCount": initialize_count
                        })
                    } else if method == Some("_x.ai/billing") {
                        serde_json::json!({
                            "period": "weekly",
                            "usedPercent": 18,
                            "fixturePid": std::process::id(),
                            "initializeCount": initialize_count
                        })
                    } else {
                        serde_json::json!({
                            "rateLimits": {
                                "primary": {"usedPercent": 12, "windowDurationMins": 300},
                                "secondary": {"usedPercent": 34, "windowDurationMins": 10080}
                            },
                            "fixturePid": std::process::id(),
                            "initializeCount": initialize_count
                        })
                    };
                    println!(
                        "{}",
                        serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
                    );
                    std::io::stdout().flush().unwrap();
                    if account_responses == 1
                        && std::env::var_os("AGENT_JUICE_FAKE_BROKER_EXIT_AFTER_ONE").is_some()
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    fn fake_persistent_broker(
        spawns: Arc<AtomicUsize>,
        configure: impl Fn(&mut Command) + Send + Sync + 'static,
        min_backoff: Duration,
    ) -> CodexAppServerBroker {
        fake_persistent_broker_for(BrokerProtocol::Codex, spawns, configure, min_backoff)
    }

    fn fake_persistent_broker_for(
        protocol: BrokerProtocol,
        spawns: Arc<AtomicUsize>,
        configure: impl Fn(&mut Command) + Send + Sync + 'static,
        min_backoff: Duration,
    ) -> CodexAppServerBroker {
        let configure = Arc::new(configure);
        let command_factory: CodexCommandFactory = Arc::new(move || {
            spawns.fetch_add(1, Ordering::SeqCst);
            let mut command = test_process("collector::tests::fake_persistent_app_server_child");
            command.env("AGENT_JUICE_FAKE_PERSISTENT_APP_SERVER", "1");
            if protocol == BrokerProtocol::Grok {
                command.env("AGENT_JUICE_FAKE_GROK_BROKER", "1");
            }
            configure(&mut command);
            Ok(command)
        });
        CodexAppServerBroker::start(
            protocol,
            true,
            command_factory,
            min_backoff,
            Duration::from_secs(1),
        )
        .unwrap()
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
    fn codex_account_usage_uses_the_same_bounded_app_server_lifecycle() {
        let mut command = test_process("collector::tests::fake_app_server_child");
        command.env("AGENT_JUICE_FAKE_APP_SERVER", "1");

        let response =
            codex_account_usage_response_with_command(command, Duration::from_secs(2)).unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(value.get("id").and_then(Value::as_i64), Some(3));
        assert_eq!(
            value
                .pointer("/result/summary/lifetimeTokens")
                .and_then(Value::as_u64),
            Some(30)
        );
    }

    #[test]
    fn persistent_codex_broker_reuses_one_initialized_child_for_one_hundred_requests() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker =
            fake_persistent_broker(Arc::clone(&spawns), |_| {}, Duration::from_millis(100));
        let mut fixture_pid = None;
        for index in 0..100 {
            let method = if index % 2 == 0 {
                "account/rateLimits/read"
            } else {
                "account/usage/read"
            };
            let raw = broker.request(method, Duration::from_secs(2)).unwrap();
            let value: Value = serde_json::from_str(&raw).unwrap();
            let result = value.get("result").unwrap();
            assert_eq!(
                result.get("initializeCount").and_then(Value::as_u64),
                Some(1)
            );
            let pid = result.get("fixturePid").and_then(Value::as_u64);
            assert!(pid.is_some());
            assert!(fixture_pid.is_none() || fixture_pid == pid);
            fixture_pid = pid;
        }
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[test]
    fn persistent_grok_broker_reuses_one_initialized_child_for_one_hundred_requests() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker = fake_persistent_broker_for(
            BrokerProtocol::Grok,
            Arc::clone(&spawns),
            |_| {},
            Duration::from_millis(100),
        );
        let mut fixture_pid = None;
        for _ in 0..100 {
            let raw = broker
                .request("_x.ai/billing", Duration::from_secs(2))
                .unwrap();
            let value: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                value
                    .pointer("/result/initializeCount")
                    .and_then(Value::as_u64),
                Some(1)
            );
            let pid = value.pointer("/result/fixturePid").and_then(Value::as_u64);
            assert!(pid.is_some());
            assert!(fixture_pid.is_none() || fixture_pid == pid);
            fixture_pid = pid;
        }
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[test]
    fn concurrent_codex_requests_share_one_child_and_receive_distinct_responses() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker = Arc::new(fake_persistent_broker(
            Arc::clone(&spawns),
            |_| {},
            Duration::from_millis(100),
        ));
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for index in 0..8 {
            let broker = Arc::clone(&broker);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let method = if index % 2 == 0 {
                    "account/rateLimits/read"
                } else {
                    "account/usage/read"
                };
                broker.request(method, Duration::from_secs(2)).unwrap()
            }));
        }
        let mut ids = std::collections::HashSet::new();
        let mut pids = std::collections::HashSet::new();
        for worker in workers {
            let value: Value = serde_json::from_str(&worker.join().unwrap()).unwrap();
            ids.insert(value.get("id").and_then(Value::as_i64).unwrap());
            pids.insert(
                value
                    .pointer("/result/fixturePid")
                    .and_then(Value::as_u64)
                    .unwrap(),
            );
        }
        assert_eq!(ids.len(), 8);
        assert_eq!(pids.len(), 1);
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[test]
    fn disabled_codex_broker_blocks_admission_without_spawning() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker =
            fake_persistent_broker(Arc::clone(&spawns), |_| {}, Duration::from_millis(100));
        broker.set_enabled(false);

        let error = broker
            .request("account/rateLimits/read", Duration::from_secs(1))
            .unwrap_err();

        assert!(format!("{error:#}").contains("collection policy"));
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
        broker.shutdown_for_test();
    }

    #[test]
    fn failed_codex_session_enters_backoff_without_respawn_churn() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker = fake_persistent_broker(
            Arc::clone(&spawns),
            |command| {
                command.env("AGENT_JUICE_FAKE_BROKER_EXIT_AFTER_ONE", "1");
            },
            Duration::from_secs(1),
        );
        broker
            .request("account/rateLimits/read", Duration::from_secs(2))
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let _ = broker.request("account/rateLimits/read", Duration::from_millis(300));
        let error = broker
            .request("account/rateLimits/read", Duration::from_millis(300))
            .unwrap_err();

        assert!(format!("{error:#}").contains("retry backoff"));
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[test]
    fn timed_out_codex_session_is_killed_and_backoff_blocks_the_next_spawn() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker = fake_persistent_broker(
            Arc::clone(&spawns),
            |command| {
                command.env("AGENT_JUICE_FAKE_BROKER_HANG", "1");
            },
            Duration::from_secs(1),
        );

        let timeout = broker
            .request("account/rateLimits/read", Duration::from_millis(150))
            .unwrap_err();
        let backoff = broker
            .request("account/rateLimits/read", Duration::from_millis(300))
            .unwrap_err();

        assert!(format!("{timeout:#}").contains("timed out"));
        assert!(format!("{backoff:#}").contains("retry backoff"));
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[test]
    fn malformed_and_oversized_codex_frames_reset_the_session_and_enter_backoff() {
        for variable in [
            "AGENT_JUICE_FAKE_BROKER_MALFORMED",
            "AGENT_JUICE_FAKE_BROKER_OVERSIZED",
        ] {
            let spawns = Arc::new(AtomicUsize::new(0));
            let broker = fake_persistent_broker(
                Arc::clone(&spawns),
                move |command| {
                    command.env(variable, "1");
                },
                Duration::from_secs(1),
            );

            let first = broker
                .request("account/rateLimits/read", Duration::from_secs(2))
                .unwrap_err();
            let second = broker
                .request("account/rateLimits/read", Duration::from_millis(300))
                .unwrap_err();

            assert!(
                format!("{first:#}").contains("malformed JSON")
                    || format!("{first:#}").contains("transport failed")
            );
            assert!(format!("{second:#}").contains("retry backoff"));
            assert_eq!(spawns.load(Ordering::SeqCst), 1);
            broker.shutdown_for_test();
        }
    }

    #[test]
    fn disabling_an_active_codex_broker_closes_the_session_before_reenable() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker =
            fake_persistent_broker(Arc::clone(&spawns), |_| {}, Duration::from_millis(100));
        let first: Value = serde_json::from_str(
            &broker
                .request("account/rateLimits/read", Duration::from_secs(2))
                .unwrap(),
        )
        .unwrap();
        broker.set_enabled(false);
        std::thread::sleep(Duration::from_millis(100));
        assert!(broker
            .request("account/rateLimits/read", Duration::from_millis(300))
            .is_err());
        broker.set_enabled(true);
        let second: Value = serde_json::from_str(
            &broker
                .request("account/rateLimits/read", Duration::from_secs(2))
                .unwrap(),
        )
        .unwrap();

        assert_ne!(
            first.pointer("/result/fixturePid").and_then(Value::as_u64),
            second.pointer("/result/fixturePid").and_then(Value::as_u64)
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
        broker.shutdown_for_test();
    }

    #[test]
    fn disabling_an_active_grok_broker_closes_the_session_before_reenable() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker = fake_persistent_broker_for(
            BrokerProtocol::Grok,
            Arc::clone(&spawns),
            |_| {},
            Duration::from_millis(100),
        );
        let first: Value = serde_json::from_str(
            &broker
                .request("_x.ai/billing", Duration::from_secs(2))
                .unwrap(),
        )
        .unwrap();
        broker.set_enabled(false);
        std::thread::sleep(Duration::from_millis(100));
        assert!(broker
            .request("_x.ai/billing", Duration::from_millis(300))
            .is_err());
        broker.set_enabled(true);
        let second: Value = serde_json::from_str(
            &broker
                .request("_x.ai/billing", Duration::from_secs(2))
                .unwrap(),
        )
        .unwrap();

        assert_ne!(
            first.pointer("/result/fixturePid").and_then(Value::as_u64),
            second.pointer("/result/fixturePid").and_then(Value::as_u64)
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
        broker.shutdown_for_test();
    }

    #[test]
    fn codex_broker_backoff_preserves_authentication_error_classification() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let broker = fake_persistent_broker(
            Arc::clone(&spawns),
            |command| {
                command.env("AGENT_JUICE_FAKE_BROKER_AUTH_ERROR", "1");
            },
            Duration::from_secs(1),
        );

        let first = broker
            .request("account/rateLimits/read", Duration::from_secs(2))
            .unwrap_err();
        let second = broker
            .request("account/rateLimits/read", Duration::from_millis(300))
            .unwrap_err();

        assert!(error_requires_login(&first));
        assert!(error_requires_login(&second));
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[test]
    fn codex_broker_backoff_preserves_missing_runtime_classification() {
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = Arc::clone(&calls);
        let command_factory: CodexCommandFactory = Arc::new(move || {
            factory_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("fixture runtime missing")
        });
        let broker = CodexAppServerBroker::start(
            BrokerProtocol::Codex,
            true,
            command_factory,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();

        let first = broker
            .request("account/rateLimits/read", Duration::from_secs(1))
            .unwrap_err();
        let second = broker
            .request("account/rateLimits/read", Duration::from_millis(300))
            .unwrap_err();

        assert!(format!("{first:#}").contains("command unavailable"));
        assert!(format!("{second:#}").contains("command unavailable"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        broker.shutdown_for_test();
    }

    #[cfg(windows)]
    fn write_codex_runtime_fixture(path: &Path, modified_secs: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs)),
        )
        .unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn codex_desktop_runtime_prefers_the_newest_versioned_binary_then_unversioned() {
        let root = unique_test_root("codex-desktop-runtime");
        let bin = root.join("OpenAI").join("Codex").join("bin");
        let unversioned = bin.join("codex.exe");
        let older = bin.join("1111111111111111").join("codex.exe");
        let newer = bin.join("2222222222222222").join("codex.exe");
        write_codex_runtime_fixture(&unversioned, 30);
        write_codex_runtime_fixture(&older, 10);
        write_codex_runtime_fixture(&newer, 20);

        assert_eq!(
            find_codex_desktop_executable(Some(&root)).unwrap(),
            Some(newer.clone())
        );
        std::fs::remove_file(newer).unwrap();
        assert_eq!(
            find_codex_desktop_executable(Some(&root)).unwrap(),
            Some(older.clone())
        );
        std::fs::remove_file(older).unwrap();
        assert_eq!(
            find_codex_desktop_executable(Some(&root)).unwrap(),
            Some(unversioned)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn codex_runtime_falls_back_to_a_path_cli_shim_and_reports_missing() {
        let root = unique_test_root("codex-path-runtime");
        let path_dir = root.join("path with spaces");
        std::fs::create_dir_all(&path_dir).unwrap();
        let native = path_dir.join("codex.exe");
        let shim = path_dir.join("codex.cmd");
        std::fs::write(&native, b"fixture").unwrap();
        std::fs::write(&shim, b"@exit /b 0\r\n").unwrap();
        let path = std::env::join_paths([&path_dir]).unwrap();

        assert_eq!(
            resolve_codex_runtime_from(Some(&root.join("missing")), Some(&path)).unwrap(),
            Some(WindowsCodexRuntime::Native(native.clone()))
        );
        std::fs::remove_file(native).unwrap();
        let runtime = resolve_codex_runtime_from(Some(&root.join("missing")), Some(&path))
            .unwrap()
            .unwrap();
        assert_eq!(runtime, WindowsCodexRuntime::CommandShim(shim.clone()));
        let command = codex_app_server_command_for(runtime).unwrap();
        assert!(command
            .get_program()
            .to_string_lossy()
            .ends_with("System32\\cmd.exe"));
        assert!(command.get_args().any(|argument| argument
            .to_string_lossy()
            .contains(shim.to_string_lossy().as_ref())));
        assert!(
            codex_app_server_command_for(WindowsCodexRuntime::CommandShim(shim))
                .unwrap()
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            resolve_codex_runtime_from(Some(&root.join("missing")), None).unwrap(),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn codex_desktop_runtime_scan_is_bounded() {
        let root = unique_test_root("codex-runtime-bound");
        let bin = root.join("OpenAI").join("Codex").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for index in 0..=MAX_CODEX_DESKTOP_BIN_ENTRIES {
            std::fs::write(bin.join(format!("entry-{index:03}")), b"fixture").unwrap();
        }

        let error = find_codex_desktop_executable(Some(&root)).unwrap_err();
        assert!(error.to_string().contains("entry limit"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn codex_desktop_runtime_rejects_a_version_directory_reparse_point() {
        let root = unique_test_root("codex-runtime-reparse");
        let bin = root.join("OpenAI").join("Codex").join("bin");
        let real = root.join("real-runtime");
        let junction = bin.join("aaaaaaaaaaaaaaaa");
        write_codex_runtime_fixture(&real.join("codex.exe"), 1);
        std::fs::create_dir_all(&bin).unwrap();
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let status = Command::new(system_root.join("System32").join("cmd.exe"))
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real)
            .status()
            .unwrap();
        assert!(status.success());

        assert!(find_codex_desktop_executable(Some(&root)).is_err());
        std::fs::remove_dir(&junction).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fake_grok_agent_child() {
        if std::env::var_os("AGENT_JUICE_FAKE_GROK_AGENT").is_none() {
            return;
        }

        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let mut methods = Vec::new();
        for _ in 0..2 {
            let mut request = String::new();
            assert!(reader.read_line(&mut request).unwrap() > 0);
            let value: Value = serde_json::from_str(&request).unwrap();
            methods.push(
                value
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_string(),
            );
        }
        assert_eq!(methods, ["initialize", "_x.ai/billing"]);
        if std::env::var_os("AGENT_JUICE_FAKE_GROK_AUTH_ERROR").is_some() {
            eprintln!("OIDC token refresh failed: invalid_grant RefreshTokenRejected");
            println!(
                "{}",
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": GROK_BILLING_RESPONSE_ID,
                    "error": {"code": -32000, "message": "authentication required"}
                })
            );
            std::io::stdout().flush().unwrap();
            return;
        }
        println!(
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": GROK_BILLING_RESPONSE_ID,
                "result": {
                    "config": {
                        "creditUsagePercent": 12,
                        "currentPeriod": {
                            "type": "USAGE_PERIOD_TYPE_WEEKLY",
                            "start": "2026-08-10T00:00:00Z",
                            "end": "2026-08-17T00:00:00Z"
                        }
                    }
                }
            })
        );
        std::io::stdout().flush().unwrap();
    }

    #[test]
    fn grok_agent_uses_bounded_acp_round_trip() {
        let mut command = test_process("collector::tests::fake_grok_agent_child");
        command.env("AGENT_JUICE_FAKE_GROK_AGENT", "1");

        let response = grok_billing_response_with_command(command, Duration::from_secs(2)).unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(value.get("id").and_then(Value::as_i64), Some(2));
        assert_eq!(
            value
                .pointer("/result/config/creditUsagePercent")
                .and_then(Value::as_i64),
            Some(12)
        );
    }

    #[test]
    fn grok_auth_failure_is_classified_without_exposing_stderr() {
        let mut command = test_process("collector::tests::fake_grok_agent_child");
        command.env("AGENT_JUICE_FAKE_GROK_AGENT", "1");
        command.env("AGENT_JUICE_FAKE_GROK_AUTH_ERROR", "1");

        let error =
            grok_billing_response_with_command(command, Duration::from_secs(2)).unwrap_err();

        assert!(error_requires_login(&error));
        assert!(!error.to_string().contains("RefreshTokenRejected"));
    }

    #[test]
    fn transient_errors_are_not_misclassified_as_login_required() {
        assert!(!text_requires_login("Grok ACP timed out"));
        assert!(!text_requires_login("Codex app-server returned an error"));
        assert!(text_requires_login("HTTP 401: sign in required"));
        assert!(!text_requires_login(
            "HTTP 403: organization policy denied access"
        ));
        assert!(text_requires_login("HTTP 403: RefreshTokenRejected"));
    }

    #[test]
    fn claude_credentials_distinguish_missing_from_other_io_failures() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-claude-credentials-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("missing.json");
        assert!(error_requires_login(
            &read_claude_oauth_credentials(&missing).unwrap_err()
        ));
        assert!(!error_requires_login(
            &read_claude_oauth_credentials(&root).unwrap_err()
        ));
        let credentials = root.join("credentials.json");
        std::fs::write(&credentials, "{}").unwrap();
        assert_eq!(read_claude_oauth_credentials(&credentials).unwrap(), "{}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grok_executable_prefers_path_then_official_home_fallback() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-grok-path-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let path_dir = root.join("path");
        let home = root.join("home");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::create_dir_all(home.join(".grok").join("bin")).unwrap();
        #[cfg(windows)]
        let name = "grok.exe";
        #[cfg(not(windows))]
        let name = "grok";
        let path_executable = path_dir.join(name);
        let fallback = home.join(".grok").join("bin").join(name);
        std::fs::write(&path_executable, b"fixture").unwrap();
        std::fs::write(&fallback, b"fixture").unwrap();
        let path = std::env::join_paths([&path_dir]).unwrap();

        assert_eq!(
            find_grok_executable(Some(path.as_os_str()), Some(&home)),
            Some(path_executable.clone())
        );
        std::fs::remove_file(path_executable).unwrap();
        assert_eq!(
            find_grok_executable(Some(path.as_os_str()), Some(&home)),
            Some(fallback)
        );
        std::fs::remove_dir_all(root).unwrap();
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
    #[ignore = "requires a locally installed and logged-in Codex Desktop or CLI"]
    fn live_codex_persistent_app_server_reuses_one_connection_for_rate_and_activity() {
        set_codex_app_server_enabled(true).unwrap();
        let first = codex_account_rate_limits_response(Duration::from_secs(5)).unwrap();
        let usage = codex_account_usage_response(Duration::from_secs(5)).unwrap();
        let second = codex_account_rate_limits_response(Duration::from_secs(5)).unwrap();
        let first_value: Value = serde_json::from_str(&first).unwrap();
        let usage_value: Value = serde_json::from_str(&usage).unwrap();
        let second_value: Value = serde_json::from_str(&second).unwrap();
        let status = crate::adapters::codex::parse_account_rate_limits_response(
            &first,
            "LIVE",
            "2026-07-13T00:00:00Z",
        )
        .unwrap();

        let first_id = first_value.get("id").and_then(Value::as_i64).unwrap();
        assert_eq!(
            usage_value.get("id").and_then(Value::as_i64),
            Some(first_id + 1)
        );
        assert_eq!(
            second_value.get("id").and_then(Value::as_i64),
            Some(first_id + 2)
        );
        assert!(first_value.pointer("/result/rateLimits").is_some());
        assert!(usage_value.pointer("/result/dailyUsageBuckets").is_some());
        assert!(status.primary.is_some() || status.secondary.is_some());
        begin_codex_app_server_shutdown();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a locally installed and logged-in Codex Desktop"]
    fn live_codex_desktop_app_server_round_trip_without_path_cli() {
        let runtime = resolve_codex_runtime_from(dirs::data_local_dir().as_deref(), None)
            .unwrap()
            .expect("Codex Desktop runtime");
        assert!(matches!(runtime, WindowsCodexRuntime::Native(_)));
        let response = codex_account_rate_limits_response_with_command(
            codex_app_server_command_for(runtime).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        let status = crate::adapters::codex::parse_account_rate_limits_response(
            &response,
            "LIVE",
            "2026-08-26T00:00:00Z",
        )
        .unwrap();

        assert!(status.primary.is_some() || status.secondary.is_some());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a current locally installed and logged-in Codex Desktop"]
    fn live_codex_account_usage_round_trip_without_path_cli() {
        let runtime = resolve_codex_runtime_from(dirs::data_local_dir().as_deref(), None)
            .unwrap()
            .expect("Codex Desktop runtime");
        let response = codex_account_usage_response_with_command(
            codex_app_server_command_for(runtime).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();

        assert!(value.pointer("/result/dailyUsageBuckets").is_some());
    }

    #[test]
    #[ignore = "requires a locally installed and logged-in Grok Build CLI"]
    fn live_grok_billing_round_trip() {
        let response = grok_billing_response(Duration::from_secs(8)).unwrap();
        let status = crate::adapters::grok::parse_billing_response(
            &response,
            "LIVE",
            "2026-08-13T00:00:00Z",
        )
        .unwrap();

        assert_eq!(status.tool, crate::model::Tool::Grok);
        assert!(status.primary.is_some());
        assert!(status.secondary.is_none());
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
    fn repeated_timeouts_leave_no_descendants_or_pipe_readers() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-process-tree-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut markers = Vec::new();
        for attempt in 0..3 {
            let marker = root.join(format!("descendant-survived-{attempt}.txt"));
            let mut command = test_process("collector::tests::process_tree_parent_child");
            command.env("AGENT_JUICE_PROCESS_TREE_PARENT", "1");
            command.env("AGENT_JUICE_PROCESS_TREE_MARKER", &marker);

            let started = Instant::now();
            let error =
                claude_usage_output_with_command(command, Duration::from_millis(200)).unwrap_err();
            assert!(
                error.to_string().contains("command timed out"),
                "pipe reader did not close: {error:#}"
            );
            assert!(started.elapsed() < Duration::from_secs(2));
            markers.push(marker);
        }

        let mut next = test_process("collector::tests::fake_quick_child");
        next.env("AGENT_JUICE_FAKE_QUICK", "1");
        let recovered = claude_usage_output_with_command(next, Duration::from_secs(1)).unwrap();
        assert!(recovered.contains("recovered"));

        std::thread::sleep(Duration::from_millis(1_400));
        assert!(
            markers.iter().all(|marker| !marker.exists()),
            "a descendant survived a timeout"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
