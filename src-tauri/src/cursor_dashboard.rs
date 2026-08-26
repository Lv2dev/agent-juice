use rusqlite::{types::ValueRef, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
    process::Command,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

const DASHBOARD_BASE_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService";
const ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";
const APPLICATION_USER_KEY: &str =
    "src.vs.platform.reactivestorage.browser.reactiveStorageServiceImpl.persistentStorage.applicationUser";
const MAX_DB_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 8 * 1024;
const MAX_APPLICATION_BYTES: usize = 2 * 1024 * 1024;
const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(200);
const TRANSPORT_CLEANUP_RESERVE: Duration = Duration::from_millis(750);
pub const STATUS_RESPONSE_CAP: usize = 256 * 1024;
pub const ACTIVITY_RESPONSE_CAP: usize = 1024 * 1024;
static VALIDATED_SCOPES: LazyLock<Mutex<BTreeSet<AccountScope>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountScope {
    pub user_id: u64,
    pub team_id: Option<u64>,
}

pub fn scope_is_validated(scope: AccountScope) -> bool {
    VALIDATED_SCOPES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&scope)
}

fn mark_scope_validated(scope: AccountScope) {
    VALIDATED_SCOPES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(scope);
}

fn invalidate_scope(scope: AccountScope) {
    VALIDATED_SCOPES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&scope);
}

pub struct DashboardCredentials {
    access_token: String,
    pub scope: AccountScope,
}

impl DashboardCredentials {
    fn access_token(&self) -> &str {
        &self.access_token
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardErrorKind {
    Deadline,
    Transport,
    Parse,
    LoginRequired,
    Unavailable,
    Oversized,
    ScopeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardError {
    pub kind: DashboardErrorKind,
    context: &'static str,
}

impl DashboardError {
    fn new(kind: DashboardErrorKind, context: &'static str) -> Self {
        Self { kind, context }
    }

    fn deadline(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::Deadline, context)
    }

    fn transport(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::Transport, context)
    }

    fn parse(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::Parse, context)
    }

    fn login(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::LoginRequired, context)
    }

    fn unavailable(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::Unavailable, context)
    }
}

impl fmt::Display for DashboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl std::error::Error for DashboardError {}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentPeriodUsage {
    pub billing_cycle_end_ms: i64,
    pub cursor_models_used_percent: f32,
    pub other_models_used_percent: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    pub timestamp_ms: i64,
    pub token_usage: Option<TokenUsage>,
    pub model: Option<String>,
    pub client_type: Option<String>,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
}

impl TokenUsage {
    pub fn total(self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cache_write_tokens: self
                .cache_write_tokens
                .saturating_add(other.cache_write_tokens),
            cache_read_tokens: self
                .cache_read_tokens
                .saturating_add(other.cache_read_tokens),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEventPage {
    pub events: Vec<UsageEvent>,
    pub total_count: usize,
}

fn remaining(deadline: Instant, context: &'static str) -> Result<Duration, DashboardError> {
    let value = deadline.saturating_duration_since(Instant::now());
    if value.is_zero() {
        Err(DashboardError::deadline(context))
    } else {
        Ok(value)
    }
}

pub fn state_db_path() -> Result<PathBuf, DashboardError> {
    dirs::config_dir()
        .map(|root| {
            root.join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
        })
        .ok_or_else(|| DashboardError::unavailable("Cursor configuration directory unavailable"))
}

fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn validate_path_components(path: &Path) -> Result<(), DashboardError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if !current.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|_| DashboardError::unavailable("Cursor state path unavailable"))?;
        if metadata_is_reparse(&metadata) {
            return Err(DashboardError::unavailable(
                "Cursor state path contains a reparse point",
            ));
        }
    }
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state.vscdb");
    path.with_file_name(format!("{name}-{suffix}"))
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    file: u64,
}

#[cfg(windows)]
struct LockedStateFile {
    path: PathBuf,
    handle: windows::Win32::Foundation::HANDLE,
    identity: FileIdentity,
}

#[cfg(windows)]
impl Drop for LockedStateFile {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
fn lock_state_file(path: &Path) -> Result<LockedStateFile, DashboardError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|_| DashboardError::unavailable("Cursor state identity lock failed"))?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut information) }.is_err()
        || information.dwFileAttributes
            & (FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0)
            != 0
    {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
        return Err(DashboardError::unavailable(
            "Cursor state identity rejected",
        ));
    }
    Ok(LockedStateFile {
        path: path.to_path_buf(),
        handle,
        identity: FileIdentity {
            volume: information.dwVolumeSerialNumber as u64,
            file: ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
        },
    })
}

