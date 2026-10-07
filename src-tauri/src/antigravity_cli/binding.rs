use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use zeroize::Zeroize;

#[cfg(windows)]
#[path = "bindings_crypto.rs"]
mod bindings_crypto;

// Original settings may contain secrets. Deliberately do not implement Debug.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: String,
    pub managed_command: String,
    pub original_present: bool,
    pub original_status_line: Option<Value>,
    #[cfg(windows)]
    #[serde(skip)]
    verified_previous_ownership: Option<VerifiedPreviousOwnership>,
}

#[cfg(windows)]
struct VerifiedPreviousOwnership {
    command: zeroize::Zeroizing<String>,
    home: std::path::PathBuf,
    data: std::path::PathBuf,
}

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Fields {
            id: String,
            managed_command: String,
            original_present: bool,
            original_status_line: Value,
        }
        let mut fields = Fields::deserialize(deserializer)?;
        if !fields.original_present && !fields.original_status_line.is_null() {
            clear_value(&mut fields.original_status_line);
            return Err(serde::de::Error::custom("invalid binding presence"));
        }
        Ok(Self {
            id: fields.id,
            managed_command: fields.managed_command,
            original_present: fields.original_present,
            original_status_line: fields
                .original_present
                .then_some(fields.original_status_line),
            #[cfg(windows)]
            verified_previous_ownership: None,
        })
    }
}

#[cfg(windows)]
impl Binding {
    pub(super) fn verified_previous_owns_at(&self, value: Option<&Value>, home: &Path) -> bool {
        platform::verified_previous_owns_at(self, value, home)
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        if let Some(value) = self.original_status_line.as_mut() {
            clear_value(value);
        }
    }
}

fn clear_value(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(clear_value),
        Value::Object(values) => {
            for (mut key, mut value) in std::mem::take(values) {
                key.zeroize();
                clear_value(&mut value);
            }
        }
        _ => {}
    }
}

pub fn reconcile(enabled: bool, helper: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let (home, data) = current_paths()?;
        if enabled {
            platform::install(&home, &data, helper, |_| Ok(()))?;
        } else {
            restore_at(&home, &data)?;
        }
    }
    #[cfg(not(windows))]
    let _ = (enabled, helper);
    Ok(())
}

/// A missing settings file is unavailable here; reconcile instead treats it as a no-op.
pub fn install_at(home: &Path, data: &Path, helper: &Path) -> Result<Binding> {
    #[cfg(windows)]
    {
        // The optional operation lets reconcile treat an absent CLI config as a no-op.
        platform::install(home, data, helper, |_| Ok(()))?
            .ok_or_else(|| platform::Failure::Unavailable.into())
    }
    #[cfg(not(windows))]
    {
        let _ = (home, data, helper);
        anyhow::bail!("Antigravity CLI binding unavailable")
    }
}

pub fn restore_at(home: &Path, data: &Path) -> Result<()> {
    #[cfg(windows)]
    platform::restore(home, data, || Ok(()))?;
    #[cfg(not(windows))]
    let _ = (home, data);
    Ok(())
}

pub fn load_owned_at(home: &Path, data: &Path) -> Result<Option<Binding>> {
    #[cfg(windows)]
    {
        platform::load(home, data, true)
    }
    #[cfg(not(windows))]
    {
        let _ = (home, data);
        Ok(None)
    }
}

/// A saved record is not current ownership proof; callers must check current settings.
pub fn load_saved_at(home: &Path, data: &Path) -> Result<Option<Binding>> {
    #[cfg(windows)]
    {
        platform::load(home, data, false)
    }
    #[cfg(not(windows))]
    {
        let _ = (home, data);
        Ok(None)
    }
}

