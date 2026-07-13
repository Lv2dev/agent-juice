use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if let Some(arg) = args.next() {
        if arg == "--restore-owned-statusline" && args.next().is_none() {
            return match agent_juice::config::Settings::restore_statusline() {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("failed to restore owned Claude statusLine: {err}");
                    ExitCode::FAILURE
                }
            };
        }
        eprintln!("unsupported agentjuice-statusline argument");
        return ExitCode::from(2);
    }

    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let output = agent_juice::statusline::run_with_default_dir(&input);
    let _ = std::io::stdout().write_all(&output);
    ExitCode::SUCCESS
}
