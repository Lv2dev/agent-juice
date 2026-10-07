use super::{CaptureError, ProducerProof};
use std::{
    collections::BTreeMap,
    mem::size_of,
    path::{Path, PathBuf},
};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_TIMEOUT},
        Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
                OpenProcessToken, QueryFullProcessImageNameW, WaitForSingleObject,
                PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        },
    },
};

pub struct Lease(HANDLE);
unsafe impl Send for Lease {}
unsafe impl Sync for Lease {}
impl Drop for Lease {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
impl Lease {
    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
    }
}

fn clean_path(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if matches!(part, std::path::Component::Prefix(_)) {
            continue;
        }
        use std::os::windows::fs::MetadataExt;
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

fn user_sid(process: HANDLE) -> Result<(Lease, Vec<usize>), CaptureError> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token)
            .map_err(|_| CaptureError::Unavailable)?;
        let token = Lease(token);
        let mut size = 0;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut size);
        if !(size_of::<TOKEN_USER>() as u32..=4096).contains(&size) {
            return Err(CaptureError::Unavailable);
        }
        let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(data.as_mut_ptr().cast()),
            size,
            &mut size,
        )
        .map_err(|_| CaptureError::Unavailable)?;
        Ok((token, data))
    }
}

fn expected_image_path(data_local_dir: &Path) -> PathBuf {
    // QueryFullProcessImageNameW returns native separators for the strict image check.
    data_local_dir.join("agy").join("bin").join("agy.exe")
}

pub fn verify(proof: &ProducerProof) -> Result<Lease, CaptureError> {
    let expected = expected_image_path(&dirs::data_local_dir().ok_or(CaptureError::Unavailable)?);
    if !clean_path(&expected) {
        return Err(CaptureError::Unavailable);
    }
    unsafe {
        let process = Lease(
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                proof.pid,
            )
            .map_err(|_| CaptureError::Unavailable)?,
        );
        if !process.alive() {
            return Err(CaptureError::Unavailable);
        }
        let mut session = 0;
        let mut other = u32::MAX;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session)
            .map_err(|_| CaptureError::Unavailable)?;
        ProcessIdToSessionId(proof.pid, &mut other).map_err(|_| CaptureError::Unavailable)?;
        if session != other {
            return Err(CaptureError::Unavailable);
        }
        let mut path = [0u16; 32768];
        let mut length = path.len() as u32;
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
        .map_err(|_| CaptureError::Unavailable)?;
        let image =
            String::from_utf16(&path[..length as usize]).map_err(|_| CaptureError::Unavailable)?;
        if !image.eq_ignore_ascii_case(&expected.to_string_lossy()) {
            return Err(CaptureError::Unavailable);
        }
        let mut created = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        GetProcessTimes(process.0, &mut created, &mut exit, &mut kernel, &mut user)
            .map_err(|_| CaptureError::Unavailable)?;
        let timestamp = u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime);
        if timestamp != proof.created {
            return Err(CaptureError::Unavailable);
        }
        let (_current_token, current_data) = user_sid(GetCurrentProcess())?;
        let (_other_token, other_data) = user_sid(process.0)?;
        let current = &*current_data.as_ptr().cast::<TOKEN_USER>();
        let other = &*other_data.as_ptr().cast::<TOKEN_USER>();
        if EqualSid(current.User.Sid, other.User.Sid).is_err() {
            return Err(CaptureError::Unavailable);
        }
        Ok(process)
    }
}

pub fn producer() -> Result<ProducerProof, CaptureError> {
    unsafe {
        let snapshot = Lease(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|_| CaptureError::Unavailable)?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut parents = BTreeMap::new();
        let mut next = Process32FirstW(snapshot.0, &mut entry).is_ok();
        while next && parents.len() < 32768 {
            let length = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            parents.insert(
                entry.th32ProcessID,
                (
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..length]),
                ),
            );
            next = Process32NextW(snapshot.0, &mut entry).is_ok();
        }
        let mut pid = GetCurrentProcessId();
        let mut child_created = u64::MAX;
        for _ in 0..8 {
            let (parent, name) = parents.get(&pid).ok_or(CaptureError::Unavailable)?;
            let process = Lease(
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                    .map_err(|_| CaptureError::Unavailable)?,
            );
            let mut created = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            GetProcessTimes(process.0, &mut created, &mut exit, &mut kernel, &mut user)
                .map_err(|_| CaptureError::Unavailable)?;
            let timestamp =
                u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime);
            if timestamp > child_created {
                return Err(CaptureError::Unavailable);
            }
            child_created = timestamp;
            if name.eq_ignore_ascii_case("agy.exe") {
                let proof = ProducerProof {
                    pid,
                    created: timestamp,
                };
                verify(&proof)?;
                return Ok(proof);
            }
            if *parent == pid || *parent == 0 {
                break;
            }
            pid = *parent;
        }
    }
    Err(CaptureError::Unavailable)
}

pub fn retired(proof: &ProducerProof) -> bool {
    unsafe {
        let process = match OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            proof.pid,
        ) {
            Ok(handle) => Lease(handle),
            Err(error) => return error.code() == windows::core::HRESULT::from_win32(87),
        };
        if !process.alive() {
            return true;
        }
        let mut created = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(process.0, &mut created, &mut exit, &mut kernel, &mut user).is_err() {
            return false;
        }
        (u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
            != proof.created
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_cli_image_matches_windows_process_image_separators() {
        let path = expected_image_path(Path::new(r"C:\Users\fixture\AppData\Local"));
        assert_eq!(
            path.as_os_str(),
            std::ffi::OsStr::new(r"C:\Users\fixture\AppData\Local\agy\bin\agy.exe")
        );
    }

    #[test]
    fn same_user_unrelated_process_is_not_a_trusted_cli_producer() {
        unsafe {
            let mut created = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exit,
                &mut kernel,
                &mut user,
            )
            .unwrap();
            let proof = ProducerProof {
                pid: GetCurrentProcessId(),
                created: u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime),
            };
            assert!(verify(&proof).is_err());
        }
    }
}
