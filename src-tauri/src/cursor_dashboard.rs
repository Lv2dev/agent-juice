use crate::http_transport::{self, HttpErrorKind, HttpMethod};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{types::ValueRef, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt,
    io::Read,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const DASHBOARD_BASE_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService";
const ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";
const APPLICATION_USER_KEY: &str =
    "src.vs.platform.reactivestorage.browser.reactiveStorageServiceImpl.persistentStorage.applicationUser";
const MAX_TOKEN_BYTES: usize = 8 * 1024;
const MAX_APPLICATION_BYTES: usize = 2 * 1024 * 1024;
const MAX_CLI_AUTH_FILE_BYTES: u64 = 64 * 1024;
const MAX_CLI_CONFIG_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(200);
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

// Local token/config binding; this does not independently verify a JWT signature.
pub struct CliIdentityProof {
    pub scope: AccountScope,
    auth_root: PathBuf,
    config_path: PathBuf,
    auth_digest: [u8; 32],
    config_digest: [u8; 32],
    auth_id: String,
}

impl CliIdentityProof {
    pub(crate) fn auth_root(&self) -> &Path {
        &self.auth_root
    }

    pub(crate) fn isolated_config(&self) -> Value {
        let mut config = serde_json::json!({
            "authInfo": {
                "authId": self.auth_id,
                "userId": self.scope.user_id,
            }
        });
        if let Some(team_id) = self.scope.team_id {
            config["authInfo"]["teamId"] = serde_json::json!(team_id);
        }
        config
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

fn cli_auth_path() -> Result<PathBuf, DashboardError> {
    cli_auth_root().map(|root| root.join("Cursor").join("auth.json"))
}

fn cli_auth_root() -> Result<PathBuf, DashboardError> {
    #[cfg(windows)]
    let root = std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::config_dir);
    #[cfg(not(windows))]
    let root = dirs::config_dir();
    root.filter(|root| root.is_absolute())
        .ok_or_else(|| DashboardError::unavailable("Cursor CLI auth root unavailable"))
}

fn cli_config_path() -> Result<PathBuf, DashboardError> {
    dirs::home_dir()
        .map(|root| root.join(".cursor").join("cli-config.json"))
        .ok_or_else(|| DashboardError::unavailable("Cursor CLI config path unavailable"))
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
impl LockedStateFile {
    fn revalidate(&self) -> Result<(), DashboardError> {
        let current = lock_state_file(&self.path)?;
        if current.identity != self.identity {
            return Err(DashboardError::new(
                DashboardErrorKind::ScopeChanged,
                "Cursor credential file identity changed",
            ));
        }
        Ok(())
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
            file.revalidate()?;
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
    if !metadata.is_file() {
        return Err(DashboardError::unavailable(
            "Cursor state database rejected",
        ));
    }
    for sidecar in [sidecar_path(path, "wal"), sidecar_path(path, "shm")] {
        if sidecar.exists() {
            validate_path_components(&sidecar)?;
            let metadata = std::fs::metadata(&sidecar)
                .map_err(|_| DashboardError::unavailable("Cursor state sidecar unavailable"))?;
            if !metadata.is_file() {
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

fn valid_access_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~'))
}

fn validate_access_token(value: String) -> Result<String, DashboardError> {
    if !valid_access_token(&value) {
        return Err(DashboardError::login("Cursor login required"));
    }
    Ok(value)
}

fn read_bounded_credential_file(
    path: &Path,
    max_bytes: u64,
    deadline: Instant,
) -> Result<Vec<u8>, DashboardError> {
    remaining(deadline, "Cursor credentials deadline exceeded")?;
    validate_path_components(path)?;
    let metadata = std::fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            DashboardError::login("Cursor login required")
        } else {
            DashboardError::unavailable("Cursor credential file unavailable")
        }
    })?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(DashboardError::login("Cursor login required"));
    }
    #[cfg(windows)]
    let identity = lock_state_file(path)?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(64 * 1024));
    std::fs::File::open(path)
        .map_err(|_| DashboardError::unavailable("Cursor credential file unavailable"))?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DashboardError::unavailable("Cursor credential file read failed"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(DashboardError::login("Cursor login required"));
    }
    validate_path_components(path)?;
    #[cfg(windows)]
    identity.revalidate()?;
    remaining(deadline, "Cursor credentials deadline exceeded")?;
    Ok(bytes)
}

#[derive(Deserialize)]
struct CursorCliAuth {
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Deserialize)]
struct CursorCliConfig {
    #[serde(rename = "authInfo")]
    auth_info: Option<CursorCliAuthInfo>,
}

#[derive(Deserialize)]
struct CursorCliAuthInfo {
    #[serde(rename = "userId")]
    user_id: Value,
    #[serde(rename = "authId")]
    auth_id: Option<String>,
    #[serde(rename = "teamId")]
    team_id: Option<Value>,
}

fn cli_team_id(value: Option<&Value>) -> Result<Option<u64>, DashboardError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) if value == &serde_json::json!(0) || value == &serde_json::json!("0") => {
            Ok(None)
        }
        Some(value) => positive_u64(Some(value))
            .map(Some)
            .ok_or_else(|| DashboardError::unavailable("Cursor CLI team identity unavailable")),
    }
}

