mod common;

use common::{Harness, envelope, fixture_events, gzip, heartbeat};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokenlens_server::clock::Clock;
use tokenlens_server::db;
use tokenlens_server::limiter::{
    CLAIM_FAILURE_LIMIT, CLAIM_FAILURE_WINDOW_MS, INGEST_REQUESTS_PER_WINDOW, INGEST_WINDOW_MS,
};
use tokenlens_server::secrets::{self, PAIR_CODE_TTL_MS};

// ---------------------------------------------------------------- pairing --

#[tokio::test]
async fn a_code_pairs_exactly_one_device() {
    let h = Harness::start().await;
    let code = h.issue_code();
    let device = h.pair_with(&code).await;
    assert!(device.id.starts_with("dev_"));
    assert!(device.ingest.starts_with("tdi_"));
    assert!(device.read.starts_with("tdr_"));
    assert_ne!(device.ingest, device.read);

    let again = h.claim(&code).await;
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        again.json::<Value>().await.expect("body")["error"],
        "invalid_pair_code"
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM devices"), 1);
    assert_eq!(h.count("SELECT COUNT(*) FROM device_tokens"), 2);
}

#[tokio::test]
async fn codes_are_typed_forgivingly() {
    let h = Harness::start().await;
    let code = h.issue_code();
    let sloppy = code.to_ascii_lowercase().replace('-', " ");
    h.pair_with(&sloppy).await;
}

#[tokio::test]
async fn expired_and_revoked_codes_are_refused() {
    let h = Harness::start().await;
    let expiring = h.issue_code();
    h.clock.advance_ms(PAIR_CODE_TTL_MS);
    assert_eq!(h.claim(&expiring).await.status(), StatusCode::UNAUTHORIZED);

    let revoked = h.issue_code();
    let changed = db::revoke_open_pair_codes(&h.side_db(), h.clock.now_ms()).expect("revoke");
    assert_eq!(changed, 1, "the expired code is not counted");
    assert_eq!(h.claim(&revoked).await.status(), StatusCode::UNAUTHORIZED);

    let fresh = h.issue_code();
    h.clock.advance_ms(PAIR_CODE_TTL_MS - 1);
    h.pair_with(&fresh).await;
}

