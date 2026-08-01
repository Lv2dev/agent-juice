use crate::{config, paths};
use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Local, Offset, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

const SCHEMA_VERSION: &str = "usage_activity.v2";
const INDEX_FILE_NAME: &str = "usage-activity-v1.json";
const RETENTION_DAYS: i64 = 371;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const TAIL_FINGERPRINT_BYTES: u64 = 4096;
const DEFAULT_MAX_FILES: usize = 4096;
const DEFAULT_MAX_ENTRIES: usize = 32_768;
const DEFAULT_MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILE_BYTES_PER_PASS: u64 = 32 * 1024 * 1024;
const DEFAULT_SCAN_DURATION: Duration = Duration::from_secs(3);

static SCAN_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ActivityDay {
    pub date: String,
    pub claude_tokens: u64,
    pub codex_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub schema_version: String,
    pub generated_at: String,
    pub timezone_offset_minutes: i32,
    pub partial: bool,
    pub backfill_pending: bool,
    pub days: Vec<ActivityDay>,
}

#[derive(Debug, Clone)]
pub struct ActivityRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ClaudeMessageContribution {
    date: String,
    tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FileCheckpoint {
    tool: ActivityTool,
    offset: u64,
    observed_len: u64,
    modified_millis: u64,
    tail_fingerprint: u64,
    #[serde(default)]
    lossy: bool,
    #[serde(default)]
    days: BTreeMap<String, u64>,
    #[serde(default)]
    claude_messages: BTreeMap<String, ClaudeMessageContribution>,
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
            tail_fingerprint: 0,
            lossy: false,
            days: BTreeMap::new(),
            claude_messages: BTreeMap::new(),
            codex_last_total: None,
            codex_fallback_total: 0,
        }
    }

    fn prune(&mut self, cutoff: &str) {
        self.days.retain(|date, _| date.as_str() >= cutoff);
        self.claude_messages
            .retain(|_, contribution| contribution.date.as_str() >= cutoff);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ActivityIndex {
    schema_version: String,
    timezone_offset_minutes: i32,
    #[serde(default)]
    files: BTreeMap<String, FileCheckpoint>,
}

impl ActivityIndex {
    fn new(timezone_offset_minutes: i32) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.into(),
            timezone_offset_minutes,
            files: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct CandidateFile {
    path: PathBuf,
    key: String,
    tool: ActivityTool,
    modified_millis: u64,
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
        codex: home.map(|path| path.join(".codex").join("sessions")),
    }
}

pub fn refresh(show_claude: bool, show_codex: bool) -> anyhow::Result<ActivitySnapshot> {
    let _guard = SCAN_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let path = index_path().ok_or_else(|| anyhow::anyhow!("activity data path unavailable"))?;
    let timezone = Local::now().offset().fix();
    refresh_at(
        &path,
        &local_roots(),
        show_claude,
        show_codex,
        Utc::now(),
        timezone,
        ScanOptions::default(),
    )
}

pub fn refresh_at(
    index_path: &Path,
    roots: &ActivityRoots,
    show_claude: bool,
    show_codex: bool,
    now: DateTime<Utc>,
    timezone: FixedOffset,
    options: ScanOptions,
) -> anyhow::Result<ActivitySnapshot> {
    let offset_minutes = timezone.local_minus_utc() / 60;
    let (mut index, rebuilt) = load_index(index_path, offset_minutes);
    let original = index.clone();
    let deadline = Instant::now() + options.max_duration;
    let cutoff = (now.with_timezone(&timezone).date_naive()
        - ChronoDuration::days(RETENTION_DAYS - 1))
    .format("%Y-%m-%d")
    .to_string();

    let (mut candidates, enumeration_partial) = collect_candidates(
        roots,
        show_claude,
        show_codex,
        &cutoff,
        timezone,
        deadline,
        options,
    );
    candidates = fair_candidate_order(candidates);

    let mut progress = ScanProgress {
        incomplete: enumeration_partial,
        lossy: false,
        bytes_read: 0,
    };
    let seen_files = candidates
        .iter()
        .map(|candidate| candidate.key.clone())
        .collect::<BTreeSet<_>>();

    for candidate in candidates {
        if Instant::now() >= deadline || progress.bytes_read >= options.max_bytes {
            progress.incomplete = true;
            break;
        }
        let checkpoint = index
            .files
            .entry(candidate.key)
            .or_insert_with(|| FileCheckpoint::new(candidate.tool));
        if checkpoint.tool != candidate.tool {
            *checkpoint = FileCheckpoint::new(candidate.tool);
        }
        match scan_file(
            &candidate.path,
            checkpoint,
            &cutoff,
            timezone,
            deadline,
            options
                .max_bytes
                .saturating_sub(progress.bytes_read)
                .min(MAX_FILE_BYTES_PER_PASS),
        ) {
            Ok(file_progress) => {
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
        seen_files.contains(key) || !checkpoint.days.is_empty()
    });
    progress.lossy |= index.files.values().any(|checkpoint| checkpoint.lossy);

    if rebuilt || index != original {
        save_index(index_path, &index)?;
    }

    Ok(snapshot_from_index(
        &index,
        now,
        offset_minutes,
        progress.incomplete,
        progress.lossy,
        &cutoff,
    ))
}

fn fair_candidate_order(candidates: Vec<CandidateFile>) -> Vec<CandidateFile> {
    let mut claude = candidates
        .iter()
        .filter(|candidate| candidate.tool == ActivityTool::Claude)
        .cloned()
        .collect::<Vec<_>>();
    let mut codex = candidates
        .into_iter()
        .filter(|candidate| candidate.tool == ActivityTool::Codex)
        .collect::<Vec<_>>();
    let newest_first = |left: &CandidateFile, right: &CandidateFile| {
        right
            .modified_millis
            .cmp(&left.modified_millis)
            .then_with(|| left.key.cmp(&right.key))
    };
    claude.sort_by(newest_first);
    codex.sort_by(newest_first);
    let mut claude = VecDeque::from(claude);
    let mut codex = VecDeque::from(codex);
    let mut ordered = Vec::with_capacity(claude.len() + codex.len());
    while !claude.is_empty() || !codex.is_empty() {
        if let Some(candidate) = claude.pop_front() {
            ordered.push(candidate);
        }
        if let Some(candidate) = codex.pop_front() {
            ordered.push(candidate);
        }
    }
    ordered
}

fn load_index(path: &Path, offset_minutes: i32) -> (ActivityIndex, bool) {
    let loaded = fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<ActivityIndex>(&contents).ok())
        .filter(|index| {
            index.schema_version == SCHEMA_VERSION
                && index.timezone_offset_minutes == offset_minutes
        });
    match loaded {
        Some(index) => (index, false),
        None => (ActivityIndex::new(offset_minutes), path.exists()),
    }
}

fn save_index(path: &Path, index: &ActivityIndex) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_vec_pretty(index)?;
    config::replace_file(path, &contents).context("replace activity index")
}