pub fn read_cli_identity(deadline: Instant) -> Result<CliIdentityProof, DashboardError> {
    let auth_root = cli_auth_root()?;
    read_cli_identity_from(&auth_root, &cli_config_path()?, deadline)
}

pub(crate) fn read_cli_identity_from(
    auth_root: &Path,
    config_path: &Path,
    deadline: Instant,
) -> Result<CliIdentityProof, DashboardError> {
    let auth_path = auth_root.join("Cursor").join("auth.json");
    let snapshot = read_cli_credential_snapshot_from(&auth_path, config_path, deadline)?;
    Ok(CliIdentityProof {
        scope: snapshot.scope,
        auth_root: auth_root.to_path_buf(),
        config_path: config_path.to_path_buf(),
        auth_digest: snapshot.auth_digest,
        config_digest: snapshot.config_digest,
        auth_id: snapshot.auth_id,
    })
}

struct CliCredentialSnapshot {
    access_token: Zeroizing<String>,
    scope: AccountScope,
    auth_digest: [u8; 32],
    config_digest: [u8; 32],
    auth_id: String,
}

fn read_cli_credential_snapshot_from(
    auth_path: &Path,
    config_path: &Path,
    deadline: Instant,
) -> Result<CliCredentialSnapshot, DashboardError> {
    let auth_bytes = Zeroizing::new(read_bounded_credential_file(
        auth_path,
        MAX_CLI_AUTH_FILE_BYTES,
        deadline,
    )?);
    let config_bytes = Zeroizing::new(read_bounded_credential_file(
        config_path,
        MAX_CLI_CONFIG_FILE_BYTES,
        deadline,
    )?);
    let mut auth: CursorCliAuth = serde_json::from_slice(&auth_bytes)
        .map_err(|_| DashboardError::parse("Cursor CLI auth was not recognized"))?;
    let token = Zeroizing::new(std::mem::take(&mut auth.access_token));
    if !valid_access_token(&token) {
        return Err(DashboardError::login("Cursor CLI login required"));
    }
    let config: CursorCliConfig = serde_json::from_slice(&config_bytes)
        .map_err(|_| DashboardError::parse("Cursor CLI config was not recognized"))?;
    let identity = config
        .auth_info
        .ok_or_else(|| DashboardError::unavailable("Cursor CLI identity unavailable"))?;
    let user_id = positive_u64(Some(&identity.user_id))
        .ok_or_else(|| DashboardError::unavailable("Cursor CLI user identity unavailable"))?;
    let auth_id = identity
        .auth_id
        .filter(|id| !id.is_empty() && id.len() <= MAX_TOKEN_BYTES)
        .ok_or_else(|| DashboardError::unavailable("Cursor CLI token identity unavailable"))?;
    if cli_token_subject(&token)? != auth_id {
        return Err(DashboardError::new(
            DashboardErrorKind::ScopeChanged,
            "Cursor CLI token and account metadata disagree",
        ));
    }
    let team_id = cli_team_id(identity.team_id.as_ref())?;
    let snapshot = CliCredentialSnapshot {
        access_token: token,
        scope: AccountScope { user_id, team_id },
        auth_digest: Sha256::digest(auth_bytes.as_slice()).into(),
        config_digest: Sha256::digest(config_bytes.as_slice()).into(),
        auth_id,
    };
    verify_cli_credential_binding(
        auth_path,
        config_path,
        &snapshot.auth_digest,
        &snapshot.config_digest,
        &snapshot.auth_id,
        deadline,
    )?;
    Ok(snapshot)
}

