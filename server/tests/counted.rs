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