fn collect_candidates(
    roots: &ActivityRoots,
    show_claude: bool,
    show_codex: bool,
    cutoff: &str,
    timezone: FixedOffset,
    deadline: Instant,
    options: ScanOptions,
) -> (Vec<CandidateFile>, bool) {
    let mut candidates = Vec::new();
    let mut partial = false;
    let mut entries_seen = 0usize;

    for (enabled, root, tool) in [
        (show_claude, roots.claude.as_deref(), ActivityTool::Claude),
        (show_codex, roots.codex.as_deref(), ActivityTool::Codex),
    ] {
        let Some(root) = root.filter(|_| enabled) else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        let mut pending = VecDeque::from([root.to_path_buf()]);
        while let Some(directory) = pending.pop_front() {
            if Instant::now() >= deadline
                || entries_seen >= options.max_entries
                || candidates.len() >= options.max_files
            {
                partial = true;
                break;
            }
            let Ok(entries) = fs::read_dir(&directory) else {
                partial = true;
                continue;
            };
            for entry in entries.flatten() {
                entries_seen += 1;
                if entries_seen > options.max_entries || Instant::now() >= deadline {
                    partial = true;
                    break;
                }
                let Ok(file_type) = entry.file_type() else {
                    partial = true;
                    continue;
                };
                if file_type.is_dir() {
                    pending.push_back(entry.path());
                    continue;
                }
                if !file_type.is_file()
                    || entry.path().extension().and_then(|value| value.to_str()) != Some("jsonl")
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
                        .format("%Y-%m-%d")
                        .to_string();
                if modified_date.as_str() < cutoff {
                    continue;
                }
                let path = entry.path();
                candidates.push(CandidateFile {
                    key: path.to_string_lossy().into_owned(),
                    path,
                    tool,
                    modified_millis,
                });
                if candidates.len() >= options.max_files {
                    partial = true;
                    break;
                }
            }
        }
    }
    (candidates, partial)
}

