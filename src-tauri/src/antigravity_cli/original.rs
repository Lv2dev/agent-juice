#[cfg(windows)]
use super::binding::Binding;

#[cfg(windows)]
pub(super) fn run_if_connected_at(
    binding: &Binding,
    input: &[u8],
    home: &std::path::Path,
) -> Vec<u8> {
    run_if_connected_with(binding, input, home, run_command)
}

#[cfg(windows)]
fn run_if_connected_with(
    binding: &Binding,
    input: &[u8],
    home: &std::path::Path,
    run: impl FnOnce(&str, &[u8]) -> Vec<u8>,
) -> Vec<u8> {
    use std::{fs, io::Read};
    let settings = home.join(".gemini/antigravity-cli/settings.json");
    if super::checked_path(&settings).is_err() {
        return Vec::new();
    }
    let Ok(file) = fs::File::open(&settings) else {
        return Vec::new();
    };
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    if file
        .take(crate::statusline::MAX_STATUSLINE_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > crate::statusline::MAX_STATUSLINE_INPUT_BYTES
    {
        return Vec::new();
    }
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let current = settings.get("statusLine");
    if current
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return Vec::new();
    }
    let owned = current
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        == Some(binding.managed_command.as_str());
    let original = current == binding.original_status_line.as_ref();
    if !owned && !original && !binding.verified_previous_owns_at(current, home) {
        return Vec::new();
    }
    let Some(original) = binding.original_status_line.as_ref() else {
        return Vec::new();
    };
    if original.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
        return Vec::new();
    }
    let Some(command) = original
        .get("command")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty() && s.len() <= 32 * 1024 && !s.contains('\0'))
    else {
        return Vec::new();
    };
    run(command, input)
}

#[cfg(all(windows, test))]
pub(super) fn run_if_connected_at_with(
    binding: &Binding,
    input: &[u8],
    home: &std::path::Path,
    run: impl FnOnce(&str, &[u8]) -> Vec<u8>,
) -> Vec<u8> {
    run_if_connected_with(binding, input, home, run)
}

#[cfg(windows)]
fn run_command(command: &str, input: &[u8]) -> Vec<u8> {
    use std::os::windows::process::CommandExt;
    use std::{
        io::{Read, Write},
        process::{Command, Stdio},
        sync::mpsc,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        System::SystemInformation::GetSystemDirectoryW,
        System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED},
    };
    let mut system = [0u16; 32768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if length == 0 || length >= system.len() {
        return Vec::new();
    }
    let shell =
        std::path::PathBuf::from(String::from_utf16_lossy(&system[..length])).join("cmd.exe");
    let Ok(tree) = crate::collector::ProcessTree::create() else {
        return Vec::new();
    };
    let mut command_line = Command::new(shell);
    command_line
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{command}\""))
        .creation_flags(CREATE_NO_WINDOW.0 | CREATE_SUSPENDED.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = command_line.spawn() else {
        return Vec::new();
    };
    if tree.assign(&child).is_err() || crate::collector::ProcessTree::resume(&child).is_err() {
        tree.terminate();
        let _ = child.kill();
        let _ = child.wait();
        return Vec::new();
    }
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let data = zeroize::Zeroizing::new(input.to_vec());
    std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&data);
        }
    });
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(stdout) = stdout {
            let _ = stdout.take(16 * 1024 + 1).read_to_end(&mut output);
        }
        let _ = send.send(output);
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => break false,
        }
    };
    tree.terminate();
    let _ = child.kill();
    let _ = child.wait();
    if !success {
        return Vec::new();
    }
    receive
        .recv_timeout(Duration::from_millis(100))
        .ok()
        .filter(|bytes| bytes.len() <= 16 * 1024)
        .unwrap_or_default()
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    #[test]
    fn original_command_keeps_stdout_and_receives_unchanged_stdin() {
        assert_eq!(
            run_command("echo original-output", b"{}"),
            b"original-output\r\n"
        );
        let input = b"original private input\r\n";
        assert_eq!(run_command("findstr /r \".*\"", input), input);
    }
    #[test]
    fn original_command_is_bounded_and_failure_does_not_forward_error_text() {
        assert!(run_command("exit /b 1", b"{}").is_empty());
        let started = std::time::Instant::now();
        assert!(run_command("ping -n 10 127.0.0.1 >nul", b"{}").is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
    }
}
