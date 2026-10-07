use agent_juice::activity::{refresh_at, ActivityRoots, ActivitySnapshot, ScanOptions};
use chrono::{FixedOffset, TimeZone, Utc};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum Tool {
    Claude,
    Grok,
}

impl Tool {
    fn event(self, tokens: u64, id: Option<&str>, oversized: bool) -> String {
        let timestamp = "2026-09-30T12:00:00Z";
        let mut value = match self {
            Self::Claude => json!({
                "timestamp": timestamp,
                "message": {"usage": {"input_tokens": tokens}}
            }),
            Self::Grok => json!({
                "timestamp": timestamp,
                "method": "session/update",
                "params": {"update": {
                    "sessionUpdate": "response_completed",
                    "usage": {"input_tokens": tokens}
                }}
            }),
        };
        if let Some(id) = id {
            match self {
                Self::Claude => value["message"]["id"] = json!(id),
                Self::Grok => value["params"]["_meta"] = json!({"eventId": id}),
            }
        }
        if oversized {
            value["padding"] = json!("x".repeat(8 * 1024 * 1024 + 512));
        }
        value.to_string() + "\n"
    }

    fn contributions(self) -> &'static str {
        match self {
            Self::Claude => "claude_messages",
            Self::Grok => "grok_responses",
        }
    }
}

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    index: PathBuf,
    roots: ActivityRoots,
    tool: Tool,
}

