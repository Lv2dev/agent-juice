use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const MAX_ORIGINAL_COMMAND_LEN: usize = 4096;

pub fn aj_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AGENT_JUICE_DATA_DIR") {
        return Some(PathBuf::from(path));
    }
    dirs::data_local_dir().map(|dir| dir.join("agent-juice"))
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
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    if !path.exists() {
        return fs::rename(&tmp, path);
    }

    match replace_existing_file(path, &tmp) {
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
    let mut child = Command::new(program)
        .args(args)
        .arg(original)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }

    let output = child.wait_with_output().ok()?;
    if output.status.success() {
        Some(output.stdout)
    } else {
        None
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