fn scan_file(
    path: &Path,
    checkpoint: &mut FileCheckpoint,
    cutoff: &str,
    timezone: FixedOffset,
    deadline: Instant,
    byte_budget: u64,
) -> anyhow::Result<ScanProgress> {
    let metadata = fs::metadata(path)?;
    let current_len = metadata.len();
    let current_modified = modified_millis(&metadata);

    if checkpoint.offset > current_len
        || (checkpoint.offset > 0
            && checkpoint.tail_fingerprint != fingerprint_before(path, checkpoint.offset)?)
    {
        *checkpoint = FileCheckpoint::new(checkpoint.tool);
    }

    if checkpoint.offset == current_len {
        checkpoint.observed_len = current_len;
        checkpoint.modified_millis = current_modified;
        checkpoint.tail_fingerprint = fingerprint_before(path, checkpoint.offset)?;
        checkpoint.prune(cutoff);
        return Ok(ScanProgress::default());
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(checkpoint.offset))?;
    let mut reader = BufReader::new(file);
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
    }

    checkpoint.observed_len = current_len;
    checkpoint.modified_millis = current_modified;
    checkpoint.tail_fingerprint = fingerprint_before(path, checkpoint.offset)?;
    checkpoint.prune(cutoff);
    Ok(ScanProgress {
        incomplete,
        lossy,
        bytes_read,
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

fn apply_claude_line(
    checkpoint: &mut FileCheckpoint,
    root: &Value,
    cutoff: &str,
    timezone: FixedOffset,
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

fn apply_codex_line(
    checkpoint: &mut FileCheckpoint,
    root: &Value,
    cutoff: &str,
    timezone: FixedOffset,
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

fn local_date(timestamp: Option<&Value>, timezone: FixedOffset) -> Option<String> {
    let timestamp = timestamp?.as_str()?;
    DateTime::parse_from_rfc3339(timestamp).ok().map(|value| {
        value
            .with_timezone(&timezone)
            .format("%Y-%m-%d")
            .to_string()
    })
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
    for (file_key, checkpoint) in &index.files {
        if checkpoint.tool == ActivityTool::Claude {
            for (message_id, contribution) in &checkpoint.claude_messages {
                if contribution.date.as_str() < cutoff {
                    continue;
                }
                let dedupe_key = if message_id.starts_with("offset:") {
                    format!("{file_key}\0{message_id}")
                } else {
                    message_id.clone()
                };
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
        for (date, tokens) in &checkpoint.days {
            if date.as_str() < cutoff {
                continue;
            }
            let day = days.entry(date.clone()).or_insert_with(|| ActivityDay {
                date: date.clone(),
                ..ActivityDay::default()
            });
            day.codex_tokens = day.codex_tokens.saturating_add(*tokens);
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
    ActivitySnapshot {
        schema_version: SCHEMA_VERSION.into(),
        generated_at: now.to_rfc3339(),
        timezone_offset_minutes: offset_minutes,
        partial: incomplete || lossy,
        backfill_pending: incomplete,
        days: days.into_values().collect(),
    }
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
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 19, 12, 0, 0).unwrap()
    }

    fn kst() -> FixedOffset {
        FixedOffset::east_opt(9 * 3600).unwrap()
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
        let first =
            refresh_at(&index, &roots(&root), true, false, now(), kst(), options()).unwrap();
        let repeated =
            refresh_at(&index, &roots(&root), true, false, now(), kst(), options()).unwrap();

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

        let first =
            refresh_at(&index, &roots(&root), false, true, now(), kst(), options()).unwrap();
        assert_eq!(first.days[0].codex_tokens, 100);
        assert!(first.partial);
        assert!(first.backfill_pending);

        let unchanged =
            refresh_at(&index, &roots(&root), false, true, now(), kst(), options()).unwrap();
        assert_eq!(unchanged.days[0].codex_tokens, 100);

        let mut output = OpenOptions::new().append(true).open(&file).unwrap();
        output.write_all(b"{\"total_tokens\":175}}}}\n").unwrap();
        drop(output);
        let appended =
            refresh_at(&index, &roots(&root), false, true, now(), kst(), options()).unwrap();
        assert_eq!(appended.days[0].codex_tokens, 175);
        assert!(!appended.backfill_pending);

        let repeated =
            refresh_at(&index, &roots(&root), false, true, now(), kst(), options()).unwrap();
        assert_eq!(repeated.days[0].codex_tokens, 175);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_order_alternates_tools_and_keeps_each_tool_newest_first() {
        let ordered = fair_candidate_order(vec![
            candidate(ActivityTool::Codex, "codex-old", 10),
            candidate(ActivityTool::Claude, "claude-old", 20),
            candidate(ActivityTool::Codex, "codex-new", 40),
            candidate(ActivityTool::Claude, "claude-new", 30),
        ]);
        let keys = ordered
            .iter()
            .map(|candidate| candidate.key.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec!["claude-new", "codex-new", "claude-old", "codex-old"]
        );
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

        let snapshot =
            refresh_at(&index, &roots(&root), true, false, now(), kst(), options()).unwrap();

        assert_eq!(snapshot.days[0].claude_tokens, 20);
        assert_eq!(fs::read_to_string(&source).unwrap(), contents);
        let saved: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], SCHEMA_VERSION);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_schema_index_is_rebuilt_from_source_logs() {
        let root = temp_root("stale-schema");
        let claude = root.join("claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("session.jsonl"),
            concat!(
                r#"{"timestamp":"2026-07-19T01:00:00Z","message":{"id":"m1","usage":{"input_tokens":12,"output_tokens":8}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let index = root.join("index.json");
        let mut stale = ActivityIndex::new(9 * 60);
        stale.schema_version = "usage_activity.v1".into();
        let mut checkpoint = FileCheckpoint::new(ActivityTool::Claude);
        checkpoint.days.insert("2026-07-19".into(), 999_999);
        stale.files.insert("stale-file".into(), checkpoint);
        fs::write(&index, serde_json::to_vec(&stale).unwrap()).unwrap();

        let snapshot =
            refresh_at(&index, &roots(&root), true, false, now(), kst(), options()).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();

        assert_eq!(snapshot.days[0].claude_tokens, 20);
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
        let enabled =
            refresh_at(&index, &roots(&root), true, false, now(), kst(), options()).unwrap();
        assert_eq!(enabled.days[0].claude_tokens, 10);

        fs::write(claude.join("session.jsonl"), "not-json\n").unwrap();
        let disabled =
            refresh_at(&index, &roots(&root), false, false, now(), kst(), options()).unwrap();
        assert_eq!(disabled.days[0].claude_tokens, 10);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "reads locally installed Claude and Codex session logs"]
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

        assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
        assert!(index.is_file());
        assert!(snapshot
            .days
            .windows(2)
            .all(|days| days[0].date < days[1].date));
        eprintln!(
            "activity live scan: elapsed={:?}, passes={}, days={}, claude_tokens={}, codex_tokens={}, partial={}, backfill_pending={}",
            started.elapsed(),
            passes,
            snapshot.days.len(),
            claude,
            codex,
            snapshot.partial,
            snapshot.backfill_pending
        );
        fs::remove_dir_all(root).unwrap();
    }
}