fn cli_token_subject(token: &str) -> Result<String, DashboardError> {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(DashboardError::unavailable(
            "Cursor CLI token identity unavailable",
        ));
    }
    let body = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| DashboardError::unavailable("Cursor CLI token identity unavailable"))?,
    );
    #[derive(Deserialize)]
    struct Claims {
        sub: String,
        exp: Option<u64>,
    }
    let claims: Claims = serde_json::from_slice(&body)
        .map_err(|_| DashboardError::unavailable("Cursor CLI token identity unavailable"))?;
    if claims.sub.is_empty()
        || claims
            .exp
            .is_some_and(|exp| exp <= chrono::Utc::now().timestamp().max(0) as u64)
    {
        return Err(DashboardError::login(
            "Cursor CLI token identity expired or unavailable",
        ));
    }
    Ok(claims.sub)
}

pub fn verify_cli_identity(
    proof: &CliIdentityProof,
    deadline: Instant,
) -> Result<(), DashboardError> {
    verify_cli_credential_binding(
        &proof.auth_root.join("Cursor").join("auth.json"),
        &proof.config_path,
        &proof.auth_digest,
        &proof.config_digest,
        &proof.auth_id,
        deadline,
    )
}

fn verify_cli_credential_binding(
    auth_path: &Path,
    config_path: &Path,
    auth_digest: &[u8; 32],
    config_digest: &[u8; 32],
    auth_id: &str,
    deadline: Instant,
) -> Result<(), DashboardError> {
    let auth = Zeroizing::new(read_bounded_credential_file(
        auth_path,
        MAX_CLI_AUTH_FILE_BYTES,
        deadline,
    )?);
    let config = Zeroizing::new(read_bounded_credential_file(
        config_path,
        MAX_CLI_CONFIG_FILE_BYTES,
        deadline,
    )?);
    if <[u8; 32]>::from(Sha256::digest(auth.as_slice())) != *auth_digest
        || <[u8; 32]>::from(Sha256::digest(config.as_slice())) != *config_digest
    {
        return Err(DashboardError::new(
            DashboardErrorKind::ScopeChanged,
            "Cursor CLI authentication changed during collection",
        ));
    }
    let mut auth: CursorCliAuth = serde_json::from_slice(&auth)
        .map_err(|_| DashboardError::parse("Cursor CLI auth was not recognized"))?;
    let token = Zeroizing::new(std::mem::take(&mut auth.access_token));
    if cli_token_subject(&token)? != auth_id {
        return Err(DashboardError::new(
            DashboardErrorKind::ScopeChanged,
            "Cursor CLI token identity changed during collection",
        ));
    }
    Ok(())
}

pub fn read_credentials(deadline: Instant) -> Result<DashboardCredentials, DashboardError> {
    remaining(deadline, "Cursor credentials deadline exceeded")?;
    let gui = state_db_path().and_then(|path| read_credentials_from(&path, deadline));
    resolve_credential_sources(gui, || {
        cli_auth_path().and_then(|auth| {
            cli_config_path().and_then(|config| read_cli_credentials_from(&auth, &config, deadline))
        })
    })
}

fn resolve_credential_sources(
    gui: Result<DashboardCredentials, DashboardError>,
    cli: impl FnOnce() -> Result<DashboardCredentials, DashboardError>,
) -> Result<DashboardCredentials, DashboardError> {
    match gui {
        Ok(credentials) => Ok(credentials),
        Err(gui_error) => {
            cli().map_err(|cli_error| preferred_credential_error(gui_error, cli_error))
        }
    }
}

