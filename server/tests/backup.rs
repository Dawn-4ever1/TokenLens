mod common;

use common::{Harness, envelope, fixture_events, heartbeat};
use reqwest::StatusCode;
use serde_json::Value;
use tokenlens_server::db::{self, DbError};

async fn seeded() -> (Harness, common::Device) {
    let h = Harness::start().await;
    let device = h.pair().await;
    let (status, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&device.ingest),
            serde_json::to_vec(&heartbeat(&device.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    (h, device)
}

async fn figures(h: &Harness, read: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for path in [
        "/api/dashboard/summary",
        "/api/dashboard/sessions",
        "/api/dashboard/devices",
    ] {
        let (status, body) = h.get(path, Some(read)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        out.push(body);
    }
    out
}

#[tokio::test]
async fn a_backup_taken_while_serving_restores_the_same_dashboard() {
    let (h, device) = seeded().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("daily").join("tokenlens-2027-01-15.sqlite");

    let summary = db::backup_to(&h.db_path, &dest).expect("backup");
    assert_eq!(
        summary.schema_version,
        db::MIGRATIONS.last().unwrap().version
    );
    assert_eq!(summary.active_devices, 1);
    assert_eq!(summary.events, fixture_events().len() as i64);

    let names = std::fs::read_dir(dest.parent().unwrap())
        .expect("list")
        .map(|entry| entry.expect("entry").file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["tokenlens-2027-01-15.sqlite"],
        "one file, no -wal/-shm/.partial"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let before = std::fs::read(&dest).expect("read");
    assert_eq!(db::verify_backup(&dest).expect("verify"), summary);
    assert_eq!(
        std::fs::read(&dest).expect("read"),
        before,
        "verify changes nothing"
    );

    match db::backup_to(&h.db_path, &dest) {
        Err(DbError::BackupExists(path)) => assert_eq!(path, dest),
        other => panic!("overwrote a backup: {:?}", other.map(|_| ())),
    }

    // Restore drill: a fresh server on the copy shows the same figures and
    // still accepts the old device's tokens.
    let restored = Harness::restored_from(&dest).await;
    assert_eq!(
        figures(&restored, &device.read).await,
        figures(&h, &device.read).await
    );
    let (status, body) = restored
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b2", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], 0, "already restored, nothing new");
}

#[tokio::test]
async fn the_health_probe_reaches_a_live_server_only() {
    let h = Harness::start().await;
    let addr: std::net::SocketAddr = h.base.trim_start_matches("http://").parse().unwrap();
    let probe = |addr| {
        tokio::task::spawn_blocking(move || {
            tokenlens_server::check_health(addr, std::time::Duration::from_secs(2))
        })
    };
    probe(addr).await.unwrap().expect("healthy");
    let any = std::net::SocketAddr::from(([0, 0, 0, 0], addr.port()));
    probe(any)
        .await
        .unwrap()
        .expect("0.0.0.0 goes through loopback");
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = closed.local_addr().unwrap();
    drop(closed);
    assert!(probe(dead).await.unwrap().is_err());
}

#[test]
fn a_missing_source_is_not_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("typo.sqlite");
    let dest = dir.path().join("copy.sqlite");
    assert!(db::backup_to(&source, &dest).is_err());
    assert!(!source.exists());
    assert!(!dest.exists());
    assert!(!dir.path().join("copy.sqlite.partial").exists());
}

#[test]
fn a_damaged_backup_fails_verification() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("server.sqlite");
    drop(db::open(&source).expect("open"));
    let dest = dir.path().join("copy.sqlite");
    db::backup_to(&source, &dest).expect("backup");

    let mut bytes = std::fs::read(&dest).expect("read");
    let page = 4096;
    assert!(bytes.len() > 2 * page);
    for byte in &mut bytes[page..2 * page] {
        *byte = 0xA5;
    }
    std::fs::write(&dest, bytes).expect("write");
    assert!(db::verify_backup(&dest).is_err());

    std::fs::write(&dest, b"not a database").expect("write");
    assert!(db::verify_backup(&dest).is_err());
}

fn with_suffix(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn store_id_of(path: &std::path::Path) -> String {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open");
    db::store_id(&conn).expect("store id")
}

/// A database as a killed server leaves it: recent writes only in `-wal`.
fn crashed_database(dir: &std::path::Path) -> std::path::PathBuf {
    let live = dir.join("live.sqlite");
    let opened = db::open(&live).expect("open");
    db::create_pair_code(&opened.conn, 0, None).expect("write");
    let target = dir.join("data").join("tokenlens.sqlite");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(&live, &target).unwrap();
    std::fs::copy(with_suffix(&live, "-wal"), with_suffix(&target, "-wal")).unwrap();
    drop(opened);
    target
}

#[tokio::test]
async fn restore_swaps_in_the_backup_under_a_new_store_id() {
    let h = Harness::start().await;
    let device = h.pair().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let early = dir.path().join("early.sqlite");
    db::backup_to(&h.db_path, &early).expect("backup");
    let (status, body) = h
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b1", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let live_id = body["storeId"].as_str().expect("store id").to_owned();
    assert_eq!(live_id.len(), 32);
    assert_ne!(
        store_id_of(&early),
        live_id,
        "a copy put back by hand looks new"
    );
    let untouched = std::fs::read(&early).expect("read");

    let target = crashed_database(dir.path());
    let lock = db::lock_database(&target).expect("lock");
    assert!(matches!(db::lock_database(&target), Err(DbError::Busy)));
    let restored = db::restore_from(&early, &target).expect("restore");
    drop(lock);
    assert_eq!(restored.summary.events, 0, "the backup predates the upload");
    assert_eq!(restored.summary.active_devices, 1);
    assert_eq!(std::fs::read(&early).expect("read"), untouched);
    assert!(!with_suffix(&target, "-wal").exists());
    assert!(!with_suffix(&target, ".restoring").exists());
    let aside = restored.previous.expect("replaced file kept");
    let kept = rusqlite::Connection::open(&aside).expect("open aside");
    let codes: i64 = kept
        .query_row("SELECT COUNT(*) FROM pair_codes", [], |row| row.get(0))
        .expect("count");
    assert_eq!(codes, 1, "its -wal moved with it");
    drop(kept);
    let restored_id = store_id_of(&target);
    assert_ne!(restored_id, store_id_of(&early));
    assert_ne!(restored_id, live_id);

    // Computers learn the new id from the next reply and re-send; whatever
    // the backup lacked counts as new.
    let server = Harness::restored_from(&target).await;
    let (status, body) = server
        .ingest(
            &device.ingest,
            &envelope(&device.id, "b2", "g1", fixture_events()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["storeId"], restored_id.as_str());
    assert_eq!(body["accepted"], fixture_events().len());
    let (status, body) = server
        .post(
            "/api/ingest/heartbeat",
            Some(&device.ingest),
            serde_json::to_vec(&heartbeat(&device.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["storeId"], restored_id.as_str());
}

#[test]
fn restore_upgrades_an_older_backup_and_refuses_a_damaged_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let old = dir.path().join("v2.sqlite");
    drop(db::open_with(&old, &db::MIGRATIONS[..2]).expect("v2"));
    let target = dir.path().join("server.sqlite");
    let restored = db::restore_from(&old, &target).expect("restore v2");
    assert_eq!(
        restored.summary.schema_version,
        db::MIGRATIONS.last().unwrap().version
    );
    assert!(restored.previous.is_none());
    assert_eq!(store_id_of(&target).len(), 32);
    let names = std::fs::read_dir(dir.path())
        .expect("list")
        .map(|entry| entry.expect("entry").file_name().into_string().unwrap())
        .filter(|name| name.starts_with("server.sqlite"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["server.sqlite"],
        "no pre-migration copy, staging file or -wal left"
    );

    let current = std::fs::read(&target).expect("read");
    let damaged = dir.path().join("damaged.sqlite");
    std::fs::write(&damaged, b"not a database").expect("write");
    assert!(db::restore_from(&damaged, &target).is_err());
    assert_eq!(
        std::fs::read(&target).expect("read"),
        current,
        "left as it was"
    );
}
