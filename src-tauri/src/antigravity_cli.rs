pub mod binding;
pub mod headless;
mod headless_report;
#[cfg(windows)]
mod native;
mod original;
pub mod status;

use crate::model::{AgentStatus, Tool};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use status::CaptureError;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
use zeroize::Zeroizing;

static CONNECTION_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn connection_failed() -> bool {
    CONNECTION_FAILED.load(std::sync::atomic::Ordering::Acquire)
}
pub fn set_connection_failed(failed: bool) {
    CONNECTION_FAILED.store(failed, std::sync::atomic::Ordering::Release);
}

const SCHEMA: &str = "antigravity_cli.v1";
const MAX_RECORD_BYTES: usize = 32 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_SOURCES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerProof {
    pub pid: u32,
    pub created: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    schema_version: String,
    binding_id: String,
    producer: ProducerProof,
    captured_at: String,
    account_hash: Option<String>,
    status: Option<AgentStatus>,
    error: Option<CaptureError>,
}

#[cfg(windows)]
static SELECTED: Mutex<Option<(String, native::Lease)>> = Mutex::new(None);

pub fn clear_cached_source() {
    #[cfg(windows)]
    {
        *SELECTED.lock().unwrap_or_else(|err| err.into_inner()) = None;
    }
}

pub fn cached_source_alive(status: &AgentStatus) -> bool {
    if !status.session_id.starts_with("antigravity-cli:") {
        return true;
    }
    #[cfg(windows)]
    {
        SELECTED
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
            .is_some_and(|(id, lease)| id == &status.session_id && lease.alive())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn capture_dir(data: &Path, binding_id: &str) -> PathBuf {
    data.join("antigravity-cli-captures").join(binding_id)
}

fn valid_binding_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_capture_name(name: &str) -> bool {
    name.strip_prefix("cli.")
        .and_then(|s| s.strip_suffix(".json"))
        .is_some_and(|s| {
            let parts: Vec<_> = s.split('.').collect();
            parts.len() == 2
                && parts.iter().all(|p| {
                    !p.is_empty() && p.len() <= 20 && p.bytes().all(|b| b.is_ascii_digit())
                })
        })
}

fn checked_path(path: &Path) -> std::io::Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if matches!(part, std::path::Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(std::io::Error::other("redirected capture path"));
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Err(std::io::Error::other("redirected capture path"));
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn read_record(path: &Path) -> Result<Record, CaptureError> {
    checked_path(path).map_err(|_| CaptureError::Unavailable)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    let file = options.open(path).map_err(|_| CaptureError::Unavailable)?;
    let meta = file.metadata().map_err(|_| CaptureError::Unavailable)?;
    if !meta.is_file() || meta.len() > MAX_RECORD_BYTES as u64 {
        return Err(CaptureError::Unavailable);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(CaptureError::Unavailable);
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CaptureError::Unavailable)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(CaptureError::Unavailable);
    }
    serde_json::from_slice(&bytes).map_err(|_| CaptureError::Unavailable)
}

fn valid_record_status(status: &AgentStatus) -> bool {
    status.schema_version == "agent_status.v1"
        && status.tool == Tool::Antigravity
        && status.cost_estimate_usd.is_none()
        && status.session.context_used_percent.is_none()
        && (status.primary.is_some() || status.secondary.is_some())
        && [(&status.primary, "5h"), (&status.secondary, "week")]
            .into_iter()
            .all(|(limit, label)| {
                limit.as_ref().is_none_or(|limit| {
                    limit.label == label
                        && limit.used_percent.is_some_and(|value| {
                            value.is_finite() && (0.0..=100.0).contains(&value)
                        })
                        && limit
                            .resets_at
                            .as_ref()
                            .is_none_or(|reset| DateTime::parse_from_rfc3339(reset).is_ok())
                })
            })
}

#[cfg(windows)]
fn prune_retired(dir: &Path, current: &Path, binding_id: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.take(MAX_ENTRIES).flatten() {
        let path = entry.path();
        if path == current || !is_capture_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        if let Ok(record) = read_record(&path) {
            if record.schema_version == SCHEMA
                && record.binding_id == binding_id
                && native::retired(&record.producer)
            {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn select_record(
    dir: &Path,
    binding_id: &str,
    mut clock: impl FnMut() -> DateTime<Utc>,
    mut alive: impl FnMut(&ProducerProof) -> bool,
) -> Result<Record, CaptureError> {
    checked_path(dir).map_err(|_| CaptureError::Unavailable)?;
    let entries = fs::read_dir(dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CaptureError::AppRequired
        } else {
            CaptureError::Unavailable
        }
    })?;
    let mut records = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ENTRIES {
            return Err(CaptureError::Unavailable);
        }
        let entry = entry.map_err(|_| CaptureError::Unavailable)?;
        if !is_capture_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let record = read_record(&entry.path())?;
        if record.schema_version != SCHEMA
            || record.binding_id != binding_id
            || !alive(&record.producer)
        {
            continue;
        }
        let captured = DateTime::parse_from_rfc3339(&record.captured_at)
            .map_err(|_| CaptureError::Unavailable)?;
        // A callback can replace the record after collection starts but before this read.
        if captured.with_timezone(&Utc) > clock() {
            return Err(CaptureError::Unavailable);
        }
        if let Some(status) = &record.status {
            if !valid_record_status(status)
                || status.captured_at != record.captured_at
                || record.error.is_some()
                || !record.account_hash.as_ref().is_some_and(|hash| {
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                })
            {
                return Err(CaptureError::Unavailable);
            }
        } else if !matches!(
            record.error,
            Some(CaptureError::LoginRequired | CaptureError::Unavailable)
        ) {
            return Err(CaptureError::Unavailable);
        }
        records.push(record);
        if records.len() > MAX_SOURCES {
            return Err(CaptureError::Unavailable);
        }
    }
    if records.is_empty() {
        return Err(CaptureError::AppRequired);
    }
    if records
        .iter()
        .any(|r| r.error == Some(CaptureError::Unavailable))
    {
        return Err(CaptureError::Unavailable);
    }
    let hashes: std::collections::BTreeSet<_> = records
        .iter()
        .filter_map(|r| r.status.as_ref().and(r.account_hash.as_ref()))
        .collect();
    if hashes.len() > 1 {
        return Err(CaptureError::Unavailable);
    }
    records.sort_by_cached_key(|r| {
        std::cmp::Reverse(DateTime::parse_from_rfc3339(&r.captured_at).ok())
    });
    records
        .into_iter()
        .find(|r| r.status.is_some())
        .ok_or(CaptureError::LoginRequired)
}

pub fn collect(pc_id: &str, stale_after_secs: i64) -> Result<AgentStatus, CaptureError> {
    #[cfg(windows)]
    {
        clear_cached_source();
        let owned = binding::load_owned()
            .map_err(|_| CaptureError::Unavailable)?
            .ok_or(CaptureError::AppRequired)?;
        if !valid_binding_id(&owned.id) {
            return Err(CaptureError::Unavailable);
        }
        let data = crate::paths::data_dir().ok_or(CaptureError::Unavailable)?;
        let mut record = select_record(
            &capture_dir(&data, &owned.id),
            &owned.id,
            Utc::now,
            |proof| native::verify(proof).is_ok(),
        )?;
        let lease = native::verify(&record.producer)?;
        let mut status = record.status.take().ok_or(CaptureError::Unavailable)?;
        status.pc_id = pc_id.into();
        status.session_id = format!(
            "antigravity-cli:{}:{}",
            record.producer.pid, record.producer.created
        );
        let age = (Utc::now()
            - DateTime::parse_from_rfc3339(&record.captured_at)
                .map_err(|_| CaptureError::Unavailable)?
                .with_timezone(&Utc))
        .num_seconds();
        status.session.active = age >= 0 && age <= stale_after_secs.max(1);
        let current = binding::load_owned()
            .map_err(|_| CaptureError::Unavailable)?
            .ok_or(CaptureError::Unavailable)?;
        if current.id != owned.id {
            return Err(CaptureError::Unavailable);
        }
        *SELECTED.lock().unwrap_or_else(|err| err.into_inner()) =
            Some((status.session_id.clone(), lease));
        Ok(status)
    }
    #[cfg(not(windows))]
    {
        let _ = (pc_id, stale_after_secs);
        Err(CaptureError::Unavailable)
    }
}

pub fn run_statusline(input: &[u8]) -> Vec<u8> {
    #[cfg(windows)]
    {
        let (Some(home), Some(data)) = (dirs::home_dir(), crate::paths::data_dir()) else {
            return Vec::new();
        };
        run_statusline_at(&home, &data, input, native::producer)
    }
    #[cfg(not(windows))]
    {
        let _ = input;
        Vec::new()
    }
}

#[cfg(windows)]
fn run_statusline_at(
    home: &Path,
    data: &Path,
    input: &[u8],
    resolve_producer: impl FnOnce() -> Result<ProducerProof, CaptureError>,
) -> Vec<u8> {
    let Ok(Some(saved)) = binding::load_saved_at(home, data) else {
        return Vec::new();
    };
    let Ok(producer) = resolve_producer() else {
        return original::run_if_connected_at(&saved, input, home);
    };
    let enabled = crate::config::Settings::try_load_from(&data.join("settings.json"))
        .is_ok_and(|s| s.show_antigravity);
    if enabled
        && valid_binding_id(&saved.id)
        && binding::load_owned_at(home, data)
            .ok()
            .flatten()
            .is_some_and(|b| b.id == saved.id)
    {
        let now = Utc::now().to_rfc3339();
        let parsed = status::parse_payload(
            input,
            &saved.id,
            &gethostname::gethostname().to_string_lossy(),
            &now,
        );
        let (status, account_hash, error) = match parsed {
            Ok((status, hash)) => (Some(status), Some(hash), None),
            Err(error) => (None, None, Some(error)),
        };
        let record = Record {
            schema_version: SCHEMA.into(),
            binding_id: saved.id.clone(),
            producer,
            captured_at: now,
            account_hash,
            status,
            error,
        };
        let dir = capture_dir(data, &saved.id);
        if checked_path(&dir).is_ok() && fs::create_dir_all(&dir).is_ok() {
            let path = dir.join(format!("cli.{}.{}.json", producer.pid, producer.created));
            if checked_path(&path).is_ok()
                && native::verify(&producer).is_ok()
                && binding::load_owned_at(home, data)
                    .ok()
                    .flatten()
                    .is_some_and(|b| b.id == saved.id)
            {
                if let Ok(bytes) = serde_json::to_vec(&record) {
                    if crate::config::replace_file(&path, &bytes).is_ok() {
                        prune_retired(&dir, &path, &saved.id);
                    }
                }
            }
        }
    }
    original::run_if_connected_at(&saved, input, home)
}

pub fn read_input(reader: impl Read) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take(crate::statusline::MAX_STATUSLINE_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > crate::statusline::MAX_STATUSLINE_INPUT_BYTES {
        return Err(std::io::Error::other("statusline input limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "juice-cli-record-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn record(pid: u32, email: &str, now: &str) -> Record {
        let bytes=serde_json::to_vec(&serde_json::json!({"product":"antigravity","email":email,"quota":{"gemini-weekly":{"remaining_fraction":0.5}}})).unwrap();
        let (status, hash) = status::parse_payload(&bytes, "binding", "pc", now).unwrap();
        Record {
            schema_version: SCHEMA.into(),
            binding_id: "binding".into(),
            producer: ProducerProof { pid, created: 42 },
            captured_at: now.into(),
            account_hash: Some(hash),
            status: Some(status),
            error: None,
        }
    }
    fn write(dir: &Path, record: &Record) {
        fs::write(
            dir.join(format!(
                "cli.{}.{}.json",
                record.producer.pid, record.producer.created
            )),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn cli_multiple_accounts_fail_closed_but_same_account_keeps_latest_snapshot() {
        let dir = fixture();
        let now = Utc::now();
        let stamp = now.to_rfc3339();
        write(&dir, &record(1, "a@example.invalid", &stamp));
        write(&dir, &record(2, "b@example.invalid", &stamp));
        assert_eq!(
            select_record(&dir, "binding", || now, |_| true).err(),
            Some(CaptureError::Unavailable)
        );
        let later = now - chrono::Duration::seconds(1);
        write(&dir, &record(2, "a@example.invalid", &later.to_rfc3339()));
        assert_eq!(
            select_record(&dir, "binding", || now, |_| true)
                .unwrap()
                .producer
                .pid,
            1
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cli_closed_producer_old_binding_and_logout_do_not_restore_previous_values() {
        let dir = fixture();
        let now = Utc::now();
        let mut r = record(1, "a@example.invalid", &now.to_rfc3339());
        write(&dir, &r);
        assert!(select_record(&dir, "binding", || now, |_| false).is_err());
        assert!(select_record(&dir, "new-binding", || now, |_| true).is_err());
        r.status = None;
        r.account_hash = None;
        r.error = Some(CaptureError::LoginRequired);
        write(&dir, &r);
        assert_eq!(
            select_record(&dir, "binding", || now, |_| true).err(),
            Some(CaptureError::LoginRequired)
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cli_future_and_inconsistent_capture_fail_closed_and_private_data_is_absent() {
        let dir = fixture();
        let now = Utc::now();
        let mut r = record(
            1,
            "private@example.invalid",
            &(now + chrono::Duration::seconds(1)).to_rfc3339(),
        );
        write(&dir, &r);
        assert_eq!(
            select_record(&dir, "binding", || now, |_| true).err(),
            Some(CaptureError::Unavailable)
        );
        r.captured_at = now.to_rfc3339();
        write(&dir, &r);
        assert_eq!(
            select_record(&dir, "binding", || now, |_| true).err(),
            Some(CaptureError::Unavailable)
        );
        let bytes = serde_json::to_string(&r).unwrap();
        assert!(!bytes.contains("private@example.invalid"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cli_record_written_after_collection_entry_uses_post_read_clock() {
        let dir = fixture();
        let entered_at = Utc::now();
        let received_at = entered_at + chrono::Duration::milliseconds(5);
        let record = record(1, "fixture@example.invalid", &received_at.to_rfc3339());
        write(&dir, &record);
        let mut clock_reads = 0;
        let selected = select_record(
            &dir,
            "binding",
            || {
                clock_reads += 1;
                assert!(read_record(&dir.join("cli.1.42.json")).is_ok());
                entered_at + chrono::Duration::milliseconds(10)
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(clock_reads, 1);
        assert_eq!(selected.captured_at, received_at.to_rfc3339());
        assert_eq!(
            selected.status.unwrap().captured_at,
            received_at.to_rfc3339()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cli_post_read_clock_still_rejects_a_genuinely_future_capture() {
        let dir = fixture();
        let entered_at = Utc::now();
        write(
            &dir,
            &record(
                1,
                "fixture@example.invalid",
                &(entered_at + chrono::Duration::seconds(30)).to_rfc3339(),
            ),
        );
        assert_eq!(
            select_record(
                &dir,
                "binding",
                || entered_at + chrono::Duration::milliseconds(10),
                |_| true,
            )
            .err(),
            Some(CaptureError::Unavailable)
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cli_input_is_bounded() {
        assert!(read_input(&b"{}"[..]).is_ok());
        let data = vec![b' '; crate::statusline::MAX_STATUSLINE_INPUT_BYTES + 1];
        assert!(read_input(data.as_slice()).is_err());
    }

    #[cfg(windows)]
    fn connected_fixture(
        original: serde_json::Value,
        enabled: bool,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let root = fixture();
        let home = root.join("home");
        let data = root.join("data");
        let settings_path = home.join(".gemini/antigravity-cli/settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        fs::write(settings_path, serde_json::to_vec(&original).unwrap()).unwrap();
        let helper = root.join("fixture.exe");
        fs::write(&helper, b"not executed").unwrap();
        binding::install_at(&home, &data, &helper).unwrap();
        let settings = crate::config::Settings {
            show_antigravity: enabled,
            ..crate::config::Settings::default()
        };
        fs::write(
            data.join("settings.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        (root, home, data)
    }

    #[cfg(windows)]
    #[test]
    fn cli_failed_producer_preserves_original_stdout_without_capture() {
        for enabled in [true, false] {
            let (root, home, data) = connected_fixture(
                serde_json::json!({"statusLine":{"type":"command","command":"echo original-output"}}),
                enabled,
            );
            assert_eq!(
                run_statusline_at(&home, &data, b"{}", || Err(CaptureError::Unavailable)),
                b"original-output\r\n"
            );
            assert!(!data.join("antigravity-cli-captures").exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn cli_failed_producer_keeps_changed_disabled_and_absent_original_guards() {
        for case in ["changed", "disabled", "absent"] {
            let original = match case {
                "absent" => serde_json::json!({}),
                "disabled" => {
                    serde_json::json!({"statusLine":{"command":"echo original-output","enabled":false}})
                }
                _ => serde_json::json!({"statusLine":{"command":"echo original-output"}}),
            };
            let (root, home, data) = connected_fixture(original, true);
            if case == "changed" {
                let path = home.join(".gemini/antigravity-cli/settings.json");
                let mut settings: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                settings["statusLine"]["command"] = "echo changed-command".into();
                fs::write(path, serde_json::to_vec(&settings).unwrap()).unwrap();
            }
            assert!(
                run_statusline_at(&home, &data, b"{}", || Err(CaptureError::Unavailable))
                    .is_empty()
            );
            assert!(!data.join("antigravity-cli-captures").exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn cli_restored_binding_forwards_original_after_failed_producer() {
        let (root, home, data) = connected_fixture(
            serde_json::json!({"statusLine":{"command":"echo original-output"}}),
            false,
        );
        binding::restore_at(&home, &data).unwrap();
        assert_eq!(
            run_statusline_at(&home, &data, b"{}", || Err(CaptureError::Unavailable)),
            b"original-output\r\n"
        );
        assert!(!data.join("antigravity-cli-captures").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn cli_failed_producer_forwards_unchanged_stdin_to_original() {
        let (root, home, data) = connected_fixture(
            serde_json::json!({"statusLine":{"command":"findstr /r \".*\""}}),
            true,
        );
        let input = b"fixture input\r\n";
        assert_eq!(
            run_statusline_at(&home, &data, input, || Err(CaptureError::Unavailable)),
            input
        );
        assert!(!data.join("antigravity-cli-captures").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn cli_failed_producer_recheck_preserves_original_but_never_writes_snapshot() {
        let (root, home, data) = connected_fixture(
            serde_json::json!({"statusLine":{"command":"echo original-output"}}),
            true,
        );
        let id = binding::load_owned_at(&home, &data)
            .unwrap()
            .unwrap()
            .id
            .clone();
        let input = br#"{"product":"antigravity","email":"fixture@example.invalid","quota":{"gemini-5h":{"remaining_fraction":0.5}}}"#;
        assert_eq!(
            run_statusline_at(&home, &data, input, || Ok(ProducerProof {
                pid: u32::MAX,
                created: 0
            })),
            b"original-output\r\n"
        );
        let dir = capture_dir(&data, &id);
        assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
