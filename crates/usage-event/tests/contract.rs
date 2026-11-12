use std::collections::BTreeSet;

use jsonschema::validator_for;
use serde_json::Value;
use usage_event::{REQUIRED_FIELDS, SCHEMA_JSON, UPLOAD_FIELDS, UsageEvent};

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../fixtures/usage-events.json"))
        .expect("valid synthetic fixture array")
}

#[test]
fn schema_and_rust_have_identical_field_sets() {
    let schema: Value = serde_json::from_str(SCHEMA_JSON).expect("valid schema JSON");
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["additionalProperties"], false);
    let schema_fields: BTreeSet<_> = schema["properties"]
        .as_object()
        .expect("schema properties")
        .keys()
        .map(String::as_str)
        .collect();
    let rust_fields: BTreeSet<_> = UPLOAD_FIELDS.into_iter().collect();
    assert_eq!(schema_fields, rust_fields);
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .expect("required fields")
        .iter()
        .map(|field| field.as_str().expect("required field name"))
        .collect();
    assert_eq!(required, REQUIRED_FIELDS.into_iter().collect());
}

#[test]
fn all_synthetic_fixtures_match_schema_and_rust_contract() {
    let validator = validator_for(&serde_json::from_str(SCHEMA_JSON).expect("schema JSON"))
        .expect("valid JSON Schema");
    let cases = fixtures();
    assert_eq!(cases.len(), 4);
    let precisions: BTreeSet<_> = cases
        .iter()
        .map(|case| case["precision"].as_str().expect("precision"))
        .collect();
    assert!(precisions.contains("measured"));
    assert!(precisions.contains("estimated"));
    for case in cases {
        assert!(
            validator.validate(&case).is_ok(),
            "fixture must match schema"
        );
        let event: UsageEvent = serde_json::from_value(case.clone()).expect("serde fixture");
        assert!(validator.validate(&case).is_ok());
        let serialized = serde_json::to_value(event).expect("round-trip event");
        assert!(validator.validate(&serialized).is_ok());
        let object = serialized.as_object().expect("event object");
        assert!(
            object
                .keys()
                .all(|key| UPLOAD_FIELDS.contains(&key.as_str()))
        );
        for field in [
            "sessionTitle",
            "workspaceKey",
            "workspaceLabel",
            "agent",
            "date",
        ] {
            assert!(!object.contains_key(field));
        }
        let optional = ["parentSessionId", "credits", "rateVersion"];
        if case["precision"] == "estimated" {
            assert!(
                optional[1..]
                    .iter()
                    .all(|field| object.contains_key(*field))
            );
        }
    }
}

#[test]
fn fallback_fixture_contains_the_key_generated_from_its_synthetic_identity() {
    let fallback = fixtures()
        .into_iter()
        .find(|event| event["sessionId"] == "synthetic-session-fallback")
        .expect("fallback fixture");
    assert_eq!(
        fallback["eventKey"],
        usage_event::fallback_event_key(
            "synthetic-client",
            "synthetic-session-fallback",
            1_700_000_003_000,
            "synthetic-model",
            0,
        )
    );
}

#[test]
fn unknown_fields_are_rejected_by_schema_and_serde() {
    let mut value = fixtures().into_iter().next().expect("fixture");
    value["unexpectedField"] = Value::String("synthetic-unknown-value".into());
    let schema: Value = serde_json::from_str(SCHEMA_JSON).expect("schema JSON");
    let validator = validator_for(&schema).expect("valid schema");
    assert!(validator.validate(&value).is_err());
    assert!(serde_json::from_value::<UsageEvent>(value).is_err());
}

#[test]
fn credits_and_rate_version_rules_match_schema_and_rust_validation() {
    let validator = validator_for(&serde_json::from_str(SCHEMA_JSON).expect("schema JSON"))
        .expect("valid JSON Schema");
    let mut measured_with_credits = fixtures().remove(0);
    measured_with_credits["credits"] = Value::from(1.25);
    let mut measured_with_rate = measured_with_credits.clone();
    measured_with_rate["rateVersion"] = Value::from("synthetic-rates-v1");
    let mut estimated_without_credits = fixtures().remove(1);
    estimated_without_credits
        .as_object_mut()
        .expect("event")
        .remove("credits");
    let mut estimated_without_rate = fixtures().remove(1);
    estimated_without_rate
        .as_object_mut()
        .expect("event")
        .remove("rateVersion");
    let mut only_rate = fixtures().remove(0);
    only_rate["rateVersion"] = Value::from("synthetic-rates-v1");

    for (mut case, accepted) in [
        (measured_with_credits, true),
        (measured_with_rate, false),
        (estimated_without_credits, false),
        (estimated_without_rate, false),
        (only_rate, false),
    ] {
        let event: UsageEvent = serde_json::from_value(case.clone()).expect("fixture event");
        assert_eq!(event.validate().is_ok(), accepted);
        assert_eq!(event.to_json_value().is_ok(), accepted);
        // Test both the optional-field representation and an explicit null.
        assert_eq!(validator.validate(&case).is_ok(), accepted);
        for field in ["credits", "rateVersion"] {
            if !case.as_object().expect("event").contains_key(field) {
                case[field] = Value::Null;
            }
        }
        assert_eq!(validator.validate(&case).is_ok(), accepted);
        let event: UsageEvent = serde_json::from_value(case).expect("nullable fields");
        assert_eq!(event.validate().is_ok(), accepted);
    }
}

#[test]
fn optional_null_fields_are_accepted() {
    let mut value = fixtures().into_iter().next().expect("fixture");
    value["providerId"] = Value::Null;
    value["costUsd"] = Value::Null;
    let validator = validator_for(&serde_json::from_str(SCHEMA_JSON).expect("schema JSON"))
        .expect("valid schema");
    assert!(validator.validate(&value).is_ok());
    let event: UsageEvent = serde_json::from_value(value).expect("null means absent");
    assert_eq!(event.provider_id, None);
    assert_eq!(event.cost_usd, None);
}

#[test]
fn non_finite_cost_cannot_be_serialized_as_valid_json() {
    let mut event: UsageEvent =
        serde_json::from_value(fixtures().remove(0)).expect("fixture event");
    event.cost_usd = Some(f64::NAN);
    assert!(event.to_json_value().is_err());
}
