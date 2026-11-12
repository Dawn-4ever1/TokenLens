//! Read-only local client selection, event mapping, and token aggregation.

pub mod credentials;
pub mod interactive;
mod mapping;
pub mod qoder;
pub mod roots;
pub mod schedule;
pub mod snapshot;
pub mod state;
pub mod sync;
pub mod transport;
pub mod watch;
pub mod worker;
pub mod worker_cli;

/// The locked upstream `tokscale-core` commit (workspace `Cargo.toml`).
pub const TOKSCALE_REV: &str = "e9cf1041af784bdaa4e217978f89f2168e731b69";
/// Changes whenever parsing, mapping, Qoder handling or `event_key` rules
/// change; a new value makes every event queue for upload once more.
pub const PARSER_GENERATION: &str = "tokscale-e9cf1041.map-3.qoder-1.key-1";

pub use mapping::{
    EventKeySetDigest, MappingError, canonical_json, event_key_set_digest, event_value_digest,
    event_value_hash, map_message, map_messages, value_digest_of_pairs,
};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokscale_core::{
    ClientId, LocalParseOptions, UnifiedMessage, parse_local_unified_messages_with_pricing,
    parse_local_unified_messages_with_pricing_uncached,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot determine the current user's home directory")]
    HomeDirectory,
    #[error("configuration could not be read as UTF-8")]
    ConfigRead(#[from] std::io::Error),
    #[error("configuration is invalid: {0}")]
    Config(String),
    #[error("local parser failed: {0}")]
    Parser(String),
    #[error(
        "an event cannot be mapped without dropping required data or changing its meaning: {0:?}"
    )]
    EventMapping(MappingError),
    #[error(transparent)]
    Qoder(#[from] qoder::QoderError),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Clients {
    Auto(String),
    Explicit(Vec<String>),
}

impl Default for Clients {
    fn default() -> Self {
        Self::Auto("auto".to_owned())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub clients: Clients,
    pub exclude: Vec<String>,
}

impl Config {
    /// Missing files intentionally use defaults; other I/O errors are failures.
    pub fn load(path: &Path) -> Result<Self, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|error| Error::Config(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Error::ConfigRead(error)),
        }
    }

    fn selected(&self) -> Result<Vec<String>, Error> {
        let excluded: BTreeSet<String> = self.exclude.iter().map(|id| normalize_id(id)).collect();
        let selected = match &self.clients {
            Clients::Auto(mode) if mode == "auto" => ClientId::iter()
                .map(|client| client.as_str().to_owned())
                .collect(),
            Clients::Auto(_mode) => {
                return Err(Error::Config(
                    "clients must be \"auto\" or a list of client ids".to_owned(),
                ));
            }
            Clients::Explicit(clients) => {
                let mut validated = Vec::new();
                for id in clients {
                    let id = normalize_id(id);
                    if id == QODER_CLIENT {
                        // The international adapter is controlled by `exclude`.
                        continue;
                    }
                    let Some(client) = ClientId::from_str(&id) else {
                        return Err(Error::Config(format!("unknown client id: {id}")));
                    };
                    if !client.parse_local() {
                        return Err(Error::Config(format!(
                            "client does not support local parsing: {id}"
                        )));
                    }
                    if !validated.contains(&id) {
                        validated.push(id);
                    }
                }
                validated
            }
        };
        let selected: Vec<String> = selected
            .into_iter()
            .filter(|id| id != "qoder" && !excluded.contains(id))
            .collect();
        // Defense in depth: no upstream parser that reads the Qoder mirror is
        // ever allowed across this boundary, irrespective of the configuration.
        assert_safe_clients(&selected)?;
        // Upstream interprets an empty list as "all clients"; never let an
        // exclusion-only selection accidentally re-enable qoder.
        if selected.is_empty() {
            return Err(Error::Config(
                "no clients remain after auto-discovery and exclusions".to_owned(),
            ));
        }
        Ok(selected)
    }

    pub fn qoder_enabled(&self) -> bool {
        !self
            .exclude
            .iter()
            .any(|id| normalize_id(id) == QODER_CLIENT)
    }
}

fn normalize_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

pub fn default_config_path() -> Result<PathBuf, Error> {
    let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
    Ok(config_path_in(&home))
}

