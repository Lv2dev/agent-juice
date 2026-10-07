use crate::{config, paths};
use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Local, Offset, TimeZone, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

const SCHEMA_VERSION: &str = "usage_activity.v3";
const INDEX_FILE_NAME: &str = "usage-activity-v1.json";
const RETENTION_DAYS: i64 = 371;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const TAIL_FINGERPRINT_BYTES: u64 = 4096;
const DEFAULT_MAX_FILES: usize = 4096;
const DEFAULT_MAX_ENTRIES: usize = 32_768;
const DEFAULT_MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILE_BYTES_PER_PASS: u64 = 32 * 1024 * 1024;
const DEFAULT_SCAN_DURATION: Duration = Duration::from_secs(3);
const MAX_ENUMERATION_DEPTH: usize = 128;

static SCAN_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static ENUMERATION_READERS: Lazy<Mutex<[Vec<OpenDirectory>; 3]>> =
    Lazy::new(|| Mutex::new(std::array::from_fn(|_| Vec::new())));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ActivityDay {
    pub date: String,
    pub claude_tokens: u64,
    pub codex_tokens: u64,
    #[serde(default)]
    pub grok_tokens: u64,
    #[serde(default)]
    pub cursor_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub schema_version: String,
    pub generated_at: String,
    pub timezone_offset_minutes: i32,
    pub partial: bool,
    pub backfill_pending: bool,
    #[serde(default)]
    pub local_partial: bool,
    #[serde(default)]
    pub local_backfill_pending: bool,
    #[serde(default)]
    pub codex_partial: bool,
    #[serde(default)]
    pub codex_backfill_pending: bool,
    #[serde(default)]
    pub codex_account_scope: bool,
    #[serde(default)]
    pub cursor_partial: bool,
    #[serde(default)]
    pub cursor_backfill_pending: bool,
    #[serde(default)]
    pub cursor_account_scope: bool,
    pub days: Vec<ActivityDay>,
}

#[derive(Debug, Clone)]
pub struct ActivityRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub grok: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub max_files: usize,
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_duration: Duration,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_FILES,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_bytes: DEFAULT_MAX_BYTES,
            max_duration: DEFAULT_SCAN_DURATION,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ActivityTool {
    Claude,
    Codex,
    Grok,
}

