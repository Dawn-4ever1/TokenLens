//! Read-only reconciliation against the Token Monitor Hub (D012).
//!
//! Pure logic only: the binary does the single network read and the local
//! scan. Nothing here keeps device names, ids, paths or source text.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone};
use collector_core::DailyUsage;
use serde_json::Value;

pub const SECRET_ENV: &str = "TOKEN_MONITOR_HUB_SECRET";
pub const URL_ENV: &str = "TOKEN_MONITOR_HUB_URL";
pub const HUB_QODER_CLIENT: &str = "qodercn";
pub const THRESHOLD_PERCENT: f64 = 1.0;

/// Token Monitor `shared/history.js` (d08a1b0): only these clients add
/// reasoning to the day total and to output.
pub const HUB_DISJOINT_REASONING: &[&str] = &[
    "reasonix",
    "antigravity",
    "codex",
    "droid",
    "dsh",
    "grok",
    "gemini",
    "zcode",
    "opencode",
    "muse",
    "codebuddy",
    "workbuddy",
    "cherrystudio",
    "hermes",
    "jcode",
];

/// Token Monitor `TOKSCALE_CLIENT_ALIASES` (d08a1b0).
const HUB_ALIASES: &[(&str, &str)] = &[
    ("antigravity-cli", "antigravity"),
    ("antigravity-extension", "antigravity"),
    ("micode", "mimo"),
    ("micode-desktop", "mimo"),
    ("kilocode", "kilo"),
    ("devin-cli", "devin"),
    ("devin-desktop", "devin"),
    ("vibe", "mistral"),
];

/// The Qoder bridge script was changed at this device-local time.
pub fn bridge_change() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 10)
        .and_then(|date| date.and_hms_opt(20, 2, 0))
        .expect("valid constant")
}

pub fn hub_client_for(ours: &str) -> String {
    if ours == collector_core::QODER_CLIENT {
        return HUB_QODER_CLIENT.to_owned();
    }
    let lower = ours.trim().to_ascii_lowercase();
    HUB_ALIASES
        .iter()
        .find(|(from, _)| *from == lower)
        .map_or(lower, |(_, to)| (*to).to_owned())
}

pub fn hub_counts_reasoning(hub_client: &str) -> bool {
    HUB_DISJOINT_REASONING.contains(&hub_client)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    MissingSecret,
    BadUrl,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSecret => write!(f, "没有读到 {SECRET_ENV}"),
            Self::BadUrl => write!(
                f,
                "{URL_ENV} 必须是 https:// 开头的地址，且不能带用户名、问号参数或 #"
            ),
        }
    }
}

/// The secret lives only in the header value; `Debug` never shows it.
pub struct HubRequest {
    pub url: String,
    authorization: String,
}

impl HubRequest {
    pub fn devices(base: &str, secret: &str) -> Result<Self, RequestError> {
        let secret = secret.trim();
        if secret.is_empty() || secret.chars().any(char::is_control) {
            return Err(RequestError::MissingSecret);
        }
        let base = collector_core::transport::validate_server_url(base)
            .map_err(|_| RequestError::BadUrl)?;
        Ok(Self {
            url: format!("{base}/api/devices"),
            authorization: format!("Bearer {secret}"),
        })
    }

    pub fn authorization(&self) -> &str {
        &self.authorization
    }
}

impl fmt::Debug for HubRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubRequest")
            .field("url", &self.url)
            .field("authorization", &"<redacted>")
            .finish()
    }
}

fn device_records(body: &Value) -> &[Value] {
    body.get("devices")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())
        .map_or(&[], Vec::as_slice)
}

fn nonblank<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// For the terminal only; never rendered into the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubDevice {
    pub id: String,
    pub label: String,
    pub last_update: Option<String>,
    pub time_zone: Option<String>,
    pub history_days: usize,
}

pub fn hub_devices(body: &Value) -> Vec<HubDevice> {
    device_records(body)
        .iter()
        .filter_map(|record| {
            let id = nonblank(record, "deviceId")?.to_owned();
            let label = ["displayName", "deviceId", "hostname"]
                .iter()
                .find_map(|key| nonblank(record, key))
                .unwrap_or("")
                .to_owned();
            Some(HubDevice {
                id,
                label,
                last_update: nonblank(record, "updatedAt")
                    .map(|text| text.chars().take(10).collect()),
                time_zone: hub_time_zone(record).map(str::to_owned),
                history_days: history_rows(record).len(),
            })
        })
        .collect()
}

