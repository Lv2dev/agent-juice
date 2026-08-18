use crate::paths;
use chrono::{DateTime, Duration, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

const RELEASES_URL: &str = "https://github.com/Lv2dev/agent-juice/releases";
const CHECK_INTERVAL_HOURS: i64 = 24;
pub const MAX_UPDATE_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;
#[cfg(windows)]
const WINDOWS_NSIS_UPDATE_PARAMETERS: &str = "/P /UPDATE";
#[cfg(windows)]
const UPDATE_HELPER_MARKER: &str = "--juice-update-helper";
#[cfg(windows)]
const UPDATE_PARENT_EXIT_TIMEOUT_MS: u32 = 30_000;
#[cfg(windows)]
const UPDATE_HELPER_READY_TIMEOUT_MS: u32 = 5_000;
#[cfg(windows)]
const UPDATE_INSTALL_TIMEOUT_SECS: u64 = 300;
#[cfg(windows)]
const UPDATE_PROCESS_CLEANUP_SECS: u64 = 2;
#[cfg(windows)]
const UPDATE_APP_START_VERIFY_MS: u64 = 1_500;
#[cfg(windows)]
const UPDATE_TEMP_STALE_SECS: u64 = 10 * 60;
#[cfg(windows)]
const UPDATE_TEMP_CLEANUP_ENTRY_LIMIT: usize = 256;
#[cfg(windows)]
const UPDATE_STATUSLINE_HELPER_NAME: &str = "agentjuice-statusline.exe";
#[cfg(windows)]
const UPDATE_STATUSLINE_QUARANTINE_NAME: &str = "agentjuice-statusline.juice-update-old.exe";
#[cfg(windows)]
const UPDATE_STATUSLINE_RENAME_ATTEMPTS: usize = 200;
#[cfg(windows)]
const UPDATE_STATUSLINE_RENAME_RETRY_MS: u64 = 50;

static UPDATE_STATE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static PENDING_NOTIFICATIONS: Lazy<Mutex<HashSet<(PathBuf, String)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdateState {
    pub last_checked_at: Option<String>,
    pub latest_release: Option<ReleaseInfo>,
    pub last_notified_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub status: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub checked_at: Option<String>,
    pub checked_now: bool,
    pub error: Option<String>,
}

pub struct PreparedNotification {
    path: PathBuf,
    version: String,
    pending: bool,
}

impl PreparedNotification {
    pub fn commit(mut self) -> anyhow::Result<bool> {
        let result = commit_notification_at(&self.path, &self.version);
        self.release_pending();
        result
    }

    fn release_pending(&mut self) {
        if self.pending {
            PENDING_NOTIFICATIONS
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .remove(&(self.path.clone(), self.version.clone()));
            self.pending = false;
        }
    }
}

impl Drop for PreparedNotification {
    fn drop(&mut self) {
        self.release_pending();
    }
}

pub fn state_path() -> Option<PathBuf> {
    paths::data_dir().map(|dir| dir.join("update-state.json"))
}

pub fn releases_url() -> &'static str {
    RELEASES_URL
}

pub fn release_info_for_version(version: &str) -> anyhow::Result<ReleaseInfo> {
    let version = version.strip_prefix('v').unwrap_or(version);
    parse_version(version).ok_or_else(|| anyhow::anyhow!("invalid release version"))?;
    Ok(ReleaseInfo {
        version: version.to_string(),
        url: format!("{RELEASES_URL}/tag/v{version}"),
    })
}

pub fn updater_asset_url_for_version(version: &str) -> anyhow::Result<String> {
    let version = release_info_for_version(version)?.version;
    Ok(format!(
        "{RELEASES_URL}/download/v{version}/Juice_{version}_x64-setup.exe"
    ))
}

pub fn is_updater_asset_url_allowed(url: &str, version: &str) -> bool {
    updater_asset_url_for_version(version).is_ok_and(|expected| url == expected)
}

pub fn update_package_size_is_allowed(downloaded: u64, content_length: Option<u64>) -> bool {
    downloaded <= MAX_UPDATE_PACKAGE_BYTES
        && content_length.is_none_or(|length| length <= MAX_UPDATE_PACKAGE_BYTES)
}

#[cfg(windows)]
pub fn prepare_verified_installer(bytes: &[u8], version: &str) -> anyhow::Result<PathBuf> {
    let expected =
        parse_version(version).ok_or_else(|| anyhow::anyhow!("invalid update version"))?;
    if bytes.len() < 2 || &bytes[..2] != b"MZ" {
        anyhow::bail!("invalid Windows updater format");
    }

    let directory = update_staging_directory();
    std::fs::create_dir_all(&directory)?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let stem = format!("Juice_{version}_{}_{}", std::process::id(), sequence);
    let partial = directory.join(format!("{stem}.partial"));
    let installer = directory.join(format!("{stem}_x64-setup.exe"));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&partial, &installer)
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&partial);
        let _ = std::fs::remove_file(&installer);
        return Err(error.into());
    }

    let actual = installer_product_version(&installer);
    if actual.as_ref().map_or(true, |actual| *actual != expected) {
        let _ = std::fs::remove_file(&installer);
        anyhow::bail!("updater ProductVersion does not match the manifest");
    }
    Ok(installer)
}

#[cfg(windows)]
fn update_staging_directory() -> PathBuf {
    update_staging_directory_for(paths::data_dir(), &std::env::temp_dir())
}

