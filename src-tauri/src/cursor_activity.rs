use crate::{config, cursor_dashboard, paths};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, LocalResult, NaiveDate, TimeZone, Utc,
};
use cursor_dashboard::{
    AccountScope, DashboardCredentials, DashboardErrorKind, TokenUsage, UsageEvent,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CACHE_SCHEMA: &str = "cursor_activity.v1";
const CACHE_FILE_NAME: &str = "cursor-activity-v1.json";
const MAX_CACHE_BYTES: u64 = 1024 * 1024;
const PAGE_SIZE: u32 = 500;
const FALLBACK_PAGE_SIZE: u32 = 250;
const MAX_PAGES_PER_MONTH: usize = 64;
const MAX_EVENTS_PER_MONTH: usize = 32_000;
const MAX_CACHE_MONTHS: usize = 14;
const CURRENT_MONTH_FRESHNESS: Duration = Duration::from_secs(5 * 60);
const PREVIOUS_MONTH_FRESHNESS: Duration = Duration::from_secs(60 * 60);
const CURRENT_INGESTION_LAG: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimezoneIdentity {
    pub key: String,
    pub dynamic_daylight_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorActivityView {
    pub days: BTreeMap<String, u64>,
    pub partial: bool,
    pub backfill_pending: bool,
    pub scope: AccountScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStepKind {
    Updated,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshStep {
    pub kind: RefreshStepKind,
    pub view: CursorActivityView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityError {
    pub kind: DashboardErrorKind,
    context: &'static str,
}

impl ActivityError {
    fn new(kind: DashboardErrorKind, context: &'static str) -> Self {
        Self { kind, context }
    }

    fn parse(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::Parse, context)
    }

    fn unavailable(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::Unavailable, context)
    }

    fn changed(context: &'static str) -> Self {
        Self::new(DashboardErrorKind::ScopeChanged, context)
    }
}

impl fmt::Display for ActivityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl std::error::Error for ActivityError {}

impl From<cursor_dashboard::DashboardError> for ActivityError {
    fn from(error: cursor_dashboard::DashboardError) -> Self {
        Self::new(error.kind, "Cursor account activity request failed")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CursorMonthCache {
    refreshed_at: String,
    #[serde(default)]
    complete: bool,
    days: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CursorActivityCache {
    schema_version: String,
    account_scope: AccountScope,
    timezone_identity: TimezoneIdentity,
    #[serde(default)]
    months: BTreeMap<String, CursorMonthCache>,
}

impl CursorActivityCache {
    fn new(scope: AccountScope, timezone: TimezoneIdentity) -> Self {
        Self {
            schema_version: CACHE_SCHEMA.into(),
            account_scope: scope,
            timezone_identity: timezone,
            months: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Month {
    year: i32,
    month: u32,
}

impl Month {
    fn from_date(date: NaiveDate) -> Self {
        Self {
            year: date.year(),
            month: date.month(),
        }
    }

    fn key(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    fn previous(self) -> Self {
        if self.month == 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }
}

trait UsageSource {
    fn filtered(
        &self,
        start_ms: i64,
        end_ms: i64,
        page: u32,
        page_size: u32,
        deadline: Instant,
    ) -> Result<cursor_dashboard::UsageEventPage, ActivityError>;

    fn aggregated(
        &self,
        start_ms: i64,
        end_ms: i64,
        deadline: Instant,
    ) -> Result<TokenUsage, ActivityError>;
}

struct DashboardSource<'a> {
    credentials: &'a DashboardCredentials,
}

impl UsageSource for DashboardSource<'_> {
    fn filtered(
        &self,
        start_ms: i64,
        end_ms: i64,
        page: u32,
        page_size: u32,
        deadline: Instant,
    ) -> Result<cursor_dashboard::UsageEventPage, ActivityError> {
        cursor_dashboard::filtered_usage_events(
            self.credentials,
            start_ms,
            end_ms,
            page,
            page_size,
            deadline,
        )
        .map_err(Into::into)
    }

    fn aggregated(
        &self,
        start_ms: i64,
        end_ms: i64,
        deadline: Instant,
    ) -> Result<TokenUsage, ActivityError> {
        cursor_dashboard::aggregated_usage(self.credentials, start_ms, end_ms, deadline)
            .map_err(Into::into)
    }
}

pub fn cache_path() -> Option<PathBuf> {
    paths::data_dir().map(|path| path.join(CACHE_FILE_NAME))
}

#[cfg(windows)]
pub fn timezone_identity() -> Result<TimezoneIdentity, ActivityError> {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation")
        .map_err(|_| ActivityError::unavailable("Windows timezone unavailable"))?;
    let name = key
        .get_value::<String, _>("TimeZoneKeyName")
        .or_else(|_| key.get_value::<String, _>("StandardName"))
        .map_err(|_| ActivityError::unavailable("Windows timezone key unavailable"))?;
    let dynamic_daylight_disabled = key
        .get_value::<u32, _>("DynamicDaylightTimeDisabled")
        .unwrap_or(0)
        != 0;
    Ok(TimezoneIdentity {
        key: name.trim_matches('\0').trim().to_owned(),
        dynamic_daylight_disabled,
    })
}

#[cfg(not(windows))]
pub fn timezone_identity() -> Result<TimezoneIdentity, ActivityError> {
    Ok(TimezoneIdentity {
        key: Local::now().offset().to_string(),
        dynamic_daylight_disabled: false,
    })
}

fn resolve_local<Tz: TimeZone>(timezone: &Tz, date: NaiveDate) -> Option<DateTime<Tz>> {
    let midnight = date.and_hms_opt(0, 0, 0)?;
    match timezone.from_local_datetime(&midnight) {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(first, second) => {
            if first.timestamp_millis() <= second.timestamp_millis() {
                Some(first)
            } else {
                Some(second)
            }
        }
        LocalResult::None => (1..=180).find_map(|minutes| {
            let candidate = midnight + ChronoDuration::minutes(minutes);
            match timezone.from_local_datetime(&candidate) {
                LocalResult::Single(value) => Some(value),
                LocalResult::Ambiguous(first, second) => {
                    if first.timestamp_millis() <= second.timestamp_millis() {
                        Some(first)
                    } else {
                        Some(second)
                    }
                }
                LocalResult::None => None,
            }
        }),
    }
}

fn month_bounds<Tz: TimeZone>(month: Month, timezone: &Tz) -> Result<(i64, i64), ActivityError> {
    let start = NaiveDate::from_ymd_opt(month.year, month.month, 1)
        .and_then(|date| resolve_local(timezone, date))
        .ok_or_else(|| ActivityError::parse("Cursor activity month start unavailable"))?;
    let next = month.next();
    let end = NaiveDate::from_ymd_opt(next.year, next.month, 1)
        .and_then(|date| resolve_local(timezone, date))
        .ok_or_else(|| ActivityError::parse("Cursor activity month end unavailable"))?;
    Ok((start.timestamp_millis(), end.timestamp_millis()))
}

fn event_date<Tz: TimeZone>(timestamp_ms: i64, timezone: &Tz) -> Option<String> {
    Utc.timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|value| {
            value
                .with_timezone(timezone)
                .date_naive()
                .format("%Y-%m-%d")
                .to_string()
        })
}

fn required_months<Tz: TimeZone>(weeks: u16, now: DateTime<Utc>, timezone: &Tz) -> Vec<Month> {
    let local = now.with_timezone(timezone);
    let today = local.date_naive();
    let sunday_offset = i64::from(today.weekday().num_days_from_sunday());
    let current_week_start = today - ChronoDuration::days(sunday_offset);
    let weeks = i64::from(weeks.clamp(4, 52));
    let start = current_week_start - ChronoDuration::days((weeks - 1) * 7);
    let first = Month::from_date(start);
    let last = Month::from_date(today);
    let mut months = Vec::new();
    let mut cursor = last;
    loop {
        months.push(cursor);
        if cursor == first || months.len() >= MAX_CACHE_MONTHS {
            break;
        }
        cursor = cursor.previous();
    }
    months
}

fn load_cache(
    path: &Path,
    scope: AccountScope,
    timezone: &TimezoneIdentity,
) -> CursorActivityCache {
    let loaded = std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_CACHE_BYTES)
        .and_then(|_| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<CursorActivityCache>(&bytes).ok())
        .filter(|cache| {
            cache.schema_version == CACHE_SCHEMA
                && cache.account_scope == scope
                && cache.timezone_identity == *timezone
        });
    loaded.unwrap_or_else(|| CursorActivityCache::new(scope, timezone.clone()))
}

fn save_cache(path: &Path, cache: &CursorActivityCache) -> Result<(), ActivityError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| ActivityError::unavailable("Cursor activity cache unavailable"))?;
    }
    let bytes = serde_json::to_vec_pretty(cache)
        .map_err(|_| ActivityError::parse("Cursor activity cache serialization failed"))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err(ActivityError::new(
            DashboardErrorKind::Oversized,
            "Cursor activity cache exceeded its limit",
        ));
    }
    config::replace_file(path, &bytes)
        .map_err(|_| ActivityError::unavailable("Cursor activity cache write failed"))
}

fn view_from_cache<Tz: TimeZone>(
    cache: &CursorActivityCache,
    required: &[Month],
    now: DateTime<Utc>,
    timezone: &Tz,
    partial: bool,
) -> CursorActivityView {
    let mut days = BTreeMap::new();
    for month in required {
        if let Some(cached) = cache.months.get(&month.key()) {
            for (date, tokens) in &cached.days {
                days.insert(date.clone(), *tokens);
            }
        }
    }
    let backfill_pending = required
        .iter()
        .any(|month| !cache.months.contains_key(&month.key()))
        || historical_target_month(cache, required, now, timezone).is_some();
    let incomplete_history = required.iter().skip(1).any(|month| {
        cache
            .months
            .get(&month.key())
            .is_none_or(|cached| !cached.complete)
    });
    CursorActivityView {
        days,
        partial: partial || backfill_pending || incomplete_history,
        backfill_pending,
        scope: cache.account_scope,
    }
}

pub fn cached_view(
    weeks: u16,
    now: DateTime<Utc>,
    deadline: Instant,
) -> Result<CursorActivityView, ActivityError> {
    let credentials = cursor_dashboard::read_credentials(deadline)?;
    if !cursor_dashboard::scope_is_validated(credentials.scope) {
        return Err(ActivityError::changed(
            "Cursor account scope has not been validated",
        ));
    }
    let timezone = timezone_identity()?;
    let path = cache_path()
        .ok_or_else(|| ActivityError::unavailable("Cursor activity data path unavailable"))?;
    let cache = load_cache(&path, credentials.scope, &timezone);
    let view = view_from_cache(
        &cache,
        &required_months(weeks, now, &Local),
        now,
        &Local,
        false,
    );
    validate_cache_access(&cache, deadline, &|| true)?;
    Ok(view)
}

fn check_cache_access(
    cache: &CursorActivityCache,
    current_scope: AccountScope,
    current_timezone: &TimezoneIdentity,
    scope_validated: bool,
    commit_allowed: bool,
) -> Result<(), ActivityError> {
    if current_scope != cache.account_scope
        || *current_timezone != cache.timezone_identity
        || !scope_validated
        || !commit_allowed
    {
        return Err(ActivityError::changed(
            "Cursor activity scope or settings changed",
        ));
    }
    Ok(())
}

fn validate_cache_access(
    cache: &CursorActivityCache,
    deadline: Instant,
    commit_allowed: &impl Fn() -> bool,
) -> Result<(), ActivityError> {
    let current = cursor_dashboard::read_credentials(deadline)?;
    check_cache_access(
        cache,
        current.scope,
        &timezone_identity()?,
        cursor_dashboard::scope_is_validated(cache.account_scope),
        commit_allowed(),
    )
}

fn refreshed_recently(value: &str, now: DateTime<Utc>, duration: Duration) -> bool {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| {
            let age = now.signed_duration_since(value.with_timezone(&Utc));
            age >= ChronoDuration::zero()
                && age
                    <= ChronoDuration::from_std(duration).unwrap_or_else(|_| ChronoDuration::zero())
        })
        .unwrap_or(false)
}

fn target_month<Tz: TimeZone>(
    cache: &CursorActivityCache,
    required: &[Month],
    now: DateTime<Utc>,
    force_current: bool,
    timezone: &Tz,
) -> Option<Month> {
    let current = *required.first()?;
    let current_cached = cache.months.get(&current.key());
    if force_current
        || current_cached.is_none_or(|month| {
            !refreshed_recently(&month.refreshed_at, now, CURRENT_MONTH_FRESHNESS)
        })
    {
        return Some(current);
    }
    historical_target_month(cache, required, now, timezone)
}

fn historical_target_month<Tz: TimeZone>(
    cache: &CursorActivityCache,
    required: &[Month],
    now: DateTime<Utc>,
    timezone: &Tz,
) -> Option<Month> {
    let current = *required.first()?;
    let local = now.with_timezone(timezone);
    if local.day() <= 7 {
        let previous = current.previous();
        if required.contains(&previous)
            && cache.months.get(&previous.key()).is_none_or(|month| {
                !refreshed_recently(&month.refreshed_at, now, PREVIOUS_MONTH_FRESHNESS)
            })
        {
            return Some(previous);
        }
    }
    required.iter().skip(1).copied().find(|month| {
        cache.months.get(&month.key()).is_none_or(|cached| {
            !cached.complete
                && month_bounds(*month, timezone)
                    .is_ok_and(|(_, end_ms)| end_ms <= ingestion_cutoff(now))
        })
    })
}

fn ingestion_cutoff(now: DateTime<Utc>) -> i64 {
    (now - ChronoDuration::from_std(CURRENT_INGESTION_LAG)
        .unwrap_or_else(|_| ChronoDuration::zero()))
    .timestamp_millis()
}

fn page_signature(events: &[UsageEvent]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for event in events {
        event.timestamp_ms.hash(&mut hasher);
        event.token_usage.hash(&mut hasher);
        event.model.hash(&mut hasher);
        event.client_type.hash(&mut hasher);
        event.conversation_id.hash(&mut hasher);
    }
    hasher.finish()
}

fn fetch_month_with_page_size<S: UsageSource, Tz: TimeZone>(
    source: &S,
    month: Month,
    timezone: &Tz,
    page_size: u32,
    deadline: Instant,
) -> Result<CursorMonthCache, ActivityError> {
    fetch_month_with_page_size_at(source, month, timezone, page_size, deadline, Utc::now())
}

fn fetch_month_with_page_size_at<S: UsageSource, Tz: TimeZone>(
    source: &S,
    month: Month,
    timezone: &Tz,
    page_size: u32,
    deadline: Instant,
    now: DateTime<Utc>,
) -> Result<CursorMonthCache, ActivityError> {
    let (start_ms, month_end_ms) = month_bounds(month, timezone)?;
    let end_ms = month_end_ms.min(ingestion_cutoff(now));
    if end_ms <= start_ms {
        return Err(ActivityError::parse(
            "Cursor activity month interval rejected",
        ));
    }
    let first = source.filtered(start_ms, end_ms, 1, page_size, deadline)?;
    if first.total_count > MAX_EVENTS_PER_MONTH {
        return Err(ActivityError::new(
            DashboardErrorKind::Oversized,
            "Cursor activity month exceeded its event limit",
        ));
    }
    let pages = first.total_count.div_ceil(page_size as usize).max(1);
    if pages > MAX_PAGES_PER_MONTH {
        return Err(ActivityError::new(
            DashboardErrorKind::Oversized,
            "Cursor activity month exceeded its page limit",
        ));
    }
    let initial_signature = page_signature(&first.events);
    let mut event_count = first.events.len();
    let mut component_total = TokenUsage::default();
    let mut days = BTreeMap::new();
    let mut apply_events = |events: &[UsageEvent]| -> Result<(), ActivityError> {
        for event in events {
            let date = event_date(event.timestamp_ms, timezone)
                .ok_or_else(|| ActivityError::parse("Cursor activity date rejected"))?;
            if !date.starts_with(&month.key()) {
                return Err(ActivityError::parse(
                    "Cursor activity event escaped its month",
                ));
            }
            if let Some(usage) = event.token_usage {
                component_total = component_total.saturating_add(usage);
                let total = days.entry(date).or_insert(0u64);
                *total = total.saturating_add(usage.total());
            }
        }
        Ok(())
    };
    apply_events(&first.events)?;
    for page in 2..=pages {
        let response = source.filtered(
            start_ms,
            end_ms,
            u32::try_from(page).map_err(|_| ActivityError::parse("Cursor page rejected"))?,
            page_size,
            deadline,
        )?;
        if response.total_count != first.total_count {
            return Err(ActivityError::changed(
                "Cursor activity count changed during pagination",
            ));
        }
        event_count = event_count.saturating_add(response.events.len());
        apply_events(&response.events)?;
    }
    if event_count != first.total_count {
        return Err(ActivityError::changed(
            "Cursor activity page coverage changed",
        ));
    }
    let final_first = source.filtered(start_ms, end_ms, 1, page_size, deadline)?;
    if final_first.total_count != first.total_count
        || page_signature(&final_first.events) != initial_signature
    {
        return Err(ActivityError::changed(
            "Cursor activity first page changed during pagination",
        ));
    }
    if source.aggregated(start_ms, end_ms, deadline)? != component_total {
        return Err(ActivityError::changed(
            "Cursor activity aggregate changed during pagination",
        ));
    }
    Ok(CursorMonthCache {
        refreshed_at: Utc::now().to_rfc3339(),
        // Use the queried interval, not the time when pagination finished.
        complete: end_ms == month_end_ms,
        days,
    })
}

fn fetch_month<S: UsageSource, Tz: TimeZone>(
    source: &S,
    month: Month,
    timezone: &Tz,
    deadline: Instant,
) -> Result<CursorMonthCache, ActivityError> {
    match fetch_month_with_page_size(source, month, timezone, PAGE_SIZE, deadline) {
        Err(error) if error.kind == DashboardErrorKind::Oversized => {
            fetch_month_with_page_size(source, month, timezone, FALLBACK_PAGE_SIZE, deadline)
        }
        outcome => outcome,
    }
}

pub fn refresh_step(
    weeks: u16,
    now: DateTime<Utc>,
    force_current: bool,
    deadline: Instant,
    commit_allowed: impl Fn() -> bool,
) -> Result<RefreshStep, ActivityError> {
    let path = cache_path()
        .ok_or_else(|| ActivityError::unavailable("Cursor activity data path unavailable"))?;
    refresh_step_at_path(&path, weeks, now, force_current, deadline, commit_allowed)
}

fn refresh_step_at_path(
    path: &Path,
    weeks: u16,
    now: DateTime<Utc>,
    force_current: bool,
    deadline: Instant,
    commit_allowed: impl Fn() -> bool,
) -> Result<RefreshStep, ActivityError> {
    let credentials = cursor_dashboard::read_credentials(deadline)?;
    let timezone = timezone_identity()?;
    let mut cache = load_cache(path, credentials.scope, &timezone);
    let required = required_months(weeks, now, &Local);
    let Some(target) = target_month(&cache, &required, now, force_current, &Local) else {
        return checked_refresh_step(&cache, &required, now, &Local, || {
            validate_cache_access(&cache, deadline, &commit_allowed)
        });
    };
    let source = DashboardSource {
        credentials: &credentials,
    };
    let month = fetch_month(&source, target, &Local, deadline)?;
    validate_cache_access(&cache, deadline, &commit_allowed)?;
    cache.months.insert(target.key(), month);
    while cache.months.len() > MAX_CACHE_MONTHS {
        let Some(oldest) = cache.months.keys().next().cloned() else {
            break;
        };
        cache.months.remove(&oldest);
    }
    save_cache(path, &cache)?;
    checked_refresh_step(&cache, &required, Utc::now(), &Local, || {
        validate_cache_access(&cache, deadline, &commit_allowed)
    })
}

fn checked_refresh_step<Tz: TimeZone>(
    cache: &CursorActivityCache,
    required: &[Month],
    now: DateTime<Utc>,
    timezone: &Tz,
    validate: impl FnOnce() -> Result<(), ActivityError>,
) -> Result<RefreshStep, ActivityError> {
    let view = view_from_cache(cache, required, now, timezone, false);
    validate()?;
    Ok(RefreshStep {
        kind: if view.backfill_pending {
            RefreshStepKind::Updated
        } else {
            RefreshStepKind::Complete
        },
        view,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::New_York;
    use std::{cell::RefCell, collections::VecDeque};

    struct FakeSource {
        first_pages: RefCell<VecDeque<cursor_dashboard::UsageEventPage>>,
        other_pages: BTreeMap<u32, cursor_dashboard::UsageEventPage>,
        aggregate: TokenUsage,
    }

    impl UsageSource for FakeSource {
        fn filtered(
            &self,
            _start_ms: i64,
            _end_ms: i64,
            page: u32,
            _page_size: u32,
            _deadline: Instant,
        ) -> Result<cursor_dashboard::UsageEventPage, ActivityError> {
            if page == 1 {
                return self
                    .first_pages
                    .borrow_mut()
                    .pop_front()
                    .ok_or_else(|| ActivityError::parse("missing fake first page"));
            }
            self.other_pages
                .get(&page)
                .cloned()
                .ok_or_else(|| ActivityError::parse("missing fake page"))
        }

        fn aggregated(
            &self,
            _start_ms: i64,
            _end_ms: i64,
            _deadline: Instant,
        ) -> Result<TokenUsage, ActivityError> {
            Ok(self.aggregate)
        }
    }

    fn event(timestamp: &str, tokens: u64) -> UsageEvent {
        UsageEvent {
            timestamp_ms: DateTime::parse_from_rfc3339(timestamp)
                .unwrap()
                .timestamp_millis(),
            token_usage: Some(TokenUsage {
                input_tokens: tokens,
                ..TokenUsage::default()
            }),
            model: Some("model".into()),
            client_type: Some("ide".into()),
            conversation_id: Some("conversation".into()),
        }
    }

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sample_cache(now: DateTime<Utc>) -> (CursorActivityCache, Vec<Month>) {
        let required = vec![
            Month {
                year: 2026,
                month: 9,
            },
            Month {
                year: 2026,
                month: 8,
            },
            Month {
                year: 2026,
                month: 7,
            },
        ];
        let mut cache = CursorActivityCache::new(
            AccountScope {
                user_id: 1,
                team_id: None,
            },
            TimezoneIdentity {
                key: "UTC".into(),
                dynamic_daylight_disabled: false,
            },
        );
        for (index, month) in required.iter().enumerate() {
            cache.months.insert(
                month.key(),
                CursorMonthCache {
                    refreshed_at: now.to_rfc3339(),
                    complete: index != 0,
                    days: BTreeMap::from([(format!("{}-15", month.key()), 10)]),
                },
            );
        }
        (cache, required)
    }

    fn fetch_empty_month<Tz: TimeZone>(
        month: Month,
        now: DateTime<Utc>,
        timezone: &Tz,
    ) -> CursorMonthCache {
        let empty = cursor_dashboard::UsageEventPage {
            events: vec![],
            total_count: 0,
        };
        let source = FakeSource {
            first_pages: RefCell::new(VecDeque::from([empty.clone(), empty])),
            other_pages: BTreeMap::new(),
            aggregate: TokenUsage::default(),
        };
        fetch_month_with_page_size_at(
            &source,
            month,
            timezone,
            PAGE_SIZE,
            Instant::now() + Duration::from_secs(2),
            now,
        )
        .unwrap()
    }

    #[test]
    fn fresh_cache_return_rejects_invalidated_scope_and_cancelled_commit() {
        let now = timestamp("2026-09-08T12:00:00Z");
        let (cache, required) = sample_cache(now);
        assert_eq!(target_month(&cache, &required, now, false, &Utc), None);
        for (validated, allowed, expected_success) in [
            (true, true, true),
            (false, true, false), // Invalidated scope or a fresh process.
            (true, false, false),
            (false, false, false),
            (true, true, true),
        ] {
            let result = checked_refresh_step(&cache, &required, now, &Utc, || {
                check_cache_access(
                    &cache,
                    cache.account_scope,
                    &cache.timezone_identity,
                    validated,
                    allowed,
                )
            });
            if expected_success {
                let step = result.unwrap();
                assert_eq!(step.kind, RefreshStepKind::Complete);
                assert!(!step.view.partial);
                assert!(!step.view.days.is_empty());
            } else {
                assert_eq!(result.unwrap_err().kind, DashboardErrorKind::ScopeChanged);
            }
        }
    }

    #[test]
    fn cache_return_rechecks_user_team_and_timezone_after_reading_cache() {
        let now = timestamp("2026-09-08T12:00:00Z");
        let (cache, required) = sample_cache(now);
        for scope in [
            AccountScope {
                user_id: 2,
                team_id: None,
            },
            AccountScope {
                user_id: 1,
                team_id: Some(1),
            },
        ] {
            let error = checked_refresh_step(&cache, &required, now, &Utc, || {
                check_cache_access(&cache, scope, &cache.timezone_identity, true, true)
            })
            .unwrap_err();
            assert_eq!(error.kind, DashboardErrorKind::ScopeChanged);
        }
        let timezone = TimezoneIdentity {
            dynamic_daylight_disabled: true,
            ..cache.timezone_identity.clone()
        };
        assert_eq!(
            check_cache_access(&cache, cache.account_scope, &timezone, true, true,)
                .unwrap_err()
                .kind,
            DashboardErrorKind::ScopeChanged
        );
    }

    #[test]
    fn midmonth_cache_is_finalized_after_day_seven_even_after_forced_current() {
        let now = timestamp("2026-09-08T12:00:00Z");
        let (mut cache, required) = sample_cache(now);
        let current = required[0];
        let previous = required[1];
        let midmonth = fetch_empty_month(previous, timestamp("2026-08-15T12:00:00Z"), &Utc);
        assert!(!midmonth.complete);
        cache.months.insert(previous.key(), midmonth);
        assert_eq!(
            target_month(&cache, &required, now, false, &Utc),
            Some(previous)
        );
        assert_eq!(
            target_month(&cache, &required, now, true, &Utc),
            Some(current)
        );

        // A successful forced current-month fetch must not end the backfill loop.
        let mut current_cache = fetch_empty_month(current, now, &Utc);
        current_cache.refreshed_at = now.to_rfc3339();
        assert!(!current_cache.complete);
        cache.months.insert(current.key(), current_cache);
        let step = checked_refresh_step(&cache, &required, now, &Utc, || Ok(())).unwrap();
        assert_eq!(step.kind, RefreshStepKind::Updated);
        assert!(step.view.partial && step.view.backfill_pending);
        assert_eq!(
            target_month(&cache, &required, now, false, &Utc),
            Some(previous)
        );

        let finalized = fetch_empty_month(previous, now, &Utc);
        assert!(finalized.complete);
        cache.months.insert(previous.key(), finalized);
        let step = checked_refresh_step(&cache, &required, now, &Utc, || Ok(())).unwrap();
        assert_eq!(step.kind, RefreshStepKind::Complete);
        assert!(!step.view.partial && !step.view.backfill_pending);
        for minutes in 0..=5 {
            assert_eq!(
                target_month(
                    &cache,
                    &required,
                    now + ChronoDuration::minutes(minutes),
                    false,
                    &Utc,
                ),
                None
            );
        }
        assert_eq!(
            target_month(
                &cache,
                &required,
                now + ChronoDuration::minutes(6),
                false,
                &Utc,
            ),
            Some(current)
        );
    }

    #[test]
    fn legacy_months_are_refetched_once_without_invalidating_fresh_current() {
        let now = timestamp("2026-09-08T12:00:00Z");
        let (mut cache, required) = sample_cache(now);
        let legacy: CursorMonthCache = serde_json::from_value(serde_json::json!({
            "refreshed_at": now.to_rfc3339(),
            "days": {"2026-07-15": 42}
        }))
        .unwrap();
        assert!(!legacy.complete);
        assert_eq!(legacy.days["2026-07-15"], 42);
        cache.months.insert(required[2].key(), legacy);
        assert_eq!(
            target_month(&cache, &required, now, false, &Utc),
            Some(required[2])
        );
        cache
            .months
            .insert(required[2].key(), fetch_empty_month(required[2], now, &Utc));
        let roundtrip: CursorActivityCache =
            serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(roundtrip, cache);
        assert_eq!(target_month(&roundtrip, &required, now, false, &Utc), None);
        assert_eq!(
            target_month(&roundtrip, &required[..1], now, false, &Utc),
            None
        );
    }

    #[test]
    fn month_finalization_waits_for_ingestion_lag_at_local_month_boundary() {
        let timezone = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        let before = timestamp("2026-09-01T00:04:59+09:00");
        let cutoff = timestamp("2026-09-01T00:05:00+09:00");
        let (mut cache, required) = sample_cache(before);
        let previous = required[1];
        let mut partial = fetch_empty_month(previous, before, &timezone);
        assert!(!partial.complete);
        partial.refreshed_at = before.to_rfc3339();
        cache.months.insert(previous.key(), partial);
        assert_eq!(
            target_month(&cache, &required, before, false, &timezone),
            None
        );
        let waiting =
            checked_refresh_step(&cache, &required, before, &timezone, || Ok(())).unwrap();
        assert!(waiting.view.partial);
        assert!(!waiting.view.backfill_pending);
        assert_eq!(
            target_month(&cache, &required, cutoff, false, &timezone),
            Some(previous)
        );

        let mut finalized = fetch_empty_month(previous, cutoff, &timezone);
        assert!(finalized.complete);
        finalized.refreshed_at = cutoff.to_rfc3339();
        cache.months.insert(previous.key(), finalized);
        assert_eq!(
            target_month(&cache, &required, cutoff, false, &timezone),
            None
        );
        assert!(!view_from_cache(&cache, &required, cutoff, &timezone, false).partial);
    }

    #[test]
    fn finalized_previous_month_keeps_only_the_existing_bounded_grace_refresh() {
        let now = timestamp("2026-09-07T12:00:00Z");
        let (mut cache, required) = sample_cache(now);
        cache
            .months
            .get_mut(&required[1].key())
            .unwrap()
            .refreshed_at = (now - ChronoDuration::hours(2)).to_rfc3339();
        assert_eq!(
            target_month(&cache, &required, now, false, &Utc),
            Some(required[1])
        );
        let after_grace = now + ChronoDuration::days(1);
        cache
            .months
            .get_mut(&required[0].key())
            .unwrap()
            .refreshed_at = after_grace.to_rfc3339();
        assert_eq!(
            target_month(&cache, &required, after_grace, false, &Utc),
            None
        );
    }

    #[test]
    fn month_bounds_follow_dst_instead_of_reusing_one_offset() {
        let march = month_bounds(
            Month {
                year: 2026,
                month: 3,
            },
            &New_York,
        )
        .unwrap();
        let duration_hours = (march.1 - march.0) / (60 * 60 * 1000);
        assert_eq!(duration_hours, 743);
        assert_eq!(
            event_date(
                DateTime::parse_from_rfc3339("2026-03-09T03:30:00Z")
                    .unwrap()
                    .timestamp_millis(),
                &New_York
            )
            .as_deref(),
            Some("2026-03-08")
        );
    }

    #[test]
    fn cache_scope_and_timezone_mismatch_rebuild_instead_of_merging() {
        let path = std::env::temp_dir().join(format!(
            "agent-juice-cursor-activity-cache-{}-{}.json",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let original_scope = AccountScope {
            user_id: 1,
            team_id: None,
        };
        let timezone = TimezoneIdentity {
            key: "Korea Standard Time".into(),
            dynamic_daylight_disabled: false,
        };
        let mut cache = CursorActivityCache::new(original_scope, timezone.clone());
        cache.months.insert(
            "2026-08".into(),
            CursorMonthCache {
                refreshed_at: Utc::now().to_rfc3339(),
                complete: true,
                days: BTreeMap::from([("2026-08-25".into(), 10)]),
            },
        );
        save_cache(&path, &cache).unwrap();
        let switched = load_cache(
            &path,
            AccountScope {
                user_id: 2,
                team_id: None,
            },
            &timezone,
        );
        assert!(switched.months.is_empty());
        let changed_zone = load_cache(
            &path,
            original_scope,
            &TimezoneIdentity {
                key: "Pacific Standard Time".into(),
                dynamic_daylight_disabled: false,
            },
        );
        assert!(changed_zone.months.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn pagination_counts_identical_real_events_and_commits_only_on_consistency() {
        let first_event = event("2026-08-21T01:00:00Z", 10);
        let second_event = first_event.clone();
        let first_page = cursor_dashboard::UsageEventPage {
            events: vec![first_event],
            total_count: 2,
        };
        let source = FakeSource {
            first_pages: RefCell::new(VecDeque::from([first_page.clone(), first_page.clone()])),
            other_pages: BTreeMap::from([(
                2,
                cursor_dashboard::UsageEventPage {
                    events: vec![second_event],
                    total_count: 2,
                },
            )]),
            aggregate: TokenUsage {
                input_tokens: 20,
                ..TokenUsage::default()
            },
        };
        let timezone = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        let month = fetch_month_with_page_size(
            &source,
            Month {
                year: 2026,
                month: 8,
            },
            &timezone,
            1,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(month.days.get("2026-08-21"), Some(&20));
    }

    #[test]
    fn pagination_rejects_a_changed_final_first_page() {
        let initial = cursor_dashboard::UsageEventPage {
            events: vec![event("2026-08-21T01:00:00Z", 10)],
            total_count: 1,
        };
        let changed = cursor_dashboard::UsageEventPage {
            events: vec![event("2026-08-21T02:00:00Z", 10)],
            total_count: 1,
        };
        let source = FakeSource {
            first_pages: RefCell::new(VecDeque::from([initial, changed])),
            other_pages: BTreeMap::new(),
            aggregate: TokenUsage {
                input_tokens: 10,
                ..TokenUsage::default()
            },
        };
        let timezone = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        let error = fetch_month_with_page_size(
            &source,
            Month {
                year: 2026,
                month: 8,
            },
            &timezone,
            500,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        assert_eq!(error.kind, DashboardErrorKind::ScopeChanged);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "uses the locally logged-in Cursor GUI account"]
    fn live_month_refresh_commits_only_to_an_isolated_cache() {
        let root = std::env::temp_dir().join(format!(
            "agent-juice-cursor-activity-live-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("cursor-activity-v1.json");
        let now = Utc::now();
        let step = refresh_step_at_path(
            &path,
            8,
            now,
            true,
            Instant::now() + Duration::from_secs(8),
            || true,
        )
        .unwrap();
        assert_eq!(step.kind, RefreshStepKind::Updated);
        assert!(!step.view.days.is_empty());
        assert!(path.is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