pub fn load_owned() -> Result<Option<Binding>> {
    #[cfg(windows)]
    {
        let (home, data) = current_paths()?;
        load_owned_at(&home, &data)
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

pub fn load_saved() -> Result<Option<Binding>> {
    #[cfg(windows)]
    {
        let (home, data) = current_paths()?;
        load_saved_at(&home, &data)
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

#[cfg(windows)]
fn current_paths() -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let home = dirs::home_dir().ok_or(platform::Failure::Unavailable)?;
    let data = crate::paths::data_dir().ok_or(platform::Failure::Unavailable)?;
    Ok((home, data))
}

#[cfg(windows)]
mod platform {
    use super::{bindings_crypto, clear_value, Binding, VerifiedPreviousOwnership};
    use anyhow::Result;
    use serde::{Deserialize, Serialize};
    use serde_json::{Map, Value};
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File, Metadata, OpenOptions},
        io::{Read, Write},
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
        path::{Component, Path, PathBuf, Prefix},
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        },
        time::{SystemTime, UNIX_EPOCH},
    };
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                GetFileInformationByHandle, GetShortPathNameW, ReplaceFileW,
                BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                REPLACE_FILE_FLAGS,
            },
        },
    };
    use zeroize::Zeroizing;

    const SETTINGS_CAP: usize = 1024 * 1024;
    const RECORD_CAP: usize = 2 * 1024 * 1024;
    const LAUNCHER_CAP: usize = 128 * 1024;
    const RECORD_NAME: &str = "antigravity-cli-binding.dpapi";
    const LAUNCHER_NAME: &str = "ajagy.cmd";
    const LEGACY_LAUNCHER_NAME: &str = "antigravity-cli-statusline.cmd";
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    static GATE: Mutex<()> = Mutex::new(());

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Failure {
        Unavailable,
        Changed,
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            out.write_str(match self {
                Self::Unavailable => "Antigravity CLI binding unavailable",
                Self::Changed => "Antigravity CLI binding changed concurrently",
            })
        }
    }

    impl std::error::Error for Failure {}

    struct Paths {
        settings: PathBuf,
        data: PathBuf,
        record: PathBuf,
        launcher: PathBuf,
        legacy_launcher: PathBuf,
        legacy_command: String,
        path_digest: String,
    }

    impl Paths {
        fn new(home: &Path, data: &Path) -> Result<Self> {
            validate_syntax(home)?;
            validate_syntax(data)?;
            let settings = home.join(".gemini/antigravity-cli/settings.json");
            let launcher = data.join(LAUNCHER_NAME);
            let legacy_launcher = data.join(LEGACY_LAUNCHER_NAME);
            let legacy_command = format!("call \"{}\"", command_path(&legacy_launcher)?);
            let path_digest = digest(
                settings
                    .as_os_str()
                    .encode_wide()
                    .flat_map(u16::to_le_bytes),
            );
            Ok(Self {
                settings,
                data: data.to_path_buf(),
                record: data.join(RECORD_NAME),
                launcher,
                legacy_launcher,
                legacy_command,
                path_digest,
            })
        }

        fn safe_command(&self) -> Result<String> {
            let directory = DirectoryGuard::new(&self.data)?;
            let direct = command_path(&self.launcher)?;
            if safe_command_token(direct) {
                return Ok(format!("call {direct}"));
            }
            let alias = short_directory(&self.data)?;
            let alias_directory = DirectoryGuard::new(&alias)?;
            if directory.identity()? != alias_directory.identity()? {
                return Err(Failure::Unavailable.into());
            }
            let launcher = alias.join(LAUNCHER_NAME);
            let text = command_path(&launcher)?;
            if !safe_command_token(text) {
                return Err(Failure::Unavailable.into());
            }
            Ok(format!("call {text}"))
        }
    }

    fn safe_command_token(text: &str) -> bool {
        text.is_ascii()
            && !text
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
            && !text.contains(['"', '&', '|', '<', '>', '(', ')', '^', '%', '!', '*', '?'])
    }

    fn short_directory(path: &Path) -> Result<PathBuf> {
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let required = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), None) } as usize;
        if required == 0 || required > 32 * 1024 {
            return Err(Failure::Unavailable.into());
        }
        let mut output = vec![0u16; required];
        let written =
            unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut output)) } as usize;
        if written == 0 || written >= output.len() {
            return Err(Failure::Unavailable.into());
        }
        let text = String::from_utf16(&output[..written]).map_err(|_| Failure::Unavailable)?;
        let alias = PathBuf::from(text);
        validate_syntax(&alias)?;
        Ok(alias)
    }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Record {
        version: u32,
        path_digest: String,
        launcher_digest: String,
        #[serde(default)]
        prepared: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_launcher_digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        legacy_launcher_digest: Option<String>,
        binding: Binding,
    }

    struct Saved {
        bytes: Zeroizing<Vec<u8>>,
        record: Record,
    }

    struct Settings {
        bytes: Zeroizing<Vec<u8>>,
        root: Value,
    }

    impl Drop for Settings {
        fn drop(&mut self) {
            clear_value(&mut self.root);
        }
    }

    impl Settings {
        fn status_line(&self) -> Option<&Value> {
            self.root.get("statusLine")
        }
    }

    fn digest(bytes: impl IntoIterator<Item = u8>) -> String {
        let mut hash = Sha256::new();
        for byte in bytes {
            hash.update([byte]);
        }
        format!("{:x}", hash.finalize())
    }

    fn new_id() -> String {
        // Provenance/generation identifier, not an authentication secret.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        digest(
            now.as_nanos()
                .to_le_bytes()
                .into_iter()
                .chain(std::process::id().to_le_bytes())
                .chain(SEQUENCE.fetch_add(1, Ordering::Relaxed).to_le_bytes()),
        )
    }

    fn valid_digest(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn validate_syntax(path: &Path) -> Result<()> {
        if !path.is_absolute()
            || !matches!(path.components().next(), Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(Failure::Unavailable.into());
        }
        for component in path.components() {
            match component {
                Component::ParentDir | Component::CurDir => {
                    return Err(Failure::Unavailable.into());
                }
                Component::Normal(name) => {
                    let name = name.to_str().ok_or(Failure::Unavailable)?;
                    if name.is_empty()
                        || name.contains([':', '"', '\0', '\r', '\n'])
                        || name.ends_with(['.', ' '])
                    {
                        return Err(Failure::Unavailable.into());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn is_reparse(metadata: &Metadata) -> bool {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }

    fn checked_metadata(path: &Path) -> Result<Option<Metadata>> {
        validate_syntax(path)?;
        let mut current = PathBuf::new();
        let mut final_metadata = None;
        for component in path.components() {
            current.push(component);
            if !current.is_absolute() {
                continue;
            }
            let metadata = match fs::symlink_metadata(&current) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(Failure::Unavailable.into()),
            };
            if is_reparse(&metadata) || (current != path && !metadata.is_dir()) {
                return Err(Failure::Unavailable.into());
            }
            final_metadata = Some(metadata);
        }
        Ok(final_metadata)
    }

    fn create_directories(path: &Path) -> Result<()> {
        validate_syntax(path)?;
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component);
            if !current.is_absolute() {
                continue;
            }
            if checked_metadata(&current)?.is_none() {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(Failure::Unavailable.into()),
                }
            }
            let metadata = checked_metadata(&current)?.ok_or(Failure::Unavailable)?;
            if !metadata.is_dir() {
                return Err(Failure::Unavailable.into());
            }
        }
        Ok(())
    }

    // Hold directory handles without DELETE sharing while replacing children.
    struct DirectoryGuard {
        _handles: Vec<File>,
    }

    impl DirectoryGuard {
        fn new(path: &Path) -> Result<Self> {
            checked_metadata(path)?.ok_or(Failure::Unavailable)?;
            let mut current = PathBuf::new();
            let mut handles = Vec::new();
            for component in path.components() {
                current.push(component);
                if !current.is_absolute() {
                    continue;
                }
                let handle = OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                    .open(&current)
                    .map_err(|_| Failure::Unavailable)?;
                let metadata = handle.metadata().map_err(|_| Failure::Unavailable)?;
                if !metadata.is_dir() || is_reparse(&metadata) {
                    return Err(Failure::Unavailable.into());
                }
                handles.push(handle);
            }
            checked_metadata(path)?.ok_or(Failure::Unavailable)?;
            Ok(Self { _handles: handles })
        }

        fn identity(&self) -> Result<(u32, u64)> {
            let file = self._handles.last().ok_or(Failure::Unavailable)?;
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
                .map_err(|_| Failure::Unavailable)?;
            Ok((
                info.dwVolumeSerialNumber,
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ))
        }
    }

    #[derive(PartialEq, Eq)]
    struct FileStamp {
        volume: u32,
        index: u64,
        len: u64,
        modified: Option<SystemTime>,
    }

    fn file_stamp(file: &File, cap: usize) -> Result<FileStamp> {
        let metadata = file.metadata().map_err(|_| Failure::Unavailable)?;
        if !metadata.is_file() || is_reparse(&metadata) || metadata.len() > cap as u64 {
            return Err(Failure::Unavailable.into());
        }
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| Failure::Unavailable)?;
        Ok(FileStamp {
            volume: info.dwVolumeSerialNumber,
            index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    fn open_read(path: &Path) -> Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .map_err(|_| Failure::Unavailable.into())
    }

    fn read_optional(path: &Path, cap: usize) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let Some(metadata) = checked_metadata(path)? else {
            return Ok(None);
        };
        if !metadata.is_file() || metadata.len() > cap as u64 {
            return Err(Failure::Unavailable.into());
        }
        let mut file = open_read(path)?;
        let before = file_stamp(&file, cap)?;
        let mut bytes = Zeroizing::new(Vec::new());
        (&mut file)
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::Unavailable)?;
        checked_metadata(path)?.ok_or(Failure::Changed)?;
        let current = open_read(path)?;
        if bytes.len() as u64 != before.len
            || bytes.len() > cap
            || before != file_stamp(&file, cap)?
            || before != file_stamp(&current, cap)?
        {
            return Err(Failure::Changed.into());
        }
        Ok(Some(bytes))
    }

    fn read_settings(path: &Path) -> Result<Option<Settings>> {
        let Some(bytes) = read_optional(path, SETTINGS_CAP)? else {
            return Ok(None);
        };
        let root: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::Unavailable)?;
        let settings = Settings { bytes, root };
        if !settings.root.is_object() {
            return Err(Failure::Unavailable.into());
        }
        Ok(Some(settings))
    }

    fn command(value: Option<&Value>) -> Option<&str> {
        value?.get("command")?.as_str()
    }

    fn owns_command(value: Option<&Value>, binding: &Binding) -> bool {
        command(value) == Some(binding.managed_command.as_str())
    }

    fn original_matches(value: Option<&Value>, binding: &Binding) -> bool {
        value.is_some() == binding.original_present
            && value == binding.original_status_line.as_ref()
    }

    fn read_saved(paths: &Paths) -> Result<Option<Saved>> {
        checked_metadata(&paths.settings)?;
        let Some(bytes) = read_optional(&paths.record, RECORD_CAP)? else {
            return Ok(None);
        };
        let plaintext = bindings_crypto::unprotect(&bytes)?;
        let record: Record =
            serde_json::from_slice(&plaintext).map_err(|_| Failure::Unavailable)?;
        let binding = &record.binding;
        if !matches!(record.version, 1 | 2)
            || record.path_digest != paths.path_digest
            || !valid_digest(&record.launcher_digest)
            || !valid_digest(&binding.id)
            || binding.original_present != binding.original_status_line.is_some()
            || command(binding.original_status_line.as_ref())
                == Some(binding.managed_command.as_str())
            || command(binding.original_status_line.as_ref()) == Some(paths.legacy_command.as_str())
            || record
                .previous_launcher_digest
                .as_ref()
                .is_some_and(|value| !valid_digest(value))
            || record
                .legacy_launcher_digest
                .as_ref()
                .is_some_and(|value| !valid_digest(value))
            || (!record.prepared
                && (record.previous_launcher_digest.is_some()
                    || record.previous_command.is_some()
                    || record.legacy_launcher_digest.is_some()))
            || record.previous_command.is_some() != record.legacy_launcher_digest.is_some()
        {
            return Err(Failure::Unavailable.into());
        }
        if record.version == 1 {
            if record.prepared || binding.managed_command != paths.legacy_command {
                return Err(Failure::Unavailable.into());
            }
            let launcher =
                read_optional(&paths.legacy_launcher, LAUNCHER_CAP)?.ok_or(Failure::Unavailable)?;
            if digest(launcher.iter().copied()) != record.launcher_digest {
                return Err(Failure::Unavailable.into());
            }
        } else if binding.managed_command != paths.safe_command()? {
            return Err(Failure::Unavailable.into());
        }
        if let Some(previous) = record.previous_command.as_ref() {
            if previous != &paths.legacy_command {
                return Err(Failure::Unavailable.into());
            }
            let legacy =
                read_optional(&paths.legacy_launcher, LAUNCHER_CAP)?.ok_or(Failure::Unavailable)?;
            if Some(digest(legacy.iter().copied())) != record.legacy_launcher_digest {
                return Err(Failure::Unavailable.into());
            }
        }
        Ok(Some(Saved { bytes, record }))
    }

    fn previous_owns(value: Option<&Value>, record: &Record) -> bool {
        record.prepared
            && record
                .previous_command
                .as_deref()
                .is_some_and(|previous| command(value) == Some(previous))
    }

    pub(super) fn verified_previous_owns_at(
        binding: &Binding,
        value: Option<&Value>,
        home: &Path,
    ) -> bool {
        let Some(previous) = binding.verified_previous_ownership.as_ref() else {
            return false;
        };
        if previous.home != home || command(value) != Some(previous.command.as_str()) {
            return false;
        }
        let Ok(_gate) = GATE.lock() else {
            return false;
        };
        let Ok(paths) = Paths::new(home, &previous.data) else {
            return false;
        };
        // Revalidate the prepared journal and legacy launcher after the initial load.
        let Ok(Some(saved)) = read_saved(&paths) else {
            return false;
        };
        let current = &saved.record.binding;
        current.id == binding.id
            && current.managed_command == binding.managed_command
            && current.original_present == binding.original_present
            && current.original_status_line == binding.original_status_line
            && saved.record.previous_command.as_deref() == Some(previous.command.as_str())
            && previous_owns(value, &saved.record)
    }

    fn launcher_matches(bytes: Option<&[u8]>, record: &Record) -> bool {
        match bytes {
            Some(bytes) => {
                let hash = digest(bytes.iter().copied());
                hash == record.launcher_digest
                    || (record.prepared
                        && record.previous_launcher_digest.as_deref() == Some(hash.as_str()))
            }
            None => !record.prepared || record.previous_launcher_digest.is_none(),
        }
    }

    fn encrypt_record(record: &Record) -> Result<Zeroizing<Vec<u8>>> {
        let plaintext =
            Zeroizing::new(serde_json::to_vec(record).map_err(|_| Failure::Unavailable)?);
        bindings_crypto::protect(&plaintext)
    }

    fn clear_prepared(record: &mut Record) {
        record.prepared = false;
        record.previous_launcher_digest = None;
        record.previous_command = None;
        record.legacy_launcher_digest = None;
    }

    fn command_path(path: &Path) -> Result<&str> {
        validate_syntax(path)?;
        let text = path.to_str().ok_or(Failure::Unavailable)?;
        if text.len() > 32 * 1024 {
            return Err(Failure::Unavailable.into());
        }
        Ok(text)
    }

    fn launcher_bytes(helper: &Path, id: &str) -> Result<Vec<u8>> {
        let path = command_path(helper)?;
        let alias;
        let helper = if path.is_ascii() {
            path
        } else {
            alias = short_directory(helper)?;
            let text = command_path(&alias)?;
            if !text.is_ascii() {
                return Err(Failure::Unavailable.into());
            }
            let original = open_read(helper)?;
            let resolved = open_read(&alias)?;
            if file_stamp(&original, usize::MAX)? != file_stamp(&resolved, usize::MAX)? {
                return Err(Failure::Unavailable.into());
            }
            text
        };
        let helper = helper.replace('%', "%%");
        if !valid_digest(id) {
            return Err(Failure::Unavailable.into());
        }
        // ASCII paths avoid changing the parent CLI's shared console code page.
        let text = format!(
            "@echo off\r\nsetlocal DisableDelayedExpansion\r\n\
             rem agent-juice-antigravity-cli {id}\r\n\
             \"{helper}\" --antigravity-cli\r\n\
             exit /b %errorlevel%\r\n"
        );
        if text.len() > LAUNCHER_CAP {
            return Err(Failure::Unavailable.into());
        }
        Ok(text.into_bytes())
    }

    fn managed_status_line(binding: &Binding) -> Value {
        let original = binding.original_status_line.as_ref();
        let mut fields = original
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(Map::new);
        let disabled = original.and_then(|value| value.get("enabled")) == Some(&Value::Bool(false));
        let custom = command(original).is_some_and(|command| !command.trim().is_empty());
        let stack = !disabled
            && if custom {
                original
                    .and_then(|value| value.get("stack_with_default"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            } else {
                true
            };
        fields.insert("type".into(), Value::String("command".into()));
        fields.insert(
            "command".into(),
            Value::String(binding.managed_command.clone()),
        );
        fields.insert("enabled".into(), Value::Bool(true));
        fields.insert("stack_with_default".into(), Value::Bool(stack));
        Value::Object(fields)
    }

    struct Temporary(PathBuf);

    impl Drop for Temporary {
        fn drop(&mut self) {
            if checked_metadata(&self.0).is_ok() {
                let _ = fs::remove_file(&self.0);
            }
        }
    }

    fn atomic_replace(
        path: &Path,
        expected: Option<&[u8]>,
        contents: &[u8],
        cap: usize,
    ) -> Result<()> {
        if contents.len() > cap {
            return Err(Failure::Unavailable.into());
        }
        let parent = path.parent().ok_or(Failure::Unavailable)?;
        let _guard = DirectoryGuard::new(parent)?;
        if read_optional(path, cap)?.as_deref().map(Vec::as_slice) != expected {
            return Err(Failure::Changed.into());
        }
        let temporary = Temporary(parent.join(format!(".antigravity-cli.{}.tmp", new_id())));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&temporary.0)
            .map_err(|_| Failure::Unavailable)?;
        file.write_all(contents).map_err(|_| Failure::Unavailable)?;
        file.sync_all().map_err(|_| Failure::Unavailable)?;
        drop(file);
        checked_metadata(&temporary.0)?.ok_or(Failure::Unavailable)?;
        if read_optional(path, cap)?.as_deref().map(Vec::as_slice) != expected {
            return Err(Failure::Changed.into());
        }
        if expected.is_some() {
            let target: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let source: Vec<_> = temporary
                .0
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            unsafe {
                ReplaceFileW(
                    PCWSTR(target.as_ptr()),
                    PCWSTR(source.as_ptr()),
                    PCWSTR::null(),
                    REPLACE_FILE_FLAGS(0),
                    None,
                    None,
                )
            }
            .map_err(|_| Failure::Unavailable)?;
        } else {
            fs::rename(&temporary.0, path).map_err(|_| Failure::Unavailable)?;
        }
        Ok(())
    }

    fn update_settings(
        paths: &Paths,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> Result<()> {
        // Re-read/merge unrelated root edits, but never adopt a changed statusLine.
        for _ in 0..3 {
            let mut latest = read_settings(&paths.settings)?.ok_or(Failure::Changed)?;
            if latest.status_line() != expected {
                return Err(Failure::Changed.into());
            }
            let root = latest.root.as_object_mut().ok_or(Failure::Unavailable)?;
            if let Some(value) = replacement {
                root.insert("statusLine".into(), value.clone());
            } else {
                root.remove("statusLine");
            }
            let bytes = Zeroizing::new(
                serde_json::to_vec_pretty(&latest.root).map_err(|_| Failure::Unavailable)?,
            );
            match atomic_replace(&paths.settings, Some(&latest.bytes), &bytes, SETTINGS_CAP) {
                Err(error) if error.downcast_ref::<Failure>() == Some(&Failure::Changed) => {
                    continue
                }
                result => return result,
            }
        }
        Err(Failure::Changed.into())
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum InstallStage {
        MetadataSaved,
        LauncherSaved,
        BeforeSettings,
    }

    fn rollback_artifact(
        path: &Path,
        attempted: &[u8],
        previous: Option<&[u8]>,
        cap: usize,
    ) -> Result<()> {
        let _guard = DirectoryGuard::new(path.parent().ok_or(Failure::Unavailable)?)?;
        let current = read_optional(path, cap)?;
        if current.as_deref().map(Vec::as_slice) == previous {
            return Ok(());
        }
        if current.as_deref().map(Vec::as_slice) != Some(attempted) {
            return Err(Failure::Changed.into());
        }
        if let Some(previous) = previous {
            atomic_replace(path, Some(attempted), previous, cap)?;
        } else {
            fs::remove_file(path).map_err(|_| Failure::Unavailable)?;
        }
        if read_optional(path, cap)?.as_deref().map(Vec::as_slice) != previous {
            return Err(Failure::Changed.into());
        }
        Ok(())
    }

    pub(super) fn install(
        home: &Path,
        data: &Path,
        helper: &Path,
        mut hook: impl FnMut(InstallStage) -> Result<()>,
    ) -> Result<Option<Binding>> {
        let _gate = GATE.lock().map_err(|_| Failure::Unavailable)?;
        let paths = Paths::new(home, data)?;
        let Some(initial) = read_settings(&paths.settings)? else {
            return Ok(None);
        };
        let saved = read_saved(&paths)?;
        let old_launcher = read_optional(&paths.launcher, LAUNCHER_CAP)?;
        let legacy_launcher = read_optional(&paths.legacy_launcher, LAUNCHER_CAP)?;
        if let Some(saved) = saved.as_ref() {
            let original = &saved.record.binding;
            if !owns_command(initial.status_line(), original)
                && !original_matches(initial.status_line(), original)
                && !previous_owns(initial.status_line(), &saved.record)
            {
                return Err(Failure::Changed.into());
            }
            if saved.record.version == 1 && old_launcher.is_some() {
                return Err(Failure::Changed.into());
            }
        } else if old_launcher.is_some()
            || legacy_launcher.is_some()
            || command(initial.status_line()) == Some(paths.legacy_command.as_str())
        {
            return Err(Failure::Unavailable.into());
        }
        command_path(helper)?;
        let helper_metadata = checked_metadata(helper)?.ok_or(Failure::Unavailable)?;
        if !helper_metadata.is_file() {
            return Err(Failure::Unavailable.into());
        }
        // Only create the checked data directory after validating existing settings/records.
        create_directories(&paths.data)?;
        let managed_command = paths.safe_command()?;
        let binding = match saved.as_ref() {
            Some(saved) => {
                let original = &saved.record.binding;
                let owned = owns_command(initial.status_line(), original);
                if !owned
                    && !original_matches(initial.status_line(), original)
                    && !previous_owns(initial.status_line(), &saved.record)
                {
                    return Err(Failure::Changed.into());
                }
                if saved.record.version == 2
                    && !launcher_matches(
                        old_launcher.as_ref().map(|bytes| bytes.as_slice()),
                        &saved.record,
                    )
                {
                    return Err(Failure::Changed.into());
                }
                Binding {
                    id: if saved.record.version == 2 && (owned || saved.record.prepared) {
                        original.id.clone()
                    } else {
                        new_id()
                    },
                    managed_command: managed_command.clone(),
                    original_present: original.original_present,
                    original_status_line: original.original_status_line.clone(),
                    verified_previous_ownership: None,
                }
            }
            None => {
                if command(initial.status_line()) == Some(managed_command.as_str()) {
                    return Err(Failure::Unavailable.into());
                }
                Binding {
                    id: new_id(),
                    managed_command,
                    original_present: initial.status_line().is_some(),
                    original_status_line: initial.status_line().cloned(),
                    verified_previous_ownership: None,
                }
            }
        };
        let launcher = launcher_bytes(helper, &binding.id)?;
        let mut record = Record {
            version: 2,
            path_digest: paths.path_digest.clone(),
            launcher_digest: digest(launcher.iter().copied()),
            prepared: true,
            previous_launcher_digest: old_launcher
                .as_ref()
                .map(|bytes| digest(bytes.iter().copied())),
            previous_command: saved.as_ref().and_then(|saved| {
                if saved.record.version == 1 {
                    Some(paths.legacy_command.clone())
                } else {
                    saved.record.previous_command.clone()
                }
            }),
            legacy_launcher_digest: saved.as_ref().and_then(|saved| {
                if saved.record.version == 1 {
                    Some(saved.record.launcher_digest.clone())
                } else {
                    saved.record.legacy_launcher_digest.clone()
                }
            }),
            binding,
        };
        let managed = managed_status_line(&record.binding);
        let encrypted = encrypt_record(&record)?;
        let old_record = saved.as_ref().map(|saved| saved.bytes.as_slice());
        let previous_launcher = old_launcher.as_ref().map(|bytes| bytes.as_slice());
        let operation = (|| {
            if record.previous_command.is_some()
                && read_optional(&paths.legacy_launcher, LAUNCHER_CAP)?
                    .as_deref()
                    .map(Vec::as_slice)
                    != legacy_launcher.as_deref().map(Vec::as_slice)
            {
                return Err(Failure::Changed.into());
            }
            atomic_replace(&paths.record, old_record, &encrypted, RECORD_CAP)?;
            hook(InstallStage::MetadataSaved)?;
            atomic_replace(&paths.launcher, previous_launcher, &launcher, LAUNCHER_CAP)?;
            hook(InstallStage::LauncherSaved)?;
            hook(InstallStage::BeforeSettings)?;
            update_settings(&paths, initial.status_line(), Some(&managed))
        })();
        if let Err(error) = operation {
            // Retain recovery data if an uncertain write may have installed the wrapper.
            let can_rollback =
                read_settings(&paths.settings)
                    .ok()
                    .flatten()
                    .is_some_and(|current| {
                        !owns_command(current.status_line(), &record.binding)
                            || saved.as_ref().is_some_and(|saved| {
                                owns_command(current.status_line(), &saved.record.binding)
                            })
                    });
            if can_rollback {
                // Keep the prepared journal until the launcher is confirmed restored.
                if rollback_artifact(&paths.launcher, &launcher, previous_launcher, LAUNCHER_CAP)
                    .is_ok()
                {
                    let _ = rollback_artifact(&paths.record, &encrypted, old_record, RECORD_CAP);
                }
            }
            return Err(error);
        }
        // An uncleared preparation is fully recoverable if final journal cleanup fails.
        clear_prepared(&mut record);
        if let Ok(finalized) = encrypt_record(&record) {
            let _ = atomic_replace(&paths.record, Some(&encrypted), &finalized, RECORD_CAP);
        }
        Ok(Some(record.binding))
    }

    pub(super) fn restore(
        home: &Path,
        data: &Path,
        before_settings: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let _gate = GATE.lock().map_err(|_| Failure::Unavailable)?;
        let paths = Paths::new(home, data)?;
        let Some(mut saved) = read_saved(&paths)? else {
            return Ok(());
        };
        let Some(initial) = read_settings(&paths.settings)? else {
            return Ok(());
        };
        let owned = owns_command(initial.status_line(), &saved.record.binding)
            || previous_owns(initial.status_line(), &saved.record);
        let original = original_matches(initial.status_line(), &saved.record.binding);
        if !owned && !original {
            return Ok(());
        }
        if owned {
            before_settings()?;
            update_settings(
                &paths,
                initial.status_line(),
                saved.record.binding.original_status_line.as_ref(),
            )?;
        }
        if saved.record.prepared {
            let launcher = read_optional(&paths.launcher, LAUNCHER_CAP)?;
            if launcher_matches(
                launcher.as_ref().map(|bytes| bytes.as_slice()),
                &saved.record,
            ) {
                if let Some(launcher) = launcher {
                    saved.record.launcher_digest = digest(launcher.iter().copied());
                }
            }
            clear_prepared(&mut saved.record);
            let encrypted = encrypt_record(&saved.record)?;
            atomic_replace(&paths.record, Some(&saved.bytes), &encrypted, RECORD_CAP)?;
        }
        // Keep the encrypted record and launcher for stale in-memory CLI commands.
        Ok(())
    }

    pub(super) fn load(home: &Path, data: &Path, require_owned: bool) -> Result<Option<Binding>> {
        let _gate = GATE.lock().map_err(|_| Failure::Unavailable)?;
        let paths = Paths::new(home, data)?;
        let Some(saved) = read_saved(&paths)? else {
            return Ok(None);
        };
        if require_owned {
            let Some(settings) = read_settings(&paths.settings)? else {
                return Ok(None);
            };
            let active = settings
                .status_line()
                .and_then(|value| value.get("enabled"))
                .is_none_or(|value| value == &Value::Bool(true));
            if !owns_command(settings.status_line(), &saved.record.binding) || !active {
                return Ok(None);
            }
        }
        let verified_previous_ownership = saved
            .record
            .previous_command
            .as_ref()
            .filter(|_| !require_owned && saved.record.prepared)
            .map(|command| VerifiedPreviousOwnership {
                command: Zeroizing::new(command.clone()),
                home: home.to_path_buf(),
                data: data.to_path_buf(),
            });
        let mut binding = saved.record.binding;
        binding.verified_previous_ownership = verified_previous_ownership;
        Ok(Some(binding))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::antigravity_cli::binding::{
            install_at, load_owned_at, load_saved_at, restore_at,
        };
        use serde_json::json;

        struct Fixture {
            root: PathBuf,
            home: PathBuf,
            data: PathBuf,
            helper: PathBuf,
        }

        impl Fixture {
            fn new(root: Value) -> Self {
                let path = std::env::temp_dir().join(format!("agent-juice-binding-{}", new_id()));
                let fixture = Self {
                    home: path.join("home"),
                    data: path.join("data"),
                    helper: path.join("program/agent-juice.exe"),
                    root: path,
                };
                fs::create_dir_all(fixture.settings_path().parent().unwrap()).unwrap();
                fs::create_dir_all(fixture.helper.parent().unwrap()).unwrap();
                fs::write(&fixture.helper, b"fixture only; never executed").unwrap();
                fixture.write_settings(&root);
                fixture
            }

            fn paths(&self) -> Paths {
                Paths::new(&self.home, &self.data).unwrap()
            }

            fn settings_path(&self) -> PathBuf {
                self.home.join(".gemini/antigravity-cli/settings.json")
            }

            fn write_settings(&self, root: &Value) {
                fs::write(self.settings_path(), serde_json::to_vec(root).unwrap()).unwrap();
            }

            fn settings(&self) -> Value {
                serde_json::from_slice(&fs::read(self.settings_path()).unwrap()).unwrap()
            }

            fn install(&self) -> Binding {
                install_at(&self.home, &self.data, &self.helper).unwrap()
            }

            fn saved(&self) -> Binding {
                load_saved_at(&self.home, &self.data).unwrap().unwrap()
            }

            fn rewrite_record(&self, change: impl FnOnce(&mut Value)) {
                let paths = self.paths();
                let bytes = fs::read(&paths.record).unwrap();
                let plain = bindings_crypto::unprotect(&bytes).unwrap();
                let mut record: Value = serde_json::from_slice(&plain).unwrap();
                change(&mut record);
                let plain = Zeroizing::new(serde_json::to_vec(&record).unwrap());
                let encrypted = bindings_crypto::protect(&plain).unwrap();
                clear_value(&mut record);
                fs::write(paths.record, &encrypted).unwrap();
            }

            fn legacy_install(&self, keep_original: bool) -> Binding {
                let paths = self.paths();
                let root = self.settings();
                let binding = Binding {
                    id: new_id(),
                    managed_command: paths.legacy_command.clone(),
                    original_present: root.get("statusLine").is_some(),
                    original_status_line: root.get("statusLine").cloned(),
                    verified_previous_ownership: None,
                };
                let launcher = launcher_bytes(&self.helper, &binding.id).unwrap();
                create_directories(&self.data).unwrap();
                fs::write(&paths.legacy_launcher, &launcher).unwrap();
                let record = Record {
                    version: 1,
                    path_digest: paths.path_digest,
                    launcher_digest: digest(launcher.iter().copied()),
                    prepared: false,
                    previous_launcher_digest: None,
                    previous_command: None,
                    legacy_launcher_digest: None,
                    binding,
                };
                fs::write(&paths.record, encrypt_record(&record).unwrap().as_slice()).unwrap();
                if !keep_original {
                    let mut root = root;
                    root["statusLine"] = managed_status_line(&record.binding);
                    self.write_settings(&root);
                }
                record.binding
            }

            fn interrupt(&self, stage: u8) -> Binding {
                let paths = self.paths();
                let saved = read_saved(&paths).unwrap().unwrap();
                let old = read_optional(&paths.launcher, LAUNCHER_CAP).unwrap();
                let original = &saved.record.binding;
                let root = self.settings();
                let binding = Binding {
                    id: if saved.record.version == 2
                        && owns_command(root.get("statusLine"), original)
                    {
                        original.id.clone()
                    } else {
                        new_id()
                    },
                    managed_command: paths.safe_command().unwrap(),
                    original_present: original.original_present,
                    original_status_line: original.original_status_line.clone(),
                    verified_previous_ownership: None,
                };
                let launcher = launcher_bytes(&self.helper, &binding.id).unwrap();
                let record = Record {
                    version: 2,
                    path_digest: paths.path_digest,
                    launcher_digest: digest(launcher.iter().copied()),
                    prepared: true,
                    previous_launcher_digest: old
                        .as_ref()
                        .map(|bytes| digest(bytes.iter().copied())),
                    previous_command: (saved.record.version == 1).then_some(paths.legacy_command),
                    legacy_launcher_digest: (saved.record.version == 1)
                        .then_some(saved.record.launcher_digest),
                    binding,
                };
                // Persist interrupted phases without entering the operation's error rollback.
                fs::write(&paths.record, encrypt_record(&record).unwrap().as_slice()).unwrap();
                if stage >= 1 {
                    fs::write(&paths.launcher, &launcher).unwrap();
                }
                if stage >= 2 {
                    let mut root = root;
                    root["statusLine"] = managed_status_line(&record.binding);
                    self.write_settings(&root);
                }
                record.binding
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                assert!(self.root.is_absolute() && self.root.starts_with(std::env::temp_dir()));
                assert!(self
                    .root
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("agent-juice-binding-"));
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        fn custom() -> Value {
            json!({
                "type": "command", "command": "echo fixture-private-original",
                "padding": 3, "stack_with_default": true,
                "unknown": {"nested": [false, "fixture-private-body"]}
            })
        }

        fn failed(error: anyhow::Error) {
            assert!(error.to_string().starts_with("Antigravity CLI binding"));
            assert!(!error.to_string().contains("fixture-private"));
        }

        fn fail_at(stage: InstallStage) -> impl FnMut(InstallStage) -> Result<()> {
            move |current| {
                if current == stage {
                    Err(Failure::Unavailable.into())
                } else {
                    Ok(())
                }
            }
        }

        #[test]
        fn absent_original_preserves_builtin_and_restore_keeps_stale_forwarding_record() {
            let fixture = Fixture::new(json!({"theme": "fixture"}));
            let binding = fixture.install();
            assert!(!binding.original_present);
            assert!(binding.original_status_line.is_none());
            let root = fixture.settings();
            assert_eq!(root["statusLine"]["stack_with_default"], true);
            assert_eq!(root["statusLine"]["enabled"], true);
            assert_eq!(root["statusLine"]["type"], "command");
            assert_eq!(root["statusLine"]["command"], binding.managed_command);
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(fixture.settings(), json!({"theme": "fixture"}));
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            assert_eq!(fixture.saved().id, binding.id);
            assert!(fixture.paths().launcher.is_file());
        }

        #[test]
        fn custom_subtree_flags_and_unknown_body_round_trip_without_public_backup() {
            let original = custom();
            let fixture = Fixture::new(json!({"statusLine": original, "root": [1, 2]}));
            let binding = fixture.install();
            assert_eq!(binding.original_status_line.as_ref(), Some(&original));
            let managed = fixture.settings();
            assert_eq!(managed["statusLine"]["padding"], 3);
            assert_eq!(managed["statusLine"]["stack_with_default"], true);
            assert_eq!(managed["statusLine"]["unknown"], original["unknown"]);
            let paths = fixture.paths();
            let encrypted = fs::read(&paths.record).unwrap();
            let launcher = fs::read(&paths.launcher).unwrap();
            for private in [
                b"fixture-private-original".as_slice(),
                b"fixture-private-body".as_slice(),
            ] {
                assert!(!encrypted
                    .windows(private.len())
                    .any(|bytes| bytes == private));
                assert!(!launcher
                    .windows(private.len())
                    .any(|bytes| bytes == private));
            }
            assert_eq!(fs::read_dir(&fixture.data).unwrap().count(), 2);
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(
                fixture.settings(),
                json!({"statusLine": original, "root": [1, 2]})
            );
        }

        #[test]
        fn disabled_empty_null_and_unknown_subtrees_preserve_exact_original() {
            for original in [
                json!({"command": "echo fixture", "enabled": false, "stack_with_default": true}),
                json!({"enabled": false, "command": "", "padding": 4}),
                json!({"command": "", "unknown": 7}),
                json!({"command": "   "}),
                json!({}),
                Value::Null,
                json!("unknown body"),
                json!(["unknown body"]),
            ] {
                let fixture = Fixture::new(json!({"statusLine": original}));
                let binding = fixture.install();
                assert!(binding.original_present);
                assert_eq!(binding.original_status_line.as_ref(), Some(&original));
                assert_eq!(
                    fixture.saved().original_status_line.as_ref(),
                    Some(&original)
                );
                let hidden = original.get("enabled") == Some(&Value::Bool(false));
                assert_eq!(
                    fixture.settings()["statusLine"]["stack_with_default"],
                    !hidden
                );
                assert_eq!(fixture.settings()["statusLine"]["enabled"], true);
                restore_at(&fixture.home, &fixture.data).unwrap();
                assert_eq!(fixture.settings()["statusLine"], original);
            }
        }

        #[test]
        fn custom_stack_uses_original_boolean_and_default_false() {
            for (flag, stack) in [(None, false), (Some(false), false), (Some(true), true)] {
                let mut original = json!({"command": "echo fixture"});
                if let Some(flag) = flag {
                    original["stack_with_default"] = json!(flag);
                }
                let fixture = Fixture::new(json!({"statusLine": original}));
                fixture.install();
                assert_eq!(
                    fixture.settings()["statusLine"]["stack_with_default"],
                    stack
                );
                restore_at(&fixture.home, &fixture.data).unwrap();
                assert_eq!(fixture.settings()["statusLine"], original);
            }
        }

        #[test]
        fn serializer_normalized_defaults_do_not_break_command_ownership() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            let binding = fixture.install();
            for normalized in [
                json!({"command": binding.managed_command}),
                json!({"type": "", "padding": "", "command": binding.managed_command}),
                json!({"type": "command", "enabled": true, "command": binding.managed_command}),
            ] {
                fixture.write_settings(&json!({"statusLine": normalized, "root": "latest"}));
                assert_eq!(
                    load_owned_at(&fixture.home, &fixture.data)
                        .unwrap()
                        .unwrap()
                        .id,
                    binding.id
                );
            }
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(
                fixture.settings(),
                json!({"statusLine": custom(), "root": "latest"})
            );
        }

        #[test]
        fn user_command_changes_are_never_overwritten_removed_or_rebacked_up() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            let binding = fixture.install();
            for command in [
                "echo user changed".to_owned(),
                format!(" {}", binding.managed_command),
            ] {
                let user = json!({"statusLine": {"command": command, "enabled": true}, "root": 9});
                fixture.write_settings(&user);
                assert!(load_owned_at(&fixture.home, &fixture.data)
                    .unwrap()
                    .is_none());
                restore_at(&fixture.home, &fixture.data).unwrap();
                assert_eq!(fixture.settings(), user);
                assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
                assert_eq!(fixture.settings(), user);
                assert_eq!(fixture.saved().original_status_line, Some(custom()));
            }
        }

        #[test]
        fn saved_record_does_not_claim_active_ownership_after_disabled_or_missing_config() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            let binding = fixture.install();
            let mut root = fixture.settings();
            root["statusLine"]["enabled"] = json!(false);
            fixture.write_settings(&root);
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            assert_eq!(fixture.saved().id, binding.id);
            let reenabled = fixture.install();
            assert_eq!(reenabled.id, binding.id);
            assert_eq!(reenabled.original_status_line, Some(custom()));
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_some());
            fs::remove_file(fixture.settings_path()).unwrap();
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            assert_eq!(fixture.saved().id, binding.id);
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert!(!fixture.settings_path().exists());
        }

        #[test]
        fn invalid_enabled_values_do_not_claim_active_ownership() {
            let fixture = Fixture::new(json!({}));
            let binding = fixture.install();
            for value in [json!(false), json!("false"), Value::Null, json!(0)] {
                fixture.write_settings(&json!({"statusLine": {
                    "command": binding.managed_command, "enabled": value
                }}));
                assert!(load_owned_at(&fixture.home, &fixture.data)
                    .unwrap()
                    .is_none());
                assert_eq!(fixture.saved().id, binding.id);
            }
        }

        #[test]
        fn update_reuses_original_and_restore_reinstall_renews_generation() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            let first = fixture.install();
            let updated_helper = fixture.root.join("new program/agent-juice.exe");
            fs::create_dir_all(updated_helper.parent().unwrap()).unwrap();
            fs::write(&updated_helper, b"not executed").unwrap();
            let update = install_at(&fixture.home, &fixture.data, &updated_helper).unwrap();
            assert_eq!(update.id, first.id);
            assert_eq!(update.original_status_line, Some(custom()));
            assert!(
                String::from_utf8(fs::read(fixture.paths().launcher).unwrap())
                    .unwrap()
                    .contains(updated_helper.to_str().unwrap())
            );
            restore_at(&fixture.home, &fixture.data).unwrap();
            let again = fixture.install();
            assert_ne!(again.id, first.id);
            assert_eq!(again.original_status_line, Some(custom()));
            assert_eq!(fixture.saved().id, again.id);
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(fixture.settings()["statusLine"], custom());
        }

        #[test]
        fn install_latest_read_preserves_unrelated_root_changes() {
            let fixture = Fixture::new(json!({"statusLine": custom(), "root": "old"}));
            install(&fixture.home, &fixture.data, &fixture.helper, |stage| {
                if stage == InstallStage::BeforeSettings {
                    fixture.write_settings(
                        &json!({"statusLine": custom(), "root": "new", "added": [3]}),
                    );
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(fixture.settings()["root"], "new");
            assert_eq!(fixture.settings()["added"], json!([3]));
            assert_eq!(fixture.saved().original_status_line, Some(custom()));
        }

        #[test]
        fn restore_latest_read_preserves_unrelated_root_changes() {
            let fixture = Fixture::new(json!({"statusLine": custom(), "root": "old"}));
            fixture.install();
            restore(&fixture.home, &fixture.data, || {
                let mut latest = fixture.settings();
                latest["root"] = json!("new");
                latest["added"] = json!({"nested": true});
                fixture.write_settings(&latest);
                Ok(())
            })
            .unwrap();
            assert_eq!(
                fixture.settings(),
                json!({
                    "statusLine": custom(), "root": "new", "added": {"nested": true}
                })
            );
        }

        #[test]
        fn concurrent_install_command_edit_is_preserved_and_new_artifacts_roll_back() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            let user = json!({"statusLine": {"command": "echo raced user"}, "root": true});
            let result = install(&fixture.home, &fixture.data, &fixture.helper, |stage| {
                if stage == InstallStage::BeforeSettings {
                    fixture.write_settings(&user);
                }
                Ok(())
            });
            failed(result.err().unwrap());
            assert_eq!(fixture.settings(), user);
            assert!(!fixture.paths().record.exists());
            assert!(!fixture.paths().launcher.exists());
        }

        #[test]
        fn concurrent_install_subtree_flag_edit_is_not_adopted() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            let mut user = fixture.settings();
            user["statusLine"]["padding"] = json!(8);
            let result = install(&fixture.home, &fixture.data, &fixture.helper, |stage| {
                if stage == InstallStage::BeforeSettings {
                    fixture.write_settings(&user);
                }
                Ok(())
            });
            failed(result.err().unwrap());
            assert_eq!(fixture.settings(), user);
            assert!(!fixture.paths().record.exists());
        }

        #[test]
        fn concurrent_restore_command_or_flag_edits_are_not_overwritten() {
            for command_changed in [false, true] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                let binding = fixture.install();
                let mut user = fixture.settings();
                if command_changed {
                    user["statusLine"]["command"] = json!("echo raced user");
                } else {
                    user["statusLine"]["padding"] = json!(9);
                }
                failed(
                    restore(&fixture.home, &fixture.data, || {
                        fixture.write_settings(&user);
                        Ok(())
                    })
                    .unwrap_err(),
                );
                assert_eq!(fixture.settings(), user);
                assert_eq!(fixture.saved().id, binding.id);
            }
        }

        #[test]
        fn failed_new_install_rolls_back_only_its_encrypted_and_launcher_artifacts() {
            for stage in [
                InstallStage::MetadataSaved,
                InstallStage::LauncherSaved,
                InstallStage::BeforeSettings,
            ] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                let settings = fs::read(fixture.settings_path()).unwrap();
                let result = install(
                    &fixture.home,
                    &fixture.data,
                    &fixture.helper,
                    fail_at(stage),
                );
                failed(result.err().unwrap());
                assert_eq!(fs::read(fixture.settings_path()).unwrap(), settings);
                assert!(!fixture.paths().record.exists());
                assert!(!fixture.paths().launcher.exists());
                assert_eq!(fs::read_dir(&fixture.data).unwrap().count(), 0);
            }
        }

        #[test]
        fn failed_update_restores_previous_ciphertext_launcher_and_original() {
            for stage in [
                InstallStage::MetadataSaved,
                InstallStage::LauncherSaved,
                InstallStage::BeforeSettings,
            ] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                let binding = fixture.install();
                let paths = fixture.paths();
                let settings = fs::read(&paths.settings).unwrap();
                let record = fs::read(&paths.record).unwrap();
                let launcher = fs::read(&paths.launcher).unwrap();
                let other = fixture.root.join("other.exe");
                fs::write(&other, b"not executed").unwrap();
                let result = install(&fixture.home, &fixture.data, &other, fail_at(stage));
                failed(result.err().unwrap());
                assert_eq!(fs::read(&paths.settings).unwrap(), settings);
                assert_eq!(fs::read(&paths.record).unwrap(), record);
                assert_eq!(fs::read(&paths.launcher).unwrap(), launcher);
                assert_eq!(fixture.saved().id, binding.id);
                assert_eq!(fixture.saved().original_status_line, Some(custom()));
            }
        }

        fn retry_after_locked_launcher_rollback(update: bool, exclusive_launcher: bool) {
            let fixture = Fixture::new(json!({"statusLine": custom(), "root": 7}));
            if update {
                fixture.install();
            }
            let paths = fixture.paths();
            let settings = fs::read(&paths.settings).unwrap();
            let old_record = read_optional(&paths.record, RECORD_CAP).unwrap();
            let other = fixture.root.join("updated-helper.exe");
            fs::write(&other, b"synthetic helper; never executed").unwrap();
            let mut locks = Vec::new();
            let result = install(&fixture.home, &fixture.data, &other, |stage| {
                if stage == InstallStage::BeforeSettings {
                    // Reads remain possible, but Windows rejects replacement/deletion.
                    for path in [&paths.settings, &paths.launcher] {
                        let share = if exclusive_launcher && path == &paths.launcher {
                            0
                        } else {
                            FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0
                        };
                        locks.push(
                            OpenOptions::new()
                                .read(true)
                                .share_mode(share)
                                .open(path)
                                .unwrap(),
                        );
                    }
                }
                Ok(())
            });
            failed(result.err().unwrap());
            assert_eq!(fs::read(&paths.settings).unwrap(), settings);
            drop(locks);
            let journal = read_saved(&paths)
                .unwrap()
                .expect("retain recovery journal");
            assert!(journal.record.prepared);
            assert_ne!(
                Some(journal.bytes.as_slice()),
                old_record.as_deref().map(Vec::as_slice)
            );
            let pending_id = journal.record.binding.id.clone();
            let resumed = install_at(&fixture.home, &fixture.data, &other).unwrap();
            assert_eq!(resumed.id, pending_id);
            assert_eq!(resumed.original_status_line, Some(custom()));
            assert!(!read_saved(&paths).unwrap().unwrap().record.prepared);
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_some());
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(
                fixture.settings(),
                json!({"statusLine": custom(), "root": 7})
            );
        }

        #[test]
        fn new_install_retries_after_locked_launcher_rollback() {
            retry_after_locked_launcher_rollback(false, false);
        }

        #[test]
        fn helper_update_retries_after_locked_launcher_rollback() {
            retry_after_locked_launcher_rollback(true, false);
        }

        #[test]
        fn unreadable_launcher_rollback_keeps_journal_for_retry() {
            for update in [false, true] {
                retry_after_locked_launcher_rollback(update, true);
            }
        }

        #[test]
        fn rollback_does_not_delete_concurrently_replaced_launcher() {
            let fixture = Fixture::new(json!({}));
            let paths = fixture.paths();
            let result = install(&fixture.home, &fixture.data, &fixture.helper, |stage| {
                if stage == InstallStage::LauncherSaved {
                    fs::write(&paths.launcher, b"user replaced launcher").unwrap();
                    return Err(Failure::Changed.into());
                }
                Ok(())
            });
            failed(result.err().unwrap());
            let launcher = fs::read(&paths.launcher).unwrap();
            let record = fs::read(&paths.record).unwrap();
            assert_eq!(launcher, b"user replaced launcher");
            assert!(read_saved(&paths).unwrap().unwrap().record.prepared);
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert_eq!(fs::read(&paths.launcher).unwrap(), launcher);
            assert_eq!(fs::read(&paths.record).unwrap(), record);
            assert_eq!(fixture.settings(), json!({}));
        }

        #[test]
        fn missing_config_is_side_effect_free_and_direct_install_reports_unavailable() {
            let fixture = Fixture::new(json!({}));
            fs::remove_file(fixture.settings_path()).unwrap();
            assert!(install(
                &fixture.home,
                &fixture.data,
                Path::new("missing helper"),
                |_| Ok(())
            )
            .unwrap()
            .is_none());
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert!(load_saved_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            assert!(!fixture.data.exists());
            assert!(!fixture.settings_path().exists());
        }

        #[test]
        fn missing_metadata_cannot_claim_or_back_up_an_orphan_wrapper() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.install();
            let paths = fixture.paths();
            let settings = fs::read(&paths.settings).unwrap();
            fs::remove_file(&paths.record).unwrap();
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            assert!(load_saved_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(fs::read(&paths.settings).unwrap(), settings);
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            fs::remove_file(&paths.launcher).unwrap();
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert_eq!(fs::read(&paths.settings).unwrap(), settings);
        }

        #[test]
        fn foreign_launcher_is_not_overwritten_and_user_modified_owned_launcher_is_preserved() {
            let fixture = Fixture::new(json!({}));
            fs::create_dir_all(&fixture.data).unwrap();
            let paths = fixture.paths();
            fs::write(&paths.launcher, b"foreign file").unwrap();
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert_eq!(fs::read(&paths.launcher).unwrap(), b"foreign file");
            assert!(!paths.record.exists());
            fs::remove_file(&paths.launcher).unwrap();
            fixture.install();
            fs::write(&paths.launcher, b"user modified file").unwrap();
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(fs::read(&paths.launcher).unwrap(), b"user modified file");
            assert_eq!(fixture.settings(), json!({}));
        }

        #[test]
        fn missing_helper_and_invalid_settings_are_side_effect_free() {
            for root in [json!([]), Value::Null, json!(true)] {
                let fixture = Fixture::new(root.clone());
                assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
                assert_eq!(fixture.settings(), root);
                assert!(!fixture.data.exists());
            }
            let fixture = Fixture::new(json!({}));
            assert!(install_at(
                &fixture.home,
                &fixture.data,
                &fixture.root.join("absent.exe")
            )
            .is_err());
            assert!(!fixture.data.exists());
            fs::write(fixture.settings_path(), b"invalid fixture json").unwrap();
            failed(
                install_at(&fixture.home, &fixture.data, &fixture.helper)
                    .err()
                    .unwrap(),
            );
            assert!(!fixture.data.exists());
        }

        #[test]
        fn dpapi_record_tampering_and_schema_mismatch_fail_without_sensitive_errors() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.install();
            let paths = fixture.paths();
            let saved = fs::read(&paths.record).unwrap();
            type RecordMutation = Box<dyn FnOnce(&mut Value)>;
            let changes: Vec<RecordMutation> = vec![
                Box::new(|record| record["version"] = json!(3)),
                Box::new(|record| record["pathDigest"] = json!("wrong")),
                Box::new(|record| record["launcherDigest"] = json!("wrong")),
                Box::new(|record| record["binding"]["id"] = json!("wrong")),
                Box::new(|record| record["binding"]["managedCommand"] = json!("wrong")),
                Box::new(|record| record["binding"]["originalPresent"] = json!(false)),
                Box::new(|record| record["binding"]["extra"] = json!(true)),
                Box::new(|record| record["extra"] = json!(true)),
                Box::new(|record| {
                    record["binding"]["originalStatusLine"]["command"] =
                        record["binding"]["managedCommand"].clone();
                }),
            ];
            for change in changes {
                fs::write(&paths.record, &saved).unwrap();
                fixture.rewrite_record(change);
                failed(load_saved_at(&fixture.home, &fixture.data).err().unwrap());
                assert!(restore_at(&fixture.home, &fixture.data).is_err());
            }
            let mut tampered = saved;
            let end = tampered.len() - 1;
            tampered[end] ^= 0x80;
            fs::write(&paths.record, tampered).unwrap();
            failed(load_saved_at(&fixture.home, &fixture.data).err().unwrap());
            assert!(bindings_crypto::unprotect(b"invalid fixture").is_err());
            assert!(bindings_crypto::protect(&[]).is_err());
        }

        #[test]
        fn saved_record_is_bound_to_home_and_data_paths() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.install();
            let other = Fixture::new(json!({}));
            assert!(load_saved_at(&other.home, &fixture.data).is_err());
            fs::create_dir_all(&other.data).unwrap();
            fs::copy(fixture.paths().record, other.paths().record).unwrap();
            assert!(load_saved_at(&fixture.home, &other.data).is_err());
            assert!(load_saved_at(&other.home, &other.data).is_err());
            assert_eq!(fixture.saved().original_status_line, Some(custom()));
        }

        #[test]
        fn oversized_settings_ciphertext_and_launcher_are_rejected_before_mutation() {
            let fixture = Fixture::new(json!({}));
            let paths = fixture.paths();
            fs::write(&paths.settings, vec![b' '; SETTINGS_CAP + 1]).unwrap();
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert!(!fixture.data.exists());
            fixture.write_settings(&json!({}));
            fixture.install();
            let settings = fs::read(&paths.settings).unwrap();
            let record = fs::read(&paths.record).unwrap();
            fs::write(&paths.record, vec![0x55; RECORD_CAP + 1]).unwrap();
            assert!(load_saved_at(&fixture.home, &fixture.data).is_err());
            assert!(restore_at(&fixture.home, &fixture.data).is_err());
            assert_eq!(fs::read(&paths.settings).unwrap(), settings);
            fs::write(&paths.record, record).unwrap();
            fs::write(&paths.launcher, vec![b' '; LAUNCHER_CAP + 1]).unwrap();
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert_eq!(fs::read(&paths.settings).unwrap(), settings);
        }

        #[test]
        fn launcher_uses_ascii_helper_alias_without_mutating_the_console() {
            let mut fixture = Fixture::new(json!({}));
            fixture.data = fixture.root.join("data space & \u{d55c}\u{ae00}");
            fixture.helper = fixture
                .root
                .join("program space & \u{d55c}\u{ae00}/agent-juice.exe");
            fs::create_dir_all(fixture.helper.parent().unwrap()).unwrap();
            fs::write(&fixture.helper, b"never executed").unwrap();
            let binding = fixture.install();
            assert_eq!(
                binding.managed_command,
                fixture.paths().safe_command().unwrap()
            );
            assert!(!binding.managed_command.contains('"'));
            assert!(safe_command_token(
                binding.managed_command.strip_prefix("call ").unwrap()
            ));
            let launcher = String::from_utf8(fs::read(fixture.paths().launcher).unwrap()).unwrap();
            assert!(launcher.starts_with("@echo off\r\nsetlocal DisableDelayedExpansion\r\n"));
            let helper_alias = short_directory(&fixture.helper).unwrap();
            assert!(launcher.contains(&format!(
                "\"{}\" --antigravity-cli\r\n",
                helper_alias.display()
            )));
            assert!(launcher.is_ascii());
            assert_eq!(launcher.matches("--antigravity-cli").count(), 1);
            assert!(!launcher.contains("chcp") && !launcher.contains("for /f"));
            assert!(!launcher.contains("--id") && !launcher.contains("--command"));
        }

        #[test]
        fn helper_percent_is_escaped_but_call_path_expansions_quotes_and_traversal_are_rejected() {
            let fixture = Fixture::new(json!({}));
            let helper = fixture.root.join("program %fixture% !/app.exe");
            fs::create_dir_all(helper.parent().unwrap()).unwrap();
            fs::write(&helper, b"never executed").unwrap();
            install_at(&fixture.home, &fixture.data, &helper).unwrap();
            let launcher = String::from_utf8(fs::read(fixture.paths().launcher).unwrap()).unwrap();
            assert!(launcher.contains("program %%fixture%% !"));
            for name in ["data\"quoted", "data\nline", "data.", "data "] {
                assert!(Paths::new(&fixture.home, &fixture.root.join(name)).is_err());
            }
            for path in [
                "C:\\data%fixture%\\ajagy.cmd",
                "C:\\data!fixture!\\ajagy.cmd",
                "C:\\data^fixture\\ajagy.cmd",
            ] {
                assert!(!safe_command_token(path));
            }
            assert!(Paths::new(&fixture.home, Path::new("relative")).is_err());
            assert!(Paths::new(&fixture.home, &fixture.root.join("data/../escape")).is_err());
            assert!(launcher_bytes(&fixture.root.join("bad\"quote.exe"), &new_id()).is_err());
            assert!(launcher_bytes(&fixture.root.join("bad\nline.exe"), &new_id()).is_err());
        }

        fn junction(link: &Path, target: &Path) {
            use std::os::windows::process::CommandExt;
            let command = format!("mklink /J \"{}\" \"{}\"", link.display(), target.display());
            let result = std::process::Command::new("cmd.exe")
                .arg("/d")
                .arg("/c")
                .raw_arg(command)
                .creation_flags(0x0800_0000)
                .output()
                .unwrap();
            assert!(result.status.success(), "fixture junction creation failed");
            assert!(is_reparse(&fs::symlink_metadata(link).unwrap()));
        }

        #[test]
        fn reparse_home_root_and_intermediate_components_are_rejected() {
            for intermediate in [false, true] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                let old = if intermediate {
                    fixture.home.join(".gemini")
                } else {
                    fixture.home.clone()
                };
                let target = fixture.root.join("redirected");
                fs::rename(&old, &target).unwrap();
                junction(&old, &target);
                assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
                assert!(load_saved_at(&fixture.home, &fixture.data).is_err());
                assert!(!fixture.data.exists());
                assert_eq!(fixture.settings()["statusLine"], custom());
            }
        }

        #[test]
        fn reparse_data_root_and_helper_parent_are_rejected_without_following_targets() {
            let fixture = Fixture::new(json!({}));
            let target = fixture.root.join("redirected-data");
            fs::create_dir(&target).unwrap();
            junction(&fixture.data, &target);
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert!(load_saved_at(&fixture.home, &fixture.data).is_err());
            assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
            assert_eq!(fixture.settings(), json!({}));

            let fixture = Fixture::new(json!({}));
            let old = fixture.helper.parent().unwrap();
            let target = fixture.root.join("redirected-program");
            fs::rename(old, &target).unwrap();
            junction(old, &target);
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert!(!fixture.data.exists());
        }

        #[test]
        fn reparse_final_entries_and_parent_swaps_are_rejected() {
            for name in ["settings", "record", "launcher"] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                if name != "settings" {
                    fixture.install();
                }
                let paths = fixture.paths();
                let path = match name {
                    "settings" => &paths.settings,
                    "record" => &paths.record,
                    _ => &paths.launcher,
                };
                fs::remove_file(path).unwrap();
                let target = fixture.root.join("redirected-entry");
                fs::create_dir(&target).unwrap();
                junction(path, &target);
                assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
                if name != "launcher" {
                    assert!(load_saved_at(&fixture.home, &fixture.data).is_err());
                }
                assert_eq!(fs::read_dir(target).unwrap().count(), 0);
            }

            let fixture = Fixture::new(json!({}));
            let target = fixture.root.join("redirected-late");
            let result = install(&fixture.home, &fixture.data, &fixture.helper, |stage| {
                if stage == InstallStage::BeforeSettings {
                    let parent = fixture.settings_path().parent().unwrap().to_path_buf();
                    fs::rename(&parent, &target).unwrap();
                    junction(&parent, &target);
                }
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(fixture.settings(), json!({}));
        }

        #[test]
        fn atomic_compare_rejects_changed_file_and_cleans_temporary_files() {
            let fixture = Fixture::new(json!({}));
            let path = fixture.settings_path();
            let initial = fs::read(&path).unwrap();
            fs::write(&path, b"changed fixture").unwrap();
            assert!(atomic_replace(&path, Some(&initial), b"replacement", SETTINGS_CAP).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"changed fixture");
            assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        }

        #[test]
        fn generation_identifiers_are_unique_hex_and_not_command_arguments() {
            let mut ids = std::collections::HashSet::new();
            for _ in 0..1024 {
                let id = new_id();
                assert!(valid_digest(&id));
                assert!(ids.insert(id));
            }
            let fixture = Fixture::new(json!({}));
            let binding = fixture.install();
            assert!(!binding.managed_command.contains(&binding.id));
        }

        #[test]
        fn direct_ascii_command_and_unicode_helper_alias_need_no_console_change() {
            let mut fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.helper = fixture.root.join("Helper Space & \u{d55c}\u{ae00}/app.exe");
            fs::create_dir_all(fixture.helper.parent().unwrap()).unwrap();
            fs::write(&fixture.helper, b"not executed").unwrap();
            let binding = fixture.install();
            assert_eq!(
                binding.managed_command,
                format!("call {}", fixture.paths().launcher.display())
            );
            assert!(!binding.managed_command.contains('"'));
            assert_eq!(fixture.paths().launcher.file_name().unwrap(), "ajagy.cmd");
            let launcher = String::from_utf8(fs::read(fixture.paths().launcher).unwrap()).unwrap();
            assert!(launcher.is_ascii());
            assert!(launcher.contains(&format!(
                "\"{}\" --antigravity-cli",
                short_directory(&fixture.helper).unwrap().display()
            )));
            assert!(!launcher.contains("chcp"));
        }

        #[test]
        fn unicode_helper_without_an_ascii_alias_leaves_user_settings_untouched() {
            let mut fixture = Fixture::new(json!({"statusLine": custom()}));
            let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
            let parent = workspace.join("src-tauri/target/antigravity-cli-tests");
            let helper_dir = parent.join(format!("Helper \u{d55c}\u{ae00} {}", new_id()));
            assert!(helper_dir.is_absolute() && helper_dir.starts_with(&parent));
            fs::create_dir_all(&helper_dir).unwrap();
            fixture.helper = helper_dir.join("app.exe");
            fs::write(&fixture.helper, b"not executed").unwrap();
            let original = fs::read(fixture.paths().settings).unwrap();
            let alias = short_directory(&fixture.helper).unwrap();
            let result = install_at(&fixture.home, &fixture.data, &fixture.helper);
            if alias.to_str().is_some_and(str::is_ascii) {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err());
                assert_eq!(fs::read(fixture.paths().settings).unwrap(), original);
                assert!(!fixture.paths().record.exists());
                assert!(!fixture.paths().launcher.exists());
            }
            fs::remove_dir_all(helper_dir).unwrap();
        }

        fn call_control(binding: &Binding, launcher: &Path) {
            use std::os::windows::process::CommandExt;
            fs::write(launcher, b"@echo off\r\necho BINDING_CALL_CONTROL\r\n").unwrap();
            assert!(!binding.managed_command.contains('"'));
            // Go quotes this whitespace-containing argument, without any inner quote escaping.
            let argument = format!("/D /S /C \"{}\"", binding.managed_command);
            let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("cmd.exe");
            let result = std::process::Command::new(shell)
                .raw_arg(argument)
                .creation_flags(0x0800_0000)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "unquoted Go command control failed"
            );
            assert_eq!(
                String::from_utf8(result.stdout).unwrap().trim(),
                "BINDING_CALL_CONTROL"
            );
        }

        #[test]
        fn direct_unquoted_call_survives_go_argument_quoting() {
            let fixture = Fixture::new(json!({}));
            let binding = fixture.install();
            call_control(&binding, &fixture.paths().launcher);
        }

        #[test]
        fn c_temp_actual_short_alias_executes_or_reports_explicit_unsupported() {
            let mut fixture = Fixture::new(json!({}));
            fixture.data = fixture.root.join("Data Space & \u{d55c}\u{ae00}");
            create_directories(&fixture.data).unwrap();
            let alias = short_directory(&fixture.data).unwrap();
            let alias_launcher = alias.join(LAUNCHER_NAME);
            if !safe_command_token(alias_launcher.to_str().unwrap()) {
                println!("C_TEMP_SHORT_ALIAS=UNSUPPORTED_EXPECTED_FAILURE");
                failed(
                    install_at(&fixture.home, &fixture.data, &fixture.helper)
                        .err()
                        .unwrap(),
                );
                assert!(!fixture.paths().record.exists());
                assert!(!fixture.paths().launcher.exists());
                assert_eq!(fixture.settings(), json!({}));
                return;
            }
            let binding = fixture.install();
            println!("C_TEMP_SHORT_ALIAS=SUPPORTED");
            assert_eq!(
                binding.managed_command,
                format!("call {}", alias_launcher.display())
            );
            assert_eq!(
                DirectoryGuard::new(&alias).unwrap().identity().unwrap(),
                DirectoryGuard::new(&fixture.data)
                    .unwrap()
                    .identity()
                    .unwrap()
            );
            call_control(&binding, &fixture.paths().launcher);
        }

        #[test]
        fn workspace_data_volume_without_short_alias_is_explicitly_unsupported() {
            let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
            let parent = workspace.join("src-tauri/target/antigravity-cli-tests");
            let data = parent.join(format!("Short Space & \u{d55c}\u{ae00} {}", new_id()));
            assert!(data.is_absolute() && data.starts_with(&parent));
            create_directories(&data).unwrap();
            let paths = Paths::new(&workspace.join("fixture-no-settings"), &data).unwrap();
            let alias = short_directory(&data).unwrap();
            let expected_safe = safe_command_token(alias.join(LAUNCHER_NAME).to_str().unwrap());
            let result = paths.safe_command();
            fs::remove_dir(&data).unwrap();
            if expected_safe {
                println!("WORKSPACE_DATA_SHORT_ALIAS=SUPPORTED");
                assert!(result.is_ok());
            } else {
                println!("WORKSPACE_DATA_SHORT_ALIAS=UNSUPPORTED_EXPECTED_FAILURE");
                failed(result.err().unwrap());
            }
        }

        #[test]
        fn legacy_restore_on_unsupported_alias_volume_preserves_encrypted_backup() {
            let mut fixture = Fixture::new(json!({"statusLine": custom(), "root": 7}));
            let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
            let parent = workspace.join("src-tauri/target/antigravity-cli-tests");
            fixture.data = parent.join(format!("Legacy Space & \u{d55c}\u{ae00} {}", new_id()));
            assert!(fixture.data.is_absolute() && fixture.data.starts_with(&parent));
            let old = fixture.legacy_install(false);
            let paths = fixture.paths();
            let encrypted = fs::read(&paths.record).unwrap();
            let supported = paths.safe_command().is_ok();
            if !supported {
                assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
                assert_eq!(fs::read(&paths.record).unwrap(), encrypted);
                assert!(!paths.launcher.exists());
            }
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(
                fixture.settings(),
                json!({"statusLine": custom(), "root": 7})
            );
            assert_eq!(fixture.saved().id, old.id);
            assert_eq!(fs::read(&paths.record).unwrap(), encrypted);
            fs::remove_file(&paths.record).unwrap();
            fs::remove_file(&paths.legacy_launcher).unwrap();
            fs::remove_dir(&fixture.data).unwrap();
            assert!(load_saved_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            restore_at(&fixture.home, &fixture.data).unwrap();
        }

        #[test]
        fn legacy_quoted_record_upgrades_only_original_or_owned_and_preserves_backup() {
            for original in [false, true] {
                let fixture = Fixture::new(json!({"statusLine": custom(), "root": 7}));
                let old = fixture.legacy_install(original);
                assert_eq!(fixture.saved().id, old.id);
                let legacy_bytes = fs::read(fixture.paths().legacy_launcher).unwrap();
                let new = fixture.install();
                assert_ne!(new.id, old.id);
                assert_eq!(new.original_status_line, Some(custom()));
                assert!(!new.managed_command.contains('"'));
                assert_eq!(
                    fs::read(fixture.paths().legacy_launcher).unwrap(),
                    legacy_bytes
                );
                restore_at(&fixture.home, &fixture.data).unwrap();
                assert_eq!(
                    fixture.settings(),
                    json!({"statusLine": custom(), "root": 7})
                );
            }
        }

        #[test]
        fn legacy_restore_checks_launcher_digest_and_user_command_is_not_upgraded() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.legacy_install(false);
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(fixture.settings()["statusLine"], custom());
            fixture.write_settings(&json!({"statusLine": {"command": "echo user replacement"}}));
            let before = fs::read(fixture.paths().record).unwrap();
            assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
            assert_eq!(fs::read(fixture.paths().record).unwrap(), before);
            assert!(!fixture.paths().launcher.exists());
            fs::write(fixture.paths().legacy_launcher, b"user launcher").unwrap();
            assert!(load_saved_at(&fixture.home, &fixture.data).is_err());
            assert!(restore_at(&fixture.home, &fixture.data).is_err());
            assert_eq!(
                fs::read(fixture.paths().legacy_launcher).unwrap(),
                b"user launcher"
            );
        }

        #[test]
        fn legacy_upgrade_failure_keeps_old_ciphertext_launcher_and_settings() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.legacy_install(false);
            let paths = fixture.paths();
            let record = fs::read(&paths.record).unwrap();
            let legacy = fs::read(&paths.legacy_launcher).unwrap();
            let settings = fs::read(&paths.settings).unwrap();
            let result = install(
                &fixture.home,
                &fixture.data,
                &fixture.helper,
                fail_at(InstallStage::BeforeSettings),
            );
            failed(result.err().unwrap());
            assert_eq!(fs::read(&paths.record).unwrap(), record);
            assert_eq!(fs::read(&paths.legacy_launcher).unwrap(), legacy);
            assert_eq!(fs::read(&paths.settings).unwrap(), settings);
            assert!(!paths.launcher.exists());
        }

        #[test]
        fn prepared_reinstall_resumes_all_persisted_phases_and_keeps_new_generation() {
            for stage in 0..=2 {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                let first = fixture.install();
                restore_at(&fixture.home, &fixture.data).unwrap();
                let prepared = fixture.interrupt(stage);
                assert_ne!(prepared.id, first.id);
                let resumed = fixture.install();
                assert_eq!(resumed.id, prepared.id);
                assert_eq!(resumed.original_status_line, Some(custom()));
                assert!(
                    !read_saved(&fixture.paths())
                        .unwrap()
                        .unwrap()
                        .record
                        .prepared
                );
                assert_eq!(
                    load_owned_at(&fixture.home, &fixture.data)
                        .unwrap()
                        .unwrap()
                        .id,
                    resumed.id
                );
            }
        }

        #[test]
        fn prepared_active_helper_update_resumes_prior_launcher_digest() {
            let mut fixture = Fixture::new(json!({"statusLine": custom()}));
            let first = fixture.install();
            fixture.helper = fixture.root.join("updated helper/app.exe");
            fs::create_dir_all(fixture.helper.parent().unwrap()).unwrap();
            fs::write(&fixture.helper, b"not executed").unwrap();
            let prepared = fixture.interrupt(0);
            let resumed = fixture.install();
            assert_eq!(prepared.id, first.id);
            assert_eq!(resumed.id, first.id);
            assert_eq!(resumed.original_status_line, Some(custom()));
            let body = String::from_utf8(fs::read(fixture.paths().launcher).unwrap()).unwrap();
            assert!(body.contains(fixture.helper.to_str().unwrap()));
        }

        #[test]
        fn prepared_legacy_migration_resumes_or_restores_only_verified_previous_command() {
            for stage in 0..=2 {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                let old = fixture.legacy_install(false);
                let prepared = fixture.interrupt(stage);
                let resumed = fixture.install();
                assert_ne!(resumed.id, old.id);
                assert_eq!(resumed.id, prepared.id);
                assert_eq!(resumed.original_status_line, Some(custom()));
            }
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.legacy_install(false);
            let prepared = fixture.interrupt(0);
            restore_at(&fixture.home, &fixture.data).unwrap();
            assert_eq!(fixture.settings()["statusLine"], custom());
            assert!(
                !read_saved(&fixture.paths())
                    .unwrap()
                    .unwrap()
                    .record
                    .prepared
            );
            assert_ne!(fixture.install().id, prepared.id);
        }

        #[test]
        fn prepared_journal_cannot_reclassify_a_user_launcher_or_command() {
            for changed_launcher in [false, true] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                fixture.install();
                restore_at(&fixture.home, &fixture.data).unwrap();
                fixture.interrupt(0);
                if changed_launcher {
                    fs::write(fixture.paths().launcher, b"user changed launcher").unwrap();
                } else {
                    fixture
                        .write_settings(&json!({"statusLine": {"command": "echo user changed"}}));
                }
                let record = fs::read(fixture.paths().record).unwrap();
                let launcher = fs::read(fixture.paths().launcher).unwrap();
                let settings = fs::read(fixture.settings_path()).unwrap();
                assert!(install_at(&fixture.home, &fixture.data, &fixture.helper).is_err());
                assert_eq!(fs::read(fixture.paths().record).unwrap(), record);
                assert_eq!(fs::read(fixture.paths().launcher).unwrap(), launcher);
                assert_eq!(fs::read(fixture.settings_path()).unwrap(), settings);
            }
        }

        #[test]
        fn restoring_a_prepared_record_cancels_resume_generation() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.install();
            restore_at(&fixture.home, &fixture.data).unwrap();
            let prepared = fixture.interrupt(0);
            restore_at(&fixture.home, &fixture.data).unwrap();
            let new = fixture.install();
            assert_ne!(prepared.id, new.id);
            assert_eq!(new.original_status_line, Some(custom()));
        }

        #[test]
        fn prepared_legacy_original_stdout_survives_all_interrupted_phases() {
            for stage in 0..=2 {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                fixture.legacy_install(false);
                fixture.interrupt(stage);
                let binding = fixture.saved();
                let before = fs::read(fixture.settings_path()).unwrap();
                let input = b"synthetic unchanged stdin";
                let output = crate::antigravity_cli::original::run_if_connected_at_with(
                    &binding,
                    input,
                    &fixture.home,
                    |command, received| {
                        assert!(
                            Some(command) == super::command(binding.original_status_line.as_ref())
                        );
                        assert!(received == input);
                        b"synthetic original stdout".to_vec()
                    },
                );
                assert!(output == b"synthetic original stdout");
                assert!(fs::read(fixture.settings_path()).unwrap() == before);
                let owned = load_owned_at(&fixture.home, &fixture.data).unwrap();
                assert_eq!(owned.is_some(), stage == 2);
                if let Some(owned) = owned {
                    assert!(owned.verified_previous_ownership.is_none());
                }
            }
        }

        #[test]
        fn prepared_legacy_original_stdout_rechecks_launcher_and_journal() {
            for change in 0..5 {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                fixture.legacy_install(false);
                fixture.interrupt(0);
                let binding = fixture.saved();
                match change {
                    0 => {
                        fs::write(fixture.paths().legacy_launcher, b"user changed launcher")
                            .unwrap();
                    }
                    1 => fixture.rewrite_record(|record| {
                        record["prepared"] = json!(false);
                        let fields = record.as_object_mut().unwrap();
                        fields.remove("previousCommand");
                        fields.remove("legacyLauncherDigest");
                    }),
                    2 => fs::remove_file(fixture.paths().record).unwrap(),
                    3 => fixture.rewrite_record(|record| {
                        record["binding"]["id"] = json!("d".repeat(64));
                    }),
                    _ => fixture.rewrite_record(|record| {
                        record["binding"]["originalStatusLine"]["command"] =
                            json!("synthetic changed original");
                    }),
                }
                let output = crate::antigravity_cli::original::run_if_connected_at_with(
                    &binding,
                    b"{}",
                    &fixture.home,
                    |_, _| panic!("changed previous ownership must not execute"),
                );
                assert!(output.is_empty());
            }
        }

        #[test]
        fn prepared_legacy_original_stdout_keeps_disabled_and_user_command_guards() {
            for disabled in [false, true] {
                let mut original = custom();
                if disabled {
                    original["enabled"] = json!(false);
                }
                let fixture = Fixture::new(json!({"statusLine": original}));
                fixture.legacy_install(false);
                fixture.interrupt(0);
                let binding = fixture.saved();
                if !disabled {
                    fixture.write_settings(&json!({"statusLine": {
                        "command": "synthetic user replacement", "enabled": true
                    }}));
                }
                let before = fs::read(fixture.settings_path()).unwrap();
                let output = crate::antigravity_cli::original::run_if_connected_at_with(
                    &binding,
                    b"{}",
                    &fixture.home,
                    |_, _| panic!("disabled or changed original must not execute"),
                );
                assert!(output.is_empty());
                assert!(fs::read(fixture.settings_path()).unwrap() == before);
                assert!(load_owned_at(&fixture.home, &fixture.data)
                    .unwrap()
                    .is_none());
            }
        }

        #[test]
        fn prepared_legacy_original_stdout_proof_is_not_serialized_or_cross_home() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.legacy_install(false);
            fixture.interrupt(0);
            let binding = fixture.saved();
            let serialized = Zeroizing::new(serde_json::to_vec(&binding).unwrap());
            let mut fields: Value = serde_json::from_slice(&serialized).unwrap();
            assert_eq!(fields.as_object().unwrap().len(), 4);
            for key in [
                "id",
                "managedCommand",
                "originalPresent",
                "originalStatusLine",
            ] {
                assert!(fields.get(key).is_some());
            }
            fields["verifiedPreviousOwnership"] = json!(true);
            assert!(serde_json::from_value::<Binding>(fields).is_err());
            let deserialized: Binding = serde_json::from_slice(&serialized).unwrap();
            let output = crate::antigravity_cli::original::run_if_connected_at_with(
                &deserialized,
                b"{}",
                &fixture.home,
                |_, _| panic!("deserialization must not create verified ownership"),
            );
            assert!(output.is_empty());

            let other = Fixture::new(fixture.settings());
            let output = crate::antigravity_cli::original::run_if_connected_at_with(
                &binding,
                b"{}",
                &other.home,
                |_, _| panic!("previous ownership must remain home bound"),
            );
            assert!(output.is_empty());
        }

        #[test]
        fn prepared_legacy_original_stdout_does_not_accept_finalized_legacy_string() {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            fixture.legacy_install(false);
            fixture.interrupt(0);
            fixture.install();
            let binding = fixture.saved();
            fixture.write_settings(&json!({"statusLine": {
                "command": fixture.paths().legacy_command, "enabled": true
            }}));
            let output = crate::antigravity_cli::original::run_if_connected_at_with(
                &binding,
                b"{}",
                &fixture.home,
                |_, _| panic!("legacy string alone must not prove ownership"),
            );
            assert!(output.is_empty());
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
        }

        fn assert_current_cli_disabled_blocks_original(prepared: bool, legacy: bool) {
            let fixture = Fixture::new(json!({"statusLine": custom()}));
            if legacy {
                fixture.legacy_install(false);
            } else {
                fixture.install();
            }
            if prepared {
                fixture.interrupt(1);
            }
            assert_eq!(
                read_saved(&fixture.paths())
                    .unwrap()
                    .unwrap()
                    .record
                    .prepared,
                prepared
            );
            let mut root = fixture.settings();
            root["statusLine"]["enabled"] = json!(false);
            fixture.write_settings(&root);
            let binding = fixture.saved();
            let before = fs::read(fixture.settings_path()).unwrap();
            assert!(load_owned_at(&fixture.home, &fixture.data)
                .unwrap()
                .is_none());
            let input = b"synthetic unchanged stdin";
            let output = crate::antigravity_cli::original::run_if_connected_at_with(
                &binding,
                input,
                &fixture.home,
                |_, _| panic!("current CLI disabled flag must block original dispatch"),
            );
            assert!(output.is_empty());
            assert_eq!(fs::read(fixture.settings_path()).unwrap(), before);

            for enabled in [Some(json!(true)), None] {
                if let Some(enabled) = enabled {
                    root["statusLine"]["enabled"] = enabled;
                } else {
                    root["statusLine"]
                        .as_object_mut()
                        .unwrap()
                        .remove("enabled");
                }
                fixture.write_settings(&root);
                let binding = fixture.saved();
                let output = crate::antigravity_cli::original::run_if_connected_at_with(
                    &binding,
                    input,
                    &fixture.home,
                    |command, received| {
                        assert_eq!(
                            Some(command),
                            super::command(binding.original_status_line.as_ref())
                        );
                        assert_eq!(received, input);
                        b"synthetic original stdout".to_vec()
                    },
                );
                assert_eq!(output, b"synthetic original stdout");
            }
        }

        #[test]
        fn current_cli_disable_blocks_finalized_original() {
            assert_current_cli_disabled_blocks_original(false, false);
        }

        #[test]
        fn current_cli_disable_blocks_prepared_update_original() {
            assert_current_cli_disabled_blocks_original(true, false);
        }

        #[test]
        fn current_cli_disable_blocks_prepared_legacy_original() {
            assert_current_cli_disabled_blocks_original(true, true);
        }

        #[test]
        fn current_cli_disable_keeps_original_disabled_and_execution_bounds() {
            let mut original = custom();
            original["enabled"] = json!(false);
            let fixture = Fixture::new(json!({"statusLine": original}));
            fixture.install();
            assert_eq!(fixture.settings()["statusLine"]["enabled"], true);
            let output = crate::antigravity_cli::original::run_if_connected_at_with(
                &fixture.saved(),
                b"{}",
                &fixture.home,
                |_, _| panic!("stored disabled original must not execute"),
            );
            assert!(output.is_empty());

            for command in [
                String::new(),
                " ".repeat(2),
                "x".repeat(32 * 1024 + 1),
                "x\0y".into(),
            ] {
                let fixture = Fixture::new(json!({"statusLine": {"command": command}}));
                fixture.install();
                let output = crate::antigravity_cli::original::run_if_connected_at_with(
                    &fixture.saved(),
                    b"{}",
                    &fixture.home,
                    |_, _| panic!("invalid original command must not execute"),
                );
                assert!(output.is_empty());
            }
            for bytes in [
                vec![b'{'],
                vec![b'x'; crate::statusline::MAX_STATUSLINE_INPUT_BYTES + 1],
            ] {
                let fixture = Fixture::new(json!({"statusLine": custom()}));
                fixture.install();
                let binding = fixture.saved();
                fs::write(fixture.settings_path(), bytes).unwrap();
                let output = crate::antigravity_cli::original::run_if_connected_at_with(
                    &binding,
                    b"{}",
                    &fixture.home,
                    |_, _| panic!("invalid or oversized settings must not execute"),
                );
                assert!(output.is_empty());
            }
        }
    }
}