#[cfg(windows)]
fn update_staging_directory_for(data_dir: Option<PathBuf>, temp_dir: &Path) -> PathBuf {
    data_dir
        .unwrap_or_else(|| temp_dir.join("Juice"))
        .join("updates")
}

#[cfg(windows)]
pub fn spawn_update_helper(installer: &Path, version: &str) -> anyhow::Result<()> {
    use std::{
        os::windows::{ffi::OsStrExt, process::CommandExt},
        process::Stdio,
    };
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{CreateEventW, WaitForSingleObject, CREATE_NO_WINDOW},
        },
    };

    let version = release_info_for_version(version)?.version;
    let app_exe = std::env::current_exe()?;
    let directory = installer
        .parent()
        .ok_or_else(|| anyhow::anyhow!("updater has no parent directory"))?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let helper = directory.join(format!(
        "Juice_update_helper_{}_{}.exe",
        std::process::id(),
        sequence
    ));
    let backup = directory.join(format!(
        "Juice_update_backup_{}_{}",
        std::process::id(),
        sequence
    ));
    let ready_event_name = format!(
        r"Local\AgentJuiceUpdaterReady_{}_{}",
        std::process::id(),
        sequence
    );
    let ready_event_name_wide = std::ffi::OsStr::new(&ready_event_name)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let ready_event =
        unsafe { CreateEventW(None, false, false, PCWSTR(ready_event_name_wide.as_ptr())) }
            .map_err(|error| anyhow::anyhow!("could not create helper ready event: {error}"))?;
    let prepare_result = (|| -> anyhow::Result<()> {
        std::fs::copy(&app_exe, &helper)
            .map_err(|error| anyhow::anyhow!("could not copy update helper: {error}"))?;
        sync_handoff_file(&helper)
            .map_err(|error| anyhow::anyhow!("could not sync update helper: {error}"))?;
        create_update_backup(&app_exe, &backup).map_err(|error| {
            anyhow::anyhow!("could not back up the current installation: {error}")
        })?;
        Ok(())
    })();
    if let Err(error) = prepare_result {
        let _ = unsafe { CloseHandle(ready_event) };
        let _ = std::fs::remove_file(&helper);
        let _ = std::fs::remove_dir_all(&backup);
        return Err(error);
    }

    let result = std::process::Command::new(&helper)
        .arg(UPDATE_HELPER_MARKER)
        .arg(installer)
        .arg(std::process::id().to_string())
        .arg(&app_exe)
        .arg(&version)
        .arg(&backup)
        .arg(&ready_event_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn();
    let mut child = match result {
        Ok(child) => child,
        Err(error) => {
            let _ = unsafe { CloseHandle(ready_event) };
            let _ = std::fs::remove_file(&helper);
            let _ = std::fs::remove_dir_all(&backup);
            return Err(anyhow::anyhow!("could not execute update helper: {error}"));
        }
    };
    let ready = unsafe { WaitForSingleObject(ready_event, UPDATE_HELPER_READY_TIMEOUT_MS) };
    let _ = unsafe { CloseHandle(ready_event) };
    if ready != WAIT_OBJECT_0 {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&helper);
        let _ = std::fs::remove_dir_all(&backup);
        anyhow::bail!("update helper did not become ready");
    }
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            let _ = std::fs::remove_file(&helper);
            let _ = std::fs::remove_dir_all(&backup);
            anyhow::bail!("update helper exited before handoff with {status}");
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&helper);
            let _ = std::fs::remove_dir_all(&backup);
            return Err(anyhow::anyhow!(
                "could not confirm update helper readiness: {error}"
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn update_helper_exit_code() -> Option<i32> {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(UPDATE_HELPER_MARKER)) {
        return None;
    }
    let result = parse_update_helper_args(args).and_then(|args| run_update_helper(&args));
    if let Err(error) = &result {
        eprintln!("[update-helper] {error}");
    }
    Some(if result.is_ok() { 0 } else { 1 })
}

#[cfg(not(windows))]
pub fn update_helper_exit_code() -> Option<i32> {
    None
}

#[cfg(windows)]
struct UpdateHelperArgs {
    installer: PathBuf,
    parent_pid: u32,
    app_exe: PathBuf,
    expected_version: String,
    backup_dir: PathBuf,
    ready_event_name: String,
}

#[cfg(windows)]
fn parse_update_helper_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> anyhow::Result<UpdateHelperArgs> {
    let installer = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing updater installer path"))?;
    let parent_pid = args
        .next()
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .ok_or_else(|| anyhow::anyhow!("invalid updater parent process"))?;
    let app_exe = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing updater recovery executable"))?;
    let expected_version = args
        .next()
        .and_then(|value| value.to_str().map(str::to_owned))
        .and_then(|value| {
            release_info_for_version(&value)
                .ok()
                .map(|item| item.version)
        })
        .ok_or_else(|| anyhow::anyhow!("invalid updater expected version"))?;
    let backup_dir = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing updater backup directory"))?;
    let ready_event_name = args
        .next()
        .and_then(|value| value.to_str().map(str::to_owned))
        .filter(|value| value.starts_with(r"Local\AgentJuiceUpdaterReady_"))
        .ok_or_else(|| anyhow::anyhow!("invalid updater ready event"))?;
    if args.next().is_some() {
        anyhow::bail!("unexpected updater helper argument");
    }
    Ok(UpdateHelperArgs {
        installer,
        parent_pid,
        app_exe,
        expected_version,
        backup_dir,
        ready_event_name,
    })
}

#[cfg(windows)]
fn run_update_helper(args: &UpdateHelperArgs) -> anyhow::Result<()> {
    let helper_exe = std::env::current_exe()?;
    let parent = open_parent_and_signal_ready(args.parent_pid, &args.ready_event_name)?;
    if let Err(error) = wait_for_process_exit(parent) {
        let _ = std::fs::remove_file(&args.installer);
        let _ = std::fs::remove_dir_all(&args.backup_dir);
        schedule_file_deletion(&helper_exe);
        return Err(error);
    }
    let statusline_quarantine = match quarantine_statusline_for_update(&args.app_exe) {
        Ok(path) => path,
        Err(error) => {
            let restart = restart_app_and_confirm(&args.app_exe);
            let _ = std::fs::remove_file(&args.installer);
            let _ = std::fs::remove_dir_all(&args.backup_dir);
            schedule_file_deletion(&helper_exe);
            return match restart {
                Ok(()) => Err(error),
                Err(restart_error) => Err(anyhow::anyhow!(
                    "could not quarantine the Claude statusline ({error}); restart failed ({restart_error})"
                )),
            };
        }
    };
    let status = run_installer_and_wait(&args.installer);
    let result = finish_installer_handoff(status, || {
        verify_installed_version(&args.app_exe, &args.expected_version)?;
        cleanup_statusline_quarantine(&statusline_quarantine);
        restart_app_and_confirm(&args.app_exe)
    });
    let result = match result {
        Ok(()) => Ok(()),
        Err(install_error) => restore_and_restart(
            &helper_exe,
            &args.backup_dir,
            &args.app_exe,
            &statusline_quarantine,
        )
        .map_err(|recovery_error| {
            anyhow::anyhow!("update failed ({install_error}); recovery failed ({recovery_error})")
        }),
    };
    if result.is_ok() {
        let _ = std::fs::remove_file(&args.installer);
        let _ = std::fs::remove_dir_all(&args.backup_dir);
        schedule_file_deletion(&helper_exe);
    }
    result
}

#[cfg(windows)]
fn finish_installer_handoff(
    status: std::io::Result<std::process::ExitStatus>,
    complete: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match status {
        Ok(status) if status.success() => complete(),
        Ok(status) => anyhow::bail!("updater installer exited with {status}"),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn open_parent_and_signal_ready(
    pid: u32,
    event_name: &str,
) -> anyhow::Result<windows::Win32::Foundation::HANDLE> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenEventW, OpenProcess, SetEvent, EVENT_MODIFY_STATE, PROCESS_SYNCHRONIZE,
            },
        },
    };

    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }?;
    let event_name = std::ffi::OsStr::new(event_name)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let ready_event =
        match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_name.as_ptr())) } {
            Ok(event) => event,
            Err(error) => {
                let _ = unsafe { CloseHandle(parent) };
                return Err(error.into());
            }
        };
    let signal = unsafe { SetEvent(ready_event) };
    let _ = unsafe { CloseHandle(ready_event) };
    if let Err(error) = signal {
        let _ = unsafe { CloseHandle(parent) };
        return Err(error.into());
    }
    Ok(parent)
}

