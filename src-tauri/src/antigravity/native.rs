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

fn request(
    agent: &ureq::Agent,
    server: &Server,
    port: u16,
    method: &str,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    #[cfg(test)]
    let started = Instant::now();
    if Instant::now() >= deadline || !server_owns_port(server, port) {
        return Err(Error::Unavailable);
    }
    let url =
        format!("https://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/{method}");
    let mut response = agent
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
        .build()
        .send(b"{}")
        .map_err(|_| {
            #[cfg(test)]
            eprintln!(
                "[antigravity-test] {method} request failed after {:?}",
                started.elapsed()
            );
            Error::Unavailable
        })?;
    if !server_owns_port(server, port) {
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
    if body.len() > 1024 * 1024 || Instant::now() >= deadline || !server_owns_port(server, port) {
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
) -> Result<AgentStatus, Error> {
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
    for port in ports(server.pid)? {
        if Instant::now() >= deadline {
            break;
        }
        let auth = match request(&agent, server, port, "GetAuthStatus", deadline) {
            Ok(body) => body,
            Err(Error::LoginRequired) => return Err(Error::LoginRequired),
            Err(_) => continue,
        };
        if !authenticated(&auth)? {
            return Err(Error::LoginRequired);
        }
        let body = request(&agent, server, port, "GetUserStatus", deadline)?;
        let status = parse_status(&body, pc_id, captured_at)?;
        if !authenticated(&request(&agent, server, port, "GetAuthStatus", deadline)?)? {
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
        )
        .unwrap();
        assert!(status.primary.is_some());
        assert!(status.primary.unwrap().used_percent.is_some());
    }
}
