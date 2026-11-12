//! Shared UsageEvent v1 wire type, upload allowlist, and stable event keys.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod envelope;

pub const SCHEMA_JSON: &str = include_str!("../../../schema/usage-event.v1.json");

/// Every property permitted by architecture section 6.2.
pub const UPLOAD_FIELDS: [&str; 26] = [
    "eventKey",
    "client",
    "sessionId",
    "parentSessionId",
    "modelId",
    "providerId",
    "ts",
    "input",
    "output",
    "cacheRead",
    "cacheWrite",
    "cacheWrite1h",
    "reasoning",
    "costUsd",
    "costSource",
    "precision",
    "credits",
    "rateVersion",
    "projectHash",
    "projectLabel",
    "messageCount",
    "isTurnStart",
    "durationMs",
    "pricingModelId",
    "billingProvider",
    "serviceTier",
];

pub const REQUIRED_FIELDS: [&str; 15] = [
    "eventKey",
    "client",
    "sessionId",
    "modelId",
    "ts",
    "input",
    "output",
    "cacheRead",
    "cacheWrite",
    "cacheWrite1h",
    "reasoning",
    "costSource",
    "precision",
    "messageCount",
    "isTurnStart",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Precision {
    Measured,
    Estimated,
    Unknown,
}

impl Precision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }
}

/// This is a deliberate construction boundary, not an upstream object pass-through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageEvent {
    pub event_key: String,
    pub client: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub ts: i64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_write_1h: u64,
    pub reasoning: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Kept as text so the upstream CostSource representation is not reinterpreted.
    pub cost_source: String,
    pub precision: Precision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_label: Option<String>,
    pub message_count: i64,
    pub is_turn_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Pricing context only (D042): the upstream model name before display
    /// normalization, sent when it differs from `model_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_model_id: Option<String>,
    /// Pricing context only (D042): the billing channel the source log names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_provider: Option<String>,
    /// Pricing context only (D042): the provider processing tier, such as
    /// OpenAI `priority`, which changes the per-token price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Architecture 5.3's primary form, using lowercase hexadecimal SHA-256.
pub fn event_key_from_dedup(client: &str, dedup_key: &str) -> String {
    sha256_hex(format!("{client}:{dedup_key}").as_bytes())
}

/// Ordinal is the zero-based parse-order position within one
/// `(client, session_id, timestamp, model_id)` group, including messages with a dedup key.
pub fn fallback_event_key(
    client: &str,
    session_id: &str,
    timestamp: i64,
    model_id: &str,
    ordinal: u64,
) -> String {
    sha256_hex(format!("{client}:{session_id}:{timestamp}:{model_id}:{ordinal}").as_bytes())
}