#[cfg(windows)]
fn wait_for_process_exit(handle: windows::Win32::Foundation::HANDLE) -> anyhow::Result<()> {
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };

    let wait = unsafe { WaitForSingleObject(handle, UPDATE_PARENT_EXIT_TIMEOUT_MS) };
    unsafe { CloseHandle(handle) }?;
    if wait == WAIT_OBJECT_0 {
        Ok(())
    } else if wait == WAIT_TIMEOUT {
        anyhow::bail!("timed out waiting for Juice to exit")
    } else {
        anyhow::bail!("failed while waiting for Juice to exit")
    }
}

#[cfg(windows)]
fn run_installer_and_wait(installer: &Path) -> std::io::Result<std::process::ExitStatus> {
    use std::{os::windows::process::CommandExt, time::Instant};
    use windows::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    let tree = UpdateProcessTree::create().map_err(std::io::Error::other)?;
    let mut child = std::process::Command::new(installer)
        .args(WINDOWS_NSIS_UPDATE_PARAMETERS.split_ascii_whitespace())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0 | CREATE_SUSPENDED.0)
        .spawn()?;
    if let Err(error) = tree.assign(&child).and_then(|()| tree.resume(&child)) {
        tree.terminate();
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other(error));
    }

    let deadline = Instant::now() + std::time::Duration::from_secs(UPDATE_INSTALL_TIMEOUT_SECS);
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            None => {
                tree.terminate();
                let cleanup_deadline =
                    Instant::now() + std::time::Duration::from_secs(UPDATE_PROCESS_CLEANUP_SECS);
                while child.try_wait()?.is_none() && Instant::now() < cleanup_deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "updater installer timed out",
                ));
            }
        }
    }
}

#[cfg(windows)]
struct UpdateProcessTree(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl UpdateProcessTree {
    fn create() -> anyhow::Result<Self> {
        use windows::{
            core::PCWSTR,
            Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
        };

        let job = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
        let tree = Self(job);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                tree.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )?;
        }
        Ok(tree)
    }

    fn assign(&self, child: &std::process::Child) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::AssignProcessToJobObject};

        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle()))? };
        Ok(())
    }

    fn resume(&self, child: &std::process::Child) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;

        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtResumeProcess(process_handle: *mut std::ffi::c_void) -> i32;
        }

        let status = unsafe { NtResumeProcess(child.as_raw_handle()) };
        if status < 0 {
            anyhow::bail!("NtResumeProcess failed with NTSTATUS 0x{status:08x}");
        }
        Ok(())
    }

    fn terminate(&self) {
        let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
    }
}

