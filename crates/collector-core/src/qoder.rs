//! Read-only Qoder International credits adapter.
//!
//! Estimated rows reproduce the existing bridge: normalized three-way mix,
//! independent half-away-from-zero rounding, and no cache-write allocation.
//! UsageEvent input excludes cache reads; the bridge's inclusive input is
//! normalized when its explicit `input_tokens_include_cache` marker is present.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, NaiveDateTime};
use serde::Deserialize;
use serde_json::{Map, Value};
use usage_event::{Precision, UsageEvent, qoder_event_key};

const RATES_JSON: &str = include_str!("../../../config/qoder-credit-rates.json");
const CLIENT_ID: &str = "qoder-intl";
const DEFAULT_MODEL: &str = "auto";

#[derive(Debug, thiserror::Error)]
pub enum QoderError {
    #[error("Qoder CN mirror paths are forbidden; use only the international source")]
    ForbiddenMirrorPath,
    #[error("Qoder source could not be read; no source details are printed for privacy")]
    Read(#[from] std::io::Error),
    #[error("Qoder credit rates are invalid")]
    InvalidRates,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rates {
    #[serde(alias = "rate_version")]
    rate_version: String,
    source: String,
    default_tokens_per_credit: f64,
    default_mix: PartialMix,
    models: BTreeMap<String, ModelRate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelRate {
    tokens_per_credit: f64,
    mix: PartialMix,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PartialMix {
    input: Option<f64>,
    cache_read: Option<f64>,
    output: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Mix {
    input: f64,
    cache_read: f64,
    output: f64,
}

impl PartialMix {
    fn normalized(&self) -> Result<Mix, QoderError> {
        let mix = Mix {
            input: self.input.unwrap_or(0.09),
            cache_read: self.cache_read.unwrap_or(0.90),
            output: self.output.unwrap_or(0.01),
        };
        let total = mix.input + mix.cache_read + mix.output;
        if [mix.input, mix.cache_read, mix.output]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
            || total <= 0.0
        {
            return Err(QoderError::InvalidRates);
        }
        Ok(Mix {
            input: mix.input / total,
            cache_read: mix.cache_read / total,
            output: mix.output / total,
        })
    }
}

impl Rates {
    fn load() -> Result<Self, QoderError> {
        let rates: Self = serde_json::from_str(RATES_JSON).map_err(|_| QoderError::InvalidRates)?;
        if rates.rate_version.trim().is_empty()
            || rates.source.trim().is_empty()
            || !rates.default_tokens_per_credit.is_finite()
            || rates.default_tokens_per_credit <= 0.0
            || rates
                .models
                .values()
                .any(|entry| !entry.tokens_per_credit.is_finite() || entry.tokens_per_credit <= 0.0)
        {
            return Err(QoderError::InvalidRates);
        }
        rates.default_mix.normalized()?;
        for entry in rates.models.values() {
            entry.mix.normalized()?;
        }
        Ok(rates)
    }

    fn for_model(&self, model: &str) -> Result<(f64, Mix), QoderError> {
        let raw = model.trim().to_ascii_lowercase();
        if !raw.is_empty() {
            let mut variants = vec![raw.clone()];
            if let Some(stripped) = raw.strip_prefix("quest-") {
                variants.push(stripped.to_owned());
            } else {
                variants.push(format!("quest-{raw}"));
            }
            // Keep the bridge's alias order: prefixed forms are tried before
            // these aliases (ultimate, auto -> quest-ultimate, quest-auto).
            if raw == "ultimate" {
                variants.push("quest-ultimate".to_owned());
            } else if raw == "auto" {
                variants.push("quest-auto".to_owned());
            }
            for variant in variants {
                if let Some(entry) = self.models.get(&variant) {
                    return Ok((entry.tokens_per_credit, entry.mix.normalized()?));
                }
            }
        }
        Ok((
            self.default_tokens_per_credit,
            self.default_mix.normalized()?,
        ))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QoderScanStats {
    pub usage_records: u64,
    pub invalid_records: u64,
    pub zero_usage_records: u64,
}

#[derive(Debug)]
pub struct QoderOutput {
    pub events: Vec<UsageEvent>,
    pub stats: QoderScanStats,
    /// Timestamps (ms) of in-range assistant replies that carry no usage block.
    /// No event is made for them; downstream tools may estimate them from text.
    pub assistant_without_usage: Vec<i64>,
}

#[derive(Debug)]
struct RawRecord {
    value: Value,
    fallback_timestamp: i64,
    fallback_session_id: String,
    file_agent_id: Option<String>,
    is_subagent_file: bool,
}

#[derive(Default)]
struct FileContext {
    session_id: Option<String>,
    pending_turn_start: bool,
}

/// The embedded credit-rate table version, attached to uploads.
pub fn rate_version() -> Option<String> {
    Rates::load().ok().map(|rates| rates.rate_version)
}

/// Resolve through `dirs`; on Windows this uses the platform home directory,
/// including USERPROFILE. Actual Windows discovery is a P2 smoke-test item.
pub fn projects_root(home: &Path) -> PathBuf {
    home.join(".qoder").join("projects")
}

pub fn scan_default(range: crate::DateRange) -> Result<QoderOutput, QoderError> {
    let home = dirs::home_dir().ok_or_else(|| {
        QoderError::Read(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "home directory unavailable",
        ))
    })?;
    scan_home(&home, range)
}

pub fn scan_home(home: &Path, range: crate::DateRange) -> Result<QoderOutput, QoderError> {
    let root = projects_root(home);
    assert_international_path(&root)?;
    if !root.is_dir() {
        return Ok(QoderOutput {
            events: Vec::new(),
            stats: QoderScanStats::default(),
            assistant_without_usage: Vec::new(),
        });
    }
    scan_root(&root, range)
}

/// Check every component before any filesystem operation on the path.
pub fn assert_international_path(path: &Path) -> Result<(), QoderError> {
    if path
        .components()
        .any(|component| matches!(component, Component::Normal(part) if part == ".qoder-cn"))
    {
        Err(QoderError::ForbiddenMirrorPath)
    } else {
        Ok(())
    }
}

pub fn scan_root(root: &Path, range: crate::DateRange) -> Result<QoderOutput, QoderError> {
    assert_international_path(root)?;
    let mut paths = Vec::new();
    collect_jsonl(root, &mut paths)?;
    paths.sort();
    let rates = Rates::load()?;
    let mut events = Vec::<UsageEvent>::new();
    let mut stats = QoderScanStats::default();
    let mut without_usage = Vec::<(Option<(String, String)>, i64)>::new();
    let mut pending = Vec::<PendingTurn>::new();
    let mut ordinals = BTreeMap::<(String, i64, String), u64>::new();
    let mut turn_indices = HashMap::<(String, String), usize>::new();

    for path in paths {
        let mut context = FileContext::default();
        for record in read_records(&path)? {
            match transform_record(&record, &mut context) {
                Ok(Some(fragment)) => {
                    stats.usage_records += 1;
                    let event = &fragment.event;
                    let ordinal = if fragment.turn_key.is_none() {
                        // Preserve the old fallback parse-order contract for
                        // unidentifiable records: filter before ordinals.
                        if !in_range(event.ts, range) {
                            continue;
                        }
                        let group = (event.session_id.clone(), event.ts, event.model_id.clone());
                        let ordinal = ordinals.entry(group).or_insert(0);
                        let current = *ordinal;
                        *ordinal = ordinal.saturating_add(1);
                        current
                    } else {
                        0
                    };
                    let event_key = qoder_event_key(
                        &event.session_id,
                        fragment.turn_key.as_deref(),
                        event.ts,
                        &event.model_id,
                        ordinal,
                    );
                    let fragment = PendingTurn {
                        event: UsageEvent {
                            event_key,
                            ..fragment.event
                        },
                        ..fragment
                    };
                    if let Some(turn_key) = fragment.turn_key.as_ref() {
                        let group = (fragment.event.session_id.clone(), turn_key.clone());
                        if let Some(index) = turn_indices.get(&group).copied() {
                            merge_fragment(&mut pending[index], fragment);
                            continue;
                        }
                        turn_indices.insert(group, pending.len());
                    }
                    pending.push(fragment);
                }
                Ok(None) => {
                    stats.zero_usage_records += 1;
                    if let Some(ts) = assistant_reply_without_usage(&record)
                        && in_range(ts, range)
                    {
                        let root = context
                            .session_id
                            .as_deref()
                            .unwrap_or(&record.fallback_session_id);
                        let group =
                            turn_key(&record.value).map(|key| (session_ids(&record, root).0, key));
                        without_usage.push((group, ts));
                    }
                }
                Err(()) => stats.invalid_records += 1,
            }
        }
    }
    let mut measured_turns = HashSet::new();
    for fragment in pending {
        let record_count = fragment.record_count;
        let group = fragment
            .turn_key
            .clone()
            .map(|key| (fragment.event.session_id.clone(), key));
        match fragment.into_event(&rates) {
            Ok(Some(event)) => {
                if let Some(group) = group {
                    measured_turns.insert(group);
                }
                if in_range(event.ts, range) {
                    events.push(event);
                }
            }
            Ok(None) => stats.zero_usage_records += record_count,
            Err(()) => stats.invalid_records += record_count,
        }
    }
    // A reply split over several lines counts once, and not at all when
    // another line of the same message produced an event.
    let mut seen = HashSet::new();
    let mut assistant_without_usage: Vec<i64> = without_usage
        .into_iter()
        .filter(|(group, _)| {
            group
                .as_ref()
                .is_none_or(|group| !measured_turns.contains(group) && seen.insert(group.clone()))
        })
        .map(|(_, ts)| ts)
        .collect();
    assistant_without_usage.sort_unstable();
    Ok(QoderOutput {
        events,
        stats,
        assistant_without_usage,
    })
}

/// Only structure is inspected: the reply text is never read.
fn assistant_reply_without_usage(record: &RawRecord) -> Option<i64> {
    let value = &record.value;
    let kind = value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str);
    let api_error = value
        .get("isApiErrorMessage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let message = value.get("message").and_then(Value::as_object)?;
    if kind != Some("assistant") || api_error || message.get("usage").is_some_and(Value::is_object)
    {
        return None;
    }
    Some(timestamp(value.get("timestamp")).unwrap_or(record.fallback_timestamp))
}

fn collect_jsonl(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), QoderError> {
    assert_international_path(directory)?;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        assert_international_path(&path)?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_jsonl(&path, paths)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn read_records(path: &Path) -> Result<Vec<RawRecord>, QoderError> {
    assert_international_path(path)?;
    let fallback_timestamp = std::fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);
    let file_agent_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("agent-"))
        .map(str::to_owned);
    let fallback_session_id = fallback_session_id(path);
    let is_subagent_file = path
        .ancestors()
        .any(|part| part.file_name().is_some_and(|name| name == "subagents"));
    let mut records = Vec::new();
    for line in BufReader::new(std::fs::File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(RawRecord {
            value: serde_json::from_str(&line).unwrap_or(Value::Null),
            fallback_timestamp,
            fallback_session_id: fallback_session_id.clone(),
            file_agent_id: file_agent_id.clone(),
            is_subagent_file,
        });
    }
    Ok(records)
}

fn fallback_session_id(path: &Path) -> String {
    if let Some(session_directory) = path
        .ancestors()
        .find(|part| part.file_name().is_some_and(|name| name == "subagents"))
        .and_then(Path::parent)
        && let Some(name) = session_directory.file_name().and_then(|name| name.to_str())
    {
        return name.to_owned();
    }
    path.file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session")
        .to_owned()
}

fn in_range(timestamp: i64, range: crate::DateRange) -> bool {
    let Some(date) = DateTime::from_timestamp_millis(timestamp)
        .map(|time| time.with_timezone(&chrono::Local).date_naive())
    else {
        return false;
    };
    range.since.is_none_or(|since| date >= since) && range.until.is_none_or(|until| date <= until)
}

fn turn_key(value: &Value) -> Option<String> {
    let message = value.get("message")?.as_object()?;
    if let Some(id) = text(message.get("id")) {
        return Some(format!("msg:{id}"));
    }
    if let Some(id) = text(value.get("requestId"))
        .or_else(|| text(value.get("request_id")))
        .or_else(|| {
            message
                .get("usage")
                .and_then(Value::as_object)
                .and_then(|usage| text(usage.get("request_id")))
        })
    {
        return Some(format!("req:{id}"));
    }
    text(value.get("uuid")).map(|uuid| format!("uuid:{uuid}"))
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn update_context(value: &Value, context: &mut FileContext) {
    if context.session_id.is_none() {
        context.session_id =
            text(value.get("sessionId").or_else(|| value.get("session_id"))).map(str::to_owned);
    }
    let kind = value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str);
    if kind == Some("user") {
        let synthetic = value
            .get("isSynthetic")
            .or_else(|| value.get("isMeta"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !synthetic {
            context.pending_turn_start = true;
        }
    }
}

fn number(usage: &Map<String, Value>, keys: &[&str]) -> Result<f64, ()> {
    Ok(optional_number(usage, keys)?.unwrap_or(0.0))
}

fn optional_number(usage: &Map<String, Value>, keys: &[&str]) -> Result<Option<f64>, ()> {
    let Some(value) = keys.iter().find_map(|key| usage.get(*key)) else {
        return Ok(None);
    };
    let number = value.as_f64().ok_or(())?;
    if !number.is_finite() || number < 0.0 {
        return Err(());
    }
    Ok(Some(number))
}

fn integer(usage: &Map<String, Value>, keys: &[&str]) -> Result<u64, ()> {
    let value = number(usage, keys)?;
    if value > i64::MAX as f64 {
        return Err(());
    }
    // Some Qoder releases serialize integral token counters as `1.0`.
    // Keep measured counters nonnegative; round only this representation,
    // never reinterpret credits as measured tokens.
    rounded_tokens(value)
}

fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(text) = value.as_str() {
        if let Ok(time) = DateTime::parse_from_rfc3339(text) {
            return Some(time.timestamp_millis());
        }
        for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
            if let Ok(time) = NaiveDateTime::parse_from_str(text, format) {
                return Some(time.and_utc().timestamp_millis());
            }
        }
        return text.parse::<i64>().ok().and_then(timestamp_number);
    }
    value.as_i64().and_then(timestamp_number)
}

fn timestamp_number(value: i64) -> Option<i64> {
    if value <= 0 {
        return None;
    }
    if value >= 1_000_000_000_000 {
        Some(value)
    } else {
        Some(value.saturating_mul(1000))
    }
}

fn rounded_tokens(value: f64) -> Result<u64, ()> {
    if !value.is_finite() || value < 0.0 || value.round() > i64::MAX as f64 {
        return Err(());
    }
    // Rust f64::round and Python round on these nonnegative values both use
    // half-away-from-zero; each bucket is rounded independently.
    Ok(value.round() as u64)
}

#[derive(Default)]
struct RawUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_1h: u64,
    reasoning: u64,
    credits: Option<f64>,
}

impl RawUsage {
    fn merge(&mut self, fragment: Self) {
        // Every counter is the maximum progress of one API call, never a sum.
        self.input = self.input.max(fragment.input);
        self.output = self.output.max(fragment.output);
        self.cache_read = self.cache_read.max(fragment.cache_read);
        self.cache_write = self.cache_write.max(fragment.cache_write);
        self.cache_write_1h = self.cache_write_1h.max(fragment.cache_write_1h);
        self.reasoning = self.reasoning.max(fragment.reasoning);
        self.credits = match (self.credits, fragment.credits) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        };
    }
}

struct PendingTurn {
    event: UsageEvent,
    turn_key: Option<String>,
    raw: RawUsage,
    inclusive_input: bool,
    latest_timestamp: i64,
    record_count: u64,
}

impl PendingTurn {
    fn into_event(mut self, rates: &Rates) -> Result<Option<UsageEvent>, ()> {
        self.event.credits = self.raw.credits;
        let has_real_tokens = self.raw.input > 0 || self.raw.output > 0 || self.raw.cache_read > 0;
        if has_real_tokens {
            self.event.input = if self.inclusive_input {
                self.raw.input.saturating_sub(self.raw.cache_read)
            } else {
                self.raw.input
            };
            self.event.output = self.raw.output;
            self.event.cache_read = self.raw.cache_read;
            self.event.cache_write = self.raw.cache_write;
            self.event.cache_write_1h = self.raw.cache_write_1h.min(self.raw.cache_write);
            self.event.reasoning = self.raw.reasoning;
            self.event.precision = Precision::Measured;
        } else if self.raw.credits.unwrap_or(0.0) > 0.0 {
            // Merge raw counters first; apply the credit rate exactly once.
            let (rate, mix) = rates.for_model(&self.event.model_id).map_err(|_| ())?;
            let total = self.raw.credits.unwrap_or(0.0) * rate;
            self.event.input = rounded_tokens(total * mix.input)?;
            self.event.output = rounded_tokens(total * mix.output)?;
            self.event.cache_read = rounded_tokens(total * mix.cache_read)?;
            self.event.precision = Precision::Estimated;
            self.event.rate_version = Some(rates.rate_version.clone());
        } else {
            return Ok(None);
        }
        self.event
            .to_json_value()
            .map(|_| Some(self.event))
            .map_err(|_| ())
    }
}

fn session_ids(record: &RawRecord, root_session: &str) -> (String, Option<String>) {
    let value = &record.value;
    let sidechain = record.is_subagent_file
        || value
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if sidechain {
        let agent_id = text(value.get("agentId"))
            .map(str::to_owned)
            .or_else(|| record.file_agent_id.clone())
            .unwrap_or_else(|| "qoder-subagent".to_owned());
        (
            format!("{root_session}:agent:{agent_id}"),
            Some(root_session.to_owned()),
        )
    } else {
        (root_session.to_owned(), None)
    }
}

fn transform_record(
    record: &RawRecord,
    context: &mut FileContext,
) -> Result<Option<PendingTurn>, ()> {
    let value = &record.value;
    if value.is_null() {
        return Err(());
    }
    update_context(value, context);
    let kind = value
        .get("type")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str);
    if kind != Some("assistant")
        || value
            .get("isApiErrorMessage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Ok(None);
    }
    let Some(message) = value.get("message").and_then(Value::as_object) else {
        return Err(());
    };
    let Some(usage) = message.get("usage").and_then(Value::as_object) else {
        return Ok(None);
    };
    let model = text(message.get("model").or_else(|| value.get("model")))
        .filter(|model| *model != "<synthetic>")
        .unwrap_or(DEFAULT_MODEL);
    let root_session = context
        .session_id
        .as_deref()
        .unwrap_or(&record.fallback_session_id);
    let (session_id, parent_session_id) = session_ids(record, root_session);
    let ts = timestamp(value.get("timestamp")).unwrap_or(record.fallback_timestamp);
    if ts < 0 || session_id.is_empty() || model.is_empty() {
        return Err(());
    }

    let raw = RawUsage {
        input: integer(usage, &["input_tokens"])?,
        output: integer(usage, &["output_tokens"])?,
        cache_read: integer(usage, &["cache_read_input_tokens"])?,
        cache_write: integer(usage, &["cache_creation_input_tokens"])?,
        cache_write_1h: usage
            .get("cache_creation")
            .and_then(Value::as_object)
            .map(|creation| integer(creation, &["ephemeral_1h_input_tokens"]))
            .transpose()?
            .unwrap_or(0),
        reasoning: integer(
            usage,
            &["thinking_tokens", "reasoning_tokens", "reasoningTokens"],
        )?,
        credits: optional_number(usage, &["credits"])?,
    };
    let inclusive_input = usage
        .get("input_tokens_include_cache")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_turn_start = std::mem::take(&mut context.pending_turn_start);
    Ok(Some(PendingTurn {
        event: UsageEvent {
            event_key: String::new(),
            client: CLIENT_ID.to_owned(),
            session_id,
            parent_session_id,
            model_id: model.to_owned(),
            provider_id: Some("qoder".to_owned()),
            ts,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            cost_usd: None,
            cost_source: "unknown".to_owned(),
            precision: Precision::Unknown,
            credits: None,
            rate_version: None,
            project_hash: None,
            project_label: None,
            message_count: 1,
            is_turn_start,
            duration_ms: None,
            pricing_model_id: None,
            billing_provider: None,
            service_tier: None,
        },
        turn_key: turn_key(value),
        raw,
        inclusive_input,
        latest_timestamp: ts,
        record_count: 1,
    }))
}

fn merge_fragment(existing: &mut PendingTurn, fragment: PendingTurn) {
    existing.raw.merge(fragment.raw);
    let latest = existing.latest_timestamp.max(fragment.latest_timestamp);
    existing.event.ts = existing.event.ts.min(fragment.event.ts);
    existing.latest_timestamp = latest;
    existing.event.duration_ms = Some(latest.saturating_sub(existing.event.ts));
    existing.event.is_turn_start |= fragment.event.is_turn_start;
    existing.inclusive_input |= fragment.inclusive_input;
    existing.record_count = existing.record_count.saturating_add(fragment.record_count);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_events() -> Vec<UsageEvent> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/qoder");
        scan_root(&root, crate::DateRange::all_history())
            .expect("synthetic fixture")
            .events
    }

    #[test]
    fn hand_calculated_credit_conversion_matches_all_buckets_and_total() {
        let temp = tempfile::tempdir().expect("fixture root");
        let path = temp.path().join("synthetic.jsonl");
        let mut file = std::fs::File::create(&path).expect("fixture file");
        writeln!(
            file,
            "{}",
            include_str!("../../../fixtures/qoder/session-main.jsonl")
                .lines()
                .nth(1)
                .unwrap()
        )
        .unwrap();
        let output = scan_root(temp.path(), crate::DateRange::all_history()).expect("scan");
        let event = &output.events[0];
        // 1 credit * 43,700 = 43,700. The source mix sums to 1:
        // input 2,417.01 -> 2,417; cache 40,988.415 -> 40,988;
        // output 294.975 -> 295. Independently rounded sum = 43,700.
        assert_eq!(
            (event.input, event.cache_read, event.output),
            (2417, 40988, 295)
        );
        assert_eq!(event.input + event.cache_read + event.output, 43700);
        assert_eq!((event.cache_write, event.reasoning), (0, 0));
        assert_eq!(event.precision, Precision::Estimated);
        assert_eq!(event.credits, Some(1.0));
        assert_eq!(event.rate_version.as_deref(), Some("bridge-2026-11-10"));
    }

    #[test]
    fn mixed_credit_and_real_usage_have_the_correct_precision() {
        let events = fixture_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].precision, Precision::Estimated);
        assert_eq!(events[1].precision, Precision::Measured);
        assert_eq!((events[1].input, events[1].cache_read), (0, 800));
        assert_eq!((events[1].cache_write, events[1].cache_write_1h), (50, 20));
        assert_eq!(events[1].reasoning, 10);
        assert_eq!(events[1].credits, Some(1.0));
    }

    #[test]
    fn forbidden_mirror_path_errors_before_reading() {
        let path = Path::new("/synthetic").join(".qoder-cn").join("projects");
        assert!(matches!(
            assert_international_path(&path),
            Err(QoderError::ForbiddenMirrorPath)
        ));
        assert!(matches!(
            scan_root(&path, crate::DateRange::all_history()),
            Err(QoderError::ForbiddenMirrorPath)
        ));
    }

    #[test]
    fn scanning_twice_has_identical_event_keys() {
        let keys = |events: Vec<UsageEvent>| {
            events
                .into_iter()
                .map(|event| event.event_key)
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(fixture_events()), keys(fixture_events()));
    }

    #[test]
    fn serialized_events_keep_credit_fields_and_only_allowlisted_fields() {
        for event in fixture_events() {
            let value = event.to_json_value().expect("valid event");
            let object = value.as_object().expect("object");
            assert!(
                object
                    .keys()
                    .all(|key| usage_event::UPLOAD_FIELDS.contains(&key.as_str()))
            );
            assert!(object.contains_key("credits"));
            assert_eq!(
                object.contains_key("rateVersion"),
                event.precision == Precision::Estimated
            );
            for forbidden in ["content", "text", "cwd", "uuid", "requestId", "usage"] {
                assert!(!object.contains_key(forbidden));
            }
        }
    }

    fn turn_fixture_events() -> Vec<UsageEvent> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/qoder-turns");
        scan_root(&root, crate::DateRange::all_history())
            .expect("synthetic turn fixture")
            .events
    }

    #[test]
    fn message_fragments_merge_credits_before_converting_once() {
        let events = turn_fixture_events();
        assert_eq!(events.len(), 8);
        let event = &events[0];
        assert_eq!(event.precision, Precision::Estimated);
        assert_eq!(event.credits, Some(1.25));
        assert_eq!(event.ts, 1_793_577_601_000);
        assert_eq!(event.duration_ms, Some(3000));
        // 1.25 * 43,700, with each bucket independently rounded.
        assert_eq!(
            (event.input, event.output, event.cache_read),
            (3021, 369, 51236)
        );
        assert_eq!(event.input + event.output + event.cache_read, 54626);
        assert_eq!(
            event.event_key,
            qoder_event_key(
                "synthetic-turn-session",
                Some("msg:synthetic-credit-message"),
                event.ts,
                "gmodel",
                0
            )
        );
    }

    #[test]
    fn message_fragments_merge_raw_measured_values_and_keep_credits() {
        let event = &turn_fixture_events()[1];
        assert_eq!(event.precision, Precision::Measured);
        assert_eq!(
            (
                event.input,
                event.output,
                event.cache_read,
                event.cache_write,
                event.cache_write_1h,
                event.reasoning
            ),
            (20, 6, 14, 50, 20, 9)
        );
        assert_eq!(event.credits, Some(1.25));
        assert_eq!(event.rate_version, None);
        event.to_json_value().expect("measured credits are valid");
    }

    #[test]
    fn distinct_message_request_and_uuid_keys_have_expected_counts() {
        let events = turn_fixture_events();
        assert_ne!(events[2].event_key, events[3].event_key);
        assert_eq!(
            (events[2].credits, events[3].credits),
            (Some(0.5), Some(0.5))
        );
        assert_eq!(events[4].credits, Some(0.25));
        assert_eq!(events[4].duration_ms, Some(1000));
        assert_eq!(
            events[4].event_key,
            qoder_event_key(
                "synthetic-turn-session",
                Some("req:synthetic-request-id"),
                events[4].ts,
                "gmodel",
                0
            )
        );
        assert_eq!(events[5].credits, Some(0.2));
        assert_eq!(events[5].duration_ms, Some(2000));
        assert_eq!(
            events[5].event_key,
            qoder_event_key(
                "synthetic-turn-session",
                Some("uuid:synthetic-uuid-only"),
                events[5].ts,
                "gmodel",
                0
            )
        );
        assert_eq!(
            events[6].event_key,
            usage_event::fallback_event_key(
                "qoder-intl",
                "synthetic-turn-session",
                events[6].ts,
                "gmodel",
                0
            )
        );
        assert_eq!(events[7].precision, Precision::Measured);
        assert_eq!(events[7].credits, None);
        assert_eq!(events[7].rate_version, None);
    }

    #[test]
    fn request_id_sources_follow_the_specified_priority() {
        let with_message_id = serde_json::json!({
            "message": {"id": "synthetic-message"},
            "requestId": "synthetic-request",
            "request_id": "synthetic-snake",
            "uuid": "synthetic-uuid"
        });
        assert_eq!(
            turn_key(&with_message_id).as_deref(),
            Some("msg:synthetic-message")
        );
        let request_id = serde_json::json!({
            "requestId": "synthetic-request",
            "request_id": "synthetic-snake",
            "message": {"usage": {"request_id": "synthetic-usage"}},
            "uuid": "synthetic-uuid"
        });
        assert_eq!(
            turn_key(&request_id).as_deref(),
            Some("req:synthetic-request")
        );
        let snake = serde_json::json!({
            "request_id": "synthetic-snake",
            "message": {"usage": {"request_id": "synthetic-usage"}}
        });
        assert_eq!(turn_key(&snake).as_deref(), Some("req:synthetic-snake"));
        let nested = serde_json::json!({
            "message": {"usage": {"request_id": "synthetic-usage"}},
            "uuid": "synthetic-uuid"
        });
        assert_eq!(turn_key(&nested).as_deref(), Some("req:synthetic-usage"));
        let uuid_only = serde_json::json!({"message": {}, "uuid": "synthetic-uuid"});
        assert_eq!(turn_key(&uuid_only).as_deref(), Some("uuid:synthetic-uuid"));
    }

    #[test]
    fn fractional_measured_usage_from_message_fragments_is_merged() {
        let temp = tempfile::tempdir().expect("root");
        let first = concat!(
            "{\"type\":\"assistant\",\"uuid\":\"synthetic-fragment-1\",",
            "\"sessionId\":\"synthetic-session\",",
            "\"timestamp\":\"2026-11-01T00:00:01Z\",\"message\":{",
            "\"id\":\"synthetic-api-message\",\"model\":\"gmodel\",",
            "\"usage\":{\"input_tokens\":10.0,\"output_tokens\":2.0,",
            "\"cache_read_input_tokens\":0,\"credits\":0}}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"synthetic-fragment-2\",",
            "\"sessionId\":\"synthetic-session\",",
            "\"timestamp\":\"2026-11-01T00:00:03Z\",\"message\":{",
            "\"id\":\"synthetic-api-message\",\"model\":\"gmodel\",",
            "\"usage\":{\"input_tokens\":20.0,\"output_tokens\":5.0,",
            "\"cache_read_input_tokens\":0,\"credits\":0}}}\n"
        );
        std::fs::write(temp.path().join("synthetic.jsonl"), first).expect("fixture");
        let events = scan_root(temp.path(), crate::DateRange::all_history())
            .expect("scan")
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].precision, Precision::Measured);
        assert_eq!((events[0].input, events[0].output), (20, 5));
        assert_eq!(events[0].duration_ms, Some(2000));
        assert_eq!(events[0].rate_version, None);
        assert_eq!(
            events[0].event_key,
            usage_event::qoder_event_key(
                "synthetic-session",
                Some("msg:synthetic-api-message"),
                events[0].ts,
                "gmodel",
                0
            )
        );
    }

    #[test]
    fn missing_uuid_uses_stable_fallback_key() {
        let temp = tempfile::tempdir().expect("root");
        std::fs::write(
            temp.path().join("synthetic-session.jsonl"),
            concat!(
                "{\"type\":\"assistant\",\"sessionId\":\"synthetic-session\",",
                "\"timestamp\":\"2026-11-01T00:00:01Z\",\"message\":{",
                "\"model\":\"gmodel\",\"usage\":{\"input_tokens\":0,",
                "\"output_tokens\":0,\"cache_read_input_tokens\":0,",
                "\"credits\":1}}}\n"
            ),
        )
        .expect("fixture");
        let first = scan_root(temp.path(), crate::DateRange::all_history()).expect("first");
        let second = scan_root(temp.path(), crate::DateRange::all_history()).expect("second");
        assert_eq!(first.events[0].event_key, second.events[0].event_key);
        assert_eq!(
            first.events[0].event_key,
            usage_event::fallback_event_key(
                "qoder-intl",
                "synthetic-session",
                1_793_491_201_000,
                "gmodel",
                0
            )
        );
    }

    #[test]
    fn model_aliases_default_and_normalization_match_the_bridge() {
        let rates = Rates::load().expect("rates");
        assert_eq!(rates.for_model("ULTIMATE").unwrap().0, 10800.0);
        assert_eq!(rates.for_model("quest-gmodel").unwrap().0, 43700.0);
        assert_eq!(rates.for_model("unknown").unwrap().0, 20000.0);
        let mix = rates.for_model("gmodel").unwrap().1;
        assert!(((mix.input + mix.cache_read + mix.output) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn session_fallback_subagent_and_record_timestamp_are_stable() {
        let temp = tempfile::tempdir().expect("root");
        let directory = temp.path().join("synthetic-session/subagents");
        std::fs::create_dir_all(&directory).expect("directory");
        std::fs::write(
            directory.join("agent-synthetic.jsonl"),
            concat!(
                "{\"type\":\"assistant\",\"uuid\":\"synthetic-sub-1\",",
                "\"timestamp\":1762509600000,\"message\":{\"model\":\"gmodel\",",
                "\"usage\":{\"input_tokens\":0,\"output_tokens\":0,",
                "\"cache_read_input_tokens\":0,\"credits\":0.01}}}\n"
            ),
        )
        .expect("synthetic subagent");
        let events = scan_root(temp.path(), crate::DateRange::all_history())
            .expect("scan")
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, "synthetic-session:agent:synthetic");
        assert_eq!(
            events[0].parent_session_id.as_deref(),
            Some("synthetic-session")
        );
        assert_eq!(events[0].ts, 1_762_509_600_000);
    }

    #[test]
    fn assistant_replies_without_usage_are_listed_by_timestamp_only() {
        let temp = tempfile::tempdir().expect("root");
        std::fs::write(
            temp.path().join("synthetic.jsonl"),
            concat!(
                "{\"type\":\"user\",\"sessionId\":\"s\",\"timestamp\":1767225600000,",
                "\"message\":{\"role\":\"user\"}}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225601000,",
                "\"message\":{\"id\":\"m1\",\"model\":\"gmodel\"}}\n",
                "{\"type\":\"assistant\",\"isApiErrorMessage\":true,\"sessionId\":\"s\",",
                "\"timestamp\":1767225602000,\"message\":{\"id\":\"m2\"}}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225603000,",
                "\"message\":{\"id\":\"m3\",\"model\":\"gmodel\",\"usage\":{\"credits\":0.01}}}\n",
                // A text fragment of m3 (which has usage) and two fragments of m4.
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225604000,",
                "\"message\":{\"id\":\"m3\",\"model\":\"gmodel\"}}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225605000,",
                "\"message\":{\"id\":\"m4\",\"model\":\"gmodel\"}}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225606000,",
                "\"message\":{\"id\":\"m4\",\"model\":\"gmodel\"}}\n",
                // m5's usage line is all zeros, so it yields no event.
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225607000,",
                "\"message\":{\"id\":\"m5\",\"model\":\"gmodel\",\"usage\":{\"credits\":0}}}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":1767225608000,",
                "\"message\":{\"id\":\"m5\",\"model\":\"gmodel\"}}\n",
            ),
        )
        .expect("synthetic session");
        let output = scan_root(temp.path(), crate::DateRange::all_history()).expect("scan");
        assert_eq!(output.events.len(), 1);
        assert_eq!(
            output.assistant_without_usage,
            vec![1_767_225_601_000, 1_767_225_605_000, 1_767_225_608_000]
        );
        let before = crate::DateRange {
            since: None,
            until: chrono::NaiveDate::from_ymd_opt(2025, 12, 30),
        };
        let empty = scan_root(temp.path(), before).expect("scan");
        assert!(empty.assistant_without_usage.is_empty());
    }
}
