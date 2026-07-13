use crate::{collector, paths};
use chrono::{DateTime, Duration, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration as StdDuration,
};

const GITHUB_LATEST_API: &str = "https://api.github.com/repos/Lv2dev/agent-juice/releases/latest";
const RELEASES_URL: &str = "https://github.com/Lv2dev/agent-juice/releases";
const CHECK_INTERVAL_HOURS: i64 = 24;
const HTTP_TIMEOUT_SECS: &str = "5";

static UPDATE_STATE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdateState {
    pub last_checked_at: Option<String>,
    pub latest_release: Option<ReleaseInfo>,
    pub last_notified_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub status: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub checked_at: Option<String>,
    pub checked_now: bool,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

pub fn state_path() -> Option<PathBuf> {
    paths::data_dir().map(|dir| dir.join("update-state.json"))
}

pub fn releases_url() -> &'static str {
    RELEASES_URL
}

pub fn parse_latest_release(contents: &str) -> anyhow::Result<ReleaseInfo> {
    let release: GithubRelease = serde_json::from_str(contents)?;
    if release.draft || release.prerelease {
        anyhow::bail!("latest release is not stable");
    }
    parse_version(&release.tag_name).ok_or_else(|| anyhow::anyhow!("invalid release version"))?;
    if !is_release_url_allowed(&release.html_url) {
        anyhow::bail!("release URL is not allowed");
    }

    let expected_suffix = format!("/tag/{}", release.tag_name);
    if !release.html_url.ends_with(&expected_suffix) {
        anyhow::bail!("release URL does not match its version");
    }

    Ok(ReleaseInfo {
        version: release.tag_name.trim_start_matches('v').to_string(),
        url: release.html_url,
    })
}

pub fn is_update_available(current: &str, latest: &str) -> anyhow::Result<bool> {
    let current =
        parse_version(current).ok_or_else(|| anyhow::anyhow!("invalid current version"))?;
    let latest = parse_version(latest).ok_or_else(|| anyhow::anyhow!("invalid latest version"))?;
    Ok(latest > current)
}

pub fn is_release_url_allowed(url: &str) -> bool {
    if url == RELEASES_URL || url == format!("{RELEASES_URL}/latest") {
        return true;
    }

    let Some(tag) = url.strip_prefix(&format!("{RELEASES_URL}/tag/")) else {
        return false;
    };
    !tag.is_empty() && !tag.contains(['/', '\\', '?', '#', '@']) && parse_version(tag).is_some()
}

pub fn load_state() -> UpdateState {
    state_path()
        .as_deref()
        .map(load_state_from)
        .unwrap_or_default()
}

pub fn load_state_from(path: &Path) -> UpdateState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn cached_result(current_version: &str) -> UpdateCheckResult {
    result_from_state(current_version, &load_state(), false)
}

pub fn check_for_update(current_version: &str, force: bool) -> anyhow::Result<UpdateCheckResult> {
    let path = state_path().ok_or_else(|| anyhow::anyhow!("no update state path"))?;
    check_for_update_at(
        &path,
        current_version,
        force,
        Utc::now(),
        fetch_latest_release,
    )
}

pub fn check_for_update_at<F>(
    path: &Path,
    current_version: &str,
    force: bool,
    now: DateTime<Utc>,
    fetch: F,
) -> anyhow::Result<UpdateCheckResult>
where
    F: FnOnce() -> anyhow::Result<String>,
{
    parse_version(current_version).ok_or_else(|| anyhow::anyhow!("invalid current version"))?;
    let _guard = UPDATE_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let mut state = load_state_from(path);
    if !force && !check_is_due(&state, now) {
        return Ok(result_from_state(current_version, &state, false));
    }

    let release = parse_latest_release(&fetch()?)?;
    state.last_checked_at = Some(now.to_rfc3339());
    state.latest_release = Some(release);
    save_state_to(path, &state)?;
    Ok(result_from_state(current_version, &state, true))
}

pub fn claim_notification(version: &str) -> anyhow::Result<bool> {
    let path = state_path().ok_or_else(|| anyhow::anyhow!("no update state path"))?;
    claim_notification_at(&path, version)
}