impl Fixture {
    fn new(tool: Tool) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "agent-juice-activity-backup-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        let folder = root.join(match tool {
            Tool::Claude => "claude",
            Tool::Grok => "grok",
        });
        fs::create_dir_all(&folder).unwrap();
        let source = folder.join(match tool {
            Tool::Claude => "session.jsonl",
            Tool::Grok => "updates.jsonl",
        });
        let roots = ActivityRoots {
            claude: matches!(tool, Tool::Claude).then_some(folder.clone()),
            codex: None,
            grok: matches!(tool, Tool::Grok).then_some(folder),
        };
        Self {
            index: root.join("index.json"),
            root,
            source,
            roots,
            tool,
        }
    }

    fn refresh(&self, max_bytes: u64) -> ActivitySnapshot {
        self.refresh_timezone(max_bytes, 0)
    }

    fn refresh_timezone(&self, max_bytes: u64, offset_seconds: i32) -> ActivitySnapshot {
        refresh_at(
            &self.index,
            &self.roots,
            matches!(self.tool, Tool::Claude),
            false,
            matches!(self.tool, Tool::Grok),
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            FixedOffset::east_opt(offset_seconds).unwrap(),
            ScanOptions {
                max_bytes,
                max_duration: Duration::from_secs(30),
                ..ScanOptions::default()
            },
        )
        .unwrap()
    }

    fn complete(&self) -> ActivitySnapshot {
        self.refresh(64 * 1024 * 1024)
    }

    fn cache(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.index).unwrap()).unwrap()
    }

    fn save(&self, value: &Value) {
        fs::write(&self.index, serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn migrate(&self) {
        let mut value = self.cache();
        value.as_object_mut().unwrap().remove("timezone_identity");
        self.save(&value);
    }

    fn anonymous_history(&self) {
        fs::write(
            &self.source,
            self.tool.event(10, None, false) + &self.tool.event(20, None, false),
        )
        .unwrap();
        assert_eq!(total(&self.complete()), 30);
        self.migrate();
    }

    fn aggregate_history(&self) {
        fs::write(&self.source, self.tool.event(30, Some("old"), false)).unwrap();
        assert_eq!(total(&self.complete()), 30);
        let mut cache = self.cache();
        let current = cache["files"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        current["days"] = json!({"2026-09-30": 30});
        current["claude_messages"] = json!({});
        current["grok_responses"] = json!({});
        // Match old serde caches without the newer optional scanner fields.
        for field in [
            "source_modified_nanos",
            "ambiguous_stable_ids",
            "prefix_digest",
            "prefix_needs_validation",
            "prefix_validation",
            "anonymous_source_verified",
        ] {
            current.as_object_mut().unwrap().remove(field);
        }
        cache.as_object_mut().unwrap().remove("timezone_identity");
        self.save(&cache);
        fs::remove_file(&self.source).unwrap();
        let snapshot = self.complete();
        assert_eq!(total(&snapshot), 30);
        assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
    }

    fn assert_preserved_history(&self, expected: u64) {
        let cache = self.cache();
        let backup = cache["legacy_files"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .or_else(|| cache["files"].as_object().unwrap().values().next())
            .unwrap();
        let contributions = backup[self.tool.contributions()].as_object().unwrap();
        let tokens = if contributions.is_empty() {
            backup["days"]
                .as_object()
                .unwrap()
                .values()
                .map(|tokens| tokens.as_u64().unwrap())
                .sum::<u64>()
        } else {
            contributions
                .values()
                .map(|value| value["tokens"].as_u64().unwrap())
                .sum::<u64>()
        };
        assert_eq!(tokens, expected);
    }

    fn assert_missing_history(&self, expected: u64) {
        fs::remove_file(&self.source).unwrap();
        for _ in 0..3 {
            self.migrate();
            let snapshot = self.complete();
            assert_eq!(total(&snapshot), expected);
            assert!(!snapshot.local_backfill_pending);
            assert_eq!(snapshot.local_partial, expected != 0);
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn total(snapshot: &ActivitySnapshot) -> u64 {
    snapshot
        .days
        .iter()
        .map(|day| day.claude_tokens + day.grok_tokens)
        .sum()
}

fn checkpoint(cache: &Value, group: &str) -> Value {
    cache[group]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone()
}

#[test]
fn aggregate_backup_survives_stable_id_reread_and_repeated_timezone_migrations() {
    for tool in [Tool::Claude, Tool::Grok] {
        for tokens in [10, 50] {
            let fixture = Fixture::new(tool);
            fixture.aggregate_history();
            fs::write(&fixture.source, tool.event(tokens, Some("restored"), false)).unwrap();
            assert_eq!(total(&fixture.complete()), tokens);
            for seconds in [3600, 7200, 10800] {
                let snapshot = fixture.refresh_timezone(64 * 1024 * 1024, seconds);
                assert_eq!(total(&snapshot), tokens);
                assert_eq!(snapshot.local_partial, tokens < 30);
                assert!(!snapshot.local_backfill_pending);
                fixture.assert_preserved_history(tokens.max(30));
                if tokens < 30 {
                    let backup = checkpoint(&fixture.cache(), "legacy_files");
                    assert!(backup[tool.contributions()].as_object().unwrap().is_empty());
                }
            }
            fs::remove_file(&fixture.source).unwrap();
            for seconds in [14400, 18000, 21600] {
                let snapshot = fixture.refresh_timezone(64 * 1024 * 1024, seconds);
                assert_eq!(total(&snapshot), tokens.max(30));
                assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
                fixture.assert_preserved_history(tokens.max(30));
            }
        }
    }
}

#[test]
fn selected_identified_history_keeps_cross_file_deduplication_after_source_loss() {
    for tool in [Tool::Claude, Tool::Grok] {
        for tokens in [30, 50] {
            let fixture = Fixture::new(tool);
            fixture.aggregate_history();
            let copy_folder = fixture.source.parent().unwrap().join("copy");
            fs::create_dir_all(&copy_folder).unwrap();
            let copy = copy_folder.join(fixture.source.file_name().unwrap());
            let event = tool.event(tokens, Some("shared-stable"), false);
            fs::write(&fixture.source, &event).unwrap();
            fs::write(&copy, &event).unwrap();
            assert_eq!(total(&fixture.complete()), tokens);
            assert_eq!(
                total(&fixture.refresh_timezone(64 * 1024 * 1024, 3600)),
                tokens
            );
            fs::remove_file(&fixture.source).unwrap();
            for seconds in [7200, 10800, 14400] {
                let snapshot = fixture.refresh_timezone(64 * 1024 * 1024, seconds);
                assert_eq!(total(&snapshot), tokens);
                assert!(!snapshot.local_backfill_pending);
            }
            fs::remove_file(&copy).unwrap();
            for seconds in [18000, 21600, 25200] {
                let snapshot = fixture.refresh_timezone(64 * 1024 * 1024, seconds);
                assert_eq!(total(&snapshot), tokens);
                assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
                let cache = fixture.cache();
                assert!(cache["legacy_files"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(
                        |value| value[tool.contributions()]["shared-stable"]["tokens"]
                            == json!(tokens)
                    ));
            }
        }
    }
}

#[test]
fn aggregate_backup_preserves_larger_mixed_or_anonymous_history_without_union() {
    for tool in [Tool::Claude, Tool::Grok] {
        for stable in [false, true] {
            for tokens in [7, 40] {
                let fixture = Fixture::new(tool);
                fixture.aggregate_history();
                fs::write(
                    &fixture.source,
                    tool.event(10, None, false)
                        + &tool.event(tokens, stable.then_some("restored"), false),
                )
                .unwrap();
                for _ in 0..3 {
                    assert_eq!(total(&fixture.complete()), 10 + tokens);
                    fixture.migrate();
                }
                fixture.assert_missing_history((10 + tokens).max(30));
                fixture.assert_preserved_history((10 + tokens).max(30));
            }
        }
    }
}

#[test]
fn aggregate_backup_is_not_replaced_by_incomplete_larger_history() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.aggregate_history();
        fs::write(
            &fixture.source,
            tool.event(50, Some("first"), false) + &tool.event(20, Some("unread"), false),
        )
        .unwrap();
        let snapshot = fixture.refresh(1);
        assert_eq!(total(&snapshot), 50);
        assert!(snapshot.local_partial && snapshot.local_backfill_pending);
        fixture.assert_missing_history(30);
        fixture.assert_preserved_history(30);
    }
}

#[test]
fn aggregate_backup_is_not_replaced_by_lossy_larger_history() {
    for tool in [Tool::Claude, Tool::Grok] {
        for stable in [false, true] {
            let fixture = Fixture::new(tool);
            fixture.aggregate_history();
            fs::write(
                &fixture.source,
                tool.event(90, None, true) + &tool.event(50, stable.then_some("survivor"), false),
            )
            .unwrap();
            let snapshot = fixture.complete();
            assert_eq!(total(&snapshot), 50);
            assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
            fixture.assert_missing_history(30);
            fixture.assert_preserved_history(30);
        }
    }
}

#[test]
fn aggregate_backup_is_not_replaced_while_prefix_validation_is_pending() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.aggregate_history();
        fs::write(&fixture.source, tool.event(50, Some("restored"), false)).unwrap();
        assert_eq!(total(&fixture.complete()), 50);
        let mut cache = fixture.cache();
        let current = cache["files"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        current["prefix_needs_validation"] = json!(true);
        fixture.save(&cache);
        fixture.assert_missing_history(30);
        fixture.assert_preserved_history(30);
    }
}

#[test]
fn aggregate_backup_survives_complete_empty_reread() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.aggregate_history();
        fs::write(&fixture.source, "").unwrap();
        assert_eq!(total(&fixture.complete()), 0);
        fixture.assert_missing_history(30);
        fixture.assert_preserved_history(30);
    }
}

#[test]
fn aggregate_backup_ignores_expired_days_when_preserving_larger_history() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.aggregate_history();
        let mut cache = fixture.cache();
        let backup = cache["legacy_files"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        backup["days"]["2020-01-01"] = json!(1000);
        fixture.save(&cache);
        fs::write(&fixture.source, tool.event(50, Some("restored"), false)).unwrap();
        // Cache the new checkpoint before loading the stale backup on migration.
        assert_eq!(total(&fixture.complete()), 50);
        let mut cache = fixture.cache();
        for backup in cache["legacy_files"].as_object_mut().unwrap().values_mut() {
            backup["days"]["2020-01-01"] = json!(1000);
        }
        fixture.save(&cache);
        fixture.assert_missing_history(50);
        fixture.assert_preserved_history(50);
    }
}

#[test]
fn aggregate_backup_does_not_union_dates_across_midnight_timezone_changes() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.aggregate_history();
        let event = tool
            .event(50, Some("restored"), false)
            .replace("T12:00:00Z", "T23:30:00Z");
        fs::write(&fixture.source, event).unwrap();
        assert_eq!(total(&fixture.complete()), 50);
        for seconds in [3600, 7200, 10800] {
            assert_eq!(
                total(&fixture.refresh_timezone(64 * 1024 * 1024, seconds)),
                50
            );
            fixture.assert_preserved_history(50);
        }
        fs::remove_file(&fixture.source).unwrap();
        for seconds in [14400, 18000] {
            assert_eq!(
                total(&fixture.refresh_timezone(64 * 1024 * 1024, seconds)),
                50
            );
            fixture.assert_preserved_history(50);
        }
    }
}

