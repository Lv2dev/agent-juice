use crate::collector;
use chrono::NaiveDate;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_BUCKETS: usize = 4096;
const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const NORMAL_FRESHNESS: Duration = Duration::from_secs(30);
const FORCE_COALESCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexActivityView {
    pub days: BTreeMap<String, u64>,
    pub partial: bool,
}

impl CodexActivityView {
    fn unavailable() -> Self {
        Self {
            days: BTreeMap::new(),
            partial: true,
        }
    }
}

#[derive(Debug, Deserialize)]
struct UsageEnvelope {
    result: Option<UsageResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageResult {
    summary: Option<UsageSummary>,
    daily_usage_buckets: Option<Vec<UsageBucket>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageSummary {
    lifetime_tokens: Option<u64>,
    peak_daily_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageBucket {
    start_date: String,
    tokens: u64,
}

fn safe_integer(value: u64, label: &str) -> anyhow::Result<u64> {
    if value > MAX_JS_SAFE_INTEGER {
        anyhow::bail!("Codex account activity {label} exceeded the wire integer limit");
    }
    Ok(value)
}

fn strict_date(value: &str) -> anyhow::Result<String> {
    let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("Codex account activity date rejected"))?;
    let canonical = parsed.format("%Y-%m-%d").to_string();
    if canonical != value {
        anyhow::bail!("Codex account activity date rejected");
    }
    Ok(canonical)
}

pub fn parse_response(raw: &str) -> anyhow::Result<CodexActivityView> {
    let envelope: UsageEnvelope = serde_json::from_str(raw)?;
    let result = envelope
        .result
        .ok_or_else(|| anyhow::anyhow!("Codex account activity result unavailable"))?;
    let buckets = result
        .daily_usage_buckets
        .ok_or_else(|| anyhow::anyhow!("Codex account activity buckets unavailable"))?;
    if buckets.len() > MAX_BUCKETS {
        anyhow::bail!("Codex account activity bucket limit exceeded");
    }

    let mut days = BTreeMap::new();
    let mut sum = 0u64;
    let mut maximum = 0u64;
    for bucket in buckets {
        let date = strict_date(&bucket.start_date)?;
        let tokens = safe_integer(bucket.tokens, "bucket")?;
        if days.insert(date, tokens).is_some() {
            anyhow::bail!("Codex account activity duplicate date rejected");
        }
        sum = sum
            .checked_add(tokens)
            .ok_or_else(|| anyhow::anyhow!("Codex account activity sum overflow"))?;
        safe_integer(sum, "sum")?;
        maximum = maximum.max(tokens);
    }

    let mut partial = result.summary.is_none();
    if let Some(summary) = result.summary {
        partial |= summary.lifetime_tokens.is_none() || summary.peak_daily_tokens.is_none();
        if let Some(lifetime) = summary.lifetime_tokens {
            if safe_integer(lifetime, "lifetime")? != sum {
                anyhow::bail!("Codex account activity lifetime mismatch");
            }
        }
        if let Some(peak) = summary.peak_daily_tokens {
            if safe_integer(peak, "peak")? != maximum {
                anyhow::bail!("Codex account activity peak mismatch");
            }
        }
    }

    Ok(CodexActivityView { days, partial })
}

#[derive(Default)]
struct FetchState {
    generation: Option<u64>,
    attempted_at: Option<Instant>,
    view: Option<CodexActivityView>,
}

#[derive(Default)]
struct CodexActivityCollector {
    gate: Mutex<()>,
    state: Mutex<FetchState>,
}

impl CodexActivityCollector {
    fn cached(
        &self,
        now: Instant,
        freshness: Duration,
        generation: u64,
    ) -> Option<CodexActivityView> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.generation != Some(generation) {
            return None;
        }
        state
            .attempted_at
            .filter(|attempted| now.saturating_duration_since(*attempted) < freshness)
            .and_then(|_| state.view.clone())
    }

    fn collect_with(
        &self,
        force: bool,
        now: Instant,
        deadline: Instant,
        current_generation: impl Fn() -> Option<u64>,
        fetch: impl FnOnce(Duration) -> anyhow::Result<String>,
    ) -> CodexActivityView {
        let Some(generation) = current_generation() else {
            return CodexActivityView::unavailable();
        };
        let freshness = if force {
            FORCE_COALESCE
        } else {
            NORMAL_FRESHNESS
        };
        if let Some(view) = self.cached(now, freshness, generation) {
            return if current_generation() == Some(generation) {
                view
            } else {
                CodexActivityView::unavailable()
            };
        }

        let _gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        if current_generation() != Some(generation) {
            return CodexActivityView::unavailable();
        }
        if let Some(view) = self.cached(now, freshness, generation) {
            return if current_generation() == Some(generation) {
                view
            } else {
                CodexActivityView::unavailable()
            };
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if current_generation() != Some(generation) {
            return CodexActivityView::unavailable();
        }
        let result = if timeout.is_zero() {
            Ok(CodexActivityView::unavailable())
        } else {
            fetch(timeout).and_then(|raw| parse_response(&raw))
        };
        let cancelled = result
            .as_ref()
            .err()
            .is_some_and(|error| error.is::<collector::CollectionPolicyCancelled>());
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if cancelled || current_generation() != Some(generation) {
            *state = FetchState::default();
            return CodexActivityView::unavailable();
        }
        let view = result.unwrap_or_else(|_| CodexActivityView::unavailable());
        *state = FetchState {
            generation: Some(generation),
            attempted_at: Some(now),
            view: Some(view.clone()),
        };
        if current_generation() != Some(generation) {
            *state = FetchState::default();
            return CodexActivityView::unavailable();
        }
        view
    }
}

static COLLECTOR: Lazy<CodexActivityCollector> = Lazy::new(CodexActivityCollector::default);

pub fn collect(force: bool, deadline: Instant) -> CodexActivityView {
    COLLECTOR.collect_with(
        force,
        Instant::now(),
        deadline,
        collector::codex_collection_generation,
        collector::codex_account_usage_response,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Barrier,
    };

    fn response(summary: &str, buckets: &str) -> String {
        format!(r#"{{"result":{{"summary":{summary},"dailyUsageBuckets":{buckets}}}}}"#)
    }

    fn token_response(tokens: u64) -> String {
        response(
            &format!(r#"{{"lifetimeTokens":{tokens},"peakDailyTokens":{tokens}}}"#),
            &format!(r#"[{{"startDate":"2026-08-25","tokens":{tokens}}}]"#),
        )
    }

    fn admission(policy: &AtomicU64) -> Option<u64> {
        let generation = policy.load(Ordering::SeqCst);
        (generation & 1 != 0).then_some(generation)
    }

    fn fetch_outcome(outcome: &str) -> anyhow::Result<String> {
        match outcome {
            "success" => Ok(token_response(111)),
            "login" => anyhow::bail!("authentication required"),
            "parse" => Ok("{invalid json".to_owned()),
            "transport" => anyhow::bail!("transport timed out"),
            "cancellation text" => anyhow::bail!("command unavailable by collection policy"),
            _ => panic!("unknown test outcome"),
        }
    }

    fn assert_uncached(collector: &CodexActivityCollector) {
        let state = collector.state.lock().unwrap();
        assert_eq!(state.generation, None);
        assert_eq!(state.attempted_at, None);
        assert_eq!(state.view, None);
    }

    #[test]
    fn parser_accepts_consistent_daily_buckets_and_preserves_source_dates() {
        let parsed = parse_response(&response(
            r#"{"lifetimeTokens":30,"peakDailyTokens":20}"#,
            r#"[{"startDate":"2026-08-25","tokens":10},{"startDate":"2026-08-26","tokens":20}]"#,
        ))
        .unwrap();

        assert_eq!(parsed.days.get("2026-08-25"), Some(&10));
        assert_eq!(parsed.days.get("2026-08-26"), Some(&20));
        assert!(!parsed.partial);
    }

    #[test]
    fn parser_marks_valid_buckets_partial_when_summary_is_missing() {
        let parsed = parse_response(&response(
            "null",
            r#"[{"startDate":"2026-08-25","tokens":10}]"#,
        ))
        .unwrap();

        assert_eq!(parsed.days.get("2026-08-25"), Some(&10));
        assert!(parsed.partial);
    }

    #[test]
    fn parser_rejects_missing_duplicate_invalid_and_inconsistent_buckets() {
        for raw in [
            response(r#"{"lifetimeTokens":0,"peakDailyTokens":0}"#, "null"),
            response(
                r#"{"lifetimeTokens":20,"peakDailyTokens":10}"#,
                r#"[{"startDate":"2026-08-25","tokens":10},{"startDate":"2026-08-25","tokens":10}]"#,
            ),
            response(
                r#"{"lifetimeTokens":10,"peakDailyTokens":10}"#,
                r#"[{"startDate":"2026-02-30","tokens":10}]"#,
            ),
            response(
                r#"{"lifetimeTokens":11,"peakDailyTokens":10}"#,
                r#"[{"startDate":"2026-08-25","tokens":10}]"#,
            ),
            response(
                r#"{"lifetimeTokens":10,"peakDailyTokens":9}"#,
                r#"[{"startDate":"2026-08-25","tokens":10}]"#,
            ),
        ] {
            assert!(parse_response(&raw).is_err());
        }
    }

    #[test]
    fn parser_rejects_values_above_the_javascript_safe_integer_limit() {
        let value = MAX_JS_SAFE_INTEGER + 1;
        assert!(parse_response(&response(
            &format!(r#"{{"lifetimeTokens":{value},"peakDailyTokens":{value}}}"#),
            &format!(r#"[{{"startDate":"2026-08-25","tokens":{value}}}]"#),
        ))
        .is_err());
    }

    #[test]
    fn failed_actual_refresh_replaces_success_instead_of_using_last_good() {
        let collector = CodexActivityCollector::default();
        let now = Instant::now();
        let success = collector.collect_with(
            false,
            now,
            now + Duration::from_secs(5),
            || Some(1),
            |_| {
                Ok(response(
                    r#"{"lifetimeTokens":10,"peakDailyTokens":10}"#,
                    r#"[{"startDate":"2026-08-25","tokens":10}]"#,
                ))
            },
        );
        assert_eq!(success.days.get("2026-08-25"), Some(&10));

        let failed = collector.collect_with(
            true,
            now + FORCE_COALESCE + Duration::from_millis(1),
            Instant::now() + Duration::from_secs(5),
            || Some(1),
            |_| anyhow::bail!("authentication required"),
        );
        assert!(failed.days.is_empty());
        assert!(failed.partial);
    }

    #[test]
    fn concurrent_force_requests_share_one_actual_fetch() {
        let collector = Arc::new(CodexActivityCollector::default());
        let barrier = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let now = Instant::now();
        let mut threads = Vec::new();
        for _ in 0..2 {
            let collector = Arc::clone(&collector);
            let barrier = Arc::clone(&barrier);
            let calls = Arc::clone(&calls);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                collector.collect_with(
                    true,
                    now,
                    Instant::now() + Duration::from_secs(5),
                    || Some(1),
                    |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(response(
                            r#"{"lifetimeTokens":1,"peakDailyTokens":1}"#,
                            r#"[{"startDate":"2026-08-25","tokens":1}]"#,
                        ))
                    },
                )
            }));
        }
        for thread in threads {
            assert_eq!(thread.join().unwrap().days.get("2026-08-25"), Some(&1));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn epoch_off_on_force_at_one_second_refetches_instead_of_returning_old_success() {
        let collector = CodexActivityCollector::default();
        let policy = AtomicU64::new(1);
        let calls = AtomicUsize::new(0);
        let now = Instant::now();
        let first = collector.collect_with(
            false,
            now,
            now + Duration::from_secs(5),
            || admission(&policy),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(token_response(111))
            },
        );
        assert_eq!(first.days.get("2026-08-25"), Some(&111));
        policy.store(2, Ordering::SeqCst);
        assert_eq!(admission(&policy), None);
        policy.store(3, Ordering::SeqCst);
        let second = collector.collect_with(
            true,
            now + Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
            || admission(&policy),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(token_response(222))
            },
        );
        assert_eq!(second.days.get("2026-08-25"), Some(&222));
        assert!(!second.partial);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn epoch_off_on_normal_at_one_second_recovers_from_old_failure_cache() {
        let collector = CodexActivityCollector::default();
        let policy = AtomicU64::new(1);
        let now = Instant::now();
        let failed = collector.collect_with(
            false,
            now,
            now + Duration::from_secs(5),
            || admission(&policy),
            |_| anyhow::bail!("transport timed out"),
        );
        assert_eq!(failed, CodexActivityView::unavailable());
        policy.store(2, Ordering::SeqCst);
        assert_eq!(admission(&policy), None);
        policy.store(3, Ordering::SeqCst);
        let recovered = collector.collect_with(
            false,
            now + Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
            || admission(&policy),
            |_| Ok(token_response(222)),
        );
        assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
        assert!(!recovered.partial);
    }

    #[test]
    fn same_epoch_success_and_failures_preserve_normal_and_force_freshness_boundaries() {
        for outcome in [
            "success",
            "login",
            "parse",
            "transport",
            "cancellation text",
        ] {
            for (force, freshness) in [(false, NORMAL_FRESHNESS), (true, FORCE_COALESCE)] {
                let collector = CodexActivityCollector::default();
                let now = Instant::now();
                let initial = collector.collect_with(
                    false,
                    now,
                    now + Duration::from_secs(5),
                    || Some(1),
                    |_| fetch_outcome(outcome),
                );
                if outcome == "success" {
                    assert_eq!(initial.days.get("2026-08-25"), Some(&111));
                    assert!(!initial.partial);
                } else {
                    assert_eq!(initial, CodexActivityView::unavailable(), "{outcome}");
                }
                let cached = collector.collect_with(
                    force,
                    now + freshness - Duration::from_millis(1),
                    Instant::now() + Duration::from_secs(5),
                    || Some(1),
                    |_| panic!("same-epoch fresh result must coalesce: {outcome}"),
                );
                assert_eq!(cached, initial);
                let refreshed = collector.collect_with(
                    force,
                    now + freshness,
                    Instant::now() + Duration::from_secs(5),
                    || Some(1),
                    |_| Ok(token_response(222)),
                );
                assert_eq!(refreshed.days.get("2026-08-25"), Some(&222));
                let state = collector.state.lock().unwrap();
                assert_eq!(state.generation, Some(1));
                assert_eq!(state.attempted_at, Some(now + freshness));
            }
        }
    }

    #[test]
    fn login_parse_and_transport_refresh_failures_replace_success_and_recover_in_new_epoch() {
        for outcome in ["login", "parse", "transport"] {
            let collector = CodexActivityCollector::default();
            let policy = AtomicU64::new(1);
            let now = Instant::now();
            let attempted = now + FORCE_COALESCE + Duration::from_millis(1);
            let success = collector.collect_with(
                false,
                now,
                now + Duration::from_secs(5),
                || admission(&policy),
                |_| Ok(token_response(111)),
            );
            assert_eq!(success.days.get("2026-08-25"), Some(&111));
            let failed = collector.collect_with(
                true,
                attempted,
                Instant::now() + Duration::from_secs(5),
                || admission(&policy),
                |_| fetch_outcome(outcome),
            );
            assert_eq!(failed, CodexActivityView::unavailable(), "{outcome}");
            let cached = collector.collect_with(
                false,
                attempted + Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
                || admission(&policy),
                |_| panic!("real failure must keep normal freshness: {outcome}"),
            );
            assert_eq!(cached, failed);
            policy.store(2, Ordering::SeqCst);
            policy.store(3, Ordering::SeqCst);
            let recovered = collector.collect_with(
                false,
                attempted + Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
                || admission(&policy),
                |_| Ok(token_response(222)),
            );
            assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
            assert!(!recovered.partial);
        }
    }

    #[test]
    fn disabled_or_shutdown_generation_neither_dispatches_nor_returns_cached_success() {
        for seed_cache in [false, true] {
            let collector = CodexActivityCollector::default();
            let now = Instant::now();
            if seed_cache {
                collector.collect_with(
                    false,
                    now,
                    now + Duration::from_secs(5),
                    || Some(1),
                    |_| Ok(token_response(111)),
                );
            }
            let unavailable = collector.collect_with(
                true,
                now + Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
                || None,
                |_| panic!("disabled or shutdown policy must not dispatch"),
            );
            assert_eq!(unavailable, CodexActivityView::unavailable());
            if !seed_cache {
                assert_uncached(&collector);
            }
            let recovered = collector.collect_with(
                false,
                now + Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
                || Some(3),
                |_| Ok(token_response(222)),
            );
            assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
        }
    }

    #[test]
    fn policy_change_while_waiting_for_gate_cancels_without_dispatch() {
        let collector = Arc::new(CodexActivityCollector::default());
        let policy = Arc::new(AtomicU64::new(1));
        let gate = collector.gate.lock().unwrap();
        let (captured, ready) = std::sync::mpsc::channel();
        let worker_collector = Arc::clone(&collector);
        let worker_policy = Arc::clone(&policy);
        let worker = std::thread::spawn(move || {
            worker_collector.collect_with(
                true,
                Instant::now(),
                Instant::now() + Duration::from_secs(5),
                || {
                    let generation = admission(&worker_policy);
                    captured.send(()).unwrap();
                    generation
                },
                |_| panic!("old queued epoch must not dispatch"),
            )
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        policy.store(2, Ordering::SeqCst);
        policy.store(3, Ordering::SeqCst);
        drop(gate);
        assert_eq!(worker.join().unwrap(), CodexActivityView::unavailable());
        assert_uncached(&collector);
    }

    #[test]
    fn policy_change_immediately_before_dispatch_does_not_fetch_or_cache() {
        for next_generation in [None, Some(3)] {
            let collector = CodexActivityCollector::default();
            let samples = AtomicUsize::new(0);
            let now = Instant::now();
            let unavailable = collector.collect_with(
                false,
                now,
                now + Duration::from_secs(5),
                || match samples.fetch_add(1, Ordering::SeqCst) {
                    0 | 1 => Some(1),
                    _ => next_generation,
                },
                |_| panic!("policy changed at dispatch admission"),
            );
            assert_eq!(unavailable, CodexActivityView::unavailable());
            assert_eq!(samples.load(Ordering::SeqCst), 3);
            assert_uncached(&collector);
        }
    }

    #[test]
    fn policy_change_after_cached_clone_discards_it_before_return() {
        for outcome in ["success", "transport"] {
            let collector = CodexActivityCollector::default();
            let now = Instant::now();
            collector.collect_with(
                false,
                now,
                now + Duration::from_secs(5),
                || Some(1),
                |_| fetch_outcome(outcome),
            );
            let samples = AtomicUsize::new(0);
            let unavailable = collector.collect_with(
                true,
                now + Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
                || {
                    if samples.fetch_add(1, Ordering::SeqCst) == 0 {
                        Some(1)
                    } else {
                        Some(3)
                    }
                },
                |_| panic!("fresh lookup must not dispatch"),
            );
            assert_eq!(unavailable, CodexActivityView::unavailable());
            assert_eq!(samples.load(Ordering::SeqCst), 2);
            let recovered = collector.collect_with(
                true,
                now + Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
                || Some(3),
                |_| Ok(token_response(222)),
            );
            assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
        }
    }

    #[test]
    fn policy_change_during_fetch_discards_success_or_failure_without_freshness() {
        for outcome in ["success", "login", "parse", "transport"] {
            for next_generation in [2, 3] {
                let collector = CodexActivityCollector::default();
                let policy = AtomicU64::new(1);
                let now = Instant::now();
                collector.collect_with(
                    false,
                    now,
                    now + Duration::from_secs(5),
                    || admission(&policy),
                    |_| Ok(token_response(111)),
                );
                let attempted = now + FORCE_COALESCE;
                let unavailable = collector.collect_with(
                    true,
                    attempted,
                    Instant::now() + Duration::from_secs(5),
                    || admission(&policy),
                    |_| {
                        policy.store(next_generation, Ordering::SeqCst);
                        fetch_outcome(outcome)
                    },
                );
                assert_eq!(unavailable, CodexActivityView::unavailable());
                assert_uncached(&collector);
                policy.store(3, Ordering::SeqCst);
                let recovered = collector.collect_with(
                    false,
                    attempted,
                    Instant::now() + Duration::from_secs(5),
                    || admission(&policy),
                    |_| Ok(token_response(222)),
                );
                assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
            }
        }
    }

    #[test]
    fn policy_change_after_commit_discards_success_or_failure_cache() {
        for outcome in ["success", "transport"] {
            for next_generation in [None, Some(3)] {
                let collector = CodexActivityCollector::default();
                let fetched = std::sync::atomic::AtomicBool::new(false);
                let completed_samples = AtomicUsize::new(0);
                let now = Instant::now();
                let unavailable = collector.collect_with(
                    false,
                    now,
                    now + Duration::from_secs(5),
                    || {
                        if fetched.load(Ordering::SeqCst)
                            && completed_samples.fetch_add(1, Ordering::SeqCst) > 0
                        {
                            next_generation
                        } else {
                            Some(1)
                        }
                    },
                    |_| {
                        fetched.store(true, Ordering::SeqCst);
                        fetch_outcome(outcome)
                    },
                );
                assert_eq!(unavailable, CodexActivityView::unavailable());
                assert_eq!(completed_samples.load(Ordering::SeqCst), 2);
                assert_uncached(&collector);
                let recovered = collector.collect_with(
                    false,
                    now,
                    Instant::now() + Duration::from_secs(5),
                    || Some(3),
                    |_| Ok(token_response(222)),
                );
                assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
            }
        }
    }

    #[test]
    fn typed_policy_cancellation_including_context_is_not_cached_as_failure() {
        for seed_cache in [false, true] {
            for with_context in [false, true] {
                let collector = CodexActivityCollector::default();
                let now = Instant::now();
                if seed_cache {
                    collector.collect_with(
                        false,
                        now,
                        now + Duration::from_secs(5),
                        || Some(1),
                        |_| Ok(token_response(111)),
                    );
                }
                let attempted = now + FORCE_COALESCE;
                let unavailable = collector.collect_with(
                    true,
                    attempted,
                    Instant::now() + Duration::from_secs(5),
                    || Some(1),
                    |_| {
                        let error = anyhow::Error::new(collector::CollectionPolicyCancelled);
                        Err(if with_context {
                            error.context("account usage request cancelled")
                        } else {
                            error
                        })
                    },
                );
                assert_eq!(unavailable, CodexActivityView::unavailable());
                assert_uncached(&collector);
                let recovered = collector.collect_with(
                    false,
                    attempted,
                    Instant::now() + Duration::from_secs(5),
                    || Some(1),
                    |_| Ok(token_response(222)),
                );
                assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
            }
        }
    }

    #[test]
    fn expired_deadline_remains_a_same_epoch_unavailable_cache_without_dispatch() {
        let collector = CodexActivityCollector::default();
        let now = Instant::now();
        let unavailable = collector.collect_with(
            false,
            now,
            now,
            || Some(1),
            |_| panic!("expired deadline must not dispatch"),
        );
        assert_eq!(unavailable, CodexActivityView::unavailable());
        let cached = collector.collect_with(
            false,
            now + Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
            || Some(1),
            |_| panic!("deadline failure must keep existing freshness semantics"),
        );
        assert_eq!(cached, unavailable);
        let recovered = collector.collect_with(
            true,
            now + FORCE_COALESCE,
            Instant::now() + Duration::from_secs(5),
            || Some(1),
            |_| Ok(token_response(222)),
        );
        assert_eq!(recovered.days.get("2026-08-25"), Some(&222));
    }

    #[test]
    #[ignore = "requires a current locally installed and logged-in Codex Desktop or CLI"]
    fn live_account_usage_returns_consistent_daily_buckets() {
        let view = collect(true, Instant::now() + Duration::from_secs(5));
        assert!(!view.days.is_empty());
        assert!(!view.partial);
    }
}
