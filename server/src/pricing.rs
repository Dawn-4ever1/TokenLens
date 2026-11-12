//! API-equivalent cost estimates (D042).
//!
//! Prices come from tokscale-core's `PricingService`, the engine Token
//! Monitor uses: LiteLLM, OpenRouter and models.dev catalogs plus this
//! project's `config/pricing-overrides.json`. Each event is priced the way
//! tokscale prices a parsed message, by rebuilding that message from the
//! event's model, provider, billing channel and service tier.
//!
//! The catalogs live in the server's own tokscale directory
//! (`TOKSCALE_CONFIG_DIR`, set by `configure_dir` before any thread starts);
//! the server never reads or writes another tokscale install's files.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use tokscale_core::pricing::PricingService;
use tokscale_core::sessions::{BillingIdentity, BillingIdentityOrigin, SharedStr};
use tokscale_core::{TokenBreakdown, UnifiedMessage};
use usage_event::UsageEvent;

/// Curated prices that win over the public catalogs; tokscale's
/// custom-pricing format. Every entry names its source and check date.
pub const OVERRIDES_JSON: &str = include_str!("../../config/pricing-overrides.json");
/// The locked tokscale-core revision; part of the price version because a
/// new engine can price the same catalog differently.
pub const TOKSCALE_REV: &str = "e9cf1041af784bdaa4e217978f89f2168e731b69";
pub const CATALOG_FILES: [&str; 3] = [
    "pricing-litellm.json",
    "pricing-openrouter.json",
    "pricing-models-dev.json",
];
const CUSTOM_PRICING_FILE: &str = "custom-pricing.json";
/// Catalogs older than this are downloaded again.
pub const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// Wait after a failed download before trying again.
pub const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceStatus {
    /// The source log carried the provider's own cost; used as is.
    Reported,
    /// Every token bucket has a published price.
    Priced,
    /// Some token buckets lack a price (for example cache writes); the known
    /// part is counted and the dashboard warns that the cost is low.
    Partial,
    /// The model's published price is zero.
    Free,
    /// No price found; never shown as $0.
    Unpriced,
    /// Qoder credits; shown as credits, never converted to dollars.
    Credits,
}

impl PriceStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Priced => "priced",
            Self::Partial => "partial",
            Self::Free => "free",
            Self::Unpriced => "unpriced",
            Self::Credits => "credits",
        }
    }
}

/// The event values pricing reads; borrowed from an event or a stored row.
#[derive(Debug, Clone, Copy)]
pub struct PricingInput<'a> {
    pub client: &'a str,
    pub model_id: &'a str,
    pub provider_id: Option<&'a str>,
    pub pricing_model_id: Option<&'a str>,
    pub billing_provider: Option<&'a str>,
    pub service_tier: Option<&'a str>,
    pub message_count: i64,
    pub tokens: [u64; 6],
    pub cost_usd: Option<f64>,
    pub credits: Option<f64>,
}

impl<'a> From<&'a UsageEvent> for PricingInput<'a> {
    fn from(event: &'a UsageEvent) -> Self {
        Self {
            client: &event.client,
            model_id: &event.model_id,
            provider_id: event.provider_id.as_deref(),
            pricing_model_id: event.pricing_model_id.as_deref(),
            billing_provider: event.billing_provider.as_deref(),
            service_tier: event.service_tier.as_deref(),
            message_count: event.message_count,
            tokens: [
                event.input,
                event.output,
                event.cache_read,
                event.cache_write,
                event.cache_write_1h,
                event.reasoning,
            ],
            cost_usd: event.cost_usd,
            credits: event.credits,
        }
    }
}

/// What gets stored next to an event.
#[derive(Debug, Clone, PartialEq)]
pub struct Priced {
    pub est_cost_usd: Option<f64>,
    /// `None` until a catalog has been loaded.
    pub status: Option<PriceStatus>,
    pub version: Option<String>,
}