#[test]
fn incoming_aggregate_history_does_not_mix_with_identified_backup() {
    for tool in [Tool::Claude, Tool::Grok] {
        for tokens in [10, 50] {
            let fixture = Fixture::new(tool);
            fs::write(&fixture.source, tool.event(30, Some("old"), false)).unwrap();
            assert_eq!(total(&fixture.complete()), 30);
            let mut cache = fixture.cache();
            cache["legacy_files"] = cache["files"].clone();
            let current = cache["files"]
                .as_object_mut()
                .unwrap()
                .values_mut()
                .next()
                .unwrap();
            current["days"] = json!({"2026-09-30": tokens});
            current["claude_messages"] = json!({});
            current["grok_responses"] = json!({});
            current
                .as_object_mut()
                .unwrap()
                .remove("source_modified_nanos");
            fixture.save(&cache);
            fixture.assert_missing_history(tokens.max(30));
            fixture.assert_preserved_history(tokens.max(30));
        }
    }
}

#[test]
fn aggregate_backup_is_pruned_after_retention_without_leaving_partial_state() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.aggregate_history();
        let mut cache = fixture.cache();
        for backup in cache["legacy_files"].as_object_mut().unwrap().values_mut() {
            backup["days"] = json!({"2020-01-01": 30});
        }
        fixture.save(&cache);
        for _ in 0..3 {
            fixture.migrate();
            let snapshot = fixture.complete();
            assert_eq!(total(&snapshot), 0);
            assert!(!snapshot.local_partial && !snapshot.local_backfill_pending);
            assert!(fixture.cache()["legacy_files"]
                .as_object()
                .unwrap()
                .is_empty());
        }
    }
}