impl ActivityTool {
    const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Grok];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ClaudeMessageContribution {
    date: String,
    tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GrokResponseContribution {
    date: String,
    tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PrefixValidation {
    offset: u64,
    digest: [u8; 32],
    target_offset: u64,
    source_modified_nanos: u64,
    legacy_prefix_matches: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FileCheckpoint {
    tool: ActivityTool,
    offset: u64,
    observed_len: u64,
    modified_millis: u64,
    #[serde(default)]
    source_modified_nanos: u64,
    tail_fingerprint: u64,
    #[serde(default)]
    anonymous_source_verified: bool,
    #[serde(default)]
    ambiguous_stable_ids: BTreeSet<String>,
    #[serde(default)]
    prefix_digest: Option<[u8; 32]>,
    #[serde(default)]
    prefix_needs_validation: bool,
    #[serde(default)]
    prefix_validation: Option<PrefixValidation>,
    #[serde(default)]
    lossy: bool,
    #[serde(default)]
    days: BTreeMap<String, u64>,
    #[serde(default)]
    claude_messages: BTreeMap<String, ClaudeMessageContribution>,
    #[serde(default)]
    grok_responses: BTreeMap<String, GrokResponseContribution>,
    #[serde(default)]
    codex_last_total: Option<u64>,
    #[serde(default)]
    codex_fallback_total: u64,
}

impl FileCheckpoint {
    fn new(tool: ActivityTool) -> Self {
        Self {
            tool,
            offset: 0,
            observed_len: 0,
            modified_millis: 0,
            source_modified_nanos: 0,
            tail_fingerprint: 0,
            anonymous_source_verified: false,
            ambiguous_stable_ids: BTreeSet::new(),
            prefix_digest: Some([0; 32]),
            prefix_needs_validation: false,
            prefix_validation: None,
            lossy: false,
            days: BTreeMap::new(),
            claude_messages: BTreeMap::new(),
            grok_responses: BTreeMap::new(),
            codex_last_total: None,
            codex_fallback_total: 0,
        }
    }

    fn prune(&mut self, cutoff: &str) {
        self.days.retain(|date, _| date.as_str() >= cutoff);
        self.claude_messages
            .retain(|_, contribution| contribution.date.as_str() >= cutoff);
        self.grok_responses
            .retain(|_, contribution| contribution.date.as_str() >= cutoff);
        self.ambiguous_stable_ids.retain(|id| {
            self.claude_messages.contains_key(id) || self.grok_responses.contains_key(id)
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ActivityTimezoneIdentity {
    Fixed {
        offset_seconds: i32,
    },
    Local {
        key: String,
        dynamic_daylight_disabled: bool,
    },
}

#[cfg(windows)]
fn local_timezone_identity() -> anyhow::Result<ActivityTimezoneIdentity> {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

    let timezone = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation")
        .context("read activity timezone identity")?;
    let key = timezone
        .get_value::<String, _>("TimeZoneKeyName")
        .or_else(|_| timezone.get_value::<String, _>("StandardName"))
        .context("read activity timezone key")?;
    let key = key.trim_matches('\0').trim().to_owned();
    anyhow::ensure!(!key.is_empty(), "activity timezone key unavailable");
    let dynamic_daylight_disabled = timezone
        .get_value::<u32, _>("DynamicDaylightTimeDisabled")
        .unwrap_or(0)
        != 0;
    Ok(ActivityTimezoneIdentity::Local {
        key,
        dynamic_daylight_disabled,
    })
}

#[cfg(not(windows))]
fn local_timezone_identity() -> anyhow::Result<ActivityTimezoneIdentity> {
    let key = match std::env::var("TZ") {
        Ok(value) => format!("TZ:{value}"),
        Err(_) => {
            let rules = fs::read("/etc/localtime").context("read activity timezone rules")?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            rules.hash(&mut hasher);
            format!("localtime:{:016x}", hasher.finish())
        }
    };
    Ok(ActivityTimezoneIdentity::Local {
        key,
        dynamic_daylight_disabled: false,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ActivityIndex {
    schema_version: String,
    timezone_offset_minutes: i32,
    #[serde(default)]
    timezone_identity: Option<ActivityTimezoneIdentity>,
    #[serde(default)]
    files: BTreeMap<String, FileCheckpoint>,
    #[serde(default)]
    legacy_files: BTreeMap<String, FileCheckpoint>,
    #[serde(default)]
    enumeration: EnumerationCheckpoint,
}

impl ActivityIndex {
    #[cfg(test)]
    fn new(timezone_offset_minutes: i32) -> Self {
        Self::with_timezone(
            timezone_offset_minutes,
            ActivityTimezoneIdentity::Fixed {
                offset_seconds: timezone_offset_minutes * 60,
            },
        )
    }

    fn with_timezone(
        timezone_offset_minutes: i32,
        timezone_identity: ActivityTimezoneIdentity,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.into(),
            timezone_offset_minutes,
            timezone_identity: Some(timezone_identity),
            files: BTreeMap::new(),
            legacy_files: BTreeMap::new(),
            enumeration: EnumerationCheckpoint::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CandidateFile {
    path: PathBuf,
    key: String,
    tool: ActivityTool,
    modified_millis: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct EnumerationCheckpoint {
    next_tool: usize,
    walks: [DirectoryWalk; 3],
    pending: VecDeque<CandidateFile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct DirectoryWalk {
    root: Option<PathBuf>,
    stack: Vec<DirectoryPosition>,
    complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DirectoryPosition {
    relative: PathBuf,
    offset: usize,
}

struct OpenDirectory {
    path: PathBuf,
    entries: fs::ReadDir,
    offset: usize,
}

#[derive(Debug, Default)]
struct ScanProgress {
    incomplete: bool,
    lossy: bool,
    bytes_read: u64,
}

pub fn index_path() -> Option<PathBuf> {
    paths::data_dir().map(|dir| dir.join(INDEX_FILE_NAME))
}

pub fn local_roots() -> ActivityRoots {
    let home = dirs::home_dir();
    ActivityRoots {
        claude: home
            .as_ref()
            .map(|path| path.join(".claude").join("projects")),
        codex: home
            .as_ref()
            .map(|path| path.join(".codex").join("sessions")),
        grok: home.map(|path| path.join(".grok").join("sessions")),
    }
}

pub fn refresh(show_claude: bool, show_grok: bool) -> anyhow::Result<ActivitySnapshot> {
    let _guard = SCAN_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let path = index_path().ok_or_else(|| anyhow::anyhow!("activity data path unavailable"))?;
    let mut snapshot = refresh_in_timezone_checked(
        &path,
        &local_roots(),
        show_claude,
        false,
        show_grok,
        Utc::now(),
        Local,
        local_timezone_identity()?,
        ScanOptions::default(),
        local_timezone_identity,
    )?;
    for day in &mut snapshot.days {
        day.codex_tokens = 0;
    }
    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
pub fn refresh_at(
    index_path: &Path,
    roots: &ActivityRoots,
    show_claude: bool,
    show_codex: bool,
    show_grok: bool,
    now: DateTime<Utc>,
    timezone: FixedOffset,
    options: ScanOptions,
) -> anyhow::Result<ActivitySnapshot> {
    refresh_in_timezone(
        index_path,
        roots,
        show_claude,
        show_codex,
        show_grok,
        now,
        timezone,
        ActivityTimezoneIdentity::Fixed {
            offset_seconds: timezone.local_minus_utc(),
        },
        options,
    )
}

#[allow(clippy::too_many_arguments)]
fn refresh_in_timezone<Tz: TimeZone + Copy>(
    index_path: &Path,
    roots: &ActivityRoots,
    show_claude: bool,
    show_codex: bool,
    show_grok: bool,
    now: DateTime<Utc>,
    timezone: Tz,
    timezone_identity: ActivityTimezoneIdentity,
    options: ScanOptions,
) -> anyhow::Result<ActivitySnapshot> {
    refresh_in_timezone_checked(
        index_path,
        roots,
        show_claude,
        show_codex,
        show_grok,
        now,
        timezone,
        timezone_identity.clone(),
        options,
        || Ok(timezone_identity.clone()),
    )
}

fn validate_timezone_identity(
    expected: &ActivityTimezoneIdentity,
    current: &mut impl FnMut() -> anyhow::Result<ActivityTimezoneIdentity>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        current()? == *expected,
        "activity timezone changed during scan"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn refresh_in_timezone_checked<Tz: TimeZone + Copy>(
    index_path: &Path,
    roots: &ActivityRoots,
    show_claude: bool,
    show_codex: bool,
    show_grok: bool,
    now: DateTime<Utc>,
    timezone: Tz,
    timezone_identity: ActivityTimezoneIdentity,
    options: ScanOptions,
    mut current_timezone: impl FnMut() -> anyhow::Result<ActivityTimezoneIdentity>,
) -> anyhow::Result<ActivitySnapshot> {
    validate_timezone_identity(&timezone_identity, &mut current_timezone)?;
    let offset_minutes = now
        .with_timezone(&timezone)
        .offset()
        .fix()
        .local_minus_utc()
        / 60;
    let cutoff = (now.with_timezone(&timezone).date_naive()
        - ChronoDuration::days(RETENTION_DAYS - 1))
    .format("%Y-%m-%d")
    .to_string();
    let (mut index, rebuilt) = load_index_for_timezone_since(
        index_path,
        offset_minutes,
        timezone_identity.clone(),
        &cutoff,
    );
    let original = index.clone();
    // Seasonal offset changes affect display metadata, not historical date rules.
    index.timezone_offset_minutes = offset_minutes;
    let deadline = Instant::now() + options.max_duration;

    let enabled_tools = [show_claude, show_codex, show_grok];
    let current_roots = [&roots.claude, &roots.codex, &roots.grok];
    let walks = &index.enumeration.walks;
    index.enumeration.pending.retain(|candidate| {
        ActivityTool::ALL
            .iter()
            .position(|tool| *tool == candidate.tool)
            .is_some_and(|i| {
                enabled_tools[i]
                    && current_roots[i].as_ref().is_some_and(|root| {
                        walks[i].root.as_ref() == Some(root)
                            && candidate
                                .path
                                .strip_prefix(root)
                                .ok()
                                .is_some_and(|relative| {
                                    !relative.as_os_str().is_empty()
                                        && relative.components().all(|component| {
                                            matches!(component, std::path::Component::Normal(_))
                                        })
                                })
                            && candidate.key == candidate.path.to_string_lossy()
                    })
            })
    });
    let enumeration_partial = if index.enumeration.pending.is_empty() {
        let mut readers = ENUMERATION_READERS
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let (candidates, partial) = collect_candidates(
            roots,
            enabled_tools,
            &cutoff,
            timezone,
            deadline,
            options,
            &mut index.enumeration,
            &mut readers,
        );
        index.enumeration.pending = fair_candidate_order(candidates).into();
        partial
    } else {
        true
    };
    validate_timezone_identity(&timezone_identity, &mut current_timezone)?;

    let mut progress = ScanProgress {
        incomplete: enumeration_partial,
        lossy: false,
        bytes_read: 0,
    };
    let seen_files = index
        .enumeration
        .pending
        .iter()
        .map(|candidate| candidate.key.clone())
        .collect::<BTreeSet<_>>();

    let mut files_scanned = 0;
    while !index.enumeration.pending.is_empty() {
        if Instant::now() >= deadline || progress.bytes_read >= options.max_bytes {
            progress.incomplete = true;
            break;
        }
        if files_scanned >= options.max_files {
            progress.incomplete = true;
            break;
        }
        let candidate = index.enumeration.pending.pop_front().unwrap();
        files_scanned += 1;
        let file_key = candidate.key.clone();
        if let Some(legacy) = index.legacy_files.get_mut(&file_key) {
            legacy.anonymous_source_verified = false;
        }
        let checkpoint = index
            .files
            .entry(candidate.key)
            .or_insert_with(|| FileCheckpoint::new(candidate.tool));
        if checkpoint.tool != candidate.tool {
            *checkpoint = FileCheckpoint::new(candidate.tool);
        }
        let file_progress = scan_file(
            &candidate.path,
            checkpoint,
            &cutoff,
            timezone,
            deadline,
            options
                .max_bytes
                .saturating_sub(progress.bytes_read)
                .min(MAX_FILE_BYTES_PER_PASS),
            index
                .legacy_files
                .get(&file_key)
                .and_then(|legacy| legacy.prefix_digest.map(|digest| (legacy.offset, digest))),
        );
        validate_timezone_identity(&timezone_identity, &mut current_timezone)?;
        match file_progress {
            Ok(file_progress) => {
                if let Some(legacy) = index.legacy_files.get_mut(&file_key) {
                    legacy.anonymous_source_verified = checkpoint.anonymous_source_verified;
                }
                progress.bytes_read = progress.bytes_read.saturating_add(file_progress.bytes_read);
                progress.incomplete |= file_progress.incomplete;
                progress.lossy |= file_progress.lossy;
            }
            Err(err) => {
                progress.incomplete = true;
                eprintln!("[activity] {}: {err:#}", candidate.path.display());
            }
        }
    }

    index.files.retain(|key, checkpoint| {
        checkpoint.prune(&cutoff);
        seen_files.contains(key)
            || !checkpoint.days.is_empty()
            || !checkpoint.claude_messages.is_empty()
            || !checkpoint.grok_responses.is_empty()
    });
    if !show_codex {
        index
            .files
            .retain(|_, checkpoint| checkpoint.tool != ActivityTool::Codex);
        index
            .legacy_files
            .retain(|_, checkpoint| checkpoint.tool != ActivityTool::Codex);
    }
    reconcile_legacy_history(&mut index, &cutoff);
    // Retained proofs stay uncertain when their source is no longer enumerable.
    progress.lossy |= index.files.values().any(|checkpoint| {
        (checkpoint.lossy || checkpoint.prefix_needs_validation)
            && match checkpoint.tool {
                ActivityTool::Claude => show_claude,
                ActivityTool::Codex => show_codex,
                ActivityTool::Grok => show_grok,
            }
    });
    progress.lossy |= index
        .legacy_files
        .values()
        .any(|checkpoint| match checkpoint.tool {
            ActivityTool::Claude => show_claude,
            ActivityTool::Codex => show_codex,
            ActivityTool::Grok => show_grok,
        });

    let snapshot = snapshot_from_index(
        &index,
        now,
        offset_minutes,
        progress.incomplete,
        progress.lossy,
        &cutoff,
    );
    validate_timezone_identity(&timezone_identity, &mut current_timezone)?;
    if rebuilt || index != original {
        save_index(index_path, &index)?;
    }
    Ok(snapshot)
}

fn fair_candidate_order(candidates: Vec<CandidateFile>) -> Vec<CandidateFile> {
    let first_tool = candidates
        .first()
        .and_then(|candidate| {
            ActivityTool::ALL
                .iter()
                .position(|tool| *tool == candidate.tool)
        })
        .unwrap_or(0);
    let newest_first = |left: &CandidateFile, right: &CandidateFile| {
        right
            .modified_millis
            .cmp(&left.modified_millis)
            .then_with(|| left.key.cmp(&right.key))
    };
    let mut queues = ActivityTool::ALL.map(|tool| {
        let mut tool_candidates = candidates
            .iter()
            .filter(|candidate| candidate.tool == tool)
            .cloned()
            .collect::<Vec<_>>();
        tool_candidates.sort_by(newest_first);
        VecDeque::from(tool_candidates)
    });
    let mut ordered = Vec::with_capacity(candidates.len());
    while queues.iter().any(|queue| !queue.is_empty()) {
        for offset in 0..queues.len() {
            if let Some(candidate) = queues[(first_tool + offset) % 3].pop_front() {
                ordered.push(candidate);
            }
        }
    }
    ordered
}

#[cfg(test)]
fn load_index(path: &Path, offset_minutes: i32) -> (ActivityIndex, bool) {
    load_index_for_timezone(
        path,
        offset_minutes,
        ActivityTimezoneIdentity::Fixed {
            offset_seconds: offset_minutes * 60,
        },
    )
}

#[cfg(test)]
fn load_index_for_timezone(
    path: &Path,
    offset_minutes: i32,
    timezone_identity: ActivityTimezoneIdentity,
) -> (ActivityIndex, bool) {
    load_index_for_timezone_since(path, offset_minutes, timezone_identity, "")
}

fn load_index_for_timezone_since(
    path: &Path,
    offset_minutes: i32,
    timezone_identity: ActivityTimezoneIdentity,
    cutoff: &str,
) -> (ActivityIndex, bool) {
    let loaded = fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<ActivityIndex>(&contents).ok())
        .filter(|index| index.schema_version == SCHEMA_VERSION);
    match loaded {
        Some(mut index) => {
            if index.timezone_identity.as_ref() == Some(&timezone_identity) {
                return (index, false);
            }
            // Unknown historical dates remain available until their source can be reread.
            for (key, mut checkpoint) in std::mem::take(&mut index.files) {
                checkpoint.anonymous_source_verified = false;
                match index.legacy_files.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(checkpoint);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let retained = entry.get_mut();
                        // Offset IDs from separate rereads are not independent events.
                        // Reset checkpoints have no observed revision, even with offset == len.
                        let replace_anonymous = checkpoint.offset == checkpoint.observed_len
                            && checkpoint.source_modified_nanos > 0
                            && !checkpoint.lossy
                            && !checkpoint.prefix_needs_validation;
                        if has_aggregate_only_history(retained)
                            || has_aggregate_only_history(&checkpoint)
                        {
                            // Unknown overlap makes these alternative histories, not a union.
                            // Keep one whole date map so timezone shifts cannot double it.
                            let retained_days = checkpoint_history_days(retained, cutoff);
                            let candidate_days = checkpoint_history_days(&checkpoint, cutoff);
                            let total = |days: &BTreeMap<String, u64>| {
                                days.values()
                                    .map(|tokens| u128::from(*tokens))
                                    .sum::<u128>()
                            };
                            let retained_total = total(&retained_days);
                            let candidate_total = total(&candidate_days);
                            if (replace_anonymous || has_aggregate_only_history(&checkpoint))
                                && (candidate_total > retained_total
                                    || (candidate_total == retained_total
                                        && has_aggregate_only_history(retained)
                                        && !has_aggregate_only_history(&checkpoint)))
                            {
                                *retained = checkpoint;
                            }
                            retained.prune(cutoff);
                            retained.anonymous_source_verified = false;
                            continue;
                        }
                        if replace_anonymous {
                            if has_anonymous_contributions(&checkpoint) {
                                // Stable IDs in the old epoch may be the new anonymous events.
                                retained.ambiguous_stable_ids.extend(
                                    retained
                                        .claude_messages
                                        .keys()
                                        .chain(retained.grok_responses.keys())
                                        .filter(|id| !id.starts_with("offset:"))
                                        .cloned(),
                                );
                            }
                            retained.claude_messages.retain(|id, contribution| {
                                if id.starts_with("offset:") {
                                    subtract_day(
                                        &mut retained.days,
                                        &contribution.date,
                                        contribution.tokens,
                                    );
                                    false
                                } else {
                                    true
                                }
                            });
                            retained
                                .grok_responses
                                .retain(|id, _| !id.starts_with("offset:"));
                            retained.offset = checkpoint.offset;
                            retained.tail_fingerprint = checkpoint.tail_fingerprint;
                            retained.observed_len = checkpoint.observed_len;
                            retained.prefix_digest = checkpoint.prefix_digest;
                        }
                        retained.anonymous_source_verified = false;
                        // New stable IDs may describe the retained anonymous events.
                        let merge_new_ids = replace_anonymous
                            || !(retained
                                .claude_messages
                                .keys()
                                .any(|id| id.starts_with("offset:"))
                                || retained
                                    .grok_responses
                                    .keys()
                                    .any(|id| id.starts_with("offset:")));
                        if checkpoint.claude_messages.is_empty() {
                            for (date, tokens) in checkpoint.days {
                                let previous = retained.days.entry(date).or_default();
                                *previous = (*previous).max(tokens);
                            }
                        }
                        for (id, mut contribution) in checkpoint.claude_messages {
                            if (id.starts_with("offset:") && !replace_anonymous)
                                || (!merge_new_ids && !retained.claude_messages.contains_key(&id))
                            {
                                continue;
                            }
                            if let Some(previous) = retained.claude_messages.get(&id) {
                                contribution.tokens = contribution.tokens.max(previous.tokens);
                                subtract_day(&mut retained.days, &previous.date, previous.tokens);
                            }
                            add_day(&mut retained.days, &contribution.date, contribution.tokens);
                            retained.ambiguous_stable_ids.remove(&id);
                            retained.claude_messages.insert(id, contribution);
                        }
                        for (id, contribution) in checkpoint.grok_responses {
                            if (id.starts_with("offset:") && !replace_anonymous)
                                || (!merge_new_ids && !retained.grok_responses.contains_key(&id))
                            {
                                continue;
                            }
                            retained.ambiguous_stable_ids.remove(&id);
                            retained.grok_responses.insert(id, contribution);
                        }
                    }
                }
            }
            index.timezone_identity = Some(timezone_identity);
            index.timezone_offset_minutes = offset_minutes;
            index.enumeration = EnumerationCheckpoint::default();
            (index, true)
        }
        None => (
            ActivityIndex::with_timezone(offset_minutes, timezone_identity),
            path.exists(),
        ),
    }
}

fn contribution_key(file_key: &str, id: &str) -> String {
    if id.starts_with("offset:") {
        format!("{file_key}\0{id}")
    } else {
        id.to_owned()
    }
}

fn has_anonymous_contributions(checkpoint: &FileCheckpoint) -> bool {
    checkpoint
        .claude_messages
        .keys()
        .chain(checkpoint.grok_responses.keys())
        .any(|id| id.starts_with("offset:"))
}

fn has_aggregate_only_history(checkpoint: &FileCheckpoint) -> bool {
    checkpoint.tool != ActivityTool::Codex
        && !checkpoint.days.is_empty()
        && checkpoint.claude_messages.is_empty()
        && checkpoint.grok_responses.is_empty()
}

fn checkpoint_history_days(checkpoint: &FileCheckpoint, cutoff: &str) -> BTreeMap<String, u64> {
    if has_aggregate_only_history(checkpoint) {
        return checkpoint
            .days
            .iter()
            .filter(|(date, _)| date.as_str() >= cutoff)
            .map(|(date, tokens)| (date.clone(), *tokens))
            .collect();
    }
    let mut days = BTreeMap::new();
    match checkpoint.tool {
        ActivityTool::Claude => {
            for contribution in checkpoint.claude_messages.values() {
                if contribution.date.as_str() >= cutoff {
                    add_day(&mut days, &contribution.date, contribution.tokens);
                }
            }
        }
        ActivityTool::Grok => {
            for contribution in checkpoint.grok_responses.values() {
                if contribution.date.as_str() >= cutoff {
                    add_day(&mut days, &contribution.date, contribution.tokens);
                }
            }
        }
        ActivityTool::Codex => return checkpoint.days.clone(),
    }
    days
}

fn suppress_legacy_contribution(
    checkpoint: &FileCheckpoint,
    id: &str,
    current_consumed: bool,
    current_anonymous: bool,
) -> bool {
    if id.starts_with("offset:") {
        !checkpoint.anonymous_source_verified && current_consumed
    } else {
        checkpoint.ambiguous_stable_ids.contains(id)
            || (!checkpoint.anonymous_source_verified && current_anonymous)
    }
}

fn reconcile_legacy_history(index: &mut ActivityIndex, cutoff: &str) {
    let mut claude_ids: BTreeMap<String, (String, String, u64, String)> = BTreeMap::new();
    let mut grok_ids = BTreeSet::new();
    for (file_key, checkpoint) in &index.files {
        for (id, contribution) in &checkpoint.claude_messages {
            let key = contribution_key(file_key, id);
            if claude_ids.get(&key).is_none_or(|(_, _, tokens, date)| {
                contribution.tokens > *tokens
                    || (contribution.tokens == *tokens && contribution.date < *date)
            }) {
                claude_ids.insert(
                    key,
                    (
                        file_key.clone(),
                        id.clone(),
                        contribution.tokens,
                        contribution.date.clone(),
                    ),
                );
            }
        }
        grok_ids.extend(
            checkpoint
                .grok_responses
                .keys()
                .map(|id| contribution_key(file_key, id)),
        );
    }
    let files = &mut index.files;
    index.legacy_files.retain(|file_key, checkpoint| {
        checkpoint.prune(cutoff);
        let anonymous_source_verified = checkpoint.anonymous_source_verified;
        let days = &mut checkpoint.days;
        checkpoint.claude_messages.retain(|id, contribution| {
            if id.starts_with("offset:") && !anonymous_source_verified {
                return true;
            }
            if let Some((current_file, current_id, _, _)) =
                claude_ids.get(&contribution_key(file_key, id))
            {
                let current = files.get_mut(current_file).unwrap();
                let confirmed = current.claude_messages.get_mut(current_id).unwrap();
                if contribution.tokens > confirmed.tokens {
                    subtract_day(&mut current.days, &confirmed.date, confirmed.tokens);
                    confirmed.tokens = contribution.tokens;
                    add_day(&mut current.days, &confirmed.date, confirmed.tokens);
                }
                subtract_day(days, &contribution.date, contribution.tokens);
                false
            } else {
                true
            }
        });
        checkpoint.grok_responses.retain(|id, _| {
            (id.starts_with("offset:") && !anonymous_source_verified)
                || !grok_ids.contains(&contribution_key(file_key, id))
        });
        !checkpoint.days.is_empty()
            || !checkpoint.claude_messages.is_empty()
            || !checkpoint.grok_responses.is_empty()
    });
}

fn save_index(path: &Path, index: &ActivityIndex) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_vec_pretty(index)?;
    config::replace_file(path, &contents).context("replace activity index")
}

#[allow(clippy::too_many_arguments)]
fn collect_candidates<Tz: TimeZone>(
    roots: &ActivityRoots,
    enabled_tools: [bool; 3],
    cutoff: &str,
    timezone: Tz,
    deadline: Instant,
    options: ScanOptions,
    checkpoint: &mut EnumerationCheckpoint,
    readers: &mut [Vec<OpenDirectory>; 3],
) -> (Vec<CandidateFile>, bool) {
    let mut candidates = Vec::new();
    let mut partial = false;
    let mut entries_seen = 0usize;

    let roots = [
        roots.claude.as_deref(),
        roots.codex.as_deref(),
        roots.grok.as_deref(),
    ];
    let mut active = [false; 3];
    for i in 0..3 {
        let walk = &mut checkpoint.walks[i];
        let root = roots[i].filter(|_| enabled_tools[i]);
        if walk.root.as_deref() != root
            || walk.stack.len() > MAX_ENUMERATION_DEPTH
            || walk.stack.iter().any(|position| {
                position
                    .relative
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            })
        {
            *walk = DirectoryWalk {
                root: root.map(Path::to_path_buf),
                stack: Vec::new(),
                complete: false,
            };
            readers[i].clear();
        }
        let Some(root) = root else {
            readers[i].clear();
            continue;
        };
        if walk.complete {
            continue;
        }
        if walk.stack.is_empty() {
            if !root.is_dir() {
                readers[i].clear();
                walk.complete = true;
                continue;
            }
            walk.stack.push(DirectoryPosition {
                relative: PathBuf::new(),
                offset: 0,
            });
            readers[i].clear();
        }
        active[i] = true;
    }

    while active.iter().any(|active| *active) {
        if Instant::now() >= deadline
            || entries_seen >= options.max_entries
            || candidates.len() >= options.max_files
        {
            partial = true;
            break;
        }
        let i = (0..3)
            .map(|offset| (checkpoint.next_tool % 3 + offset) % 3)
            .find(|i| active[*i])
            .unwrap();
        checkpoint.next_tool = (i + 1) % 3;
        // Replay after reopening is also charged to the entry budget. The live
        // readers retain replay progress and parent positions across passes.
        // Both the DFS stack and its directory handles have a fixed depth bound.
        entries_seen += 1;
        let walk = &mut checkpoint.walks[i];
        let depth = walk.stack.len() - 1;
        let position = walk.stack.last_mut().unwrap();
        let directory = roots[i].unwrap().join(&position.relative);
        if readers[i]
            .last()
            .is_none_or(|reader| reader.path != directory || reader.offset > position.offset)
        {
            // Ancestors reopened from a saved checkpoint are filled lazily.
            readers[i]
                .retain(|reader| directory.starts_with(&reader.path) && reader.path != directory);
            let metadata = if position.relative.as_os_str().is_empty() {
                fs::metadata(&directory)
            } else {
                fs::symlink_metadata(&directory)
            };
            let entries = metadata
                .ok()
                .filter(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                .and_then(|_| fs::read_dir(&directory).ok());
            let Some(entries) = entries else {
                partial = true;
                walk.stack.pop();
                active[i] = !walk.stack.is_empty();
                walk.complete = !active[i];
                continue;
            };
            readers[i].push(OpenDirectory {
                path: directory,
                entries,
                offset: 0,
            });
        }
        debug_assert!(readers[i].len() <= depth + 1);
        let reader = readers[i].last_mut().unwrap();
        let Some(entry) = reader.entries.next() else {
            readers[i].pop();
            walk.stack.pop();
            active[i] = !walk.stack.is_empty();
            walk.complete = !active[i];
            continue;
        };
        reader.offset = reader.offset.saturating_add(1);
        if reader.offset <= position.offset {
            continue;
        }
        position.offset = reader.offset;
        let Ok(entry) = entry else {
            partial = true;
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            partial = true;
            continue;
        };
        if file_type.is_dir() {
            let relative = position.relative.join(entry.file_name());
            if walk.stack.len() < MAX_ENUMERATION_DEPTH {
                walk.stack.push(DirectoryPosition {
                    relative,
                    offset: 0,
                });
            } else {
                partial = true;
            }
            continue;
        }
        let path = entry.path();
        let tool = ActivityTool::ALL[i];
        if !file_type.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
            || (tool == ActivityTool::Grok
                && path.file_name().and_then(|value| value.to_str()) != Some("updates.jsonl"))
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            partial = true;
            continue;
        };
        let modified_millis = modified_millis(&metadata);
        let modified_date =
            DateTime::<Utc>::from(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH))
                .with_timezone(&timezone)
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
        if modified_date.as_str() >= cutoff {
            candidates.push(CandidateFile {
                key: path.to_string_lossy().into_owned(),
                path,
                tool,
                modified_millis,
            });
        }
    }
    if !active.iter().any(|active| *active) {
        for walk in &mut checkpoint.walks {
            walk.complete = false;
        }
    }
    (candidates, partial)
}

fn scan_file<Tz: TimeZone + Copy>(
    path: &Path,
    checkpoint: &mut FileCheckpoint,
    cutoff: &str,
    timezone: Tz,
    deadline: Instant,
    byte_budget: u64,
    prefix_proof: Option<(u64, [u8; 32])>,
) -> anyhow::Result<ScanProgress> {
    let metadata = fs::metadata(path)?;
    let current_len = metadata.len();
    let current_modified = modified_millis(&metadata);
    let current_revision = modified_nanos(&metadata);
    anyhow::ensure!(current_revision > 0, "activity source revision unavailable");

    let revision_changed =
        checkpoint.offset > 0 && checkpoint.source_modified_nanos != current_revision;
    if (checkpoint.offset > 0 && checkpoint.source_modified_nanos == 0)
        || (revision_changed && checkpoint.prefix_digest.is_none())
        || (revision_changed && current_len <= checkpoint.observed_len)
        || checkpoint.offset > current_len
        || (checkpoint.offset > 0
            && checkpoint.tail_fingerprint != fingerprint_before(path, checkpoint.offset)?)
    {
        *checkpoint = FileCheckpoint::new(checkpoint.tool);
    } else if revision_changed {
        invalidate_prefix_proof(checkpoint);
    }

    if checkpoint.offset == current_len {
        checkpoint.observed_len = current_len;
        checkpoint.modified_millis = current_modified;
        checkpoint.source_modified_nanos = current_revision;
        checkpoint.tail_fingerprint = fingerprint_before(path, checkpoint.offset)?;
        checkpoint.prune(cutoff);
        return validate_scanned_prefix(path, checkpoint, prefix_proof, deadline, byte_budget);
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(checkpoint.offset))?;
    let mut reader = BufReader::new(file.take(current_len - checkpoint.offset));
    let mut bytes_read = 0u64;
    let mut incomplete = false;
    let mut lossy = false;
    let mut buffer = Vec::with_capacity(16 * 1024);

    loop {
        if Instant::now() >= deadline || bytes_read >= byte_budget {
            incomplete = true;
            break;
        }
        let line_start = checkpoint.offset;
        let line = read_bounded_line(&mut reader, &mut buffer, MAX_LINE_BYTES)?;
        if line.consumed == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(line.consumed as u64);

        if line.overflow {
            checkpoint.offset = checkpoint.offset.saturating_add(line.consumed as u64);
            checkpoint.prefix_digest = None;
            checkpoint.anonymous_source_verified = false;
            checkpoint.lossy = true;
            lossy = true;
            continue;
        }

        let contents = trim_line_ending(&buffer);
        let parsed = serde_json::from_slice::<Value>(contents);
        match parsed {
            Ok(value) => {
                match checkpoint.tool {
                    ActivityTool::Claude => {
                        apply_claude_line(checkpoint, &value, cutoff, timezone, line_start)
                    }
                    ActivityTool::Codex => apply_codex_line(checkpoint, &value, cutoff, timezone),
                    ActivityTool::Grok => {
                        apply_grok_line(checkpoint, &value, cutoff, timezone, line_start)
                    }
                }
                checkpoint.offset = checkpoint.offset.saturating_add(line.consumed as u64);
            }
            Err(_) if line.terminated => {
                checkpoint.offset = checkpoint.offset.saturating_add(line.consumed as u64);
                checkpoint.lossy = true;
                lossy = true;
            }
            Err(_) => {
                incomplete = true;
                break;
            }
        }
        // A chained digest covers every consumed byte without rereading large logs.
        checkpoint.prefix_digest = checkpoint.prefix_digest.map(|previous| {
            let mut hasher = Sha256::new();
            hasher.update(previous);
            hasher.update(&buffer);
            hasher.finalize().into()
        });
        if let Some((offset, digest)) = prefix_proof {
            if !checkpoint.prefix_needs_validation && checkpoint.offset == offset {
                checkpoint.anonymous_source_verified = checkpoint.prefix_digest == Some(digest);
            }
        }
    }

    let observed_metadata = {
        let after = fs::metadata(path)?;
        if after.len() < current_len
            || (after.len() == current_len && modified_nanos(&after) != current_revision)
        {
            *checkpoint = FileCheckpoint::new(checkpoint.tool);
            return Ok(ScanProgress {
                incomplete: true,
                lossy,
                bytes_read,
            });
        }
        if after.len() > current_len {
            invalidate_prefix_proof(checkpoint);
            incomplete = true;
        }
        after
    };
    checkpoint.observed_len = observed_metadata.len();
    checkpoint.modified_millis = modified_millis(&observed_metadata);
    checkpoint.source_modified_nanos = modified_nanos(&observed_metadata);
    checkpoint.tail_fingerprint = fingerprint_before(path, checkpoint.offset)?;
    checkpoint.prune(cutoff);
    let validation = validate_scanned_prefix(
        path,
        checkpoint,
        prefix_proof,
        deadline,
        byte_budget.saturating_sub(bytes_read),
    )?;
    incomplete |= validation.incomplete;
    bytes_read = bytes_read.saturating_add(validation.bytes_read);
    Ok(ScanProgress {
        incomplete,
        lossy,
        bytes_read,
    })
}

fn invalidate_prefix_proof(checkpoint: &mut FileCheckpoint) {
    checkpoint.anonymous_source_verified = false;
    checkpoint.prefix_needs_validation = checkpoint.prefix_digest.is_some();
    checkpoint.prefix_validation = None;
}

fn validate_scanned_prefix(
    path: &Path,
    checkpoint: &mut FileCheckpoint,
    prefix_proof: Option<(u64, [u8; 32])>,
    deadline: Instant,
    byte_budget: u64,
) -> anyhow::Result<ScanProgress> {
    if !checkpoint.prefix_needs_validation {
        return Ok(ScanProgress::default());
    }
    if Instant::now() >= deadline || byte_budget == 0 {
        return Ok(ScanProgress {
            incomplete: true,
            ..ScanProgress::default()
        });
    }
    // Collection keeps advancing on append; only this bounded proof restarts.
    if checkpoint.offset < checkpoint.observed_len {
        return Ok(ScanProgress {
            incomplete: true,
            ..ScanProgress::default()
        });
    }
    let before = fs::metadata(path)?;
    let revision = modified_nanos(&before);
    if before.len() != checkpoint.offset || revision == 0 {
        checkpoint.prefix_validation = None;
        return Ok(ScanProgress {
            incomplete: true,
            ..ScanProgress::default()
        });
    }
    let mut validation = checkpoint
        .prefix_validation
        .take()
        .filter(|validation| {
            validation.target_offset == checkpoint.offset
                && validation.source_modified_nanos == revision
                && validation.offset <= validation.target_offset
        })
        .unwrap_or(PrefixValidation {
            offset: 0,
            digest: [0; 32],
            target_offset: checkpoint.offset,
            source_modified_nanos: revision,
            legacy_prefix_matches: false,
        });
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(validation.offset))?;
    let mut reader = BufReader::new(file.take(validation.target_offset - validation.offset));
    let mut buffer = Vec::new();
    let mut bytes_read = 0u64;
    while validation.offset < validation.target_offset {
        if Instant::now() >= deadline || bytes_read >= byte_budget {
            break;
        }
        let line = read_bounded_line(&mut reader, &mut buffer, MAX_LINE_BYTES)?;
        bytes_read = bytes_read.saturating_add(line.consumed as u64);
        if line.overflow || line.consumed == 0 {
            *checkpoint = FileCheckpoint::new(checkpoint.tool);
            return Ok(ScanProgress {
                incomplete: true,
                bytes_read,
                ..ScanProgress::default()
            });
        }
        validation.offset = validation.offset.saturating_add(line.consumed as u64);
        let mut hasher = Sha256::new();
        hasher.update(validation.digest);
        hasher.update(&buffer);
        validation.digest = hasher.finalize().into();
        if let Some((offset, digest)) = prefix_proof {
            if validation.offset == offset {
                validation.legacy_prefix_matches = validation.digest == digest;
            }
        }
    }
    let after = fs::metadata(path)?;
    if after.len() != before.len() || modified_nanos(&after) != revision {
        checkpoint.prefix_validation = None;
        return Ok(ScanProgress {
            incomplete: true,
            bytes_read,
            ..ScanProgress::default()
        });
    }
    if validation.offset < validation.target_offset {
        checkpoint.prefix_validation = Some(validation);
        return Ok(ScanProgress {
            incomplete: true,
            bytes_read,
            ..ScanProgress::default()
        });
    }
    if checkpoint.prefix_digest != Some(validation.digest) {
        *checkpoint = FileCheckpoint::new(checkpoint.tool);
        return Ok(ScanProgress {
            incomplete: true,
            bytes_read,
            ..ScanProgress::default()
        });
    }
    checkpoint.prefix_needs_validation = false;
    checkpoint.anonymous_source_verified = validation.legacy_prefix_matches;
    Ok(ScanProgress {
        bytes_read,
        ..ScanProgress::default()
    })
}

struct BoundedLine {
    consumed: usize,
    terminated: bool,
    overflow: bool,
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    output: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<BoundedLine> {
    output.clear();
    let mut consumed = 0usize;
    let mut terminated = false;
    let mut overflow = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        if !overflow && output.len().saturating_add(take) <= max_bytes {
            output.extend_from_slice(&available[..take]);
        } else {
            overflow = true;
        }
        reader.consume(take);
        consumed = consumed.saturating_add(take);
        if newline.is_some() {
            terminated = true;
            break;
        }
    }

    Ok(BoundedLine {
        consumed,
        terminated,
        overflow,
    })
}

fn trim_line_ending(mut value: &[u8]) -> &[u8] {
    if value.ends_with(b"\n") {
        value = &value[..value.len() - 1];
    }
    if value.ends_with(b"\r") {
        value = &value[..value.len() - 1];
    }
    value
}

fn apply_claude_line<Tz: TimeZone>(
    checkpoint: &mut FileCheckpoint,
    root: &Value,
    cutoff: &str,
    timezone: Tz,
    line_start: u64,
) {
    let Some(usage) = root.pointer("/message/usage") else {
        return;
    };
    let tokens = claude_usage_tokens(usage);
    if tokens == 0 {
        return;
    }
    let Some(date) = local_date(root.get("timestamp"), timezone) else {
        return;
    };
    if date.as_str() < cutoff {
        return;
    }
    let message_id = root
        .pointer("/message/id")
        .and_then(Value::as_str)
        .or_else(|| root.get("uuid").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("offset:{line_start}"));

    if let Some(previous) = checkpoint.claude_messages.get(&message_id) {
        if previous.tokens >= tokens {
            return;
        }
        subtract_day(&mut checkpoint.days, &previous.date, previous.tokens);
    }
    add_day(&mut checkpoint.days, &date, tokens);
    checkpoint
        .claude_messages
        .insert(message_id, ClaudeMessageContribution { date, tokens });
}

fn claude_usage_tokens(usage: &Value) -> u64 {
    if let Some(iterations) = usage
        .get("iterations")
        .and_then(Value::as_array)
        .filter(|iterations| !iterations.is_empty())
    {
        let total = iterations.iter().fold(0u64, |total, iteration| {
            total.saturating_add(usage_token_fields(iteration))
        });
        if total > 0 {
            return total;
        }
    }
    usage_token_fields(usage)
}

fn usage_token_fields(usage: &Value) -> u64 {
    [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .into_iter()
    .filter_map(|key| usage.get(key).and_then(Value::as_u64))
    .fold(0u64, u64::saturating_add)
}

fn apply_codex_line<Tz: TimeZone>(
    checkpoint: &mut FileCheckpoint,
    root: &Value,
    cutoff: &str,
    timezone: Tz,
) {
    if root.get("type").and_then(Value::as_str) != Some("event_msg")
        || root.pointer("/payload/type").and_then(Value::as_str) != Some("token_count")
    {
        return;
    }
    let Some(date) = local_date(root.get("timestamp"), timezone) else {
        return;
    };
    let cumulative = root
        .pointer("/payload/info/total_token_usage/total_tokens")
        .and_then(Value::as_u64);
    let last_usage = root
        .pointer("/payload/info/last_token_usage/total_tokens")
        .and_then(Value::as_u64);
    let delta = if let Some(total) = cumulative {
        let delta = match checkpoint.codex_last_total {
            Some(previous) if total >= previous => total - previous,
            Some(_) => last_usage.unwrap_or(total),
            None => {
                last_usage.unwrap_or_else(|| total.saturating_sub(checkpoint.codex_fallback_total))
            }
        };
        checkpoint.codex_last_total = Some(total);
        checkpoint.codex_fallback_total = 0;
        delta
    } else {
        let fallback = last_usage.unwrap_or(0);
        if checkpoint.codex_last_total.is_none() {
            checkpoint.codex_fallback_total =
                checkpoint.codex_fallback_total.saturating_add(fallback);
            fallback
        } else {
            0
        }
    };
    if delta > 0 && date.as_str() >= cutoff {
        add_day(&mut checkpoint.days, &date, delta);
    }
}

fn apply_grok_line<Tz: TimeZone>(
    checkpoint: &mut FileCheckpoint,
    root: &Value,
    cutoff: &str,
    timezone: Tz,
    line_start: u64,
) {
    if !matches!(
        root.get("method").and_then(Value::as_str),
        Some("session/update" | "_x.ai/session/update")
    ) || root
        .pointer("/params/update/sessionUpdate")
        .and_then(Value::as_str)
        != Some("response_completed")
    {
        return;
    }
    let Some(usage) = root.pointer("/params/update/usage") else {
        return;
    };
    let tokens = usage_token_fields(usage);
    if tokens == 0 {
        return;
    }
    let Some(date) = local_date(root.get("timestamp"), timezone) else {
        return;
    };
    if date.as_str() < cutoff {
        return;
    }
    let response_id = root
        .pointer("/params/_meta/eventId")
        .and_then(Value::as_str)
        .or_else(|| {
            root.pointer("/params/update/message_id")
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("offset:{line_start}"));

    checkpoint
        .grok_responses
        .insert(response_id, GrokResponseContribution { date, tokens });
}

fn local_date<Tz: TimeZone>(timestamp: Option<&Value>, timezone: Tz) -> Option<String> {
    let timestamp = timestamp?;
    let value = match timestamp {
        Value::String(timestamp) => DateTime::parse_from_rfc3339(timestamp)
            .ok()?
            .with_timezone(&Utc),
        Value::Number(_) => {
            let timestamp = timestamp.as_u64()?;
            let timestamp = i64::try_from(timestamp).ok()?;
            if timestamp >= 100_000_000_000 {
                DateTime::<Utc>::from_timestamp_millis(timestamp)?
            } else {
                DateTime::<Utc>::from_timestamp(timestamp, 0)?
            }
        }
        _ => return None,
    };
    Some(
        value
            .with_timezone(&timezone)
            .date_naive()
            .format("%Y-%m-%d")
            .to_string(),
    )
}

fn add_day(days: &mut BTreeMap<String, u64>, date: &str, tokens: u64) {
    let current = days.entry(date.to_owned()).or_default();
    *current = current.saturating_add(tokens);
}

fn subtract_day(days: &mut BTreeMap<String, u64>, date: &str, tokens: u64) {
    if let Some(current) = days.get_mut(date) {
        *current = current.saturating_sub(tokens);
        if *current == 0 {
            days.remove(date);
        }
    }
}

fn snapshot_from_index(
    index: &ActivityIndex,
    now: DateTime<Utc>,
    offset_minutes: i32,
    incomplete: bool,
    lossy: bool,
    cutoff: &str,
) -> ActivitySnapshot {
    let mut days: BTreeMap<String, ActivityDay> = BTreeMap::new();
    let mut claude_messages: BTreeMap<String, &ClaudeMessageContribution> = BTreeMap::new();
    let mut grok_responses: BTreeMap<String, &GrokResponseContribution> = BTreeMap::new();
    for (file_key, checkpoint, legacy) in index
        .files
        .iter()
        .map(|(key, checkpoint)| (key, checkpoint, false))
        .chain(
            index
                .legacy_files
                .iter()
                .map(|(key, checkpoint)| (key, checkpoint, true)),
        )
    {
        let aggregate_only =
            legacy && checkpoint.claude_messages.is_empty() && checkpoint.grok_responses.is_empty();
        if aggregate_only && index.files.contains_key(file_key) {
            // Without IDs, keep the old totals as a lossy backup rather than add them twice.
            continue;
        }
        let current = index.files.get(file_key).filter(|_| legacy);
        let current_consumed = current.is_some_and(|current| current.offset > 0);
        let current_anonymous = current.is_some_and(has_anonymous_contributions);
        if checkpoint.tool == ActivityTool::Claude && !aggregate_only {
            for (message_id, contribution) in &checkpoint.claude_messages {
                if legacy
                    && suppress_legacy_contribution(
                        checkpoint,
                        message_id,
                        current_consumed,
                        current_anonymous,
                    )
                {
                    continue;
                }
                if contribution.date.as_str() < cutoff {
                    continue;
                }
                let dedupe_key = contribution_key(file_key, message_id);
                let should_replace = claude_messages.get(&dedupe_key).is_none_or(|previous| {
                    contribution.tokens > previous.tokens
                        || (contribution.tokens == previous.tokens
                            && contribution.date < previous.date)
                });
                if should_replace {
                    claude_messages.insert(dedupe_key, contribution);
                }
            }
            continue;
        }
        if checkpoint.tool == ActivityTool::Grok && !aggregate_only {
            for (response_id, contribution) in &checkpoint.grok_responses {
                if legacy
                    && suppress_legacy_contribution(
                        checkpoint,
                        response_id,
                        current_consumed,
                        current_anonymous,
                    )
                {
                    continue;
                }
                if contribution.date.as_str() < cutoff {
                    continue;
                }
                let dedupe_key = contribution_key(file_key, response_id);
                grok_responses.entry(dedupe_key).or_insert(contribution);
            }
            continue;
        }
        for (date, tokens) in &checkpoint.days {
            if date.as_str() < cutoff {
                continue;
            }
            let day = days.entry(date.clone()).or_insert_with(|| ActivityDay {
                date: date.clone(),
                ..ActivityDay::default()
            });
            match checkpoint.tool {
                ActivityTool::Codex => day.codex_tokens = day.codex_tokens.saturating_add(*tokens),
                ActivityTool::Claude => {
                    day.claude_tokens = day.claude_tokens.saturating_add(*tokens)
                }
                ActivityTool::Grok => day.grok_tokens = day.grok_tokens.saturating_add(*tokens),
            }
        }
    }
    for contribution in claude_messages.into_values() {
        let day = days
            .entry(contribution.date.clone())
            .or_insert_with(|| ActivityDay {
                date: contribution.date.clone(),
                ..ActivityDay::default()
            });
        day.claude_tokens = day.claude_tokens.saturating_add(contribution.tokens);
    }
    for contribution in grok_responses.into_values() {
        let day = days
            .entry(contribution.date.clone())
            .or_insert_with(|| ActivityDay {
                date: contribution.date.clone(),
                ..ActivityDay::default()
            });
        day.grok_tokens = day.grok_tokens.saturating_add(contribution.tokens);
    }
    ActivitySnapshot {
        schema_version: SCHEMA_VERSION.into(),
        generated_at: now.to_rfc3339(),
        timezone_offset_minutes: offset_minutes,
        partial: incomplete || lossy,
        backfill_pending: incomplete,
        local_partial: incomplete || lossy,
        local_backfill_pending: incomplete,
        codex_partial: false,
        codex_backfill_pending: false,
        codex_account_scope: false,
        cursor_partial: false,
        cursor_backfill_pending: false,
        cursor_account_scope: false,
        days: days.into_values().collect(),
    }
}

pub fn merge_codex_activity(
    mut snapshot: ActivitySnapshot,
    codex: Option<&crate::codex_activity::CodexActivityView>,
    codex_enabled: bool,
) -> ActivitySnapshot {
    let mut days = snapshot
        .days
        .into_iter()
        .map(|mut day| {
            day.codex_tokens = 0;
            (day.date.clone(), day)
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(codex) = codex.filter(|_| codex_enabled) {
        for (date, tokens) in &codex.days {
            days.entry(date.clone())
                .or_insert_with(|| ActivityDay {
                    date: date.clone(),
                    ..ActivityDay::default()
                })
                .codex_tokens = *tokens;
        }
        snapshot.codex_partial = codex.partial;
        snapshot.codex_account_scope = true;
    } else {
        snapshot.codex_partial = codex_enabled;
        snapshot.codex_account_scope = codex_enabled;
    }
    snapshot.codex_backfill_pending = false;
    snapshot.partial = snapshot.local_partial || snapshot.codex_partial || snapshot.cursor_partial;
    snapshot.backfill_pending = snapshot.local_backfill_pending
        || snapshot.codex_backfill_pending
        || snapshot.cursor_backfill_pending;
    snapshot.days = days.into_values().collect();
    snapshot
}

pub fn merge_cursor_activity(
    mut snapshot: ActivitySnapshot,
    cursor: Option<&crate::cursor_activity::CursorActivityView>,
    cursor_enabled: bool,
) -> ActivitySnapshot {
    let mut days = snapshot
        .days
        .into_iter()
        .map(|day| (day.date.clone(), day))
        .collect::<BTreeMap<_, _>>();
    if let Some(cursor) = cursor.filter(|_| cursor_enabled) {
        for (date, tokens) in &cursor.days {
            days.entry(date.clone())
                .or_insert_with(|| ActivityDay {
                    date: date.clone(),
                    ..ActivityDay::default()
                })
                .cursor_tokens = *tokens;
        }
        snapshot.cursor_partial = cursor.partial;
        snapshot.cursor_backfill_pending = cursor.backfill_pending;
        snapshot.cursor_account_scope = true;
    } else {
        snapshot.cursor_partial = cursor_enabled;
        snapshot.cursor_backfill_pending = cursor_enabled;
        snapshot.cursor_account_scope = false;
    }
    snapshot.partial = snapshot.local_partial || snapshot.codex_partial || snapshot.cursor_partial;
    snapshot.backfill_pending = snapshot.local_backfill_pending
        || snapshot.codex_backfill_pending
        || snapshot.cursor_backfill_pending;
    snapshot.days = days.into_values().collect();
    snapshot
}

fn fingerprint_before(path: &Path, offset: u64) -> std::io::Result<u64> {
    if offset == 0 {
        return Ok(0);
    }
    let start = offset.saturating_sub(TAIL_FINGERPRINT_BYTES);
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((offset - start) as usize);
    file.take(offset - start).read_to_end(&mut bytes)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(hasher.finish())
}

fn modified_nanos(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn modified_millis(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::{fs::OpenOptions, io::Write};

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-activity-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn options() -> ScanOptions {
        ScanOptions {
            max_files: 32,
            max_entries: 128,
            max_bytes: 16 * 1024 * 1024,
            max_duration: Duration::from_secs(2),
        }
    }

    fn candidate(tool: ActivityTool, key: &str, modified_millis: u64) -> CandidateFile {
        CandidateFile {
            path: PathBuf::from(key),
            key: key.into(),
            tool,
            modified_millis,
        }
    }

    fn roots(root: &Path) -> ActivityRoots {
        ActivityRoots {
            claude: Some(root.join("claude")),
            codex: Some(root.join("codex")),
            grok: Some(root.join("grok")),
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 19, 12, 0, 0).unwrap()
    }

    fn kst() -> FixedOffset {
        FixedOffset::east_opt(9 * 3600).unwrap()
    }

    fn grok_event(timestamp: Value, method: &str, usage: Value) -> String {
        grok_event_with_id(timestamp, method, usage, None)
    }

    fn grok_event_with_id(
        timestamp: Value,
        method: &str,
        usage: Value,
        event_id: Option<&str>,
    ) -> String {
        let mut event = serde_json::json!({
            "timestamp": timestamp,
            "method": method,
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "response_completed",
                    "usage": usage
                }
            }
        });
        if let Some(event_id) = event_id {
            event["params"]["_meta"] = serde_json::json!({ "eventId": event_id });
        }
        event.to_string()
    }

    fn test_local_identity(key: &str) -> ActivityTimezoneIdentity {
        ActivityTimezoneIdentity::Local {
            key: key.into(),
            dynamic_daylight_disabled: false,
        }
    }

    fn write_dst_logs(root: &Path) {
        fs::create_dir_all(root.join("claude")).unwrap();
        fs::create_dir_all(root.join("grok")).unwrap();
        let timestamps = ["2026-01-15T04:30:00Z", "2026-07-19T04:30:00Z"];
        let claude = timestamps
            .iter()
            .enumerate()
            .map(|(index, timestamp)| {
                serde_json::json!({
                    "timestamp": timestamp,
                    "message": {
                        "id": format!("dst-message-{index}"),
                        "usage": { "input_tokens": 10, "output_tokens": 20 }
                    }
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("claude").join("session.jsonl"), claude + "\n").unwrap();
        let grok = timestamps
            .iter()
            .enumerate()
            .map(|(index, timestamp)| {
                grok_event_with_id(
                    serde_json::json!(timestamp),
                    "session/update",
                    serde_json::json!({ "input_tokens": 4, "output_tokens": 6 }),
                    Some(&format!("dst-response-{index}")),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("grok").join("updates.jsonl"), grok + "\n").unwrap();
    }

    #[test]
    fn historical_dates_use_winter_and_summer_offsets_at_each_event() {
        use chrono_tz::America::New_York;

        for (timestamp, expected) in [
            ("2026-01-15T04:30:00Z", "2026-01-14"),
            ("2026-01-15T05:00:00Z", "2026-01-15"),
            ("2026-07-19T03:59:59Z", "2026-07-18"),
            ("2026-07-19T04:00:00Z", "2026-07-19"),
            ("2026-07-19T04:30:00Z", "2026-07-19"),
            ("2026-03-08T06:59:59Z", "2026-03-08"),
            ("2026-03-08T07:00:00Z", "2026-03-08"),
            ("2026-11-01T05:59:59Z", "2026-11-01"),
            ("2026-11-01T06:00:00Z", "2026-11-01"),
        ] {
            let utc = DateTime::parse_from_rfc3339(timestamp).unwrap();
            for value in [
                serde_json::json!(timestamp),
                serde_json::json!(utc.timestamp()),
                serde_json::json!(utc.timestamp_millis()),
            ] {
                assert_eq!(
                    local_date(Some(&value), New_York).as_deref(),
                    Some(expected)
                );
            }
        }
        assert_eq!(
            local_date(
                Some(&serde_json::json!("2026-01-15T04:30:00Z")),
                FixedOffset::west_opt(4 * 3600).unwrap(),
            )
            .as_deref(),
            Some("2026-01-15")
        );
    }

    #[test]
    fn local_dates_apply_the_system_timezone_at_each_event() {
        for timestamp in ["2026-01-15T04:30:00Z", "2026-07-19T04:30:00Z"] {
            let utc = DateTime::parse_from_rfc3339(timestamp)
                .unwrap()
                .with_timezone(&Utc);
            let expected = Local
                .from_utc_datetime(&utc.naive_utc())
                .format("%Y-%m-%d")
                .to_string();
            assert_eq!(
                local_date(Some(&serde_json::json!(timestamp)), Local),
                Some(expected)
            );
        }
    }

    #[test]
    fn dst_scanner_keeps_correct_days_and_reuses_cache_across_seasons() {
        use chrono_tz::America::New_York;

        let root = temp_root("dst-scanner");
        write_dst_logs(&root);
        let path = root.join("index.json");
        let identity = test_local_identity("Eastern Standard Time");
        let summer = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            identity.clone(),
            options(),
        )
        .unwrap();
        assert_eq!(summer.timezone_offset_minutes, -240);
        assert_eq!(summer.days.len(), 2);
        assert_eq!(summer.days[0].date, "2026-01-14");
        assert_eq!(summer.days[1].date, "2026-07-19");
        for day in &summer.days {
            assert_eq!(day.claude_tokens, 30);
            assert_eq!(day.grok_tokens, 10);
        }
        let (cached, rebuilt) = load_index_for_timezone(&path, -300, identity.clone());
        assert!(!rebuilt);
        assert_eq!(cached.files.len(), 2);
        let mut paused_options = options();
        paused_options.max_entries = 0;
        let winter = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap(),
            New_York,
            identity.clone(),
            paused_options,
        )
        .unwrap();
        assert_eq!(winter.timezone_offset_minutes, -300);
        assert_eq!(winter.days, summer.days);
        let (updated, rebuilt) = load_index_for_timezone(&path, -300, identity);
        assert!(!rebuilt);
        assert_eq!(updated.timezone_offset_minutes, -300);
        assert_eq!(updated.files, cached.files);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_fixed_offset_cache_is_rebuilt_with_historical_dates() {
        use chrono_tz::America::New_York;

        let root = temp_root("dst-cache-migration");
        write_dst_logs(&root);
        let path = root.join("index.json");
        let source = root.join("claude").join("session.jsonl");
        let original = fs::read(&source).unwrap();
        let legacy = refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            FixedOffset::west_opt(4 * 3600).unwrap(),
            options(),
        )
        .unwrap();
        assert_eq!(legacy.days[0].date, "2026-01-15");
        let identity = test_local_identity("Eastern Standard Time");
        let (empty, rebuilt) = load_index_for_timezone(&path, -240, identity.clone());
        assert!(rebuilt && empty.files.is_empty());
        assert_eq!(empty.legacy_files.len(), 2);
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("timezone_identity");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let (empty, rebuilt) = load_index_for_timezone(&path, -240, identity.clone());
        assert!(rebuilt && empty.files.is_empty());
        assert_eq!(empty.legacy_files.len(), 2);
        let corrected = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            identity.clone(),
            options(),
        )
        .unwrap();
        assert_eq!(corrected.days[0].date, "2026-01-14");
        assert_eq!(corrected.days[0].claude_tokens, 30);
        assert_eq!(corrected.days[0].grok_tokens, 10);
        assert_eq!(corrected.days.len(), 2);
        assert_eq!(fs::read(source).unwrap(), original);
        let (saved, rebuilt) = load_index_for_timezone(&path, -240, identity);
        assert!(!rebuilt);
        assert!(saved.timezone_identity.is_some());
        assert!(saved.legacy_files.is_empty());
        assert!(!corrected.partial);
        fs::remove_dir_all(root).unwrap();
    }

    fn make_cache_legacy(path: &Path) {
        let mut value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("timezone_identity");
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn anonymous_event(tool: ActivityTool, tokens: u64) -> String {
        let timestamp = "2026-07-19T04:30:00Z";
        (match tool {
            ActivityTool::Claude => serde_json::json!({
                "timestamp": timestamp,
                "message": { "usage": { "input_tokens": tokens } }
            })
            .to_string(),
            ActivityTool::Grok => grok_event(
                serde_json::json!(timestamp),
                "session/update",
                serde_json::json!({ "input_tokens": tokens }),
            ),
            ActivityTool::Codex => unreachable!(),
        }) + "\n"
    }

    #[test]
    fn anonymous_migration_rewrites_do_not_add_or_reconcile_unverified_offsets() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            for shifted in [true, false] {
                let root = temp_root("anonymous-rewrite");
                let folder = root.join(if tool == ActivityTool::Claude {
                    "claude"
                } else {
                    "grok"
                });
                fs::create_dir_all(&folder).unwrap();
                let source = folder.join(if tool == ActivityTool::Claude {
                    "session.jsonl"
                } else {
                    "updates.jsonl"
                });
                let path = root.join("index.json");
                let event = anonymous_event(tool, 10);
                fs::write(&source, &event).unwrap();
                let refresh = || {
                    refresh_at(
                        &path,
                        &roots(&root),
                        true,
                        false,
                        true,
                        now(),
                        kst(),
                        options(),
                    )
                    .unwrap()
                };
                refresh();
                make_cache_legacy(&path);
                fs::write(
                    &source,
                    if shifted {
                        format!("{{}}\n{event}")
                    } else {
                        anonymous_event(tool, 5)
                    },
                )
                .unwrap();
                for _ in 0..3 {
                    let snapshot = refresh();
                    let total = snapshot
                        .days
                        .iter()
                        .map(|day| day.claude_tokens + day.grok_tokens)
                        .sum::<u64>();
                    assert_eq!(total, if shifted { 10 } else { 5 });
                    assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
                    let index = load_index(&path, 540).0;
                    let legacy = index.legacy_files.values().next().unwrap();
                    assert!(!legacy.anonymous_source_verified);
                    assert!(
                        legacy
                            .claude_messages
                            .values()
                            .any(|value| value.tokens == 10)
                            || legacy
                                .grok_responses
                                .values()
                                .any(|value| value.tokens == 10)
                    );
                }
                // Another migration keeps the latest backup, not both overlapping epochs.
                make_cache_legacy(&path);
                assert_eq!(
                    refresh()
                        .days
                        .iter()
                        .map(|day| day.claude_tokens + day.grok_tokens)
                        .sum::<u64>(),
                    if shifted { 10 } else { 5 }
                );
                fs::remove_file(&source).unwrap();
                make_cache_legacy(&path);
                let missing = refresh();
                assert_eq!(
                    missing
                        .days
                        .iter()
                        .map(|day| day.claude_tokens + day.grok_tokens)
                        .sum::<u64>(),
                    if shifted { 10 } else { 5 }
                );
                assert!(missing.local_partial);
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn anonymous_migration_verified_append_reconciles_without_double_counting() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            let root = temp_root("anonymous-append");
            let folder = root.join(if tool == ActivityTool::Claude {
                "claude"
            } else {
                "grok"
            });
            fs::create_dir_all(&folder).unwrap();
            let source = folder.join(if tool == ActivityTool::Claude {
                "session.jsonl"
            } else {
                "updates.jsonl"
            });
            let path = root.join("index.json");
            let first = anonymous_event(tool, 10);
            fs::write(&source, &first).unwrap();
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            make_cache_legacy(&path);
            fs::write(&source, first + &anonymous_event(tool, 5)).unwrap();
            for _ in 0..3 {
                let snapshot = refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    options(),
                )
                .unwrap();
                assert_eq!(
                    snapshot
                        .days
                        .iter()
                        .map(|day| day.claude_tokens + day.grok_tokens)
                        .sum::<u64>(),
                    15
                );
                assert!(!snapshot.local_partial && !snapshot.local_backfill_pending);
                assert!(load_index(&path, 540).0.legacy_files.is_empty());
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn anonymous_migration_rejects_prefix_rewrites_with_identical_tail_and_length() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            for shifted in [true, false] {
                let root = temp_root("anonymous-prefix-rewrite");
                let folder = root.join(if tool == ActivityTool::Claude {
                    "claude"
                } else {
                    "grok"
                });
                fs::create_dir_all(&folder).unwrap();
                let source = folder.join(if tool == ActivityTool::Claude {
                    "session.jsonl"
                } else {
                    "updates.jsonl"
                });
                let path = root.join("index.json");
                let filler = "{}\n".repeat(2500);
                let original = anonymous_event(tool, 10) + &filler;
                fs::write(&source, &original).unwrap();
                refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    options(),
                )
                .unwrap();
                let saved = load_index(&path, 540).0;
                let old = saved.files.values().next().unwrap();
                make_cache_legacy(&path);
                let rewritten = if shifted {
                    "{}\n".to_owned() + &anonymous_event(tool, 10) + &filler[3..]
                } else {
                    anonymous_event(tool, 20) + &filler
                };
                assert_eq!(rewritten.len(), original.len());
                fs::write(&source, rewritten).unwrap();
                assert_eq!(
                    fingerprint_before(&source, old.offset).unwrap(),
                    old.tail_fingerprint
                );
                let snapshot = refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    options(),
                )
                .unwrap();
                assert_eq!(
                    snapshot
                        .days
                        .iter()
                        .map(|day| day.claude_tokens + day.grok_tokens)
                        .sum::<u64>(),
                    if shifted { 10 } else { 20 }
                );
                assert!(snapshot.local_partial);
                assert!(
                    !load_index(&path, 540)
                        .0
                        .legacy_files
                        .values()
                        .next()
                        .unwrap()
                        .anonymous_source_verified
                );
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn anonymous_partial_reread_and_repeated_migration_preserve_unread_history() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            let root = temp_root("anonymous-partial-remigration");
            let folder = root.join(if tool == ActivityTool::Claude {
                "claude"
            } else {
                "grok"
            });
            fs::create_dir_all(&folder).unwrap();
            let source = folder.join(if tool == ActivityTool::Claude {
                "session.jsonl"
            } else {
                "updates.jsonl"
            });
            let path = root.join("index.json");
            fs::write(
                &source,
                anonymous_event(tool, 10) + &anonymous_event(tool, 20),
            )
            .unwrap();
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            make_cache_legacy(&path);
            let mut bounded = options();
            bounded.max_bytes = 1;
            let partial = refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                bounded,
            )
            .unwrap();
            assert!(partial.local_partial && partial.local_backfill_pending);
            fs::remove_file(source).unwrap();
            for _ in 0..3 {
                make_cache_legacy(&path);
                let snapshot = refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    options(),
                )
                .unwrap();
                assert_eq!(
                    snapshot
                        .days
                        .iter()
                        .map(|day| day.claude_tokens + day.grok_tokens)
                        .sum::<u64>(),
                    30
                );
                assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn anonymous_prefix_proof_restarts_when_source_changes_between_scan_passes() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            let root = temp_root("anonymous-between-passes");
            let folder = root.join(if tool == ActivityTool::Claude {
                "claude"
            } else {
                "grok"
            });
            fs::create_dir_all(&folder).unwrap();
            let source = folder.join(if tool == ActivityTool::Claude {
                "session.jsonl"
            } else {
                "updates.jsonl"
            });
            let path = root.join("index.json");
            let filler = "{}\n".repeat(2500);
            let original = anonymous_event(tool, 10) + &filler + &anonymous_event(tool, 20);
            fs::write(&source, &original).unwrap();
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            make_cache_legacy(&path);
            let mut bounded = options();
            bounded.max_bytes = 5500;
            let partial = refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                bounded,
            )
            .unwrap();
            assert!(partial.local_backfill_pending);
            let checkpoint = load_index(&path, 540)
                .0
                .files
                .values()
                .next()
                .unwrap()
                .clone();
            assert!(
                checkpoint.offset > TAIL_FINGERPRINT_BYTES
                    && checkpoint.offset < checkpoint.observed_len
            );
            let changed = anonymous_event(tool, 90) + &filler + &anonymous_event(tool, 20);
            assert_eq!(changed.len(), original.len());
            fs::write(&source, changed).unwrap();
            File::options()
                .write(true)
                .open(&source)
                .unwrap()
                .set_modified(SystemTime::now() + Duration::from_secs(1))
                .unwrap();
            assert_eq!(
                fingerprint_before(&source, checkpoint.offset).unwrap(),
                checkpoint.tail_fingerprint
            );
            let result = refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            assert_eq!(
                result
                    .days
                    .iter()
                    .map(|day| day.claude_tokens + day.grok_tokens)
                    .sum::<u64>(),
                110
            );
            assert!(result.local_partial && !result.local_backfill_pending);
            assert!(
                !load_index(&path, 540)
                    .0
                    .legacy_files
                    .values()
                    .next()
                    .unwrap()
                    .anonymous_source_verified
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn append_during_bounded_migration_keeps_collecting_and_finishes_prefix_validation() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            let root = temp_root("bounded-append-migration");
            let folder = root.join(if tool == ActivityTool::Claude {
                "claude"
            } else {
                "grok"
            });
            fs::create_dir_all(&folder).unwrap();
            let source = folder.join(if tool == ActivityTool::Claude {
                "session.jsonl"
            } else {
                "updates.jsonl"
            });
            let path = root.join("index.json");
            fs::write(
                &source,
                anonymous_event(tool, 10) + &anonymous_event(tool, 20),
            )
            .unwrap();
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            make_cache_legacy(&path);
            let mut bounded = options();
            bounded.max_bytes = 1;
            let mut previous_offset = 0;
            for step in 0..5 {
                if step > 0 {
                    OpenOptions::new()
                        .append(true)
                        .open(&source)
                        .unwrap()
                        .write_all(anonymous_event(tool, 30 + step * 10).as_bytes())
                        .unwrap();
                    File::options()
                        .write(true)
                        .open(&source)
                        .unwrap()
                        .set_modified(SystemTime::now() + Duration::from_secs(step))
                        .unwrap();
                }
                let result = refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    bounded,
                )
                .unwrap();
                let saved = load_index(&path, 540).0;
                let checkpoint = saved.files.values().next().unwrap();
                assert!(checkpoint.offset > previous_offset);
                previous_offset = checkpoint.offset;
                assert!(result.local_backfill_pending);
                assert!(!checkpoint.anonymous_source_verified);
            }
            let mut completed = None;
            for _ in 0..12 {
                let result = refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    bounded,
                )
                .unwrap();
                if !result.local_backfill_pending {
                    completed = Some(result);
                    break;
                }
            }
            let result = completed.expect("quiet source finishes bounded validation");
            assert_eq!(
                result
                    .days
                    .iter()
                    .map(|day| day.claude_tokens + day.grok_tokens)
                    .sum::<u64>(),
                250
            );
            assert!(!result.local_partial);
            assert!(load_index(&path, 540).0.legacy_files.is_empty());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn append_with_changed_prefix_does_not_certify_old_digest() {
        for tool in [ActivityTool::Claude, ActivityTool::Grok] {
            let root = temp_root("changed-prefix-append");
            let folder = root.join(if tool == ActivityTool::Claude {
                "claude"
            } else {
                "grok"
            });
            fs::create_dir_all(&folder).unwrap();
            let source = folder.join(if tool == ActivityTool::Claude {
                "session.jsonl"
            } else {
                "updates.jsonl"
            });
            let path = root.join("index.json");
            let filler = "{}\n".repeat(2500);
            fs::write(
                &source,
                anonymous_event(tool, 10) + &filler + &anonymous_event(tool, 20),
            )
            .unwrap();
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            make_cache_legacy(&path);
            let mut bounded = options();
            bounded.max_bytes = 5500;
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                bounded,
            )
            .unwrap();
            fs::write(
                &source,
                anonymous_event(tool, 90)
                    + &filler
                    + &anonymous_event(tool, 20)
                    + &anonymous_event(tool, 40),
            )
            .unwrap();
            File::options()
                .write(true)
                .open(&source)
                .unwrap()
                .set_modified(SystemTime::now() + Duration::from_secs(1))
                .unwrap();
            let mut final_result = None;
            for _ in 0..12 {
                let result = refresh_at(
                    &path,
                    &roots(&root),
                    true,
                    false,
                    true,
                    now(),
                    kst(),
                    bounded,
                )
                .unwrap();
                assert!(load_index(&path, 540)
                    .0
                    .legacy_files
                    .values()
                    .all(|legacy| !legacy.anonymous_source_verified));
                if !result.local_backfill_pending {
                    final_result = Some(result);
                    break;
                }
            }
            let result = final_result.expect("changed prefix is reread after validation");
            assert_eq!(
                result
                    .days
                    .iter()
                    .map(|day| day.claude_tokens + day.grok_tokens)
                    .sum::<u64>(),
                150
            );
            assert!(result.local_partial);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn invalid_saved_prefix_validation_restarts_proof_without_losing_collection() {
        let root = temp_root("invalid-prefix-validation");
        let source = root.join("updates.jsonl");
        fs::write(&source, anonymous_event(ActivityTool::Grok, 10)).unwrap();
        let mut checkpoint = FileCheckpoint::new(ActivityTool::Grok);
        scan_file(
            &source,
            &mut checkpoint,
            "2026-01-01",
            kst(),
            Instant::now() + Duration::from_secs(2),
            u64::MAX,
            None,
        )
        .unwrap();
        let proof = Some((checkpoint.offset, checkpoint.prefix_digest.unwrap()));
        invalidate_prefix_proof(&mut checkpoint);
        checkpoint.prefix_validation = Some(PrefixValidation {
            offset: checkpoint.offset + 1,
            digest: [0; 32],
            target_offset: checkpoint.offset,
            source_modified_nanos: modified_nanos(&fs::metadata(&source).unwrap()),
            legacy_prefix_matches: false,
        });
        let progress = validate_scanned_prefix(
            &source,
            &mut checkpoint,
            proof,
            Instant::now() + Duration::from_secs(2),
            u64::MAX,
        )
        .unwrap();
        assert!(!progress.incomplete);
        assert!(!checkpoint.prefix_needs_validation);
        assert!(checkpoint.anonymous_source_verified);
        assert_eq!(
            checkpoint
                .grok_responses
                .values()
                .map(|value| value.tokens)
                .sum::<u64>(),
            10
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_legacy_history_is_preserved_without_affecting_enabled_quality() {
        for disabled in [ActivityTool::Claude, ActivityTool::Grok] {
            let root = temp_root("disabled-legacy-quality");
            write_dst_logs(&root);
            let path = root.join("index.json");
            refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            make_cache_legacy(&path);
            let source = if disabled == ActivityTool::Claude {
                root.join("claude/session.jsonl")
            } else {
                root.join("grok/updates.jsonl")
            };
            fs::remove_file(source).unwrap();
            let filtered = refresh_at(
                &path,
                &roots(&root),
                disabled != ActivityTool::Claude,
                false,
                disabled != ActivityTool::Grok,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            assert!(!filtered.local_partial && !filtered.local_backfill_pending);
            assert!(load_index(&path, 540)
                .0
                .legacy_files
                .values()
                .any(|checkpoint| checkpoint.tool == disabled));
            let restored = refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            assert!(restored.local_partial && !restored.local_backfill_pending);
            assert!(restored
                .days
                .iter()
                .any(|day| if disabled == ActivityTool::Claude {
                    day.claude_tokens > 0
                } else {
                    day.grok_tokens > 0
                }));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn legacy_cache_without_logs_keeps_history_and_reports_uncertain_dates() {
        let root = temp_root("legacy-missing-logs");
        write_dst_logs(&root);
        let path = root.join("index.json");
        let first = refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        make_cache_legacy(&path);
        fs::remove_file(root.join("claude").join("session.jsonl")).unwrap();
        fs::remove_file(root.join("grok").join("updates.jsonl")).unwrap();
        for _ in 0..3 {
            let retained = refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            assert_eq!(retained.days, first.days);
            assert!(retained.partial && retained.local_partial);
            assert!(!retained.backfill_pending);
            let (cache, rebuilt) = load_index(&path, 540);
            assert!(!rebuilt);
            assert!(cache.files.is_empty());
            assert_eq!(cache.legacy_files.len(), 2);
        }
        let expired = refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now() + ChronoDuration::days(RETENTION_DAYS),
            kst(),
            options(),
        )
        .unwrap();
        assert!(expired.days.is_empty());
        assert!(!expired.partial);
        assert!(load_index(&path, 540).0.legacy_files.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_reconciles_known_ids_without_losing_unrecoverable_or_forked_history() {
        use chrono_tz::America::New_York;

        let root = temp_root("legacy-partial-rebuild");
        write_dst_logs(&root);
        let path = root.join("index.json");
        let fixed = FixedOffset::west_opt(4 * 3600).unwrap();
        refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            fixed,
            options(),
        )
        .unwrap();
        let mut cache = load_index(&path, -240).0;
        let claude_key = root
            .join("claude")
            .join("session.jsonl")
            .to_string_lossy()
            .into_owned();
        let grok_key = root
            .join("grok")
            .join("updates.jsonl")
            .to_string_lossy()
            .into_owned();
        let claude = cache.files.get_mut(&claude_key).unwrap();
        claude.claude_messages.insert(
            "lost-message".into(),
            ClaudeMessageContribution {
                date: "2026-01-16".into(),
                tokens: 7,
            },
        );
        add_day(&mut claude.days, "2026-01-16", 7);
        let fork = claude.clone();
        cache.files.insert("deleted-fork.jsonl".into(), fork);
        cache
            .files
            .get_mut(&grok_key)
            .unwrap()
            .grok_responses
            .insert(
                "lost-response".into(),
                GrokResponseContribution {
                    date: "2026-01-16".into(),
                    tokens: 17,
                },
            );
        cache.timezone_identity = None;
        save_index(&path, &cache).unwrap();
        let identity = test_local_identity("Eastern Standard Time");
        let mut bounded = options();
        bounded.max_bytes = 1;
        let partial = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            identity.clone(),
            bounded,
        )
        .unwrap();
        assert!(partial.partial && partial.backfill_pending);
        assert_eq!(
            partial
                .days
                .iter()
                .map(|day| day.claude_tokens)
                .sum::<u64>(),
            67
        );
        assert_eq!(
            partial.days.iter().map(|day| day.grok_tokens).sum::<u64>(),
            37
        );
        for _ in 0..3 {
            let rebuilt = refresh_in_timezone(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                New_York,
                identity.clone(),
                options(),
            )
            .unwrap();
            assert_eq!(
                rebuilt
                    .days
                    .iter()
                    .map(|day| day.claude_tokens)
                    .sum::<u64>(),
                67
            );
            assert_eq!(
                rebuilt.days.iter().map(|day| day.grok_tokens).sum::<u64>(),
                37
            );
            assert!(rebuilt.partial);
            let january = rebuilt
                .days
                .iter()
                .find(|day| day.date == "2026-01-14")
                .unwrap();
            assert_eq!(january.claude_tokens, 30);
            assert_eq!(january.grok_tokens, 10);
            assert!(!rebuilt.days.iter().any(|day| day.date == "2026-01-15"));
        }
        let (saved, _) = load_index_for_timezone(&path, -240, identity.clone());
        assert!(saved.legacy_files.values().all(|file| {
            file.claude_messages.keys().all(|id| id == "lost-message")
                && file.grok_responses.keys().all(|id| id == "lost-response")
        }));
        let original = fs::read(root.join("claude").join("session.jsonl")).unwrap();
        let recovered_claude = serde_json::json!({
            "timestamp": "2026-01-16T12:00:00Z",
            "message": { "id": "lost-message", "usage": { "input_tokens": 7 } }
        });
        let mut output = OpenOptions::new()
            .append(true)
            .open(root.join("claude").join("session.jsonl"))
            .unwrap();
        writeln!(output, "{recovered_claude}").unwrap();
        let mut grok_output = OpenOptions::new()
            .append(true)
            .open(root.join("grok").join("updates.jsonl"))
            .unwrap();
        writeln!(
            grok_output,
            "{}",
            grok_event_with_id(
                serde_json::json!("2026-01-16T12:00:00Z"),
                "session/update",
                serde_json::json!({ "input_tokens": 17 }),
                Some("lost-response"),
            )
        )
        .unwrap();
        drop((output, grok_output));
        let recovered = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            identity.clone(),
            options(),
        )
        .unwrap();
        assert!(!recovered.partial && !recovered.backfill_pending);
        assert_eq!(
            recovered
                .days
                .iter()
                .map(|day| day.claude_tokens)
                .sum::<u64>(),
            67
        );
        assert_eq!(
            recovered
                .days
                .iter()
                .map(|day| day.grok_tokens)
                .sum::<u64>(),
            37
        );
        assert!(load_index_for_timezone(&path, -240, identity)
            .0
            .legacy_files
            .is_empty());
        assert!(fs::read(root.join("claude").join("session.jsonl"))
            .unwrap()
            .starts_with(&original));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_keeps_larger_cached_claude_total_on_the_confirmed_source_date() {
        use chrono_tz::America::New_York;

        let root = temp_root("legacy-larger-total");
        let source = root.join("claude").join("session.jsonl");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let write_message = |tokens: u64| {
            fs::write(
                &source,
                serde_json::json!({
                    "timestamp": "2026-01-15T04:30:00Z",
                    "message": { "id": "same-message", "usage": { "input_tokens": tokens } }
                })
                .to_string()
                    + "\n",
            )
            .unwrap();
        };
        write_message(30);
        let path = root.join("index.json");
        let before = refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            false,
            now(),
            FixedOffset::west_opt(4 * 3600).unwrap(),
            options(),
        )
        .unwrap();
        assert_eq!(before.days[0].date, "2026-01-15");
        assert_eq!(before.days[0].claude_tokens, 30);
        make_cache_legacy(&path);
        write_message(20);
        let identity = test_local_identity("Eastern Standard Time");
        for _ in 0..3 {
            let rebuilt = refresh_in_timezone(
                &path,
                &roots(&root),
                true,
                false,
                false,
                now(),
                New_York,
                identity.clone(),
                options(),
            )
            .unwrap();
            assert_eq!(rebuilt.days.len(), 1);
            assert_eq!(rebuilt.days[0].date, "2026-01-14");
            assert_eq!(rebuilt.days[0].claude_tokens, 30);
            assert!(!rebuilt.partial);
            let (saved, _) = load_index_for_timezone(&path, -240, identity.clone());
            assert!(saved.legacy_files.is_empty());
            assert_eq!(saved.files.values().next().unwrap().days["2026-01-14"], 30);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_aggregate_only_history_is_preserved_as_lossy_backup_without_double_counting() {
        let root = temp_root("legacy-aggregate-only");
        let path = root.join("index.json");
        let source = root.join("claude").join("session.jsonl");
        let key = source.to_string_lossy().into_owned();
        let mut cache = ActivityIndex::new(540);
        cache.timezone_identity = None;
        let mut legacy = FileCheckpoint::new(ActivityTool::Claude);
        legacy.days.insert("2026-07-19".into(), 30);
        cache.files.insert(key.clone(), legacy);
        save_index(&path, &cache).unwrap();
        let retained = refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(retained.days[0].claude_tokens, 30);
        assert!(retained.partial && !retained.backfill_pending);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, concat!(
            r#"{"timestamp":"2026-07-19T01:00:00Z","message":{"id":"restored","usage":{"input_tokens":30}}}"#,
            "\n"
        )).unwrap();
        for _ in 0..3 {
            let rebuilt = refresh_at(
                &path,
                &roots(&root),
                true,
                false,
                false,
                now(),
                kst(),
                options(),
            )
            .unwrap();
            assert_eq!(rebuilt.days[0].claude_tokens, 30);
            assert!(rebuilt.partial);
            assert_eq!(
                load_index(&path, 540).0.legacy_files[&key].days["2026-07-19"],
                30
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timezone_change_during_scan_never_creates_an_index_or_returns_a_snapshot() {
        use chrono_tz::America::New_York;

        let root = temp_root("timezone-scan-abort");
        write_dst_logs(&root);
        let path = root.join("index.json");
        let expected = test_local_identity("Eastern Standard Time");
        let mut checks = 0;
        let result = refresh_in_timezone_checked(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            expected.clone(),
            options(),
            || {
                checks += 1;
                Ok(if checks >= 3 {
                    test_local_identity("SA Western Standard Time")
                } else {
                    expected.clone()
                })
            },
        );
        assert!(result.unwrap_err().to_string().contains("timezone changed"));
        assert_eq!(checks, 3);
        assert!(!path.exists());
        let recovered = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            expected,
            options(),
        )
        .unwrap();
        assert_eq!(
            recovered
                .days
                .iter()
                .map(|day| day.claude_tokens)
                .sum::<u64>(),
            60
        );
        assert_eq!(
            recovered
                .days
                .iter()
                .map(|day| day.grok_tokens)
                .sum::<u64>(),
            20
        );
        assert!(!recovered.partial);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn final_timezone_validation_preserves_existing_legacy_cache_on_change_or_failure() {
        use chrono_tz::America::New_York;

        let root = temp_root("timezone-migration-abort");
        write_dst_logs(&root);
        let path = root.join("index.json");
        refresh_at(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        make_cache_legacy(&path);
        fs::remove_file(root.join("claude").join("session.jsonl")).unwrap();
        fs::remove_file(root.join("grok").join("updates.jsonl")).unwrap();
        let original = fs::read(&path).unwrap();
        let identity = test_local_identity("Eastern Standard Time");
        for unreadable in [false, true] {
            let mut checks = 0;
            let result = refresh_in_timezone_checked(
                &path,
                &roots(&root),
                true,
                false,
                true,
                now(),
                New_York,
                identity.clone(),
                options(),
                || {
                    checks += 1;
                    if checks == 3 {
                        if unreadable {
                            anyhow::bail!("fixture timezone unavailable");
                        }
                        return Ok(test_local_identity("SA Western Standard Time"));
                    }
                    Ok(identity.clone())
                },
            );
            assert!(result.is_err());
            assert_eq!(checks, 3);
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        let retained = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            identity,
            options(),
        )
        .unwrap();
        assert_eq!(
            retained
                .days
                .iter()
                .map(|day| day.claude_tokens)
                .sum::<u64>(),
            60
        );
        assert_eq!(
            retained.days.iter().map(|day| day.grok_tokens).sum::<u64>(),
            20
        );
        assert!(retained.partial && !retained.backfill_pending);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_current_offset_timezone_switch_rebuilds_and_daylight_setting_invalidates() {
        use chrono_tz::America::{New_York, Puerto_Rico};

        let root = temp_root("dst-timezone-switch");
        write_dst_logs(&root);
        let path = root.join("index.json");
        let eastern = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            New_York,
            test_local_identity("Eastern Standard Time"),
            options(),
        )
        .unwrap();
        let identity = test_local_identity("SA Western Standard Time");
        let (empty, rebuilt) = load_index_for_timezone(&path, -240, identity.clone());
        assert!(rebuilt && empty.files.is_empty());
        let switched = refresh_in_timezone(
            &path,
            &roots(&root),
            true,
            false,
            true,
            now(),
            Puerto_Rico,
            identity,
            options(),
        )
        .unwrap();
        assert_eq!(
            switched.timezone_offset_minutes,
            eastern.timezone_offset_minutes
        );
        assert_eq!(eastern.days[0].date, "2026-01-14");
        assert_eq!(switched.days[0].date, "2026-01-15");
        assert_eq!(switched.days[0].claude_tokens, 30);
        assert_eq!(switched.days[0].grok_tokens, 10);
        let (empty, rebuilt) = load_index_for_timezone(
            &path,
            -240,
            ActivityTimezoneIdentity::Local {
                key: "SA Western Standard Time".into(),
                dynamic_daylight_disabled: true,
            },
        );
        assert!(rebuilt && empty.files.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_deduplicates_message_ids_and_keeps_the_largest_total() {
        let root = temp_root("claude-dedupe");
        let claude = root.join("claude");
        fs::create_dir_all(&claude).unwrap();
        let file = claude.join("session.jsonl");
        fs::write(
            &file,
            concat!(
                r#"{"timestamp":"2026-07-18T14:59:00Z","message":{"id":"m1","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":30,"cache_read_input_tokens":40}}}"#,
                "\n",
                r#"{"timestamp":"2026-07-18T14:59:00Z","message":{"id":"m1","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":30,"cache_read_input_tokens":40}}}"#,
                "\n",
                r#"{"timestamp":"2026-07-18T15:01:00Z","message":{"id":"m1","usage":{"input_tokens":20,"output_tokens":30,"cache_creation_input_tokens":40,"cache_read_input_tokens":50}}}"#,
                "\n",
                r#"{"timestamp":"2026-07-18T15:02:00Z","message":{"id":"m2","usage":{"input_tokens":3,"output_tokens":7}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days.len(), 1);
        assert_eq!(snapshot.days[0].date, "2026-07-19");
        assert_eq!(snapshot.days[0].claude_tokens, 150);
        assert_eq!(snapshot.days[0].codex_tokens, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_sums_iterations_and_deduplicates_messages_across_files() {
        let root = temp_root("claude-iterations");
        let claude = root.join("claude");
        fs::create_dir_all(&claude).unwrap();
        let message = |iterations: Value| {
            serde_json::json!({
                "timestamp": "2026-07-19T01:00:00Z",
                "message": {
                    "id": "shared-message",
                    "usage": {
                        "input_tokens": 1,
                        "output_tokens": 1,
                        "cache_creation_input_tokens": 1,
                        "cache_read_input_tokens": 1,
                        "iterations": iterations
                    }
                }
            })
            .to_string()
                + "\n"
        };
        fs::write(
            claude.join("session-a.jsonl"),
            message(serde_json::json!([{
                "input_tokens": 10,
                "output_tokens": 20,
                "cache_creation_input_tokens": 30,
                "cache_read_input_tokens": 40
            }])),
        )
        .unwrap();
        fs::write(
            claude.join("session-b.jsonl"),
            message(serde_json::json!([
                {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "cache_creation_input_tokens": 30,
                    "cache_read_input_tokens": 40
                },
                {
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "cache_creation_input_tokens": 3,
                    "cache_read_input_tokens": 4
                }
            ])),
        )
        .unwrap();
        let no_id_message = serde_json::json!({
            "timestamp": "2026-07-19T01:00:00Z",
            "message": {"usage": {"input_tokens": 2, "output_tokens": 2}}
        })
        .to_string()
            + "\n";
        fs::write(claude.join("no-id-a.jsonl"), &no_id_message).unwrap();
        fs::write(claude.join("no-id-b.jsonl"), &no_id_message).unwrap();

        let index = root.join("index.json");
        let first = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        let repeated = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(first.days[0].claude_tokens, 118);
        assert_eq!(repeated.days[0].claude_tokens, 118);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_uses_positive_cumulative_deltas_and_handles_counter_reset() {
        let root = temp_root("codex-delta");
        let codex = root.join("codex");
        fs::create_dir_all(&codex).unwrap();
        let file = codex.join("rollout.jsonl");
        let event = |timestamp: &str, total: u64| {
            serde_json::json!({
                "timestamp": timestamp,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {"total_token_usage": {"total_tokens": total}}
                }
            })
            .to_string()
        };
        fs::write(
            &file,
            [
                serde_json::json!({
                    "timestamp": "2026-07-18T14:58:00Z",
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "info": {"last_token_usage": {"total_tokens": 20}}
                    }
                })
                .to_string(),
                event("2026-07-18T14:59:00Z", 100),
                event("2026-07-18T15:00:00Z", 100),
                event("2026-07-18T15:01:00Z", 250),
                event("2026-07-18T15:02:00Z", 40),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            false,
            true,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days.len(), 2);
        assert_eq!(snapshot.days[0].date, "2026-07-18");
        assert_eq!(snapshot.days[0].codex_tokens, 100);
        assert_eq!(snapshot.days[1].date, "2026-07-19");
        assert_eq!(snapshot.days[1].codex_tokens, 190);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_uses_last_usage_for_resumed_baselines_and_counter_resets() {
        let root = temp_root("codex-resumed-baseline");
        let codex = root.join("codex");
        fs::create_dir_all(&codex).unwrap();
        let event = |total: u64, last: u64| {
            serde_json::json!({
                "timestamp": "2026-07-19T01:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {"total_tokens": total},
                        "last_token_usage": {"total_tokens": last}
                    }
                }
            })
            .to_string()
        };
        fs::write(
            codex.join("rollout.jsonl"),
            [
                event(201_791_695, 203_203),
                event(201_791_695, 203_203),
                event(202_005_633, 213_938),
                event(500, 100),
                event(650, 150),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            false,
            true,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days[0].codex_tokens, 417_391);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_append_is_idempotent_and_retries_an_incomplete_line() {
        let root = temp_root("incremental");
        let codex = root.join("codex");
        fs::create_dir_all(&codex).unwrap();
        let file = codex.join("rollout.jsonl");
        fs::write(
            &file,
            concat!(
                r#"{"timestamp":"2026-07-19T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":100}}}}"#,
                "\n",
                r#"{"timestamp":"2026-07-19T02:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":"#
            ),
        )
        .unwrap();
        let index = root.join("index.json");

        let first = refresh_at(
            &index,
            &roots(&root),
            false,
            true,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(first.days[0].codex_tokens, 100);
        assert!(first.partial);
        assert!(first.backfill_pending);

        let unchanged = refresh_at(
            &index,
            &roots(&root),
            false,
            true,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(unchanged.days[0].codex_tokens, 100);

        let mut output = OpenOptions::new().append(true).open(&file).unwrap();
        output.write_all(b"{\"total_tokens\":175}}}}\n").unwrap();
        drop(output);
        let appended = refresh_at(
            &index,
            &roots(&root),
            false,
            true,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(appended.days[0].codex_tokens, 175);
        assert!(!appended.backfill_pending);

        let repeated = refresh_at(
            &index,
            &roots(&root),
            false,
            true,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(repeated.days[0].codex_tokens, 175);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activity_day_defaults_grok_tokens_for_legacy_payloads() {
        let day: ActivityDay = serde_json::from_value(serde_json::json!({
            "date": "2026-07-19",
            "claude_tokens": 10,
            "codex_tokens": 20
        }))
        .unwrap();

        assert_eq!(day.grok_tokens, 0);
        assert_eq!(day.cursor_tokens, 0);
    }

    #[test]
    fn cursor_account_activity_merges_without_changing_local_source_state() {
        let local = ActivitySnapshot {
            schema_version: SCHEMA_VERSION.into(),
            generated_at: now().to_rfc3339(),
            timezone_offset_minutes: 540,
            partial: false,
            backfill_pending: false,
            local_partial: false,
            local_backfill_pending: false,
            codex_partial: false,
            codex_backfill_pending: false,
            codex_account_scope: false,
            cursor_partial: false,
            cursor_backfill_pending: false,
            cursor_account_scope: false,
            days: vec![ActivityDay {
                date: "2026-07-19".into(),
                claude_tokens: 10,
                ..ActivityDay::default()
            }],
        };
        let cursor = crate::cursor_activity::CursorActivityView {
            days: BTreeMap::from([("2026-07-19".into(), 90), ("2026-07-20".into(), 50)]),
            partial: true,
            backfill_pending: false,
            scope: crate::cursor_dashboard::AccountScope {
                user_id: 1,
                team_id: None,
            },
        };
        let merged = merge_cursor_activity(local, Some(&cursor), true);
        assert_eq!(merged.days[0].claude_tokens, 10);
        assert_eq!(merged.days[0].cursor_tokens, 90);
        assert_eq!(merged.days[1].cursor_tokens, 50);
        assert!(!merged.local_partial);
        assert!(merged.cursor_partial);
        assert!(merged.partial);
        assert!(merged.cursor_account_scope);
    }

    #[test]
    fn codex_account_activity_replaces_stale_local_checkpoint_values() {
        let local = ActivitySnapshot {
            schema_version: SCHEMA_VERSION.into(),
            generated_at: now().to_rfc3339(),
            timezone_offset_minutes: 540,
            partial: false,
            backfill_pending: false,
            local_partial: false,
            local_backfill_pending: false,
            codex_partial: false,
            codex_backfill_pending: false,
            codex_account_scope: false,
            cursor_partial: false,
            cursor_backfill_pending: false,
            cursor_account_scope: false,
            days: vec![ActivityDay {
                date: "2026-07-19".into(),
                claude_tokens: 10,
                codex_tokens: 999,
                ..ActivityDay::default()
            }],
        };
        let codex = crate::codex_activity::CodexActivityView {
            days: BTreeMap::from([("2026-07-19".into(), 20), ("2026-07-20".into(), 30)]),
            partial: false,
        };

        let merged = merge_codex_activity(local, Some(&codex), true);
        assert_eq!(merged.days[0].claude_tokens, 10);
        assert_eq!(merged.days[0].codex_tokens, 20);
        assert_eq!(merged.days[1].codex_tokens, 30);
        assert!(merged.codex_account_scope);
        assert!(!merged.codex_partial);
        assert!(!merged.partial);
    }

    #[test]
    fn unavailable_codex_account_activity_never_falls_back_to_local_rollout_tokens() {
        let local = ActivitySnapshot {
            schema_version: SCHEMA_VERSION.into(),
            generated_at: now().to_rfc3339(),
            timezone_offset_minutes: 540,
            partial: false,
            backfill_pending: false,
            local_partial: false,
            local_backfill_pending: false,
            codex_partial: false,
            codex_backfill_pending: false,
            codex_account_scope: false,
            cursor_partial: false,
            cursor_backfill_pending: false,
            cursor_account_scope: false,
            days: vec![ActivityDay {
                date: "2026-07-19".into(),
                codex_tokens: 999,
                ..ActivityDay::default()
            }],
        };

        let merged = merge_codex_activity(local, None, true);
        assert_eq!(merged.days[0].codex_tokens, 0);
        assert!(merged.codex_account_scope);
        assert!(merged.codex_partial);
        assert!(merged.partial);
    }

    #[test]
    fn disabling_the_retired_local_codex_source_removes_stored_checkpoints() {
        let root = temp_root("retired-codex-source");
        let index_path = root.join("index.json");
        let mut index = ActivityIndex::new(9 * 60);
        let mut codex = FileCheckpoint::new(ActivityTool::Codex);
        codex.days.insert("2026-07-19".into(), 999);
        index.files.insert("old-codex-rollout".into(), codex);
        fs::create_dir_all(&root).unwrap();
        fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();

        let snapshot = refresh_at(
            &index_path,
            &ActivityRoots {
                claude: None,
                codex: None,
                grok: None,
            },
            false,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        let saved: ActivityIndex = serde_json::from_slice(&fs::read(&index_path).unwrap()).unwrap();

        assert!(saved.files.is_empty());
        assert!(snapshot.days.iter().all(|day| day.codex_tokens == 0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grok_sums_completed_responses_without_double_counting_reasoning() {
        let root = temp_root("grok-responses");
        let grok = root.join("grok").join("workspace").join("session");
        fs::create_dir_all(&grok).unwrap();
        let timestamp = Utc.with_ymd_and_hms(2026, 7, 19, 1, 0, 0).unwrap();
        let events = [
            grok_event(
                serde_json::json!(timestamp.timestamp_millis()),
                "session/update",
                serde_json::json!({
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "cache_read_input_tokens": 30,
                    "cache_creation_input_tokens": 40,
                    "reasoning_tokens": 999
                }),
            ),
            grok_event(
                serde_json::json!(timestamp.to_rfc3339()),
                "_x.ai/session/update",
                serde_json::json!({
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 4
                }),
            ),
            grok_event(
                serde_json::json!(timestamp.timestamp()),
                "_x.ai/session/update",
                serde_json::json!({"input_tokens": 5, "output_tokens": 6}),
            ),
            serde_json::json!({
                "timestamp": timestamp.timestamp_millis(),
                "method": "_x.ai/session/update",
                "params": {"update": {"sessionUpdate": "response_started", "usage": {"input_tokens": 1000}}}
            })
            .to_string(),
            grok_event(
                serde_json::json!(timestamp.timestamp_millis()),
                "other/update",
                serde_json::json!({"input_tokens": 1000}),
            ),
            grok_event(
                serde_json::json!("not-a-timestamp"),
                "_x.ai/session/update",
                serde_json::json!({"input_tokens": 1000}),
            ),
            grok_event(
                serde_json::json!(timestamp.timestamp_millis()),
                "_x.ai/session/update",
                serde_json::json!({"inputTokens": 1000, "outputTokens": 1000}),
            ),
        ];
        fs::write(grok.join("updates.jsonl"), events.join("\n") + "\n").unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days.len(), 1);
        assert_eq!(snapshot.days[0].date, "2026-07-19");
        assert_eq!(snapshot.days[0].grok_tokens, 121);
        assert_eq!(snapshot.days[0].claude_tokens, 0);
        assert_eq!(snapshot.days[0].codex_tokens, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grok_incremental_append_is_idempotent_and_retries_an_incomplete_line() {
        let root = temp_root("grok-incremental");
        let grok = root.join("grok").join("session");
        fs::create_dir_all(&grok).unwrap();
        let file = grok.join("updates.jsonl");
        let first_event = grok_event(
            serde_json::json!("2026-07-19T01:00:00Z"),
            "_x.ai/session/update",
            serde_json::json!({"input_tokens": 4, "output_tokens": 6}),
        );
        let second_event = grok_event(
            serde_json::json!("2026-07-19T02:00:00Z"),
            "_x.ai/session/update",
            serde_json::json!({
                "input_tokens": 2,
                "output_tokens": 3,
                "cache_read_input_tokens": 4,
                "cache_creation_input_tokens": 5
            }),
        );
        let split = second_event.len() / 2;
        fs::write(&file, format!("{first_event}\n{}", &second_event[..split])).unwrap();
        let index = root.join("index.json");

        let first = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(first.days[0].grok_tokens, 10);
        assert!(first.partial);
        assert!(first.backfill_pending);

        let unchanged = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(unchanged.days[0].grok_tokens, 10);

        let mut output = OpenOptions::new().append(true).open(&file).unwrap();
        output
            .write_all(format!("{}\n", &second_event[split..]).as_bytes())
            .unwrap();
        drop(output);
        let appended = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(appended.days[0].grok_tokens, 24);
        assert!(!appended.backfill_pending);

        let repeated = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(repeated.days[0].grok_tokens, 24);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grok_scans_only_updates_jsonl() {
        let root = temp_root("grok-updates-only");
        let grok = root.join("grok").join("session");
        fs::create_dir_all(&grok).unwrap();
        let event = grok_event(
            serde_json::json!("2026-07-19T01:00:00Z"),
            "_x.ai/session/update",
            serde_json::json!({"input_tokens": 7, "output_tokens": 3}),
        ) + "\n";
        fs::write(grok.join("updates.jsonl"), &event).unwrap();
        fs::write(grok.join("chat_history.jsonl"), &event).unwrap();
        fs::write(grok.join("other.jsonl"), &event).unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days[0].grok_tokens, 10);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grok_deduplicates_forked_responses_by_persisted_event_id() {
        let root = temp_root("grok-fork-dedupe");
        let original = root.join("grok").join("original");
        let fork = root.join("grok").join("fork");
        fs::create_dir_all(&original).unwrap();
        fs::create_dir_all(&fork).unwrap();
        let event = grok_event_with_id(
            serde_json::json!("2026-07-19T01:00:00Z"),
            "_x.ai/session/update",
            serde_json::json!({"input_tokens": 7, "output_tokens": 3}),
            Some("original-session-42"),
        ) + "\n";
        fs::write(original.join("updates.jsonl"), &event).unwrap();
        fs::write(fork.join("updates.jsonl"), &event).unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days[0].grok_tokens, 10);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grok_keeps_idless_legacy_responses_from_different_files() {
        let root = temp_root("grok-idless-files");
        let first = root.join("grok").join("first");
        let second = root.join("grok").join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let event = grok_event(
            serde_json::json!("2026-07-19T01:00:00Z"),
            "_x.ai/session/update",
            serde_json::json!({"input_tokens": 7, "output_tokens": 3}),
        ) + "\n";
        fs::write(first.join("updates.jsonl"), &event).unwrap();
        fs::write(second.join("updates.jsonl"), &event).unwrap();

        let snapshot = refresh_at(
            &root.join("index.json"),
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days[0].grok_tokens, 20);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_order_round_robins_three_tools_and_keeps_each_newest_first() {
        let ordered = fair_candidate_order(vec![
            candidate(ActivityTool::Claude, "claude-old", 20),
            candidate(ActivityTool::Codex, "codex-old", 10),
            candidate(ActivityTool::Codex, "codex-new", 40),
            candidate(ActivityTool::Claude, "claude-new", 30),
            candidate(ActivityTool::Grok, "grok-old", 5),
            candidate(ActivityTool::Grok, "grok-new", 50),
        ]);
        let keys = ordered
            .iter()
            .map(|candidate| candidate.key.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec![
                "claude-new",
                "codex-new",
                "grok-new",
                "claude-old",
                "codex-old",
                "grok-old"
            ]
        );
    }

    #[test]
    fn candidate_order_preserves_the_enumeration_starting_tool() {
        let ordered = fair_candidate_order(vec![
            candidate(ActivityTool::Grok, "grok", 10),
            candidate(ActivityTool::Claude, "claude", 20),
        ]);
        assert_eq!(ordered[0].tool, ActivityTool::Grok);
    }

    #[test]
    fn existing_v3_index_keeps_history_without_an_enumeration_checkpoint() {
        let mut index = ActivityIndex::new(540);
        let mut file = FileCheckpoint::new(ActivityTool::Claude);
        file.days.insert("2026-07-19".into(), 30);
        index.files.insert("session.jsonl".into(), file);
        let mut value = serde_json::to_value(&index).unwrap();
        value.as_object_mut().unwrap().remove("enumeration");
        let restored: ActivityIndex = serde_json::from_value(value).unwrap();
        assert_eq!(restored, index);
    }

    fn write_enumeration_logs(root: &Path, claude_files: usize) {
        fs::create_dir_all(root.join("claude")).unwrap();
        fs::create_dir_all(root.join("grok")).unwrap();
        for i in 0..claude_files {
            let line = serde_json::json!({
                "timestamp": "2026-07-19T01:00:00Z",
                "message": {
                    "id": format!("message-{i}"),
                    "usage": { "input_tokens": 10, "output_tokens": 20 }
                }
            });
            fs::write(
                root.join("claude").join(format!("{i}.jsonl")),
                format!("{line}\n"),
            )
            .unwrap();
        }
        fs::write(
            root.join("grok").join("updates.jsonl"),
            grok_event(
                serde_json::json!("2026-07-19T01:00:00Z"),
                "_x.ai/session/update",
                serde_json::json!({ "input_tokens": 7, "output_tokens": 3 }),
            ) + "\n",
        )
        .unwrap();
    }

    #[test]
    fn one_file_budget_collects_both_providers_and_finishes_the_sweep() {
        let root = temp_root("enumeration-provider-fairness");
        write_enumeration_logs(&root, 1);
        let mut scan_options = options();
        scan_options.max_files = 1;
        let index = root.join("index.json");
        let mut complete = false;
        for pass in 0..6 {
            let snapshot = refresh_at(
                &index,
                &roots(&root),
                true,
                false,
                true,
                now(),
                kst(),
                scan_options,
            )
            .unwrap();
            if pass == 1 {
                assert_eq!(snapshot.days[0].claude_tokens, 30);
                assert_eq!(snapshot.days[0].grok_tokens, 10);
            }
            if !snapshot.backfill_pending {
                complete = true;
                break;
            }
        }
        assert!(complete);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_file_budget_resumes_all_files_of_the_same_tool_without_double_counting() {
        let root = temp_root("enumeration-file-fairness");
        write_enumeration_logs(&root, 4);
        let mut scan_options = options();
        scan_options.max_files = 1;
        let index = root.join("index.json");
        let mut completed_sweeps = 0;
        for _ in 0..20 {
            let snapshot = refresh_at(
                &index,
                &roots(&root),
                true,
                false,
                false,
                now(),
                kst(),
                scan_options,
            )
            .unwrap();
            if !snapshot.backfill_pending {
                assert_eq!(snapshot.days[0].claude_tokens, 120);
                assert_eq!(snapshot.days[0].grok_tokens, 0);
                completed_sweeps += 1;
                if completed_sweeps == 2 {
                    break;
                }
            }
        }
        assert_eq!(completed_sweeps, 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enumeration_resumes_entry_budget_through_nested_directories_and_reopen() {
        let root = temp_root("enumeration-entry-resume");
        write_enumeration_logs(&root, 4);
        let nested = root.join("claude").join("nested").join("deeper");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("last.jsonl"), "{}\n").unwrap();
        fs::write(root.join("claude").join("ignored.txt"), "ignored").unwrap();
        let mut checkpoint = EnumerationCheckpoint::default();
        let mut readers = std::array::from_fn(|_| Vec::new());
        let mut scan_options = options();
        scan_options.max_entries = 1;
        let mut seen = BTreeSet::new();
        let mut grok_seen = false;
        let mut complete = false;
        for pass in 0..100 {
            let (candidates, partial) = collect_candidates(
                &roots(&root),
                [true, false, true],
                "2026-01-01",
                kst(),
                Instant::now() + Duration::from_secs(2),
                scan_options,
                &mut checkpoint,
                &mut readers,
            );
            assert!(candidates.len() <= 1);
            for candidate in candidates {
                grok_seen |= candidate.tool == ActivityTool::Grok;
                assert!(seen.insert(candidate.key));
            }
            if pass == 1 {
                assert!(grok_seen, "Claude must not monopolize the entry budget");
            }
            if pass == 3 {
                readers = std::array::from_fn(|_| Vec::new());
            }
            // Simulate disk checkpoints without discarding live reader progress.
            checkpoint = serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
            if !partial {
                complete = true;
                break;
            }
        }
        assert!(complete);
        assert_eq!(seen.len(), 6);
        assert!(readers.iter().all(Vec::is_empty));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enumeration_deadline_keeps_the_resume_position() {
        let root = temp_root("enumeration-deadline");
        write_enumeration_logs(&root, 2);
        let mut checkpoint = EnumerationCheckpoint::default();
        let mut readers = std::array::from_fn(|_| Vec::new());
        let mut scan_options = options();
        scan_options.max_entries = 1;
        collect_candidates(
            &roots(&root),
            [true, false, true],
            "2026-01-01",
            kst(),
            Instant::now() + Duration::from_secs(2),
            scan_options,
            &mut checkpoint,
            &mut readers,
        );
        let before = checkpoint.clone();
        let (candidates, partial) = collect_candidates(
            &roots(&root),
            [true, false, true],
            "2026-01-01",
            kst(),
            Instant::now(),
            scan_options,
            &mut checkpoint,
            &mut readers,
        );
        assert!(partial);
        assert!(candidates.is_empty());
        assert_eq!(checkpoint, before);
        let (candidates, _) = collect_candidates(
            &roots(&root),
            [true, false, true],
            "2026-01-01",
            kst(),
            Instant::now() + Duration::from_secs(2),
            scan_options,
            &mut checkpoint,
            &mut readers,
        );
        assert_eq!(candidates[0].tool, ActivityTool::Grok);
        drop(readers);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deferred_candidates_are_scanned_before_enumerating_and_disabled_ones_are_dropped() {
        let root = temp_root("enumeration-deferred");
        write_enumeration_logs(&root, 1);
        let index = root.join("index.json");
        let mut scan_options = options();
        scan_options.max_bytes = 0;
        let snapshot = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            true,
            now(),
            kst(),
            scan_options,
        )
        .unwrap();
        assert!(snapshot.backfill_pending);
        assert!(snapshot.days.is_empty());
        let (saved, _) = load_index(&index, 540);
        assert_eq!(saved.enumeration.pending.len(), 2);

        scan_options = options();
        scan_options.max_duration = Duration::ZERO;
        refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            true,
            now(),
            kst(),
            scan_options,
        )
        .unwrap();
        let (paused, _) = load_index(&index, 540);
        assert_eq!(paused.enumeration.pending, saved.enumeration.pending);

        scan_options = options();
        scan_options.max_entries = 0;
        let snapshot = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            scan_options,
        )
        .unwrap();
        assert_eq!(snapshot.days[0].claude_tokens, 0);
        assert_eq!(snapshot.days[0].grok_tokens, 10);
        let (saved, _) = load_index(&index, 540);
        assert!(saved.enumeration.pending.is_empty());
        assert!(saved
            .files
            .values()
            .all(|file| file.tool == ActivityTool::Grok));
        fs::write(root.join("grok").join("updates.jsonl"), "not-json\n").unwrap();
        let not_enumerated = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            true,
            now(),
            kst(),
            scan_options,
        )
        .unwrap();
        assert_eq!(not_enumerated.days, snapshot.days);
        assert!(not_enumerated.backfill_pending);
        assert_eq!(load_index(&index, 540).0.files, saved.files);
        let disabled = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(disabled.days, snapshot.days);
        assert!(!disabled.partial);
        assert_eq!(load_index(&index, 540).0.files, saved.files);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unseen_message_contributions_are_preserved_until_retention_expires() {
        let root = temp_root("enumeration-history-retention");
        write_enumeration_logs(&root, 1);
        let index = root.join("index.json");
        let initial = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            true,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(initial.days.len(), 1);
        assert_eq!(initial.days[0].claude_tokens, 30);
        assert_eq!(initial.days[0].grok_tokens, 10);

        let retained = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            false,
            now() + ChronoDuration::days(RETENTION_DAYS - 1),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(retained.days, initial.days);
        assert_eq!(load_index(&index, 540).0.files.len(), 2);

        let expired = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            false,
            now() + ChronoDuration::days(RETENTION_DAYS),
            kst(),
            options(),
        )
        .unwrap();
        assert!(expired.days.is_empty());
        assert!(load_index(&index, 540).0.files.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_index_is_rebuilt_without_touching_source_logs() {
        let root = temp_root("corrupt-index");
        let claude = root.join("claude");
        fs::create_dir_all(&claude).unwrap();
        let source = claude.join("session.jsonl");
        let contents = concat!(
            r#"{"timestamp":"2026-07-19T01:00:00Z","message":{"id":"m1","usage":{"input_tokens":12,"output_tokens":8}}}"#,
            "\n"
        );
        fs::write(&source, contents).unwrap();
        let index = root.join("index.json");
        fs::write(&index, "{broken").unwrap();

        let snapshot = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();

        assert_eq!(snapshot.days[0].claude_tokens, 20);
        assert_eq!(fs::read_to_string(&source).unwrap(), contents);
        let saved: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], SCHEMA_VERSION);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v2_schema_index_is_rebuilt_from_source_logs() {
        let root = temp_root("stale-schema");
        let claude = root.join("claude");
        fs::create_dir_all(&claude).unwrap();
        let source = claude.join("session.jsonl");
        let contents = concat!(
            r#"{"timestamp":"2026-07-19T01:00:00Z","message":{"id":"m1","usage":{"input_tokens":12,"output_tokens":8}}}"#,
            "\n"
        );
        fs::write(&source, contents).unwrap();
        let index = root.join("index.json");
        let mut stale = ActivityIndex::new(9 * 60);
        stale.schema_version = "usage_activity.v2".into();
        let mut checkpoint = FileCheckpoint::new(ActivityTool::Claude);
        checkpoint.days.insert("2026-07-19".into(), 999_999);
        stale.files.insert("stale-file".into(), checkpoint);
        fs::write(&index, serde_json::to_vec(&stale).unwrap()).unwrap();

        let snapshot = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();

        assert_eq!(snapshot.days[0].claude_tokens, 20);
        assert_eq!(fs::read_to_string(&source).unwrap(), contents);
        assert_eq!(saved["schema_version"], SCHEMA_VERSION);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_tool_is_not_scanned_but_previous_history_is_preserved() {
        let root = temp_root("disabled-tool");
        let claude = root.join("claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("session.jsonl"),
            concat!(
                r#"{"timestamp":"2026-07-19T01:00:00Z","message":{"id":"m1","usage":{"input_tokens":5,"output_tokens":5}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let index = root.join("index.json");
        let enabled = refresh_at(
            &index,
            &roots(&root),
            true,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(enabled.days[0].claude_tokens, 10);

        fs::write(claude.join("session.jsonl"), "not-json\n").unwrap();
        let disabled = refresh_at(
            &index,
            &roots(&root),
            false,
            false,
            false,
            now(),
            kst(),
            options(),
        )
        .unwrap();
        assert_eq!(disabled.days[0].claude_tokens, 10);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "reads locally installed Claude, Codex, and Grok session logs"]
    fn live_local_logs_complete_within_the_bounded_scanner() {
        let root = temp_root("live-local");
        let index = root.join("index.json");
        let timezone = Local::now().offset().fix();
        let started = Instant::now();
        let mut snapshot = refresh_at(
            &index,
            &local_roots(),
            true,
            true,
            true,
            Utc::now(),
            timezone,
            ScanOptions::default(),
        )
        .unwrap();
        let mut passes = 1usize;
        while snapshot.backfill_pending
            && passes < 16
            && started.elapsed() < Duration::from_secs(30)
        {
            snapshot = refresh_at(
                &index,
                &local_roots(),
                true,
                true,
                true,
                Utc::now(),
                timezone,
                ScanOptions::default(),
            )
            .unwrap();
            passes += 1;
        }
        let claude = snapshot
            .days
            .iter()
            .fold(0u64, |total, day| total.saturating_add(day.claude_tokens));
        let codex = snapshot
            .days
            .iter()
            .fold(0u64, |total, day| total.saturating_add(day.codex_tokens));
        let grok = snapshot
            .days
            .iter()
            .fold(0u64, |total, day| total.saturating_add(day.grok_tokens));

        assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
        assert!(index.is_file());
        assert!(snapshot
            .days
            .windows(2)
            .all(|days| days[0].date < days[1].date));
        eprintln!(
            "activity live scan: elapsed={:?}, passes={}, days={}, claude_tokens={}, codex_tokens={}, grok_tokens={}, partial={}, backfill_pending={}",
            started.elapsed(),
            passes,
            snapshot.days.len(),
            claude,
            codex,
            grok,
            snapshot.partial,
            snapshot.backfill_pending
        );
        fs::remove_dir_all(root).unwrap();
    }
}