pub struct Pricer {
    service: PricingService,
    version: String,
    as_of_ms: i64,
}

impl Pricer {
    pub fn new(service: PricingService, version: impl Into<String>, as_of_ms: i64) -> Self {
        Self {
            service,
            version: version.into(),
            as_of_ms,
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// When the newest catalog was downloaded, UTC milliseconds.
    pub const fn as_of_ms(&self) -> i64 {
        self.as_of_ms
    }

    /// The upstream message tokscale would have priced for this event.
    ///
    /// A `cn:` model (ZCode, or Codex through a China-region channel) is
    /// priced as the same model without the prefix: the catalogs have no
    /// `cn:` rows, and fuzzy matching otherwise lands on unrelated ones such
    /// as batch or coding-plan variants.
    fn message(input: &PricingInput<'_>) -> UnifiedMessage {
        let signed = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        let [
            prompt,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
            reasoning,
        ] = input.tokens;
        let tokens = TokenBreakdown {
            input: signed(prompt),
            output: signed(output),
            cache_read: signed(cache_read),
            cache_write: signed(cache_write),
            cache_write_1h: signed(cache_write_1h),
            reasoning: signed(reasoning),
        };
        let model_id = strip_region_prefix(input.model_id);
        let mut message = UnifiedMessage::new(
            input.client,
            model_id,
            input.provider_id.unwrap_or(""),
            "",
            0,
            tokens,
            0.0,
        );
        message.pricing_model_id = input.pricing_model_id.map(SharedStr::from);
        message.billing_identity = BillingIdentity::from_explicit(
            input.billing_provider,
            BillingIdentityOrigin::ExplicitRecord,
        );
        message.service_tier = input.service_tier.map(SharedStr::from);
        message.message_count = i32::try_from(input.message_count.max(0)).unwrap_or(i32::MAX);
        message
    }

    /// tokscale's own explanation of a price: matched catalog row, missing
    /// buckets, safety gaps. For `pricing report` only.
    pub fn explain(&self, input: &PricingInput<'_>) -> String {
        let explanation = self.service.explain_message(&Self::message(input));
        format!(
            "matched {} via {}, missing {:?}, gap {:?}",
            explanation.matched_model_id.as_deref().unwrap_or("-"),
            explanation.price_source.as_deref().unwrap_or("-"),
            explanation.uncovered_buckets,
            explanation.submission_safety_gap,
        )
    }

    /// Prices an event that has neither a reported cost nor credits.
    ///
    /// tokscale's `coverage` also fails when every bucket has a rate but the
    /// row's provider cannot be verified (a relay such as sub2api) or the
    /// catalogs disagree; that matters for its leaderboard, not for an
    /// API-equivalent estimate. Only buckets without any rate make `Partial`.
    fn estimate(&self, input: &PricingInput<'_>) -> (Option<f64>, PriceStatus) {
        let message = Self::message(input);
        let estimate = self.service.estimate_message(&message);
        let cost = estimate.cost;
        if !cost.is_finite() || cost < 0.0 || estimate.selected.is_none() {
            return (None, PriceStatus::Unpriced);
        }
        match (estimate.coverage, cost > 0.0) {
            (true, true) => (Some(cost), PriceStatus::Priced),
            (true, false) => (Some(0.0), PriceStatus::Free),
            (false, true) => {
                let missing = self.service.explain_message(&message).uncovered_buckets;
                if missing.is_empty() {
                    (Some(cost), PriceStatus::Priced)
                } else {
                    (Some(cost), PriceStatus::Partial)
                }
            }
            // A zero without a trusted row (a coding-plan rate, say) is not
            // an API price.
            (false, false) => (None, PriceStatus::Unpriced),
        }
    }
}

fn strip_region_prefix(model_id: &str) -> &str {
    match model_id.get(..3) {
        Some(prefix) if prefix.eq_ignore_ascii_case("cn:") && model_id.len() > 3 => &model_id[3..],
        _ => model_id,
    }
}

/// Reported costs and Qoder credits are classified without a catalog; every
/// other event waits for one.
pub fn price(pricer: Option<&Pricer>, input: &PricingInput<'_>) -> Priced {
    let version = pricer.map(|pricer| pricer.version.clone());
    if input.cost_usd.is_some() {
        return Priced {
            est_cost_usd: None,
            status: Some(PriceStatus::Reported),
            version,
        };
    }
    if input.credits.is_some() {
        return Priced {
            est_cost_usd: None,
            status: Some(PriceStatus::Credits),
            version,
        };
    }
    let Some(pricer) = pricer else {
        return Priced {
            est_cost_usd: None,
            status: None,
            version: None,
        };
    };
    let (est_cost_usd, status) = pricer.estimate(input);
    Priced {
        est_cost_usd,
        status: Some(status),
        version,
    }
}

// ---------------------------------------------------------------- catalogs --

/// Points tokscale at the server's own directory.
///
/// # Safety
/// Changes the process environment, so it must run before any other thread
/// exists (the tokio runtime included).
pub unsafe fn configure_dir(dir: &Path) {
    // SAFETY: the caller guarantees the process is still single-threaded.
    unsafe { std::env::set_var("TOKSCALE_CONFIG_DIR", dir) };
}

/// `PRICING_DIR`, else `tokscale/` next to the database.
pub fn default_dir(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("tokscale")
}

fn catalog_paths(dir: &Path) -> [PathBuf; 3] {
    CATALOG_FILES.map(|name| dir.join("cache").join(name))
}

/// Writes the curated overrides where tokscale reads custom pricing.
pub fn write_overrides(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("cache"))?;
    let path = dir.join(CUSTOM_PRICING_FILE);
    if std::fs::read_to_string(&path).is_ok_and(|current| current == OVERRIDES_JSON) {
        return Ok(());
    }
    let temporary = dir.join(format!("{CUSTOM_PRICING_FILE}.tmp"));
    std::fs::write(&temporary, OVERRIDES_JSON)?;
    std::fs::rename(&temporary, &path)
}

fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Newest catalog download, UTC milliseconds; `None` when none exists.
pub fn newest_catalog_ms(dir: &Path) -> Option<i64> {
    catalog_paths(dir)
        .iter()
        .filter_map(|path| modified_ms(path))
        .max()
}

fn refresh_after_ms() -> i64 {
    i64::try_from(REFRESH_AFTER.as_millis()).unwrap_or(i64::MAX)
}

/// True when no catalog exists or the newest one is a day old. A source that
/// keeps failing does not cause hourly downloads of the other two.
pub fn catalogs_stale(dir: &Path, now_ms: i64) -> bool {
    newest_catalog_ms(dir).is_none_or(|newest| now_ms.saturating_sub(newest) >= refresh_after_ms())
}

/// Content hash of the catalogs, the overrides and the engine revision.
pub fn catalog_version(dir: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(TOKSCALE_REV.as_bytes());
    hasher.update([0]);
    hasher.update(OVERRIDES_JSON.as_bytes());
    for path in catalog_paths(dir) {
        hasher.update([0]);
        match std::fs::read(&path) {
            Ok(bytes) => hasher.update(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update(b"-"),
            Err(error) => return Err(error),
        }
    }
    Ok(format!("{:x}", hasher.finalize())[..16].to_owned())
}

/// Builds a pricer from the downloaded catalogs unless `current` already is
/// that version; `None` also until at least one catalog exists.
/// `configure_dir(dir)` must have run first.
pub fn load(dir: &Path, current: Option<&str>) -> std::io::Result<Option<Pricer>> {
    let Some(as_of_ms) = newest_catalog_ms(dir) else {
        return Ok(None);
    };
    write_overrides(dir)?;
    let version = catalog_version(dir)?;
    if current == Some(version.as_str()) {
        return Ok(None);
    }
    Ok(
        PricingService::load_cached_any_age()
            .map(|service| Pricer::new(service, version, as_of_ms)),
    )
}

/// Downloads the three catalogs; a failed source keeps its old file.
pub async fn refresh() -> Result<(), String> {
    let report = PricingService::refresh().await?;
    let failed = report
        .sources
        .iter()
        .filter_map(|source| source.error.as_ref().map(|error| (source.source, error)))
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(());
    }
    Err(failed
        .into_iter()
        .map(|(source, error)| format!("{source}: {error}"))
        .collect::<Vec<_>>()
        .join("; "))
}

// ------------------------------------------------------------- maintenance --

/// Keeps prices current while the server runs: loads the cached catalogs,
/// re-prices stored events, downloads new catalogs once a day when
/// `download` is on, and re-prices again whenever the version changes.
pub async fn maintain(state: crate::http::AppState, dir: PathBuf, download: bool) {
    loop {
        sync(&state, &dir).await;
        let mut wait = REFRESH_AFTER;
        if download && catalogs_stale(&dir, state.now_ms()) {
            let before = newest_catalog_ms(&dir);
            match refresh().await {
                Ok(()) => tracing::info!("price catalogs downloaded"),
                Err(error) => {
                    tracing::warn!(%error, "price catalog download failed; keeping cached prices");
                }
            }
            if newest_catalog_ms(&dir) > before {
                sync(&state, &dir).await;
            } else {
                wait = RETRY_AFTER;
            }
        }
        if wait == REFRESH_AFTER && download {
            let due = newest_catalog_ms(&dir)
                .map(|newest| newest.saturating_add(refresh_after_ms()) - state.now_ms())
                .unwrap_or(0);
            let due = Duration::from_millis(u64::try_from(due).unwrap_or(0));
            wait = due.clamp(RETRY_AFTER, REFRESH_AFTER);
        }
        tokio::time::sleep(wait).await;
    }
}

async fn sync(state: &crate::http::AppState, dir: &Path) {
    let current = state.pricer().map(|pricer| pricer.version().to_owned());
    let path = dir.to_path_buf();
    match tokio::task::spawn_blocking(move || load(&path, current.as_deref())).await {
        Ok(Ok(Some(pricer))) => {
            tracing::info!(version = pricer.version(), "price catalog loaded");
            state.set_pricer(pricer);
        }
        Ok(Ok(None)) => {
            if state.pricer().is_none() {
                tracing::info!("no price catalog yet; costs stay unpriced until one downloads");
            }
        }
        Ok(Err(error)) => tracing::warn!(%error, "price catalog could not be read"),
        Err(_) => tracing::warn!("price catalog loading stopped unexpectedly"),
    }
    match state.reprice().await {
        Ok(0) => {}
        Ok(events) => tracing::info!(events, "events re-priced"),
        Err(error) => tracing::warn!(%error, "re-pricing failed; it will be retried"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use tokscale_core::pricing::ModelPricing;
    use tokscale_core::pricing::custom::CustomPricing;

    use super::*;

    /// $3 / $15 per million for input / output, $0.30 cache read, $3.75
    /// cache write, $6 for one-hour cache writes.
    pub(crate) fn synthetic_pricer() -> Pricer {
        let per_million = |value: f64| Some(value / 1_000_000.0);
        let mut models = HashMap::new();
        models.insert(
            "synthetic-model".to_owned(),
            ModelPricing {
                input_cost_per_token: per_million(3.0),
                output_cost_per_token: per_million(15.0),
                cache_read_input_token_cost: per_million(0.3),
                cache_creation_input_token_cost: per_million(3.75),
                cache_creation_input_token_cost_above_1hr: per_million(6.0),
                ..ModelPricing::default()
            },
        );
        models.insert(
            "synthetic-free".to_owned(),
            ModelPricing {
                input_cost_per_token: Some(0.0),
                output_cost_per_token: Some(0.0),
                cache_read_input_token_cost: Some(0.0),
                cache_creation_input_token_cost: Some(0.0),
                ..ModelPricing::default()
            },
        );
        models.insert(
            "synthetic-no-cache".to_owned(),
            ModelPricing {
                input_cost_per_token: per_million(1.0),
                output_cost_per_token: per_million(2.0),
                ..ModelPricing::default()
            },
        );
        let service = PricingService::new_with_custom(
            CustomPricing::from_models(models),
            HashMap::new(),
            HashMap::new(),
        );
        Pricer::new(service, "synthetic-v1", 1_700_000_000_000)
    }

    fn input<'a>(model_id: &'a str, tokens: [u64; 6]) -> PricingInput<'a> {
        PricingInput {
            client: "synthetic-client",
            model_id,
            provider_id: None,
            pricing_model_id: None,
            billing_provider: None,
            service_tier: None,
            message_count: 1,
            tokens,
            cost_usd: None,
            credits: None,
        }
    }

    fn close(left: f64, right: f64) -> bool {
        (left - right).abs() < 1e-9
    }

    #[test]
    fn prices_every_bucket_with_its_own_rate() {
        let pricer = synthetic_pricer();
        // 1M input, 1M output, 1M cache read, 1M cache write of which 0.5M
        // are one-hour writes.
        let priced = price(
            Some(&pricer),
            &input(
                "synthetic-model",
                [1_000_000, 1_000_000, 1_000_000, 1_000_000, 500_000, 0],
            ),
        );
        assert_eq!(priced.status, Some(PriceStatus::Priced));
        assert_eq!(priced.version.as_deref(), Some("synthetic-v1"));
        let cost = priced.est_cost_usd.expect("cost");
        assert!(
            close(cost, 3.0 + 15.0 + 0.3 + 0.5 * 3.75 + 0.5 * 6.0),
            "{cost}"
        );
    }

    #[test]
    fn free_partial_and_unpriced_are_distinct() {
        let pricer = synthetic_pricer();
        let free = price(
            Some(&pricer),
            &input("synthetic-free", [1_000, 1_000, 0, 0, 0, 0]),
        );
        assert_eq!(free.status, Some(PriceStatus::Free));
        assert_eq!(free.est_cost_usd, Some(0.0));

        let partial = price(
            Some(&pricer),
            &input("synthetic-no-cache", [1_000_000, 0, 1_000_000, 0, 0, 0]),
        );
        assert_eq!(partial.status, Some(PriceStatus::Partial));
        assert!(close(partial.est_cost_usd.expect("known part"), 1.0));

        let unknown = price(
            Some(&pricer),
            &input("never-heard-of-it", [1_000, 1_000, 0, 0, 0, 0]),
        );
        assert_eq!(unknown.status, Some(PriceStatus::Unpriced));
        assert_eq!(unknown.est_cost_usd, None);
    }

    #[test]
    fn reported_cost_and_credits_are_never_repriced() {
        let pricer = synthetic_pricer();
        let mut reported = input("synthetic-model", [1_000_000, 0, 0, 0, 0, 0]);
        reported.cost_usd = Some(0.5);
        let priced = price(Some(&pricer), &reported);
        assert_eq!(priced.status, Some(PriceStatus::Reported));
        assert_eq!(priced.est_cost_usd, None);

        let mut qoder = input("synthetic-model", [1_000_000, 0, 0, 0, 0, 0]);
        qoder.client = "qoder-intl";
        qoder.credits = Some(2.0);
        let priced = price(Some(&pricer), &qoder);
        assert_eq!(priced.status, Some(PriceStatus::Credits));
        assert_eq!(priced.est_cost_usd, None);
        // Both are settled even before any catalog loads.
        assert_eq!(price(None, &qoder).status, Some(PriceStatus::Credits));
        assert_eq!(
            price(None, &input("synthetic-model", [1, 1, 0, 0, 0, 0])),
            Priced {
                est_cost_usd: None,
                status: None,
                version: None
            }
        );
    }

    #[test]
    fn openai_priority_tier_doubles_the_price() {
        let mut models = HashMap::new();
        models.insert(
            "gpt-synthetic".to_owned(),
            ModelPricing {
                input_cost_per_token: Some(1.0 / 1_000_000.0),
                output_cost_per_token: Some(2.0 / 1_000_000.0),
                cache_read_input_token_cost: Some(0.1 / 1_000_000.0),
                ..ModelPricing::default()
            },
        );
        let pricer = Pricer::new(
            PricingService::new_with_custom(
                CustomPricing::from_models(models),
                HashMap::new(),
                HashMap::new(),
            ),
            "v",
            0,
        );
        let mut standard = input("gpt-synthetic", [1_000_000, 0, 0, 0, 0, 0]);
        standard.provider_id = Some("openai");
        let mut priority = standard;
        priority.service_tier = Some("priority");
        let base = price(Some(&pricer), &standard).est_cost_usd.expect("base");
        let fast = price(Some(&pricer), &priority).est_cost_usd.expect("fast");
        assert!(close(base, 1.0), "{base}");
        assert!(close(fast, 2.0), "{fast}");
    }

    #[test]
    fn region_prefixed_models_price_as_the_base_model() {
        let pricer = synthetic_pricer();
        let tokens = [1_000_000, 0, 0, 0, 0, 0];
        let base = price(Some(&pricer), &input("synthetic-model", tokens));
        let prefixed = price(Some(&pricer), &input("cn:synthetic-model", tokens));
        assert_eq!(prefixed, base);
        assert_eq!(strip_region_prefix("CN:x"), "x");
        assert_eq!(strip_region_prefix("cn:"), "cn:");
        assert_eq!(strip_region_prefix("cnx"), "cnx");
        assert_eq!(strip_region_prefix("模型"), "模型");
    }

    #[test]
    fn curated_overrides_parse_without_diagnostics() {
        let custom =
            CustomPricing::load_from_str(OVERRIDES_JSON, Path::new("pricing-overrides.json"));
        assert!(
            custom.diagnostics().is_empty(),
            "{:?}",
            custom.diagnostics()
        );
    }

    #[test]
    fn locked_revision_matches_the_workspace() {
        assert!(include_str!("../../Cargo.toml").contains(TOKSCALE_REV));
    }

    #[test]
    fn version_tracks_catalog_content_and_staleness_tracks_age() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(newest_catalog_ms(dir.path()), None);
        assert!(catalogs_stale(dir.path(), 0));
        let empty = catalog_version(dir.path()).expect("version");
        write_overrides(dir.path()).expect("overrides");
        for name in CATALOG_FILES {
            std::fs::write(dir.path().join("cache").join(name), b"{}").expect("catalog");
        }
        let filled = catalog_version(dir.path()).expect("version");
        assert_ne!(empty, filled);
        assert_eq!(filled, catalog_version(dir.path()).expect("same content"));
        let newest = newest_catalog_ms(dir.path()).expect("newest");
        assert!(!catalogs_stale(dir.path(), newest));
        assert!(catalogs_stale(
            dir.path(),
            newest + refresh_after_ms() + 1_000
        ));
        std::fs::write(
            dir.path().join("cache").join(CATALOG_FILES[1]),
            b"{\"x\":1}",
        )
        .expect("changed");
        assert_ne!(filled, catalog_version(dir.path()).expect("version"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CUSTOM_PRICING_FILE)).expect("custom"),
            OVERRIDES_JSON
        );
    }

    #[test]
    fn default_dir_sits_next_to_the_database() {
        assert_eq!(
            default_dir(Path::new("/app/data/tokenlens.sqlite")),
            PathBuf::from("/app/data/tokscale")
        );
        assert_eq!(
            default_dir(Path::new("db.sqlite")),
            PathBuf::from("./tokscale")
        );
    }
}
