//! Upload envelope (architecture 6.1), heartbeat body (5.8) and pairing
//! messages (6.4), shared by the server and the collector.
//!
//! Request types reject unknown fields; response types accept them so an
//! older collector keeps working against a newer server.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_EVENTS_PER_BATCH: usize = 2_000;
/// Limit on the request body as sent, compressed or not.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_DECOMPRESSED_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectorInfo {
    pub version: String,
    pub tokscale_rev: String,
    pub parser_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qoder_rate_version: Option<String>,
}

/// Events stay raw JSON here so that the server can reject one bad event
/// without failing the whole batch. Senders put `UsageEvent::to_json_value()`
/// results in it, never a plain serde serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngestEnvelope {
    pub schema_version: u32,
    pub device_id: String,
    pub batch_id: String,
    pub collector: CollectorInfo,
    pub scan_generation: String,
    pub events: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    UnknownField,
    InvalidEvent,
    InvalidValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedEvent {
    pub index: usize,
    pub reason: RejectReason,
}

/// `accepted + updated + duplicates + rejected.len()` equals the number of
/// events sent; `accepted` counts keys the server had not stored before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestResponse {
    pub accepted: u64,
    pub updated: u64,
    pub duplicates: u64,
    pub rejected: Vec<RejectedEvent>,
    pub min_collector_version: String,
    pub min_app_version: String,
    /// Changes when the server database is restored from a backup (D031).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientStatus {
    pub client: String,
    pub detected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan_events: Option<u64>,
    /// A short code such as `parse_failed`; never a path or a log excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Heartbeat {
    pub schema_version: u32,
    pub device_id: String,
    pub collector: CollectorInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan_duration_ms: Option<u64>,
    pub outbox_backlog: u64,
    pub backfill_complete: bool,
    pub clients: Vec<ClientStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatResponse {
    pub min_collector_version: String,
    pub min_app_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

impl Platform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormFactor {
    Laptop,
    Desktop,
}

impl FormFactor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Laptop => "laptop",
            Self::Desktop => "desktop",
        }
    }
}

/// `POST /api/pair/claim`. The code is a one-time secret: never log it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairClaimRequest {
    pub code: String,
    pub platform: Platform,
    pub form_factor: FormFactor,
    pub display_name: String,
}

impl std::fmt::Debug for PairClaimRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairClaimRequest")
            .field("code", &"<redacted>")
            .field("platform", &self.platform)
            .field("form_factor", &self.form_factor)
            .finish_non_exhaustive()
    }
}

/// Both tokens appear in plain text only in this one response.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairClaimResponse {
    pub device_id: String,
    pub ingest_token: String,
    pub read_token: String,
}

impl std::fmt::Debug for PairClaimResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairClaimResponse")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// `POST /api/pair/codes`; `code` is grouped for display (`XXXX-XXXX-XXXX`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCodeResponse {
    pub code: String,
    pub expires_at: i64,
}

impl std::fmt::Debug for PairCodeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairCodeResponse")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope_json() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "deviceId": "dev_x",
            "batchId": "b1",
            "collector": {"version": "0.1.0", "tokscaleRev": "e9cf104", "parserGeneration": "g1"},
            "scanGeneration": "g1",
            "events": [{"anything": 1}]
        })
    }

    #[test]
    fn envelope_rejects_unknown_fields_but_keeps_events_raw() {
        let parsed: IngestEnvelope = serde_json::from_value(envelope_json()).expect("valid");
        assert_eq!(parsed.events.len(), 1);
        let mut extra = envelope_json();
        extra["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<IngestEnvelope>(extra).is_err());
        let mut nested = envelope_json();
        nested["collector"]["hostname"] = serde_json::json!("x");
        assert!(serde_json::from_value::<IngestEnvelope>(nested).is_err());
    }

    #[test]
    fn responses_tolerate_new_server_fields() {
        let response = serde_json::json!({
            "accepted": 1, "updated": 0, "duplicates": 0,
            "rejected": [{"index": 0, "reason": "unknown_field", "detail": "x"}],
            "minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0", "future": 1
        });
        let parsed: IngestResponse = serde_json::from_value(response).expect("lenient");
        assert_eq!(parsed.rejected[0].reason, RejectReason::UnknownField);
        assert_eq!(parsed.store_id, None);
        let restored = serde_json::json!({
            "minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0", "storeId": "abc"
        });
        let parsed: HeartbeatResponse = serde_json::from_value(restored).expect("store id");
        assert_eq!(parsed.store_id.as_deref(), Some("abc"));
    }

    #[test]
    fn secrets_are_not_in_debug_output() {
        let claim = PairClaimRequest {
            code: "SECRETCODE12".into(),
            platform: Platform::Macos,
            form_factor: FormFactor::Laptop,
            display_name: "x".into(),
        };
        assert!(!format!("{claim:?}").contains("SECRETCODE12"));
        let tokens = PairClaimResponse {
            device_id: "dev_x".into(),
            ingest_token: "tdi_secret".into(),
            read_token: "tdr_secret".into(),
        };
        assert!(!format!("{tokens:?}").contains("secret"));
        let code = PairCodeResponse {
            code: "ABCD-EFGH-JKMN".into(),
            expires_at: 1,
        };
        assert!(!format!("{code:?}").contains("ABCD"));
    }
}
