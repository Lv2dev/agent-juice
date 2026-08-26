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

static SCAN_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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
            tail_fingerprint: 0,
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
        codex: home
            .as_ref()
            .map(|path| path.join(".codex").join("sessions")),
        grok: home.map(|path| path.join(".grok").join("sessions")),
    }
}

pub fn refresh(show_claude: bool, show_grok: bool) -> anyhow::Result<ActivitySnapshot> {
    let _guard = SCAN_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let path = index_path().ok_or_else(|| anyhow::anyhow!("activity data path unavailable"))?;
    let timezone = Local::now().offset().fix();
    let mut snapshot = refresh_at(
        &path,
        &local_roots(),
        show_claude,
        false,
        show_grok,
        Utc::now(),
        timezone,
        ScanOptions::default(),
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
        [show_claude, show_codex, show_grok],
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
    if !show_codex {
        index
            .files
            .retain(|_, checkpoint| checkpoint.tool != ActivityTool::Codex);
    }
    progress.lossy |= index.files.values().any(|checkpoint| {
        checkpoint.lossy
            && match checkpoint.tool {
                ActivityTool::Claude => show_claude,
                ActivityTool::Codex => show_codex,
                ActivityTool::Grok => show_grok,
            }
    });

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
        for queue in &mut queues {
            if let Some(candidate) = queue.pop_front() {
                ordered.push(candidate);
            }
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
    enabled_tools: [bool; 3],
    cutoff: &str,
    timezone: FixedOffset,
    deadline: Instant,
    options: ScanOptions,
) -> (Vec<CandidateFile>, bool) {
    let mut candidates = Vec::new();
    let mut partial = false;
    let mut entries_seen = 0usize;

    let roots = [
        roots.claude.as_deref(),
        roots.codex.as_deref(),
        roots.grok.as_deref(),
    ];
    for ((enabled, root), tool) in enabled_tools.into_iter().zip(roots).zip(ActivityTool::ALL) {
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
                let path = entry.path();
                if !file_type.is_file()
                    || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
                    || (tool == ActivityTool::Grok
                        && path.file_name().and_then(|value| value.to_str())
                            != Some("updates.jsonl"))
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

fn apply_grok_line(
    checkpoint: &mut FileCheckpoint,
    root: &Value,
    cutoff: &str,
    timezone: FixedOffset,
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

fn local_date(timestamp: Option<&Value>, timezone: FixedOffset) -> Option<String> {
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
        if checkpoint.tool == ActivityTool::Grok {
            for (response_id, contribution) in &checkpoint.grok_responses {
                if contribution.date.as_str() < cutoff {
                    continue;
                }
                let dedupe_key = if response_id.starts_with("offset:") {
                    format!("{file_key}\0{response_id}")
                } else {
                    response_id.clone()
                };
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
                ActivityTool::Claude | ActivityTool::Grok => {
                    unreachable!("message contributions are handled above")
                }
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
            candidate(ActivityTool::Codex, "codex-old", 10),
            candidate(ActivityTool::Claude, "claude-old", 20),
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