#[cfg(windows)]
struct StateFilesGuard {
    files: Vec<LockedStateFile>,
}

#[cfg(windows)]
impl StateFilesGuard {
    fn lock(path: &Path) -> Result<Self, DashboardError> {
        let mut paths = vec![path.to_path_buf()];
        paths.extend(
            [sidecar_path(path, "wal"), sidecar_path(path, "shm")]
                .into_iter()
                .filter(|candidate| candidate.exists()),
        );
        let files = paths
            .iter()
            .map(|candidate| lock_state_file(candidate))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { files })
    }

    fn revalidate(&self, path: &Path) -> Result<(), DashboardError> {
        let current = [
            path.to_path_buf(),
            sidecar_path(path, "wal"),
            sidecar_path(path, "shm"),
        ]
        .into_iter()
        .filter(|candidate| candidate.exists())
        .collect::<Vec<_>>();
        if current.len() != self.files.len() {
            return Err(DashboardError::new(
                DashboardErrorKind::ScopeChanged,
                "Cursor state sidecar set changed",
            ));
        }
        for file in &self.files {
            let current = lock_state_file(&file.path)?;
            if current.identity != file.identity {
                return Err(DashboardError::new(
                    DashboardErrorKind::ScopeChanged,
                    "Cursor state file identity changed",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
struct StateFilesGuard;

#[cfg(not(windows))]
impl StateFilesGuard {
    fn lock(_path: &Path) -> Result<Self, DashboardError> {
        Ok(Self)
    }

    fn revalidate(&self, path: &Path) -> Result<(), DashboardError> {
        validate_state_files(path)
    }
}

fn validate_state_files(path: &Path) -> Result<(), DashboardError> {
    validate_path_components(path)?;
    let metadata = std::fs::metadata(path)
        .map_err(|_| DashboardError::unavailable("Cursor state database unavailable"))?;
    if !metadata.is_file() || metadata.len() > MAX_DB_BYTES {
        return Err(DashboardError::unavailable(
            "Cursor state database rejected",
        ));
    }
    for sidecar in [sidecar_path(path, "wal"), sidecar_path(path, "shm")] {
        if sidecar.exists() {
            validate_path_components(&sidecar)?;
            let metadata = std::fs::metadata(&sidecar)
                .map_err(|_| DashboardError::unavailable("Cursor state sidecar unavailable"))?;
            if !metadata.is_file() || metadata.len() > MAX_DB_BYTES {
                return Err(DashboardError::unavailable("Cursor state sidecar rejected"));
            }
        }
    }
    Ok(())
}

fn query_value(
    transaction: &Transaction<'_>,
    key: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, DashboardError> {
    transaction
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1 AND length(value) <= ?2 LIMIT 1",
            (key, max_bytes as i64),
            |row| match row.get_ref(0)? {
                ValueRef::Text(value) | ValueRef::Blob(value) => Ok(value.to_vec()),
                _ => Err(rusqlite::Error::InvalidColumnType(
                    0,
                    "value".into(),
                    row.get_ref(0)?.data_type(),
                )),
            },
        )
        .optional()
        .map_err(|_| DashboardError::unavailable("Cursor state query failed"))?
        .ok_or_else(|| DashboardError::login("Cursor login required"))
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64().filter(|value| *value > 0),
        Value::String(value) => value.parse::<u64>().ok().filter(|value| *value > 0),
        _ => None,
    }
}

pub fn read_credentials(deadline: Instant) -> Result<DashboardCredentials, DashboardError> {
    remaining(deadline, "Cursor credentials deadline exceeded")?;
    let path = state_db_path()?;
    read_credentials_from(&path, deadline)
}

fn read_credentials_from(
    path: &Path,
    deadline: Instant,
) -> Result<DashboardCredentials, DashboardError> {
    validate_state_files(path)?;
    let state_files = StateFilesGuard::lock(path)?;
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
    )
    .map_err(|_| DashboardError::unavailable("Cursor state database open failed"))?;
    connection
        .busy_timeout(
            DB_BUSY_TIMEOUT.min(remaining(deadline, "Cursor credentials deadline exceeded")?),
        )
        .map_err(|_| DashboardError::unavailable("Cursor state busy timeout failed"))?;
    let transaction = connection
        .transaction()
        .map_err(|_| DashboardError::unavailable("Cursor state snapshot failed"))?;
    let token = query_value(&transaction, ACCESS_TOKEN_KEY, MAX_TOKEN_BYTES)?;
    let application = query_value(&transaction, APPLICATION_USER_KEY, MAX_APPLICATION_BYTES)?;
    transaction
        .commit()
        .map_err(|_| DashboardError::unavailable("Cursor state snapshot commit failed"))?;
    drop(connection);
    validate_state_files(path)?;
    state_files.revalidate(path)?;
    remaining(deadline, "Cursor credentials deadline exceeded")?;

    let access_token = String::from_utf8(token)
        .ok()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_TOKEN_BYTES
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~')
                })
        })
        .ok_or_else(|| DashboardError::login("Cursor login required"))?;
    let application: Value = serde_json::from_slice(&application)
        .map_err(|_| DashboardError::parse("Cursor account identity was not recognized"))?;
    let user_id = positive_u64(application.get("dashboardUserId"))
        .ok_or_else(|| DashboardError::login("Cursor account identity unavailable"))?;
    let team_id = positive_u64(application.pointer("/aiSettings/teamIds/0"));

    Ok(DashboardCredentials {
        access_token,
        scope: AccountScope { user_id, team_id },
    })
}

