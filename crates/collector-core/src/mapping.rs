use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use tokscale_core::{UnifiedMessage, sessions::CostSource};
use usage_event::{FallbackKey, Precision, UsageEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingError {
    ForbiddenUpstreamQoder,
    EmptyRequiredText,
    NegativeTimestamp,
    NegativeTokenCount,
    InvalidCost,
}

/// Token precision at upstream e9cf1041. Cost provenance is deliberately not
/// consulted: provider-reported costs do not prove that tokens were measured.
pub fn token_precision(message: &UnifiedMessage) -> Precision {
    match message.client.as_str() {
        // sessions/zcode.rs:153-160 (legacy JSONL char estimate), 447-471 (DB
        // counters and the distinguishing key prefix).
        "zcode"
            if message
                .dedup_key
                .as_deref()
                .is_some_and(|key| key.starts_with("zcode-sqlite:")) =>
        {
            Precision::Measured
        }
        // sessions/codex.rs:167-207, 275-302, 2558-2686: only reported counters
        // are converted/differenced; the token_count "estimate" at 1674-1675
        // is Codex's non-advancing snapshot, not a character-derived count.
        "codex" => Precision::Measured,
        // sessions/dsh.rs:485-518, 595-609: token values come from usage chunks.
        "dsh" => Precision::Measured,
        // sessions/openclaw.rs:357-430 and utils.rs:776-827: usage fields only.
        // openclaw.rs:1908-1909 estimates cost later, not token counts.
        "openclaw" => Precision::Measured,
        // sessions/claudecode.rs:931-947, 1785-1790: production disables char
        // estimation for tool_result. Explicit tool-result input tokens remain.
        "claude" => Precision::Measured,
        // sessions/tencent_buddy.rs:118-152, 262-278, 445-459: JSONL and
        // extension-log rows carry reported usage fields. The aggregate SQLite
        // fallback (sessions/workbuddy.rs:43-112) has one input-only counter.
        "workbuddy"
            if message.tokens.output > 0
                || message.tokens.cache_read > 0
                || message.tokens.cache_write > 0
                || message.tokens.reasoning > 0 =>
        {
            Precision::Measured
        }
        _ => Precision::Unknown,
    }
}

/// Map only fields explicitly present in UsageEvent v1. In particular, this
/// never copies workspace data, titles, agents, or source dates. The pricing
/// model, billing channel and service tier are copied only so the server can
/// price the event the way tokscale would (D042); they name no content.
pub fn map_message(message: &UnifiedMessage, ordinal: u64) -> Result<UsageEvent, MappingError> {
    let client = message.client.as_str();
    if client == "qoder" {
        return Err(MappingError::ForbiddenUpstreamQoder);
    }
    if client.is_empty() || message.session_id.is_empty() || message.model_id.is_empty() {
        return Err(MappingError::EmptyRequiredText);
    }
    if message.timestamp < 0 {
        return Err(MappingError::NegativeTimestamp);
    }
    let event_key = usage_event::event_key(
        client,
        message.dedup_key.as_deref(),
        FallbackKey {
            session_id: message.session_id.as_str(),
            timestamp: message.timestamp,
            model_id: message.model_id.as_str(),
            ordinal,
        },
    );
    let nonnegative =
        |value: i64| u64::try_from(value).map_err(|_| MappingError::NegativeTokenCount);
    let cost_usd = match message.cost_source {
        CostSource::ProviderReported if message.cost.is_finite() && message.cost >= 0.0 => {
            Some(message.cost)
        }
        CostSource::ProviderReported => return Err(MappingError::InvalidCost),
        // An absent pricing service means non-authoritative costs are not usable.
        CostSource::Unknown | CostSource::Estimated => None,
    };
    let cost_source = serde_json::to_value(message.cost_source)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(MappingError::InvalidCost)?;
    Ok(UsageEvent {
        event_key,
        client: client.to_owned(),
        session_id: message.session_id.to_string(),
        parent_session_id: message.parent_session_id.as_ref().map(ToString::to_string),
        model_id: message.model_id.to_string(),
        provider_id: (!message.provider_id.is_empty()).then(|| message.provider_id.to_string()),
        ts: message.timestamp,
        input: nonnegative(message.tokens.input)?,
        output: nonnegative(message.tokens.output)?,
        cache_read: nonnegative(message.tokens.cache_read)?,
        cache_write: nonnegative(message.tokens.cache_write)?,
        cache_write_1h: nonnegative(message.tokens.cache_write_1h)?,
        reasoning: nonnegative(message.tokens.reasoning)?,
        cost_usd,
        cost_source,
        precision: token_precision(message),
        credits: None,
        rate_version: None,
        // No device salt or project-label configuration is available in T3.
        project_hash: None,
        project_label: None,
        message_count: i64::from(message.message_count),
        is_turn_start: message.is_turn_start,
        duration_ms: message.duration_ms,
        pricing_model_id: message
            .pricing_model_id
            .as_ref()
            .filter(|raw| !raw.is_empty() && raw.as_str() != message.model_id.as_str())
            .map(ToString::to_string),
        billing_provider: message.billing_identity.id().map(str::to_owned),
        service_tier: message
            .service_tier
            .as_ref()
            .map(|tier| tier.trim())
            .filter(|tier| !tier.is_empty())
            .map(str::to_owned),
    })
}

pub fn map_messages(messages: &[UnifiedMessage]) -> Result<Vec<UsageEvent>, MappingError> {
    // Client is part of the group so that a scan of a client subset assigns
    // the same ordinals as a scan of every client (D023 revision).
    let mut ordinals = BTreeMap::<(String, String, i64, String), u64>::new();
    messages
        .iter()
        .map(|message| {
            let group = (
                message.client.to_string(),
                message.session_id.to_string(),
                message.timestamp,
                message.model_id.to_string(),
            );
            let ordinal = ordinals.entry(group).or_insert(0);
            let current = *ordinal;
            *ordinal = ordinal.saturating_add(1);
            map_message(message, current)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventKeySetDigest {
    pub event_count: usize,
    pub unique_event_keys: usize,
    pub sha256: String,
}

/// Object keys sorted at every level, no insignificant whitespace.
pub fn canonical_json(value: &serde_json::Value) -> String {
    fn write(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries = map.iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                out.push('{');
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String(key.clone()).to_string());
                    out.push(':');
                    write(value, out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// sha256 of the canonical JSON of every serialized (allowlisted) field.
pub fn event_value_hash(event: &UsageEvent) -> [u8; 32] {
    let value = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    Sha256::digest(canonical_json(&value).as_bytes()).into()
}

/// Hash of the sorted, de-duplicated `(event_key, value hash)` pairs.
pub fn value_digest_of_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, [u8; 32])>) -> String {
    let pairs = pairs.into_iter().collect::<std::collections::BTreeSet<_>>();
    let mut hasher = Sha256::new();
    for (key, hash) in pairs {
        hasher.update(key.as_bytes());
        hasher.update([0]);
        hasher.update(hash);
    }
    format!("{:x}", hasher.finalize())
}

pub fn event_value_digest(events: &[UsageEvent]) -> String {
    value_digest_of_pairs(
        events
            .iter()
            .map(|event| (event.event_key.as_str(), event_value_hash(event))),
    )
}

/// Hash only a sorted set; neither keys nor their source fields are logged.
pub fn event_key_set_digest(events: &[UsageEvent]) -> EventKeySetDigest {
    let keys = events
        .iter()
        .map(|event| event.event_key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut hasher = Sha256::new();
    for key in &keys {
        hasher.update(key.as_bytes());
    }
    EventKeySetDigest {
        event_count: events.len(),
        unique_event_keys: keys.len(),
        sha256: format!("{:x}", hasher.finalize()),
    }
}

#[cfg(test)]
mod tests {
    use tokscale_core::{
        TokenBreakdown,
        sessions::{
            BillingIdentity, BillingIdentityOrigin, ClaudeEntryUuid, EstimateStatus, SharedStr,
        },
    };

    use super::*;

    fn synthetic_message() -> UnifiedMessage {
        UnifiedMessage {
            client: SharedStr::from("synthetic-client"),
            model_id: SharedStr::from("synthetic-model"),
            provider_id: SharedStr::from("synthetic-provider"),
            session_id: SharedStr::from("synthetic-session"),
            workspace_key: Some(SharedStr::from("workspace-key-must-not-survive")),
            workspace_label: Some(SharedStr::from("workspace-label-must-not-survive")),
            timestamp: 1_700_000_000_000,
            date: SharedStr::from("synthetic-date-must-not-survive"),
            tokens: TokenBreakdown {
                input: 100,
                output: 20,
                cache_read: 30,
                cache_write: 10,
                cache_write_1h: 5,
                reasoning: 4,
            },
            cost: 0.0123,
            cost_source: CostSource::ProviderReported,
            service_tier: Some(SharedStr::from("priority")),
            duration_ms: Some(1_250),
            message_count: 1,
            agent: Some(SharedStr::from("agent-must-not-survive")),
            dedup_key: Some("synthetic-dedup-key".into()),
            session_title: Some(SharedStr::from("session-title-must-not-survive")),
            parent_session_id: Some(SharedStr::from("synthetic-parent-session")),
            is_turn_start: true,
            model_attribution_conflicted: true,
            billing_identity: BillingIdentity::from_explicit(
                Some("Synthetic-Billing"),
                BillingIdentityOrigin::ExplicitRecord,
            ),
            pricing_model_id: Some(SharedStr::from("synthetic-model-raw")),
            estimate_status: EstimateStatus::Authoritative,
            copilot_context: None,
            machine_label: Some(SharedStr::from("machine-label-must-not-survive")),
            claude_entry_uuid: ClaudeEntryUuid::default(),
        }
    }

    #[test]
    fn precision_is_client_specific_without_changing_event_keys() {
        let mut message = synthetic_message();
        assert_eq!(token_precision(&message), Precision::Unknown);
        message.client = SharedStr::from("zcode");
        assert_eq!(token_precision(&message), Precision::Unknown);
        message.dedup_key = Some("zcode-sqlite:synthetic-id".into());
        assert_eq!(token_precision(&message), Precision::Measured);
        let before = map_message(&message, 0).expect("mapped event");
        assert_eq!(before.precision, Precision::Measured);
        message.dedup_key = Some("zcode-jsonl:synthetic-id".into());
        assert_eq!(
            map_message(&message, 0).expect("mapped event").precision,
            Precision::Unknown
        );
        let mut changed = before.clone();
        changed.precision = Precision::Unknown;
        assert_eq!(changed.event_key, before.event_key);
        message.dedup_key = None;
        let key_before = map_message(&message, 0).expect("fallback event").event_key;
        let mut precision_override = map_message(&message, 0).expect("fallback event");
        precision_override.precision = Precision::Measured;
        assert_eq!(precision_override.event_key, key_before);
    }

    #[test]
    fn workbuddy_is_measured_only_when_usage_has_more_than_one_input_counter() {
        let mut detailed = synthetic_message();
        detailed.client = SharedStr::from("workbuddy");
        detailed.dedup_key = Some("workbuddy:synthetic-session:synthetic-message".into());
        assert_eq!(token_precision(&detailed), Precision::Measured);

        let mut aggregate = detailed.clone();
        aggregate.dedup_key = Some("workbuddy:synthetic-session:1700000000".into());
        aggregate.tokens = TokenBreakdown {
            input: 12_345,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
        };
        assert_eq!(token_precision(&aggregate), Precision::Unknown);
    }

    #[test]
    fn maps_allowed_fields_and_drops_every_non_allowlisted_source_field() {
        let event = map_message(&synthetic_message(), 0).expect("synthetic message");
        assert_eq!(event.client, "synthetic-client");
        assert_eq!(
            event.parent_session_id.as_deref(),
            Some("synthetic-parent-session")
        );
        assert_eq!(event.input, 100);
        assert_eq!(event.cache_write_1h, 5);
        assert_eq!(event.cost_usd, Some(0.0123));
        assert_eq!(event.cost_source, "providerReported");
        assert_eq!(event.precision, Precision::Unknown);
        assert_eq!(
            event.pricing_model_id.as_deref(),
            Some("synthetic-model-raw")
        );
        assert_eq!(event.billing_provider.as_deref(), Some("synthetic-billing"));
        assert_eq!(event.service_tier.as_deref(), Some("priority"));
        let serialized = serde_json::to_string(&event).expect("serialize mapped event");
        for source_value in [
            "workspace-key-must-not-survive",
            "workspace-label-must-not-survive",
            "session-title-must-not-survive",
            "agent-must-not-survive",
            "synthetic-date-must-not-survive",
            "machine-label-must-not-survive",
        ] {
            assert!(!serialized.contains(source_value));
        }
        let value: serde_json::Value = serde_json::from_str(&serialized).expect("JSON object");
        let object = value.as_object().expect("event object");
        assert!(
            object
                .keys()
                .all(|key| usage_event::UPLOAD_FIELDS.contains(&key.as_str()))
        );
        for key in [
            "sessionTitle",
            "workspaceKey",
            "workspaceLabel",
            "projectLabel",
        ] {
            assert!(!object.contains_key(key));
        }
    }

    #[test]
    fn pricing_context_is_sent_only_when_it_adds_information() {
        let mut message = synthetic_message();
        message.pricing_model_id = Some(message.model_id.clone());
        message.billing_identity = BillingIdentity::Unknown;
        message.service_tier = Some(SharedStr::from("  "));
        let event = map_message(&message, 0).expect("mapped event");
        assert_eq!(event.pricing_model_id, None);
        assert_eq!(event.billing_provider, None);
        assert_eq!(event.service_tier, None);
        let value = serde_json::to_value(&event).expect("serialize");
        for key in ["pricingModelId", "billingProvider", "serviceTier"] {
            assert!(value.get(key).is_none());
        }
        // The pricing fields never feed the event key.
        assert_eq!(
            event.event_key,
            map_message(&synthetic_message(), 0)
                .expect("mapped event")
                .event_key
        );
    }

    #[test]
    fn value_digest_is_order_independent_and_tracks_values() {
        let first = map_message(&synthetic_message(), 0).expect("first");
        let mut second = first.clone();
        second.event_key = "synthetic-second-key".into();
        let forward = event_value_digest(&[first.clone(), second.clone()]);
        assert_eq!(
            forward,
            event_value_digest(&[second.clone(), first.clone()])
        );
        let mut changed = second;
        changed.output += 1;
        assert_ne!(forward, event_value_digest(&[first, changed]));
        let value = serde_json::json!({"b": [1, {"d": 2, "c": "x"}], "a": null});
        assert_eq!(
            canonical_json(&value),
            r#"{"a":null,"b":[1,{"c":"x","d":2}]}"#
        );
    }

    #[test]
    fn mapping_twice_produces_identical_key_lists() {
        let messages = vec![synthetic_message(), synthetic_message()];
        let first = map_messages(&messages).expect("first mapping");
        let second = map_messages(&messages).expect("second mapping");
        let first_keys: Vec<_> = first.iter().map(|event| &event.event_key).collect();
        let second_keys: Vec<_> = second.iter().map(|event| &event.event_key).collect();
        assert_eq!(first_keys, second_keys);
    }

    #[test]
    fn fallback_uses_parse_order_within_session_timestamp_and_model() {
        let mut first = synthetic_message();
        first.dedup_key = None;
        let mut second = first.clone();
        second.dedup_key = None;
        let mut different_model = first.clone();
        different_model.model_id = SharedStr::from("another-synthetic-model");
        different_model.dedup_key = None;
        let events = map_messages(&[first.clone(), second, different_model]).expect("map events");
        assert_eq!(
            events[0].event_key,
            map_message(&first, 0).expect("first").event_key
        );
        assert_ne!(events[0].event_key, events[1].event_key);
        assert_eq!(
            events[2].event_key,
            usage_event::fallback_event_key(
                "synthetic-client",
                "synthetic-session",
                1_700_000_000_000,
                "another-synthetic-model",
                0
            )
        );
    }

    #[test]
    fn changing_tokens_does_not_change_mapped_event_key() {
        let mut original = synthetic_message();
        original.dedup_key = None;
        let mut changed = original.clone();
        changed.tokens.input = 9_999;
        changed.tokens.output = 8_888;
        changed.tokens.cache_read = 7_777;
        assert_eq!(
            map_message(&original, 0).expect("original").event_key,
            map_message(&changed, 0).expect("changed").event_key
        );
    }

    #[test]
    fn non_authoritative_cost_and_nonnegative_rules_are_enforced() {
        let mut message = synthetic_message();
        message.cost_source = CostSource::Estimated;
        assert_eq!(map_message(&message, 0).expect("map").cost_usd, None);
        message.tokens.input = -1;
        assert_eq!(
            map_message(&message, 0),
            Err(MappingError::NegativeTokenCount)
        );
        message.tokens.input = 0;
        message.timestamp = -1;
        assert_eq!(
            map_message(&message, 0),
            Err(MappingError::NegativeTimestamp)
        );
        message.timestamp = 1;
        message.cost = f64::NAN;
        message.cost_source = CostSource::ProviderReported;
        assert_eq!(map_message(&message, 0), Err(MappingError::InvalidCost));
    }

    #[test]
    fn event_summary_has_counts_and_types_but_never_event_values() {
        let event = map_message(&synthetic_message(), 0).expect("synthetic event");
        let report = crate::summarize(
            vec![synthetic_message()],
            vec!["synthetic-client".into()],
            1,
            crate::DateRange::all_history(),
            std::time::Duration::ZERO,
            vec![event.clone()],
        );
        let summary = crate::event_summary(&report).expect("event summary");
        assert_eq!(summary.total, 1);
        assert_eq!(summary.by_client[0].count, 1);
        let example = serde_json::to_string(&summary).expect("serialized summary");
        assert_eq!(summary.example["eventKey"], "string");
        assert_eq!(summary.example["ts"], "integer");
        assert_eq!(summary.example["isTurnStart"], "boolean");
        for value in [
            "synthetic-session",
            "synthetic-parent-session",
            "synthetic-model",
            "0.0123",
            "1250",
        ] {
            assert!(!example.contains(value));
        }
        assert!(!example.contains(&event.event_key));
        for key in usage_event::UPLOAD_FIELDS {
            assert!(
                summary
                    .example
                    .get(key)
                    .is_some_and(serde_json::Value::is_string)
            );
        }
    }

    #[test]
    fn forbidden_qoder_never_reaches_event_mapping() {
        let mut message = synthetic_message();
        message.client = SharedStr::from("qoder");
        assert_eq!(
            map_message(&message, 0),
            Err(MappingError::ForbiddenUpstreamQoder)
        );
    }
}