pub fn find_device<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    device_records(body)
        .iter()
        .find(|record| nonblank(record, "deviceId") == Some(id.trim()))
}

pub fn hub_time_zone(device: &Value) -> Option<&str> {
    device
        .get("periodWindows")
        .and_then(|windows| nonblank(windows, "timeZone"))
}

fn history_rows(device: &Value) -> &[Value] {
    device
        .get("history")
        .and_then(|history| history.get("daily"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn num(value: Option<&Value>) -> i64 {
    match value.and_then(Value::as_f64) {
        Some(number) if number.is_finite() && number > 0.0 => number.round() as i64,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Components {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl Components {
    fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    fn named(&self) -> [(&'static str, i64); 4] {
        [
            ("input", self.input),
            ("output", self.output),
            ("cache read", self.cache_read),
            ("cache write", self.cache_write),
        ]
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubCell {
    pub tokens: i64,
    pub messages: i64,
    /// `None` when the Hub no longer keeps (or never had) the split that day.
    pub components: Option<Components>,
}

/// `(Hub client id, device-local date)`.
pub type Key = (String, NaiveDate);

fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
}

/// Follows Token Monitor's `unclassifiedTokensFor`: an explicit value wins,
/// otherwise the row flag decides whether the whole amount is unclassified.
pub fn hub_grid(device: &Value, since: NaiveDate, until: NaiveDate) -> BTreeMap<Key, HubCell> {
    let mut grid: BTreeMap<Key, HubCell> = BTreeMap::new();
    for row in history_rows(device) {
        let Some(date) = row.get("date").and_then(Value::as_str).and_then(parse_date) else {
            continue;
        };
        if date < since || date > until {
            continue;
        }
        let row_exact = row.get("tokenComponentsAvailable").and_then(Value::as_bool) == Some(true);
        let Some(clients) = row.get("perClient").and_then(Value::as_object) else {
            continue;
        };
        for (client, value) in clients {
            let tokens = num(value.get("tokens"));
            let cache_read = num(value.get("cacheReadTokens"));
            let cache_write = num(value.get("cacheWriteTokens"));
            let output = num(value.get("outputTokens"));
            let unclassified = match value.get("unclassifiedTokens") {
                Some(explicit) => num(Some(explicit)),
                None if row_exact => 0,
                None => tokens,
            };
            let classified = cache_read + cache_write + output;
            let components = (unclassified == 0 && classified <= tokens).then_some(Components {
                input: tokens - classified,
                output,
                cache_read,
                cache_write,
            });
            let cell = grid
                .entry((hub_client_for(client), date))
                .or_insert_with(|| HubCell {
                    components: Some(Components::default()),
                    ..HubCell::default()
                });
            cell.tokens += tokens;
            cell.messages += num(value.get("messages"));
            cell.components = match (cell.components, components) {
                (Some(mut sum), Some(more)) => {
                    sum.add(&more);
                    Some(sum)
                }
                _ => None,
            };
        }
    }
    grid
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OurCell {
    /// This project's own total (reasoning always included).
    pub native_total: i64,
    /// The same events added up the way the Hub adds them.
    pub hub_view_total: i64,
    pub messages: i64,
    /// Hub view: output includes reasoning only where the Hub does so.
    pub components: Components,
    pub reasoning: i64,
    pub precision: BTreeMap<String, u64>,
}

pub fn our_grid(
    daily: &[DailyUsage],
    since: NaiveDate,
    until: NaiveDate,
) -> BTreeMap<Key, OurCell> {
    let mut grid: BTreeMap<Key, OurCell> = BTreeMap::new();
    for day in daily {
        let Some(date) = parse_date(&day.date) else {
            continue;
        };
        if date < since || date > until {
            continue;
        }
        let client = hub_client_for(&day.client);
        let reasoning = if hub_counts_reasoning(&client) {
            day.tokens.reasoning
        } else {
            0
        };
        let tokens = &day.tokens;
        let cell = grid.entry((client, date)).or_default();
        cell.native_total += tokens.total;
        cell.hub_view_total +=
            tokens.input + tokens.output + tokens.cache_read + tokens.cache_write + reasoning;
        cell.messages += day.messages;
        cell.components.add(&Components {
            input: tokens.input,
            output: tokens.output + reasoning,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write,
        });
        cell.reasoning += tokens.reasoning;
        for (precision, count) in &day.precision_counts {
            *cell.precision.entry(precision.clone()).or_default() += count;
        }
    }
    grid
}

/// Days on which the Hub device's zone and this computer's zone disagree.
pub fn tz_mismatch_days<L: TimeZone>(
    hub_zone: &str,
    local: &L,
    since: NaiveDate,
    until: NaiveDate,
) -> Option<BTreeSet<NaiveDate>> {
    let hub: chrono_tz::Tz = hub_zone.parse().ok()?;
    let mut days = BTreeSet::new();
    let mut date = since;
    while date <= until {
        let start = date.and_time(NaiveTime::MIN);
        let end = start + Duration::hours(24) - Duration::seconds(1);
        if [start, end].iter().any(|at| {
            hub.offset_from_utc_datetime(at).fix() != local.offset_from_utc_datetime(at).fix()
        }) {
            days.insert(date);
        }
        date += Duration::days(1);
    }
    Some(days)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClientPrecision {
    Measured,
    Estimated,
    Mixed,
    Unknown,
    /// Only the Hub has this client.
    HubOnly,
}

impl ClientPrecision {
    fn label(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Estimated => "estimated",
            Self::Mixed => "measured + 其他",
            Self::Unknown => "unknown",
            Self::HubOnly => "本机没有",
        }
    }
}

fn client_precision<'a>(cells: impl Iterator<Item = &'a OurCell>) -> ClientPrecision {
    let mut kinds = BTreeSet::new();
    for cell in cells {
        kinds.extend(
            cell.precision
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(k, _)| k.as_str()),
        );
    }
    match (
        kinds.contains("measured"),
        kinds.contains("estimated"),
        kinds.len(),
    ) {
        (_, _, 0) => ClientPrecision::Unknown,
        (true, _, 1) => ClientPrecision::Measured,
        (false, true, _) => ClientPrecision::Estimated,
        (true, _, _) => ClientPrecision::Mixed,
        _ => ClientPrecision::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub client: String,
    pub date: NaiveDate,
    pub ours: Option<OurCell>,
    pub hub: Option<HubCell>,
    pub diff: i64,
    /// `None` when the Hub has zero and we do not.
    pub percent: Option<f64>,
    pub causes: Vec<String>,
    pub clues: Vec<String>,
}

impl Row {
    pub fn within(&self) -> bool {
        self.percent.is_some_and(|p| p.abs() < THRESHOLD_PERCENT)
    }

    fn ours_total(&self) -> i64 {
        self.ours.as_ref().map_or(0, |cell| cell.hub_view_total)
    }

    fn hub_total(&self) -> i64 {
        self.hub.as_ref().map_or(0, |cell| cell.tokens)
    }
}

fn percent(diff: i64, base: i64) -> Option<f64> {
    if base == 0 {
        return (diff == 0).then_some(0.0);
    }
    Some(diff as f64 * 100.0 / base as f64)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientSummary {
    pub client: String,
    pub precision: ClientPrecision,
    pub days: usize,
    pub within: usize,
    pub worst: Option<(NaiveDate, i64, Option<f64>)>,
    pub ours_total: i64,
    pub ours_native_total: i64,
    pub hub_total: i64,
}

impl ClientSummary {
    pub fn total_percent(&self) -> Option<f64> {
        percent(self.ours_total - self.hub_total, self.hub_total)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reconciliation {
    pub since: NaiveDate,
    pub until: NaiveDate,
    pub rows: Vec<Row>,
    pub summaries: Vec<ClientSummary>,
    /// Our client id → Hub client id, for every client either side reported.
    pub mapping: BTreeMap<String, String>,
    pub hub_only_clients: BTreeSet<String>,
    /// `None` when the Hub device does not report a usable time zone.
    pub tz_mismatch: Option<BTreeSet<NaiveDate>>,
}

fn worse(a: &(NaiveDate, i64, Option<f64>), b: &(NaiveDate, i64, Option<f64>)) -> bool {
    let size = |p: Option<f64>| p.map_or(f64::INFINITY, f64::abs);
    (size(a.2), a.1.unsigned_abs()) > (size(b.2), b.1.unsigned_abs())
}

pub fn reconcile(
    our_clients: &BTreeSet<String>,
    ours: &BTreeMap<Key, OurCell>,
    hub: &BTreeMap<Key, HubCell>,
    since: NaiveDate,
    until: NaiveDate,
    tz_mismatch: Option<BTreeSet<NaiveDate>>,
) -> Reconciliation {
    let keys: BTreeSet<&Key> = ours.keys().chain(hub.keys()).collect();
    let mut rows: Vec<Row> = keys
        .into_iter()
        .map(|key| {
            let our = ours.get(key).cloned();
            let theirs = hub.get(key).cloned();
            let ours_total = our.as_ref().map_or(0, |cell| cell.hub_view_total);
            let hub_total = theirs.as_ref().map_or(0, |cell| cell.tokens);
            let diff = ours_total - hub_total;
            Row {
                client: key.0.clone(),
                date: key.1,
                ours: our,
                hub: theirs,
                diff,
                percent: percent(diff, hub_total),
                causes: Vec::new(),
                clues: Vec::new(),
            }
        })
        .collect();

    let index: BTreeMap<Key, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| ((row.client.clone(), row.date), i))
        .collect();
    for i in 0..rows.len() {
        if rows[i].within() {
            continue;
        }
        let (causes, clues) = attribute(&rows, &index, i, tz_mismatch.as_ref());
        rows[i].causes = causes;
        rows[i].clues = clues;
    }

    let mut summaries: BTreeMap<String, ClientSummary> = BTreeMap::new();
    for row in &rows {
        let summary = summaries
            .entry(row.client.clone())
            .or_insert_with(|| ClientSummary {
                client: row.client.clone(),
                precision: ClientPrecision::HubOnly,
                days: 0,
                within: 0,
                worst: None,
                ours_total: 0,
                ours_native_total: 0,
                hub_total: 0,
            });
        summary.days += 1;
        summary.within += usize::from(row.within());
        summary.ours_total += row.ours_total();
        summary.ours_native_total += row.ours.as_ref().map_or(0, |cell| cell.native_total);
        summary.hub_total += row.hub_total();
        let candidate = (row.date, row.diff, row.percent);
        if !row.within()
            && summary
                .worst
                .as_ref()
                .is_none_or(|worst| worse(&candidate, worst))
        {
            summary.worst = Some(candidate);
        }
    }
    for summary in summaries.values_mut() {
        let cells: Vec<&OurCell> = rows
            .iter()
            .filter(|row| row.client == summary.client)
            .filter_map(|row| row.ours.as_ref())
            .collect();
        if !cells.is_empty() {
            summary.precision = client_precision(cells.into_iter());
        }
    }

    let mapping: BTreeMap<String, String> = our_clients
        .iter()
        .map(|client| (client.clone(), hub_client_for(client)))
        .collect();
    let mapped: BTreeSet<&String> = mapping.values().collect();
    let hub_only_clients: BTreeSet<String> = hub
        .keys()
        .map(|(client, _)| client.clone())
        .filter(|client| !mapped.contains(client))
        .collect();

    Reconciliation {
        since,
        until,
        rows,
        summaries: summaries.into_values().collect(),
        mapping,
        hub_only_clients,
        tz_mismatch,
    }
}

fn attribute(
    rows: &[Row],
    index: &BTreeMap<Key, usize>,
    i: usize,
    tz_mismatch: Option<&BTreeSet<NaiveDate>>,
) -> (Vec<String>, Vec<String>) {
    let row = &rows[i];
    let mut causes = Vec::new();
    let mut clues = Vec::new();
    match (&row.ours, &row.hub) {
        (Some(_), None) => causes.push(
            "Hub 没有这一天的这个 client（当天可能没同步，或 Token Monitor 没识别这个 client）"
                .to_owned(),
        ),
        (None, Some(_)) => {
            causes.push("本机没有这一天的数据而 Hub 有（本地日志可能已被清理）".to_owned())
        }
        _ => {}
    }
    if tz_mismatch.is_some_and(|days| days.contains(&row.date)) {
        causes.push("这天 Hub 设备时区和本机时区的偏移不同，两边分日可能不同".to_owned());
    }
    for neighbour in [row.date - Duration::days(1), row.date + Duration::days(1)] {
        let Some(&j) = index.get(&(row.client.clone(), neighbour)) else {
            continue;
        };
        let other = &rows[j];
        let base = row.hub_total() + other.hub_total();
        let combined = row.diff + other.diff;
        if row.diff.signum() == -other.diff.signum()
            && other.diff != 0
            && percent(combined, base).is_some_and(|p| p.abs() < THRESHOLD_PERCENT)
        {
            causes.push(format!(
                "和 {neighbour} 的差异基本抵消（两天合计差 {}），像是跨日边界：同一批消息被两边分到了相邻两天",
                group(combined)
            ));
        }
    }
    if let (Some(ours), Some(hub)) = (&row.ours, &row.hub) {
        if ours.native_total != ours.hub_view_total {
            clues.push(format!(
                "已按 Hub 口径扣掉 reasoning {}（Hub 对这个 client 的总量不含 reasoning）",
                group(ours.native_total - ours.hub_view_total)
            ));
        }
        match hub.components {
            Some(theirs) => {
                let (name, ours_value, hub_value) = ours
                    .components
                    .named()
                    .into_iter()
                    .zip(theirs.named())
                    .map(|((name, a), (_, b))| (name, a, b))
                    .max_by_key(|(_, a, b)| (a - b).unsigned_abs())
                    .expect("four components");
                clues.push(format!(
                    "分项里差得最多的是 {name}：我们 {}，Hub {}",
                    group(ours_value),
                    group(hub_value)
                ));
            }
            None => clues.push("Hub 这天没有分项（Hub 只保留最近约 30 天的分项）".to_owned()),
        }
        if ours.messages != hub.messages {
            clues.push(format!(
                "消息数：我们 {}，Hub {}",
                group(ours.messages),
                group(hub.messages)
            ));
        }
    }
    (causes, clues)
}

pub fn group(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 { format!("-{out}") } else { out }
}

fn signed(value: i64) -> String {
    if value > 0 {
        format!("+{}", group(value))
    } else {
        group(value)
    }
}

pub fn format_percent(value: Option<f64>) -> String {
    match value {
        Some(p) if p.abs() < 0.005 => "0.00%".to_owned(),
        Some(p) => format!("{p:+.2}%"),
        None => "—（Hub 为 0）".to_owned(),
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QoderPeriod {
    pub label: &'static str,
    pub days: usize,
    pub ours: i64,
    pub hub: i64,
    pub our_events: i64,
    pub hub_messages: i64,
    pub without_usage: usize,
    /// Records without usage × the Hub's tokens per message on that day.
    pub estimate: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QoderDay {
    pub date: NaiveDate,
    pub ours: i64,
    pub hub: Option<i64>,
    pub our_events: i64,
    pub hub_messages: i64,
    pub without_usage: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QoderSection {
    pub days: Vec<QoderDay>,
    pub periods: Vec<QoderPeriod>,
    /// Our Hub-view tokens and event count before / after the bridge change
    /// on the change day itself.
    pub change_day_split: Option<((i64, i64), (i64, i64))>,
}

/// `events` are `(timestamp ms, Hub-view tokens)` of our Qoder events.
pub fn qoder_section<L: TimeZone>(
    local: &L,
    ours: &BTreeMap<Key, OurCell>,
    hub: &BTreeMap<Key, HubCell>,
    events: &[(i64, i64)],
    without_usage: &[i64],
    since: NaiveDate,
    until: NaiveDate,
) -> QoderSection {
    let local_time = |ts: i64| {
        local
            .timestamp_millis_opt(ts)
            .single()
            .map(|t| t.naive_local())
    };
    let mut missing: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    for time in without_usage.iter().filter_map(|ts| local_time(*ts)) {
        *missing.entry(time.date()).or_default() += 1;
    }
    let mut days = Vec::new();
    let mut date = since;
    while date <= until {
        let key = (HUB_QODER_CLIENT.to_owned(), date);
        let our = ours.get(&key);
        let theirs = hub.get(&key);
        let without = missing.get(&date).copied().unwrap_or(0);
        if our.is_some() || theirs.is_some() || without > 0 {
            days.push(QoderDay {
                date,
                ours: our.map_or(0, |cell| cell.hub_view_total),
                hub: theirs.map(|cell| cell.tokens),
                our_events: our.map_or(0, |cell| cell.messages),
                hub_messages: theirs.map_or(0, |cell| cell.messages),
                without_usage: without,
            });
        }
        date += Duration::days(1);
    }

    let change = bridge_change();
    let mut periods = [
        QoderPeriod {
            label: "改动前（2026-11-10 之前）",
            ..QoderPeriod::default()
        },
        QoderPeriod {
            label: "改动当天（2026-11-10 全天）",
            ..QoderPeriod::default()
        },
        QoderPeriod {
            label: "改动后（2026-11-11 起）",
            ..QoderPeriod::default()
        },
    ];
    for day in &days {
        let slot = match day.date.cmp(&change.date()) {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Greater => 2,
        };
        let period = &mut periods[slot];
        period.days += 1;
        period.ours += day.ours;
        period.hub += day.hub.unwrap_or(0);
        period.our_events += day.our_events;
        period.hub_messages += day.hub_messages;
        period.without_usage += day.without_usage;
        if let Some(hub_tokens) = day.hub.filter(|_| day.hub_messages > 0) {
            period.estimate += day.without_usage as i64 * hub_tokens / day.hub_messages;
        }
    }

    let change_day_split = (since <= change.date() && change.date() <= until).then(|| {
        let mut before = (0, 0);
        let mut after = (0, 0);
        for (ts, tokens) in events {
            let Some(time) = local_time(*ts) else {
                continue;
            };
            if time.date() != change.date() {
                continue;
            }
            let side = if time < change {
                &mut before
            } else {
                &mut after
            };
            side.0 += tokens;
            side.1 += 1;
        }
        (before, after)
    });

    QoderSection {
        days,
        periods: periods
            .into_iter()
            .filter(|period| period.days > 0)
            .collect(),
        change_day_split,
    }
}

pub fn render(
    rec: &Reconciliation,
    qoder: &QoderSection,
    generated: NaiveDate,
    hub_has_qoder_mirror: bool,
) -> String {
    let mut out = String::new();
    let span = (rec.until - rec.since).num_days() + 1;
    let _ = writeln!(out, "# Token Monitor 对账结果\n");
    let _ = writeln!(out, "- 生成日期：{generated}（本机时区）");
    let _ = writeln!(
        out,
        "- 对比范围：{} ~ {}，共 {span} 个已结束的自然日",
        rec.since, rec.until
    );
    let _ = writeln!(out, "- 粒度：client × 日，只列任一边有数据的日子");
    let with_reasoning: Vec<&str> = rec
        .summaries
        .iter()
        .map(|s| s.client.as_str())
        .filter(|client| hub_counts_reasoning(client))
        .collect();
    let _ = writeln!(
        out,
        "- 比较口径：「我们（按 Hub 口径）」= input + output + cache read + cache write，\
         只有 Token Monitor 规定的一组 client 再加 reasoning（本次涉及：{}）；\
         「我们（原口径）」是本项目的 total，所有 client 都含 reasoning",
        if with_reasoning.is_empty() {
            "无".to_owned()
        } else {
            with_reasoning.join("、")
        }
    );
    let _ = writeln!(out, "- 差异 % =（我们按 Hub 口径 − Hub）÷ Hub");
    let _ = writeln!(
        out,
        "- 达标标准：measured 的 client 每天差异 < {THRESHOLD_PERCENT}%；estimated / unknown 只报告差异"
    );
    let _ = match &rec.tz_mismatch {
        None => writeln!(out, "- 时区：Hub 设备没有报告时区，无法检查分日是否一致"),
        Some(days) if days.is_empty() => {
            writeln!(out, "- 时区：Hub 设备时区与本机时区在整个范围内偏移一致")
        }
        Some(days) => writeln!(
            out,
            "- 时区：有 {} 天 Hub 设备时区与本机时区偏移不同：{}",
            days.len(),
            days.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("、")
        ),
    };

    let _ = writeln!(out, "\n## client 对应关系\n");
    let _ = writeln!(out, "| 我们 | Hub | 说明 |\n|---|---|---|");
    for (ours, hub) in &rec.mapping {
        let note = if ours == collector_core::QODER_CLIENT {
            "我们读 Qoder 国际版自己的记录；Hub 的 qodercn 来自桥接镜像"
        } else if ours == hub {
            "同名"
        } else {
            "Token Monitor 的别名"
        };
        let _ = writeln!(out, "| {ours} | {hub} | {note} |");
    }
    for hub in &rec.hub_only_clients {
        let _ = writeln!(out, "| （本机没有） | {hub} | 只有 Hub 有 |");
    }
    if !hub_has_qoder_mirror && rec.mapping.contains_key(collector_core::QODER_CLIENT) {
        let _ = writeln!(out, "\nHub 在这段时间没有 qodercn 数据。");
    }

    let _ = writeln!(out, "\n## 汇总（一 client 一行）\n");
    let _ = writeln!(
        out,
        "| client | 精度 | 天数 | 差异 <1% 的天数 | 最大差异日 | 最大差异 | 合计（我们，按 Hub 口径） | 合计（我们，原口径） | 合计（Hub） | 合计差异 % | 结论 |"
    );
    let _ = writeln!(out, "|---|---|---:|---:|---|---:|---:|---:|---:|---:|---|");
    for s in &rec.summaries {
        let (worst_day, worst) = match &s.worst {
            Some((date, diff, p)) => (
                date.to_string(),
                format!("{} / {}", signed(*diff), format_percent(*p)),
            ),
            None => ("—".to_owned(), "—".to_owned()),
        };
        let verdict = match s.precision {
            ClientPrecision::Measured if s.within == s.days => "达标",
            ClientPrecision::Measured => "有超标日，见下方",
            ClientPrecision::HubOnly => "本机没有",
            _ => "不套用 1% 标准",
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {worst_day} | {worst} | {} | {} | {} | {} | {verdict} |",
            s.client,
            s.precision.label(),
            s.days,
            s.within,
            group(s.ours_total),
            group(s.ours_native_total),
            group(s.hub_total),
            format_percent(s.total_percent()),
        );
    }

    let measured: BTreeSet<&str> = rec
        .summaries
        .iter()
        .filter(|s| s.precision == ClientPrecision::Measured)
        .map(|s| s.client.as_str())
        .collect();
    let hub_absent: BTreeSet<&str> = rec
        .summaries
        .iter()
        .filter(|s| {
            rec.rows
                .iter()
                .filter(|row| row.client == s.client)
                .all(|row| row.hub.is_none())
        })
        .map(|s| s.client.as_str())
        .collect();
    let absent_block = |out: &mut String, s: &ClientSummary| {
        let _ = writeln!(
            out,
            "### {}：Hub 在整个范围内都没有这个 client\n\n\
             - 本机有 {} 天数据，合计 {}（按 Hub 口径）\n\
             - 可能原因：Token Monitor 没有采集这个 client，或这台电脑这段时间没有同步到 Hub\n",
            s.client,
            s.days,
            group(s.ours_total)
        );
    };
    let _ = writeln!(out, "\n## 差异 ≥ 1% 的 measured client × 日\n");
    let flagged: Vec<&Row> = rec
        .rows
        .iter()
        .filter(|row| {
            !row.within()
                && measured.contains(row.client.as_str())
                && !hub_absent.contains(row.client.as_str())
        })
        .collect();
    let absent_measured: Vec<&ClientSummary> = rec
        .summaries
        .iter()
        .filter(|s| measured.contains(s.client.as_str()) && hub_absent.contains(s.client.as_str()))
        .collect();
    if flagged.is_empty() && absent_measured.is_empty() {
        let _ = writeln!(out, "没有。");
    }
    for s in absent_measured {
        absent_block(&mut out, s);
    }
    for row in flagged {
        render_flagged(&mut out, row);
    }

    let others: Vec<&Row> = rec
        .rows
        .iter()
        .filter(|row| {
            !row.within()
                && !measured.contains(row.client.as_str())
                && !hub_absent.contains(row.client.as_str())
                && row.client != HUB_QODER_CLIENT
        })
        .collect();
    let absent_others: Vec<&ClientSummary> = rec
        .summaries
        .iter()
        .filter(|s| {
            !measured.contains(s.client.as_str())
                && hub_absent.contains(s.client.as_str())
                && s.client != HUB_QODER_CLIENT
        })
        .collect();
    if !others.is_empty() || !absent_others.is_empty() {
        let _ = writeln!(out, "\n## 其他 client（非 measured）差异 ≥ 1% 的日子\n");
        for s in absent_others {
            absent_block(&mut out, s);
        }
        for row in others {
            render_flagged(&mut out, row);
        }
    }

    let _ = writeln!(out, "\n## 逐日明细\n");
    for s in &rec.summaries {
        let _ = writeln!(out, "### {}\n", s.client);
        let _ = writeln!(
            out,
            "| 日期 | 我们（原口径） | 我们（按 Hub 口径） | Hub | 差值 | 差异 % | 消息 我们 / Hub |"
        );
        let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|");
        for row in rec.rows.iter().filter(|row| row.client == s.client) {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} / {} |",
                row.date,
                row.ours
                    .as_ref()
                    .map_or("—".to_owned(), |c| group(c.native_total)),
                row.ours
                    .as_ref()
                    .map_or("—".to_owned(), |c| group(c.hub_view_total)),
                row.hub.as_ref().map_or("—".to_owned(), |c| group(c.tokens)),
                signed(row.diff),
                format_percent(row.percent),
                row.ours
                    .as_ref()
                    .map_or("—".to_owned(), |c| group(c.messages)),
                row.hub
                    .as_ref()
                    .map_or("—".to_owned(), |c| group(c.messages)),
            );
        }
        let _ = writeln!(out);
    }

    render_qoder(&mut out, qoder);
    out
}

fn render_flagged(out: &mut String, row: &Row) {
    let _ = writeln!(out, "### {} {}\n", row.client, row.date);
    let _ = writeln!(
        out,
        "- 我们（按 Hub 口径）{}，Hub {}，差值 {}，差异 {}",
        group(row.ours_total()),
        group(row.hub_total()),
        signed(row.diff),
        format_percent(row.percent)
    );
    if let (
        Some(ours),
        Some(Components {
            input,
            output,
            cache_read,
            cache_write,
        }),
    ) = (&row.ours, row.hub.as_ref().and_then(|hub| hub.components))
    {
        let c = ours.components;
        let _ = writeln!(
            out,
            "- 分项（我们 / Hub）：input {} / {}，output {} / {}，cache read {} / {}，cache write {} / {}",
            group(c.input),
            group(input),
            group(c.output),
            group(output),
            group(c.cache_read),
            group(cache_read),
            group(c.cache_write),
            group(cache_write)
        );
    }
    if row.causes.is_empty() {
        let _ = writeln!(out, "- 原因：未归因（差值 {}）", signed(row.diff));
    } else {
        for cause in &row.causes {
            let _ = writeln!(out, "- 可能原因：{cause}");
        }
    }
    for clue in &row.clues {
        let _ = writeln!(out, "- 线索：{clue}");
    }
    let _ = writeln!(out);
}

fn render_qoder(out: &mut String, qoder: &QoderSection) {
    let _ = writeln!(out, "## Qoder\n");
    let _ = writeln!(
        out,
        "- 我们的 qoder-intl 是 credits 折算的估算值（estimated），不套用 1% 标准。"
    );
    let _ = writeln!(
        out,
        "- 「没有 usage 的回复」= 本机 Qoder 记录里没有 usage 字段的助手回复条数；我们不为它们生成事件，只统计条数，不读内容。"
    );
    let _ = writeln!(
        out,
        "- 「粗估」= 每天的没有 usage 条数 × 当天 Hub 平均每条消息的 token，用来估计下游按字数估算的那部分大约有多少。"
    );
    if qoder.days.is_empty() {
        let _ = writeln!(out, "\n这段时间两边都没有 Qoder 数据。");
        return;
    }
    let _ = writeln!(out, "\n### 按桥接脚本改动（2026-11-10 20:02）分段\n");
    let _ = writeln!(
        out,
        "| 时段 | 天数 | 我们 | Hub | 差值 | 差异 % | 我们的事件 | Hub 消息 | 没有 usage 的回复 | 粗估 | 粗估占差值 |"
    );
    let _ = writeln!(
        out,
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    );
    for p in &qoder.periods {
        let diff = p.ours - p.hub;
        let share = if diff < 0 && p.estimate > 0 {
            format!("{:.0}%", p.estimate as f64 * 100.0 / (-diff) as f64)
        } else {
            "—".to_owned()
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {share} |",
            p.label,
            p.days,
            group(p.ours),
            group(p.hub),
            signed(diff),
            format_percent(percent(diff, p.hub)),
            group(p.our_events),
            group(p.hub_messages),
            p.without_usage,
            group(p.estimate),
        );
    }
    if let Some(((before_tokens, before_events), (after_tokens, after_events))) =
        qoder.change_day_split
    {
        let _ = writeln!(
            out,
            "\n改动当天我们这边：20:02 前 {} token / {} 个事件，20:02 后 {} token / {} 个事件。Hub 只有按天的数，无法在当天内拆开。",
            group(before_tokens),
            before_events,
            group(after_tokens),
            after_events
        );
    }
    let _ = writeln!(out, "\n### 逐日\n");
    let _ = writeln!(
        out,
        "| 日期 | 我们 | Hub | 差值 | 差异 % | 我们的事件 | Hub 消息 | 没有 usage 的回复 |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|");
    for day in &qoder.days {
        let hub = day.hub.unwrap_or(0);
        let diff = day.ours - hub;
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            day.date,
            group(day.ours),
            day.hub.map_or("—".to_owned(), group),
            signed(diff),
            format_percent(percent(diff, hub)),
            group(day.our_events),
            group(day.hub_messages),
            day.without_usage
        );
    }
}

#[cfg(test)]
mod tests;