fn lossy_eof_preserves_backup(stable: bool) {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.anonymous_history();
        fs::write(
            &fixture.source,
            tool.event(10, None, true)
                + &tool.event(20, stable.then_some("stable-survivor"), false),
        )
        .unwrap();
        let rebuilt = fixture.complete();
        assert_eq!(total(&rebuilt), 20);
        assert!(rebuilt.local_partial && !rebuilt.local_backfill_pending);
        let current = checkpoint(&fixture.cache(), "files");
        assert_eq!(current["offset"], current["observed_len"]);
        assert_eq!(current["lossy"], true);
        fixture.assert_missing_history(30);
        let backup = checkpoint(&fixture.cache(), "legacy_files");
        assert!(backup[tool.contributions()]
            .as_object()
            .unwrap()
            .keys()
            .all(|id| id.starts_with("offset:")));
    }
}

#[test]
fn lossy_anonymous_eof_does_not_replace_complete_backup() {
    lossy_eof_preserves_backup(false);
}

#[test]
fn lossy_stable_id_eof_does_not_union_with_anonymous_backup() {
    lossy_eof_preserves_backup(true);
}

#[test]
fn complete_stable_id_only_reread_replaces_anonymous_epoch_without_duplicates() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fs::write(&fixture.source, tool.event(10, None, false)).unwrap();
        assert_eq!(total(&fixture.complete()), 10);
        fixture.migrate();
        fs::write(&fixture.source, tool.event(10, Some("stable-event"), false)).unwrap();
        for _ in 0..3 {
            let rebuilt = fixture.complete();
            assert_eq!(total(&rebuilt), 10);
            assert!(!rebuilt.local_backfill_pending);
            let current = checkpoint(&fixture.cache(), "files");
            assert_eq!(current["offset"], current["observed_len"]);
            assert_eq!(current["lossy"], false);
            assert_eq!(current["prefix_needs_validation"], false);
            fixture.migrate();
        }
        fixture.assert_missing_history(10);
        let backup = checkpoint(&fixture.cache(), "legacy_files");
        assert!(backup[tool.contributions()]
            .as_object()
            .unwrap()
            .keys()
            .all(|id| !id.starts_with("offset:")));
    }
}

#[test]
fn incomplete_stable_id_reread_does_not_union_with_anonymous_backup() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.anonymous_history();
        fs::write(
            &fixture.source,
            tool.event(10, Some("stable-first"), false)
                + &tool.event(20, Some("stable-unread"), false),
        )
        .unwrap();
        let partial = fixture.refresh(1);
        assert_eq!(total(&partial), 10);
        assert!(partial.local_partial && partial.local_backfill_pending);
        let current = checkpoint(&fixture.cache(), "files");
        assert!(current["offset"].as_u64().unwrap() < current["observed_len"].as_u64().unwrap());
        assert_eq!(current["lossy"], false);
        fixture.assert_missing_history(30);
        let backup = checkpoint(&fixture.cache(), "legacy_files");
        assert!(backup[tool.contributions()].get("stable-first").is_none());
    }
}

#[test]
fn validation_pending_eof_does_not_replace_or_union_anonymous_backup() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.anonymous_history();
        fs::write(&fixture.source, tool.event(10, Some("stable-event"), false)).unwrap();
        assert_eq!(total(&fixture.complete()), 10);
        let mut cache = fixture.cache();
        // Persist the scanner's pending-validation boundary independently of scan timing.
        let current = cache["files"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        assert_eq!(current["offset"], current["observed_len"]);
        assert_eq!(current["lossy"], false);
        current["prefix_needs_validation"] = json!(true);
        fixture.save(&cache);
        fixture.assert_missing_history(30);
    }
}

