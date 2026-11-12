//! Dashboard read API (04 §8): read-only aggregates over `counted_events`
//! (`usage_events` with each event_key once across devices, D029).
//!
//! SQL groups events into 15-minute UTC slots. Every real time zone offset is
//! a multiple of 15 minutes, so Rust can label each slot with its local hour,
//! day, week or month in `DASHBOARD_TIMEZONE` without a second pass.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone};
use chrono_tz::Tz;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::Serialize;

use crate::db::{self, DbError};
use crate::http::{MIN_APP_VERSION, MIN_COLLECTOR_VERSION};

pub const ONLINE_WINDOW_MS: i64 = 15 * 60 * 1000;
pub const LONG_OFFLINE_MS: i64 = 3 * 24 * 60 * 60 * 1000;
const SLOT_MS: i64 = 15 * 60 * 1000;
pub const MAX_BUCKETS: usize = 5_000;
pub const MAX_HOUR_BUCKETS: usize = 2_000;
const MAX_LIST_ITEMS: usize = 50;
const MAX_ITEM_BYTES: usize = 128;
const MAX_QUERY_BYTES: usize = 8 * 1024;
pub const DEFAULT_PAGE: usize = 50;
pub const MAX_PAGE: usize = 200;
pub const MAX_SESSION_EVENTS: usize = 20_000;
pub const MAX_TURNS: usize = 5_000;
pub const OTHER_KEY: &str = "__other__";
pub const NO_PROJECT_KEY: &str = "__none__";
const SEP: char = '\u{1f}';

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("bad query parameters")]
    Bad,
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for QueryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::Sqlite(error))
    }
}

type QueryResult<T> = Result<T, QueryError>;

// ----------------------------------------------------------------- params --

pub type Query = HashMap<String, String>;

pub const FILTER_KEYS: [&str; 6] = ["from", "to", "devices", "clients", "models", "projects"];

/// Unknown or repeated keys are refused, so typos never silently widen a query.
pub fn parse_query(raw: Option<&str>, extra: &[&str]) -> QueryResult<Query> {
    let raw = raw.unwrap_or("");
    if raw.len() > MAX_QUERY_BYTES {
        return Err(QueryError::Bad);
    }
    let pairs =
        serde_urlencoded::from_str::<Vec<(String, String)>>(raw).map_err(|_| QueryError::Bad)?;
    let mut query = Query::new();
    for (key, value) in pairs {
        if !FILTER_KEYS.contains(&key.as_str()) && !extra.contains(&key.as_str()) {
            return Err(QueryError::Bad);
        }
        if query.insert(key, value).is_some() {
            return Err(QueryError::Bad);
        }
    }
    Ok(query)
}

fn is_plain(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_ITEM_BYTES && !text.chars().any(char::is_control)
}

fn list(query: &Query, key: &str) -> QueryResult<Vec<String>> {
    let Some(raw) = query.get(key) else {
        return Ok(Vec::new());
    };
    let items = raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if items.len() > MAX_LIST_ITEMS || !items.iter().all(|item| is_plain(item)) {
        return Err(QueryError::Bad);
    }
    Ok(items.into_iter().collect())
}

fn date(query: &Query, key: &str) -> QueryResult<Option<NaiveDate>> {
    let Some(raw) = query.get(key) else {
        return Ok(None);
    };
    let parsed = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| QueryError::Bad)?;
    if !(2000..=2100).contains(&parsed.year()) {
        return Err(QueryError::Bad);
    }
    Ok(Some(parsed))
}

pub fn text(query: &Query, key: &str) -> QueryResult<Option<String>> {
    match query.get(key).map(|value| value.trim()) {
        None | Some("") => Ok(None),
        Some(value) if is_plain(value) => Ok(Some(value.to_owned())),
        Some(_) => Err(QueryError::Bad),
    }
}

pub fn number(query: &Query, key: &str, default: usize, max: usize) -> QueryResult<usize> {
    match query.get(key) {
        None => Ok(default),
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(value) if (1..=max).contains(&value) => Ok(value),
            _ => Err(QueryError::Bad),
        },
    }
}

pub fn choice<T: Copy>(
    query: &Query,
    key: &str,
    default: T,
    options: &[(&str, T)],
) -> QueryResult<T> {
    match query.get(key) {
        None => Ok(default),
        Some(raw) => options
            .iter()
            .find(|(name, _)| *name == raw.trim())
            .map(|(_, value)| *value)
            .ok_or(QueryError::Bad),
    }
}

#[derive(Debug, Default, Clone)]
pub struct Filters {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub devices: Vec<String>,
    pub clients: Vec<String>,
    pub models: Vec<String>,
    pub projects: Vec<String>,
}

impl Filters {
    pub fn from_query(query: &Query) -> QueryResult<Self> {
        let filters = Self {
            from: date(query, "from")?,
            to: date(query, "to")?,
            devices: list(query, "devices")?,
            clients: list(query, "clients")?,
            models: list(query, "models")?,
            projects: list(query, "projects")?,
        };
        if let (Some(from), Some(to)) = (filters.from, filters.to)
            && from > to
        {
            return Err(QueryError::Bad);
        }
        Ok(filters)
    }
}

// ------------------------------------------------------------------- time --

