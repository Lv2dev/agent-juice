use crate::paths;
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

pub const MAX_STATUSLINE_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_CLAUDE_SESSION_FILES: usize = 64;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn aj_dir() -> Option<PathBuf> {
    paths::data_dir()
}

pub fn run_with_default_dir(input: &str) -> Vec<u8> {
    match aj_dir() {
        Some(dir) => run_with_dir(input, &dir),
        None => fallback_line(serde_json::from_str::<Value>(input).ok().as_ref()).into_bytes(),
    }
}

pub fn run_without_original_with_default_dir(input: &str) -> Vec<u8> {
    run_with_default_dir(input)
}

pub fn run_with_dir(input: &str, dir: &Path) -> Vec<u8> {
    let parsed = serde_json::from_str::<Value>(input).ok();
    if let Some(value) = parsed.as_ref() {
        let _ = forward_subset(value, dir);
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
    let _ = prune_session_files(dir, &path);
    Ok(path)
}

struct SessionFile {
    path: PathBuf,
    modified: SystemTime,
}

fn prune_session_files(dir: &Path, current_path: &Path) -> std::io::Result<()> {
    let mut candidates: Vec<_> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !(name.starts_with("claude_last.") && name.ends_with(".json")) {
                return None;
            }
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some(SessionFile {
                path: entry.path(),
                modified,
            })
        })
        .collect();
    if candidates.len() <= MAX_CLAUDE_SESSION_FILES {
        return Ok(());
    }

    candidates.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.path.cmp(&right.path))
    });

    let mut keep = HashSet::with_capacity(MAX_CLAUDE_SESSION_FILES);
    if candidates
        .iter()
        .any(|candidate| candidate.path == current_path)
    {
        keep.insert(current_path.to_path_buf());
    }
    if let Some(latest_valid) = candidates
        .iter()
        .find(|candidate| is_valid_session_file(&candidate.path))
    {
        keep.insert(latest_valid.path.clone());
    }
    for candidate in &candidates {
        if keep.len() >= MAX_CLAUDE_SESSION_FILES {
            break;
        }
        keep.insert(candidate.path.clone());
    }

    for candidate in candidates {
        if !keep.contains(&candidate.path) {
            let _ = fs::remove_file(candidate.path);
        }
    }
    Ok(())
}

fn is_valid_session_file(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut contents = Vec::with_capacity(16 * 1024);
    if file
        .take(MAX_STATUSLINE_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .is_err()
        || contents.len() > MAX_STATUSLINE_INPUT_BYTES
    {
        return false;
    }
    serde_json::from_slice::<Value>(&contents).is_ok_and(|value| value.is_object())
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
    use std::time::Duration;

    #[test]
    fn session_prune_keeps_current_and_latest_valid_within_cap() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-statusline-prune-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let current = root.join("claude_last.current.json");
        fs::write(&current, r#"{"session_id":"current"}"#).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let latest_valid = root.join("claude_last.latest-valid.json");
        fs::write(&latest_valid, r#"{"session_id":"latest-valid"}"#).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        for index in 0..(MAX_CLAUDE_SESSION_FILES + 5) {
            fs::write(
                root.join(format!("claude_last.malformed-{index:03}.json")),
                "{malformed",
            )
            .unwrap();
        }

        prune_session_files(&root, &current).unwrap();

        let retained = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("claude_last.") && name.ends_with(".json")
            })
            .count();
        assert_eq!(retained, MAX_CLAUDE_SESSION_FILES);
        assert!(current.exists());
        assert!(latest_valid.exists());

        fs::remove_dir_all(root).unwrap();
    }
}