#[tokio::test]
async fn failed_claims_are_rate_limited() {
    let h = Harness::start().await;
    let good = h.issue_code();
    for _ in 0..CLAIM_FAILURE_LIMIT {
        assert_eq!(
            h.claim("0000-0000-0000").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let blocked = h.claim(&good).await;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h.count("SELECT COUNT(*) FROM devices"), 0);
    // The window and the code lifetime are both ten minutes.
    h.clock.advance_ms(CLAIM_FAILURE_WINDOW_MS);
    assert_eq!(h.claim(&good).await.status(), StatusCode::UNAUTHORIZED);
    let fresh = h.issue_code();
    h.pair_with(&fresh).await;
}

#[tokio::test]
async fn malformed_claims_are_bad_requests() {
    let h = Harness::start().await;
    let code = h.issue_code();
    for body in [
        json!({"code": code, "platform": "macos", "formFactor": "laptop"}),
        json!({"code": code, "platform": "amiga", "formFactor": "laptop", "displayName": "x"}),
        json!({"code": code, "platform": "macos", "formFactor": "laptop", "displayName": "  "}),
        json!({"code": code, "platform": "macos", "formFactor": "laptop", "displayName": "a\u{7}b"}),
        json!({"code": code, "platform": "macos", "formFactor": "laptop", "displayName": "x", "hostname": "y"}),
    ] {
        let (status, _) = h
            .post(
                "/api/pair/claim",
                None,
                serde_json::to_vec(&body).expect("encode"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    h.pair_with(&code).await;
}

#[tokio::test]
async fn a_paired_device_issues_the_next_code() {
    let h = Harness::start().await;
    let first = h.pair().await;

    let (status, _) = h
        .post("/api/pair/codes", Some(&first.ingest), b"{}".to_vec(), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "ingest tokens cannot pair"
    );

    let (status, body) = h
        .post("/api/pair/codes", Some(&first.read), Vec::new(), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let code = body["code"].as_str().expect("code").to_owned();
    assert_eq!(body["expiresAt"], h.clock.now_ms() + PAIR_CODE_TTL_MS);
    let second = h.pair_with(&code).await;
    assert_ne!(second.id, first.id);

    // Codes issued by a device die with it.
    let (_, body) = h
        .post("/api/pair/codes", Some(&first.read), b"{}".to_vec(), None)
        .await;
    let orphan = body["code"].as_str().expect("code").to_owned();
    assert!(
        db::revoke_device(
            &mut h.side_db(),
            db::DEFAULT_TENANT_ID,
            &first.id,
            h.clock.now_ms()
        )
        .expect("revoke")
    );
    assert_eq!(h.claim(&orphan).await.status(), StatusCode::UNAUTHORIZED);

    let (status, _) = h
        .post(
            "/api/pair/codes",
            Some(&second.read),
            b"{\"x\":1}".to_vec(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_database_holds_only_hashes() {
    let h = Harness::start().await;
    let code = h.issue_code();
    let device = h.pair_with(&code).await;
    let normalized = code.replace('-', "");

    let conn = h.side_db();
    let hashes = conn
        .prepare("SELECT kind, token_hash FROM device_tokens ORDER BY kind")
        .expect("prepare")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(
        hashes,
        vec![
            ("ingest".to_owned(), secrets::sha256_hex(&device.ingest)),
            ("read".to_owned(), secrets::sha256_hex(&device.read)),
        ]
    );
    drop(conn);

    let mut stored = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let path = format!("{}{suffix}", h.db_path.display());
        if let Ok(bytes) = std::fs::read(&path) {
            stored.extend(bytes);
        }
    }
    let stored = String::from_utf8_lossy(&stored);
    for secret in [&device.ingest, &device.read, &code, &normalized] {
        assert!(!stored.contains(secret.as_str()));
    }
}

// ------------------------------------------------------------------- auth --

#[tokio::test]
async fn ingest_requires_a_live_ingest_token() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let body = serde_json::to_vec(&envelope(&device.id, "b1", "g1", Vec::new())).expect("encode");

    let response = h
        .http
        .post(format!("{}/api/ingest/events", h.base))
        .body(body.clone())
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .map(|v| v.as_bytes()),
        Some(&b"Bearer"[..])
    );

    for token in [
        device.read.as_str(),
        "tdi_not-a-real-token",
        "garbage",
        &device.ingest[..device.ingest.len() - 1],
    ] {
        let (status, body) = h
            .post("/api/ingest/events", Some(token), body.clone(), None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");
    }

    let (status, _) = h
        .post(
            "/api/ingest/events",
            Some(&device.ingest),
            body.clone(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        db::revoke_device(
            &mut h.side_db(),
            db::DEFAULT_TENANT_ID,
            &device.id,
            h.clock.now_ms()
        )
        .expect("revoke")
    );
    let (status, _) = h
        .post("/api/ingest/events", Some(&device.ingest), body, None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = h
        .post("/api/pair/codes", Some(&device.read), Vec::new(), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        !db::revoke_device(
            &mut h.side_db(),
            db::DEFAULT_TENANT_ID,
            &device.id,
            h.clock.now_ms()
        )
        .expect("revoke")
    );
}

#[tokio::test]
async fn a_device_cannot_write_for_another() {
    let h = Harness::start().await;
    let first = h.pair().await;
    let second = h.pair().await;
    let (status, body) = h
        .ingest(
            &first.ingest,
            &envelope(&second.id, "b1", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "device_mismatch");
    let (status, _) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&first.ingest),
            serde_json::to_vec(&heartbeat(&second.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(h.count("SELECT COUNT(*) FROM usage_events"), 0);
}

// ----------------------------------------------------------------- ingest --

#[tokio::test]
async fn replays_are_duplicates_and_changes_are_updates() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let events = fixture_events();
    let total = events.len() as u64;

    let (status, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", events.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], total);
    assert_eq!(body["updated"], 0);
    assert_eq!(body["duplicates"], 0);
    assert_eq!(body["rejected"], json!([]));
    assert_eq!(body["minCollectorVersion"], "0.1.0");

    let (_, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", events.clone()),
        )
        .await;
    assert_eq!(
        (body["accepted"].clone(), body["duplicates"].clone()),
        (json!(0), json!(total))
    );

    let mut changed = events.clone();
    changed[0]["output"] = json!(changed[0]["output"].as_u64().expect("output") + 7);
    let (_, body) = h
        .ingest(&device.ingest, &envelope(&device.id, "b2", "g1", changed))
        .await;
    assert_eq!(body["updated"], 1);
    assert_eq!(body["duplicates"], total - 1);

    let (_, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b3", "g2", events.clone()),
        )
        .await;
    assert_eq!(
        body["updated"], total,
        "a new scan generation rewrites every row"
    );

    assert_eq!(h.count("SELECT COUNT(*) FROM usage_events"), total as i64);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events WHERE scan_generation = 'g2'"),
        total as i64
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM ingest_batches"), 3);
    assert_eq!(
        h.count("SELECT duplicates FROM ingest_batches WHERE batch_id = 'b1'"),
        total as i64,
        "a replayed batch id keeps the latest counts"
    );

    // Same keys on another device are separate rows.
    let other = h.pair().await;
    let (_, body) = h
        .ingest(&other.ingest, &envelope(&other.id, "b4", "g1", events))
        .await;
    assert_eq!(body["accepted"], total);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events"),
        2 * total as i64
    );
}

#[tokio::test]
async fn stored_rows_round_trip_every_field() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let events = fixture_events();
    h.ingest(
        &device.ingest,
        &envelope(&device.id, "b1", "g1", events.clone()),
    )
    .await;
    let conn = h.side_db();
    for event in &events {
        let key = event["eventKey"].as_str().expect("key");
        let (precision, credits, rate): (String, Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT precision, credits, rate_version FROM usage_events WHERE event_key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("row");
        assert_eq!(json!(precision), event["precision"]);
        assert_eq!(credits, event.get("credits").and_then(Value::as_f64));
        assert_eq!(rate.map(Value::from), event.get("rateVersion").cloned());
    }
    // Replaying proves every other column compared equal too.
    let (_, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b2", "g1", events.clone()),
        )
        .await;
    assert_eq!(body["duplicates"], events.len());
}

#[tokio::test]
async fn bad_events_are_rejected_one_by_one() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let good = fixture_events();
    let mut unknown = good[0].clone();
    unknown["prompt"] = json!("must never be stored");
    unknown["eventKey"] = json!("a".repeat(64));
    let mut invalid = good[0].clone();
    invalid["precision"] = json!("guessed");
    let mut huge = good[0].clone();
    huge["input"] = json!(1_u64 << 53);
    huge["eventKey"] = json!("b".repeat(64));
    let mut short_key = good[0].clone();
    short_key["eventKey"] = json!("abc");
    let mut control = good[0].clone();
    control["modelId"] = json!("model\nname");
    control["eventKey"] = json!("c".repeat(64));

    let mut events = vec![unknown, invalid, huge, short_key, control];
    events.extend(good.clone());
    let (status, body) = h
        .ingest(&device.ingest, &envelope(&device.id, "b1", "g1", events))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["rejected"],
        json!([
            {"index": 0, "reason": "unknown_field"},
            {"index": 1, "reason": "invalid_event"},
            {"index": 2, "reason": "invalid_value"},
            {"index": 3, "reason": "invalid_value"},
            {"index": 4, "reason": "invalid_value"},
        ])
    );
    assert_eq!(body["accepted"], good.len());
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events"),
        good.len() as i64
    );
    assert_eq!(h.count("SELECT rejected FROM ingest_batches"), 5);
}

#[tokio::test]
async fn bad_envelopes_write_nothing() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let base = envelope(&device.id, "b1", "g1", fixture_events());
    let mut cases = Vec::new();
    let mut extra = base.clone();
    extra["hostname"] = json!("laptop.local");
    cases.push(extra);
    let mut nested = base.clone();
    nested["collector"]["path"] = json!("~/x");
    cases.push(nested);
    let mut version = base.clone();
    version["schemaVersion"] = json!(2);
    cases.push(version);
    let mut batch = base.clone();
    batch["batchId"] = json!("has space");
    cases.push(batch);
    let mut generation = base.clone();
    generation["scanGeneration"] = json!("");
    cases.push(generation);
    for case in cases {
        let (status, body) = h.ingest(&device.ingest, &case).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}");
        assert_eq!(body["error"], "bad_request");
    }
    let (status, _) = h
        .post(
            "/api/ingest/events",
            Some(&device.ingest),
            b"{not json".to_vec(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(h.count("SELECT COUNT(*) FROM usage_events"), 0);
    assert_eq!(h.count("SELECT COUNT(*) FROM ingest_batches"), 0);
}

#[tokio::test]
async fn gzip_bodies_are_accepted() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let body =
        serde_json::to_vec(&envelope(&device.id, "b1", "g1", fixture_events())).expect("encode");
    let (status, response) = h
        .post(
            "/api/ingest/events",
            Some(&device.ingest),
            gzip(&body),
            Some("gzip"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["accepted"], fixture_events().len());

    let (status, body) = h
        .post("/api/ingest/events", Some(&device.ingest), body, Some("br"))
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["error"], "unsupported_encoding");
    let (status, _) = h
        .post(
            "/api/ingest/events",
            Some(&device.ingest),
            b"plain".to_vec(),
            Some("gzip"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversized_batches_are_refused() {
    let h = Harness::start().await;
    let device = h.pair().await;

    let mut padded = envelope(&device.id, "b1", "g1", fixture_events());
    padded["events"][0]["modelId"] = json!("x".repeat(1024 * 1024));
    let (status, body) = h.ingest(&device.ingest, &padded).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"], "payload_too_large");

    let many = envelope(&device.id, "b2", "g1", vec![json!({}); 2_001]);
    let (status, _) = h.ingest(&device.ingest, &many).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    let mut bomb =
        serde_json::to_vec(&envelope(&device.id, "b3", "g1", Vec::new())).expect("encode");
    bomb.extend(std::iter::repeat_n(b' ', 9 * 1024 * 1024));
    let compressed = gzip(&bomb);
    assert!(compressed.len() < 64 * 1024);
    let (status, _) = h
        .post(
            "/api/ingest/events",
            Some(&device.ingest),
            compressed,
            Some("gzip"),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    assert_eq!(h.count("SELECT COUNT(*) FROM usage_events"), 0);
    let (status, _) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b4", "g1", vec![json!({}); 2_000]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "exactly the limit is fine");
}

// -------------------------------------------------------------- heartbeat --

#[tokio::test]
async fn heartbeats_record_device_and_client_status() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let (status, body) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&device.ingest),
            serde_json::to_vec(&heartbeat(&device.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["minAppVersion"], "0.1.0");
    assert_eq!(
        h.count("SELECT backfill_complete FROM devices"),
        1,
        "backfill flag stored"
    );
    assert_eq!(h.count("SELECT last_scan_duration_ms FROM devices"), 620);
    assert_eq!(h.count("SELECT COUNT(*) FROM device_client_status"), 3);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM device_client_status WHERE client = 'zcode' AND error_code = 'parse_failed'"),
        1
    );

    let mut later = heartbeat(&device.id);
    later["clients"] = json!([{"client": "zcode", "detected": true}]);
    later["osName"] = Value::Null;
    h.post(
        "/api/ingest/heartbeat",
        Some(&device.ingest),
        gzip(&serde_json::to_vec(&later).expect("encode")),
        Some("gzip"),
    )
    .await;
    assert_eq!(
        h.count("SELECT COUNT(*) FROM device_client_status WHERE error_code IS NOT NULL"),
        0,
        "a cleared error is cleared"
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM devices WHERE os_name = 'macOS'"),
        1,
        "missing OS fields keep the last value"
    );

    let mut bad = heartbeat(&device.id);
    bad["clients"][0]["errorCode"] = json!("~/Library/secret path");
    let (status, _) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&device.ingest),
            serde_json::to_vec(&bad).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&device.read),
            serde_json::to_vec(&heartbeat(&device.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn each_device_has_an_ingest_budget() {
    let h = Harness::start().await;
    let busy = h.pair().await;
    let quiet = h.pair().await;
    let beat = serde_json::to_vec(&heartbeat(&busy.id)).expect("encode");
    for _ in 0..INGEST_REQUESTS_PER_WINDOW {
        let (status, _) = h
            .post(
                "/api/ingest/heartbeat",
                Some(&busy.ingest),
                beat.clone(),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let response = h
        .http
        .post(format!("{}/api/ingest/events", h.base))
        .bearer_auth(&busy.ingest)
        .json(&envelope(&busy.id, "b1", "g1", fixture_events()))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.headers().get("retry-after").map(|v| v.as_bytes()),
        Some(&b"60"[..])
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM usage_events"), 0);

    let (status, _) = h
        .ingest(
            &quiet.ingest,
            &envelope(&quiet.id, "b2", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "other devices are unaffected");

    // Bad credentials never consume a budget.
    let (status, _) = h
        .post("/api/ingest/heartbeat", Some("tdi_x"), beat.clone(), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    h.clock.advance_ms(INGEST_WINDOW_MS);
    let (status, _) = h
        .ingest(
            &busy.ingest,
            &envelope(&busy.id, "b1", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unknown_routes_are_not_found() {
    let h = Harness::start().await;
    let (status, _) = h.post("/api/ingest/other", None, Vec::new(), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let response = h
        .http
        .get(format!("{}/api/ingest/events", h.base))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
