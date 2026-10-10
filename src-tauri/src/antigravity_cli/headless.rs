use super::{headless_report, CaptureError};
use crate::model::AgentStatus;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

pub const SESSION_ID: &str = "antigravity-cli-account";
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const OUTPUT_CAP: usize = 64 * 1024;
const CHANGELOG_CAP: usize = 512 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct RuntimeIdentity {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
}

static CAPABILITY: Mutex<Option<(RuntimeIdentity, bool)>> = Mutex::new(None);

#[cfg(test)]
fn supported_changelog(body: &[u8]) -> bool {
    changelog_capability(body).unwrap_or(false)
}

fn changelog_capability(body: &[u8]) -> Option<bool> {
    let Ok(text) = std::str::from_utf8(body) else {
        return None;
    };
    let first = text.lines().find(|line| !line.trim().is_empty())?;
    let version = first.trim().strip_suffix(':')?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let Ok(major) = parts[0].parse::<u32>() else {
        return None;
    };
    let Ok(minor) = parts[1].parse::<u32>() else {
        return None;
    };
    let Ok(patch) = parts[2].parse::<u32>() else {
        return None;
    };
    Some((major, minor, patch) >= (1, 1, 11))
}

fn remember_capability(
    state: &Mutex<Option<(RuntimeIdentity, bool)>>,
    identity: RuntimeIdentity,
    success: bool,
    body: &[u8],
) -> Result<bool, CaptureError> {
    if !success {
        return Err(CaptureError::Unavailable);
    }
    let supported = changelog_capability(body).ok_or(CaptureError::Unavailable)?;
    *state.lock().unwrap_or_else(|error| error.into_inner()) = Some((identity, supported));
    Ok(supported)
}

#[cfg(windows)]
fn checked_runtime() -> Result<RuntimeIdentity, CaptureError> {
    let path = dirs::data_local_dir()
        .ok_or(CaptureError::AppRequired)?
        .join("agy")
        .join("bin")
        .join("agy.exe");
    let mut ancestor = PathBuf::new();
    for part in path.components() {
        ancestor.push(part);
        if matches!(part, std::path::Component::Prefix(_)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&ancestor).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CaptureError::AppRequired
            } else {
                CaptureError::Unavailable
            }
        })?;
        use std::os::windows::fs::MetadataExt;
        if metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0 {
            return Err(CaptureError::Unavailable);
        }
    }
    let metadata = std::fs::metadata(&path).map_err(|_| CaptureError::Unavailable)?;
    if !metadata.is_file() {
        return Err(CaptureError::Unavailable);
    }
    Ok(RuntimeIdentity {
        path,
        length: metadata.len(),
        modified: metadata.modified().map_err(|_| CaptureError::Unavailable)?,
    })
}

#[cfg(windows)]
fn workspace() -> Result<PathBuf, CaptureError> {
    let path = crate::paths::data_dir()
        .ok_or(CaptureError::Unavailable)?
        .join("antigravity-usage-workspace");
    super::checked_path(&path).map_err(|_| CaptureError::Unavailable)?;
    std::fs::create_dir_all(&path).map_err(|_| CaptureError::Unavailable)?;
    super::checked_path(&path).map_err(|_| CaptureError::Unavailable)?;
    Ok(path)
}

#[cfg(any(windows, test))]
fn quota_command(executable: &Path, directory: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(directory)
        .args([
            "--print",
            "/usage",
            "--output-format",
            "json",
            "--print-timeout",
            "10s",
            "--log-file",
            "NUL",
        ])
        .env("AGY_CLI_HIDE_ACCOUNT_INFO", "1");
    command
}

