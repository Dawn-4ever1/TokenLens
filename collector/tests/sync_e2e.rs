//! End-to-end: pair with a real local server, watch a synthetic home, upload,
//! restart without re-sending, stop on a revoked device, and re-send after
//! the server database is restored from a backup.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use collector_core::credentials::{self, Credentials};
use collector_core::schedule::Timing;
use collector_core::sync::{self, SyncTiming, Syncer};
use collector_core::transport::UreqTransport;
use collector_core::watch::{self, Msg, WatchOptions};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokenlens_server::clock::SystemClock;
use tokenlens_server::db;
use tokenlens_server::http::AppState;
use tokenlens_server::secrets;
use usage_event::envelope::FormFactor;

mod common;
use common::*;

struct Server {
    base: String,
    db_path: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: Option<tempfile::TempDir>,
}

impl Server {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("server dir");
        Self::start_at(dir.path().join("server.sqlite"), Some(dir))
    }

    fn start_at(db_path: PathBuf, dir: Option<tempfile::TempDir>) -> Self {
        let opened = db::open(&db_path).expect("open database");
        let state = AppState::new(opened.conn, Arc::new(SystemClock));
        let (address_sender, address) = std::sync::mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind");
                address_sender
                    .send(listener.local_addr().expect("address"))
                    .expect("send address");
                let shutdown = async {
                    let _ = stopped.await;
                };
                tokenlens_server::serve(listener, state, shutdown)
                    .await
                    .expect("serve");
            });
        });
        let address = address.recv().expect("server address");
        Self {
            base: format!("http://{address}"),
            db_path,
            stop: Some(stop),
            thread: Some(thread),
            _dir: dir,
        }
    }

    /// Stops serving and closes the database, keeping its directory.
    fn shut_down(mut self) -> Option<tempfile::TempDir> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("server thread");
        }
        self._dir.take()
    }

    fn conn(&self) -> Conn {
        Conn(db::open(&self.db_path).expect("open").conn)
    }

    fn issue_code(&self) -> String {
        let now = chrono::Utc::now().timestamp_millis();
        let issued = db::create_pair_code(&self.conn().0, now, None).expect("code");
        secrets::format_pair_code(&issued.code).into_inner()
    }
}

struct Conn(rusqlite::Connection);

impl Conn {
    fn count(&self, sql: &str) -> i64 {
        self.0.query_row(sql, [], |row| row.get(0)).expect("count")
    }

    fn keys(&self) -> Vec<String> {
        let mut statement = self
            .0
            .prepare("SELECT event_key FROM usage_events ORDER BY event_key")
            .expect("prepare");
        statement
            .query_map([], |row| row.get(0))
            .expect("query")
            .map(|key| key.expect("key"))
            .collect()
    }
}

