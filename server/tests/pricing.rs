//! Server-side cost estimates (D042): events are priced on ingest, re-priced
//! when the catalog changes, and the dashboard reports the estimate.

mod common;

use std::collections::HashMap;

use common::{Harness, START_MS, envelope};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokenlens_server::pricing::Pricer;
use tokenlens_server::secrets;
use tokscale_core::pricing::custom::CustomPricing;
use tokscale_core::pricing::{ModelPricing, PricingService};

/// `synthetic-model` costs `input_per_million` per million input tokens and
/// $0.10 per million cache reads; `no-cache-model` has no cache price.
fn pricer(version: &str, input_per_million: f64) -> Pricer {
    let mut models = HashMap::new();
    models.insert(
        "synthetic-model".to_owned(),
        ModelPricing {
            input_cost_per_token: Some(input_per_million / 1_000_000.0),
            output_cost_per_token: Some(10.0 / 1_000_000.0),
            cache_read_input_token_cost: Some(0.1 / 1_000_000.0),
            cache_creation_input_token_cost: Some(1.0 / 1_000_000.0),
            ..ModelPricing::default()
        },
    );
    models.insert(
        "no-cache-model".to_owned(),
        ModelPricing {
            input_cost_per_token: Some(1.0 / 1_000_000.0),
            output_cost_per_token: Some(2.0 / 1_000_000.0),
            ..ModelPricing::default()
        },
    );
    let service = PricingService::new_with_custom(
        CustomPricing::from_models(models),
        HashMap::new(),
        HashMap::new(),
    );
    // START_MS, 2027-01-15 in Asia/Shanghai.
    Pricer::new(service, version, START_MS)
}

fn event(seed: &str, client: &str, model: &str, [input, cache_read]: [u64; 2]) -> Value {
    json!({
        "eventKey": secrets::sha256_hex(seed),
        "client": client,
        "sessionId": format!("session-{seed}"),
        "modelId": model,
        "ts": START_MS - 60_000,
        "input": input,
        "output": 0,
        "cacheRead": cache_read,
        "cacheWrite": 0,
        "cacheWrite1h": 0,
        "reasoning": 0,
        "costSource": "unknown",
        "precision": "measured",
        "messageCount": 1,
        "isTurnStart": true
    })
}

fn world_events() -> Vec<Value> {
    let mut reported = event(
        "reported",
        "synthetic-client",
        "synthetic-model",
        [1_000_000, 0],
    );
    reported["costUsd"] = json!(0.5);
    reported["costSource"] = json!("providerReported");
    let mut qoder = event("qoder", "qoder-intl", "auto", [400, 0]);
    qoder["precision"] = json!("estimated");
    qoder["credits"] = json!(2.5);
    qoder["rateVersion"] = json!("synthetic-rates-v1");
    vec![
        // 1M input at the catalog rate.
        event(
            "priced",
            "synthetic-client",
            "synthetic-model",
            [1_000_000, 0],
        ),
        reported,
        qoder,
        event("unknown", "synthetic-client", "never-heard-of-it", [300, 0]),
    ]
}

fn close(value: &Value, expected: f64) -> bool {
    value
        .as_f64()
        .is_some_and(|value| (value - expected).abs() < 1e-9)
}

fn caveat<'a>(summary: &'a Value, code: &str) -> Option<&'a Value> {
    summary["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .find(|caveat| caveat["code"] == code)
}

