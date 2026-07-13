use agent_juice::collector::*;
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn unique_temp_dir() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-juice-collector-test-{}-{suffix}",
        std::process::id()
    ))
}

#[test]
fn picks_last_token_count_line() {
    let jsonl = concat!(
        r#"{"type":"response_item","x":1}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"other"}}"#,
        "\n"
    );
    let line = last_token_count_line(jsonl).unwrap();
    assert!(line.contains("token_count") && line.contains("258400"));
}

#[test]
fn token_count_line_skips_malformed_and_non_token_matches() {
    let jsonl = concat!(
        r#"{"timestamp":"2026-07-07T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"message","text":"mentions token_count but is not usage"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"token_count","info":"#,
        "\n"
    );

    let line = last_token_count_line(jsonl).unwrap();

    assert!(line.contains("258400"));
    assert!(line.contains(r#""type":"token_count""#));
}

#[test]
fn reads_last_token_count_line_from_file_tail() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let path = root.join("rollout-2026-07-08-tail.jsonl");
    fs::write(
        &path,
        concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":100}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"message","text":"not usage"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":"#,
            "\n"
        ),
    )
    .unwrap();

    let line = last_token_count_line_from_file(&path).unwrap().unwrap();

    assert!(line.contains("258400"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recursively_lists_rollouts_and_picks_latest() {
    let root = unique_temp_dir();
    let day = root.join("2026").join("07").join("07");
    fs::create_dir_all(&day).unwrap();

    let older = day.join("rollout-2026-07-07-old.jsonl");
    let ignored = day.join("notes.jsonl");
    fs::write(&older, "{}\n").unwrap();
    fs::write(&ignored, "{}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));

    let newer = day.join("rollout-2026-07-07-new.jsonl");
    fs::write(&newer, "{}\n").unwrap();

    let mut names: Vec<_> = list_rollouts(&root)
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "rollout-2026-07-07-new.jsonl".to_string(),
            "rollout-2026-07-07-old.jsonl".to_string()
        ]
    );
    assert_eq!(latest_rollout(&root).unwrap(), newer);

    fs::remove_dir_all(&root).unwrap();
}

#[cfg(windows)]
fn create_dir_link(target: &Path, link: &Path) {
    let status = Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(unix)]
fn create_dir_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn remove_dir_link(link: &Path) {
    fs::remove_dir(link).unwrap();
}

#[cfg(unix)]
fn remove_dir_link(link: &Path) {
    fs::remove_file(link).unwrap();
}

#[cfg(any(windows, unix))]
#[test]
fn rollout_walk_skips_cycles_and_links_outside_sessions() {
    let root = unique_temp_dir();
    let outside = root.with_extension("outside");
    let day = root.join("2026").join("07").join("11");
    fs::create_dir_all(&day).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let inside = day.join("rollout-inside.jsonl");
    let outside_rollout = outside.join("rollout-outside.jsonl");
    fs::write(&inside, "{}\n").unwrap();
    fs::write(&outside_rollout, "{}\n").unwrap();
    let cycle = day.join("ancestor-cycle");
    let outside_link = day.join("outside-link");
    create_dir_link(&root, &cycle);
    create_dir_link(&outside, &outside_link);

    let started = Instant::now();
    let rollouts = list_rollouts(&root);

    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(rollouts, vec![inside]);

    remove_dir_link(&cycle);
    remove_dir_link(&outside_link);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn rollout_walk_stops_beyond_the_date_tree_depth() {
    let root = unique_temp_dir();
    let day = root.join("2026").join("07").join("11");
    let too_deep = day.join("extra").join("nested");
    fs::create_dir_all(&too_deep).unwrap();
    let expected = day.join("rollout-date-tree.jsonl");
    fs::write(&expected, "{}\n").unwrap();
    fs::write(too_deep.join("rollout-too-deep.jsonl"), "{}\n").unwrap();

    assert_eq!(list_rollouts(&root), vec![expected]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recent_rollouts_are_mtime_desc_and_limited() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();

    let oldest = root.join("rollout-oldest.jsonl");
    fs::write(&oldest, "{}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let middle = root.join("rollout-middle.jsonl");
    fs::write(&middle, "{}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let newest = root.join("rollout-newest.jsonl");
    fs::write(&newest, "{}\n").unwrap();

    let names: Vec<_> = recent_rollouts(&root, 2)
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();

    assert_eq!(
        names,
        vec![
            "rollout-newest.jsonl".to_string(),
            "rollout-middle.jsonl".to_string()
        ]
    );

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn rollout_cache_preserves_last_candidates_when_deadline_expires() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let existing = root.join("rollout-existing.jsonl");
    fs::write(&existing, "{}\n").unwrap();
    let mut cache = RolloutCache::default();
    let now = Instant::now();

    assert_eq!(
        cache.recent(&root, 1, false, Duration::from_secs(60), now),
        vec![existing.clone()]
    );
    assert_eq!(
        cache.recent_with_deadline(
            &root,
            1,
            true,
            Duration::from_secs(60),
            now + Duration::from_secs(1),
            Some(Instant::now()),
        ),
        vec![existing]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollout_cache_retries_after_an_incomplete_first_scan() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let existing = root.join("rollout-existing.jsonl");
    fs::write(&existing, "{}\n").unwrap();
    let mut cache = RolloutCache::default();
    let now = Instant::now();

    assert!(cache
        .recent_with_deadline(
            &root,
            1,
            false,
            Duration::from_secs(60),
            now,
            Some(Instant::now()),
        )
        .is_empty());
    assert_eq!(
        cache.recent(
            &root,
            1,
            false,
            Duration::from_secs(60),
            now + Duration::from_millis(1),
        ),
        vec![existing]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollout_cache_retries_after_a_missing_sessions_directory() {
    let root = unique_temp_dir();
    let mut cache = RolloutCache::default();
    let now = Instant::now();

    assert!(cache
        .recent(&root, 1, false, Duration::from_secs(60), now)
        .is_empty());
    fs::create_dir_all(&root).unwrap();
    let created = root.join("rollout-created.jsonl");
    fs::write(&created, "{}\n").unwrap();
    assert_eq!(
        cache.recent(
            &root,
            1,
            false,
            Duration::from_secs(60),
            now + Duration::from_millis(1),
        ),
        vec![created]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bounded_rollout_tail_stops_before_old_token_events() {
    let root = unique_temp_dir();
    fs::create_dir_all(&root).unwrap();
    let path = root.join("rollout-large.jsonl");
    let token = r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1}}}}"#;
    let mut contents = format!("{token}\n");
    contents.push_str(&"x\n".repeat(40_000));
    fs::write(&path, contents).unwrap();

    assert!(last_token_count_line_from_file(&path).unwrap().is_some());
    assert_eq!(
        last_token_count_line_from_file_until(
            &path,
            Instant::now() + Duration::from_secs(1),
            64 * 1024,
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollout_cache_refreshes_on_force_candidate_mtime_and_period_expiry() {
    let root = unique_temp_dir();
    let day = root.join("2026").join("07").join("11");
    fs::create_dir_all(&day).unwrap();
    let first = day.join("rollout-first.jsonl");
    fs::write(&first, "{}\n").unwrap();

    let started_at = Instant::now();
    let mut cache = RolloutCache::default();
    assert_eq!(
        cache.recent(&root, 1, false, Duration::from_secs(60), started_at),
        vec![first.clone()]
    );

    std::thread::sleep(Duration::from_millis(20));
    let second = day.join("rollout-second.jsonl");
    fs::write(&second, "{}\n").unwrap();
    assert_eq!(
        cache.recent(
            &root,
            1,
            false,
            Duration::from_secs(60),
            started_at + Duration::from_secs(1),
        ),
        vec![first.clone()]
    );
    assert_eq!(
        cache.recent(
            &root,
            1,
            true,
            Duration::from_secs(60),
            started_at + Duration::from_secs(2),
        ),
        vec![second.clone()]
    );

    std::thread::sleep(Duration::from_millis(20));
    fs::write(&second, "{\"updated\":true}\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let third = day.join("rollout-third.jsonl");
    fs::write(&third, "{}\n").unwrap();
    let refreshed = cache.recent(
        &root,
        1,
        false,
        Duration::from_secs(60),
        started_at + Duration::from_secs(3),
    );
    assert_eq!(refreshed, vec![third]);

    std::thread::sleep(Duration::from_millis(20));
    let fourth = day.join("rollout-fourth.jsonl");
    fs::write(&fourth, "{}\n").unwrap();
    assert_eq!(
        cache.recent(
            &root,
            1,
            false,
            Duration::from_secs(60),
            started_at + Duration::from_secs(63),
        ),
        vec![fourth]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn derives_session_id_from_rollout_stem() {
    let path = Path::new("sessions/2026/07/07/rollout-2026-07-07-session-uuid.jsonl");

    assert_eq!(
        session_id_of(path),
        "rollout-2026-07-07-session-uuid".to_string()
    );
}