#[cfg(windows)]
impl Drop for UpdateProcessTree {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn verify_installed_version(app_exe: &Path, expected_version: &str) -> anyhow::Result<()> {
    let expected = parse_version(expected_version)
        .ok_or_else(|| anyhow::anyhow!("invalid expected installed version"))?;
    if installer_product_version(app_exe)? != expected {
        anyhow::bail!("installed updater version does not match the manifest");
    }
    Ok(())
}

#[cfg(windows)]
fn restart_app_and_confirm(app_exe: &Path) -> anyhow::Result<()> {
    let mut child = std::process::Command::new(app_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::sleep(std::time::Duration::from_millis(UPDATE_APP_START_VERIFY_MS));
    match child.try_wait()? {
        None => Ok(()),
        Some(status) => anyhow::bail!("updated Juice exited during startup with {status}"),
    }
}

#[cfg(windows)]
fn create_update_backup(app_exe: &Path, backup_dir: &Path) -> anyhow::Result<()> {
    let app_dir = app_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed Juice has no parent directory"))?;
    std::fs::create_dir(backup_dir)?;
    let result = (|| -> anyhow::Result<()> {
        let mut backed_up_app = false;
        let owned_files = [
            app_exe.to_path_buf(),
            app_dir.join("agentjuice-statusline.exe"),
            app_dir.join("uninstall.exe"),
        ];
        for source in owned_files {
            if !source.is_file() {
                continue;
            }
            let file_name = source
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("installed Juice file has no name"))?;
            let destination = backup_dir.join(file_name);
            std::fs::copy(&source, &destination)?;
            sync_handoff_file(&destination)?;
            backed_up_app |= source == app_exe;
        }
        if !backed_up_app {
            anyhow::bail!("current Juice executable was not backed up");
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(backup_dir);
    }
    result
}

#[cfg(windows)]
fn restore_and_restart(
    helper_exe: &Path,
    backup_dir: &Path,
    app_exe: &Path,
    statusline_quarantine: &Path,
) -> anyhow::Result<()> {
    let restore = restore_update_backup(backup_dir, app_exe).and_then(|()| {
        recover_statusline_quarantine(app_exe)?;
        cleanup_statusline_quarantine(statusline_quarantine);
        let expected = installer_product_version(helper_exe)?;
        if installer_product_version(app_exe)? != expected {
            anyhow::bail!("recovered Juice version does not match the previous installation");
        }
        Ok(())
    });
    restart_after_successful_restore(restore, || restart_app_and_confirm(app_exe))
}

#[cfg(windows)]
fn restart_after_successful_restore(
    restore: anyhow::Result<()>,
    restart: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    restore?;
    restart()
}

#[cfg(windows)]
fn restore_update_backup(backup_dir: &Path, app_exe: &Path) -> anyhow::Result<()> {
    let app_dir = app_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed Juice has no parent directory"))?;
    std::fs::create_dir_all(app_dir)?;
    let app_name = app_exe
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("installed Juice has no file name"))?;
    let mut entries = std::fs::read_dir(backup_dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.retain(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()));
    entries.sort_by_key(|entry| entry.file_name() == app_name);

    let mut restored_app = false;
    let mut first_error = None;
    for entry in entries {
        let destination = app_dir.join(entry.file_name());
        restored_app |= destination == app_exe;
        if let Err(error) = replace_file_from_backup(&entry.path(), &destination) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if !restored_app {
        anyhow::bail!("backup does not contain the previous Juice executable");
    }
    if let Some(error) = first_error {
        return Err(error.into());
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file_from_backup(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        },
    };

    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let recovery = destination.with_extension(format!(
        "juice-recovery-{}-{sequence}.tmp",
        std::process::id()
    ));
    std::fs::copy(source, &recovery)?;
    sync_handoff_file(&recovery)?;
    let recovery_wide = recovery
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            PCWSTR(recovery_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result.is_err() {
        let _ = std::fs::remove_file(recovery);
    }
    result.map_err(|error| std::io::Error::other(error.to_string()))
}

#[cfg(windows)]
fn sync_handoff_file(path: &Path) -> std::io::Result<()> {
    OpenOptions::new().write(true).open(path)?.sync_all()
}

#[cfg(windows)]
fn statusline_update_paths(app_exe: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let app_dir = app_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed Juice has no parent directory"))?;
    Ok((
        app_dir.join(UPDATE_STATUSLINE_HELPER_NAME),
        app_dir.join(UPDATE_STATUSLINE_QUARANTINE_NAME),
    ))
}

#[cfg(windows)]
fn retry_statusline_operation(
    attempts: usize,
    mut operation: impl FnMut() -> std::io::Result<()>,
    mut wait: impl FnMut(),
) -> std::io::Result<()> {
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 == attempts => return Err(error),
            Err(_) => wait(),
        }
    }
    unreachable!("at least one statusline operation attempt is required")
}

#[cfg(windows)]
fn recover_statusline_quarantine(app_exe: &Path) -> anyhow::Result<()> {
    recover_statusline_quarantine_with(app_exe, |statusline, app| {
        installer_product_version(statusline)
            .and_then(|statusline_version| {
                installer_product_version(app).map(|app_version| statusline_version == app_version)
            })
            .unwrap_or(false)
    })
}

#[cfg(windows)]
fn recover_statusline_quarantine_with(
    app_exe: &Path,
    canonical_is_valid: impl FnOnce(&Path, &Path) -> bool,
) -> anyhow::Result<()> {
    let (statusline, quarantine) = statusline_update_paths(app_exe)?;
    if statusline.is_file() && quarantine.is_file() {
        if canonical_is_valid(&statusline, app_exe) {
            retry_statusline_operation(
                UPDATE_STATUSLINE_RENAME_ATTEMPTS,
                || std::fs::remove_file(&quarantine),
                || {
                    std::thread::sleep(std::time::Duration::from_millis(
                        UPDATE_STATUSLINE_RENAME_RETRY_MS,
                    ));
                },
            )?;
        } else {
            retry_statusline_operation(
                UPDATE_STATUSLINE_RENAME_ATTEMPTS,
                || std::fs::remove_file(&statusline),
                || {
                    std::thread::sleep(std::time::Duration::from_millis(
                        UPDATE_STATUSLINE_RENAME_RETRY_MS,
                    ));
                },
            )?;
            retry_statusline_operation(
                UPDATE_STATUSLINE_RENAME_ATTEMPTS,
                || std::fs::rename(&quarantine, &statusline),
                || {
                    std::thread::sleep(std::time::Duration::from_millis(
                        UPDATE_STATUSLINE_RENAME_RETRY_MS,
                    ));
                },
            )?;
        }
    } else if !statusline.exists() && quarantine.is_file() {
        retry_statusline_operation(
            UPDATE_STATUSLINE_RENAME_ATTEMPTS,
            || std::fs::rename(&quarantine, &statusline),
            || {
                std::thread::sleep(std::time::Duration::from_millis(
                    UPDATE_STATUSLINE_RENAME_RETRY_MS,
                ));
            },
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn quarantine_statusline_with(
    app_exe: &Path,
    attempts: usize,
    mut move_file: impl FnMut(&Path, &Path) -> std::io::Result<()>,
    wait: impl FnMut(),
) -> anyhow::Result<PathBuf> {
    recover_statusline_quarantine(app_exe)?;
    let (statusline, quarantine) = statusline_update_paths(app_exe)?;
    if statusline.is_file() {
        retry_statusline_operation(attempts, || move_file(&statusline, &quarantine), wait)?;
    }
    Ok(quarantine)
}

#[cfg(windows)]
fn quarantine_statusline_for_update(app_exe: &Path) -> anyhow::Result<PathBuf> {
    quarantine_statusline_with(
        app_exe,
        UPDATE_STATUSLINE_RENAME_ATTEMPTS,
        |source, destination| std::fs::rename(source, destination),
        || {
            std::thread::sleep(std::time::Duration::from_millis(
                UPDATE_STATUSLINE_RENAME_RETRY_MS,
            ));
        },
    )
}

#[cfg(windows)]
fn cleanup_statusline_quarantine(path: &Path) {
    if path.exists() && std::fs::remove_file(path).is_err() {
        schedule_file_deletion(path);
    }
}

#[cfg(windows)]
fn schedule_file_deletion(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT},
    };

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let _ = unsafe {
        MoveFileExW(
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };
}

#[cfg(windows)]
pub fn start_update_temp_cleanup() {
    if let Ok(app_exe) = std::env::current_exe() {
        if let Err(error) = recover_statusline_quarantine(&app_exe) {
            eprintln!("[update] could not recover statusline quarantine: {error}");
        }
    }
    let _ = std::thread::Builder::new()
        .name("juice-update-temp-cleanup".into())
        .stack_size(256 * 1024)
        .spawn(|| {
            cleanup_stale_update_files_at(std::time::SystemTime::now());
            std::thread::sleep(std::time::Duration::from_secs(UPDATE_TEMP_STALE_SECS + 60));
            cleanup_stale_update_files_at(std::time::SystemTime::now());
        });
}

#[cfg(windows)]
fn cleanup_stale_update_files_at(now: std::time::SystemTime) {
    cleanup_stale_update_files_in(&update_staging_directory(), now);
    let legacy_directory = std::env::temp_dir().join("Juice-updates");
    if legacy_directory != update_staging_directory() {
        cleanup_stale_update_files_in(&legacy_directory, now);
    }
}

#[cfg(windows)]
fn cleanup_stale_update_files_in(directory: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.take(UPDATE_TEMP_CLEANUP_ENTRY_LIMIT).flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() || !is_cleanup_candidate(&entry.file_name()) {
            continue;
        }
        let is_stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() >= UPDATE_TEMP_STALE_SECS);
        if is_stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(windows)]
fn is_cleanup_candidate(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    (name.starts_with("Juice_update_helper_") && name.ends_with(".exe"))
        || (name.starts_with("Juice_") && name.ends_with("_x64-setup.exe"))
        || (name.starts_with("Juice_") && name.ends_with(".partial"))
}

#[cfg(windows)]
fn installer_product_version(path: &Path) -> anyhow::Result<(u64, u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::{w, PCWSTR},
        Win32::Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
        },
    };

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
    if size == 0 {
        anyhow::bail!("updater has no Windows version metadata");
    }
    let mut data = vec![0_u8; size as usize];
    unsafe { GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr().cast()) }?;

