//! Desktop account quota reads without starting a CLI or refreshing credentials.
use crate::http_transport::{self, HttpErrorKind, HttpMethod};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};
use zeroize::{Zeroize, Zeroizing};

const FILE_CAP: u64 = 1024 * 1024;
const RESPONSE_CAP: usize = 256 * 1024;
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

#[cfg(test)]
#[derive(Clone, Debug, Default)]
struct Diagnostic {
    profile_http_status: Option<u16>,
    usage_http_status: Option<u16>,
    content_type: Option<&'static str>,
    five_hour: Option<(&'static str, &'static str)>,
    seven_day: Option<(&'static str, &'static str)>,
}
#[cfg(test)]
thread_local! {static DIAGNOSTIC:std::cell::RefCell<Diagnostic>=std::cell::RefCell::new(Diagnostic::default());}
#[cfg(test)]
fn observe_http(url: &str, response: &http_transport::HttpResponse) {
    DIAGNOSTIC.with(|d| {
        let mut d = d.borrow_mut();
        if url == PROFILE_URL {
            d.profile_http_status = Some(response.status);
            return;
        }
        d.usage_http_status = Some(response.status);
        d.content_type = Some(
            match response.content_type.split(';').next().map(str::trim) {
                Some("application/json") => "application/json",
                Some("text/html") => "text/html",
                _ => "other",
            },
        );
        fn kind(value: Option<&serde_json::Value>) -> &'static str {
            match value {
                None => "missing",
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::String(_)) => "string",
                Some(serde_json::Value::Number(_)) => "number",
                Some(serde_json::Value::Bool(_)) => "boolean",
                Some(serde_json::Value::Array(_)) => "array",
                Some(serde_json::Value::Object(_)) => "object",
            }
        }
        if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&response.body) {
            let fields = |name: &str| {
                let w = body.get(name);
                (
                    kind(w.and_then(|w| w.get("utilization"))),
                    kind(w.and_then(|w| w.get("resets_at"))),
                )
            };
            d.five_hour = Some(fields("five_hour"));
            d.seven_day = Some(fields("seven_day"));
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Unavailable,
    LoginRequired,
    Transport,
    Deadline,
    Parse,
    Changed,
    RateLimited,
    Decode,
}

#[derive(PartialEq, Eq)]
pub(crate) enum Revision {
    Files(Vec<(PathBuf, Option<(u64, SystemTime)>)>, bool),
    Desktop(PathBuf, String, String),
}

fn code_path() -> Option<PathBuf> {
    dirs::home_dir().map(|p| p.join(".claude/.credentials.json"))
}

