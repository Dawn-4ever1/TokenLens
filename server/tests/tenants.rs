//! Tenant isolation (T21): devices in one tenant share its dashboard; nothing
//! of another tenant is visible, changeable or countable through the API.
mod common;

use common::{Device, Harness, START_MS, envelope, heartbeat};
use reqwest::StatusCode;
use rusqlite::params;
use serde_json::{Value, json};
use tokenlens_server::clock::Clock;
use tokenlens_server::db::{self, DbError};
use tokenlens_server::secrets::{self, TokenKind};

const DASHBOARD: [&str; 7] = [
    "/api/dashboard/summary",
    "/api/dashboard/timeseries?groupBy=device",
    "/api/dashboard/heatmap",
    "/api/dashboard/sessions",
    "/api/dashboard/devices",
    "/api/dashboard/filters",
    "/api/dashboard/health",
];

fn utc(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .expect("timestamp")
        .timestamp_millis()
}

fn event(seed: &str, client: &str, session: &str, ts: &str, input: u64) -> Value {
    json!({
        "eventKey": secrets::sha256_hex(seed),
        "client": client,
        "sessionId": session,
        "modelId": "m1",
        "ts": utc(ts),
        "input": input,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "cacheWrite1h": 0,
        "reasoning": 0,
        "costSource": "synthetic",
        "precision": "measured",
        "messageCount": 1,
        "isTurnStart": true,
        "durationMs": 1000
    })
}