#[test]
fn failed_prefix_validation_does_not_retire_anonymous_backup_as_empty() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        let filler = "{}\n".repeat(2500);
        fs::write(
            &fixture.source,
            tool.event(10, None, false) + &filler + &tool.event(20, None, false),
        )
        .unwrap();
        assert_eq!(total(&fixture.complete()), 30);
        fixture.migrate();
        assert_eq!(total(&fixture.refresh(5500)), 10);
        fs::write(
            &fixture.source,
            tool.event(90, None, false)
                + &filler
                + &tool.event(20, None, false)
                + &tool.event(40, None, false),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(&fixture.source)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        let mut reset_observed = false;
        for _ in 0..12 {
            let snapshot = fixture.refresh(5500);
            let current = checkpoint(&fixture.cache(), "files");
            if current["offset"] == 0 && current["observed_len"] == 0 {
                assert!(snapshot.local_backfill_pending);
                assert_eq!(current["source_modified_nanos"], 0);
                reset_observed = true;
                break;
            }
        }
        assert!(
            reset_observed,
            "changed prefix resets the collection checkpoint"
        );
        fixture.assert_missing_history(30);
    }
}

#[test]
fn complete_empty_reread_retires_anonymous_backup_on_repeated_migration() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fixture.anonymous_history();
        fs::write(&fixture.source, "").unwrap();
        fixture.complete();
        let current = checkpoint(&fixture.cache(), "files");
        assert_eq!(current["offset"], 0);
        assert_eq!(current["observed_len"], 0);
        assert_eq!(current["lossy"], false);
        assert_eq!(current["prefix_needs_validation"], false);
        fixture.assert_missing_history(0);
    }
}

#[test]
fn stable_known_ids_keep_historical_max_and_missing_source_history() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fs::write(
            &fixture.source,
            tool.event(30, Some("known"), false) + &tool.event(7, Some("missing"), false),
        )
        .unwrap();
        assert_eq!(total(&fixture.complete()), 37);
        fixture.migrate();
        fs::write(&fixture.source, tool.event(20, Some("known"), false)).unwrap();
        let expected = match tool {
            Tool::Claude => 37,
            Tool::Grok => 27,
        };
        for _ in 0..3 {
            assert_eq!(total(&fixture.complete()), expected);
            fixture.migrate();
        }
        fixture.assert_missing_history(expected);
    }
}

#[test]
fn incomplete_merge_updates_known_claude_id_without_lowering_historical_max() {
    let fixture = Fixture::new(Tool::Claude);
    fs::write(
        &fixture.source,
        Tool::Claude.event(30, Some("known"), false) + &Tool::Claude.event(10, None, false),
    )
    .unwrap();
    assert_eq!(total(&fixture.complete()), 40);
    let mut cache = fixture.cache();
    cache["legacy_files"] = cache["files"].clone();
    let current = cache["files"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    current["claude_messages"] = json!({
        "known": {"date": "2026-09-30", "tokens": 20},
        "new-overlapping": {"date": "2026-09-30", "tokens": 10}
    });
    current["days"] = json!({"2026-09-30": 30});
    current["observed_len"] = json!(current["offset"].as_u64().unwrap() + 1);
    fixture.save(&cache);
    fixture.assert_missing_history(40);
    let backup = checkpoint(&fixture.cache(), "legacy_files");
    assert_eq!(backup["claude_messages"]["known"]["tokens"], 30);
    assert!(backup["claude_messages"].get("new-overlapping").is_none());
}

#[test]
fn stable_to_anonymous_epochs_are_not_summed_or_deleted() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fs::write(&fixture.source, tool.event(10, Some("old-stable"), false)).unwrap();
        assert_eq!(total(&fixture.complete()), 10);
        fixture.migrate();
        fs::write(&fixture.source, tool.event(10, None, false)).unwrap();
        for _ in 0..3 {
            let snapshot = fixture.complete();
            assert_eq!(total(&snapshot), 10);
            assert!(snapshot.local_partial && !snapshot.local_backfill_pending);
            fixture.migrate();
        }
        fixture.assert_missing_history(10);
        let backup = checkpoint(&fixture.cache(), "legacy_files");
        assert!(backup[tool.contributions()].get("old-stable").is_some());
        assert_eq!(backup["ambiguous_stable_ids"], json!(["old-stable"]));
    }
}