pub fn iso(ms: i64) -> Option<String> {
    DateTime::from_timestamp_millis(ms)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn local(tz: Tz, ms: i64) -> Option<NaiveDateTime> {
    DateTime::from_timestamp_millis(ms).map(|time| time.with_timezone(&tz).naive_local())
}

fn local_date(tz: Tz, ms: i64) -> Option<NaiveDate> {
    local(tz, ms).map(|time| time.date())
}

/// The first instant of `date` in `tz`; a midnight skipped by a clock change
/// starts at the first local time that exists.
fn day_start_ms(tz: Tz, date: NaiveDate) -> i64 {
    let midnight = date.and_time(chrono::NaiveTime::MIN);
    (0..=4)
        .find_map(|hours| {
            tz.from_local_datetime(&(midnight + Duration::hours(hours)))
                .earliest()
        })
        .map_or_else(
            || midnight.and_utc().timestamp_millis(),
            |time| time.timestamp_millis(),
        )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeOut {
    pub from: String,
    pub to: String,
    pub time_zone: String,
}

struct Scope {
    from: NaiveDate,
    to: NaiveDate,
    clause: String,
    values: Vec<SqlValue>,
}

impl Scope {
    fn out(&self, tz: Tz) -> RangeOut {
        RangeOut {
            from: self.from.format("%Y-%m-%d").to_string(),
            to: self.to.format("%Y-%m-%d").to_string(),
            time_zone: tz.name().to_owned(),
        }
    }
}

fn filter_clause(filters: &Filters, tz: Tz, tenant_id: &str) -> (String, Vec<SqlValue>) {
    let mut clause = String::from("WHERE tenant_id = ?");
    let mut values = vec![SqlValue::Text(tenant_id.to_owned())];
    if let Some(from) = filters.from {
        clause.push_str(" AND ts >= ?");
        values.push(SqlValue::Integer(day_start_ms(tz, from)));
    }
    if let Some(next) = filters.to.and_then(|to| to.succ_opt()) {
        clause.push_str(" AND ts < ?");
        values.push(SqlValue::Integer(day_start_ms(tz, next)));
    }
    for (column, items) in [
        ("device_id", &filters.devices),
        ("client", &filters.clients),
        ("model_id", &filters.models),
        ("project_hash", &filters.projects),
    ] {
        if items.is_empty() {
            continue;
        }
        clause.push_str(&format!(
            " AND {column} IN ({})",
            vec!["?"; items.len()].join(", ")
        ));
        values.extend(items.iter().cloned().map(SqlValue::Text));
    }
    (clause, values)
}

/// Open ends of the range run from the first matching event to today (or
/// the last event, if one is dated later).
fn scope(
    conn: &Connection,
    tz: Tz,
    filters: &Filters,
    tenant_id: &str,
    now: i64,
) -> QueryResult<Scope> {
    let (clause, values) = filter_clause(filters, tz, tenant_id);
    let today = local_date(tz, now).ok_or(QueryError::Bad)?;
    let (from, to) = if let (Some(from), Some(to)) = (filters.from, filters.to) {
        (from, to)
    } else {
        let (first, last) = conn.query_row(
            &format!("SELECT MIN(ts), MAX(ts) FROM counted_events {clause}"),
            params_from_iter(values.iter()),
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )?;
        let first = first.and_then(|ms| local_date(tz, ms));
        let last = last.and_then(|ms| local_date(tz, ms));
        let to = filters
            .to
            .unwrap_or_else(|| last.map_or(today, |last| last.max(today)));
        let from = filters
            .from
            .unwrap_or_else(|| first.map_or(to, |first| first.min(to)));
        (from, to.max(from))
    };
    Ok(Scope {
        from,
        to,
        clause,
        values,
    })
}

// ----------------------------------------------------------------- totals --

/// A reported cost wins, else the server's estimate (D042). Qoder events
/// carry credits instead and count as neither priced nor unpriced.
const TOTALS_SQL: &str = "COALESCE(SUM(input), 0), COALESCE(SUM(output), 0), \
     COALESCE(SUM(cache_read), 0), COALESCE(SUM(cache_write), 0), COALESCE(SUM(reasoning), 0), \
     COALESCE(SUM(COALESCE(cost_usd, est_cost_usd)), 0.0), \
     COALESCE(SUM(COALESCE(cost_usd, est_cost_usd) IS NOT NULL), 0), \
     COALESCE(SUM(CASE WHEN COALESCE(cost_usd, est_cost_usd) IS NULL AND credits IS NULL \
       THEN input + output + cache_read + cache_write + reasoning ELSE 0 END), 0), \
     SUM(credits), COALESCE(SUM(message_count), 0), \
     COALESCE(SUM(precision = 'measured'), 0), COALESCE(SUM(precision = 'estimated'), 0), \
     COALESCE(SUM(precision = 'unknown'), 0), \
     COALESCE(SUM(CASE WHEN precision = 'unknown' \
       THEN input + output + cache_read + cache_write + reasoning ELSE 0 END), 0), \
     COUNT(*), COALESCE(SUM(price_status = 'partial'), 0)";
const TOTALS_WIDTH: usize = 16;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Totals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub cost: f64,
    pub priced: i64,
    pub unpriced_tokens: i64,
    pub credits: Option<f64>,
    pub messages: i64,
    pub measured: i64,
    pub estimated: i64,
    pub unknown: i64,
    pub unknown_tokens: i64,
    pub events: i64,
    /// Events priced from only some of their token buckets.
    pub partial: i64,
}

impl Totals {
    fn read(row: &Row<'_>, at: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            input: row.get(at)?,
            output: row.get(at + 1)?,
            cache_read: row.get(at + 2)?,
            cache_write: row.get(at + 3)?,
            reasoning: row.get(at + 4)?,
            cost: row.get(at + 5)?,
            priced: row.get(at + 6)?,
            unpriced_tokens: row.get(at + 7)?,
            credits: row.get(at + 8)?,
            messages: row.get(at + 9)?,
            measured: row.get(at + 10)?,
            estimated: row.get(at + 11)?,
            unknown: row.get(at + 12)?,
            unknown_tokens: row.get(at + 13)?,
            events: row.get(at + 14)?,
            partial: row.get(at + 15)?,
        })
    }

    fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.reasoning += other.reasoning;
        self.cost += other.cost;
        self.priced += other.priced;
        self.unpriced_tokens += other.unpriced_tokens;
        self.credits = match (self.credits, other.credits) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        self.messages += other.messages;
        self.measured += other.measured;
        self.estimated += other.estimated;
        self.unknown += other.unknown;
        self.unknown_tokens += other.unknown_tokens;
        self.events += other.events;
        self.partial += other.partial;
    }

    pub const fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write + self.reasoning
    }

    /// 04 §8.2: a single precision stays as is, anything mixed is `mixed`.
    pub const fn precision(&self) -> &'static str {
        if self.events == 0 {
            "unknown"
        } else if self.measured == self.events {
            "measured"
        } else if self.estimated == self.events {
            "estimated"
        } else if self.unknown == self.events {
            "unknown"
        } else {
            "mixed"
        }
    }

    /// D017: `cache_read ÷ (input + cache_read + cache_write)`.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let denominator = self.input + self.cache_read + self.cache_write;
        (denominator > 0).then(|| round(self.cache_read as f64 / denominator as f64, 6))
    }

    fn breakdown(&self) -> Breakdown {
        Breakdown {
            total_tokens: self.total(),
            input_tokens: self.input,
            output_tokens: self.output,
            cache_read_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
            reasoning_tokens: self.reasoning,
        }
    }

    fn measures(&self) -> Measures {
        Measures {
            tokens: self.breakdown(),
            cost_usd: (self.priced > 0).then(|| round(self.cost, 6)),
            unpriced_tokens: self.unpriced_tokens,
            credits: self.credits.map(|credits| round(credits, 6)),
            message_count: self.messages,
            cache_hit_rate: self.cache_hit_rate(),
            precision: self.precision(),
        }
    }

    fn point(&self) -> Point {
        Point {
            tokens: self.breakdown(),
            cost_usd: round(self.cost, 6),
            unpriced_tokens: self.unpriced_tokens,
            message_count: self.messages,
        }
    }
}