fn preferred_credential_error(gui: DashboardError, cli: DashboardError) -> DashboardError {
    fn priority(kind: DashboardErrorKind) -> u8 {
        match kind {
            DashboardErrorKind::Deadline => 5,
            DashboardErrorKind::ScopeChanged => 4,
            DashboardErrorKind::Parse | DashboardErrorKind::Oversized => 3,
            DashboardErrorKind::LoginRequired => 2,
            DashboardErrorKind::Unavailable => 1,
            DashboardErrorKind::Transport => 0,
        }
    }

    if priority(gui.kind) >= priority(cli.kind) {
        gui
    } else {
        cli
    }
}

fn read_cli_credentials_from(
    auth_path: &Path,
    config_path: &Path,
    deadline: Instant,
) -> Result<DashboardCredentials, DashboardError> {
    let mut snapshot = read_cli_credential_snapshot_from(auth_path, config_path, deadline)?;
    Ok(DashboardCredentials {
        access_token: std::mem::take(&mut *snapshot.access_token),
        scope: snapshot.scope,
    })
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
        .map_err(|_| DashboardError::login("Cursor login required"))
        .and_then(validate_access_token)?;
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

fn dashboard_url(method: &'static str) -> Result<String, DashboardError> {
    match method {
        "GetCurrentPeriodUsage" | "GetFilteredUsageEvents" => {
            Ok(format!("{DASHBOARD_BASE_URL}/{method}"))
        }
        _ => Err(DashboardError::parse(
            "Cursor Dashboard method was not recognized",
        )),
    }
}

fn post_json<Request: Serialize, Output: DeserializeOwned>(
    credentials: &DashboardCredentials,
    method: &'static str,
    request: &Request,
    deadline: Instant,
    cap: usize,
) -> Result<Output, DashboardError> {
    remaining(deadline, "Cursor Dashboard deadline exceeded")?;
    let body = serde_json::to_string(request)
        .map_err(|_| DashboardError::parse("Cursor Dashboard request was not recognized"))?;
    let authorization = format!("Bearer {}", credentials.access_token());
    let response = http_transport::execute(
        HttpMethod::PostRead,
        &dashboard_url(method)?,
        &[
            ("authorization", authorization.as_str()),
            ("connect-protocol-version", "1"),
            ("content-type", "application/json"),
        ],
        Some(body.as_bytes()),
        deadline,
        cap,
        "Cursor Dashboard request failed",
    )
    .map_err(|error| match error.kind {
        HttpErrorKind::Deadline => DashboardError::deadline("Cursor Dashboard request timed out"),
        HttpErrorKind::Oversized => DashboardError::new(
            DashboardErrorKind::Oversized,
            "Cursor Dashboard response exceeded its limit",
        ),
        HttpErrorKind::InvalidRequest => {
            DashboardError::parse("Cursor Dashboard request was not recognized")
        }
        HttpErrorKind::Transport => DashboardError::transport("Cursor Dashboard request failed"),
    })?;
    let status = response.status;
    let content_type = response.content_type;
    let body = response.body;
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

    fn create_cli_credentials_fixture(
        name: &str,
        token: &str,
        user_id: u64,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-cli-{name}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let auth = root.join("Cursor").join("auth.json");
        let config = root.join(".cursor").join("cli-config.json");
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &auth,
            serde_json::json!({
                "accessToken": token,
                "refreshToken": "fixture-refresh-token-must-be-ignored"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &config,
            serde_json::json!({
                "authInfo": {
                    "userId": user_id,
                    "authId": "fixture-account-a",
                    "email": "fixture@example.invalid"
                }
            })
            .to_string(),
        )
        .unwrap();
        (root, auth, config)
    }

    fn identity_token(subject: &str, expires: u64) -> String {
        let body = serde_json::to_vec(&serde_json::json!({
            "sub": subject,
            "exp": expires,
        }))
        .unwrap();
        format!("e30.{}.fixture-signature", URL_SAFE_NO_PAD.encode(body))
    }

    fn create_cli_identity_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (root, auth, config) = create_cli_credentials_fixture(
            name,
            &identity_token("fixture-account-a", u64::MAX),
            77,
        );
        std::fs::write(
            &config,
            serde_json::json!({
                "authInfo": { "userId": 77, "authId": "fixture-account-a", "teamId": 7 },
                "email": "fixture-private@example.invalid",
                "unrelated": "fixture-private-config",
            })
            .to_string(),
        )
        .unwrap();
        (root, auth, config)
    }

    #[test]
    fn cli_identity_binds_token_subject_to_user_team_and_auth_root() {
        let (root, _, config) = create_cli_identity_fixture("identity-binding");
        let gui_path = root.join("Cursor/User/globalStorage/state.vscdb");
        std::fs::create_dir_all(gui_path.parent().unwrap()).unwrap();
        drop(create_credentials_fixture(&gui_path, false));
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(
            read_credentials_from(&gui_path, deadline)
                .unwrap()
                .scope
                .user_id,
            42
        );
        let proof = read_cli_identity_from(&root, &config, deadline).unwrap();
        assert_eq!(
            proof.scope,
            AccountScope {
                user_id: 77,
                team_id: Some(7)
            }
        );
        assert_eq!(proof.auth_root(), root);
        verify_cli_identity(&proof, deadline).unwrap();
        let isolated = proof.isolated_config().to_string();
        assert!(isolated.contains("fixture-account-a"));
        assert!(!isolated.contains("accessToken"));
        assert!(!isolated.contains("refreshToken"));
        assert!(!isolated.contains("fixture-private"));
        assert!(!isolated.contains("fixture-signature"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_identity_rejects_stale_metadata_opaque_tokens_and_expiry() {
        let (root, auth, config) = create_cli_identity_fixture("identity-rejected");
        let deadline = Instant::now() + Duration::from_secs(2);
        for (token, expected) in [
            (
                identity_token("fixture-account-b", u64::MAX),
                DashboardErrorKind::ScopeChanged,
            ),
            (
                "opaque-fixture-token".into(),
                DashboardErrorKind::Unavailable,
            ),
            (
                "header.invalid.signature".into(),
                DashboardErrorKind::Unavailable,
            ),
            (
                identity_token("fixture-account-a", 1),
                DashboardErrorKind::LoginRequired,
            ),
            (
                identity_token("", u64::MAX),
                DashboardErrorKind::LoginRequired,
            ),
            (
                "invalid fixture token".into(),
                DashboardErrorKind::LoginRequired,
            ),
        ] {
            std::fs::write(&auth, serde_json::json!({"accessToken":token}).to_string()).unwrap();
            let error = read_cli_identity_from(&root, &config, deadline)
                .err()
                .unwrap();
            assert_eq!(error.kind, expected);
            let credentials_error = read_cli_credentials_from(&auth, &config, deadline)
                .err()
                .unwrap();
            assert_eq!(credentials_error, error);
            assert!(!error.to_string().contains("fixture-account"));
            assert!(!format!("{error:?}").contains("signature"));
        }
        std::fs::write(
            &auth,
            serde_json::json!({
                "accessToken": identity_token("fixture-account-a", u64::MAX)
            })
            .to_string(),
        )
        .unwrap();
        for value in [
            serde_json::json!({"authInfo":{"userId":77}}),
            serde_json::json!({"authInfo":{"userId":77,"authId":""}}),
            serde_json::json!({"authInfo":{"userId":77,"authId":"fixture-account-b"}}),
        ] {
            std::fs::write(&config, value.to_string()).unwrap();
            let proof_error = read_cli_identity_from(&root, &config, deadline)
                .err()
                .unwrap();
            let credentials_error = read_cli_credentials_from(&auth, &config, deadline)
                .err()
                .unwrap();
            assert_eq!(credentials_error, proof_error);
            assert!(matches!(
                credentials_error.kind,
                DashboardErrorKind::Unavailable | DashboardErrorKind::ScopeChanged
            ));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_credential_snapshot_revalidation_rejects_file_changes() {
        let (root, auth, config) = create_cli_identity_fixture("snapshot-changes");
        let original_auth = std::fs::read(&auth).unwrap();
        let original_config = std::fs::read(&config).unwrap();
        for change_auth in [true, false] {
            let deadline = Instant::now() + Duration::from_secs(2);
            let snapshot = read_cli_credential_snapshot_from(&auth, &config, deadline).unwrap();
            if change_auth {
                std::fs::write(
                    &auth,
                    serde_json::json!({
                        "accessToken": identity_token("fixture-account-b", u64::MAX)
                    })
                    .to_string(),
                )
                .unwrap();
            } else {
                std::fs::write(
                    &config,
                    serde_json::json!({"authInfo": {
                        "userId": 88, "authId": "fixture-account-a", "teamId": 8
                    }})
                    .to_string(),
                )
                .unwrap();
            }
            let error = verify_cli_credential_binding(
                &auth,
                &config,
                &snapshot.auth_digest,
                &snapshot.config_digest,
                &snapshot.auth_id,
                deadline,
            )
            .unwrap_err();
            assert_eq!(error.kind, DashboardErrorKind::ScopeChanged);
            assert!(!error.to_string().contains("fixture-account"));
            std::fs::write(&auth, &original_auth).unwrap();
            std::fs::write(&config, &original_config).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_credentials_and_identity_accept_subject_without_optional_expiry() {
        let (root, auth, config) = create_cli_identity_fixture("optional-expiry");
        let body = URL_SAFE_NO_PAD.encode(br#"{"sub":"fixture-account-a"}"#);
        let token = format!("e30.{body}.fixture-signature");
        std::fs::write(&auth, serde_json::json!({"accessToken": token}).to_string()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let credentials = read_cli_credentials_from(&auth, &config, deadline).unwrap();
        let proof = read_cli_identity_from(&root, &config, deadline).unwrap();
        assert_eq!(credentials.scope, proof.scope);
        assert!(credentials.access_token() == token);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_identity_detects_token_rotation_config_changes_and_logout() {
        let (root, auth, config) = create_cli_identity_fixture("identity-changes");
        let deadline = Instant::now() + Duration::from_secs(2);
        let original_auth = std::fs::read(&auth).unwrap();
        let original_config = std::fs::read(&config).unwrap();
        for path in [&auth, &config] {
            let proof = read_cli_identity_from(&root, &config, deadline).unwrap();
            std::fs::write(path, b"changed-fixture").unwrap();
            assert_eq!(
                verify_cli_identity(&proof, deadline).unwrap_err().kind,
                DashboardErrorKind::ScopeChanged
            );
            std::fs::write(&auth, &original_auth).unwrap();
            std::fs::write(&config, &original_config).unwrap();
        }
        let proof = read_cli_identity_from(&root, &config, deadline).unwrap();
        std::fs::remove_file(&auth).unwrap();
        assert!(verify_cli_identity(&proof, deadline).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_identity_without_team_accepts_proto_zero() {
        let (root, _, config) = create_cli_identity_fixture("identity-no-team");
        for team in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!("0"),
        ] {
            std::fs::write(
                &config,
                serde_json::json!({"authInfo":{
                    "userId":77,"authId":"fixture-account-a","teamId":team
                }})
                .to_string(),
            )
            .unwrap();
            let proof =
                read_cli_identity_from(&root, &config, Instant::now() + Duration::from_secs(2))
                    .unwrap();
            assert_eq!(proof.scope.team_id, None);
            assert!(proof.isolated_config()["authInfo"].get("teamId").is_none());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_credentials_and_identity_share_personal_and_team_scopes() {
        let (root, auth, config) = create_cli_identity_fixture("shared-team-scope");
        for (team, expected) in [
            (None, None),
            (Some(Value::Null), None),
            (Some(serde_json::json!(0)), None),
            (Some(serde_json::json!("0")), None),
            (Some(serde_json::json!(7)), Some(7)),
            (Some(serde_json::json!("7")), Some(7)),
            (Some(serde_json::json!(u64::MAX)), Some(u64::MAX)),
            (
                Some(serde_json::json!(u64::MAX.to_string())),
                Some(u64::MAX),
            ),
        ] {
            let mut value = serde_json::json!({"authInfo": {
                "userId": 77, "authId": "fixture-account-a"
            }});
            if let Some(team) = team {
                value["authInfo"]["teamId"] = team;
            }
            std::fs::write(&config, value.to_string()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            let credentials = read_cli_credentials_from(&auth, &config, deadline).unwrap();
            let proof = read_cli_identity_from(&root, &config, deadline).unwrap();
            assert_eq!(
                credentials.scope,
                AccountScope {
                    user_id: 77,
                    team_id: expected
                }
            );
            assert_eq!(credentials.scope, proof.scope);
            assert_eq!(
                credentials.access_token(),
                identity_token("fixture-account-a", u64::MAX)
            );
            if let Some(team_id) = expected {
                assert_eq!(proof.isolated_config()["authInfo"]["teamId"], team_id);
            } else {
                assert!(proof.isolated_config()["authInfo"].get("teamId").is_none());
            }
            verify_cli_identity(&proof, deadline).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_credentials_and_identity_reject_invalid_team_scopes() {
        let (root, auth, config) = create_cli_identity_fixture("invalid-team-scope");
        for team in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(7.0),
            serde_json::json!(true),
            serde_json::json!(false),
            serde_json::json!(""),
            serde_json::json!("-1"),
            serde_json::json!("1.5"),
            serde_json::json!("fixture-private-invalid-team"),
            serde_json::json!("18446744073709551616"),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            std::fs::write(
                &config,
                serde_json::json!({"authInfo": {
                    "userId": 77, "authId": "fixture-account-a", "teamId": team
                }})
                .to_string(),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            let credentials_error = read_cli_credentials_from(&auth, &config, deadline)
                .err()
                .unwrap();
            let proof_error = read_cli_identity_from(&root, &config, deadline)
                .err()
                .unwrap();
            assert_eq!(credentials_error.kind, DashboardErrorKind::Unavailable);
            assert_eq!(credentials_error, proof_error);
            assert!(!credentials_error.to_string().contains("fixture"));
            assert!(!format!("{credentials_error:?}").contains("signature"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cli_shared_scope_still_detects_team_changes() {
        let (root, auth, config) = create_cli_identity_fixture("team-scope-change");
        let deadline = Instant::now() + Duration::from_secs(2);
        let credentials = read_cli_credentials_from(&auth, &config, deadline).unwrap();
        let proof = read_cli_identity_from(&root, &config, deadline).unwrap();
        assert_eq!(credentials.scope, proof.scope);
        std::fs::write(
            &config,
            serde_json::json!({"authInfo": {
                "userId": 77, "authId": "fixture-account-a", "teamId": 8
            }})
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            verify_cli_identity(&proof, deadline).unwrap_err().kind,
            DashboardErrorKind::ScopeChanged
        );
        let changed_credentials = read_cli_credentials_from(&auth, &config, deadline).unwrap();
        let changed_proof = read_cli_identity_from(&root, &config, deadline).unwrap();
        assert_eq!(changed_credentials.scope, changed_proof.scope);
        assert_eq!(changed_proof.scope.team_id, Some(8));
        assert_ne!(credentials.scope, changed_proof.scope);
        std::fs::remove_dir_all(root).unwrap();
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
    fn dashboard_transport_accepts_only_known_constant_methods() {
        assert!(dashboard_url("GetAggregatedUsageEvents").is_err());
        for method in ["GetCurrentPeriodUsage", "GetFilteredUsageEvents"] {
            assert_eq!(
                dashboard_url(method).unwrap(),
                format!("{DASHBOARD_BASE_URL}/{method}")
            );
        }
        assert_eq!(
            dashboard_url("https://example.invalid").unwrap_err().kind,
            DashboardErrorKind::Parse
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
    fn credential_sources_prefer_gui_without_reading_cli() {
        let path = fixture_path("gui-preferred");
        let connection = create_credentials_fixture(&path, false);
        drop(connection);
        let gui = read_credentials_from(&path, Instant::now() + Duration::from_secs(2));
        let credentials = resolve_credential_sources(gui, || {
            panic!("CLI source must not be read when GUI credentials are valid")
        })
        .unwrap();
        assert_eq!(credentials.scope.user_id, 42);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn credential_sources_fall_back_to_cli_access_token_and_user_id() {
        let token = identity_token("fixture-account-a", u64::MAX);
        let (root, auth, config) = create_cli_credentials_fixture("fallback", &token, 77);
        let credentials = resolve_credential_sources(
            Err(DashboardError::login("Cursor GUI login unavailable")),
            || read_cli_credentials_from(&auth, &config, Instant::now() + Duration::from_secs(2)),
        )
        .unwrap();
        assert!(credentials.access_token() == token);
        assert_eq!(credentials.scope.user_id, 77);
        assert_eq!(credentials.scope.team_id, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn credentials_ignore_unrelated_large_cursor_disk_kv_content() {
        const LEGACY_DB_REJECTION_BYTES: u64 = 64 * 1024 * 1024;

        let path = fixture_path("large-cursor-disk-kv");
        let connection = create_credentials_fixture(&path, false);
        connection
            .execute("CREATE TABLE cursorDiskKV (key TEXT, value BLOB)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES ('large', zeroblob(?1))",
                [i64::try_from(LEGACY_DB_REJECTION_BYTES + 1024 * 1024).unwrap()],
            )
            .unwrap();
        drop(connection);
        assert!(std::fs::metadata(&path).unwrap().len() > LEGACY_DB_REJECTION_BYTES);

        let credentials =
            read_credentials_from(&path, Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(credentials.scope.user_id, 42);
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
        for (label, key, value) in [
            (
                "oversized-token",
                ACCESS_TOKEN_KEY,
                "x".repeat(MAX_TOKEN_BYTES + 1),
            ),
            (
                "oversized-application",
                APPLICATION_USER_KEY,
                "x".repeat(MAX_APPLICATION_BYTES + 1),
            ),
        ] {
            let path = fixture_path(label);
            let connection = create_credentials_fixture(&path, false);
            connection
                .execute(
                    "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                    (&value, key),
                )
                .unwrap();
            drop(connection);
            let error = match read_credentials_from(&path, Instant::now() + Duration::from_secs(2))
            {
                Ok(_) => panic!("oversized Cursor credential value was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.kind, DashboardErrorKind::LoginRequired);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn cli_credentials_reject_oversized_files_and_invalid_identity() {
        let token = identity_token("fixture-account-a", u64::MAX);
        let (root, auth, config) = create_cli_credentials_fixture("oversized", &token, 77);
        std::fs::write(&auth, vec![b'x'; MAX_CLI_AUTH_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read_cli_credentials_from(&auth, &config, Instant::now() + Duration::from_secs(2))
                .err()
                .unwrap()
                .kind,
            DashboardErrorKind::LoginRequired
        );

        std::fs::write(
            &auth,
            serde_json::json!({"accessToken": token, "refreshToken": "ignored"}).to_string(),
        )
        .unwrap();
        std::fs::write(&config, r#"{"authInfo":{"userId":0}}"#).unwrap();
        assert_eq!(
            read_cli_credentials_from(&auth, &config, Instant::now() + Duration::from_secs(2))
                .err()
                .unwrap()
                .kind,
            DashboardErrorKind::Unavailable
        );
        std::fs::remove_dir_all(root).unwrap();
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
    #[ignore = "uses the locally logged-in Cursor CLI account"]
    fn live_cli_credentials_and_current_period_round_trip() {
        let deadline = Instant::now() + Duration::from_secs(8);
        let credentials = read_cli_credentials_from(
            &cli_auth_path().unwrap(),
            &cli_config_path().unwrap(),
            deadline,
        )
        .unwrap();
        let usage = current_period_usage(&credentials, deadline).unwrap();
        assert!((0.0..=100.0).contains(&usage.cursor_models_used_percent));
        assert!((0.0..=100.0).contains(&usage.other_models_used_percent));
        assert!(usage.billing_cycle_end_ms > 0);
    }
}