#[test]
fn mixed_reread_keeps_current_stable_events_without_summing_old_epoch() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fs::write(&fixture.source, tool.event(10, Some("old-stable"), false)).unwrap();
        assert_eq!(total(&fixture.complete()), 10);
        fixture.migrate();
        fs::write(
            &fixture.source,
            tool.event(10, None, false) + &tool.event(7, Some("new-stable"), false),
        )
        .unwrap();
        for _ in 0..3 {
            assert_eq!(total(&fixture.complete()), 17);
            fixture.migrate();
        }
        fixture.assert_missing_history(17);
    }
}

#[test]
fn partial_anonymous_reread_preserves_complete_stable_backup() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fs::write(
            &fixture.source,
            tool.event(10, Some("first"), false) + &tool.event(20, Some("second"), false),
        )
        .unwrap();
        assert_eq!(total(&fixture.complete()), 30);
        fixture.migrate();
        fs::write(
            &fixture.source,
            tool.event(10, None, false) + &tool.event(20, None, false),
        )
        .unwrap();
        let partial = fixture.refresh(1);
        assert_eq!(total(&partial), 10);
        assert!(partial.local_backfill_pending);
        fixture.assert_missing_history(30);
    }
}

#[test]
fn lossy_anonymous_reread_preserves_complete_stable_backup() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        fs::write(
            &fixture.source,
            tool.event(10, Some("first"), false) + &tool.event(20, Some("second"), false),
        )
        .unwrap();
        assert_eq!(total(&fixture.complete()), 30);
        fixture.migrate();
        fs::write(
            &fixture.source,
            tool.event(10, None, true) + &tool.event(20, None, false),
        )
        .unwrap();
        let lossy = fixture.complete();
        assert_eq!(total(&lossy), 20);
        assert!(lossy.local_partial && !lossy.local_backfill_pending);
        fixture.assert_missing_history(30);
    }
}

#[test]
fn legacy_without_digest_append_preserves_progress_then_finishes_validation() {
    use std::io::Write;
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        let filler = "{}\n".repeat(2500);
        fs::write(
            &fixture.source,
            tool.event(10, None, false) + &filler + &tool.event(20, None, false),
        )
        .unwrap();
        assert_eq!(total(&fixture.complete()), 30);
        let mut cache = fixture.cache();
        cache.as_object_mut().unwrap().remove("timezone_identity");
        for current in cache["files"].as_object_mut().unwrap().values_mut() {
            current.as_object_mut().unwrap().remove("prefix_digest");
        }
        fixture.save(&cache);
        assert_eq!(total(&fixture.refresh(5500)), 10);
        let before = checkpoint(&fixture.cache(), "files")["offset"]
            .as_u64()
            .unwrap();
        let mut source = fs::OpenOptions::new()
            .append(true)
            .open(&fixture.source)
            .unwrap();
        source
            .write_all(tool.event(40, None, false).as_bytes())
            .unwrap();
        source
            .set_modified(SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        drop(source);
        let mut snapshot = fixture.refresh(5500);
        let after = checkpoint(&fixture.cache(), "files")["offset"]
            .as_u64()
            .unwrap();
        assert!(after > before);
        for _ in 0..8 {
            if !snapshot.local_backfill_pending {
                break;
            }
            snapshot = fixture.complete();
        }
        assert_eq!(total(&snapshot), 70);
        assert!(!snapshot.local_backfill_pending);
        fixture.assert_missing_history(70);
    }
}

#[test]
fn legacy_without_digest_still_validates_new_collection_revision() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = Fixture::new(tool);
        let filler = "{}\n".repeat(2500);
        fs::write(
            &fixture.source,
            tool.event(10, None, false) + &filler + &tool.event(20, None, false),
        )
        .unwrap();
        assert_eq!(total(&fixture.complete()), 30);
        let mut cache = fixture.cache();
        cache.as_object_mut().unwrap().remove("timezone_identity");
        for current in cache["files"].as_object_mut().unwrap().values_mut() {
            current.as_object_mut().unwrap().remove("prefix_digest");
        }
        fixture.save(&cache);
        assert_eq!(total(&fixture.refresh(5500)), 10);
        fs::write(
            &fixture.source,
            tool.event(90, None, false)
                + &filler
                + &tool.event(20, None, false)
                + &tool.event(40, None, false),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(&fixture.source)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        let mut snapshot = fixture.complete();
        for _ in 0..4 {
            if !snapshot.local_backfill_pending {
                break;
            }
            snapshot = fixture.complete();
        }
        assert_eq!(total(&snapshot), 150);
        assert!(!snapshot.local_backfill_pending);
        fixture.assert_missing_history(150);
    }
}