fn profile_paths() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        [
            dirs::config_dir().map(|p| p.join("Claude")),
            dirs::data_local_dir()
                .map(|p| p.join("Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude")),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub(crate) fn selected() -> bool {
    code_path().is_some_and(|p| selected_from(&p, &profile_paths()))
}

fn selected_from(code: &Path, roots: &[PathBuf]) -> bool {
    code_unavailable(code)
        && roots
            .iter()
            .any(|p| checked_metadata(&p.join("config.json")).is_ok_and(|m| m.is_file()))
}

pub(crate) fn revision(desktop: bool) -> Revision {
    let paths = code_path().into_iter().chain(
        profile_paths()
            .into_iter()
            .flat_map(|p| [p.join("config.json"), p.join("Local State")]),
    );
    Revision::Files(
        paths
            .map(|p| {
                let stamp = checked_metadata(&p)
                    .ok()
                    .and_then(|m| m.modified().ok().map(|t| (m.len(), t)));
                (p, stamp)
            })
            .collect(),
        desktop,
    )
}

fn code_unavailable(path: &Path) -> bool {
    if matches!(checked_metadata(path), Err(e) if e.kind()==std::io::ErrorKind::NotFound) {
        return true;
    }
    let Ok(bytes) = read_file(path) else {
        return false;
    };
    #[derive(Deserialize)]
    struct File {
        #[serde(rename = "claudeAiOauth")]
        oauth: Option<Oauth>,
    }
    #[derive(Deserialize)]
    struct Oauth {
        #[serde(rename = "accessToken")]
        token: Option<String>,
        #[serde(rename = "expiresAt")]
        expires: Option<u64>,
    }
    let Ok(mut file) = serde_json::from_slice::<File>(&bytes) else {
        return false;
    };
    let Some(oauth) = file.oauth.as_mut() else {
        return true;
    };
    let absent = oauth.token.as_deref().is_none_or(|t| !valid_token(t))
        || oauth.expires.is_some_and(|t| t <= now_ms());
    if let Some(token) = oauth.token.as_mut() {
        token.zeroize();
    }
    absent
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn validate_path(path: &Path) -> Result<(), Error> {
    checked_metadata(path)
        .map(|_| ())
        .map_err(|_| Error::Unavailable)
}

fn checked_metadata(path: &Path) -> std::io::Result<std::fs::Metadata> {
    let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    if !path.is_absolute() {
        return Err(denied());
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !matches!(path.components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
        {
            return Err(denied());
        }
    }
    let mut current = PathBuf::new();
    let mut final_metadata = None;
    for component in path.components() {
        current.push(component);
        if !current.is_absolute() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&current)?;
        if is_reparse(&metadata) {
            return Err(denied());
        }
        final_metadata = Some(metadata);
    }
    final_metadata.ok_or_else(denied)
}

fn read_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, Error> {
    validate_path(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    let mut file = options.open(path).map_err(|_| Error::Unavailable)?;
    let before = file.metadata().map_err(|_| Error::Unavailable)?;
    if !before.is_file() || is_reparse(&before) || before.len() > FILE_CAP {
        return Err(Error::Unavailable);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    (&mut file)
        .take(FILE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    let after = file.metadata().map_err(|_| Error::Unavailable)?;
    if bytes.len() as u64 > FILE_CAP
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(Error::Changed);
    }
    validate_path(path)?;
    Ok(bytes)
}

#[derive(Deserialize)]
struct Config {
    #[serde(rename = "lastKnownAccountUuid")]
    account: Option<String>,
    #[serde(
        default,
        rename = "oauth:tokenCacheV2",
        deserialize_with = "scoped_cache"
    )]
    scoped: Option<String>,
    #[serde(rename = "oauth:tokenCache")]
    legacy: Option<String>,
}
#[derive(Deserialize)]
struct LocalState {
    os_crypt: OsCrypt,
}

fn scoped_cache<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}
#[derive(Deserialize)]
struct OsCrypt {
    encrypted_key: String,
    app_bound_encrypted_key: Option<String>,
}
#[derive(Deserialize)]
struct Token {
    token: String,
    #[serde(rename = "expiresAt")]
    expires: u64,
}
impl Drop for Token {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

pub(crate) struct Credentials {
    token: Zeroizing<String>,
    account: String,
    org: String,
    expires: u64,
    root: PathBuf,
    config: Zeroizing<Vec<u8>>,
    state: Zeroizing<Vec<u8>>,
}

impl Credentials {
    pub(crate) fn revision(&self) -> Revision {
        Revision::Desktop(
            self.root.clone(),
            self.account.to_ascii_lowercase(),
            self.org.to_ascii_lowercase(),
        )
    }
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8192
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn select_token(
    bytes: &[u8],
    account: &str,
    now: u64,
) -> Result<(Zeroizing<String>, String, u64), Error> {
    if !uuid(account) {
        return Err(Error::Unavailable);
    }
    let entries: BTreeMap<String, Option<Token>> =
        serde_json::from_slice(bytes).map_err(|_| Error::Parse)?;
    if entries.len() > 64 {
        return Err(Error::Unavailable);
    }
    let mut selected: Option<(usize, Token, String)> = None;
    for (key, token) in entries {
        let Some(token) = token else {
            continue;
        };
        let key = if let Some((owner, tail)) =
            key.strip_prefix("acct:").and_then(|k| k.split_once('|'))
        {
            if !owner.eq_ignore_ascii_case(account) {
                continue;
            }
            tail
        } else {
            key.as_str()
        };
        let Some((client, tail)) = key.split_once(':') else {
            continue;
        };
        let Some((org, tail)) = tail.split_once(':') else {
            continue;
        };
        let Some(scope) = tail.strip_prefix("https://api.anthropic.com:") else {
            continue;
        };
        if !uuid(client) || !uuid(org) {
            continue;
        }
        let scopes: Vec<_> = scope.split_ascii_whitespace().collect();
        if !scopes.contains(&"user:profile")
            || !valid_token(&token.token)
            || token.expires <= now.saturating_add(5000)
        {
            continue;
        }
        if selected
            .as_ref()
            .is_some_and(|(_, _, prev)| !prev.eq_ignore_ascii_case(org))
        {
            return Err(Error::Unavailable);
        }
        let rank = scopes.len() + usize::from(scopes.contains(&"user:sessions:claude_code")) * 100;
        if selected.as_ref().is_none_or(|(prev, _, _)| rank < *prev) {
            selected = Some((rank, token, org.to_ascii_lowercase()));
        }
    }
    let (_, mut token, org) = selected.ok_or(Error::Unavailable)?;
    Ok((
        Zeroizing::new(std::mem::take(&mut token.token)),
        org,
        token.expires,
    ))
}

fn credentials_from(root: &Path) -> Result<Credentials, Error> {
    let config = read_file(&root.join("config.json"))?;
    let state = read_file(&root.join("Local State"))?;
    let data: Config = serde_json::from_slice(&config).map_err(|_| Error::Decode)?;
    let local: LocalState = serde_json::from_slice(&state).map_err(|_| Error::Decode)?;
    let account = data.account.filter(|a| uuid(a)).ok_or(Error::Unavailable)?;
    if local.os_crypt.app_bound_encrypted_key.is_some() {
        return Err(Error::Decode);
    }
    let protected = STANDARD
        .decode(&local.os_crypt.encrypted_key)
        .map_err(|_| Error::Decode)?;
    let protected = protected.strip_prefix(b"DPAPI").ok_or(Error::Decode)?;
    let key = unprotect(protected).map_err(|_| Error::Decode)?;
    let encoded = data.scoped.or(data.legacy).ok_or(Error::Unavailable)?;
    let encrypted = STANDARD.decode(encoded).map_err(|_| Error::Decode)?;
    let decoded = decrypt(&key, &encrypted).map_err(|_| Error::Decode)?;
    let (token, org, expires) = select_token(&decoded, &account, now_ms()).map_err(|e| {
        if e == Error::Parse {
            Error::Decode
        } else {
            e
        }
    })?;
    Ok(Credentials {
        token,
        org,
        expires,
        account,
        root: root.into(),
        config,
        state,
    })
}

fn credentials_unchanged(c: &Credentials) -> bool {
    c.expires > now_ms()
        && read_file(&c.root.join("config.json")).is_ok_and(|b| b.as_slice() == c.config.as_slice())
        && read_file(&c.root.join("Local State")).is_ok_and(|b| b.as_slice() == c.state.as_slice())
}

fn http(url: &str, token: &str, deadline: Instant) -> Result<Vec<u8>, Error> {
    let authorization = Zeroizing::new(format!("Bearer {token}"));
    let response = http_transport::execute(
        HttpMethod::Get,
        url,
        &[
            ("authorization", authorization.as_str()),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("user-agent", concat!("Juice/", env!("CARGO_PKG_VERSION"))),
        ],
        None,
        deadline,
        RESPONSE_CAP,
        "Claude desktop account request failed",
    )
    .map_err(|e| match e.kind {
        HttpErrorKind::Deadline => Error::Deadline,
        _ => Error::Transport,
    })?;
    #[cfg(test)]
    observe_http(url, &response);
    response_body(response)
}

fn response_body(response: http_transport::HttpResponse) -> Result<Vec<u8>, Error> {
    if response.status == 429 {
        return Err(Error::RateLimited);
    }
    if response.status == 401 {
        return Err(Error::LoginRequired);
    }
    // A forbidden scope or gateway rejection is not evidence of a GUI sign-out.
    if !(200..300).contains(&response.status) {
        return Err(Error::Transport);
    }
    if response.body.len() > RESPONSE_CAP
        || response.content_type.split(';').next().map(str::trim) != Some("application/json")
    {
        return Err(Error::Parse);
    }
    Ok(response.body)
}

fn matching_profile(body: &[u8], account: &str, org: &str) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Identity {
        uuid: String,
    }
    #[derive(Deserialize)]
    struct Profile {
        account: Identity,
        organization: Identity,
    }
    let profile: Profile = serde_json::from_slice(body).map_err(|_| Error::Parse)?;
    if !profile.account.uuid.eq_ignore_ascii_case(account)
        || !profile.organization.uuid.eq_ignore_ascii_case(org)
    {
        return Err(Error::Changed);
    }
    Ok(())
}

pub(crate) fn prepare() -> Result<Credentials, Error> {
    #[cfg(test)]
    DIAGNOSTIC.with(|d| *d.borrow_mut() = Diagnostic::default());
    let roots: Vec<_> = profile_paths()
        .into_iter()
        .filter(|p| checked_metadata(&p.join("config.json")).is_ok_and(|m| m.is_file()))
        .collect();
    if roots.len() != 1 {
        return Err(Error::Unavailable);
    }
    credentials_from(&roots[0])
}

#[cfg(test)]
pub(crate) fn usage(deadline: Instant) -> Result<String, Error> {
    usage_prepared(prepare()?, deadline)
}

pub(crate) fn usage_prepared(c: Credentials, deadline: Instant) -> Result<String, Error> {
    query_credentials_cached(c, deadline, http, &PROFILE_PROOF)
}

#[cfg(test)]
fn query_credentials(
    c: Credentials,
    deadline: Instant,
    mut fetch: impl FnMut(&str, &str, Instant) -> Result<Vec<u8>, Error>,
) -> Result<String, Error> {
    query_credentials_cached(c, deadline, &mut fetch, &std::sync::Mutex::new(None))
}

struct ProfileProof {
    revision: Revision,
    token_digest: [u8; 32],
    valid_until: Instant,
}
// Reuse identity verification briefly, but never across tokens, accounts or organizations.
static PROFILE_PROOF: std::sync::Mutex<Option<ProfileProof>> = std::sync::Mutex::new(None);

fn query_credentials_cached(
    c: Credentials,
    deadline: Instant,
    mut fetch: impl FnMut(&str, &str, Instant) -> Result<Vec<u8>, Error>,
    proof: &std::sync::Mutex<Option<ProfileProof>>,
) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    if Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    if c.expires <= now_ms() || !credentials_unchanged(&c) {
        return Err(Error::Changed);
    }
    let revision = c.revision();
    let token_digest: [u8; 32] = Sha256::digest(c.token.as_bytes()).into();
    let verified = proof
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|p| {
            p.revision == revision
                && p.token_digest == token_digest
                && Instant::now() < p.valid_until
        });
    let result = (|| {
        if !verified {
            *proof.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let profile = Zeroizing::new(fetch(PROFILE_URL, &c.token, deadline)?);
            matching_profile(&profile, &c.account, &c.org)?;
            *proof.lock().unwrap_or_else(|e| e.into_inner()) = Some(ProfileProof {
                revision,
                token_digest,
                valid_until: Instant::now() + std::time::Duration::from_secs(300),
            });
        }
        let body = fetch(USAGE_URL, &c.token, deadline).inspect_err(|e| {
            if matches!(e, Error::LoginRequired | Error::Changed) {
                *proof.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        })?;
        String::from_utf8(body).map_err(|_| Error::Parse)
    })();
    if !credentials_unchanged(&c) {
        *proof.lock().unwrap_or_else(|e| e.into_inner()) = None;
        return Err(Error::Changed);
    }
    if Instant::now() >= deadline && result.is_ok() {
        return Err(Error::Deadline);
    }
    result
}

#[cfg(windows)]
mod crypto;
#[cfg(windows)]
use crypto::{decrypt, unprotect};
#[cfg(not(windows))]
fn decrypt(_: &[u8], _: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    Err(Error::Unavailable)
}
#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    Err(Error::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const ACCOUNT: &str = "11111111-1111-4111-8111-111111111111";
    const ORG: &str = "22222222-2222-4222-8222-222222222222";
    const OTHER: &str = "33333333-3333-4333-8333-333333333333";
    #[test]
    #[ignore = "one read-only Desktop diagnostic; emits only kinds, statuses, MIME and field types"]
    fn live_desktop_safe_diagnostic() {
        let result = usage(Instant::now() + std::time::Duration::from_secs(10));
        let usage_error = result.as_ref().err().copied();
        let adapter_error = result.as_ref().ok().and_then(|raw| {
            crate::adapters::claude::parse_oauth_usage_response(
                raw,
                "test",
                &chrono::Utc::now().to_rfc3339(),
            )
            .err()
            .map(|_| "Parse")
        });
        DIAGNOSTIC.with(|d| {
            eprintln!(
                "usage_error={usage_error:?}; adapter_error={adapter_error:?}; {:?}",
                d.borrow()
            )
        });
    }
    fn cache_key(account: &str, org: &str, scope: &str) -> String {
        format!("acct:{account}|00000000-0000-4000-8000-000000000000:{org}:https://api.anthropic.com:{scope}")
    }
    fn entry(token: &str, expires: u64) -> serde_json::Value {
        json!({"token":token,"expiresAt":expires})
    }
    #[test]
    fn selects_only_active_account_and_prefers_non_code_scope() {
        let bytes=serde_json::to_vec(&json!({
            cache_key(OTHER,OTHER,"user:profile"):entry("other-fixture",10000),
            cache_key(ACCOUNT,ORG,"user:profile user:sessions:claude_code"):entry("code-fixture",10000),
            cache_key(ACCOUNT,ORG,"user:profile"):entry("gui-fixture",10000)
        })).unwrap();
        let (token, org, _) = select_token(&bytes, ACCOUNT, 0).unwrap();
        assert_eq!(token.as_str(), "gui-fixture");
        assert_eq!(org, ORG);
    }

    #[test]
    fn cleared_v2_cache_cannot_resurrect_a_legacy_token() {
        assert!(serde_json::from_str::<Config>(
            r#"{"oauth:tokenCacheV2":null,"oauth:tokenCache":"legacy-fixture"}"#
        )
        .is_err());
        let data: Config = serde_json::from_str(
            r#"{"oauth:tokenCacheV2":"","oauth:tokenCache":"legacy-fixture"}"#,
        )
        .unwrap();
        assert_eq!(data.scoped.or(data.legacy).as_deref(), Some(""));
        assert!(select_token(b"{}", ACCOUNT, 0).is_err());
    }
    #[test]
    fn cache_rejects_expiry_invalid_tokens_wrong_hosts_and_ambiguous_orgs() {
        for value in [
            json!({}),
            json!({cache_key(ACCOUNT,ORG,"user:profile"):null}),
            json!({cache_key(ACCOUNT,ORG,"user:profile"):entry("fixture",5000)}),
            json!({cache_key(ACCOUNT,ORG,"user:profile"):entry("bad\r\ntoken",10000)}),
            json!({cache_key(OTHER,ORG,"user:profile"):entry("fixture",10000)}),
            json!({cache_key(ACCOUNT,ORG,"user:inference"):entry("fixture",10000)}),
            json!({cache_key(ACCOUNT,ORG,"user:profile").replace("api.anthropic.com","attacker.invalid"):entry("fixture",10000)}),
            json!({cache_key(ACCOUNT,ORG,"user:profile"):entry("fixture",10000),cache_key(ACCOUNT,OTHER,"user:profile"):entry("other",10000)}),
        ] {
            assert!(select_token(&serde_json::to_vec(&value).unwrap(), ACCOUNT, 0).is_err());
        }
    }
    #[test]
    fn legacy_keys_still_require_profile_identity_validation() {
        let key = cache_key(ACCOUNT, ORG, "user:profile");
        let key = key.split_once('|').unwrap().1;
        let bytes = serde_json::to_vec(&json!({key:entry("fixture",10000)})).unwrap();
        assert!(select_token(&bytes, ACCOUNT, 0).is_ok());
        assert_eq!(
            matching_profile(
                &serde_json::to_vec(&json!({"account":{"uuid":OTHER},"organization":{"uuid":ORG}}))
                    .unwrap(),
                ACCOUNT,
                ORG
            ),
            Err(Error::Changed)
        );
        assert_eq!(matching_profile(b"{}", ACCOUNT, ORG), Err(Error::Parse));
    }

    #[test]
    fn source_revision_changes_clear_success_and_backoff() {
        use std::sync::Mutex;
        let previous = Mutex::new(None);
        let cache = Mutex::new(None);
        let revision = |desktop| Revision::Files(vec![], desktop);
        crate::reconcile_claude_source(&cache, &previous, revision(false));
        crate::cached_status_attempt(&cache, chrono::Utc::now(), 60, true, || {
            Err(crate::CollectionErrorKind::LoginRequired)
        });
        assert!(cache.lock().unwrap().is_some());
        crate::reconcile_claude_source(&cache, &previous, revision(false));
        assert!(cache.lock().unwrap().is_some());
        crate::reconcile_claude_source(&cache, &previous, revision(true));
        assert!(cache.lock().unwrap().is_none());
        crate::cached_status_attempt(&cache, chrono::Utc::now(), 60, true, || {
            Err(crate::CollectionErrorKind::Unavailable)
        });
        crate::reconcile_claude_source(
            &cache,
            &previous,
            Revision::Files(vec![(PathBuf::from("fixture"), None)], true),
        );
        assert!(cache.lock().unwrap().is_none());
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "juice-desktop-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture_credentials(root: &Path) -> Credentials {
        std::fs::write(root.join("config.json"), b"config-fixture").unwrap();
        std::fs::write(root.join("Local State"), b"state-fixture").unwrap();
        Credentials {
            token: Zeroizing::new("fixture".into()),
            account: ACCOUNT.into(),
            org: ORG.into(),
            expires: now_ms() + 60000,
            root: root.into(),
            config: Zeroizing::new(b"config-fixture".to_vec()),
            state: Zeroizing::new(b"state-fixture".to_vec()),
        }
    }
    fn profile() -> Vec<u8> {
        serde_json::to_vec(&json!({"account":{"uuid":ACCOUNT},"organization":{"uuid":ORG}}))
            .unwrap()
    }
    #[test]
    fn no_usage_request_for_wrong_identity_and_no_result_after_logout() {
        let dir = Temp::new();
        let c = fixture_credentials(&dir.0);
        let mut calls = 0;
        let result = query_credentials(
            c,
            Instant::now() + std::time::Duration::from_secs(1),
            |url, _, _| {
                calls += 1;
                assert_eq!(url, PROFILE_URL);
                Ok(serde_json::to_vec(
                    &json!({"account":{"uuid":ACCOUNT},"organization":{"uuid":OTHER}}),
                )
                .unwrap())
            },
        );
        assert_eq!(result, Err(Error::Changed));
        assert_eq!(calls, 1);
        let c = fixture_credentials(&dir.0);
        let result = query_credentials(
            c,
            Instant::now() + std::time::Duration::from_secs(1),
            |url, _, _| {
                if url == PROFILE_URL {
                    return Ok(profile());
                }
                assert_eq!(url, USAGE_URL);
                std::fs::write(dir.0.join("config.json"), b"logged-out").unwrap();
                Ok(b"{}".to_vec())
            },
        );
        assert_eq!(result, Err(Error::Changed));
    }
    #[test]
    fn successful_read_is_bounded_to_two_read_endpoints_and_expiry_clears() {
        let dir = Temp::new();
        let c = fixture_credentials(&dir.0);
        let mut calls = Vec::new();
        let result = query_credentials(
            c,
            Instant::now() + std::time::Duration::from_secs(1),
            |url, token, _| {
                assert_eq!(token, "fixture");
                calls.push(url.to_string());
                Ok(if url == PROFILE_URL {
                    profile()
                } else {
                    br#"{"five_hour":{"utilization":20}}"#.to_vec()
                })
            },
        )
        .unwrap();
        assert!(result.contains("five_hour"));
        assert_eq!(calls, [PROFILE_URL, USAGE_URL]);
        let mut c = fixture_credentials(&dir.0);
        c.expires = 0;
        assert_eq!(
            query_credentials(
                c,
                Instant::now() + std::time::Duration::from_secs(1),
                |_, _, _| panic!("expired credential")
            ),
            Err(Error::Changed)
        );
    }

    #[test]
    fn http_errors_do_not_turn_permissions_or_transport_failures_into_logout() {
        let response = |status, content_type: &str| http_transport::HttpResponse {
            status,
            content_type: content_type.into(),
            body: b"fixture-private-response".to_vec(),
        };
        assert_eq!(
            response_body(response(401, "application/json")),
            Err(Error::LoginRequired)
        );
        assert_eq!(
            response_body(response(429, "application/json")),
            Err(Error::RateLimited)
        );
        for status in [302, 403, 500] {
            assert_eq!(
                response_body(response(status, "application/json")),
                Err(Error::Transport)
            );
        }
        assert_eq!(response_body(response(200, "text/html")), Err(Error::Parse));
        assert_eq!(
            response_body(response(200, "application/json-invalid")),
            Err(Error::Parse)
        );
        assert!(response_body(response(200, "application/json; charset=utf-8")).is_ok());
        assert!(!format!("{:?}", Error::Transport).contains("fixture-private-response"));
        let dir = Temp::new();
        let c = fixture_credentials(&dir.0);
        assert_eq!(
            query_credentials(c, Instant::now(), |_, _, _| panic!("deadline")),
            Err(Error::Deadline)
        );
    }
    #[test]
    fn profile_proof_is_bounded_to_token_identity_and_lifetime() {
        let dir = Temp::new();
        let proof = std::sync::Mutex::new(None);
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let mut profiles = 0;
        let mut usages = 0;
        let mut run = |c, error: Option<Error>| {
            query_credentials_cached(
                c,
                deadline,
                |url, _, _| {
                    if url == PROFILE_URL {
                        profiles += 1;
                        Ok(profile())
                    } else {
                        usages += 1;
                        error.map_or_else(|| Ok(b"{}".to_vec()), Err)
                    }
                },
                &proof,
            )
        };
        assert!(run(fixture_credentials(&dir.0), None).is_ok());
        assert_eq!(
            run(fixture_credentials(&dir.0), Some(Error::RateLimited)),
            Err(Error::RateLimited)
        );
        assert!(run(fixture_credentials(&dir.0), None).is_ok());
        let mut c = fixture_credentials(&dir.0);
        c.token = Zeroizing::new("rotated-fixture".into());
        assert!(run(c, None).is_ok());
        proof.lock().unwrap().as_mut().unwrap().valid_until = Instant::now();
        assert!(run(fixture_credentials(&dir.0), None).is_ok());
        assert_eq!(
            run(fixture_credentials(&dir.0), Some(Error::LoginRequired)),
            Err(Error::LoginRequired)
        );
        assert!(proof.lock().unwrap().is_none());
        assert!(run(fixture_credentials(&dir.0), None).is_ok());
        assert_eq!(profiles, 4);
        assert_eq!(usages, 7);
        let mut c = fixture_credentials(&dir.0);
        c.org = OTHER.into();
        assert_eq!(
            query_credentials_cached(
                c,
                deadline,
                |url, _, _| {
                    assert_eq!(url, PROFILE_URL);
                    Ok(profile())
                },
                &proof
            ),
            Err(Error::Changed)
        );
        assert!(proof.lock().unwrap().is_none());
    }

    #[test]
    fn failed_request_cannot_keep_values_after_credentials_change() {
        let dir = Temp::new();
        for error in [Error::RateLimited, Error::Transport, Error::Parse] {
            let c = fixture_credentials(&dir.0);
            let result = query_credentials(
                c,
                Instant::now() + std::time::Duration::from_secs(1),
                |url, _, _| {
                    if url == PROFILE_URL {
                        return Ok(profile());
                    }
                    std::fs::write(dir.0.join("config.json"), b"changed").unwrap();
                    Err(error)
                },
            );
            assert_eq!(result, Err(Error::Changed));
        }
    }

    #[test]
    fn desktop_revision_keeps_same_account_preferences_but_separates_identity() {
        let dir = Temp::new();
        let c = fixture_credentials(&dir.0);
        let mut next = fixture_credentials(&dir.0);
        next.config = Zeroizing::new(b"changed-preferences".to_vec());
        next.token = Zeroizing::new("rotated-fixture".into());
        assert!(c.revision() == next.revision());
        next.account = OTHER.into();
        assert!(c.revision() != next.revision());
        next.account = ACCOUNT.into();
        next.org = OTHER.into();
        assert!(c.revision() != next.revision());
        let bad = credentials_from(&dir.0);
        assert!(matches!(bad, Err(Error::Decode)));
    }
    #[test]
    fn safe_file_read_rejects_large_and_linked_files_and_code_fallback_is_conservative() {
        let dir = Temp::new();
        let path = dir.0.join("credentials.json");
        assert!(code_unavailable(&path));
        std::fs::write(dir.0.join("config.json"), b"{}").unwrap();
        assert!(selected_from(&path, std::slice::from_ref(&dir.0)));
        std::fs::write(
            &path,
            br#"{"claudeAiOauth":{"accessToken":"fixture","expiresAt":1}}"#,
        )
        .unwrap();
        assert!(code_unavailable(&path));
        std::fs::write(&path, br#"{"claudeAiOauth":{"accessToken":"fixture"}}"#).unwrap();
        assert!(!code_unavailable(&path));
        assert!(!selected_from(&path, std::slice::from_ref(&dir.0)));
        std::fs::write(&path, b"not-json").unwrap();
        assert!(!code_unavailable(&path));
        std::fs::write(&path, vec![b' '; FILE_CAP as usize + 1]).unwrap();
        assert!(read_file(&path).is_err());
        assert!(!code_unavailable(&path));
        assert!(read_file(Path::new("relative")).is_err());
        #[cfg(windows)]
        {
            assert!(checked_metadata(Path::new(r"\\invalid-host\share\credential")).is_err());
            let link = dir.0.join("linked");
            if std::os::windows::fs::symlink_file(&path, &link).is_ok() {
                assert!(read_file(&link).is_err());
            }
        }
    }
    #[test]
    #[ignore = "reads the local Claude Desktop login and quota without a model request"]
    fn live_desktop_usage() {
        let status = crate::collect_claude_usage_status(
            &crate::config::Settings::default(),
            "test-pc",
            chrono::Utc::now(),
            true,
            Instant::now() + std::time::Duration::from_secs(10),
            true,
        )
        .unwrap();
        assert_eq!(status.session_id, "claude-desktop-usage");
        assert!(status.primary.is_some() || status.secondary.is_some());
        assert!(!status.approx);
    }
}
