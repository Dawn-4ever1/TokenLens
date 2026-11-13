//! Email-code accounts, one personal space per account, replaceable persistent
//! pairing codes, and the separation between login sessions and device tokens
//! (D049).

mod common;

use common::{Harness, envelope};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokenlens_server::secrets;

fn utc(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .expect("timestamp")
        .timestamp_millis()
}

fn event(seed: &str, input: u64) -> Value {
    json!({
        "eventKey": secrets::sha256_hex(seed),
        "client": "codex",
        "sessionId": "account-synthetic-session",
        "modelId": "m1",
        "ts": utc("2026-11-01T10:00:00Z"),
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

fn summary_path() -> &'static str {
    "/api/dashboard/summary?from=2026-11-01&to=2026-11-30"
}

#[tokio::test]
async fn one_email_creates_one_space_and_signing_in_again_returns_to_it() {
    let h = Harness::start().await;
    let (session_a, status, body) = h.login("owner@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["email"], "owner@example.com");
    assert!(!session_a.is_empty(), "a session is issued");
    assert!(session_a.starts_with("tls_"));

    // Signing in again with the same address reuses the space.
    let (session_b, status, body) = h.login("OWNER@example.com ").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(session_a, session_b, "each sign-in gets its own session");

    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 1);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM tenants WHERE id <> 'tenant_personal'"),
        1,
        "one personal space per account"
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM account_sessions WHERE revoked_at IS NULL"),
        2
    );
}

#[tokio::test]
async fn two_accounts_never_see_each_others_figures() {
    let h = Harness::start().await;
    let (alpha, status, body) = h.login("alpha@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let beta = h.login("beta@example.com").await.0;
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 2);

    let alpha_device = h.add_device_with_session(&alpha).await;
    let beta_device = h.add_device_with_session(&beta).await;
    let (status, body) = h
        .ingest(
            &alpha_device.ingest,
            &envelope(
                &alpha_device.id,
                "a1",
                "g1",
                vec![event("alpha-event", 500)],
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, alpha_summary) = h.get(summary_path(), Some(&alpha_device.read)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alpha_summary["inputTokens"], 500);
    assert_eq!(alpha_summary["byDevice"].as_array().unwrap().len(), 1);

    let (status, beta_summary) = h.get(summary_path(), Some(&beta_device.read)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(beta_summary["inputTokens"], 0, "no other space's events");
    assert_eq!(beta_summary["byDevice"].as_array().unwrap().len(), 0);

    // The two devices also live in different tenants, not one shared space.
    assert_eq!(
        h.count("SELECT COUNT(DISTINCT tenant_id) FROM devices WHERE revoked_at IS NULL"),
        2
    );
}

#[tokio::test]
async fn a_wrong_expired_or_reused_code_never_signs_in() {
    let h = Harness::start().await;
    let code = h.stage_login_code("careful@example.com");

    let (status, body) = h
        .post(
            "/api/auth/login",
            None,
            serde_json::to_vec(&json!({"email": "careful@example.com", "code": "000000"})).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, body) = h
        .post(
            "/api/auth/login",
            None,
            serde_json::to_vec(&json!({"email": "careful@example.com", "code": code})).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 1);

    // The same code cannot be replayed.
    let (status, body) = h
        .post(
            "/api/auth/login",
            None,
            serde_json::to_vec(&json!({"email": "careful@example.com", "code": code})).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    // An expired code is refused and removed.
    let expired = h.stage_login_code("late@example.com");
    h.clock.advance_ms(11 * 60 * 1000);
    let (status, body) = h
        .post(
            "/api/auth/login",
            None,
            serde_json::to_vec(&json!({"email": "late@example.com", "code": expired})).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM email_challenges WHERE email = 'late@example.com'"),
        0
    );
}

#[tokio::test]
async fn the_persistent_code_adds_devices_and_rotation_retires_the_old_one() {
    let h = Harness::start().await;
    let (session, status, body) = h.login("team@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, first) = h.account_pair_code(&session, false).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let code = first["code"].as_str().expect("code").to_owned();
    assert_eq!(first["persistent"], true);

    // Showing it again returns the same code.
    let (status, again) = h.account_pair_code(&session, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["code"], code.as_str());

    // Two more computers join the same space with that one code.
    let first_device = h.pair_with(&code).await;
    let second_device = h.pair_with(&code).await;
    assert_eq!(
        h.count("SELECT COUNT(DISTINCT tenant_id) FROM devices WHERE revoked_at IS NULL"),
        1
    );
    let (status, body) = h
        .ingest(
            &first_device.ingest,
            &envelope(&first_device.id, "b1", "g1", vec![event("team-1", 40)]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, summary) = h.get(summary_path(), Some(&second_device.read)).await;
    assert_eq!(
        summary["inputTokens"], 40,
        "both computers share the space's figures"
    );

    // Rotating replaces it: the old code stops working at once.
    let (status, rotated) = h.account_pair_code(&session, true).await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let new_code = rotated["code"].as_str().expect("new code").to_owned();
    assert_ne!(new_code, code);
    assert_eq!(
        h.claim(&code).await.status(),
        StatusCode::UNAUTHORIZED,
        "the retired code is refused"
    );
    h.pair_with(&new_code).await;
    assert_eq!(
        h.count("SELECT COUNT(DISTINCT tenant_id) FROM devices WHERE revoked_at IS NULL"),
        1,
        "the new code still joins the same space"
    );
}

#[tokio::test]
async fn a_login_session_cannot_upload_or_read_usage() {
    let h = Harness::start().await;
    let (session, status, body) = h.login("guard@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let device = h.add_device_with_session(&session).await;

    let (status, body) = h
        .ingest(
            &session,
            &envelope(&device.id, "c1", "g1", vec![event("x", 1)]),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _) = h.get(summary_path(), Some(&session)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a session is not a read token"
    );

    // Device tokens stay separate too: they cannot act as an account.
    let (status, _) = h.account_pair_code(&device.read, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pairing_code_and_device_endpoints_need_a_login() {
    let h = Harness::start().await;
    for path in ["/api/account/pair-code", "/api/account/devices"] {
        let (status, _) = h
            .post(path, None, serde_json::to_vec(&json!({})).unwrap(), None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let (status, _) = h.get("/api/auth/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Logging out revokes the session for every later request.
    let (session, status, body) = h.login("bye@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h
        .post(
            "/api/auth/logout",
            Some(&session),
            serde_json::to_vec(&json!({})).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = h.get("/api/auth/me", Some(&session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_retired_session_no_longer_opens_the_space() {
    let h = Harness::start().await;
    let (session, status, body) = h.login("expire@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = h.get("/api/auth/me", Some(&session)).await;
    assert_eq!(status, StatusCode::OK);

    h.clock
        .advance_ms(tokenlens_server::accounts::SESSION_TTL_MS + 1);
    let (status, _) = h.get("/api/auth/me", Some(&session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_database_never_stores_a_plain_login_or_pairing_secret() {
    let h = Harness::start().await;
    let (session, status, body) = h.login("private@example.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, issued) = h.account_pair_code(&session, false).await;
    let code = issued["code"].as_str().expect("code");

    let conn = h.side_db();
    let sessions: Vec<String> = conn
        .prepare("SELECT token_hash FROM account_sessions")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(sessions.iter().all(|hash| hash != &session));

    let stored: Vec<String> = conn
        .prepare("SELECT code_hash || '|' || encrypted_code FROM tenant_pair_secrets")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!stored.is_empty());
    assert!(
        stored
            .iter()
            .all(|value| !value.contains(&code.replace('-', ""))),
        "neither the hash nor the ciphertext contains the code"
    );
}

#[test]
fn upgrading_to_v7_keeps_existing_devices_and_usage_in_the_personal_space() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("server.sqlite");
    // A database exactly as the deployed v6 server leaves it.
    let v6 =
        tokenlens_server::db::open_with(&path, &tokenlens_server::db::MIGRATIONS[..6]).expect("v6");
    let code = tokenlens_server::db::create_pair_code(&v6.conn, 1, None).expect("code");
    let issued = tokenlens_server::secrets::format_pair_code(&code.code).into_inner();
    let mut conn = v6.conn;
    let claimed = tokenlens_server::db::claim_pair_code(
        &mut conn,
        &tokenlens_server::secrets::pair_code_hash(
            &tokenlens_server::secrets::normalize_pair_code(&issued).expect("normalize"),
        ),
        2,
        &tokenlens_server::db::NewDevice {
            platform: "macos",
            form_factor: "laptop",
            display_name: "Old Mac",
        },
    )
    .expect("claim")
    .expect("a valid code claims");
    conn.execute(
        "INSERT INTO usage_events
           (device_id, event_key, client, session_id, model_id, ts, input, output,
            cache_read, cache_write, cache_write_1h, reasoning, cost_source, precision,
            message_count, is_turn_start, collector_version, tokscale_rev,
            scan_generation, ingested_at, updated_at, tenant_id)
         VALUES (?1, 'old-event', 'codex', 's', 'm1', 10, 7, 0, 0, 0, 0, 0,
                 'synthetic', 'measured', 1, 1, '0.5.1', 'synthetic', 'g1', 10, 10,
                 'tenant_personal')",
        rusqlite::params![claimed.device_id],
    )
    .expect("old event");
    drop(conn);

    let upgraded = tokenlens_server::db::open(&path).expect("upgrade to v7");
    assert!(upgraded.backup.is_some(), "a migration backs up first");
    assert_eq!(
        tokenlens_server::db::schema_version(&upgraded.conn).expect("version"),
        tokenlens_server::db::MIGRATIONS.last().unwrap().version
    );
    let count = |sql: &str| -> i64 {
        upgraded
            .conn
            .query_row(sql, [], |row| row.get(0))
            .expect(sql)
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM devices WHERE tenant_id = 'tenant_personal'"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM usage_events WHERE tenant_id = 'tenant_personal'"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM accounts"),
        0,
        "no account is invented"
    );
    assert_eq!(count("SELECT COUNT(*) FROM tenant_pair_secrets"), 0);
    // The old device tokens still authenticate after the upgrade.
    let auth = tokenlens_server::db::authenticate(
        &upgraded.conn,
        claimed.read_token.expose(),
        tokenlens_server::secrets::TokenKind::Read,
        3,
    )
    .expect("authenticate");
    assert_eq!(
        auth.map(|context| context.tenant_id).as_deref(),
        Some("tenant_personal")
    );
}
