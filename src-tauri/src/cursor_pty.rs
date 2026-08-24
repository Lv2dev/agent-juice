use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_VERSION_ENTRIES: usize = 64;
const MAX_VERSION_CANDIDATES: usize = 16;
const READ_CHUNK_BYTES: usize = 8 * 1024;
const TERMINATION_GRACE: Duration = Duration::from_millis(750);
const CLEANUP_RESERVE: Duration = Duration::from_millis(1_500);
static ISOLATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    file: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAgentCommand {
    pub node: PathBuf,
    pub index: PathBuf,
    pub version: String,
    node_identity: FileIdentity,
    index_identity: FileIdentity,
}

#[cfg(windows)]
fn open_windows_file_identity(
    path: &Path,
    share: windows::Win32::Storage::FileSystem::FILE_SHARE_MODE,
) -> anyhow::Result<(windows::Win32::Foundation::HANDLE, FileIdentity)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::CloseHandle,
            Storage::FileSystem::{
                CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
                FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, OPEN_EXISTING,
            },
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
            share,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if let Err(error) = unsafe { GetFileInformationByHandle(handle, &mut information) } {
        let _ = unsafe { CloseHandle(handle) };
        return Err(error.into());
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        let _ = unsafe { CloseHandle(handle) };
        anyhow::bail!("Cursor Agent runtime file identity rejected");
    }
    Ok((
        handle,
        FileIdentity {
            volume: information.dwVolumeSerialNumber as u64,
            file: ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
        },
    ))
}

#[cfg(windows)]
fn query_file_identity(path: &Path) -> anyhow::Result<FileIdentity> {
    use windows::Win32::{
        Foundation::CloseHandle,
        Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
    };

    let (handle, identity) =
        open_windows_file_identity(path, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)?;
    let _ = unsafe { CloseHandle(handle) };
    Ok(identity)
}

#[cfg(not(windows))]
fn query_file_identity(path: &Path) -> anyhow::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path)?;
    Ok(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

fn version_sort_key(name: &str) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let parts = name.split('-').collect::<Vec<_>>();
    if parts.len() != 2 && parts.len() != 5 {
        return None;
    }
    let date = parts[0].split('.').collect::<Vec<_>>();
    if date.len() != 3
        || date[0].len() != 4
        || !(1..=2).contains(&date[1].len())
        || !(1..=2).contains(&date[2].len())
        || !date
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let year = date[0].parse::<u32>().ok()?;
    let month = date[1].parse::<u32>().ok()?.checked_sub(1)?;
    let day = date[2].parse::<u32>().ok()?.checked_sub(1)?;
    if month >= 12 || day >= 31 {
        return None;
    }
    let (hour, minute, second, commit) = if parts.len() == 5 {
        if !parts[1..4]
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        let hour = parts[1].parse::<u32>().ok()?;
        let minute = parts[2].parse::<u32>().ok()?;
        let second = parts[3].parse::<u32>().ok()?;
        if hour >= 24 || minute >= 60 || second >= 60 {
            return None;
        }
        (hour, minute, second, parts[4])
    } else {
        (0, 0, 0, parts[1])
    };
    if commit.is_empty()
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some((year, month, day, hour, minute, second))
}

fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
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

fn inspect_safe_path(path: &Path, directory: bool) -> anyhow::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Cursor Agent installation could not be inspected: {error}"
            ))
        }
    };
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type || metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("Cursor Agent installation path rejected");
    }
    Ok(true)
}

pub fn resolve_cursor_agent_from(
    local_app_data: &Path,
) -> anyhow::Result<Option<CursorAgentCommand>> {
    resolve_cursor_agent_from_until(local_app_data, Instant::now() + Duration::from_secs(5))
}

fn ensure_before_deadline(deadline: Instant) -> anyhow::Result<()> {
    if Instant::now() >= deadline {
        anyhow::bail!("Cursor Agent /usage timed out");
    }
    Ok(())
}