pub fn claim_notification_at(path: &Path, version: &str) -> anyhow::Result<bool> {
    parse_version(version).ok_or_else(|| anyhow::anyhow!("invalid notification version"))?;
    let _guard = UPDATE_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let mut state = load_state_from(path);
    if state.last_notified_version.as_deref() == Some(version) {
        return Ok(false);
    }
    state.last_notified_version = Some(version.to_string());
    save_state_to(path, &state)?;
    Ok(true)
}

fn check_is_due(state: &UpdateState, now: DateTime<Utc>) -> bool {
    let Some(last_checked) = state
        .last_checked_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
    else {
        return true;
    };

    if last_checked > now {
        return true;
    }
    now - last_checked >= Duration::hours(CHECK_INTERVAL_HOURS)
}

fn result_from_state(
    current_version: &str,
    state: &UpdateState,
    checked_now: bool,
) -> UpdateCheckResult {
    let release = state.latest_release.as_ref();
    let status = release
        .and_then(|item| is_update_available(current_version, &item.version).ok())
        .map(|available| {
            if available {
                "update_available"
            } else {
                "current"
            }
        })
        .unwrap_or("unknown");

    UpdateCheckResult {
        status: status.into(),
        current_version: current_version.into(),
        latest_version: release.map(|item| item.version.clone()),
        release_url: release.map(|item| item.url.clone()),
        checked_at: state.last_checked_at.clone(),
        checked_now,
        error: None,
    }
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut parts = value.split('.');
    let major = parse_version_part(parts.next()?)?;
    let minor = parse_version_part(parts.next()?)?;
    let patch = parse_version_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn parse_version_part(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn fetch_latest_release() -> anyhow::Result<String> {
    let executable = if cfg!(windows) {
        windows_system_directory()?.join("curl.exe")
    } else {
        PathBuf::from("curl")
    };
    let mut command = Command::new(executable);
    command.args([
        "-q",
        "--silent",
        "--show-error",
        "--fail",
        "--max-time",
        HTTP_TIMEOUT_SECS,
        "--header",
        "Accept: application/vnd.github+json",
        "--header",
        "X-GitHub-Api-Version: 2022-11-28",
        "--user-agent",
        concat!("Juice/", env!("CARGO_PKG_VERSION")),
        GITHUB_LATEST_API,
    ]);
    collector::command_output_with_input(
        command,
        None,
        StdDuration::from_secs(6),
        "GitHub release check",
    )
}

#[cfg(windows)]
fn windows_system_directory() -> anyhow::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0u16; 260];
    loop {
        let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if length == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if length < buffer.len() {
            return Ok(std::ffi::OsString::from_wide(&buffer[..length]).into());
        }
        buffer.resize(length + 1, 0);
    }
}

#[cfg(not(windows))]
fn windows_system_directory() -> anyhow::Result<PathBuf> {
    anyhow::bail!("Windows system directory is unavailable")
}

fn save_state_to(path: &Path, state: &UpdateState) -> anyhow::Result<()> {
    save_state_to_with(path, state, replace_state_file)
}

fn save_state_to_with(
    path: &Path,
    state: &UpdateState,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), sequence));
    std::fs::write(&temp, serde_json::to_vec_pretty(state)?)?;
    match replace(path, &temp) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error.into())
        }
    }
}

fn replace_state_file(path: &Path, temp: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return std::fs::rename(temp, path);
    }
    replace_existing_state_file(path, temp)
}

#[cfg(windows)]
fn replace_existing_state_file(path: &Path, temp: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS},
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let target = wide(path);
    let replacement = wide(temp);
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(not(windows))]
fn replace_existing_state_file(path: &Path, temp: &Path) -> std::io::Result<()> {
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_state_replace_preserves_old_bytes_and_cleans_temp() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-update-state-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("update-state.json");
        let original = br#"{"last_checked_at":"old"}"#;
        std::fs::write(&path, original).unwrap();

        let result = save_state_to_with(&path, &UpdateState::default(), |_, _| {
            Err(std::io::Error::other("injected replace failure"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains("json.tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn system_directory_is_absolute_and_not_environment_derived() {
        let directory = windows_system_directory().unwrap();
        assert!(directory.is_absolute());
        assert!(directory.is_dir());
        assert!(directory.join("curl.exe").is_file());
    }
}