pub fn event_key(client: &str, dedup_key: Option<&str>, fallback: FallbackKey<'_>) -> String {
    match dedup_key.filter(|key| !key.is_empty()) {
        Some(key) => event_key_from_dedup(client, key),
        None => fallback_event_key(
            client,
            fallback.session_id,
            fallback.timestamp,
            fallback.model_id,
            fallback.ordinal,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    NonFiniteNumber,
    NegativeNumber,
    InvalidCreditsRateVersion,
}

impl UsageEvent {
    pub fn validate(&self) -> Result<(), ValidationError> {
        for value in [self.cost_usd, self.credits].into_iter().flatten() {
            if !value.is_finite() {
                return Err(ValidationError::NonFiniteNumber);
            }
            if value < 0.0 {
                return Err(ValidationError::NegativeNumber);
            }
        }
        let invalid_credit_fields = (self.precision == Precision::Estimated
            && (self.credits.is_none()
                || self
                    .rate_version
                    .as_deref()
                    .is_none_or(|version| version.is_empty())))
            || (self.rate_version.is_some()
                && (self.credits.is_none() || self.precision != Precision::Estimated));
        if invalid_credit_fields {
            return Err(ValidationError::InvalidCreditsRateVersion);
        }
        Ok(())
    }

    pub fn to_json_value(&self) -> Result<serde_json::Value, ValidationError> {
        self.validate()?;
        serde_json::to_value(self).map_err(|_| ValidationError::NonFiniteNumber)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FallbackKey<'a> {
    pub session_id: &'a str,
    pub timestamp: i64,
    pub model_id: &'a str,
    pub ordinal: u64,
}

/// Qoder turn keys are prefixed as `msg:`, `req:`, or `uuid:` by the adapter.
/// A missing turn key retains the timestamp/model/ordinal fallback.
pub fn qoder_event_key(
    session_id: &str,
    turn_key: Option<&str>,
    timestamp: i64,
    model_id: &str,
    ordinal: u64,
) -> String {
    match turn_key.filter(|key| !key.is_empty()) {
        Some(turn_key) => sha256_hex(format!("qoder-intl:{session_id}:{turn_key}").as_bytes()),
        None => fallback_event_key("qoder-intl", session_id, timestamp, model_id, ordinal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_event() -> UsageEvent {
        UsageEvent {
            event_key: event_key_from_dedup("synthetic", "message-1"),
            client: "synthetic".into(),
            session_id: "synthetic-session".into(),
            parent_session_id: None,
            model_id: "synthetic-model".into(),
            provider_id: None,
            ts: 1_700_000_000_000,
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write_1h: 1,
            reasoning: 5,
            cost_usd: None,
            cost_source: "unknown".into(),
            precision: Precision::Unknown,
            credits: None,
            rate_version: None,
            project_hash: None,
            project_label: None,
            message_count: 1,
            is_turn_start: true,
            duration_ms: None,
            pricing_model_id: None,
            billing_provider: None,
            service_tier: None,
        }
    }

    #[test]
    fn primary_and_fallback_keys_match_fixed_vectors() {
        assert_eq!(
            event_key_from_dedup("synthetic", "message-1"),
            "c67e86d0fa1d6ce7a64b672e2be830ea40ea99da78e94054c021f12377852286"
        );
        assert_eq!(
            fallback_event_key("synthetic", "session-1", 123, "model-1", 0),
            "c7566b9ce648dba5e80354d79f125a8dadbe0f98acf0b0714b0b30c3a5e37143"
        );
    }

    #[test]
    fn fallback_ordinals_are_stable_and_distinct() {
        let first = fallback_event_key("synthetic", "session-1", 123, "model-1", 0);
        assert_eq!(
            first,
            fallback_event_key("synthetic", "session-1", 123, "model-1", 0)
        );
        assert_ne!(
            first,
            fallback_event_key("synthetic", "session-1", 123, "model-1", 1)
        );
    }

    #[test]
    fn qoder_turn_key_matches_fixed_vector() {
        assert_eq!(
            qoder_event_key("session-1", Some("msg:message-1"), 123, "model-1", 0),
            "79ee5eb782eda31e84a5b53e3686ae636d2244b0f2fcba3b6921b9f3c54daafc"
        );
    }

    #[test]
    fn blank_dedup_keys_and_blank_qoder_turn_keys_use_fallback() {
        let fallback = FallbackKey {
            session_id: "session-1",
            timestamp: 123,
            model_id: "model-1",
            ordinal: 2,
        };
        assert_eq!(
            event_key("synthetic", Some(""), fallback),
            fallback_event_key("synthetic", "session-1", 123, "model-1", 2)
        );
        assert_eq!(
            qoder_event_key("session-1", None, 123, "model-1", 2),
            fallback_event_key("qoder-intl", "session-1", 123, "model-1", 2)
        );
        assert_ne!(
            qoder_event_key("session-1", Some("uuid:uuid-1"), 123, "model-1", 2),
            qoder_event_key("session-1", Some("uuid:uuid-2"), 123, "model-1", 2)
        );
    }

    #[test]
    fn changing_token_numbers_does_not_change_the_key() {
        let first = fixture_event();
        let mut changed = first.clone();
        changed.input = 9_999;
        changed.output = 8_888;
        changed.cache_read = 7_777;
        changed.cache_write = 6_666;
        changed.cache_write_1h = 5_555;
        changed.reasoning = 4_444;
        assert_eq!(first.event_key, changed.event_key);
    }

    #[test]
    fn serialized_keys_are_within_the_upload_allowlist() {
        let value = serde_json::to_value(fixture_event()).expect("serialize event");
        let object = value.as_object().expect("event object");
        assert!(
            object
                .keys()
                .all(|key| UPLOAD_FIELDS.contains(&key.as_str()))
        );
        assert!(!object.contains_key("costUsd"));
    }

    #[test]
    fn serde_rejects_unknown_fields() {
        let mut value = serde_json::to_value(fixture_event()).expect("serialize event");
        value["sessionTitle"] = serde_json::Value::String("not-allowed".into());
        assert!(serde_json::from_value::<UsageEvent>(value).is_err());
    }
}