#[cfg(windows)]
pub fn collect(
    pc_id: &str,
    captured_at: &str,
    deadline: Instant,
) -> Result<AgentStatus, CaptureError> {
    if Instant::now() >= deadline {
        return Err(CaptureError::Unavailable);
    }
    let runtime = checked_runtime()?;
    let directory = workspace()?;
    let known = CAPABILITY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .filter(|(identity, _)| identity == &runtime)
        .map(|(_, supported)| *supported);
    let supported = match known {
        Some(value) => value,
        None => {
            if Instant::now() >= deadline {
                return Err(CaptureError::Unavailable);
            }
            let mut command = Command::new(&runtime.path);
            command.current_dir(&directory).arg("changelog");
            let output = crate::collector::captured_command_output_with_input_caps(
                command,
                None,
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(3)),
                "Antigravity CLI capability",
                CHANGELOG_CAP,
                16 * 1024,
            )
            .map_err(|_| CaptureError::Unavailable)?;
            if checked_runtime()? != runtime {
                return Err(CaptureError::Unavailable);
            }
            remember_capability(&CAPABILITY, runtime.clone(), output.success, &output.stdout)?
        }
    };
    if !supported || Instant::now() >= deadline {
        return Err(CaptureError::Unavailable);
    }
    let output = crate::collector::captured_command_output_with_input_caps(
        quota_command(&runtime.path, &directory),
        None,
        deadline.saturating_duration_since(Instant::now()),
        "Antigravity CLI quota",
        OUTPUT_CAP,
        16 * 1024,
    )
    .map_err(|_| CaptureError::Unavailable)?;
    if checked_runtime()? != runtime {
        return Err(CaptureError::Unavailable);
    }
    let result = headless_report::parse_report(&output.stdout, pc_id, captured_at);
    if output.success {
        return result;
    }
    if result.as_ref().err() == Some(&CaptureError::LoginRequired)
        || crate::collector::text_requires_login(&String::from_utf8_lossy(&output.stderr))
    {
        return Err(CaptureError::LoginRequired);
    }
    Err(CaptureError::Unavailable)
}

#[cfg(not(windows))]
pub fn collect(
    _pc_id: &str,
    _captured_at: &str,
    _deadline: Instant,
) -> Result<AgentStatus, CaptureError> {
    Err(CaptureError::AppRequired)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_pre_readonly_releases_are_rejected_before_quota_execution() {
        for text in [
            "",
            "latest",
            "1.1.10:\nnotes",
            "0.9.99:\nnotes",
            "1.1.x:",
            "1.1.11.0:",
        ] {
            assert!(!supported_changelog(text.as_bytes()));
        }
        for text in [
            "1.1.11:\nnotes",
            "1.2.17:\nnotes",
            "1.3.1:\nnotes",
            "\n1.3.1:\nnotes",
        ] {
            assert!(supported_changelog(text.as_bytes()));
        }
    }

    #[test]
    fn failed_or_unrecognized_capability_reads_retry_without_a_permanent_negative_cache() {
        let state = Mutex::new(None);
        let identity = RuntimeIdentity {
            path: PathBuf::from("fixture.exe"),
            length: 1,
            modified: SystemTime::UNIX_EPOCH,
        };
        assert!(remember_capability(&state, identity.clone(), false, b"1.3.1:\nnotes").is_err());
        assert!(state.lock().unwrap().is_none());
        assert!(remember_capability(&state, identity.clone(), true, b"temporary error").is_err());
        assert!(state.lock().unwrap().is_none());
        assert_eq!(
            remember_capability(&state, identity.clone(), true, b"1.3.1:\nnotes"),
            Ok(true)
        );
        assert!(state.lock().unwrap().as_ref().unwrap().1);
        assert_eq!(
            remember_capability(&state, identity, true, b"1.1.10:\nnotes"),
            Ok(false)
        );
        assert!(!state.lock().unwrap().as_ref().unwrap().1);
    }
    #[test]
    fn quota_command_never_accepts_a_model_prompt_or_shell_interpolation() {
        let command = quota_command(Path::new("fixture.exe"), Path::new("fixture-workspace"));
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "--print",
                "/usage",
                "--output-format",
                "json",
                "--print-timeout",
                "10s",
                "--log-file",
                "NUL"
            ]
        );
        assert_eq!(command.get_program(), "fixture.exe");
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new("fixture-workspace"))
        );
    }

    #[test]
    #[ignore = "requires explicit JUICE_TEST_ANTIGRAVITY_HEADLESS=1 and the existing CLI login"]
    fn live_readonly_quota_roundtrip_without_statusline_or_terminal() {
        assert_eq!(
            std::env::var("JUICE_TEST_ANTIGRAVITY_HEADLESS").as_deref(),
            Ok("1")
        );
        let captured = chrono::Utc::now().to_rfc3339();
        let status = collect(
            "live-fixture",
            &captured,
            Instant::now() + Duration::from_secs(15),
        )
        .expect("read-only CLI account quota");
        assert_eq!(status.session_id, SESSION_ID);
        assert!(!status.approx);
        assert!(status.primary.is_some() || status.secondary.is_some());
        assert!(status.session.context_used_percent.is_none());
    }
}