fn retained_prefix_fixture(tool: Tool, stable: bool) -> Fixture {
    let fixture = Fixture::new(tool);
    let filler = "{}\n".repeat(2500);
    let first_id = stable.then_some("prefix-first");
    let second_id = stable.then_some("prefix-second");
    fs::write(
        &fixture.source,
        tool.event(10, first_id, false) + &filler + &tool.event(20, second_id, false),
    )
    .unwrap();
    let initial = fixture.complete();
    assert_eq!(total(&initial), 30);
    assert!(!initial.local_partial && !initial.local_backfill_pending);
    fs::write(
        &fixture.source,
        tool.event(90, first_id, false)
            + &filler
            + &tool.event(20, second_id, false)
            + &tool.event(40, stable.then_some("prefix-third"), false),
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(&fixture.source)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(1))
        .unwrap();
    let pending = fixture.refresh(1);
    assert_eq!(total(&pending), 70);
    assert!(pending.local_partial && pending.local_backfill_pending);
    let current = checkpoint(&fixture.cache(), "files");
    assert_eq!(current["offset"], current["observed_len"]);
    assert_eq!(current["prefix_needs_validation"], true);
    assert_eq!(current["lossy"], false);
    assert!(fixture.cache()["legacy_files"]
        .as_object()
        .unwrap()
        .is_empty());
    fixture
}

fn assert_retained_prefix_recovery(fixture: &Fixture) {
    let mut snapshot = fixture.complete();
    for _ in 0..4 {
        if !snapshot.local_backfill_pending {
            break;
        }
        snapshot = fixture.complete();
    }
    assert_eq!(total(&snapshot), 150);
    assert!(!snapshot.partial && !snapshot.backfill_pending);
    assert!(!snapshot.local_partial && !snapshot.local_backfill_pending);
    assert_eq!(
        checkpoint(&fixture.cache(), "files")["prefix_needs_validation"],
        false
    );
}

#[test]
fn retained_prefix_missing_source_or_root_stays_partial_and_recovers() {
    for tool in [Tool::Claude, Tool::Grok] {
        for stable in [false, true] {
            for missing_root in [false, true] {
                let fixture = retained_prefix_fixture(tool, stable);
                let pending = checkpoint(&fixture.cache(), "files");
                let source = if missing_root {
                    fixture.source.parent().unwrap().to_path_buf()
                } else {
                    fixture.source.clone()
                };
                let parked = fixture.root.join("parked-source.data");
                fs::rename(&source, &parked).unwrap();
                for _ in 0..3 {
                    let snapshot = fixture.complete();
                    assert_eq!(total(&snapshot), 70);
                    assert!(snapshot.partial && snapshot.local_partial);
                    assert!(!snapshot.backfill_pending && !snapshot.local_backfill_pending);
                    assert_eq!(checkpoint(&fixture.cache(), "files"), pending);
                }
                fs::rename(&parked, &source).unwrap();
                assert_retained_prefix_recovery(&fixture);
            }
        }
    }
}

#[test]
fn retained_prefix_missing_source_quality_survives_process_restart() {
    for tool in [Tool::Claude, Tool::Grok] {
        for stable in [false, true] {
            let fixture = retained_prefix_fixture(tool, stable);
            let pending = checkpoint(&fixture.cache(), "files");
            let parked = fixture.root.join("parked-source.data");
            fs::rename(&fixture.source, &parked).unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "retained_prefix_restart_child",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env("G3223_PREFIX_FIXTURE_ROOT", &fixture.root)
                .env(
                    "G3223_PREFIX_FIXTURE_TOOL",
                    if matches!(tool, Tool::Grok) {
                        "grok"
                    } else {
                        "claude"
                    },
                )
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "restart child failed:\n{}\n{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            assert_eq!(checkpoint(&fixture.cache(), "files"), pending);
            fs::rename(&parked, &fixture.source).unwrap();
            assert_retained_prefix_recovery(&fixture);
        }
    }
}

