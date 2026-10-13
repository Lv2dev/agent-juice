use super::{authenticated, parse_status, AgentStatus, Error};
use std::{
    io::Read,
    mem::size_of,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::PWSTR,
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
    Win32::{
        Foundation::{CloseHandle, HANDLE, UNICODE_STRING, WAIT_TIMEOUT},
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        },
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
                PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        },
    },
};

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Server {
    process: OwnedHandle,
    pid: u32,
    csrf: String,
}

fn checked_install_path(path: &Path, root: &Path) -> bool {
    if !path
        .to_string_lossy()
        .replace('/', "\\")
        .eq_ignore_ascii_case(
            &root
                .join("resources/bin/language_server.exe")
                .to_string_lossy()
                .replace('/', "\\"),
        )
    {
        return false;
    }
    let mut current = PathBuf::new();
    // Reject redirected install trees; no arbitrary process-name or PATH matching.
    for component in path.components() {
        current.push(component);
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        use std::os::windows::fs::MetadataExt;
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

fn csrf_argument(command: &str) -> Option<String> {
    let mut words = command.split_whitespace();
    let mut value = None;
    while let Some(word) = words.next() {
        let candidate = if word == "--csrf_token" {
            words.next()
        } else {
            word.strip_prefix("--csrf_token=")
        };
        if let Some(token) = candidate {
            let token = token.trim_matches('"');
            if value.is_some()
                || !(16..=128).contains(&token.len())
                || !token
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return None;
            }
            value = Some(token.to_owned());
        }
    }
    value
}

fn command_line(process: HANDLE) -> Option<String> {
    unsafe {
        let mut length = 0u32;
        let _ = NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            std::ptr::null_mut(),
            0,
            &mut length,
        );
        if !(size_of::<UNICODE_STRING>() as u32..=65536).contains(&length) {
            return None;
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        let capacity = (buffer.len() * size_of::<usize>()) as u32;
        let result = NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            buffer.as_mut_ptr().cast(),
            capacity,
            &mut length,
        );
        if result.is_err() || length > capacity {
            return None;
        }
        let header = &*buffer.as_ptr().cast::<UNICODE_STRING>();
        let start = buffer.as_ptr() as usize;
        let ptr = header.Buffer.0 as usize;
        let len = header.Length as usize;
        if !len.is_multiple_of(2)
            || ptr < start + size_of::<UNICODE_STRING>()
            || ptr.checked_add(len)? > start + length as usize
        {
            return None;
        }
        String::from_utf16(std::slice::from_raw_parts(header.Buffer.0, len / 2)).ok()
    }
}

