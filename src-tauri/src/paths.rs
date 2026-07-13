use std::path::PathBuf;

pub const DATA_DIR_NAME: &str = "agent-juice";

pub fn data_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AGENT_JUICE_DATA_DIR") {
        return Some(PathBuf::from(path));
    }
    dirs::data_local_dir().map(|dir| dir.join(DATA_DIR_NAME))
}