#[test]
fn retained_prefix_restart_child() {
    let Some(root) = std::env::var_os("G3223_PREFIX_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root).canonicalize().unwrap();
    assert!(root.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert!(root
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("agent-juice-activity-backup-"));
    let grok = std::env::var("G3223_PREFIX_FIXTURE_TOOL").unwrap() == "grok";
    let folder = root.join(if grok { "grok" } else { "claude" });
    let index = root.join("index.json");
    let roots = ActivityRoots {
        claude: (!grok).then_some(folder.clone()),
        codex: None,
        grok: grok.then_some(folder),
    };
    for _ in 0..3 {
        let snapshot = refresh_at(
            &index,
            &roots,
            !grok,
            false,
            grok,
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            FixedOffset::east_opt(0).unwrap(),
            ScanOptions {
                max_duration: Duration::from_secs(30),
                ..ScanOptions::default()
            },
        )
        .unwrap();
        assert_eq!(total(&snapshot), 70);
        assert!(snapshot.partial && snapshot.local_partial);
        assert!(!snapshot.backfill_pending && !snapshot.local_backfill_pending);
        let cache: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        assert_eq!(checkpoint(&cache, "files")["prefix_needs_validation"], true);
    }
}

#[test]
fn retained_prefix_disabled_tool_does_not_affect_active_quality() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = retained_prefix_fixture(tool, true);
        let pending = checkpoint(&fixture.cache(), "files");
        fs::rename(&fixture.source, fixture.root.join("parked-source.data")).unwrap();
        let other = match tool {
            Tool::Claude => Tool::Grok,
            Tool::Grok => Tool::Claude,
        };
        let other_folder = fixture.root.join(match other {
            Tool::Claude => "claude",
            Tool::Grok => "grok",
        });
        fs::create_dir_all(&other_folder).unwrap();
        fs::write(
            other_folder.join(match other {
                Tool::Claude => "session.jsonl",
                Tool::Grok => "updates.jsonl",
            }),
            other.event(50, Some("verified-other"), false),
        )
        .unwrap();
        let roots = ActivityRoots {
            claude: Some(fixture.root.join("claude")),
            codex: None,
            grok: Some(fixture.root.join("grok")),
        };
        for enabled in [false, true, false] {
            let snapshot = refresh_at(
                &fixture.index,
                &roots,
                matches!(other, Tool::Claude) || enabled,
                false,
                matches!(other, Tool::Grok) || enabled,
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                FixedOffset::east_opt(0).unwrap(),
                ScanOptions::default(),
            )
            .unwrap();
            assert_eq!(total(&snapshot), 120);
            assert_eq!(snapshot.partial, enabled);
            assert_eq!(snapshot.local_partial, enabled);
            assert!(!snapshot.backfill_pending && !snapshot.local_backfill_pending);
            assert_eq!(
                fixture.cache()["files"][fixture.source.to_string_lossy().as_ref()],
                pending
            );
        }
    }
}

#[test]
fn retained_prefix_verified_missing_history_has_no_quality_warning() {
    for tool in [Tool::Claude, Tool::Grok] {
        for stable in [false, true] {
            let fixture = Fixture::new(tool);
            fs::write(
                &fixture.source,
                tool.event(30, stable.then_some("verified"), false),
            )
            .unwrap();
            let initial = fixture.complete();
            assert_eq!(total(&initial), 30);
            assert!(!initial.local_partial && !initial.local_backfill_pending);
            let verified = checkpoint(&fixture.cache(), "files");
            assert_eq!(verified["prefix_needs_validation"], false);
            fs::rename(&fixture.source, fixture.root.join("parked-source.data")).unwrap();
            for _ in 0..3 {
                let snapshot = fixture.complete();
                assert_eq!(total(&snapshot), 30);
                assert!(!snapshot.partial && !snapshot.backfill_pending);
                assert!(!snapshot.local_partial && !snapshot.local_backfill_pending);
                assert_eq!(checkpoint(&fixture.cache(), "files"), verified);
            }
        }
    }
}

#[test]
fn retained_prefix_missing_history_expires_without_quality_warning() {
    for tool in [Tool::Claude, Tool::Grok] {
        let fixture = retained_prefix_fixture(tool, true);
        fs::rename(&fixture.source, fixture.root.join("parked-source.data")).unwrap();
        let snapshot = refresh_at(
            &fixture.index,
            &fixture.roots,
            matches!(tool, Tool::Claude),
            false,
            matches!(tool, Tool::Grok),
            Utc.with_ymd_and_hms(2027, 10, 7, 0, 0, 0).unwrap(),
            FixedOffset::east_opt(0).unwrap(),
            ScanOptions::default(),
        )
        .unwrap();
        assert!(snapshot.days.is_empty());
        assert!(!snapshot.partial && !snapshot.backfill_pending);
        assert!(!snapshot.local_partial && !snapshot.local_backfill_pending);
        assert!(fixture.cache()["files"].as_object().unwrap().is_empty());
    }
}