fn round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round() / scale
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakdown {
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Measures {
    #[serde(flatten)]
    pub tokens: Breakdown,
    /// API-equivalent estimate; `null` when nothing in scope has a price.
    pub cost_usd: Option<f64>,
    pub unpriced_tokens: i64,
    pub credits: Option<f64>,
    pub message_count: i64,
    pub cache_hit_rate: Option<f64>,
    pub precision: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    #[serde(flatten)]
    pub tokens: Breakdown,
    pub cost_usd: f64,
    pub unpriced_tokens: i64,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slice {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub measures: Measures,
}

fn device_names(conn: &Connection, tenant_id: &str) -> QueryResult<HashMap<String, String>> {
    let mut statement =
        conn.prepare_cached("SELECT id, display_name FROM devices WHERE tenant_id = ?1")?;
    let names = statement
        .query_map(params![tenant_id], |row| {
            let id = row.get::<_, String>(0)?;
            let name = row
                .get::<_, Option<String>>(1)?
                .unwrap_or_else(|| id.clone());
            Ok((id, name))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(names)
}

fn project_label(hash: &str, label: Option<String>) -> String {
    label.unwrap_or_else(|| format!("项目 {}", hash.chars().take(8).collect::<String>()))
}

#[derive(Clone, Copy)]
enum Dimension {
    Client,
    Model,
    Device,
    Project,
}

impl Dimension {
    const fn key_sql(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Model => "model_id",
            Self::Device => "device_id",
            Self::Project => "project_hash",
        }
    }
}

fn slices(
    conn: &Connection,
    scope: &Scope,
    dimension: Dimension,
    names: &HashMap<String, String>,
) -> QueryResult<Vec<Slice>> {
    let key = dimension.key_sql();
    let extra = if matches!(dimension, Dimension::Project) {
        " AND project_hash IS NOT NULL"
    } else {
        ""
    };
    let mut statement = conn.prepare(&format!(
        "SELECT {key}, MAX(project_label), {TOTALS_SQL} FROM counted_events {}{extra} GROUP BY 1",
        scope.clause
    ))?;
    let mut rows = statement
        .query_map(params_from_iter(scope.values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                Totals::read(row, 2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_by(|a, b| b.2.total().cmp(&a.2.total()).then_with(|| a.0.cmp(&b.0)));
    Ok(rows
        .into_iter()
        .map(|(key, label, totals)| Slice {
            label: match dimension {
                Dimension::Device => names.get(&key).cloned().unwrap_or_else(|| key.clone()),
                Dimension::Project => project_label(&key, label),
                Dimension::Client | Dimension::Model => key.clone(),
            },
            key,
            measures: totals.measures(),
        })
        .collect())
}

fn totals(conn: &Connection, scope: &Scope) -> QueryResult<Totals> {
    Ok(conn.query_row(
        &format!("SELECT {TOTALS_SQL} FROM counted_events {}", scope.clause),
        params_from_iter(scope.values.iter()),
        |row| Totals::read(row, 0),
    )?)
}

/// Group key, its project label, slot start (UTC ms) and the slot totals.
type SlotRow = (String, Option<String>, i64, Totals);

/// One row per group and 15-minute slot that has events.
fn slots(conn: &Connection, scope: &Scope, key_sql: &str) -> QueryResult<Vec<SlotRow>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {key_sql}, MAX(project_label), ts / {SLOT_MS}, {TOTALS_SQL}
         FROM counted_events {} GROUP BY 1, 3",
        scope.clause
    ))?;
    let rows = statement
        .query_map(params_from_iter(scope.values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)? * SLOT_MS,
                Totals::read(row, 3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------- caveats --

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Caveat {
    pub code: &'static str,
    pub message: String,
    pub severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

fn caveat(code: &'static str, severity: &'static str, message: String) -> Caveat {
    Caveat {
        code,
        message,
        severity,
        device_id: None,
        client: None,
    }
}

fn grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// Which price catalog the estimates come from (D042).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingInfo {
    /// Local date of the newest catalog download, `YYYY-MM-DD`.
    pub as_of: String,
    pub version: String,
}

impl PricingInfo {
    pub fn new(tz: Tz, as_of_ms: i64, version: &str) -> Option<Self> {
        let as_of = DateTime::from_timestamp_millis(as_of_ms)?
            .with_timezone(&tz)
            .format("%Y-%m-%d")
            .to_string();
        Some(Self {
            as_of,
            version: version.to_owned(),
        })
    }
}

fn data_caveats(totals: &Totals, pricing: Option<&PricingInfo>) -> Vec<Caveat> {
    let mut caveats = Vec::new();
    if totals.events == 0 {
        return caveats;
    }
    if totals.estimated > 0 {
        caveats.push(caveat(
            "estimated_qoder",
            "info",
            "含 Qoder 估算值：由 credits × 校准汇率折算，不是官方 token 计数".to_owned(),
        ));
    }
    if totals.unknown > 0 {
        caveats.push(caveat(
            "unknown_precision",
            "info",
            format!(
                "{} tokens 来自尚未核查精度的工具，可能含按字数估算的数字",
                grouped(totals.unknown_tokens)
            ),
        ));
    }
    caveats.push(caveat(
        "estimated_cost",
        "info",
        match pricing {
            Some(pricing) => format!(
                "成本按 {} 的公开价格估算，是 API 等价金额，不是账单",
                pricing.as_of
            ),
            None => "成本是按 API 价格估算的等价金额，不是账单".to_owned(),
        },
    ));
    if totals.partial > 0 {
        caveats.push(caveat(
            "partial_cost",
            "warning",
            "部分模型缺少某类价格（例如缓存写入），这部分没算进成本，成本偏低".to_owned(),
        ));
    }
    if totals.unpriced_tokens > 0 {
        caveats.push(caveat(
            "unpriced_tokens",
            "warning",
            format!(
                "{} tokens 没有价格，成本偏低",
                grouped(totals.unpriced_tokens)
            ),
        ));
    }
    caveats
}

fn device_caveats(devices: &[DeviceStatus]) -> Vec<Caveat> {
    let mut caveats = Vec::new();
    for device in devices {
        let with_device = |mut caveat: Caveat| {
            caveat.device_id = Some(device.device_id.clone());
            caveat
        };
        if device.connection == "long_offline" {
            caveats.push(with_device(caveat(
                "device_long_offline",
                "warning",
                format!(
                    "{} 已超过 3 天没有上线，请确认它的采集端是否还在自启动",
                    device.display_name
                ),
            )));
        }
        if device.outdated {
            caveats.push(with_device(caveat(
                "collector_outdated",
                "warning",
                format!("{} 的采集端版本过旧，请升级", device.display_name),
            )));
        }
        // Before its first heartbeat a device has nothing to backfill yet.
        if !device.backfill_complete && device.last_scan_at.is_some() {
            caveats.push(with_device(caveat(
                "backfill_incomplete",
                "info",
                format!("{} 的历史数据仍在上传", device.display_name),
            )));
        }
        for client in &device.clients {
            if let Some(code) = &client.error_code {
                let mut item = with_device(caveat(
                    "client_error",
                    "error",
                    format!(
                        "{} 上的 {} 采集出错（{code}）",
                        device.display_name, client.client
                    ),
                ));
                item.client = Some(client.client.clone());
                caveats.push(item);
            }
        }
    }
    caveats
}

// ---------------------------------------------------------------- summary --

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub range: RangeOut,
    #[serde(flatten)]
    pub measures: Measures,
    pub session_count: i64,
    pub turn_count: i64,
    pub active_days: usize,
    pub by_client: Vec<Slice>,
    pub by_model: Vec<Slice>,
    pub by_device: Vec<Slice>,
    pub by_project: Vec<Slice>,
    pub caveats: Vec<Caveat>,
    /// `null` until the server has loaded a price catalog.
    pub pricing: Option<PricingInfo>,
    pub generated_at: String,
}

pub fn summary(
    conn: &Connection,
    tz: Tz,
    filters: &Filters,
    tenant_id: &str,
    pricing: Option<PricingInfo>,
    now: i64,
) -> QueryResult<Summary> {
    let scope = scope(conn, tz, filters, tenant_id, now)?;
    let names = device_names(conn, tenant_id)?;
    let overall = totals(conn, &scope)?;
    let (session_count, turn_count) = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT device_id || char(31) || client || char(31)
                      || COALESCE(parent_session_id, session_id)),
                    COALESCE(SUM(CASE WHEN parent_session_id IS NULL THEN is_turn_start ELSE 0 END), 0)
             FROM counted_events {}",
            scope.clause
        ),
        params_from_iter(scope.values.iter()),
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let active_days = slots(conn, &scope, "''")?
        .into_iter()
        .filter_map(|(_, _, slot, _)| local_date(tz, slot))
        .collect::<HashSet<_>>()
        .len();
    let mut caveats = data_caveats(&overall, pricing.as_ref());
    let devices = device_statuses(conn, tenant_id, now, None)?
        .into_iter()
        .filter(|device| filters.devices.is_empty() || filters.devices.contains(&device.device_id))
        .collect::<Vec<_>>();
    caveats.extend(device_caveats(&devices));
    Ok(Summary {
        range: scope.out(tz),
        measures: overall.measures(),
        session_count,
        turn_count,
        active_days,
        by_client: slices(conn, &scope, Dimension::Client, &names)?,
        by_model: slices(conn, &scope, Dimension::Model, &names)?,
        by_device: slices(conn, &scope, Dimension::Device, &names)?,
        by_project: slices(conn, &scope, Dimension::Project, &names)?,
        caveats,
        pricing,
        generated_at: iso(now).unwrap_or_default(),
    })
}

// ------------------------------------------------------------- timeseries --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Hour,
    Day,
    Week,
    Month,
}

pub const BUCKETS: [(&str, Bucket); 4] = [
    ("hour", Bucket::Hour),
    ("day", Bucket::Day),
    ("week", Bucket::Week),
    ("month", Bucket::Month),
];

impl Bucket {
    fn label(self, time: NaiveDateTime) -> String {
        match self {
            Self::Hour => time.format("%Y-%m-%dT%H:00").to_string(),
            Self::Day => time.format("%Y-%m-%d").to_string(),
            Self::Week => {
                let monday =
                    time.date() - Duration::days(i64::from(time.weekday().num_days_from_monday()));
                monday.format("%Y-%m-%d").to_string()
            }
            Self::Month => time.format("%Y-%m").to_string(),
        }
    }

    /// Every bucket of the range in order, so charts have no gaps.
    fn labels(self, from: NaiveDate, to: NaiveDate) -> QueryResult<Vec<String>> {
        let days = usize::try_from((to - from).num_days() + 1).unwrap_or(usize::MAX);
        let limit = if self == Self::Hour {
            MAX_HOUR_BUCKETS
        } else {
            MAX_BUCKETS
        };
        if days > MAX_BUCKETS || (self == Self::Hour && days.saturating_mul(24) > limit) {
            return Err(QueryError::Bad);
        }
        let mut labels = Vec::new();
        let mut seen = HashSet::new();
        let mut day = from;
        while day <= to {
            if self == Self::Hour {
                for hour in 0..24 {
                    let time = day.and_hms_opt(hour, 0, 0).ok_or(QueryError::Bad)?;
                    labels.push(self.label(time));
                }
            } else {
                let label = self.label(day.and_time(chrono::NaiveTime::MIN));
                if seen.insert(label.clone()) {
                    labels.push(label);
                }
            }
            day = day.succ_opt().ok_or(QueryError::Bad)?;
        }
        Ok(labels)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    None,
    Client,
    Model,
    Device,
    Project,
}

pub const GROUPS: [(&str, GroupBy); 5] = [
    ("none", GroupBy::None),
    ("client", GroupBy::Client),
    ("model", GroupBy::Model),
    ("device", GroupBy::Device),
    ("project", GroupBy::Project),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub measures: Measures,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timeseries {
    pub range: RangeOut,
    pub bucket: &'static str,
    pub group_by: &'static str,
    pub buckets: Vec<String>,
    pub series: Vec<Series>,
    pub generated_at: String,
}

struct SeriesAcc {
    label: Option<String>,
    totals: Totals,
    points: Vec<Totals>,
}

/// What `/timeseries` asks for besides the filters.
#[derive(Debug, Clone, Copy)]
pub struct SeriesQuery {
    pub bucket: Bucket,
    pub group_by: GroupBy,
    pub limit: usize,
}

pub fn timeseries(
    conn: &Connection,
    tz: Tz,
    filters: &Filters,
    request: SeriesQuery,
    tenant_id: &str,
    now: i64,
) -> QueryResult<Timeseries> {
    let SeriesQuery {
        bucket,
        group_by,
        limit,
    } = request;
    let scope = scope(conn, tz, filters, tenant_id, now)?;
    let labels = bucket.labels(scope.from, scope.to)?;
    let index = labels
        .iter()
        .enumerate()
        .map(|(position, label)| (label.as_str(), position))
        .collect::<HashMap<_, _>>();
    let key_sql = match group_by {
        GroupBy::None => "''",
        GroupBy::Client => "client",
        GroupBy::Model => "model_id",
        GroupBy::Device => "device_id",
        GroupBy::Project => "COALESCE(project_hash, '__none__')",
    };
    let mut series = BTreeMap::<String, SeriesAcc>::new();
    for (key, label, slot, slot_totals) in slots(conn, &scope, key_sql)? {
        let Some(position) =
            local(tz, slot).and_then(|time| index.get(bucket.label(time).as_str()).copied())
        else {
            continue;
        };
        let entry = series.entry(key).or_insert_with(|| SeriesAcc {
            label: None,
            totals: Totals::default(),
            points: vec![Totals::default(); labels.len()],
        });
        if label.is_some() {
            entry.label = label;
        }
        entry.totals.add(&slot_totals);
        entry.points[position].add(&slot_totals);
    }
    let names = device_names(conn, tenant_id)?;
    let mut ranked = series.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.1.totals
            .total()
            .cmp(&a.1.totals.total())
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut other: Option<SeriesAcc> = None;
    if ranked.len() > limit {
        for (_, acc) in ranked.drain(limit..) {
            let merged = other.get_or_insert_with(|| SeriesAcc {
                label: None,
                totals: Totals::default(),
                points: vec![Totals::default(); labels.len()],
            });
            merged.totals.add(&acc.totals);
            for (into, from) in merged.points.iter_mut().zip(&acc.points) {
                into.add(from);
            }
        }
    }
    let mut out = ranked
        .into_iter()
        .map(|(key, acc)| {
            let label = match group_by {
                GroupBy::None => "全部".to_owned(),
                GroupBy::Device => names.get(&key).cloned().unwrap_or_else(|| key.clone()),
                GroupBy::Project if key == NO_PROJECT_KEY => "未标记项目".to_owned(),
                GroupBy::Project => project_label(&key, acc.label.clone()),
                GroupBy::Client | GroupBy::Model => key.clone(),
            };
            let key = if group_by == GroupBy::None {
                "all".to_owned()
            } else {
                key
            };
            Series {
                key,
                label,
                measures: acc.totals.measures(),
                points: acc.points.iter().map(Totals::point).collect(),
            }
        })
        .collect::<Vec<_>>();
    if let Some(acc) = other {
        out.push(Series {
            key: OTHER_KEY.to_owned(),
            label: "其他".to_owned(),
            measures: acc.totals.measures(),
            points: acc.points.iter().map(Totals::point).collect(),
        });
    }
    Ok(Timeseries {
        range: scope.out(tz),
        bucket: BUCKETS
            .iter()
            .find(|(_, value)| *value == bucket)
            .map_or("day", |(name, _)| name),
        group_by: GROUPS
            .iter()
            .find(|(_, value)| *value == group_by)
            .map_or("none", |(name, _)| name),
        buckets: labels,
        series: out,
        generated_at: iso(now).unwrap_or_default(),
    })
}

// ---------------------------------------------------------------- heatmap --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatmapKind {
    Calendar,
    HourWeekday,
}

pub const HEATMAPS: [(&str, HeatmapKind); 2] = [
    ("calendar", HeatmapKind::Calendar),
    ("hour_weekday", HeatmapKind::HourWeekday),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeatDay {
    pub date: String,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeatCell {
    /// Monday is 0.
    pub weekday: u32,
    pub hour: u32,
    pub total_tokens: i64,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Heatmap {
    pub range: RangeOut,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<Vec<HeatDay>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<HeatCell>>,
    pub max_total_tokens: i64,
    pub generated_at: String,
}

pub fn heatmap(
    conn: &Connection,
    tz: Tz,
    filters: &Filters,
    kind: HeatmapKind,
    tenant_id: &str,
    now: i64,
) -> QueryResult<Heatmap> {
    let scope = scope(conn, tz, filters, tenant_id, now)?;
    let slots = slots(conn, &scope, "''")?;
    let (days, cells, max) = match kind {
        HeatmapKind::Calendar => {
            let labels = Bucket::Day.labels(scope.from, scope.to)?;
            let index = labels
                .iter()
                .enumerate()
                .map(|(position, label)| (label.clone(), position))
                .collect::<HashMap<_, _>>();
            let mut sums = vec![Totals::default(); labels.len()];
            for (_, _, slot, totals) in &slots {
                if let Some(position) =
                    local(tz, *slot).and_then(|time| index.get(&Bucket::Day.label(time)))
                {
                    sums[*position].add(totals);
                }
            }
            let max = sums.iter().map(Totals::total).max().unwrap_or(0);
            let days = labels
                .into_iter()
                .zip(sums)
                .map(|(date, totals)| HeatDay {
                    date,
                    total_tokens: totals.total(),
                    cost_usd: round(totals.cost, 6),
                    message_count: totals.messages,
                })
                .collect();
            (Some(days), None, max)
        }
        HeatmapKind::HourWeekday => {
            let mut sums = vec![Totals::default(); 7 * 24];
            for (_, _, slot, totals) in &slots {
                if let Some(time) = local(tz, *slot) {
                    let weekday = time.weekday().num_days_from_monday() as usize;
                    sums[weekday * 24 + chrono::Timelike::hour(&time) as usize].add(totals);
                }
            }
            let max = sums.iter().map(Totals::total).max().unwrap_or(0);
            let cells = sums
                .iter()
                .enumerate()
                .map(|(position, totals)| HeatCell {
                    weekday: u32::try_from(position / 24).unwrap_or(0),
                    hour: u32::try_from(position % 24).unwrap_or(0),
                    total_tokens: totals.total(),
                    message_count: totals.messages,
                })
                .collect();
            (None, Some(cells), max)
        }
    };
    Ok(Heatmap {
        range: scope.out(tz),
        kind: HEATMAPS
            .iter()
            .find(|(_, value)| *value == kind)
            .map_or("calendar", |(name, _)| name),
        days,
        cells,
        max_total_tokens: max,
        generated_at: iso(now).unwrap_or_default(),
    })
}

// --------------------------------------------------------------- sessions --

/// Reversible so the detail view needs no lookup table; the parts are
/// validated plain text at ingest, so the separator never occurs inside them.
pub fn session_key(device_id: &str, client: &str, root: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{device_id}{SEP}{client}{SEP}{root}"))
}

pub fn parse_session_key(key: &str) -> Option<(String, String, String)> {
    if key.is_empty() || key.len() > 2048 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(key).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let mut parts = text.split(SEP);
    let (device, client, root) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || [device, client, root].iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((device.to_owned(), client.to_owned(), root.to_owned()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSort {
    Recent,
    Tokens,
    Cost,
    Turns,
}

pub const SESSION_SORTS: [(&str, SessionSort); 4] = [
    ("recent", SessionSort::Recent),
    ("tokens", SessionSort::Tokens),
    ("cost", SessionSort::Cost),
    ("turns", SessionSort::Turns),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_key: String,
    pub session_id: String,
    pub device_id: String,
    pub device_name: String,
    pub client: String,
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_label: Option<String>,
    pub started_at: String,
    pub last_used_at: String,
    pub turn_count: i64,
    pub subagent_count: i64,
    #[serde(flatten)]
    pub measures: Measures,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPage {
    pub range: RangeOut,
    pub sessions: Vec<SessionSummary>,
    pub total: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub generated_at: String,
}

pub struct SessionQuery {
    pub search: Option<String>,
    pub sort: SessionSort,
    pub offset: usize,
    pub limit: usize,
}

pub fn parse_cursor(raw: Option<String>) -> QueryResult<usize> {
    match raw {
        None => Ok(0),
        Some(raw) => raw.parse::<usize>().map_err(|_| QueryError::Bad),
    }
}

fn like_pattern(search: &str) -> String {
    let mut pattern = String::from("%");
    for ch in search.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

pub fn sessions(
    conn: &Connection,
    tz: Tz,
    filters: &Filters,
    request: &SessionQuery,
    tenant_id: &str,
    now: i64,
) -> QueryResult<SessionPage> {
    let scope = scope(conn, tz, filters, tenant_id, now)?;
    let names = device_names(conn, tenant_id)?;
    let mut values = scope.values.clone();
    let having = match &request.search {
        Some(search) => {
            let pattern = like_pattern(search);
            values.extend(std::iter::repeat_n(SqlValue::Text(pattern), 4));
            "HAVING SUM(session_id LIKE ? ESCAPE '\\'
                     OR COALESCE(parent_session_id, '') LIKE ? ESCAPE '\\'
                     OR model_id LIKE ? ESCAPE '\\'
                     OR COALESCE(project_label, '') LIKE ? ESCAPE '\\') > 0"
        }
        None => "",
    };
    let grouped_sql = format!(
        "FROM counted_events {} GROUP BY device_id, client, COALESCE(parent_session_id, session_id) {having}",
        scope.clause
    );
    let total = conn.query_row(
        &format!("SELECT COUNT(*) FROM (SELECT 1 {grouped_sql})"),
        params_from_iter(values.iter()),
        |row| row.get::<_, i64>(0),
    )?;
    let order = match request.sort {
        SessionSort::Recent => "last_ts DESC",
        SessionSort::Tokens => "total_tokens DESC, last_ts DESC",
        SessionSort::Cost => "cost DESC, last_ts DESC",
        SessionSort::Turns => "turns DESC, last_ts DESC",
    };
    let mut page_values = values;
    page_values.push(SqlValue::Integer(
        i64::try_from(request.limit).unwrap_or(i64::MAX),
    ));
    page_values.push(SqlValue::Integer(
        i64::try_from(request.offset).unwrap_or(i64::MAX),
    ));
    let mut statement = conn.prepare(&format!(
        "SELECT device_id, client, COALESCE(parent_session_id, session_id) AS root, {TOTALS_SQL},
           MIN(ts), MAX(ts) AS last_ts,
           MAX(COALESCE(SUM(CASE WHEN parent_session_id IS NULL THEN is_turn_start ELSE 0 END), 0),
               MAX(parent_session_id IS NULL)) AS turns,
           COUNT(DISTINCT CASE WHEN parent_session_id IS NOT NULL THEN session_id END),
           group_concat(DISTINCT model_id), MAX(project_label),
           SUM(input + output + cache_read + cache_write + reasoning) AS total_tokens,
           COALESCE(SUM(COALESCE(cost_usd, est_cost_usd)), 0.0) AS cost
         {grouped_sql}
         ORDER BY {order}, device_id, client, root
         LIMIT ? OFFSET ?"
    ))?;
    let base = 3 + TOTALS_WIDTH;
    let rows = statement
        .query_map(params_from_iter(page_values.iter()), |row| {
            let device_id = row.get::<_, String>(0)?;
            let client = row.get::<_, String>(1)?;
            let root = row.get::<_, String>(2)?;
            let totals = Totals::read(row, 3)?;
            let models = row
                .get::<_, Option<String>>(base + 4)?
                .map(|joined| {
                    let mut models = joined.split(',').map(str::to_owned).collect::<Vec<_>>();
                    models.sort();
                    models
                })
                .unwrap_or_default();
            Ok(SessionSummary {
                session_key: session_key(&device_id, &client, &root),
                session_id: root,
                device_name: names
                    .get(&device_id)
                    .cloned()
                    .unwrap_or_else(|| device_id.clone()),
                device_id,
                client,
                models,
                project_label: row.get(base + 5)?,
                started_at: iso(row.get(base)?).unwrap_or_default(),
                last_used_at: iso(row.get(base + 1)?).unwrap_or_default(),
                turn_count: row.get(base + 2)?,
                subagent_count: row.get(base + 3)?,
                measures: totals.measures(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let next = request.offset + rows.len();
    Ok(SessionPage {
        range: scope.out(tz),
        next_cursor: (i64::try_from(next).unwrap_or(i64::MAX) < total).then(|| next.to_string()),
        sessions: rows,
        total,
        generated_at: iso(now).unwrap_or_default(),
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurn {
    /// 1-based, in start order across the session and its subagents.
    pub index: usize,
    pub started_at: String,
    pub ended_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    pub models: Vec<String>,
    pub is_subagent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<String>,
    #[serde(flatten)]
    pub measures: Measures,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub session_id: String,
    pub started_at: String,
    pub last_used_at: String,
    pub models: Vec<String>,
    pub turn_count: usize,
    #[serde(flatten)]
    pub measures: Measures,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    pub session: SessionSummary,
    pub turns: Vec<SessionTurn>,
    pub subagents: Vec<Subagent>,
    /// More than `MAX_SESSION_EVENTS` events or `MAX_TURNS` turns.
    pub truncated: bool,
    pub generated_at: String,
}

struct DetailEvent {
    session_id: String,
    parent: Option<String>,
    model: String,
    ts: i64,
    totals: Totals,
    is_turn_start: bool,
    duration_ms: Option<i64>,
    project_label: Option<String>,
}

fn event_totals(row: &Row<'_>) -> rusqlite::Result<Totals> {
    let tokens = [
        row.get::<_, i64>(4)?,
        row.get::<_, i64>(5)?,
        row.get::<_, i64>(6)?,
        row.get::<_, i64>(7)?,
        row.get::<_, i64>(8)?,
    ];
    let cost = row.get::<_, Option<f64>>(9)?;
    let precision = row.get::<_, String>(10)?;
    let credits = row.get::<_, Option<f64>>(11)?;
    let total = tokens.iter().sum::<i64>();
    Ok(Totals {
        input: tokens[0],
        output: tokens[1],
        cache_read: tokens[2],
        cache_write: tokens[3],
        reasoning: tokens[4],
        cost: cost.unwrap_or(0.0),
        priced: i64::from(cost.is_some()),
        unpriced_tokens: if cost.is_none() && credits.is_none() {
            total
        } else {
            0
        },
        credits,
        messages: row.get(12)?,
        measured: i64::from(precision == "measured"),
        estimated: i64::from(precision == "estimated"),
        unknown: i64::from(precision == "unknown"),
        unknown_tokens: if precision == "unknown" { total } else { 0 },
        events: 1,
        partial: i64::from(row.get::<_, Option<String>>(16)?.as_deref() == Some("partial")),
    })
}

struct TurnAcc {
    started: i64,
    ended: i64,
    duration: Option<i64>,
    models: BTreeSet<String>,
    subagent: Option<String>,
    totals: Totals,
}

/// The whole session regardless of filters: the root session's events and
/// those of every subagent that names it as parent.
pub fn session_detail(
    conn: &Connection,
    tenant_id: &str,
    key: &str,
    now: i64,
) -> QueryResult<Option<SessionDetail>> {
    let Some((device_id, client, root)) = parse_session_key(key) else {
        return Ok(None);
    };
    let mut statement = conn.prepare(
        "SELECT session_id, parent_session_id, model_id, ts, input, output, cache_read,
           cache_write, reasoning, COALESCE(cost_usd, est_cost_usd), precision, credits,
           message_count, is_turn_start, duration_ms, project_label, price_status
         FROM counted_events
          WHERE tenant_id = ?1 AND device_id = ?2 AND client = ?3
            AND ((session_id = ?4 AND parent_session_id IS NULL) OR parent_session_id = ?4)
          ORDER BY ts, event_key LIMIT ?5",
    )?;
    let mut events = statement
        .query_map(
            params![
                tenant_id,
                device_id,
                client,
                root,
                i64::try_from(MAX_SESSION_EVENTS + 1).unwrap_or(i64::MAX)
            ],
            |row| {
                Ok(DetailEvent {
                    session_id: row.get(0)?,
                    parent: row.get(1)?,
                    model: row.get(2)?,
                    ts: row.get(3)?,
                    totals: event_totals(row)?,
                    is_turn_start: row.get(13)?,
                    duration_ms: row.get(14)?,
                    project_label: row.get(15)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if events.is_empty() {
        return Ok(None);
    }
    let mut truncated = events.len() > MAX_SESSION_EVENTS;
    events.truncate(MAX_SESSION_EVENTS);

    let mut overall = Totals::default();
    let mut models = BTreeSet::new();
    let mut project = None;
    let mut turns = Vec::<TurnAcc>::new();
    let mut open = HashMap::<Option<String>, usize>::new();
    let mut subagents = BTreeMap::<String, (i64, i64, BTreeSet<String>, usize, Totals)>::new();
    let mut root_turns = 0_i64;
    for event in &events {
        overall.add(&event.totals);
        models.insert(event.model.clone());
        if event.project_label.is_some() {
            project.clone_from(&event.project_label);
        }
        let stream = event.parent.as_ref().map(|_| event.session_id.clone());
        let current = match open.get(&stream) {
            Some(&position) if !event.is_turn_start => Some(position),
            _ if turns.len() >= MAX_TURNS => {
                truncated = true;
                None
            }
            _ => {
                turns.push(TurnAcc {
                    started: event.ts,
                    ended: event.ts,
                    duration: None,
                    models: BTreeSet::new(),
                    subagent: stream.clone(),
                    totals: Totals::default(),
                });
                open.insert(stream.clone(), turns.len() - 1);
                if let Some(id) = &stream {
                    subagents
                        .entry(id.clone())
                        .or_insert_with(|| {
                            (event.ts, event.ts, BTreeSet::new(), 0, Totals::default())
                        })
                        .3 += 1;
                } else {
                    root_turns += 1;
                }
                Some(turns.len() - 1)
            }
        };
        if let Some(position) = current {
            let turn = &mut turns[position];
            turn.ended = event.ts;
            turn.models.insert(event.model.clone());
            turn.totals.add(&event.totals);
            if let Some(duration) = event.duration_ms {
                turn.duration = Some(turn.duration.unwrap_or(0) + duration);
            }
        }
        if let Some(id) = &stream {
            let entry = subagents
                .entry(id.clone())
                .or_insert_with(|| (event.ts, event.ts, BTreeSet::new(), 0, Totals::default()));
            entry.1 = event.ts;
            entry.2.insert(event.model.clone());
            entry.4.add(&event.totals);
        }
    }
    let first = events.first().map_or(0, |event| event.ts);
    let last = events.last().map_or(0, |event| event.ts);
    let names = device_names(conn, tenant_id)?;
    let session = SessionSummary {
        session_key: key.to_owned(),
        session_id: root,
        device_name: names
            .get(&device_id)
            .cloned()
            .unwrap_or_else(|| device_id.clone()),
        device_id,
        client,
        models: models.into_iter().collect(),
        project_label: project,
        started_at: iso(first).unwrap_or_default(),
        last_used_at: iso(last).unwrap_or_default(),
        turn_count: root_turns,
        subagent_count: i64::try_from(subagents.len()).unwrap_or(i64::MAX),
        measures: overall.measures(),
    };
    Ok(Some(SessionDetail {
        session,
        turns: turns
            .into_iter()
            .enumerate()
            .map(|(position, turn)| SessionTurn {
                index: position + 1,
                started_at: iso(turn.started).unwrap_or_default(),
                ended_at: iso(turn.ended).unwrap_or_default(),
                duration_ms: turn.duration,
                models: turn.models.into_iter().collect(),
                is_subagent: turn.subagent.is_some(),
                subagent_session_id: turn.subagent,
                measures: turn.totals.measures(),
            })
            .collect(),
        subagents: subagents
            .into_iter()
            .map(
                |(session_id, (started, ended, models, turn_count, totals))| Subagent {
                    session_id,
                    started_at: iso(started).unwrap_or_default(),
                    last_used_at: iso(ended).unwrap_or_default(),
                    models: models.into_iter().collect(),
                    turn_count,
                    measures: totals.measures(),
                },
            )
            .collect(),
        truncated,
        generated_at: iso(now).unwrap_or_default(),
    }))
}

// ---------------------------------------------------------------- devices --

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCoverage {
    pub client: String,
    pub detected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_events: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub device_id: String,
    pub display_name: String,
    pub platform: Option<String>,
    pub form_factor: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub created_at: String,
    pub last_seen_at: Option<String>,
    pub last_upload_at: Option<String>,
    pub collector_version: Option<String>,
    pub tokscale_rev: Option<String>,
    pub parser_generation: Option<String>,
    pub backfill_complete: bool,
    pub last_scan_at: Option<String>,
    pub last_scan_duration_ms: Option<i64>,
    pub outbox_backlog: Option<i64>,
    /// `offline` is normal for a sleeping laptop; only errors are red (03 §7.4).
    pub connection: &'static str,
    pub outdated: bool,
    pub is_current: bool,
    pub clients: Vec<ClientCoverage>,
}

fn version_parts(version: &str) -> Option<[u64; 3]> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut out = [0_u64; 3];
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Unknown or unparsable versions are not flagged; there is nothing to act on.
pub fn is_outdated(version: Option<&str>, minimum: &str) -> bool {
    match (version.and_then(version_parts), version_parts(minimum)) {
        (Some(version), Some(minimum)) => version < minimum,
        _ => false,
    }
}

pub fn connection(last_seen_at: Option<i64>, now: i64) -> &'static str {
    match last_seen_at {
        Some(seen) if now - seen <= ONLINE_WINDOW_MS => "online",
        Some(seen) if now - seen <= LONG_OFFLINE_MS => "offline",
        Some(_) => "long_offline",
        // Paired but never reported: waiting for the first upload, not missing.
        None => "offline",
    }
}

pub fn device_statuses(
    conn: &Connection,
    tenant_id: &str,
    now: i64,
    caller: Option<&str>,
) -> QueryResult<Vec<DeviceStatus>> {
    let mut coverage = HashMap::<String, Vec<ClientCoverage>>::new();
    {
        let mut statement = conn.prepare(
            "SELECT s.device_id, s.client, s.detected, s.last_event_at, s.last_scan_events,
               s.error_code, s.updated_at
             FROM device_client_status s JOIN devices d ON d.id = s.device_id
             WHERE d.tenant_id = ?1 AND d.revoked_at IS NULL ORDER BY s.client",
        )?;
        let rows = statement.query_map(params![tenant_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ClientCoverage {
                    client: row.get(1)?,
                    detected: row.get(2)?,
                    last_event_at: row.get::<_, Option<i64>>(3)?.and_then(iso),
                    last_scan_events: row.get(4)?,
                    error_code: row.get(5)?,
                    updated_at: iso(row.get(6)?).unwrap_or_default(),
                },
            ))
        })?;
        for row in rows {
            let (device, client) = row?;
            coverage.entry(device).or_default().push(client);
        }
    }
    let mut statement = conn.prepare(
        "SELECT id, display_name, platform, form_factor, os_name, os_version, created_at,
           last_seen_at, last_upload_at, collector_version, tokscale_rev, parser_generation,
           backfill_complete, last_scan_at, last_scan_duration_ms, outbox_backlog
         FROM devices WHERE tenant_id = ?1 AND revoked_at IS NULL ORDER BY created_at, id",
    )?;
    let devices = statement
        .query_map(params![tenant_id], |row| {
            let id = row.get::<_, String>(0)?;
            let last_seen = row.get::<_, Option<i64>>(7)?;
            let collector_version = row.get::<_, Option<String>>(9)?;
            Ok(DeviceStatus {
                display_name: row
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| id.clone()),
                platform: row.get(2)?,
                form_factor: row.get(3)?,
                os_name: row.get(4)?,
                os_version: row.get(5)?,
                created_at: iso(row.get(6)?).unwrap_or_default(),
                last_seen_at: last_seen.and_then(iso),
                last_upload_at: row.get::<_, Option<i64>>(8)?.and_then(iso),
                outdated: is_outdated(collector_version.as_deref(), MIN_COLLECTOR_VERSION),
                collector_version,
                tokscale_rev: row.get(10)?,
                parser_generation: row.get(11)?,
                backfill_complete: row.get(12)?,
                last_scan_at: row.get::<_, Option<i64>>(13)?.and_then(iso),
                last_scan_duration_ms: row.get(14)?,
                outbox_backlog: row.get(15)?,
                connection: connection(last_seen, now),
                is_current: caller == Some(id.as_str()),
                clients: Vec::new(),
                device_id: id,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut devices: Vec<DeviceStatus> = devices
        .into_iter()
        .map(|mut device| {
            device.clients = coverage.remove(&device.device_id).unwrap_or_default();
            device
        })
        .collect();
    devices.sort_by_key(|device| !device.is_current);
    Ok(devices)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceUsage {
    pub device_id: String,
    #[serde(flatten)]
    pub measures: Measures,
    pub by_client: Vec<Slice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Thresholds {
    pub online_ms: i64,
    pub long_offline_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Devices {
    pub range: RangeOut,
    pub devices: Vec<DeviceStatus>,
    /// Usage in the range for each listed device, in the same order.
    pub usage: Vec<DeviceUsage>,
    /// Every client seen in coverage or usage: the matrix columns.
    pub clients: Vec<String>,
    pub caveats: Vec<Caveat>,
    pub thresholds: Thresholds,
    pub min_collector_version: &'static str,
    pub generated_at: String,
}

pub fn devices(
    conn: &Connection,
    tz: Tz,
    filters: &Filters,
    tenant_id: &str,
    caller: &str,
    now: i64,
) -> QueryResult<Devices> {
    let devices = device_statuses(conn, tenant_id, now, Some(caller))?;
    let scope = scope(conn, tz, filters, tenant_id, now)?;
    let mut statement = conn.prepare(&format!(
        "SELECT device_id, client, {TOTALS_SQL} FROM counted_events {} GROUP BY 1, 2",
        scope.clause
    ))?;
    let mut per_device = HashMap::<String, (Totals, Vec<(String, Totals)>)>::new();
    let rows = statement.query_map(params_from_iter(scope.values.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            Totals::read(row, 2)?,
        ))
    })?;
    let mut clients = BTreeSet::new();
    for row in rows {
        let (device, client, totals) = row?;
        clients.insert(client.clone());
        let entry = per_device.entry(device).or_default();
        entry.0.add(&totals);
        entry.1.push((client, totals));
    }
    for device in &devices {
        clients.extend(device.clients.iter().map(|client| client.client.clone()));
    }
    let usage = devices
        .iter()
        .map(|device| {
            let (totals, mut by_client) = per_device.remove(&device.device_id).unwrap_or_default();
            by_client.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));
            DeviceUsage {
                device_id: device.device_id.clone(),
                measures: totals.measures(),
                by_client: by_client
                    .into_iter()
                    .map(|(client, totals)| Slice {
                        label: client.clone(),
                        key: client,
                        measures: totals.measures(),
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(Devices {
        range: scope.out(tz),
        caveats: device_caveats(&devices),
        devices,
        usage,
        clients: clients.into_iter().collect(),
        thresholds: Thresholds {
            online_ms: ONLINE_WINDOW_MS,
            long_offline_ms: LONG_OFFLINE_MS,
        },
        min_collector_version: MIN_COLLECTOR_VERSION,
        generated_at: iso(now).unwrap_or_default(),
    })
}

// ---------------------------------------------------------------- filters --

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterOption {
    pub key: String,
    pub label: String,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterOptions {
    pub devices: Vec<FilterOption>,
    pub clients: Vec<FilterOption>,
    pub models: Vec<FilterOption>,
    pub projects: Vec<FilterOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_event_date: Option<String>,
    pub time_zone: String,
}

/// Every value ever seen, for the filter pickers; independent of filters.
pub fn filter_options(conn: &Connection, tz: Tz, tenant_id: &str) -> QueryResult<FilterOptions> {
    let names = device_names(conn, tenant_id)?;
    let options = |column: &str,
                   label: &dyn Fn(String, Option<String>) -> String|
     -> QueryResult<Vec<FilterOption>> {
        let mut statement = conn.prepare(&format!(
            "SELECT {column}, MAX(project_label),
               COALESCE(SUM(input + output + cache_read + cache_write + reasoning), 0)
             FROM counted_events WHERE tenant_id = ?1 AND {column} IS NOT NULL GROUP BY 1 ORDER BY 3 DESC, 1"
        ))?;
        let rows = statement
            .query_map(params![tenant_id], |row| {
                let key = row.get::<_, String>(0)?;
                Ok(FilterOption {
                    label: label(key.clone(), row.get(1)?),
                    key,
                    total_tokens: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    let devices = options("device_id", &|key, _| {
        names.get(&key).cloned().unwrap_or(key)
    })?;
    let clients = options("client", &|key, _| key)?;
    let models = options("model_id", &|key, _| key)?;
    let projects = options("project_hash", &|key, label| project_label(&key, label))?;
    let first = conn.query_row(
        "SELECT MIN(ts) FROM counted_events WHERE tenant_id = ?1",
        params![tenant_id],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(FilterOptions {
        devices,
        clients,
        models,
        projects,
        first_event_date: first
            .and_then(|ms| local_date(tz, ms))
            .map(|date| date.format("%Y-%m-%d").to_string()),
        time_zone: tz.name().to_owned(),
    })
}

// ----------------------------------------------------------------- health --

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub status: &'static str,
    pub server_version: &'static str,
    pub schema_version: u32,
    /// Stored rows of the caller's tenant. The database file size is not
    /// reported: it would reveal how much the other tenants store.
    pub event_count: i64,
    pub device_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ingest_at: Option<String>,
    pub time_zone: String,
    pub min_collector_version: &'static str,
    pub min_app_version: &'static str,
    pub generated_at: String,
}

pub fn health(conn: &Connection, tz: Tz, tenant_id: &str, now: i64) -> QueryResult<Health> {
    let event_count = conn.query_row(
        "SELECT COUNT(*) FROM usage_events WHERE tenant_id = ?1",
        params![tenant_id],
        |row| row.get(0),
    )?;
    let device_count = conn.query_row(
        "SELECT COUNT(*) FROM devices WHERE tenant_id = ?1 AND revoked_at IS NULL",
        params![tenant_id],
        |row| row.get(0),
    )?;
    let last_ingest = conn
        .query_row(
            "SELECT MAX(received_at) FROM ingest_batches WHERE tenant_id = ?1",
            params![tenant_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    Ok(Health {
        status: "ok",
        server_version: env!("CARGO_PKG_VERSION"),
        schema_version: db::schema_version(conn)?,
        event_count,
        device_count,
        last_ingest_at: last_ingest.and_then(iso),
        time_zone: tz.name().to_owned(),
        min_collector_version: MIN_COLLECTOR_VERSION,
        min_app_version: MIN_APP_VERSION,
        generated_at: iso(now).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_keys_round_trip_and_reject_junk() {
        let key = session_key("dev_1", "codex", "abc-123");
        assert_eq!(
            parse_session_key(&key),
            Some(("dev_1".to_owned(), "codex".to_owned(), "abc-123".to_owned()))
        );
        assert_eq!(parse_session_key(""), None);
        assert_eq!(parse_session_key("!!!"), None);
        assert_eq!(parse_session_key(&URL_SAFE_NO_PAD.encode("a\u{1f}b")), None);
        assert_eq!(
            parse_session_key(&URL_SAFE_NO_PAD.encode("a\u{1f}b\u{1f}c\u{1f}d")),
            None
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_outdated(Some("0.0.9"), "0.1.0"));
        assert!(is_outdated(Some("0.9.0"), "0.10.0"));
        assert!(!is_outdated(Some("0.1.0"), "0.1.0"));
        assert!(!is_outdated(Some("0.1.1-dev"), "0.1.0"));
        assert!(!is_outdated(None, "0.1.0"));
        assert!(!is_outdated(Some("garbage"), "0.1.0"));
    }

    #[test]
    fn connection_thresholds() {
        let now = 10 * LONG_OFFLINE_MS;
        assert_eq!(connection(Some(now - ONLINE_WINDOW_MS), now), "online");
        assert_eq!(connection(Some(now - ONLINE_WINDOW_MS - 1), now), "offline");
        assert_eq!(
            connection(Some(now - LONG_OFFLINE_MS - 1), now),
            "long_offline"
        );
        assert_eq!(connection(None, now), "offline");
    }

    #[test]
    fn precision_and_hit_rate_follow_the_spec() {
        let mut totals = Totals {
            input: 10,
            cache_read: 30,
            cache_write: 10,
            measured: 2,
            events: 2,
            ..Totals::default()
        };
        assert_eq!(totals.precision(), "measured");
        assert_eq!(totals.cache_hit_rate(), Some(0.6));
        totals.add(&Totals {
            estimated: 1,
            events: 1,
            ..Totals::default()
        });
        assert_eq!(totals.precision(), "mixed");
        assert_eq!(Totals::default().cache_hit_rate(), None);
        assert_eq!(Totals::default().precision(), "unknown");
    }

    #[test]
    fn query_strings_are_strict() {
        assert!(parse_query(Some("from=2026-01-01&clients=a,b"), &[]).is_ok());
        assert!(parse_query(Some("form=2026-01-01"), &[]).is_err());
        assert!(parse_query(Some("from=2026-01-01&from=2026-01-02"), &[]).is_err());
        let query = parse_query(Some("from=2026-02-01&to=2026-01-01"), &[]).expect("parse");
        assert!(Filters::from_query(&query).is_err());
        let query = parse_query(Some("from=1999-12-31"), &[]).expect("parse");
        assert!(Filters::from_query(&query).is_err());
        let query = parse_query(Some("clients=%20a%20,,b,a"), &[]).expect("parse");
        assert_eq!(
            Filters::from_query(&query).expect("filters").clients,
            ["a", "b"]
        );
    }

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern("a%b_c\\"), "%a\\%b\\_c\\\\%");
    }

    #[test]
    fn grouped_numbers() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(1234), "1,234");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
