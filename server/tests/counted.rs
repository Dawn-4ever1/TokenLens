//! Schema v6 (D047): the D029 dedup is stored per event and must always
//! agree with the rule worked out from scratch.

mod common;

use common::{Harness, envelope, fixture_events};
use reqwest::StatusCode;
use tokenlens_server::db;

async fn total(h: &Harness, token: &str) -> i64 {
    let (status, body) = h.get("/api/dashboard/summary", Some(token)).await;
    assert_eq!(status, StatusCode::OK);
    body["totalTokens"].as_i64().expect("total")
}

#[tokio::test]
async fn the_stored_flag_follows_uploads_revocation_and_deletion() {
    let h = Harness::start().await;
    let a = h.pair().await;
    let events = fixture_events();
    let (status, _) = h
        .ingest(&a.ingest, &envelope(&a.id, "batch-a", "g1", events.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let once = total(&h, &a.read).await;
    assert!(once > 0);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events WHERE counted = 0"),
        0
    );

    // A second device uploads the same events: still counted once, and the
    // older device keeps them.
    let b = h.pair().await;
    let (status, _) = h
        .ingest(&b.ingest, &envelope(&b.id, "batch-b", "g1", events.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let stored = h.count("SELECT COUNT(*) FROM usage_events");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events WHERE counted = 1") * 2,
        stored
    );
    assert_eq!(total(&h, &a.read).await, once);
    assert_eq!(db::counted_drift(&h.side_db()).expect("drift"), 0);

    // Revoking the older device hands its events to the newer one.
    let (status, _) = h
        .post(
            &format!("/api/dashboard/devices/{}/revoke", a.id),
            Some(&a.read),
            b"{}".to_vec(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(total(&h, &b.read).await, once);
    let side = h.side_db();
    let kept_by_b: i64 = side
        .query_row(
            "SELECT COUNT(*) FROM usage_events WHERE counted = 1 AND device_id = ?1",
            [&b.id],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(kept_by_b * 2, stored);
    assert_eq!(db::counted_drift(&side).expect("drift"), 0);

    // Deleting the counted copy makes the other copy count again.
    side.execute("DELETE FROM usage_events WHERE device_id = ?1", [&b.id])
        .expect("delete");
    assert_eq!(db::counted_drift(&side).expect("drift"), 0);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM usage_events WHERE counted = 1") * 2,
        stored
    );
}

#[tokio::test]
async fn revoking_device_with_unique_and_shared_events_keeps_zero_drift() {
    let h = Harness::start().await;
    let a = h.pair().await;
    let b = h.pair().await;
    let mut events = fixture_events();
    let shared_count = events.len();
    let mut unique_a = events[0].clone();
    unique_a["eventKey"] = serde_json::json!(format!("{:064x}", 0x101_u64));
    unique_a["sessionId"] = serde_json::json!("unique-a");
    let mut unique_b = events[0].clone();
    unique_b["eventKey"] = serde_json::json!(format!("{:064x}", 0x202_u64));
    unique_b["sessionId"] = serde_json::json!("unique-b");

    let mut batch_a = events.clone();
    batch_a.push(unique_a);
    let mut batch_b = std::mem::take(&mut events);
    batch_b.push(unique_b);

    let (status, _) = h
        .ingest(&a.ingest, &envelope(&a.id, "batch-a", "g1", batch_a))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .ingest(&b.ingest, &envelope(&b.id, "batch-b", "g1", batch_b))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = h
        .post(
            &format!("/api/dashboard/devices/{}/revoke", a.id),
            Some(&a.read),
            b"{}".to_vec(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let side = h.side_db();
    assert_eq!(db::counted_drift(&side).expect("drift"), 0);
    let b_counted: i64 = side
        .query_row(
            "SELECT COUNT(*) FROM usage_events WHERE counted = 1 AND device_id = ?1",
            [&b.id],
            |row| row.get(0),
        )
        .expect("b counted");
    assert_eq!(b_counted, (shared_count + 1) as i64);
    let a_counted: i64 = side
        .query_row(
            "SELECT COUNT(*) FROM usage_events WHERE counted = 1 AND device_id = ?1",
            [&a.id],
            |row| row.get(0),
        )
        .expect("a counted");
    assert_eq!(
        a_counted, 1,
        "revoked device keeps its unshared unique event"
    );
}
