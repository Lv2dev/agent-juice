#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = agent_juice::update::update_helper_exit_code() {
        std::process::exit(code);
    }
    agent_juice::run()
}
