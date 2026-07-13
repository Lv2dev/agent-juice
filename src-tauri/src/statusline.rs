use crate::paths;
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_ORIGINAL_COMMAND_LEN: usize = 4096;
const MAX_ORIGINAL_INPUT_BYTES: usize = 1024 * 1024;
const MAX_ORIGINAL_OUTPUT_BYTES: usize = 256 * 1024;
const ORIGINAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_CLEANUP_GRACE: Duration = Duration::from_millis(500);
#[cfg(windows)]
const TASKKILL_TIMEOUT: Duration = Duration::from_millis(250);
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

pub fn aj_dir() -> Option<PathBuf> {
    paths::data_dir()
}

pub fn run_with_default_dir(input: &str) -> Vec<u8> {
    match aj_dir() {
        Some(dir) => run_with_dir(input, &dir),
        None => fallback_line(serde_json::from_str::<Value>(input).ok().as_ref()).into_bytes(),
    }
}

pub fn run_with_dir(input: &str, dir: &Path) -> Vec<u8> {
    let parsed = serde_json::from_str::<Value>(input).ok();
    if let Some(value) = parsed.as_ref() {
        let _ = forward_subset(value, dir);
    }

    if let Some(output) = run_original(input, dir) {
        return output;
    }

    fallback_line(parsed.as_ref()).into_bytes()
}

fn forward_subset(value: &Value, dir: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    let session_id = value
        .get("session_id")
        .and_then(|session| session.as_str())
        .unwrap_or("default");
    let safe = safe_session_id(session_id);
    let subset = serde_json::json!({
        "session_id": value.get("session_id"),
        "context_window": value.get("context_window"),
        "rate_limits": value.get("rate_limits"),
        "cost": value.get("cost"),
    });

    let path = dir.join(format!("claude_last.{safe}.json"));
    replace_file(&path, subset.to_string().as_bytes())?;
    Ok(path)
}

fn safe_session_id(session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    if safe.is_empty() {
        "default".into()
    } else {
        safe
    }
}

fn replace_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("claude-status");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(
        ".{file_name}.{}.{}.aj-tmp",
        std::process::id(),
        sequence
    ));
    fs::write(&tmp, contents)?;
    let result = if path.exists() {
        replace_existing_file(path, &tmp)
    } else {
        fs::rename(&tmp, path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

#[cfg(windows)]
fn replace_existing_file(path: &Path, tmp: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS},
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let target = wide(path);
    let replacement = wide(tmp);
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .map_err(|err| std::io::Error::other(err.to_string()))
    }
}

#[cfg(not(windows))]
fn replace_existing_file(path: &Path, tmp: &Path) -> std::io::Result<()> {
    fs::rename(tmp, path)
}

fn run_original(input: &str, dir: &Path) -> Option<Vec<u8>> {
    let original = verified_original_command(dir)?;
    let original = original.trim();
    if original.is_empty() {
        return None;
    }
    if original.len() > MAX_ORIGINAL_COMMAND_LEN {
        return None;
    }

    if original == "cat" {
        return Some(input.as_bytes().to_vec());
    }

    for (program, args) in shell_specs() {
        if let Some(output) = run_shell(program, args, original, input) {
            return Some(output);
        }
    }

    None
}

fn verified_original_command(dir: &Path) -> Option<String> {
    let original = fs::read_to_string(dir.join("wrap.json")).ok()?;
    let original = original.trim();

    if original == "cat" {
        return Some(original.to_string());
    }

    let meta: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("wrap-meta.json")).ok()?).ok()?;
    let expected = meta.get("original_command").and_then(Value::as_str)?.trim();
    if expected == original {
        Some(original.to_string())
    } else {
        None
    }
}

fn run_shell(program: &str, args: &[&str], original: &str, input: &str) -> Option<Vec<u8>> {
    if input.len() > MAX_ORIGINAL_INPUT_BYTES {
        return None;
    }
    let deadline = Instant::now() + ORIGINAL_COMMAND_TIMEOUT;
    let hard_deadline = deadline + PROCESS_CLEANUP_GRACE;
    let mut child = Command::new(program)
        .args(args)
        .arg(original)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(_) => {
            terminate_process_tree_until(&mut child, None, hard_deadline);
            return None;
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
            return None;
        }
    };
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = read_bounded_to_end(stdout, MAX_ORIGINAL_OUTPUT_BYTES);
        let _ = stdout_tx.send(result);
    });

    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
            return None;
        }
    };
    let input = input.as_bytes().to_vec();
    let (stdin_tx, stdin_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = stdin.write_all(&input).and_then(|_| stdin.flush());
        let _ = stdin_tx.send(result);
    });

    let mut stdin_failed = false;
    let status = loop {
        if let Ok(Err(_)) = stdin_rx.try_recv() {
            stdin_failed = true;
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) | Err(_) => break None,
        }
    };

    terminate_process_tree_until(&mut child, Some(&tree), hard_deadline);
    let remaining = hard_deadline.saturating_duration_since(Instant::now());
    let output = stdout_rx.recv_timeout(remaining).ok()?.ok()?;
    if stdin_failed || !status.is_some_and(|status| status.success()) {
        return None;
    }
    Some(output)
}

fn read_bounded_to_end(mut reader: impl Read, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(max_bytes.min(16 * 1024));
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
            "original statusLine output exceeded byte limit",
        ));
    }
    Ok(output)
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

#[cfg(windows)]
fn shell_specs() -> &'static [(&'static str, &'static [&'static str])] {
    &[("cmd", &["/C"])]
}

#[cfg(not(windows))]
fn shell_specs() -> &'static [(&'static str, &'static [&'static str])] {
    &[("sh", &["-c"])]
}

fn fallback_line(parsed: Option<&Value>) -> String {
    parsed
        .and_then(|value| value.pointer("/context_window/used_percentage"))
        .and_then(|value| value.as_f64())
        .map(|pct| format!("ctx {}%\n", pct.round() as i64))
        .unwrap_or_else(|| "agent-juice\n".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_output_reader_rejects_bytes_beyond_the_cap() {
        let oversized = vec![b'x'; MAX_ORIGINAL_OUTPUT_BYTES + 1];
        assert!(
            read_bounded_to_end(std::io::Cursor::new(oversized), MAX_ORIGINAL_OUTPUT_BYTES)
                .is_err()
        );
    }
}