fn servers(deadline: Instant) -> Result<Vec<Server>, Error> {
    let root = dirs::data_local_dir()
        .ok_or(Error::Unavailable)?
        .join("Programs/antigravity");
    let mut found = Vec::new();
    let mut installed_process = false;
    unsafe {
        let snapshot = OwnedHandle(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|_| Error::Unavailable)?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut session = 0;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session)
            .map_err(|_| Error::Unavailable)?;
        let mut next = Process32FirstW(snapshot.0, &mut entry).is_ok();
        while next && Instant::now() < deadline && found.len() < 4 {
            let length = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..length])
                .eq_ignore_ascii_case("language_server.exe")
            {
                let mut other_session = u32::MAX;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut other_session).is_ok()
                    && session == other_session
                {
                    if let Ok(handle) = OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        false,
                        entry.th32ProcessID,
                    ) {
                        let process = OwnedHandle(handle);
                        let mut path = [0u16; 32768];
                        let mut size = path.len() as u32;
                        if QueryFullProcessImageNameW(
                            handle,
                            PROCESS_NAME_WIN32,
                            PWSTR(path.as_mut_ptr()),
                            &mut size,
                        )
                        .is_ok()
                        {
                            let path =
                                PathBuf::from(String::from_utf16_lossy(&path[..size as usize]));
                            if checked_install_path(&path, &root) {
                                installed_process = true;
                                if let Some(csrf) =
                                    command_line(handle).and_then(|line| csrf_argument(&line))
                                {
                                    found.push(Server {
                                        process,
                                        pid: entry.th32ProcessID,
                                        csrf,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            next = Process32NextW(snapshot.0, &mut entry).is_ok();
        }
    }
    if Instant::now() >= deadline {
        return Err(Error::Unavailable);
    }
    if installed_process && found.is_empty() {
        return Err(Error::Unavailable);
    }
    Ok(found)
}

fn ports(pid: u32) -> Result<Vec<u16>, Error> {
    unsafe {
        let mut size = 0;
        let _ = GetExtendedTcpTable(None, &mut size, false, 2, TCP_TABLE_OWNER_PID_LISTENER, 0);
        if !(4..=1024 * 1024).contains(&size) {
            return Err(Error::Unavailable);
        }
        let mut buffer = vec![0u32; (size as usize).div_ceil(4)];
        let capacity = buffer.len() * 4;
        if GetExtendedTcpTable(
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            false,
            2,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        ) != 0
            || size < 4
            || size as usize > capacity
        {
            return Err(Error::Unavailable);
        }
        let count = buffer[0] as usize;
        if count > (size as usize - 4) / size_of::<MIB_TCPROW_OWNER_PID>() {
            return Err(Error::Unavailable);
        }
        let rows = std::slice::from_raw_parts(
            buffer.as_ptr().add(1).cast::<MIB_TCPROW_OWNER_PID>(),
            count,
        );
        let mut result: Vec<_> = rows
            .iter()
            .filter(|r| r.dwOwningPid == pid && matches!(r.dwLocalAddr, 0 | 0x0100007f))
            .map(|r| u16::from_be(r.dwLocalPort as u16))
            .filter(|p| *p != 0)
            .collect();
        result.sort_unstable();
        result.dedup();
        result.truncate(8);
        Ok(result)
    }
}

fn server_owns_port(server: &Server, port: u16) -> bool {
    (unsafe { WaitForSingleObject(server.process.0, 0) == WAIT_TIMEOUT })
        && ports(server.pid).is_ok_and(|ports| ports.contains(&port))
}

#[derive(Clone, Copy)]
enum Rpc {
    Auth,
    Quota,
}

impl Rpc {
    fn method(self) -> &'static str {
        match self {
            Self::Auth => "GetAuthStatus",
            Self::Quota => "RetrieveUserQuotaSummary",
        }
    }

    fn body(self) -> &'static [u8] {
        match self {
            Self::Auth => b"{}",
            Self::Quota => br#"{"request":{},"forceRefresh":true}"#,
        }
    }
}

fn admitted_until(deadline: Instant, admitted: &dyn Fn() -> bool) -> bool {
    Instant::now() < deadline && admitted() && Instant::now() < deadline
}

fn request(
    agent: &ureq::Agent,
    server: &Server,
    port: u16,
    rpc: Rpc,
    deadline: Instant,
    admitted: &dyn Fn() -> bool,
) -> Result<Vec<u8>, Error> {
    #[cfg(test)]
    let started = Instant::now();
    if !admitted_until(deadline, admitted) || !server_owns_port(server, port) {
        return Err(Error::Unavailable);
    }
    let method = rpc.method();
    let url =
        format!("https://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/{method}");
    let request = agent
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-codeium-csrf-token", &server.csrf)
        .header("Connect-Protocol-Version", "1")
        .config()
        .timeout_global(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(8)),
        ))
        .build();
    if !admitted_until(deadline, admitted) {
        return Err(Error::Unavailable);
    }
    let mut response = request.send(rpc.body()).map_err(|_| {
        #[cfg(test)]
        eprintln!(
            "[antigravity-test] {method} request failed after {:?}",
            started.elapsed()
        );
        Error::Unavailable
    })?;
    if !admitted_until(deadline, admitted) || !server_owns_port(server, port) {
        return Err(Error::Unavailable);
    }
    let status = response.status().as_u16();
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut body)
        .map_err(|_| Error::Unavailable)?;
    if body.len() > 1024 * 1024
        || !admitted_until(deadline, admitted)
        || !server_owns_port(server, port)
    {
        return Err(Error::Unavailable);
    }
    if status != 200 {
        // HTTP/CSRF failures are not proof that the Google account is logged out.
        return Err(Error::Unavailable);
    }
    #[cfg(test)]
    eprintln!(
        "[antigravity-test] {method} completed after {:?}",
        started.elapsed()
    );
    Ok(body)
}

pub(super) fn collect(
    pc_id: &str,
    captured_at: &str,
    deadline: Instant,
    admitted: &dyn Fn() -> bool,
) -> Result<AgentStatus, Error> {
    if !admitted_until(deadline, admitted) {
        return Err(Error::Unavailable);
    }
    let servers = servers(deadline)?;
    if servers.is_empty() {
        return Err(Error::AppRequired);
    }
    // Multiple GUI instances/accounts are ambiguous. Do not silently choose an account.
    if servers.len() != 1 {
        return Err(Error::Unavailable);
    }
    let server = &servers[0];
    // Antigravity itself uses a self-signed loopback server. The trust boundary is
    // the held, same-session installed process and its owned listener, not Web PKI.
    // This client never accepts a caller URL, proxy, redirect or remote credential.
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_millis(700)))
        .max_idle_connections(0)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .disable_verification(true)
                .build(),
        )
        .build()
        .new_agent();
    collect_from_ports(
        pc_id,
        captured_at,
        ports(server.pid)?,
        deadline,
        admitted,
        |port, rpc| request(&agent, server, port, rpc, deadline, admitted),
    )
}

