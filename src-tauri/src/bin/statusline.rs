use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if let Some(arg) = args.next() {
        if arg == "--restore-owned-statusline" && args.next().is_none() {
            let restore = agent_juice::config::Settings::restore_statusline_if_installed()
                .and_then(|_| {
                    if let (Some(home), Some(data)) =
                        (dirs::home_dir(), agent_juice::paths::data_dir())
                    {
                        agent_juice::antigravity_cli::binding::restore_at(&home, &data)?;
                    }
                    Ok(())
                });
            return match restore {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    let _ = err;
                    eprintln!("failed to restore owned statusLine configuration");
                    ExitCode::FAILURE
                }
            };
        }
        if arg == "--antigravity-cli" && args.next().is_none() {
            if let Ok(input) = agent_juice::antigravity_cli::read_input(std::io::stdin()) {
                let output = agent_juice::antigravity_cli::run_statusline(&input);
                let _ = std::io::stdout().write_all(&output);
            }
            return ExitCode::SUCCESS;
        }
        eprintln!("unsupported agentjuice-statusline argument");
        return ExitCode::from(2);
    }

    if !agent_juice::config::Settings::try_load().is_ok_and(|settings| settings.show_claude) {
        return ExitCode::SUCCESS;
    }

    let max_bytes = agent_juice::statusline::MAX_STATUSLINE_INPUT_BYTES;
    let mut input = Vec::with_capacity(max_bytes + 1);
    let _ = std::io::stdin()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut input);
    let oversized = input.len() > max_bytes;
    input.truncate(max_bytes);
    let input = String::from_utf8_lossy(&input);
    let output = if oversized {
        agent_juice::statusline::run_without_original_with_default_dir(&input)
    } else {
        agent_juice::statusline::run_with_default_dir(&input)
    };
    let _ = std::io::stdout().write_all(&output);
    ExitCode::SUCCESS
}
