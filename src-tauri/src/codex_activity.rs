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
    attempted_at: Option<Instant>,
    view: Option<CodexActivityView>,
}

#[derive(Default)]
struct CodexActivityCollector {
    gate: Mutex<()>,
    state: Mutex<FetchState>,
}

impl CodexActivityCollector {
    fn cached(&self, now: Instant, freshness: Duration) -> Option<CodexActivityView> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
        fetch: impl FnOnce(Duration) -> anyhow::Result<String>,
    ) -> CodexActivityView {
        let freshness = if force {
            FORCE_COALESCE
        } else {
            NORMAL_FRESHNESS
        };
        if let Some(view) = self.cached(now, freshness) {
            return view;
        }

        let _gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(view) = self.cached(now, freshness) {
            return view;
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        let view = if timeout.is_zero() {
            CodexActivityView::unavailable()
        } else {
            fetch(timeout)
                .and_then(|raw| parse_response(&raw))
                .unwrap_or_else(|_| CodexActivityView::unavailable())
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.attempted_at = Some(now);
        state.view = Some(view.clone());
        view
    }
}

static COLLECTOR: Lazy<CodexActivityCollector> = Lazy::new(CodexActivityCollector::default);

pub fn collect(force: bool, deadline: Instant) -> CodexActivityView {
    COLLECTOR.collect_with(force, Instant::now(), deadline, |timeout| {
        collector::codex_account_usage_response(timeout)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    fn response(summary: &str, buckets: &str) -> String {
        format!(r#"{{"result":{{"summary":{summary},"dailyUsageBuckets":{buckets}}}}}"#)
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
        let success = collector.collect_with(false, now, now + Duration::from_secs(5), |_| {
            Ok(response(
                r#"{"lifetimeTokens":10,"peakDailyTokens":10}"#,
                r#"[{"startDate":"2026-08-25","tokens":10}]"#,
            ))
        });
        assert_eq!(success.days.get("2026-08-25"), Some(&10));

        let failed = collector.collect_with(
            true,
            now + FORCE_COALESCE + Duration::from_millis(1),
            Instant::now() + Duration::from_secs(5),
            |_| anyhow::bail!("authentication required"),
        );
        assert!(failed.days.is_empty());
        assert!(failed.partial);
    }

    #[cfg(windows)]
    #[test]
    fn windows_safety_policy_exposes_empty_partial_account_activity() {
        let collector = CodexActivityCollector::default();
        let now = Instant::now();
        let view = collector.collect_with(false, now, now + Duration::from_secs(1), |timeout| {
            collector::codex_account_usage_response(timeout)
        });

        assert!(view.days.is_empty());
        assert!(view.partial);
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
                collector.collect_with(true, now, Instant::now() + Duration::from_secs(5), |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(response(
                        r#"{"lifetimeTokens":1,"peakDailyTokens":1}"#,
                        r#"[{"startDate":"2026-08-25","tokens":1}]"#,
                    ))
                })
            }));
        }
        for thread in threads {
            assert_eq!(thread.join().unwrap().days.get("2026-08-25"), Some(&1));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[ignore = "requires a current locally installed and logged-in Codex Desktop or CLI"]
    fn live_account_usage_returns_consistent_daily_buckets() {
        let view = collect(true, Instant::now() + Duration::from_secs(5));
        assert!(!view.days.is_empty());
        assert!(!view.partial);
    }
}