fn collect_from_ports(
    pc_id: &str,
    captured_at: &str,
    ports: impl IntoIterator<Item = u16>,
    deadline: Instant,
    admitted: &dyn Fn() -> bool,
    mut request: impl FnMut(u16, Rpc) -> Result<Vec<u8>, Error>,
) -> Result<AgentStatus, Error> {
    for port in ports {
        if !admitted_until(deadline, admitted) {
            return Err(Error::Unavailable);
        }
        let mut request_current = |rpc| {
            if !admitted_until(deadline, admitted) {
                return Err(Error::Unavailable);
            }
            request(port, rpc)
        };
        let auth = match request_current(Rpc::Auth) {
            Ok(body) => body,
            Err(Error::LoginRequired) => return Err(Error::LoginRequired),
            Err(_) => continue,
        };
        if !authenticated(&auth)? {
            return Err(Error::LoginRequired);
        }
        let body = request_current(Rpc::Quota)?;
        let status = parse_status(&body, pc_id, captured_at)?;
        if !authenticated(&request_current(Rpc::Auth)?)? {
            return Err(Error::LoginRequired);
        }
        return Ok(status);
    }
    Err(Error::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_that_expires_while_waiting_never_authorizes_dispatch() {
        let deadline = Instant::now() + Duration::from_millis(20);
        assert!(!admitted_until(deadline, &|| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining + Duration::from_millis(1));
            true
        }));
    }

    #[test]
    fn expired_admission_skips_policy_lookup_and_current_admission_preserves_it() {
        let calls = std::cell::Cell::new(0);
        let policy = || {
            calls.set(calls.get() + 1);
            true
        };
        assert!(!admitted_until(Instant::now(), &policy));
        assert_eq!(calls.get(), 0);
        assert!(admitted_until(
            Instant::now() + Duration::from_secs(2),
            &policy
        ));
        assert_eq!(calls.get(), 1);
        assert!(!admitted_until(
            Instant::now() + Duration::from_secs(2),
            &|| false
        ));
    }

    fn desktop_response(rpc: Rpc) -> Vec<u8> {
        match rpc {
            Rpc::Auth => br#"{"authResult":{"hasValidAuth":true}}"#.to_vec(),
            Rpc::Quota => {
                br#"{"response":{"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.5}]}}"#
                    .to_vec()
            }
        }
    }

    enum PolicyTransition {
        Off,
        Suspend,
        OffOn,
    }

    fn assert_retired_desktop_stops(
        force: bool,
        transition: PolicyTransition,
        pause_at: usize,
        fail_first_auth: bool,
    ) {
        use super::super::{refresh_cache, Cache};
        use std::sync::{mpsc, Mutex};

        let state = Mutex::new(Cache {
            enabled: true,
            ..Default::default()
        });
        let calls = Mutex::new(Vec::new());
        let (reached_tx, reached_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        let timeout = Duration::from_secs(2);
        let result = std::thread::scope(|scope| {
            let state = &state;
            let calls = &calls;
            let worker = scope.spawn(move || {
                refresh_cache(state, force, |admitted| {
                    collect_from_ports(
                        "test-pc",
                        "2026-10-13T00:00:00Z",
                        [1, 2],
                        Instant::now() + Duration::from_secs(5),
                        admitted,
                        |port, rpc| {
                            assert!(state.try_lock().is_ok(), "I/O must not hold CACHE");
                            let index = {
                                let mut calls = calls.lock().unwrap();
                                calls.push((port, rpc.method()));
                                calls.len()
                            };
                            if index == pause_at {
                                reached_tx.send(()).unwrap();
                                resume_rx.recv_timeout(timeout).unwrap();
                            }
                            if fail_first_auth && index == 1 {
                                Err(Error::Unavailable)
                            } else {
                                Ok(desktop_response(rpc))
                            }
                        },
                    )
                })
            });
            reached_rx.recv_timeout(timeout).unwrap();
            {
                let mut cache = state.lock().unwrap();
                match transition {
                    PolicyTransition::Off => assert!(cache.set_enabled(false)),
                    PolicyTransition::Suspend => assert!(cache.suspend_for_settings_failure()),
                    PolicyTransition::OffOn => {
                        assert!(cache.set_enabled(false));
                        assert!(cache.set_enabled(true));
                    }
                }
            }
            resume_tx.send(()).unwrap();
            worker.join().unwrap()
        });
        assert!(result.is_none(), "retired Desktop quota must not return");
        let cache = state.lock().unwrap();
        assert!(!cache.busy);
        assert!(cache.result.is_none(), "retired quota must not enter CACHE");
        assert!(cache.retry_at.is_some() && cache.force_retry_at.is_some());
        assert_eq!(
            calls.lock().unwrap().len(),
            pause_at,
            "no new RPC after retirement"
        );
    }

    #[test]
    fn desktop_current_epoch_preserves_auth_quota_auth_for_normal_and_force() {
        use super::super::{refresh_cache, Cache};
        use std::sync::Mutex;

        for force in [false, true] {
            let state = Mutex::new(Cache {
                enabled: true,
                ..Default::default()
            });
            let mut calls = Vec::new();
            let status = refresh_cache(&state, force, |admitted| {
                collect_from_ports(
                    "test-pc",
                    "2026-10-13T00:00:00Z",
                    [1, 2],
                    Instant::now() + Duration::from_secs(5),
                    admitted,
                    |port, rpc| {
                        assert!(state.try_lock().is_ok(), "I/O must not hold CACHE");
                        calls.push((port, rpc.method()));
                        Ok(desktop_response(rpc))
                    },
                )
            })
            .unwrap();
            assert_eq!(status.session_id, "antigravity-gui");
            assert_eq!(status.primary.unwrap().used_percent, Some(50.0));
            assert_eq!(
                calls,
                [
                    (1, "GetAuthStatus"),
                    (1, "RetrieveUserQuotaSummary"),
                    (1, "GetAuthStatus"),
                ]
            );
            let cache = state.lock().unwrap();
            assert!(cache.result.as_ref().unwrap().is_ok());
            assert!(cache.force_retry_at.is_none());
            assert_eq!(cache.failures, 0);
        }
    }

    #[test]
    fn desktop_off_after_auth_blocks_quota() {
        for force in [false, true] {
            assert_retired_desktop_stops(force, PolicyTransition::Off, 1, false);
        }
    }

    #[test]
    fn desktop_suspension_after_auth_blocks_quota() {
        for force in [false, true] {
            assert_retired_desktop_stops(force, PolicyTransition::Suspend, 1, false);
        }
    }

    #[test]
    fn desktop_off_on_after_auth_keeps_old_generation_retired() {
        for force in [false, true] {
            assert_retired_desktop_stops(force, PolicyTransition::OffOn, 1, false);
        }
    }

    #[test]
    fn desktop_failed_auth_after_off_blocks_next_port() {
        for force in [false, true] {
            assert_retired_desktop_stops(force, PolicyTransition::Off, 1, true);
        }
    }

    #[test]
    fn desktop_off_during_quota_blocks_reauthentication() {
        for force in [false, true] {
            assert_retired_desktop_stops(force, PolicyTransition::Off, 2, false);
        }
    }

    #[test]
    fn desktop_policy_or_deadline_denial_never_dispatches() {
        for (allowed, deadline) in [
            (false, Instant::now() + Duration::from_secs(5)),
            (true, Instant::now() - Duration::from_secs(1)),
        ] {
            assert_eq!(
                collect_from_ports(
                    "test-pc",
                    "2026-10-13T00:00:00Z",
                    [1, 2],
                    deadline,
                    &|| allowed,
                    |_, _| panic!("denied collection must not dispatch"),
                ),
                Err(Error::Unavailable)
            );
        }
    }

    #[test]
    fn quota_rpc_requests_a_fresh_summary_and_auth_remains_read_only() {
        assert_eq!(Rpc::Quota.method(), "RetrieveUserQuotaSummary");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(Rpc::Quota.body()).unwrap(),
            serde_json::json!({"request":{},"forceRefresh":true})
        );
        assert_eq!(Rpc::Auth.method(), "GetAuthStatus");
        assert_eq!(Rpc::Auth.body(), b"{}");
    }
    #[test]
    fn csrf_parser_rejects_duplicate_or_header_injection() {
        assert!(csrf_argument("x --csrf_token=12345678-1234-1234").is_some());
        assert!(csrf_argument("x --csrf_token short").is_none());
        assert!(
            csrf_argument("x --csrf_token 12345678-1234-1234 --csrf_token 12345678-1234-1234")
                .is_none()
        );
        assert!(csrf_argument("x --csrf_token=12345678:12345678").is_none());
    }
    #[test]
    fn rejects_other_programs_and_missing_install_paths() {
        assert!(!checked_install_path(
            Path::new("C:/fake/language_server.exe"),
            Path::new("C:/fake")
        ));
    }
    #[test]
    #[ignore = "reads quota from the locally running Antigravity GUI without a prompt"]
    fn live_running_gui_quota() {
        let status = collect(
            "test-pc",
            &chrono::Utc::now().to_rfc3339(),
            Instant::now() + super::super::COLLECTION_TIMEOUT,
            &|| true,
        )
        .unwrap();
        assert!(status.primary.is_some());
        let primary = status.primary.unwrap();
        assert_eq!(primary.label, "5h");
        assert!(primary.used_percent.is_some());
        if let Some(weekly) = status.secondary {
            assert_eq!(weekly.label, "week");
            assert!(weekly.used_percent.is_some());
        }
    }
}