pub fn config_path_in(home: &Path) -> PathBuf {
    home.join(".config/tokenlens/collector.toml")
}

pub fn normalize_client_id(id: &str) -> String {
    normalize_id(id)
}

fn assert_safe_clients(clients: &[String]) -> Result<(), Error> {
    if clients.iter().any(|id| id == "qoder") {
        return Err(Error::Config(
            "upstream qoder is forbidden because it reads the Qoder CN mirror".to_owned(),
        ));
    }
    Ok(())
}

pub fn selected_clients(config: &Config) -> Result<Vec<String>, Error> {
    config.selected()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
}

impl DateRange {
    pub fn last_30_days() -> Self {
        let until = chrono::Local::now().date_naive();
        Self {
            since: Some(until - chrono::Duration::days(29)),
            until: Some(until),
        }
    }

    pub const fn all_history() -> Self {
        Self {
            since: None,
            until: None,
        }
    }

    pub fn validate(self) -> Result<Self, Error> {
        if self
            .since
            .zip(self.until)
            .is_some_and(|(since, until)| since > until)
        {
            return Err(Error::Config("since must not be after until".to_owned()));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_write_1h: i64,
    pub reasoning: i64,
    pub total: i64,
}

impl Tokens {
    fn add(&mut self, tokens: &tokscale_core::TokenBreakdown) {
        self.input = self.input.saturating_add(tokens.input);
        self.output = self.output.saturating_add(tokens.output);
        self.cache_read = self.cache_read.saturating_add(tokens.cache_read);
        self.cache_write = self.cache_write.saturating_add(tokens.cache_write);
        self.cache_write_1h = self.cache_write_1h.saturating_add(tokens.cache_write_1h);
        self.reasoning = self.reasoning.saturating_add(tokens.reasoning);
        // The upstream breakdown normalizes reasoning into a disjoint bucket;
        // cache_write_1h is a subset of cache_write and must not be added.
        self.total = self.total.saturating_add(tokens.total());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DailyUsage {
    pub client: String,
    pub date: String,
    pub messages: i64,
    #[serde(flatten)]
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub precision_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientUsage {
    pub client: String,
    pub messages: i64,
    #[serde(flatten)]
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub precision_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanReport {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub selected_clients: Vec<String>,
    pub scanned_clients: usize,
    pub detected_clients: Vec<String>,
    pub daily: Vec<DailyUsage>,
    pub client_totals: Vec<ClientUsage>,
    pub elapsed_ms: u64,
    pub event_digest: EventDigests,
    #[serde(skip)]
    pub events: Vec<usage_event::UsageEvent>,
    #[serde(skip)]
    pub upstream_cache_used: bool,
}

/// Counts and hashes only; no key or event value is ever serialized here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventDigests {
    pub event_count: usize,
    pub unique_event_keys: usize,
    pub key_set_sha256: String,
    pub value_sha256: String,
}

impl EventDigests {
    pub fn of(events: &[usage_event::UsageEvent]) -> Self {
        let keys = event_key_set_digest(events);
        Self {
            event_count: keys.event_count,
            unique_event_keys: keys.unique_event_keys,
            key_set_sha256: keys.sha256,
            value_sha256: event_value_digest(events),
        }
    }
}

/// This entry point disables upstream's persistent cache and supplies no
/// pricing service, so a dry-run does not write caches or fetch network data.
pub fn scan(config: &Config, range: DateRange) -> Result<ScanReport, Error> {
    scan_with(config, &ScanOptions::for_current_user(range)?)
}

pub const QODER_CLIENT: &str = "qoder-intl";

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub home: PathBuf,
    /// `false` ignores agent root overrides such as `CODEX_HOME`.
    pub use_env_roots: bool,
    /// `None` scans every selected client and the Qoder adapter.
    pub clients: Option<BTreeSet<String>>,
    pub range: DateRange,
    /// Use upstream's persistent source cache. Only workers whose parent set
    /// `TOKSCALE_CONFIG_DIR` to this project's own directory may enable it.
    pub upstream_cache: bool,
}

impl ScanOptions {
    pub fn for_current_user(range: DateRange) -> Result<Self, Error> {
        Ok(Self {
            home: dirs::home_dir().ok_or(Error::HomeDirectory)?,
            use_env_roots: true,
            clients: None,
            range,
            upstream_cache: false,
        })
    }
}

pub fn scan_with(config: &Config, options: &ScanOptions) -> Result<ScanReport, Error> {
    let range = options.range.validate()?;
    let selected = config.selected()?;
    let upstream = match &options.clients {
        None => selected,
        Some(subset) => selected
            .into_iter()
            .filter(|id| subset.contains(id))
            .collect(),
    };
    assert_safe_clients(&upstream)?;
    let include_qoder = config.qoder_enabled()
        && options
            .clients
            .as_ref()
            .is_none_or(|subset| subset.contains(QODER_CLIENT));
    let use_cache = options.upstream_cache && upstream_cache_is_exact(&upstream);
    let start = Instant::now();
    // An empty upstream list means "all clients" upstream, so skip the call.
    let messages = if upstream.is_empty() {
        Vec::new()
    } else {
        let parse_options = LocalParseOptions {
            home_dir: Some(options.home.to_string_lossy().into_owned()),
            use_env_roots: options.use_env_roots,
            clients: Some(upstream.clone()),
            since: range.since.map(|date| date.to_string()),
            until: range.until.map(|date| date.to_string()),
            year: None,
            scanner_settings: tokscale_core::scanner::ScannerSettings::default(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| Error::Parser(error.to_string()))?;
        let parsed = if use_cache {
            runtime.block_on(parse_local_unified_messages_with_pricing(
                parse_options,
                None,
            ))
        } else {
            runtime.block_on(parse_local_unified_messages_with_pricing_uncached(
                parse_options,
                None,
            ))
        };
        parsed.map_err(|_| {
            Error::Parser(
                "an agent could not be parsed; no source details are printed for privacy"
                    .to_owned(),
            )
        })?
    };
    let mut events = map_messages(&messages).map_err(Error::EventMapping)?;
    let scanned_clients = upstream.len();
    let mut listed = upstream;
    let mut combined = messages;
    if include_qoder {
        let qoder = qoder::scan_home(&options.home, range)?;
        combined.extend(qoder.events.iter().map(qoder_message_for_summary));
        events.extend(qoder.events);
        listed.push(QODER_CLIENT.to_owned());
    }
    let mut report = summarize(
        combined,
        listed,
        scanned_clients,
        range,
        start.elapsed(),
        events,
    );
    report.upstream_cache_used = use_cache && report.scanned_clients > 0;
    Ok(report)
}

/// Upstream's persistent cache keeps Claude turns that a transcript rewrite
/// removed, so only scans without Claude return exactly what is on disk.
pub const HISTORY_RETAINING_CLIENTS: &[&str] = &["claude"];

pub fn upstream_cache_is_exact(clients: &[String]) -> bool {
    !clients
        .iter()
        .any(|client| HISTORY_RETAINING_CLIENTS.contains(&client.as_str()))
}

fn qoder_message_for_summary(event: &usage_event::UsageEvent) -> UnifiedMessage {
    let mut message = UnifiedMessage::new_with_dedup(
        &event.client,
        &event.model_id,
        event.provider_id.as_deref().unwrap_or("qoder"),
        &event.session_id,
        event.ts,
        tokscale_core::TokenBreakdown {
            input: event.input as i64,
            output: event.output as i64,
            cache_read: event.cache_read as i64,
            cache_write: event.cache_write as i64,
            cache_write_1h: event.cache_write_1h as i64,
            reasoning: event.reasoning as i64,
        },
        0.0,
        None,
    );
    message.message_count = event.message_count as i32;
    message.is_turn_start = event.is_turn_start;
    message
}

pub fn summarize(
    messages: Vec<UnifiedMessage>,
    selected_clients: Vec<String>,
    scanned_clients: usize,
    range: DateRange,
    elapsed: Duration,
    events: Vec<usage_event::UsageEvent>,
) -> ScanReport {
    #[derive(Default)]
    struct Aggregate {
        messages: i64,
        tokens: Tokens,
        credits: f64,
        has_credits: bool,
        precision_counts: BTreeMap<String, u64>,
    }
    let mut daily: BTreeMap<(String, String), Aggregate> = BTreeMap::new();
    let mut totals: BTreeMap<String, Aggregate> = BTreeMap::new();
    let detected_clients = messages
        .iter()
        .filter(|message| message.message_count > 0)
        .map(|message| message.client.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for message in messages {
        // Dates come from the timestamp in the current device's local timezone;
        // no session id, title, workspace, or other source text is retained.
        let Some(date) = chrono::DateTime::from_timestamp_millis(message.timestamp)
            .map(|time| time.with_timezone(&chrono::Local).date_naive().to_string())
        else {
            continue;
        };
        let count = i64::from(message.message_count.max(0));
        let day = daily
            .entry((message.client.to_string(), date.clone()))
            .or_default();
        day.messages = day.messages.saturating_add(count);
        day.tokens.add(&message.tokens);
        let total = totals.entry(message.client.to_string()).or_default();
        total.messages = total.messages.saturating_add(count);
        total.tokens.add(&message.tokens);
    }
    for event in events.iter() {
        let Some(date) = chrono::DateTime::from_timestamp_millis(event.ts)
            .map(|time| time.with_timezone(&chrono::Local).date_naive().to_string())
        else {
            continue;
        };
        for aggregate in [
            daily.get_mut(&(event.client.clone(), date)),
            totals.get_mut(&event.client),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(credits) = event.credits {
                aggregate.credits += credits;
                aggregate.has_credits = true;
            }
            *aggregate
                .precision_counts
                .entry(event.precision.as_str().to_owned())
                .or_default() += 1;
        }
    }
    ScanReport {
        since: range.since,
        until: range.until,
        selected_clients,
        scanned_clients,
        detected_clients,
        daily: daily
            .into_iter()
            .map(|((client, date), aggregate)| DailyUsage {
                client,
                date,
                messages: aggregate.messages,
                tokens: aggregate.tokens,
                credits: aggregate.has_credits.then_some(aggregate.credits),
                precision_counts: aggregate.precision_counts,
            })
            .collect(),
        client_totals: totals
            .into_iter()
            .map(|(client, aggregate)| ClientUsage {
                client,
                messages: aggregate.messages,
                tokens: aggregate.tokens,
                credits: aggregate.has_credits.then_some(aggregate.credits),
                precision_counts: aggregate.precision_counts,
            })
            .collect(),
        elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
        upstream_cache_used: false,
        event_digest: EventDigests::of(&events),
        events,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventCount {
    pub client: String,
    pub count: usize,
    pub precision_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventSummary {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub scanned_clients: usize,
    pub detected_clients: Vec<String>,
    pub total: usize,
    pub by_client: Vec<EventCount>,
    /// One structural sample only: property names and JSON types, never values.
    pub example: serde_json::Value,
}

pub fn event_summary(report: &ScanReport) -> Result<EventSummary, Error> {
    let events = &report.events;
    let mut by_client = BTreeMap::<String, EventCount>::new();
    for event in events {
        let count = by_client
            .entry(event.client.clone())
            .or_insert_with(|| EventCount {
                client: event.client.clone(),
                count: 0,
                precision_counts: ["measured", "estimated", "unknown"]
                    .into_iter()
                    .map(|precision| (precision.to_owned(), 0))
                    .collect(),
            });
        count.count += 1;
        *count
            .precision_counts
            .entry(event.precision.as_str().to_owned())
            .or_default() += 1;
    }
    let schema: serde_json::Value = serde_json::from_str(usage_event::SCHEMA_JSON)
        .map_err(|_| Error::Parser("the built-in event schema is invalid".to_owned()))?;
    let example = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| Error::Parser("the built-in event schema has no properties".to_owned()))?
        .iter()
        .map(|(key, definition)| {
            let kind = match definition.get("type") {
                Some(serde_json::Value::String(kind)) => kind.clone(),
                Some(serde_json::Value::Array(kinds)) => kinds
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join("|"),
                _ => "unknown".to_owned(),
            };
            (key.clone(), serde_json::Value::String(kind.to_owned()))
        })
        .collect();
    Ok(EventSummary {
        since: report.since,
        until: report.until,
        scanned_clients: report.scanned_clients,
        detected_clients: report.detected_clients.clone(),
        total: events.len(),
        by_client: by_client.into_values().collect(),
        example,
    })
}

#[cfg(test)]
mod generation_tests {
    use super::*;

    #[test]
    fn parser_generation_follows_the_locked_upstream_rev() {
        let lock = include_str!("../../../Cargo.lock");
        assert!(lock.contains(&format!("rev={TOKSCALE_REV}#")));
        assert!(PARSER_GENERATION.contains(&TOKSCALE_REV[..8]));
        assert!(PARSER_GENERATION.len() <= 128);
        assert!(qoder::rate_version().is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_includes_zcode_without_a_legacy_projects_directory() {
        let clients = Config::default().selected().expect("selection should work");
        let expected = ClientId::iter()
            .map(|client| client.as_str())
            .filter(|id| *id != "qoder")
            .collect::<Vec<_>>();
        assert_eq!(clients, expected);
        assert!(clients.contains(&"zcode".to_owned()));
        assert!(!clients.contains(&"qoder".to_owned()));
    }

    #[test]
    fn explicit_qoder_is_removed_and_final_list_is_checked() {
        let config = Config {
            clients: Clients::Explicit(vec!["codex".into(), "qoder".into()]),
            exclude: vec![],
        };
        let clients = config.selected().expect("valid list");
        assert_eq!(clients, ["codex"]);
        assert!(assert_safe_clients(&["qoder".into()]).is_err());
    }

    #[test]
    fn exclude_applies_to_auto_and_explicit_selection() {
        let auto = Config {
            exclude: vec!["codex".into(), "qoder".into()],
            ..Config::default()
        };
        let clients = auto.selected().expect("auto");
        assert!(!clients.contains(&"codex".to_owned()));
        assert!(!clients.contains(&"qoder".to_owned()));
        assert!(clients.contains(&"zcode".to_owned()));
        let explicit = Config {
            clients: Clients::Explicit(vec!["codex".into(), "dsh".into()]),
            exclude: vec!["dsh".into()],
        };
        assert_eq!(explicit.selected().expect("explicit"), ["codex"]);
    }

    #[test]
    fn empty_selection_cannot_mean_all_upstream_clients() {
        let config = Config {
            clients: Clients::Explicit(vec!["qoder".into()]),
            exclude: vec![],
        };
        assert!(config.selected().is_err());
        let config = Config {
            clients: Clients::Explicit(vec!["codex".into()]),
            exclude: vec!["codex".into()],
        };
        assert!(config.selected().is_err());
        let config = Config {
            exclude: ClientId::iter()
                .map(|client| client.as_str().to_owned())
                .collect(),
            ..Config::default()
        };
        assert!(config.selected().is_err());
    }

    #[test]
    fn total_adds_disjoint_reasoning_but_not_the_cache_write_subset() {
        let tokens = tokscale_core::TokenBreakdown {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            cache_write_1h: 15,
            reasoning: 5,
        };
        let mut sum = Tokens::default();
        sum.add(&tokens);
        assert_eq!(sum.total, 105);
        assert_eq!(sum.cache_write_1h, 15);
    }

    #[test]
    fn defaults_are_used_when_configuration_is_absent() {
        let home = tempfile::tempdir().expect("temporary home");
        let config = Config::load(&home.path().join("absent.toml")).expect("default config");
        assert_eq!(config.clients, Clients::Auto("auto".into()));
        assert!(config.exclude.is_empty());
    }

    #[test]
    fn invalid_dates_are_rejected() {
        let range = DateRange {
            since: NaiveDate::from_ymd_opt(2026, 11, 10),
            until: NaiveDate::from_ymd_opt(2026, 11, 9),
        };
        assert!(range.validate().is_err());
    }

    #[test]
    fn qoder_summary_has_credits_and_precision_counts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/qoder");
        let events = qoder::scan_root(&root, DateRange::all_history())
            .expect("synthetic fixture")
            .events;
        let messages = events.iter().map(qoder_message_for_summary).collect();
        let report = summarize(
            messages,
            vec!["qoder-intl".into()],
            1,
            DateRange::all_history(),
            Duration::ZERO,
            events,
        );
        assert_eq!(report.client_totals[0].credits, Some(2.0));
        assert_eq!(report.client_totals[0].precision_counts["estimated"], 1);
        assert_eq!(report.client_totals[0].precision_counts["measured"], 1);
        assert_eq!(report.daily[0].tokens.total, 44560);
    }
}