async fn upload(h: &Harness, device: &Device, batch: &str, events: Vec<Value>) -> Value {
    let (status, body) = h
        .ingest(&device.ingest, &envelope(&device.id, batch, "g1", events))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn beat(h: &Harness, device: &Device) {
    let (status, body) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&device.ingest),
            serde_json::to_vec(&heartbeat(&device.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn read(h: &Harness, path: &str, token: &str) -> Value {
    let (status, body) = h.get(path, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

fn keys(list: &Value, field: &str) -> Vec<String> {
    list.as_array()
        .expect("array")
        .iter()
        .map(|item| item[field].as_str().expect(field).to_owned())
        .collect()
}

struct Tenants {
    h: Harness,
    team: String,
    /// Personal tenant.
    a: Device,
    /// Team tenant.
    b: Device,
}

/// Both tenants upload an event with the same `event_key` (`shared`), in a
/// session with the same id, under the same batch id. Personal: 100 + 20
/// tokens of codex; team: 7000 tokens of claude, uploaded a minute later.
async fn tenants() -> Tenants {
    let h = Harness::start().await;
    let team = h.create_tenant("team-a");
    let a = h.pair().await;
    let b = h.pair_in(&team).await;
    let at = "2027-01-15T01:00:00Z";
    upload(
        &h,
        &a,
        "batch-1",
        vec![
            event("shared", "codex", "s1", at, 100),
            event("a-only", "codex", "s1", "2027-01-14T03:00:00Z", 20),
        ],
    )
    .await;
    beat(&h, &a).await;
    h.clock.advance_ms(60_000);
    let body = upload(
        &h,
        &b,
        "batch-1",
        vec![event("shared", "claude", "s1", at, 7000)],
    )
    .await;
    assert_eq!(
        body["accepted"], 1,
        "another tenant's copy is not a duplicate"
    );
    beat(&h, &b).await;
    Tenants { h, team, a, b }
}

#[tokio::test]
async fn each_tenant_sees_only_its_own_figures() {
    let t = tenants().await;
    let (h, a, b) = (&t.h, &t.a, &t.b);

    let summary = read(h, "/api/dashboard/summary", &a.read).await;
    assert_eq!(summary["totalTokens"], 120);
    assert_eq!(keys(&summary["byDevice"], "key"), [a.id.as_str()]);
    assert_eq!(keys(&summary["byClient"], "key"), ["codex"]);
    assert_eq!(summary["sessionCount"], 1);
    let summary = read(h, "/api/dashboard/summary", &b.read).await;
    assert_eq!(summary["totalTokens"], 7000);
    assert_eq!(keys(&summary["byDevice"], "key"), [b.id.as_str()]);
    assert_eq!(keys(&summary["byClient"], "key"), ["claude"]);
    assert_eq!(
        summary["range"]["from"], "2027-01-15",
        "the range starts at the tenant's own first event"
    );

    let series = read(h, "/api/dashboard/timeseries?groupBy=device", &a.read).await;
    assert_eq!(keys(&series["series"], "key"), [a.id.as_str()]);
    let series = read(h, "/api/dashboard/timeseries?groupBy=device", &b.read).await;
    assert_eq!(keys(&series["series"], "key"), [b.id.as_str()]);

    let heat_total = |m: &Value| {
        m["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["totalTokens"].as_i64().unwrap())
            .sum::<i64>()
    };
    assert_eq!(
        heat_total(&read(h, "/api/dashboard/heatmap", &a.read).await),
        120
    );
    assert_eq!(
        heat_total(&read(h, "/api/dashboard/heatmap", &b.read).await),
        7000
    );

    let sessions = read(h, "/api/dashboard/sessions", &a.read).await;
    assert_eq!(sessions["total"], 1, "{sessions}");
    assert_eq!(keys(&sessions["sessions"], "deviceId"), [a.id.as_str()]);
    let a_key = sessions["sessions"][0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();
    let sessions = read(h, "/api/dashboard/sessions", &b.read).await;
    assert_eq!(keys(&sessions["sessions"], "deviceId"), [b.id.as_str()]);
    let b_key = sessions["sessions"][0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();

    let devices = read(h, "/api/dashboard/devices", &a.read).await;
    assert_eq!(keys(&devices["devices"], "deviceId"), [a.id.as_str()]);
    assert_eq!(keys(&devices["usage"], "deviceId"), [a.id.as_str()]);
    let devices = read(h, "/api/dashboard/devices", &b.read).await;
    assert_eq!(keys(&devices["devices"], "deviceId"), [b.id.as_str()]);

    let filters = read(h, "/api/dashboard/filters", &a.read).await;
    assert_eq!(keys(&filters["devices"], "key"), [a.id.as_str()]);
    assert_eq!(keys(&filters["clients"], "key"), ["codex"]);
    assert_eq!(filters["firstEventDate"], "2027-01-14");
    let filters = read(h, "/api/dashboard/filters", &b.read).await;
    assert_eq!(keys(&filters["devices"], "key"), [b.id.as_str()]);
    assert_eq!(keys(&filters["clients"], "key"), ["claude"]);
    assert_eq!(filters["firstEventDate"], "2027-01-15");

    let health_a = read(h, "/api/dashboard/health", &a.read).await;
    let health_b = read(h, "/api/dashboard/health", &b.read).await;
    assert_eq!(
        (
            health_a["eventCount"].as_i64(),
            health_a["deviceCount"].as_i64()
        ),
        (Some(2), Some(1))
    );
    assert_eq!(
        (
            health_b["eventCount"].as_i64(),
            health_b["deviceCount"].as_i64()
        ),
        (Some(1), Some(1))
    );
    assert_ne!(
        health_a["lastIngestAt"], health_b["lastIngestAt"],
        "each tenant sees its own last upload"
    );
    assert!(health_a.get("databaseBytes").is_none());

    // Naming the other tenant's device or session finds nothing, exactly as
    // a device or session that never existed.
    for (token, foreign) in [(&a.read, &b.id), (&b.read, &a.id)] {
        let summary = read(
            h,
            &format!("/api/dashboard/summary?devices={foreign}"),
            token,
        )
        .await;
        assert_eq!(summary["totalTokens"], 0);
        let sessions = read(
            h,
            &format!("/api/dashboard/sessions?devices={foreign}"),
            token,
        )
        .await;
        assert_eq!(sessions["sessions"], json!([]));
    }
    let missing = tokenlens_server::dashboard::session_key("dev_missing", "codex", "s1");
    for (token, own, foreign) in [(&a.read, &a_key, &b_key), (&b.read, &b_key, &a_key)] {
        let path = |key: &str| format!("/api/dashboard/sessions/{key}");
        assert_eq!(h.get(&path(own), Some(token)).await.0, StatusCode::OK);
        assert_eq!(
            h.get(&path(foreign), Some(token)).await,
            h.get(&path(&missing), Some(token)).await
        );
        assert_eq!(
            h.get(&path(foreign), Some(token)).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn one_event_counts_once_per_tenant() {
    let t = tenants().await;
    let (h, a, b) = (&t.h, &t.a, &t.b);
    // A second computer in each tenant uploads the shared event again (D029).
    h.clock.advance_ms(60_000);
    let a2 = h.pair().await;
    let b2 = h.pair_in(&t.team).await;
    let at = "2027-01-15T01:00:00Z";
    upload(
        h,
        &a2,
        "batch-1",
        vec![event("shared", "codex", "s1", at, 100)],
    )
    .await;
    upload(
        h,
        &b2,
        "batch-1",
        vec![event("shared", "claude", "s1", at, 7000)],
    )
    .await;
    for (token, total, owner) in [(&a.read, 120, &a.id), (&b.read, 7000, &b.id)] {
        let summary = read(h, "/api/dashboard/summary", token).await;
        assert_eq!(summary["totalTokens"], total);
        assert_eq!(
            keys(&summary["byDevice"], "key"),
            [owner.as_str()],
            "the older device owns it"
        );
        let health = read(h, "/api/dashboard/health", token).await;
        assert_eq!(health["deviceCount"], 2);
    }
    // Revoking the owner hands the event to the tenant's other device, never
    // to the other tenant.
    let path = format!("/api/dashboard/devices/{}/revoke", a.id);
    assert_eq!(
        h.post(&path, Some(&a2.read), Vec::new(), None).await.0,
        StatusCode::OK
    );
    let summary = read(h, "/api/dashboard/summary", &a2.read).await;
    assert_eq!(summary["totalTokens"], 120);
    let summary = read(h, "/api/dashboard/summary", &b.read).await;
    assert_eq!(summary["totalTokens"], 7000);
    assert_eq!(keys(&summary["byDevice"], "key"), [b.id.as_str()]);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM ingest_batches WHERE batch_id = 'batch-1'"),
        4,
        "one batch id per device and tenant"
    );
}

#[tokio::test]
async fn other_tenants_devices_cannot_be_changed() {
    let t = tenants().await;
    let (h, a, b) = (&t.h, &t.a, &t.b);
    let a2 = h.pair().await;
    let post = |path: String, token: String, body: Vec<u8>| async move {
        h.post(&path, Some(&token), body, None).await
    };
    let rename = |id: &str| format!("/api/dashboard/devices/{id}/rename");
    let name = serde_json::to_vec(&json!({"displayName": "Mine now"})).unwrap();

    for verb in ["rename", "revoke"] {
        let path = |id: &str| format!("/api/dashboard/devices/{id}/{verb}");
        let body = if verb == "rename" {
            name.clone()
        } else {
            Vec::new()
        };
        let foreign = post(path(&b.id), a.read.clone(), body.clone()).await;
        let missing = post(path("dev_missing"), a.read.clone(), body).await;
        assert_eq!(
            foreign, missing,
            "{verb}: indistinguishable from a missing device"
        );
        assert_eq!(foreign.0, StatusCode::NOT_FOUND, "{verb}");
    }
    let foreign = post(
        format!("/api/dashboard/devices/{}/rotate", b.id),
        a.read.clone(),
        Vec::new(),
    )
    .await;
    let neighbour = post(
        format!("/api/dashboard/devices/{}/rotate", a2.id),
        a.read.clone(),
        Vec::new(),
    )
    .await;
    assert_eq!(
        foreign, neighbour,
        "only one's own tokens rotate, in any tenant"
    );
    assert_eq!(foreign.0, StatusCode::FORBIDDEN);

    // The team device is untouched and still works.
    let devices = read(h, "/api/dashboard/devices", &b.read).await;
    assert_eq!(devices["devices"][0]["displayName"], "Test Mac");
    beat(h, b).await;
    // Within a tenant the same actions still work.
    assert_eq!(
        post(rename(&a2.id), a.read.clone(), name.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        post(
            format!("/api/dashboard/devices/{}/revoke", a2.id),
            a.read.clone(),
            Vec::new()
        )
        .await
        .0,
        StatusCode::OK
    );
    // The operator CLI cannot be pointed at the wrong tenant either.
    let mut conn = h.side_db();
    let now = h.clock.now_ms();
    assert!(!db::rename_device(&conn, db::DEFAULT_TENANT_ID, &b.id, "x").unwrap());
    assert!(!db::revoke_device(&mut conn, db::DEFAULT_TENANT_ID, &b.id, now).unwrap());
    assert!(
        db::rotate_tokens(&mut conn, db::DEFAULT_TENANT_ID, &b.id, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db::device_tenant(&conn, &b.id).unwrap().as_deref(),
        Some(t.team.as_str())
    );
}

#[tokio::test]
async fn uploads_land_in_the_uploaders_tenant_only() {
    let t = tenants().await;
    let (h, a, b) = (&t.h, &t.a, &t.b);
    let a2 = h.pair().await;
    let batch = |device: &str| {
        envelope(
            device,
            "batch-x",
            "g1",
            vec![event("x", "codex", "s9", "2027-01-15T02:00:00Z", 5)],
        )
    };
    // Claiming to be a device of another tenant is refused exactly as
    // claiming to be another device of one's own tenant.
    let foreign = h.ingest(&b.ingest, &batch(&a.id)).await;
    let neighbour = h.ingest(&b.ingest, &batch(&a2.id)).await;
    assert_eq!(foreign.0, StatusCode::FORBIDDEN);
    assert_eq!(foreign, h.ingest(&a.ingest, &batch(&b.id)).await);
    assert_eq!(foreign.0, neighbour.0);
    let beat = heartbeat(&a.id);
    let (status, _) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&b.ingest),
            serde_json::to_vec(&beat).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let summary = read(h, "/api/dashboard/summary", &a.read).await;
    assert_eq!(summary["totalTokens"], 120, "nothing was written");
    // Every stored row sits in its device's tenant.
    for table in [
        "usage_events",
        "device_tokens",
        "device_client_status",
        "ingest_batches",
    ] {
        assert_eq!(
            h.count(&format!(
                "SELECT COUNT(*) FROM {table} x JOIN devices d ON d.id = x.device_id
                 WHERE x.tenant_id IS NOT d.tenant_id"
            )),
            0,
            "{table}"
        );
    }
    assert_eq!(
        h.count("SELECT COUNT(*) FROM device_client_status WHERE tenant_id = 'tenant_team-a'"),
        3
    );
}

#[tokio::test]
async fn pairing_codes_carry_their_tenant() {
    let t = tenants().await;
    let (h, a, b) = (&t.h, &t.a, &t.b);
    // A team device invites the next one; the newcomer shares the team view.
    let (status, body) = h
        .post("/api/pair/codes", Some(&b.read), b"{}".to_vec(), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let c = h.pair_with(body["code"].as_str().unwrap()).await;
    let devices = read(h, "/api/dashboard/devices", &c.read).await;
    assert_eq!(
        keys(&devices["devices"], "deviceId"),
        [c.id.clone(), b.id.clone()],
        "the caller first, then the rest of its tenant"
    );
    assert_eq!(
        read(h, "/api/dashboard/summary", &c.read).await["totalTokens"],
        7000
    );
    let devices = read(h, "/api/dashboard/devices", &a.read).await;
    assert_eq!(keys(&devices["devices"], "deviceId"), [a.id.as_str()]);

    // A device can only issue codes for its own tenant.
    let conn = h.side_db();
    let now = h.clock.now_ms();
    assert!(
        db::create_pair_code_for_tenant(&conn, now, db::DEFAULT_TENANT_ID, Some(&b.id)).is_err()
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM pair_codes WHERE tenant_id <> 'tenant_team-a' AND created_by_device_id IS NOT NULL"), 0);
    assert!(matches!(
        db::create_pair_code_for_tenant(&conn, now, "tenant_nobody", None),
        Err(DbError::NoSuchTenant(_))
    ));

    // Closing a tenant locks out its devices and its unused codes at once.
    let pending = h.issue_code_for(&t.team);
    conn.execute(
        "UPDATE tenants SET revoked_at = ?2 WHERE id = ?1",
        params![t.team, now],
    )
    .unwrap();
    for path in DASHBOARD {
        assert_eq!(
            h.get(path, Some(&b.read)).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    assert_eq!(
        h.ingest(&b.ingest, &envelope(&b.id, "late", "g1", vec![]))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(h.claim(&pending).await.status(), StatusCode::UNAUTHORIZED);
    assert!(matches!(
        db::create_pair_code_for_tenant(&conn, now, &t.team, None),
        Err(DbError::NoSuchTenant(_))
    ));
    assert_eq!(
        read(h, "/api/dashboard/summary", &a.read).await["totalTokens"],
        120
    );
}

fn raw_event(
    conn: &rusqlite::Connection,
    tenant: &str,
    device: &str,
    key: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO usage_events (tenant_id, device_id, event_key, client, session_id, model_id,
           ts, input, output, cache_read, cache_write, cost_source, precision,
           collector_version, tokscale_rev, scan_generation, ingested_at, updated_at)
         VALUES (?1, ?2, ?3, 'codex', 's', 'm', 0, 1, 0, 0, 0, 'x', 'measured', 'v', 'r', 'g', 0, 0)",
        params![tenant, device, key],
    )
}

#[tokio::test]
async fn the_schema_keeps_rows_in_their_devices_tenant() {
    let t = tenants().await;
    let (h, a, b) = (&t.h, &t.a, &t.b);
    let conn = h.side_db();
    let rejected = |result: rusqlite::Result<usize>, what: &str| {
        assert!(result.is_err(), "{what} was accepted");
    };
    rejected(
        raw_event(&conn, &t.team, &a.id, "k1"),
        "event in another tenant than its device",
    );
    rejected(
        conn.execute(
            "INSERT INTO usage_events (device_id, event_key, client, session_id, model_id, ts,
               input, output, cache_read, cache_write, cost_source, precision,
               collector_version, tokscale_rev, scan_generation, ingested_at, updated_at)
             VALUES (?1, 'k2', 'codex', 's', 'm', 0, 1, 0, 0, 0, 'x', 'measured', 'v', 'r', 'g', 0, 0)",
            params![a.id],
        ),
        "event without a tenant",
    );
    raw_event(&conn, db::DEFAULT_TENANT_ID, &a.id, "k3").expect("a consistent row is fine");
    rejected(
        conn.execute(
            "UPDATE usage_events SET tenant_id = ?1 WHERE device_id = ?2",
            params![t.team, a.id],
        ),
        "moving events",
    );
    rejected(
        conn.execute(
            "UPDATE usage_events SET device_id = ?1 WHERE device_id = ?2",
            params![b.id, a.id],
        ),
        "moving events to another tenant's device",
    );
    rejected(
        conn.execute(
            "UPDATE devices SET tenant_id = ?1 WHERE id = ?2",
            params![t.team, a.id],
        ),
        "moving a device",
    );
    rejected(
        conn.execute(
            "INSERT INTO devices (id, created_at, tenant_id) VALUES ('dev_nowhere', 0, 'tenant_nobody')",
            [],
        ),
        "device in a missing tenant",
    );
    rejected(
        conn.execute(
            "INSERT INTO devices (id, created_at) VALUES ('dev_nowhere', 0)",
            [],
        ),
        "device without a tenant",
    );
    rejected(
        conn.execute(
            "INSERT INTO device_tokens (id, device_id, kind, token_hash, created_at, tenant_id)
             VALUES ('tok_x', ?1, 'read', 'hash-x', 0, ?2)",
            params![a.id, t.team],
        ),
        "token in another tenant than its device",
    );
    rejected(
        conn.execute(
            "UPDATE device_tokens SET tenant_id = ?1 WHERE device_id = ?2",
            params![t.team, a.id],
        ),
        "moving tokens",
    );
    rejected(
        conn.execute(
            "INSERT INTO device_client_status (tenant_id, device_id, client, detected, updated_at)
             VALUES (?1, ?2, 'qoder', 1, 0)",
            params![t.team, a.id],
        ),
        "status in another tenant than its device",
    );
    rejected(
        conn.execute(
            "INSERT INTO ingest_batches (tenant_id, batch_id, device_id, received_at)
             VALUES (?1, 'bx', ?2, 0)",
            params![t.team, a.id],
        ),
        "batch in another tenant than its device",
    );
    rejected(
        conn.execute(
            "INSERT INTO pair_codes (code_hash, created_at, expires_at, created_by_device_id, tenant_id)
             VALUES ('code-x', 0, 1, ?1, ?2)",
            params![a.id, t.team],
        ),
        "code issued by a device into another tenant",
    );
    conn.execute(
        "INSERT INTO pair_codes (code_hash, created_at, expires_at, tenant_id) VALUES ('code-y', 0, 1, ?1)",
        params![t.team],
    )
    .expect("operator code");
    rejected(
        conn.execute(
            "UPDATE pair_codes SET claimed_device_id = ?1 WHERE code_hash = 'code-y'",
            params![a.id],
        ),
        "a team code claimed by a personal device",
    );
    rejected(
        conn.execute(
            "UPDATE pair_codes SET tenant_id = ?1 WHERE code_hash = 'code-y'",
            params![db::DEFAULT_TENANT_ID],
        ),
        "moving a code",
    );
    let dangling: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .ok();
    assert_eq!(dangling, None);
}

#[test]
fn tenant_names_are_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = db::open(&dir.path().join("server.sqlite"))
        .expect("open")
        .conn;
    let long = "a".repeat(33);
    for slug in [
        "",
        "Team",
        "team_a",
        "-team",
        "team-",
        "te--am",
        "团队",
        "a b",
        long.as_str(),
    ] {
        assert!(
            matches!(
                db::create_tenant(&conn, slug, "Name", 0),
                Err(DbError::InvalidTenant(_))
            ),
            "{slug:?}"
        );
    }
    let name = "名".repeat(65);
    for display in ["", "   ", "tab\there", name.as_str()] {
        assert!(
            matches!(
                db::create_tenant(&conn, "ok", display, 0),
                Err(DbError::InvalidTenant(_))
            ),
            "{display:?}"
        );
    }
    assert_eq!(
        db::create_tenant(&conn, "team-a", "  设计组  ", 5).unwrap(),
        "tenant_team-a"
    );
    assert_eq!(
        db::create_tenant(&conn, &"b".repeat(32), "B", 6)
            .unwrap()
            .len(),
        39
    );
    for taken in ["team-a", "personal"] {
        assert!(matches!(
            db::create_tenant(&conn, taken, "Again", 7),
            Err(DbError::TenantExists(_))
        ));
    }
    assert_eq!(
        db::resolve_tenant(&conn, "team-a").unwrap(),
        "tenant_team-a"
    );
    assert_eq!(
        db::resolve_tenant(&conn, "tenant_team-a").unwrap(),
        "tenant_team-a"
    );
    assert_eq!(
        db::resolve_tenant(&conn, "personal").unwrap(),
        db::DEFAULT_TENANT_ID
    );
    assert!(matches!(
        db::resolve_tenant(&conn, "nope"),
        Err(DbError::NoSuchTenant(_))
    ));
    let listed = db::list_tenants(&conn).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|t| (t.slug.as_str(), t.display_name.as_str(), t.active_devices))
            .collect::<Vec<_>>(),
        [
            ("personal", "Personal", 0),
            ("team-a", "设计组", 0),
            (&"b".repeat(32)[..], "B", 0)
        ]
    );
    conn.execute(
        "UPDATE tenants SET revoked_at = 9 WHERE slug = 'team-a'",
        [],
    )
    .unwrap();
    assert!(db::resolve_tenant(&conn, "team-a").is_err());
    assert_eq!(db::list_tenants(&conn).unwrap().len(), 2);
}

/// A v3 database as the production server holds it today: one device with
/// tokens, events, coverage, a batch and two unused codes.
#[tokio::test]
async fn upgrading_puts_existing_data_in_the_personal_tenant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let old = dir.path().join("v3.sqlite");
    let conn = db::open_with(&old, &db::MIGRATIONS[..3]).expect("v3").conn;
    let device = secrets::new_device_id();
    let read_token = secrets::new_token(TokenKind::Read);
    let ingest_token = secrets::new_token(TokenKind::Ingest);
    let code = secrets::new_pair_code();
    let ts = utc("2027-01-15T01:00:00Z");
    conn.execute(
        "INSERT INTO devices (id, display_name, platform, form_factor, created_at, last_seen_at)
         VALUES (?1, 'Old PC', 'windows', 'desktop', ?2, ?2)",
        params![device, START_MS - 86_400_000],
    )
    .unwrap();
    for (id, kind, token) in [
        ("tok_r", "read", &read_token),
        ("tok_i", "ingest", &ingest_token),
    ] {
        conn.execute(
            "INSERT INTO device_tokens (id, device_id, kind, token_hash, created_at) VALUES (?1, ?2, ?3, ?4, 0)",
            params![id, device, kind, secrets::token_hash(token.expose())],
        )
        .unwrap();
    }
    for (key, input) in [("old-1", 300), ("old-2", 45)] {
        conn.execute(
            "INSERT INTO usage_events (device_id, event_key, client, session_id, model_id, ts,
               input, output, cache_read, cache_write, cost_source, precision,
               collector_version, tokscale_rev, scan_generation, ingested_at, updated_at)
             VALUES (?1, ?2, 'codex', 's1', 'm1', ?3, ?4, 0, 0, 0, 'synthetic', 'measured',
               '0.1.0', 'e9cf104', 'g1', 0, 0)",
            params![device, secrets::sha256_hex(key), ts, input],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO device_client_status (device_id, client, detected, updated_at) VALUES (?1, 'codex', 1, 0)",
        params![device],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ingest_batches (batch_id, device_id, received_at, accepted) VALUES ('batch-1', ?1, ?2, 2)",
        params![device, START_MS - 1000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pair_codes (code_hash, created_at, expires_at, created_by_device_id)
         VALUES (?1, ?2, ?3, ?4), ('cli-code', ?2, ?3, NULL)",
        params![
            secrets::pair_code_hash(&code),
            START_MS,
            START_MS + 600_000,
            device
        ],
    )
    .unwrap();
    drop(conn);

    let h = Harness::upgraded_from(&old).await;
    for table in [
        "devices",
        "device_tokens",
        "usage_events",
        "device_client_status",
        "ingest_batches",
        "pair_codes",
    ] {
        assert_eq!(
            h.count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE tenant_id IS NOT 'tenant_personal'"
            )),
            0,
            "{table}"
        );
    }
    assert_eq!(
        h.count("SELECT COUNT(*) FROM ingest_batches"),
        1,
        "batches carried over"
    );
    let dangling: Option<String> = h
        .side_db()
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .ok();
    assert_eq!(dangling, None);

    // The paired computer keeps working without re-pairing.
    let summary = read(&h, "/api/dashboard/summary", read_token.expose()).await;
    assert_eq!(summary["totalTokens"], 345);
    assert_eq!(
        read(&h, "/api/dashboard/health", read_token.expose()).await["schemaVersion"],
        5
    );
    let old_pc = Device {
        id: device.clone(),
        ingest: ingest_token.expose().to_owned(),
        read: read_token.expose().to_owned(),
    };
    let body = upload(
        &h,
        &old_pc,
        "batch-1",
        vec![event("old-3", "codex", "s1", "2027-01-15T02:00:00Z", 5)],
    )
    .await;
    assert_eq!(body["accepted"], 1);
    // Its outstanding code still adds a device to the same (personal) space,
    // and a new team stays apart from it.
    let joined = h
        .pair_with(&secrets::format_pair_code(&code).into_inner())
        .await;
    let devices = read(&h, "/api/dashboard/devices", &joined.read).await;
    assert_eq!(
        keys(&devices["devices"], "deviceId"),
        [joined.id.clone(), device.clone()],
        "the caller first, then the rest of its tenant"
    );
    let team = h.create_tenant("team-a");
    let outsider = h.pair_in(&team).await;
    assert_eq!(
        read(&h, "/api/dashboard/summary", &outsider.read).await["totalTokens"],
        0
    );
    assert_eq!(
        read(&h, "/api/dashboard/summary", &joined.read).await["totalTokens"],
        350
    );
}

#[tokio::test]
async fn backups_restore_every_tenant_apart() {
    let t = tenants().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("backup.sqlite");
    let summary = db::backup_to(&t.h.db_path, &dest).expect("backup");
    assert_eq!((summary.active_devices, summary.events), (2, 3));

    let restored = Harness::restored_from(&dest).await;
    restored.clock.advance_ms(60_000);
    for token in [&t.a.read, &t.b.read] {
        for path in [
            "/api/dashboard/summary",
            "/api/dashboard/devices",
            "/api/dashboard/sessions",
        ] {
            let mut before = read(&t.h, path, token).await;
            let mut after = read(&restored, path, token).await;
            for value in [&mut before, &mut after] {
                value.as_object_mut().unwrap().remove("generatedAt");
            }
            assert_eq!(before, after, "{path}");
        }
    }
    let target = dir.path().join("server.sqlite");
    let put_back = db::restore_from(&dest, &target).expect("restore");
    assert_eq!(put_back.summary.active_devices, 2);

    // A copy whose rows point at a deleted device is refused.
    let broken = dir.path().join("broken.sqlite");
    std::fs::copy(&dest, &broken).unwrap();
    let conn = rusqlite::Connection::open(&broken).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute("DELETE FROM devices WHERE id = ?1", params![t.b.id])
        .unwrap();
    drop(conn);
    match db::verify_backup(&broken) {
        Err(DbError::Corrupt(reason)) => assert!(reason.contains("dangling"), "{reason}"),
        other => panic!("accepted a broken copy: {:?}", other.map(|_| ())),
    }
    assert!(db::restore_from(&broken, &dir.path().join("other.sqlite")).is_err());
}