fn resolve_cursor_agent_from_until(
    local_app_data: &Path,
    deadline: Instant,
) -> anyhow::Result<Option<CursorAgentCommand>> {
    ensure_before_deadline(deadline)?;
    if !inspect_safe_path(local_app_data, true)? {
        return Ok(None);
    }
    let cursor_root = local_app_data.join("cursor-agent");
    if !inspect_safe_path(&cursor_root, true)? {
        return Ok(None);
    }
    let versions = cursor_root.join("versions");
    if !inspect_safe_path(&versions, true)? {
        return Ok(None);
    }
    ensure_before_deadline(deadline)?;
    let canonical_root = fs::canonicalize(local_app_data)?;
    let canonical_versions = fs::canonicalize(&versions)?;
    if !canonical_versions.starts_with(&canonical_root) {
        anyhow::bail!("Cursor Agent installation root rejected");
    }

    let mut candidates = Vec::new();
    for (entry_index, entry) in fs::read_dir(&versions)?.enumerate() {
        if entry_index >= MAX_VERSION_ENTRIES {
            anyhow::bail!("Cursor Agent version entry limit exceeded");
        }
        ensure_before_deadline(deadline)?;
        let entry = entry.map_err(|error| {
            anyhow::anyhow!("Cursor Agent installation could not be inspected: {error}")
        })?;
        let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(sort_key) = version_sort_key(&version) else {
            continue;
        };
        let directory = entry.path();
        if !inspect_safe_path(&directory, true)? {
            continue;
        }
        let node = directory.join("node.exe");
        let index = directory.join("index.js");
        if !inspect_safe_path(&node, false)? || !inspect_safe_path(&index, false)? {
            continue;
        }
        let canonical_directory = fs::canonicalize(&directory)?;
        let canonical_node = fs::canonicalize(&node)?;
        let canonical_index = fs::canonicalize(&index)?;
        if canonical_directory.parent() != Some(canonical_versions.as_path())
            || canonical_node.parent() != Some(canonical_directory.as_path())
            || canonical_index.parent() != Some(canonical_directory.as_path())
        {
            anyhow::bail!("Cursor Agent installation containment rejected");
        }
        let node_identity = query_file_identity(&node)?;
        let index_identity = query_file_identity(&index)?;
        if candidates.len() >= MAX_VERSION_CANDIDATES {
            anyhow::bail!("Cursor Agent version candidate limit exceeded");
        }
        candidates.push((
            sort_key,
            version.clone(),
            CursorAgentCommand {
                node,
                index,
                version,
                node_identity,
                index_identity,
            },
        ));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    Ok(candidates.into_iter().next().map(|(_, _, command)| command))
}

pub fn resolve_cursor_agent() -> anyhow::Result<CursorAgentCommand> {
    resolve_cursor_agent_until(Instant::now() + Duration::from_secs(5))
}

fn resolve_cursor_agent_until(deadline: Instant) -> anyhow::Result<CursorAgentCommand> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Cursor Agent local app data unavailable"))?;
    resolve_cursor_agent_from_until(&local_app_data, deadline)?
        .ok_or_else(|| anyhow::anyhow!("Cursor Agent executable unavailable"))
}

fn revalidate_cursor_agent(command: &CursorAgentCommand) -> anyhow::Result<()> {
    if version_sort_key(&command.version).is_none()
        || !inspect_safe_path(&command.node, false)?
        || !inspect_safe_path(&command.index, false)?
    {
        anyhow::bail!("Cursor Agent executable identity rejected");
    }
    let version_directory = command
        .node
        .parent()
        .filter(|parent| command.index.parent() == Some(*parent))
        .filter(|parent| {
            parent.file_name().and_then(|name| name.to_str()) == Some(&command.version)
        })
        .ok_or_else(|| anyhow::anyhow!("Cursor Agent executable containment rejected"))?;
    let versions = version_directory
        .parent()
        .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("versions"))
        .ok_or_else(|| anyhow::anyhow!("Cursor Agent versions directory rejected"))?;
    let cursor_root = versions
        .parent()
        .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("cursor-agent"))
        .ok_or_else(|| anyhow::anyhow!("Cursor Agent installation root rejected"))?;
    for directory in [cursor_root, versions, version_directory] {
        if !inspect_safe_path(directory, true)? {
            anyhow::bail!("Cursor Agent installation path unavailable");
        }
    }
    let canonical_directory = fs::canonicalize(version_directory)?;
    for file in [&command.node, &command.index] {
        if fs::canonicalize(file)?.parent() != Some(canonical_directory.as_path()) {
            anyhow::bail!("Cursor Agent executable containment rejected");
        }
    }
    if query_file_identity(&command.node)? != command.node_identity
        || query_file_identity(&command.index)? != command.index_identity
    {
        anyhow::bail!("Cursor Agent runtime file identity changed");
    }
    Ok(())
}