    let mut value = std::ptr::null_mut();
    let mut value_len = 0_u32;
    let found =
        unsafe { VerQueryValueW(data.as_ptr().cast(), w!("\\"), &mut value, &mut value_len) };
    if !found.as_bool()
        || value.is_null()
        || value_len < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
    {
        anyhow::bail!("updater has invalid Windows version metadata");
    }
    let info = unsafe { &*value.cast::<VS_FIXEDFILEINFO>() };
    if info.dwSignature != 0xFEEF04BD {
        anyhow::bail!("updater has invalid Windows version signature");
    }
    Ok((
        u64::from(info.dwProductVersionMS >> 16),
        u64::from(info.dwProductVersionMS & 0xffff),
        u64::from(info.dwProductVersionLS >> 16),
    ))
}

pub fn is_update_available(current: &str, latest: &str) -> anyhow::Result<bool> {
    let current =
        parse_version(current).ok_or_else(|| anyhow::anyhow!("invalid current version"))?;
    let latest = parse_version(latest).ok_or_else(|| anyhow::anyhow!("invalid latest version"))?;
    Ok(latest > current)
}

pub fn is_release_url_allowed(url: &str) -> bool {
    if url == RELEASES_URL || url == format!("{RELEASES_URL}/latest") {
        return true;
    }

    let Some(tag) = url.strip_prefix(&format!("{RELEASES_URL}/tag/")) else {
        return false;
    };
    !tag.is_empty() && !tag.contains(['/', '\\', '?', '#', '@']) && parse_version(tag).is_some()
}