fn key_set_sha256(keys: &[String]) -> String {
    let mut hasher = Sha256::new();
    for key in keys {
        hasher.update(key.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// One `collector run` lifetime, in-process, with fast timings.
fn run_once(home: &Path, state_dir: &Path, paired: &Credentials, seconds: u64) -> Vec<Value> {
    let options = WatchOptions {
        home: home.to_owned(),
        explicit_home: true,
        use_env_roots: false,
        worker_program: PathBuf::from(BIN),
        timing: Timing {
            debounce: Duration::from_millis(200),
            max_delay: Duration::from_millis(1000),
            cooldown: Duration::from_millis(300),
            ..Timing::default()
        },
        worker_timeout: Duration::from_secs(60),
        exit_after: Some(Duration::from_secs(seconds)),
        ignored: vec![state_dir.to_owned()],
        upstream_cache_dir: None,
    };
    let (sender, receiver) = watch::channel();
    let sink = sender.clone();
    let mut syncer = Syncer::start(
        state_dir,
        paired.clone(),
        Box::new(UreqTransport::new()),
        SyncTiming {
            min_round_gap: Duration::from_millis(200),
            heartbeat_interval: Duration::from_secs(60),
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(2),
        },
        Box::new(move |line| {
            let _ = sink.send(Msg::Sync(line));
        }),
    )
    .expect("syncer");
    let mut out = Vec::new();
    let summary = watch::run(&options, sender, receiver, &mut out, Some(&mut syncer));
    syncer.stop();
    let text = String::from_utf8(out).expect("utf8");
    let home_name = home.file_name().expect("name").to_string_lossy();
    assert!(!text.contains(home_name.as_ref()), "no path is printed");
    assert!(!text.contains(&paired.ingest_token), "no token is printed");
    let mut lines = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json line"))
        .collect::<Vec<_>>();
    lines.push(serde_json::json!({ "summary": summary }));
    lines
}

fn sum(lines: &[Value], sync: &str, field: &str) -> u64 {
    lines
        .iter()
        .filter(|line| line["sync"] == sync)
        .filter_map(|line| line[field].as_u64())
        .sum()
}

#[test]
fn upload_restart_and_revoke() {
    let server = Server::start();
    let paired = credentials::claim(
        &UreqTransport::new(),
        &server.base,
        &server.issue_code(),
        FormFactor::Laptop,
        "Mac laptop",
    )
    .expect("pair");
    assert!(paired.ingest_token.starts_with("tdi_"));

    let temp = tempfile::tempdir().expect("home");
    let home = temp.path().join("home");
    let state_dir = temp.path().join("state");
    let files = paths(&home);
    append(&files.claude, &[claude(1), claude(2)]);
    let mut codex = codex_header();
    codex.push(codex_count(100, 100));
    append(&files.codex, &codex);
    append(&files.qoder, &[qoder_fragment(1, 0.5)]);
    append(&files.mirror, &[qoder_fragment(9, 7.0)]);

    // First run: backfill everything.
    let first = run_once(&home, &state_dir, &paired, 5);
    let seed = first
        .iter()
        .find(|line| line["state"] == "seed")
        .expect("seed");
    assert_eq!(seed["known"], 0);
    assert_eq!(seed["deviceChanged"], true);
    assert_eq!(sum(&first, "upload", "accepted"), 4);
    assert_eq!(sum(&first, "upload", "rejected"), 0);
    assert!(
        first
            .iter()
            .any(|line| line["sync"] == "heartbeat" && line["ok"] == true)
    );
    let conn = server.conn();
    assert_eq!(conn.count("SELECT count(*) FROM usage_events"), 4);
    assert_eq!(
        conn.count("SELECT count(*) FROM usage_events WHERE client = 'qoder-intl' AND precision = 'estimated'"),
        1
    );
    assert!(conn.count("SELECT count(*) FROM device_client_status WHERE detected = 1") >= 3);
    let local = sync::local_status(&state_dir)
        .expect("status")
        .expect("exists");
    assert_eq!(local.outbox_backlog, 0);
    assert!(local.backfill_complete);
    assert_eq!(local.known_events, 4);
    assert_eq!(local.upload_state.as_deref(), Some("ok"));

    // Restart after new and changed events: only those are sent.
    append(&files.claude, &[claude(3)]);
    append(&files.codex, &[codex_count(250, 150)]);
    append(&files.qoder, &[qoder_fragment(2, 0.75)]);
    let second = run_once(&home, &state_dir, &paired, 5);
    let seed = second
        .iter()
        .find(|line| line["state"] == "seed")
        .expect("seed");
    assert_eq!(seed["known"], 4);
    assert_eq!(seed["deviceChanged"], false);
    assert_eq!(
        sum(&second, "queued", "events"),
        3,
        "claude 3, codex 2, qoder merged"
    );
    assert_eq!(sum(&second, "upload", "accepted"), 2);
    assert_eq!(sum(&second, "upload", "updated"), 1);
    assert_eq!(sum(&second, "upload", "duplicates"), 0);
    let full = full_scan_digest(&home);
    assert_eq!(full["event_count"], 6);
    let keys = conn.keys();
    assert_eq!(keys.len(), 6);
    assert_eq!(Value::from(key_set_sha256(&keys)), full["key_set_sha256"]);
    // Fragments of one Qoder message keep the maximum, not the sum.
    assert_eq!(
        conn.count("SELECT CAST(round(sum(credits) * 100) AS INTEGER) FROM usage_events"),
        75
    );

    // A revoked device stops uploading and keeps its queue.
    let mut raw = db::open(&server.db_path).expect("open").conn;
    let now = chrono::Utc::now().timestamp_millis();
    assert!(
        db::revoke_device(&mut raw, db::DEFAULT_TENANT_ID, &paired.device_id, now).expect("revoke")
    );
    append(&files.claude, &[claude(4)]);
    let third = run_once(&home, &state_dir, &paired, 4);
    assert!(
        third.iter().any(|line| line["state"] == "pairing_required"),
        "{third:?}"
    );
    assert_eq!(sum(&third, "upload", "accepted"), 0);
    assert_eq!(conn.count("SELECT count(*) FROM usage_events"), 6);
    let local = sync::local_status(&state_dir)
        .expect("status")
        .expect("exists");
    assert_eq!(local.outbox_backlog, 1);
    assert_eq!(local.upload_state.as_deref(), Some("pairing_required"));
}

#[test]
fn a_restored_server_gets_the_history_again() {
    let server = Server::start();
    let mut paired = credentials::claim(
        &UreqTransport::new(),
        &server.base,
        &server.issue_code(),
        FormFactor::Laptop,
        "Mac laptop",
    )
    .expect("pair");
    let temp = tempfile::tempdir().expect("home");
    let home = temp.path().join("home");
    let state_dir = temp.path().join("state");
    let backup = temp.path().join("before-upload.sqlite");
    let files = paths(&home);
    append(&files.claude, &[claude(1), claude(2)]);
    let mut codex = codex_header();
    codex.push(codex_count(100, 100));
    append(&files.codex, &codex);

    // Backed up after pairing, before any event arrived.
    db::backup_to(&server.db_path, &backup).expect("backup");
    let first = run_once(&home, &state_dir, &paired, 4);
    assert_eq!(sum(&first, "upload", "accepted"), 3);
    assert!(!first.iter().any(|line| line["resync"] == true));

    let db_path = server.db_path.clone();
    let dir = server.shut_down();
    let restored = db::restore_from(&backup, &db_path).expect("restore");
    assert_eq!(restored.summary.events, 0);
    let server = Server::start_at(db_path, dir);
    paired.server_url = server.base.clone();

    append(&files.claude, &[claude(3)]);
    let second = run_once(&home, &state_dir, &paired, 6);
    assert!(
        second.iter().any(|line| line["resync"] == true),
        "{second:?}"
    );
    let full = full_scan_digest(&home);
    assert_eq!(full["event_count"], 4);
    let keys = server.conn().keys();
    assert_eq!(Value::from(key_set_sha256(&keys)), full["key_set_sha256"]);
    let local = sync::local_status(&state_dir)
        .expect("status")
        .expect("exists");
    assert_eq!(local.outbox_backlog, 0);
    assert_eq!(local.known_events, 4);
    assert!(local.backfill_complete);
}
