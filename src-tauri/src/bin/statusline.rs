use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let output = agent_juice::statusline::run_with_default_dir(&input);
    let _ = std::io::stdout().write_all(&output);
}