#[derive(Deserialize)]
struct ConnectErrorBody {
    code: Option<String>,
}

fn curl_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let path = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("curl.exe");
        let mut command = Command::new(path);
        command.args(["--config", "-"]);
        command.creation_flags(0x08000000);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("curl");
        command.args(["--config", "-"]);
        command
    }
}

fn config_value(value: &str) -> Result<String, DashboardError> {
    if value.contains(['\r', '\n', '\0']) {
        return Err(DashboardError::parse(
            "Cursor Dashboard request value rejected",
        ));
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn curl_config(
    credentials: &DashboardCredentials,
    method: &'static str,
    body: &str,
    timeout: Duration,
) -> Result<String, DashboardError> {
    let url = format!("{DASHBOARD_BASE_URL}/{method}");
    let token = config_value(credentials.access_token())?;
    let body = config_value(body)?;
    Ok(format!(
        "silent\nshow-error\nrequest = \"POST\"\nmax-time = \"{:.3}\"\nconnect-timeout = \"{:.3}\"\nurl = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Connect-Protocol-Version: 1\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{}\"\nwrite-out = \"\\n__AJ_CURSOR_HTTP__:%{{http_code}}:%{{content_type}}\"\n",
        timeout.as_secs_f64(),
        timeout.min(Duration::from_secs(3)).as_secs_f64(),
        url,
        token,
        body,
    ))
}

fn parse_transport_output(
    output: String,
    cap: usize,
) -> Result<(u16, String, Vec<u8>), DashboardError> {
    const MARKER: &str = "\n__AJ_CURSOR_HTTP__:";
    let (body, metadata) = output
        .rsplit_once(MARKER)
        .ok_or_else(|| DashboardError::parse("Cursor Dashboard metadata unavailable"))?;
    if body.len() > cap {
        return Err(DashboardError::new(
            DashboardErrorKind::Oversized,
            "Cursor Dashboard response exceeded its limit",
        ));
    }
    let (status, content_type) = metadata
        .split_once(':')
        .ok_or_else(|| DashboardError::parse("Cursor Dashboard metadata rejected"))?;
    let status = status
        .parse::<u16>()
        .ok()
        .filter(|value| (100..=599).contains(value))
        .ok_or_else(|| DashboardError::parse("Cursor Dashboard status rejected"))?;
    Ok((
        status,
        content_type.to_ascii_lowercase(),
        body.as_bytes().to_vec(),
    ))
}

fn post_json<Request: Serialize, Output: DeserializeOwned>(
    credentials: &DashboardCredentials,
    method: &'static str,
    request: &Request,
    deadline: Instant,
    cap: usize,
) -> Result<Output, DashboardError> {
    let remaining = remaining(deadline, "Cursor Dashboard deadline exceeded")?;
    let timeout = remaining
        .checked_sub(TRANSPORT_CLEANUP_RESERVE)
        .filter(|value| !value.is_zero())
        .ok_or_else(|| DashboardError::deadline("Cursor Dashboard cleanup reserve unavailable"))?;
    let body = serde_json::to_string(request)
        .map_err(|_| DashboardError::parse("Cursor Dashboard request was not recognized"))?;
    let config = curl_config(credentials, method, &body, timeout)?;
    let output = crate::collector::command_output_with_input_caps(
        curl_command(),
        Some(config.as_bytes()),
        timeout,
        "Cursor Dashboard",
        cap.saturating_add(256),
        64 * 1024,
    )
    .map_err(|error| {
        if error.to_string().contains("exceeded") {
            return DashboardError::new(
                DashboardErrorKind::Oversized,
                "Cursor Dashboard response exceeded its limit",
            );
        }
        if Instant::now() >= deadline {
            DashboardError::deadline("Cursor Dashboard request timed out")
        } else {
            DashboardError::transport("Cursor Dashboard request failed")
        }
    })?;
    let (status, content_type, body) = parse_transport_output(output, cap)?;
    if !(200..=299).contains(&status) {
        let code = serde_json::from_slice::<ConnectErrorBody>(&body)
            .ok()
            .and_then(|value| value.code);
        if status == 401 || code.as_deref() == Some("unauthenticated") {
            invalidate_scope(credentials.scope);
            return Err(DashboardError::login("Cursor login required"));
        }
        if matches!(status, 400 | 404 | 415) {
            return Err(DashboardError::parse(
                "Cursor Dashboard contract was not recognized",
            ));
        }
        return Err(DashboardError::transport(
            "Cursor Dashboard rejected the request",
        ));
    }
    if !content_type.starts_with("application/json") {
        return Err(DashboardError::parse(
            "Cursor Dashboard response type was not recognized",
        ));
    }
    let parsed = serde_json::from_slice(&body)
        .map_err(|_| DashboardError::parse("Cursor Dashboard response was not recognized"))?;
    mark_scope_validated(credentials.scope);
    Ok(parsed)
}

#[derive(Serialize, Default)]
struct EmptyRequest {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurrentPeriodResponse {
    billing_cycle_end: Int64Json,
    plan_usage: Option<PlanUsageResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanUsageResponse {
    auto_percent_used: Option<f64>,
    api_percent_used: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Int64Json {
    String(String),
    Number(i64),
}

impl Int64Json {
    fn value(&self) -> Option<i64> {
        match self {
            Self::String(value) => value.parse().ok(),
            Self::Number(value) => Some(*value),
        }
    }
}

fn percent(value: Option<f64>) -> Option<f32> {
    value
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .map(|value| value as f32)
}

pub fn current_period_usage(
    credentials: &DashboardCredentials,
    deadline: Instant,
) -> Result<CurrentPeriodUsage, DashboardError> {
    let response: CurrentPeriodResponse = post_json(
        credentials,
        "GetCurrentPeriodUsage",
        &EmptyRequest::default(),
        deadline,
        STATUS_RESPONSE_CAP,
    )?;
    let plan = response
        .plan_usage
        .ok_or_else(|| DashboardError::parse("Cursor usage pools unavailable"))?;
    Ok(CurrentPeriodUsage {
        billing_cycle_end_ms: response
            .billing_cycle_end
            .value()
            .filter(|value| *value > 0)
            .ok_or_else(|| DashboardError::parse("Cursor billing cycle unavailable"))?,
        cursor_models_used_percent: percent(plan.auto_percent_used)
            .ok_or_else(|| DashboardError::parse("Cursor Models usage unavailable"))?,
        other_models_used_percent: percent(plan.api_percent_used)
            .ok_or_else(|| DashboardError::parse("Cursor Other Models usage unavailable"))?,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FilteredUsageRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    team_id: Option<u64>,
    start_date: String,
    end_date: String,
    user_id: u64,
    page: u32,
    page_size: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilteredUsageResponse {
    #[serde(default)]
    usage_events_display: Vec<UsageEventResponse>,
    #[serde(default)]
    total_usage_events_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageEventResponse {
    timestamp: Int64Json,
    token_usage: Option<TokenUsageResponse>,
    model: Option<String>,
    client_type: Option<String>,
    conversation_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenUsageResponse {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
}

pub fn filtered_usage_events(
    credentials: &DashboardCredentials,
    start_ms: i64,
    end_ms: i64,
    page: u32,
    page_size: u32,
    deadline: Instant,
) -> Result<UsageEventPage, DashboardError> {
    if start_ms < 0 || end_ms <= start_ms || page == 0 || page_size == 0 || page_size > 500 {
        return Err(DashboardError::parse(
            "Cursor activity request bounds rejected",
        ));
    }
    let response: FilteredUsageResponse = post_json(
        credentials,
        "GetFilteredUsageEvents",
        &FilteredUsageRequest {
            team_id: credentials.scope.team_id,
            start_date: start_ms.to_string(),
            end_date: end_ms.to_string(),
            user_id: credentials.scope.user_id,
            page,
            page_size,
        },
        deadline,
        ACTIVITY_RESPONSE_CAP,
    )?;
    let mut events = Vec::with_capacity(response.usage_events_display.len());
    for event in response.usage_events_display {
        let Some(timestamp_ms) = event.timestamp.value().filter(|value| *value >= 0) else {
            return Err(DashboardError::parse(
                "Cursor activity timestamp was not recognized",
            ));
        };
        events.push(UsageEvent {
            timestamp_ms,
            token_usage: event.token_usage.map(|usage| TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                cache_read_tokens: usage.cache_read_tokens,
            }),
            model: event.model,
            client_type: event.client_type,
            conversation_id: event.conversation_id,
        });
    }
    Ok(UsageEventPage {
        events,
        total_count: response.total_usage_events_count,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregatedUsageRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    team_id: Option<u64>,
    start_date: String,
    end_date: String,
    user_id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AggregatedUsageResponse {
    #[serde(default)]
    total_input_tokens: Int64JsonDefault,
    #[serde(default)]
    total_output_tokens: Int64JsonDefault,
    #[serde(default)]
    total_cache_write_tokens: Int64JsonDefault,
    #[serde(default)]
    total_cache_read_tokens: Int64JsonDefault,
}

#[derive(Deserialize, Default)]
#[serde(transparent)]
struct Int64JsonDefault(Option<Int64Json>);

impl Int64JsonDefault {
    fn unsigned(&self) -> Result<u64, DashboardError> {
        let Some(value) = self.0.as_ref() else {
            return Ok(0);
        };
        value
            .value()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| DashboardError::parse("Cursor aggregate token count was not recognized"))
    }
}

pub fn aggregated_usage(
    credentials: &DashboardCredentials,
    start_ms: i64,
    end_ms: i64,
    deadline: Instant,
) -> Result<TokenUsage, DashboardError> {
    if start_ms < 0 || end_ms <= start_ms {
        return Err(DashboardError::parse(
            "Cursor aggregate request bounds rejected",
        ));
    }
    let response: AggregatedUsageResponse = post_json(
        credentials,
        "GetAggregatedUsageEvents",
        &AggregatedUsageRequest {
            team_id: credentials.scope.team_id,
            start_date: start_ms.to_string(),
            end_date: end_ms.to_string(),
            user_id: credentials.scope.user_id,
        },
        deadline,
        STATUS_RESPONSE_CAP,
    )?;
    Ok(TokenUsage {
        input_tokens: response.total_input_tokens.unsigned()?,
        output_tokens: response.total_output_tokens.unsigned()?,
        cache_write_tokens: response.total_cache_write_tokens.unsigned()?,
        cache_read_tokens: response.total_cache_read_tokens.unsigned()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "agent-juice-cursor-dashboard-{name}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path.join("state.vscdb")
    }

    fn create_credentials_fixture(path: &Path, wal: bool) -> Connection {
        let connection = Connection::open(path).unwrap();
        if wal {
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .unwrap();
        }
        connection
            .execute(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
                (ACCESS_TOKEN_KEY, "header.payload.signature"),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
                (
                    APPLICATION_USER_KEY,
                    r#"{"dashboardUserId":42,"aiSettings":{"teamIds":[7]}}"#,
                ),
            )
            .unwrap();
        connection
    }

    #[test]
    fn token_usage_sums_all_four_components() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            cache_write_tokens: 30,
            cache_read_tokens: 40,
        };
        assert_eq!(usage.total(), 100);
    }

    #[test]
    fn current_period_json_accepts_proto_int64_strings() {
        let response: CurrentPeriodResponse = serde_json::from_str(
            r#"{"billingCycleEnd":"1780000000000","planUsage":{"autoPercentUsed":12.5,"apiPercentUsed":3}}"#,
        )
        .unwrap();
        assert_eq!(response.billing_cycle_end.value(), Some(1_780_000_000_000));
        assert_eq!(
            percent(response.plan_usage.unwrap().auto_percent_used),
            Some(12.5)
        );
    }

    #[test]
    fn filtered_request_uses_camel_case_and_omits_missing_team() {
        let value = serde_json::to_value(FilteredUsageRequest {
            team_id: None,
            start_date: "1".into(),
            end_date: "2".into(),
            user_id: 7,
            page: 1,
            page_size: 500,
        })
        .unwrap();
        assert_eq!(value["userId"], 7);
        assert_eq!(value["pageSize"], 500);
        assert!(value.get("teamId").is_none());
    }

    #[test]
    fn dashboard_errors_never_include_credentials_or_response_bodies() {
        for error in [
            DashboardError::login("Cursor login required"),
            DashboardError::transport("Cursor Dashboard request failed"),
            DashboardError::parse("Cursor Dashboard response was not recognized"),
        ] {
            let text = error.to_string();
            assert!(!text.contains("Bearer"));
            assert!(!text.contains("accessToken"));
            assert!(!text.contains('{'));
        }
    }

    #[test]
    fn dashboard_transport_keeps_token_out_of_process_arguments_and_parses_status() {
        let credentials = DashboardCredentials {
            access_token: "secret.token".into(),
            scope: AccountScope {
                user_id: 1,
                team_id: None,
            },
        };
        let config = curl_config(
            &credentials,
            "GetCurrentPeriodUsage",
            "{}",
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(config.contains("Authorization: Bearer secret.token"));
        assert!(!format!("{:?}", curl_command()).contains("secret.token"));

        let (status, content_type, body) = parse_transport_output(
            "{\"ok\":true}\n__AJ_CURSOR_HTTP__:200:application/json".into(),
            1024,
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(body, br#"{"ok":true}"#);
    }

    #[test]
    fn dashboard_transport_rejects_body_past_the_call_cap() {
        let output = format!(
            "{}\n__AJ_CURSOR_HTTP__:200:application/json",
            "x".repeat(17)
        );
        assert_eq!(
            parse_transport_output(output, 16).unwrap_err().kind,
            DashboardErrorKind::Oversized
        );
    }

    #[test]
    fn credentials_use_one_read_only_snapshot_with_optional_team_scope() {
        let path = fixture_path("credentials");
        let connection = create_credentials_fixture(&path, false);
        drop(connection);
        let credentials =
            read_credentials_from(&path, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(
            credentials.scope,
            AccountScope {
                user_id: 42,
                team_id: Some(7)
            }
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn credentials_read_the_latest_wal_value_without_copying_the_database() {
        let path = fixture_path("wal");
        let connection = create_credentials_fixture(&path, true);
        connection
            .execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                (
                    r#"{"dashboardUserId":84,"aiSettings":{"teamIds":[]}}"#,
                    APPLICATION_USER_KEY,
                ),
            )
            .unwrap();
        let credentials =
            read_credentials_from(&path, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(credentials.scope.user_id, 84);
        assert_eq!(credentials.scope.team_id, None);
        drop(connection);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn credentials_reject_oversized_values_before_loading_them() {
        let path = fixture_path("oversized-token");
        let connection = create_credentials_fixture(&path, false);
        connection
            .execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                ("x".repeat(MAX_TOKEN_BYTES + 1), ACCESS_TOKEN_KEY),
            )
            .unwrap();
        drop(connection);
        let error = match read_credentials_from(&path, Instant::now() + Duration::from_secs(2)) {
            Ok(_) => panic!("oversized token was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind, DashboardErrorKind::LoginRequired);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "uses the locally logged-in Cursor GUI account"]
    fn live_gui_credentials_and_current_period_round_trip() {
        let deadline = Instant::now() + Duration::from_secs(8);
        let credentials = read_credentials(deadline).unwrap();
        let usage = current_period_usage(&credentials, deadline).unwrap();
        assert!((0.0..=100.0).contains(&usage.cursor_models_used_percent));
        assert!((0.0..=100.0).contains(&usage.other_models_used_percent));
        assert!(usage.billing_cycle_end_ms > 0);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "uses the locally logged-in Cursor GUI account"]
    fn live_filtered_events_match_the_account_aggregate() {
        use chrono::{Datelike, TimeZone, Utc};

        let deadline = Instant::now() + Duration::from_secs(8);
        let credentials = read_credentials(deadline).unwrap();
        let now = Utc::now();
        let start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let end = (now - chrono::Duration::minutes(5)).timestamp_millis();
        let page = filtered_usage_events(&credentials, start, end, 1, 500, deadline).unwrap();
        assert_eq!(page.events.len(), page.total_count);
        let event_total = page
            .events
            .iter()
            .filter_map(|event| event.token_usage)
            .fold(TokenUsage::default(), TokenUsage::saturating_add);
        assert_eq!(
            aggregated_usage(&credentials, start, end, deadline).unwrap(),
            event_total
        );
    }
}