struct CursorIsolation {
    root: Option<PathBuf>,
    workspace: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

impl CursorIsolation {
    fn create(base: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(base)?;
        if !inspect_safe_path(base, true)? {
            anyhow::bail!("Cursor Agent isolation root unavailable");
        }
        for _ in 0..16 {
            let sequence = ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = base.join(format!("run-{}-{sequence}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let workspace = root.join("workspace");
                    let home = root.join("home");
                    let data = root.join("data");
                    let setup = fs::create_dir(&workspace)
                        .and_then(|()| fs::create_dir(&home))
                        .and_then(|()| fs::create_dir(&data));
                    if let Err(error) = setup {
                        let _ = fs::remove_dir_all(&root);
                        return Err(error.into());
                    }
                    return Ok(Self {
                        root: Some(root),
                        workspace,
                        home,
                        data,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("Cursor Agent isolation directory unavailable")
    }

    fn take_cleanup_root(&mut self) -> PathBuf {
        self.root
            .take()
            .expect("Cursor isolation cleanup root is owned exactly once")
    }
}

impl Drop for CursorIsolation {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

#[cfg(windows)]
struct UncontainedProcessGuard {
    process: windows::Win32::Foundation::HANDLE,
    armed: bool,
}

#[cfg(windows)]
impl UncontainedProcessGuard {
    fn new(process: windows::Win32::Foundation::HANDLE) -> Self {
        Self {
            process,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(windows)]
impl Drop for UncontainedProcessGuard {
    fn drop(&mut self) {
        use windows::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

        if self.armed && !self.process.is_invalid() {
            let _ = unsafe { TerminateProcess(self.process, 1) };
            let _ =
                unsafe { WaitForSingleObject(self.process, TERMINATION_GRACE.as_millis() as u32) };
        }
    }
}

fn cursor_environment_variables(
    home: &Path,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const ALLOWED: [&str; 25] = [
        "ALL_PROXY",
        "APPDATA",
        "COMSPEC",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "LANG",
        "LC_ALL",
        "LOCALAPPDATA",
        "NODE_COMPILE_CACHE",
        "NO_PROXY",
        "OS",
        "PATH",
        "PATHEXT",
        "PROGRAMDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
        "SSL_CERT_DIR",
        "SSL_CERT_FILE",
        "SYSTEMDRIVE",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
        "WINDIR",
        "NUMBER_OF_PROCESSORS",
    ];
    let mut variables = BTreeMap::new();
    for (key, value) in inherited {
        let normalized = key.to_string_lossy().to_ascii_uppercase();
        if ALLOWED.iter().any(|allowed| *allowed == normalized) {
            variables.insert(normalized, (key, value));
        }
    }
    variables.insert("HOME".into(), ("HOME".into(), home.as_os_str().to_owned()));
    variables.insert(
        "USERPROFILE".into(),
        ("USERPROFILE".into(), home.as_os_str().to_owned()),
    );
    variables.insert(
        "CURSOR_INVOKED_AS".into(),
        ("CURSOR_INVOKED_AS".into(), "cursor-agent".into()),
    );
    variables.into_values().collect()
}

#[cfg(windows)]
fn cursor_environment_block(home: &Path) -> anyhow::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut block = Vec::new();
    for (key, value) in cursor_environment_variables(home, std::env::vars_os()) {
        let mut entry = key;
        entry.push("=");
        entry.push(value);
        let encoded = entry.encode_wide().collect::<Vec<_>>();
        if encoded.contains(&0) {
            anyhow::bail!("Cursor Agent environment contains an invalid value");
        }
        block.extend(encoded);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

pub fn strip_terminal_sequences(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let byte = bytes[index];
            if byte == b'\r' {
                out.push(b'\n');
            } else if byte == b'\n' || byte == b'\t' || byte >= 0x20 {
                out.push(byte);
            }
            index += 1;
            continue;
        }

        index += 1;
        let Some(kind) = bytes.get(index).copied() else {
            break;
        };
        index += 1;
        match kind {
            b'[' => {
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' => {
                while index < bytes.len() {
                    match bytes[index] {
                        0x07 => {
                            index += 1;
                            break;
                        }
                        0x1b if bytes.get(index + 1) == Some(&b'\\') => {
                            index += 2;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            _ => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn usage_table_complete(text: &str) -> bool {
    let has_cursor_pool = ["Auto", "Cursor Models"]
        .iter()
        .any(|label| text.contains(label));
    let has_other_pool = ["API", "Other Models"]
        .iter()
        .any(|label| text.contains(label));
    has_cursor_pool && has_other_pool && text.match_indices("% used").count() >= 2
}

#[cfg(windows)]
pub fn capture_cursor_usage(workspace: &Path, timeout: Duration) -> anyhow::Result<String> {
    capture_cursor_usage_until(workspace, Instant::now() + timeout)
}

#[cfg(windows)]
pub fn capture_cursor_usage_until(workspace: &Path, deadline: Instant) -> anyhow::Result<String> {
    let command = resolve_cursor_agent_until(deadline)?;
    revalidate_cursor_agent(&command)?;
    capture_cursor_usage_with_command(&command, workspace, deadline)
}

#[cfg(not(windows))]
pub fn capture_cursor_usage(_workspace: &Path, _timeout: Duration) -> anyhow::Result<String> {
    anyhow::bail!("Cursor Agent usage collection is only available on Windows")
}

#[cfg(not(windows))]
pub fn capture_cursor_usage_until(_workspace: &Path, _deadline: Instant) -> anyhow::Result<String> {
    anyhow::bail!("Cursor Agent usage collection is only available on Windows")
}

#[cfg(windows)]
fn capture_cursor_usage_with_command(
    command: &CursorAgentCommand,
    isolation_base: &Path,
    deadline: Instant,
) -> anyhow::Result<String> {
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::{
            Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
            System::{
                Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON},
                JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, TerminateJobObject,
                    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
                Pipes::CreatePipe,
                Threading::{
                    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
                    InitializeProcThreadAttributeList, ResumeThread, UpdateProcThreadAttribute,
                    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
                    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
                    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
                    STARTUPINFOEXW, STARTUPINFOW,
                },
            },
        },
    };

    struct OwnedHandle(HANDLE);
    impl OwnedHandle {
        fn take(&mut self) -> HANDLE {
            std::mem::take(&mut self.0)
        }
    }
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    struct OwnedPseudoConsole(Option<HPCON>);
    impl Drop for OwnedPseudoConsole {
        fn drop(&mut self) {
            if let Some(console) = self.0.take() {
                unsafe { ClosePseudoConsole(console) };
            }
        }
    }

    struct OwnedAttributeList {
        list: LPPROC_THREAD_ATTRIBUTE_LIST,
        _storage: Vec<usize>,
    }
    impl Drop for OwnedAttributeList {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.list) };
        }
    }

    struct Session {
        process: OwnedHandle,
        job: OwnedHandle,
        console: OwnedPseudoConsole,
        input: Option<std::fs::File>,
        reader: Option<std::thread::JoinHandle<()>>,
        cleanup_root: Option<PathBuf>,
        _runtime_files: Vec<OwnedHandle>,
        _attributes: OwnedAttributeList,
    }
    impl Drop for Session {
        fn drop(&mut self) {
            self.input.take();
            let _ = unsafe { TerminateJobObject(self.job.0, 1) };
            let _ = unsafe {
                WaitForSingleObject(self.process.0, TERMINATION_GRACE.as_millis() as u32)
            };
            let console = OwnedPseudoConsole(self.console.0.take());
            let reader = self.reader.take();
            let cleanup_root = self.cleanup_root.take();
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let cleanup = std::thread::Builder::new()
                .name("juice-cursor-conpty-cleanup".into())
                .spawn(move || {
                    drop(console);
                    if let Some(reader) = reader {
                        let _ = reader.join();
                    }
                    if let Some(cleanup_root) = cleanup_root {
                        let _ = fs::remove_dir_all(cleanup_root);
                    }
                    let _ = done_tx.send(());
                });
            if let Ok(cleanup) = cleanup {
                if done_rx.recv_timeout(TERMINATION_GRACE).is_ok() {
                    let _ = cleanup.join();
                }
            }
        }
    }

    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }

    fn lock_runtime_file(path: &Path, expected: FileIdentity) -> anyhow::Result<OwnedHandle> {
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let (handle, identity) = open_windows_file_identity(path, FILE_SHARE_READ)?;
        let handle = OwnedHandle(handle);
        if identity != expected {
            anyhow::bail!("Cursor Agent runtime file identity changed");
        }
        Ok(handle)
    }

    fn quoted(value: &Path) -> anyhow::Result<String> {
        let value = value
            .to_str()
            .filter(|value| !value.contains(['"', '\r', '\n']))
            .ok_or_else(|| anyhow::anyhow!("Cursor Agent path is not command-line safe"))?;
        Ok(format!("\"{value}\""))
    }

    ensure_before_deadline(deadline)?;
    let mut isolation = CursorIsolation::create(isolation_base)?;

    let mut input_read = OwnedHandle(HANDLE::default());
    let mut input_write = OwnedHandle(HANDLE::default());
    let mut output_read = OwnedHandle(HANDLE::default());
    let mut output_write = OwnedHandle(HANDLE::default());
    unsafe {
        CreatePipe(&mut input_read.0, &mut input_write.0, None, 0)?;
        CreatePipe(&mut output_read.0, &mut output_write.0, None, 0)?;
    }
    let console =
        unsafe { CreatePseudoConsole(COORD { X: 100, Y: 30 }, input_read.0, output_write.0, 0)? };
    drop(input_read);
    drop(output_write);
    let mut console = OwnedPseudoConsole(Some(console));

    let mut attribute_bytes = 0usize;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut attribute_bytes) };
    if attribute_bytes == 0 {
        anyhow::bail!("Cursor Agent pseudo console attribute size unavailable");
    }
    let words = attribute_bytes.div_ceil(std::mem::size_of::<usize>());
    let mut attribute_storage = vec![0usize; words];
    let attribute_list = LPPROC_THREAD_ATTRIBUTE_LIST(attribute_storage.as_mut_ptr().cast());
    unsafe {
        InitializeProcThreadAttributeList(Some(attribute_list), 1, None, &mut attribute_bytes)?;
    }
    let attributes = OwnedAttributeList {
        list: attribute_list,
        _storage: attribute_storage,
    };
    unsafe {
        UpdateProcThreadAttribute(
            attribute_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            console
                .0
                .as_ref()
                .map(|console| console.0 as *const std::ffi::c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )?;
    }

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.hStdInput = HANDLE::default();
    startup.StartupInfo.hStdOutput = HANDLE::default();
    startup.StartupInfo.hStdError = HANDLE::default();
    startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    startup.lpAttributeList = attribute_list;
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
    let job = OwnedHandle(job);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )?;
    }
    ensure_before_deadline(deadline)?;
    let mut process_info = PROCESS_INFORMATION::default();
    let application_name = wide(command.node.as_os_str());
    let current_directory = wide(isolation.workspace.as_os_str());
    let environment = cursor_environment_block(&isolation.home)?;
    let mut command_line = wide(
        format!(
            "{} {} --disable-auto-update --disable-project-configs --disable-indexing --disable-codebase-ref --exclude-workspace-context --data-dir {} --trust --workspace {}",
            quoted(&command.node)?,
            quoted(&command.index)?,
            quoted(&isolation.data)?,
            quoted(&isolation.workspace)?
        )
        .as_ref(),
    );
    revalidate_cursor_agent(command)?;
    let runtime_files = vec![
        lock_runtime_file(&command.node, command.node_identity)?,
        lock_runtime_file(&command.index, command.index_identity)?,
    ];
    ensure_before_deadline(deadline)?;
    unsafe {
        CreateProcessW(
            PCWSTR(application_name.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(current_directory.as_ptr()),
            std::ptr::from_ref(&startup).cast::<STARTUPINFOW>(),
            &mut process_info,
        )?;
    }
    let process = OwnedHandle(process_info.hProcess);
    let thread = OwnedHandle(process_info.hThread);
    let mut containment = UncontainedProcessGuard::new(process.0);
    unsafe {
        AssignProcessToJobObject(job.0, process.0)?;
        containment.disarm();
        if ResumeThread(thread.0) == u32::MAX {
            anyhow::bail!("Cursor Agent thread could not be resumed");
        }
    }
    drop(thread);

    let input_handle = input_write.take();
    let output_handle = output_read.take();
    let input = unsafe { std::fs::File::from_raw_handle(input_handle.0.cast()) };
    let mut output = unsafe { std::fs::File::from_raw_handle(output_handle.0.cast()) };
    let mut session = Session {
        process,
        job,
        console: OwnedPseudoConsole(console.0.take()),
        input: Some(input),
        reader: None,
        cleanup_root: Some(isolation.take_cleanup_root()),
        _runtime_files: runtime_files,
        _attributes: attributes,
    };

    let (output_tx, output_rx) = mpsc::sync_channel(32);
    session.reader = Some(std::thread::spawn(move || {
        let mut buffer = [0u8; READ_CHUNK_BYTES];
        let mut receiver_connected = true;
        loop {
            match output.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if receiver_connected && output_tx.send(Ok(buffer[..read].to_vec())).is_err() {
                        receiver_connected = false;
                    }
                }
                Err(error) => {
                    if receiver_connected {
                        let _ = output_tx.send(Err(error));
                    }
                    break;
                }
            }
        }
    }));

    let interaction_deadline = deadline
        .checked_sub(CLEANUP_RESERVE)
        .ok_or_else(|| anyhow::anyhow!("Cursor Agent /usage deadline unavailable"))?;
    ensure_before_deadline(interaction_deadline)?;
    let mut bytes = Vec::new();
    let mut usage_typed = false;
    let mut usage_selected = false;
    loop {
        if Instant::now() >= interaction_deadline {
            let text = strip_terminal_sequences(&bytes);
            anyhow::bail!(
                "Cursor Agent /usage timed out (bytes={}, banner={}, prompt={}, typed={}, menu={}, selected={}, complete={})",
                bytes.len(),
                text.contains("Cursor Agent"),
                text.contains("Plan, search, build anything"),
                usage_typed,
                text.contains("Show plan and on-demand usage"),
                usage_selected,
                text.contains("Esc to close")
            );
        }
        let remaining = interaction_deadline.saturating_duration_since(Instant::now());
        match output_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(Ok(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > MAX_OUTPUT_BYTES {
                    anyhow::bail!("Cursor Agent /usage output exceeded {MAX_OUTPUT_BYTES} bytes");
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Cursor Agent /usage output closed before completion")
            }
        }

        let text = strip_terminal_sequences(&bytes);
        let normalized = text.to_ascii_lowercase();
        if [
            "not authenticated",
            "not logged in",
            "please log in",
            "please login",
            "sign in required",
            "authentication required",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
        {
            anyhow::bail!("Cursor Agent authentication required");
        }
        if !usage_typed
            && text.contains("Cursor Agent")
            && text.contains("Plan, search, build anything")
        {
            session
                .input
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Cursor Agent input unavailable"))?
                .write_all(b"/usage\r")?;
            session.input.as_mut().unwrap().flush()?;
            usage_typed = true;
        }
        if usage_typed && !usage_selected && text.contains("Show plan and on-demand usage") {
            session.input.as_mut().unwrap().write_all(b"\r")?;
            session.input.as_mut().unwrap().flush()?;
            usage_selected = true;
        }
        if usage_selected && (text.contains("Esc to close") || usage_table_complete(&text)) {
            return Ok(text);
        }

        if unsafe { WaitForSingleObject(session.process.0, 0) } == WAIT_OBJECT_0 {
            let drain_deadline = std::cmp::min(
                Instant::now() + Duration::from_millis(250),
                interaction_deadline,
            );
            while Instant::now() < drain_deadline {
                match output_rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(Ok(chunk)) => {
                        if bytes.len().saturating_add(chunk.len()) <= MAX_OUTPUT_BYTES {
                            bytes.extend_from_slice(&chunk);
                        }
                    }
                    Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            let mut exit_code = 0u32;
            let _ = unsafe { GetExitCodeProcess(session.process.0, &mut exit_code) };
            let text = strip_terminal_sequences(&bytes);
            anyhow::bail!(
                "Cursor Agent exited before /usage completed (code={exit_code}, bytes={}, banner={}, prompt={})",
                bytes.len(),
                text.contains("Cursor Agent"),
                text.contains("Plan, search, build anything")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_cursor_version_directories() {
        assert!(version_sort_key("2026.08.11-e8db854").is_some());
        assert!(version_sort_key("2026.8.11-12-30-59-e8db854").is_some());
        for invalid in [
            "latest",
            "2026.08.11",
            "2026.08.11-nope",
            "2026.13.11-e8db854",
            "2026.08.11-25-00-00-e8db854",
            "2026.08.11-E8DB854",
        ] {
            assert!(version_sort_key(invalid).is_none());
        }
    }

    #[test]
    fn resolves_the_newest_complete_cursor_install() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-install-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let versions = root.join("cursor-agent").join("versions");
        for version in ["2026.9.30-aaaaaa", "2026.10.1-bbbbbb", "latest"] {
            let directory = versions.join(version);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("node.exe"), b"node").unwrap();
            if version != "2026.10.1-bbbbbb" || version == "latest" {
                std::fs::write(directory.join("index.js"), b"index").unwrap();
            }
        }
        std::fs::write(versions.join("2026.10.1-bbbbbb").join("index.js"), b"index").unwrap();

        let command = resolve_cursor_agent_from(&root).unwrap().unwrap();
        assert_eq!(command.version, "2026.10.1-bbbbbb");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolver_keeps_inspection_failures_distinct_from_a_missing_install() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-inspection-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("cursor-agent")).unwrap();
        std::fs::write(
            root.join("cursor-agent").join("versions"),
            b"not a directory",
        )
        .unwrap();

        assert!(resolve_cursor_agent_from(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();

        let missing = std::env::temp_dir().join(format!(
            "agent-juice-cursor-missing-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        assert_eq!(resolve_cursor_agent_from(&missing).unwrap(), None);
    }

    #[test]
    fn resolver_enforces_the_absolute_deadline_and_entry_budget() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-budget-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let versions = root.join("cursor-agent").join("versions");
        std::fs::create_dir_all(&versions).unwrap();
        assert!(resolve_cursor_agent_from_until(&root, Instant::now()).is_err());

        for index in 0..=MAX_VERSION_ENTRIES {
            std::fs::create_dir(versions.join(format!("invalid-{index:03}"))).unwrap();
        }
        assert!(resolve_cursor_agent_from(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn resolver_rejects_a_reparse_point_in_the_installation_root() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-reparse-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let cursor_root = root.join("cursor-agent");
        let real_versions = root.join("real-versions");
        std::fs::create_dir_all(&cursor_root).unwrap();
        std::fs::create_dir_all(&real_versions).unwrap();
        let versions = cursor_root.join("versions");
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&versions)
            .arg(&real_versions)
            .status()
            .unwrap();
        assert!(status.success());

        assert!(resolve_cursor_agent_from(&root).is_err());
        std::fs::remove_dir(&versions).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn isolated_cursor_runtime_is_unique_empty_and_removed_on_drop() {
        let base = std::env::temp_dir().join(format!(
            "agent-juice-cursor-isolation-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let first = CursorIsolation::create(&base).unwrap();
        let second = CursorIsolation::create(&base).unwrap();
        assert_ne!(first.root, second.root);
        assert!(first.workspace.is_dir());
        assert!(first.home.is_dir());
        assert!(first.data.is_dir());
        assert!(!first.workspace.join(".cursor").exists());
        let first_root = first.root.as_ref().unwrap().clone();
        drop(first);
        assert!(!first_root.exists());
        drop(second);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn cursor_child_environment_uses_an_allowlist_and_isolated_home() {
        let home = PathBuf::from(r"C:\isolated-cursor-home");
        let variables = cursor_environment_variables(
            &home,
            [
                (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
                (
                    OsString::from("PATH"),
                    OsString::from(r"C:\Windows\System32"),
                ),
                (OsString::from("OPENAI_API_KEY"), OsString::from("secret")),
                (OsString::from("GITHUB_TOKEN"), OsString::from("secret")),
                (OsString::from("HOME"), OsString::from(r"C:\real-home")),
            ],
        );
        let values = variables
            .into_iter()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(values.get("SystemRoot").unwrap(), r"C:\Windows");
        assert_eq!(values.get("PATH").unwrap(), r"C:\Windows\System32");
        assert_eq!(values.get("HOME").unwrap(), home.as_os_str());
        assert_eq!(values.get("USERPROFILE").unwrap(), home.as_os_str());
        assert!(!values.contains_key("OPENAI_API_KEY"));
        assert!(!values.contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn spawn_revalidation_rejects_an_index_outside_the_version_directory() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-revalidate-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let version = "2026.8.11-abcdef0";
        let directory = root.join("cursor-agent").join("versions").join(version);
        std::fs::create_dir_all(&directory).unwrap();
        let node = directory.join("node.exe");
        let index = root.join("outside.js");
        std::fs::write(&node, b"node").unwrap();
        std::fs::write(&index, b"index").unwrap();

        let command = CursorAgentCommand {
            node: node.clone(),
            index: index.clone(),
            version: version.into(),
            node_identity: query_file_identity(&node).unwrap(),
            index_identity: query_file_identity(&index).unwrap(),
        };
        assert!(revalidate_cursor_agent(&command).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn spawn_revalidation_rejects_a_replaced_runtime_file() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-replaced-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let directory = root
            .join("cursor-agent")
            .join("versions")
            .join("2026.8.11-abcdef0");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("node.exe"), b"node").unwrap();
        std::fs::write(directory.join("index.js"), b"index-one").unwrap();
        let command = resolve_cursor_agent_from(&root).unwrap().unwrap();

        std::fs::remove_file(&command.index).unwrap();
        std::fs::write(&command.index, b"index-two").unwrap();
        assert!(revalidate_cursor_agent(&command).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn runtime_identity_handle_blocks_replacement_until_collection_cleanup() {
        use windows::Win32::{Foundation::CloseHandle, Storage::FileSystem::FILE_SHARE_READ};

        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-lock-{}-{}",
            std::process::id(),
            ISOLATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("index.js");
        std::fs::write(&file, b"runtime").unwrap();
        let (handle, _) = open_windows_file_identity(&file, FILE_SHARE_READ).unwrap();

        assert!(std::fs::remove_file(&file).is_err());
        let _ = unsafe { CloseHandle(handle) };
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn uncontained_process_guard_terminates_a_child_on_scope_error() {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;

        let mut child = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ping -n 30 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        let guard = UncontainedProcessGuard::new(HANDLE(child.as_raw_handle()));
        drop(guard);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && child.try_wait().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn strips_csi_osc_and_control_sequences() {
        let raw = b"\x1b[2JCursor Agent\r\n\x1b]8;;https://cursor.com\x1b\\Usage\x1b]8;;\x1b\\\r\n  Auto 1% used\x1b[K";
        let text = strip_terminal_sequences(raw);
        assert!(text.contains("Cursor Agent"));
        assert!(text.contains("Usage"));
        assert!(text.contains("Auto 1% used"));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn usage_completion_requires_both_real_pool_rows() {
        assert!(!usage_table_complete(
            "/usage Show plan and on-demand usage Auto"
        ));
        assert!(usage_table_complete("Auto 1% used\nAPI 0% used"));
        assert!(usage_table_complete(
            "Cursor Models 2% used\nOther Models 3% used"
        ));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a locally installed and logged-in Cursor Agent"]
    fn live_conpty_exposes_tty_handles_to_cursor_node() {
        let workspace = std::env::temp_dir().join("agent-juice-cursor-pty-live");
        std::fs::create_dir_all(&workspace).unwrap();
        let version_directory = workspace
            .join("install")
            .join("cursor-agent")
            .join("versions")
            .join("2026.8.11-abcdef0");
        std::fs::create_dir_all(&version_directory).unwrap();
        let installed = resolve_cursor_agent().unwrap();
        std::fs::hard_link(&installed.node, version_directory.join("node.exe")).unwrap();
        let script = version_directory.join("index.js");
        std::fs::write(
            &script,
            r#"
console.log(`TTY stdin=${process.stdin.isTTY} stdout=${process.stdout.isTTY} stderr=${process.stderr.isTTY}`);
console.log(`ISOLATED_HOME=${process.env.HOME === process.env.USERPROFILE && process.env.HOME.includes('run-')}`);
console.log(`SAFE_FLAGS=${['--disable-project-configs', '--disable-indexing', '--disable-codebase-ref', '--exclude-workspace-context', '--data-dir'].every((flag) => process.argv.includes(flag))}`);
console.log('Cursor Agent');
console.log('Plan, search, build anything');
process.stdin.setEncoding('utf8');
let commandSeen = false;
process.stdin.on('data', (chunk) => {
  if (!commandSeen && chunk.includes('/usage')) {
    commandSeen = true;
    console.log('Show plan and on-demand usage');
    return;
  }
  if (commandSeen && chunk.includes('\r')) {
    console.log('Usage Resets Sep 21');
    console.log('Auto 1% used');
    console.log('API 0% used');
    console.log('Esc to close');
    process.stdout.write('x'.repeat(128 * 1024));
  }
});
setTimeout(() => process.exit(2), 15000);
"#,
        )
        .unwrap();
        let command = resolve_cursor_agent_from(&workspace.join("install"))
            .unwrap()
            .unwrap();
        let output = capture_cursor_usage_with_command(
            &command,
            &workspace,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert!(output.contains("TTY stdin=true stdout=true stderr=true"));
        assert!(output.contains("ISOLATED_HOME=true"));
        assert!(output.contains("SAFE_FLAGS=true"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a locally installed and logged-in Cursor Agent"]
    fn live_cursor_usage_round_trip() {
        let workspace = std::env::temp_dir().join("agent-juice-cursor-usage-live");
        let output = capture_cursor_usage(&workspace, Duration::from_secs(20)).unwrap();
        assert!(output.contains("Usage"));
        assert!(output.contains("Auto"));
        assert!(output.contains("API"));
        assert!(output.contains("Resets"));
        assert!(output.contains("Esc to close"));
        let _ = std::fs::remove_dir_all(workspace);
    }
}
