//! A real server on a loopback port, a temp database and a manual clock.
#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use tokenlens_server::clock::{Clock, ManualClock};
use tokenlens_server::db;
use tokenlens_server::http::AppState;
use tokenlens_server::secrets;

pub const START_MS: i64 = 1_800_000_000_000;

pub struct Harness {
    pub base: String,
    pub clock: ManualClock,
    pub db_path: PathBuf,
    pub http: reqwest::Client,
    /// The running server's state, for installing a price catalog.
    pub state: AppState,
    _dir: tempfile::TempDir,
}

pub struct Device {
    pub id: String,
    pub ingest: String,
    pub read: String,
}

impl Harness {
    pub async fn start() -> Self {
        Self::start_in(tokenlens_server::DEFAULT_TIME_ZONE).await
    }

    pub async fn start_in(time_zone: chrono_tz::Tz) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("server.sqlite");
        let opened = db::open(&db_path).expect("open database");
        assert!(opened.backup.is_none(), "a new database needs no backup");
        Self::serve(dir, db_path, opened.conn, time_zone).await
    }

    /// A server on a copy of `file`, as after restoring a backup.
    pub async fn restored_from(file: &std::path::Path) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("server.sqlite");
        std::fs::copy(file, &db_path).expect("restore copy");
        let opened = db::open(&db_path).expect("open restored database");
        assert!(opened.backup.is_none(), "same schema, nothing to migrate");
        Self::serve(
            dir,
            db_path,
            opened.conn,
            tokenlens_server::DEFAULT_TIME_ZONE,
        )
        .await
    }

    /// A server on a copy of `file` written by an older schema, as after
    /// deploying a new server version onto the existing database.
    pub async fn upgraded_from(file: &std::path::Path) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("server.sqlite");
        std::fs::copy(file, &db_path).expect("copy");
        let opened = db::open(&db_path).expect("open and migrate");
        assert!(opened.backup.is_some(), "a migration backs up first");
        Self::serve(
            dir,
            db_path,
            opened.conn,
            tokenlens_server::DEFAULT_TIME_ZONE,
        )
        .await
    }

    async fn serve(
        dir: tempfile::TempDir,
        db_path: PathBuf,
        conn: rusqlite::Connection,
        time_zone: chrono_tz::Tz,
    ) -> Self {
        let clock = ManualClock::new(START_MS);
        let state = AppState::new(conn, Arc::new(clock.clone())).with_time_zone(time_zone);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        tokio::spawn(tokenlens_server::serve(
            listener,
            state.clone(),
            std::future::pending(),
        ));
        Self {
            base,
            clock,
            db_path,
            http: reqwest::Client::new(),
            state,
            _dir: dir,
        }
    }

    /// A second connection, as the operator CLI would open.
    pub fn side_db(&self) -> rusqlite::Connection {
        db::open(&self.db_path).expect("open database").conn
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.side_db()
            .query_row(sql, [], |row| row.get(0))
            .expect("count")
    }

    /// What the operator CLI does for the first device.
    pub fn issue_code(&self) -> String {
        let issued =
            db::create_pair_code(&self.side_db(), self.clock.now_ms(), None).expect("issue code");
        secrets::format_pair_code(&issued.code).into_inner()
    }

    pub async fn claim(&self, code: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/api/pair/claim", self.base))
            .json(&json!({
                "code": code,
                "platform": "macos",
                "formFactor": "laptop",
                "displayName": "Test Mac"
            }))
            .send()
            .await
            .expect("send claim")
    }

    pub async fn pair_with(&self, code: &str) -> Device {
        let response = self.claim(code).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .map(|v| v.as_bytes()),
            Some(&b"no-store"[..])
        );
        let body = response.json::<Value>().await.expect("claim body");
        let text = |name: &str| body[name].as_str().expect(name).to_owned();
        Device {
            id: text("deviceId"),
            ingest: text("ingestToken"),
            read: text("readToken"),
        }
    }

    pub async fn pair(&self) -> Device {
        let code = self.issue_code();
        self.pair_with(&code).await
    }

    /// What `tenant create` does; returns the tenant id.
    pub fn create_tenant(&self, slug: &str) -> String {
        db::create_tenant(&self.side_db(), slug, slug, self.clock.now_ms()).expect("tenant")
    }

    /// What `pair-code create --tenant <id>` does.
    pub fn issue_code_for(&self, tenant_id: &str) -> String {
        let issued =
            db::create_pair_code_for_tenant(&self.side_db(), self.clock.now_ms(), tenant_id, None)
                .expect("issue code");
        secrets::format_pair_code(&issued.code).into_inner()
    }

    pub async fn pair_in(&self, tenant_id: &str) -> Device {
        let code = self.issue_code_for(tenant_id);
        self.pair_with(&code).await
    }

    pub async fn post(
        &self,
        path: &str,
        token: Option<&str>,
        body: Vec<u8>,
        encoding: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .http
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .body(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(encoding) = encoding {
            request = request.header("content-encoding", encoding);
        }
        let response = request.send().await.expect("send");
        let status = response.status();
        let bytes = response.bytes().await.expect("body");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut request = self.http.get(format!("{}{path}", self.base));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.expect("send");
        let status = response.status();
        let bytes = response.bytes().await.expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub async fn ingest(&self, token: &str, envelope: &Value) -> (StatusCode, Value) {
        self.post(
            "/api/ingest/events",
            Some(token),
            serde_json::to_vec(envelope).expect("encode"),
            None,
        )
        .await
    }
}

pub fn fixture_events() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../fixtures/usage-events.json")).expect("fixture")
}

pub fn envelope(device_id: &str, batch_id: &str, generation: &str, events: Vec<Value>) -> Value {
    json!({
        "schemaVersion": 1,
        "deviceId": device_id,
        "batchId": batch_id,
        "collector": {
            "version": "0.1.0",
            "tokscaleRev": "e9cf104",
            "parserGeneration": generation
        },
        "scanGeneration": generation,
        "events": events
    })
}

pub fn heartbeat(device_id: &str) -> Value {
    json!({
        "schemaVersion": 1,
        "deviceId": device_id,
        "collector": {"version": "0.1.0", "tokscaleRev": "e9cf104", "parserGeneration": "g1"},
        "osName": "macOS",
        "osVersion": "15.6",
        "lastScanAt": START_MS - 5_000,
        "lastScanDurationMs": 620,
        "outboxBacklog": 0,
        "backfillComplete": true,
        "clients": [
            {"client": "codex", "detected": true, "lastEventAt": START_MS - 60_000, "lastScanEvents": 12},
            {"client": "claude", "detected": false},
            {"client": "zcode", "detected": true, "errorCode": "parse_failed"}
        ]
    })
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("compress");
    encoder.finish().expect("finish")
}
