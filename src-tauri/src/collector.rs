use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;

const TAIL_CHUNK_BYTES: u64 = 64 * 1024;

fn is_rollout(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_rollout)
            {
                out.push(path);
            }
        }
    }
}

pub fn list_rollouts(sessions_dir: &Path) -> Vec<PathBuf> {
    let mut rollouts = Vec::new();
    walk(sessions_dir, &mut rollouts);
    rollouts
}

pub fn latest_rollout(sessions_dir: &Path) -> Option<PathBuf> {
    recent_rollouts(sessions_dir, 1).into_iter().next()
}

pub fn recent_rollouts(sessions_dir: &Path, limit: usize) -> Vec<PathBuf> {
    if limit == 0 {
        return Vec::new();
    }

    let mut rollouts: Vec<_> = list_rollouts(sessions_dir)
        .into_iter()
        .filter_map(|path| {
            std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(|modified| (modified, path))
        })
        .collect();
    rollouts.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    rollouts
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect()
}

pub fn session_id_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_default()
}

pub fn last_token_count_line(contents: &str) -> Option<String> {
    contents
        .lines()
        .rev()
        .find(|line| is_token_count_event(line))
        .map(str::to_string)
}

pub fn last_token_count_line_from_file(path: &Path) -> anyhow::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    let mut position = file.metadata()?.len();
    let mut carry = String::new();

    while position > 0 {
        let read_len = position.min(TAIL_CHUNK_BYTES);
        position -= read_len;

        file.seek(SeekFrom::Start(position))?;
        let mut buffer = vec![0; read_len as usize];
        file.read_exact(&mut buffer)?;

        let mut combined = String::from_utf8_lossy(&buffer).into_owned();
        combined.push_str(&carry);

        let mut lines: Vec<&str> = combined.lines().collect();
        if position > 0 {
            carry = lines.first().copied().unwrap_or_default().to_string();
            if !lines.is_empty() {
                lines.remove(0);
            }
        }

        if let Some(line) = lines.iter().rev().find(|line| is_token_count_event(line)) {
            return Ok(Some((*line).to_string()));
        }
    }

    Ok(None)
}

fn is_token_count_event(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };

    value.get("type").and_then(Value::as_str) == Some("event_msg")
        && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
}