pub fn load_state() -> UpdateState {
    state_path()
        .as_deref()
        .map(load_state_from)
        .unwrap_or_default()
}

pub fn load_state_from(path: &Path) -> UpdateState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn cached_result(current_version: &str) -> UpdateCheckResult {
    result_from_state(current_version, &load_state(), false)
}

pub fn update_check_is_due(force: bool) -> bool {
    state_path()
        .as_deref()
        .map(|path| update_check_is_due_at(path, force, Utc::now()))
        .unwrap_or(true)
}

pub fn update_check_is_due_at(path: &Path, force: bool, now: DateTime<Utc>) -> bool {
    force || check_is_due(&load_state_from(path), now)
}

pub fn record_update_check(
    current_version: &str,
    available_version: Option<&str>,
) -> anyhow::Result<UpdateCheckResult> {
    let path = state_path().ok_or_else(|| anyhow::anyhow!("no update state path"))?;
    record_update_check_at(&path, current_version, available_version, Utc::now())
}

pub fn record_update_check_at(
    path: &Path,
    current_version: &str,
    available_version: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<UpdateCheckResult> {
    parse_version(current_version).ok_or_else(|| anyhow::anyhow!("invalid current version"))?;
    let _guard = UPDATE_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let mut state = load_state_from(path);
    let release = release_info_for_version(available_version.unwrap_or(current_version))?;
    state.last_checked_at = Some(now.to_rfc3339());
    state.latest_release = Some(release);
    save_state_to(path, &state)?;
    Ok(result_from_state(current_version, &state, true))
}

pub fn prepare_notification(version: &str) -> anyhow::Result<Option<PreparedNotification>> {
    let path = state_path().ok_or_else(|| anyhow::anyhow!("no update state path"))?;
    prepare_notification_at(&path, version)
}

pub fn prepare_notification_at(
    path: &Path,
    version: &str,
) -> anyhow::Result<Option<PreparedNotification>> {
    parse_version(version).ok_or_else(|| anyhow::anyhow!("invalid notification version"))?;
    let _guard = UPDATE_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let state = load_state_from(path);
    if !notification_is_newer(state.last_notified_version.as_deref(), version) {
        return Ok(None);
    }

    let key = (path.to_path_buf(), version.to_string());
    if !PENDING_NOTIFICATIONS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(key)
    {
        return Ok(None);
    }
    Ok(Some(PreparedNotification {
        path: path.to_path_buf(),
        version: version.to_string(),
        pending: true,
    }))
}

fn commit_notification_at(path: &Path, version: &str) -> anyhow::Result<bool> {
    parse_version(version).ok_or_else(|| anyhow::anyhow!("invalid notification version"))?;
    let _guard = UPDATE_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let mut state = load_state_from(path);
    if !notification_is_newer(state.last_notified_version.as_deref(), version) {
        return Ok(false);
    }
    state.last_notified_version = Some(version.to_string());
    save_state_to(path, &state)?;
    Ok(true)
}

fn notification_is_newer(last_notified: Option<&str>, candidate: &str) -> bool {
    let Some(last_notified) = last_notified.and_then(parse_version) else {
        return true;
    };
    parse_version(candidate).is_some_and(|candidate| candidate > last_notified)
}

fn check_is_due(state: &UpdateState, now: DateTime<Utc>) -> bool {
    let Some(last_checked) = state
        .last_checked_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
    else {
        return true;
    };

    if last_checked > now {
        return true;
    }
    now - last_checked >= Duration::hours(CHECK_INTERVAL_HOURS)
}

fn result_from_state(
    current_version: &str,
    state: &UpdateState,
    checked_now: bool,
) -> UpdateCheckResult {
    let release = state.latest_release.as_ref();
    let status = release
        .and_then(|item| is_update_available(current_version, &item.version).ok())
        .map(|available| {
            if available {
                "update_available"
            } else {
                "current"
            }
        })
        .unwrap_or("unknown");

    UpdateCheckResult {
        status: status.into(),
        current_version: current_version.into(),
        latest_version: release.map(|item| item.version.clone()),
        release_url: release.map(|item| item.url.clone()),
        checked_at: state.last_checked_at.clone(),
        checked_now,
        error: None,
    }
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut parts = value.split('.');
    let major = parse_version_part(parts.next()?)?;
    let minor = parse_version_part(parts.next()?)?;
    let patch = parse_version_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn parse_version_part(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn save_state_to(path: &Path, state: &UpdateState) -> anyhow::Result<()> {
    save_state_to_with(path, state, replace_state_file)
}

fn save_state_to_with(
    path: &Path,
    state: &UpdateState,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), sequence));
    let contents = serde_json::to_vec_pretty(state)?;
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&contents)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    match replace(path, &temp) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error.into())
        }
    }
}