#[tokio::test]
async fn events_are_priced_on_ingest_and_follow_catalog_changes() {
    let h = Harness::start().await;
    let device = h.pair().await;
    h.state.set_pricer(pricer("v1", 3.0));
    let (status, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", world_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, s) = h.get("/api/dashboard/summary", Some(&device.read)).await;
    assert!(close(&s["costUsd"], 3.0 + 0.5), "{}", s["costUsd"]);
    // Qoder shows credits and is not counted as unpriced; the unknown model is.
    assert_eq!(s["unpricedTokens"], 300);
    assert_eq!(s["credits"], 2.5);
    assert_eq!(s["pricing"]["version"], "v1");
    assert_eq!(s["pricing"]["asOf"], "2027-01-15");
    let note = caveat(&s, "estimated_cost").expect("cost caveat");
    assert!(
        note["message"]
            .as_str()
            .expect("text")
            .contains("2027-01-15")
    );
    assert!(caveat(&s, "partial_cost").is_none());

    // A new catalog re-prices stored events; the reported cost stays.
    h.state.set_pricer(pricer("v2", 6.0));
    assert_eq!(h.state.reprice().await.expect("reprice"), 4);
    assert_eq!(h.state.reprice().await.expect("nothing left"), 0);
    let (_, s) = h.get("/api/dashboard/summary", Some(&device.read)).await;
    assert!(close(&s["costUsd"], 6.0 + 0.5), "{}", s["costUsd"]);
    assert_eq!(s["pricing"]["version"], "v2");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events WHERE price_version = 'v2'"),
        4
    );
    let statuses = h
        .side_db()
        .prepare("SELECT price_status FROM usage_events ORDER BY price_status")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(statuses, ["credits", "priced", "reported", "unpriced"]);

    // Session list and detail use the same estimate.
    let (_, sessions) = h
        .get("/api/dashboard/sessions?sort=cost", Some(&device.read))
        .await;
    let first = &sessions["sessions"][0];
    assert!(close(&first["costUsd"], 6.0), "{first}");
    let key = first["sessionKey"].as_str().expect("key").to_owned();
    let (status, detail) = h
        .get(
            &format!("/api/dashboard/sessions/{key}"),
            Some(&device.read),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(close(&detail["session"]["costUsd"], 6.0), "{detail}");
}

#[tokio::test]
async fn events_wait_for_the_first_catalog() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let (status, _) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", world_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, s) = h.get("/api/dashboard/summary", Some(&device.read)).await;
    // Only the reported cost is known; nothing is shown as $0.
    assert!(close(&s["costUsd"], 0.5), "{}", s["costUsd"]);
    assert_eq!(s["unpricedTokens"], 1_000_000 + 300);
    assert_eq!(s["pricing"], Value::Null);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events WHERE price_status IS NULL"),
        2
    );

    h.state.set_pricer(pricer("v1", 3.0));
    assert_eq!(h.state.reprice().await.expect("reprice"), 4);
    let (_, s) = h.get("/api/dashboard/summary", Some(&device.read)).await;
    assert!(close(&s["costUsd"], 3.5), "{}", s["costUsd"]);
}

#[tokio::test]
async fn missing_bucket_prices_warn_that_the_cost_is_low() {
    let h = Harness::start().await;
    let device = h.pair().await;
    h.state.set_pricer(pricer("v1", 3.0));
    let events = vec![event(
        "p",
        "synthetic-client",
        "no-cache-model",
        [1_000_000, 1_000_000],
    )];
    let (status, _) = h
        .ingest(&device.ingest, &envelope(&device.id, "b1", "g1", events))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, s) = h.get("/api/dashboard/summary", Some(&device.read)).await;
    // The known input part is counted; the cache reads have no price.
    assert!(close(&s["costUsd"], 1.0), "{}", s["costUsd"]);
    assert_eq!(s["unpricedTokens"], 0);
    assert!(caveat(&s, "partial_cost").is_some(), "{}", s["caveats"]);
}

#[tokio::test]
async fn pricing_context_is_stored_and_changes_count_as_updates() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let plain = event("ctx", "codex", "synthetic-model", [1_000, 0]);
    let (_, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", vec![plain.clone()]),
        )
        .await;
    assert_eq!(body["accepted"], 1);
    let mut with_context = plain;
    with_context["pricingModelId"] = json!("synthetic-model-raw");
    with_context["billingProvider"] = json!("openai");
    with_context["serviceTier"] = json!("priority");
    let (status, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b2", "g1", vec![with_context.clone()]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["updated"], 1);
    let (_, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b3", "g1", vec![with_context]),
        )
        .await;
    assert_eq!(body["duplicates"], 1);
    let stored: (String, String, String) = h
        .side_db()
        .query_row(
            "SELECT pricing_model_id, billing_provider, service_tier FROM usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("row");
    assert_eq!(
        stored,
        (
            "synthetic-model-raw".to_owned(),
            "openai".to_owned(),
            "priority".to_owned()
        )
    );
    // Control characters are refused like in every other text field.
    let mut bad = event("bad", "codex", "synthetic-model", [1, 0]);
    bad["serviceTier"] = json!("prio\u{7}rity");
    let (_, body) = h
        .ingest(&device.ingest, &envelope(&device.id, "b4", "g1", vec![bad]))
        .await;
    assert_eq!(body["accepted"], 0);
    assert_eq!(body["rejected"].as_array().map(Vec::len), Some(1));
}