fn replace_state_file(path: &Path, temp: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return std::fs::rename(temp, path);
    }
    replace_existing_state_file(path, temp)
}

#[cfg(windows)]
fn replace_existing_state_file(path: &Path, temp: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS},
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let target = wide(path);
    let replacement = wide(temp);
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(not(windows))]
fn replace_existing_state_file(path: &Path, temp: &Path) -> std::io::Result<()> {
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_state_replace_preserves_old_bytes_and_cleans_temp() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-update-state-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("update-state.json");
        let original = br#"{"last_checked_at":"old"}"#;
        std::fs::write(&path, original).unwrap();

        let result = save_state_to_with(&path, &UpdateState::default(), |_, _| {
            Err(std::io::Error::other("injected replace failure"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains("json.tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn nsis_update_switches_are_unquoted_and_match_tauri_contract() {
        assert_eq!(WINDOWS_NSIS_UPDATE_PARAMETERS, "/P /UPDATE");
        assert!(!WINDOWS_NSIS_UPDATE_PARAMETERS.contains('"'));
        assert!(!WINDOWS_NSIS_UPDATE_PARAMETERS
            .split_ascii_whitespace()
            .any(|value| value == "/R"));
    }

    #[cfg(windows)]
    #[test]
    fn update_helper_arguments_are_strict_and_path_safe() {
        use std::ffi::OsString;

        let args = parse_update_helper_args(
            [
                OsString::from(r"C:\Temp\installer with spaces.exe"),
                OsString::from("42"),
                OsString::from(r"C:\Program Files\Juice\agent-juice.exe"),
                OsString::from("0.1.12"),
                OsString::from(r"C:\Temp\Juice_update_backup_42_1"),
                OsString::from(r"Local\AgentJuiceUpdaterReady_42_1"),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(args.parent_pid, 42);
        assert_eq!(args.expected_version, "0.1.12");
        assert_eq!(
            args.installer,
            PathBuf::from(r"C:\Temp\installer with spaces.exe")
        );
        assert_eq!(
            args.app_exe,
            PathBuf::from(r"C:\Program Files\Juice\agent-juice.exe")
        );
        assert_eq!(
            args.backup_dir,
            PathBuf::from(r"C:\Temp\Juice_update_backup_42_1")
        );
        assert!(parse_update_helper_args([OsString::from("only-one")].into_iter()).is_err());
        assert!(parse_update_helper_args(
            [
                OsString::from("installer.exe"),
                OsString::from("42"),
                OsString::from("app.exe"),
                OsString::from("0.1.12"),
                OsString::from("backup"),
                OsString::from(r"Local\AgentJuiceUpdaterReady_42_1"),
                OsString::from("extra"),
            ]
            .into_iter(),
        )
        .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn update_helper_completes_only_after_installer_success() {
        use std::{cell::Cell, os::windows::process::ExitStatusExt};

        let restarted = Cell::new(false);
        finish_installer_handoff(Ok(std::process::ExitStatus::from_raw(0)), || {
            restarted.set(true);
            Ok(())
        })
        .unwrap();
        assert!(restarted.get());

        restarted.set(false);

        let failed = finish_installer_handoff(Ok(std::process::ExitStatus::from_raw(7)), || {
            restarted.set(true);
            Ok(())
        });
        assert!(failed.is_err());
        assert!(!restarted.get());
    }

    #[cfg(windows)]
    #[test]
    fn helper_handoff_sync_uses_a_windows_writable_handle() {
        let path = std::env::temp_dir().join(format!(
            "agent-juice-helper-sync-{}-{}.exe",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, b"helper").unwrap();
        sync_handoff_file(&path).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn statusline_quarantine_retries_then_recovers_original_helper() {
        use std::cell::Cell;

        let root = std::env::temp_dir().join(format!(
            "agent-juice-statusline-quarantine-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let app_exe = root.join("agent-juice.exe");
        let statusline = root.join(UPDATE_STATUSLINE_HELPER_NAME);
        std::fs::write(&app_exe, b"app").unwrap();
        std::fs::write(&statusline, b"statusline").unwrap();
        let move_attempts = Cell::new(0);
        let waits = Cell::new(0);

        let quarantine = quarantine_statusline_with(
            &app_exe,
            3,
            |source, destination| {
                let attempt = move_attempts.get() + 1;
                move_attempts.set(attempt);
                if attempt < 3 {
                    Err(std::io::Error::from_raw_os_error(32))
                } else {
                    std::fs::rename(source, destination)
                }
            },
            || waits.set(waits.get() + 1),
        )
        .unwrap();

        assert_eq!(move_attempts.get(), 3);
        assert_eq!(waits.get(), 2);
        assert!(!statusline.exists());
        assert_eq!(std::fs::read(&quarantine).unwrap(), b"statusline");

        recover_statusline_quarantine(&app_exe).unwrap();
        assert_eq!(std::fs::read(&statusline).unwrap(), b"statusline");
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn statusline_quarantine_timeout_leaves_canonical_helper_intact() {
        use std::cell::Cell;

        let root = std::env::temp_dir().join(format!(
            "agent-juice-statusline-timeout-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let app_exe = root.join("agent-juice.exe");
        let statusline = root.join(UPDATE_STATUSLINE_HELPER_NAME);
        let quarantine = root.join(UPDATE_STATUSLINE_QUARANTINE_NAME);
        std::fs::write(&app_exe, b"app").unwrap();
        std::fs::write(&statusline, b"statusline").unwrap();
        let waits = Cell::new(0);

        let result = quarantine_statusline_with(
            &app_exe,
            2,
            |_, _| Err(std::io::Error::from_raw_os_error(32)),
            || waits.set(waits.get() + 1),
        );

        assert!(result.is_err());
        assert_eq!(waits.get(), 1);
        assert_eq!(std::fs::read(&statusline).unwrap(), b"statusline");
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn startup_recovery_prefers_an_existing_canonical_statusline() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-statusline-stale-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let app_exe = root.join("agent-juice.exe");
        let statusline = root.join(UPDATE_STATUSLINE_HELPER_NAME);
        let quarantine = root.join(UPDATE_STATUSLINE_QUARANTINE_NAME);
        std::fs::write(&app_exe, b"app").unwrap();
        std::fs::write(&statusline, b"current").unwrap();
        std::fs::write(&quarantine, b"stale").unwrap();

        recover_statusline_quarantine_with(&app_exe, |_, _| true).unwrap();

        assert_eq!(std::fs::read(&statusline).unwrap(), b"current");
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn startup_recovery_restores_quarantine_when_canonical_statusline_is_invalid() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-statusline-corrupt-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let app_exe = root.join("agent-juice.exe");
        let statusline = root.join(UPDATE_STATUSLINE_HELPER_NAME);
        let quarantine = root.join(UPDATE_STATUSLINE_QUARANTINE_NAME);
        std::fs::write(&app_exe, b"app").unwrap();
        std::fs::write(&statusline, b"partial-new-helper").unwrap();
        std::fs::write(&quarantine, b"known-good-helper").unwrap();

        recover_statusline_quarantine_with(&app_exe, |_, _| false).unwrap();

        assert_eq!(std::fs::read(&statusline).unwrap(), b"known-good-helper");
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn failed_restore_does_not_restart_the_application() {
        use std::cell::Cell;

        let restarted = Cell::new(false);
        let result = restart_after_successful_restore(
            Err(anyhow::anyhow!("main executable restore failed")),
            || {
                restarted.set(true);
                Ok(())
            },
        );

        assert!(result.is_err());
        assert!(!restarted.get());
    }

    #[cfg(windows)]
    #[test]
    fn failed_install_restores_every_preexisting_application_file() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-update-backup-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let app_dir = root.join("app");
        let backup_dir = root.join("backup");
        std::fs::create_dir_all(&app_dir).unwrap();
        let app_exe = app_dir.join("agent-juice.exe");
        let statusline = app_dir.join("agentjuice-statusline.exe");
        let uninstaller = app_dir.join("uninstall.exe");
        std::fs::write(&app_exe, b"old-app").unwrap();
        std::fs::write(&statusline, b"old-statusline").unwrap();
        std::fs::write(&uninstaller, b"old-uninstaller").unwrap();

        create_update_backup(&app_exe, &backup_dir).unwrap();
        let quarantine = quarantine_statusline_for_update(&app_exe).unwrap();
        std::fs::write(&app_exe, b"partial-new-app").unwrap();
        std::fs::write(&statusline, b"partial-new-statusline").unwrap();
        std::fs::write(&uninstaller, b"partial-new-uninstaller").unwrap();
        restore_update_backup(&backup_dir, &app_exe).unwrap();
        recover_statusline_quarantine(&app_exe).unwrap();

        assert_eq!(std::fs::read(&app_exe).unwrap(), b"old-app");
        assert_eq!(std::fs::read(&statusline).unwrap(), b"old-statusline");
        assert_eq!(std::fs::read(&uninstaller).unwrap(), b"old-uninstaller");
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn stale_temp_cleanup_is_bounded_to_known_files_and_preserves_backups() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-update-cleanup-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(root.join("Juice_update_backup_1_1")).unwrap();
        let helper = root.join("Juice_update_helper_1_1.exe");
        let installer = root.join("Juice_0.1.12_1_1_x64-setup.exe");
        let unrelated = root.join("keep-me.exe");
        std::fs::write(&helper, b"helper").unwrap();
        std::fs::write(&installer, b"installer").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();

        cleanup_stale_update_files_in(
            &root,
            std::time::SystemTime::now()
                + std::time::Duration::from_secs(UPDATE_TEMP_STALE_SECS + 1),
        );

        assert!(!helper.exists());
        assert!(!installer.exists());
        assert!(unrelated.exists());
        assert!(root.join("Juice_update_backup_1_1").is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn updater_staging_prefers_the_isolated_juice_data_directory() {
        let data = PathBuf::from(r"C:\isolated\agent-juice");
        let temp = Path::new(r"C:\Users\test\AppData\Local\Temp");

        assert_eq!(
            update_staging_directory_for(Some(data.clone()), temp),
            data.join("updates")
        );
        assert_eq!(
            update_staging_directory_for(None, temp),
            temp.join("Juice").join("updates")
        );
    }
}
