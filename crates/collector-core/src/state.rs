//! Local collector state (architecture 5.6): the persisted snapshot of events
//! already handed to the outbox (`known`) and the upload outbox itself.
//!
//! Only whitelisted canonical event JSON is stored; no paths, titles or
//! content. The file lives in this project's own data directory.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::snapshot::ScannedEvent;

pub const STATE_FILE: &str = "collector-state.sqlite";
const LOCK_FILE: &str = "collector.lock";
const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = "
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
CREATE TABLE known (
    event_key TEXT PRIMARY KEY,
    client TEXT NOT NULL,
    ts INTEGER NOT NULL,
    hash BLOB NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE outbox (
    id INTEGER PRIMARY KEY,
    event_key TEXT NOT NULL UNIQUE,
    version INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    generation TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX outbox_by_generation ON outbox (generation, ts, event_key);
";

pub mod meta {
    pub const DEVICE_ID: &str = "device_id";
    pub const GENERATION: &str = "generation";
    pub const FULL_SCAN_GENERATION: &str = "full_scan_generation";
    pub const REJECTED_TOTAL: &str = "rejected_total";
    pub const UPLOAD_STATE: &str = "upload_state";
    pub const LAST_UPLOAD_AT: &str = "last_upload_at";
    pub const LAST_HEARTBEAT_AT: &str = "last_heartbeat_at";
    pub const SERVER_STORE_ID: &str = "server_store_id";
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("the local collector state could not be read or written")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the local collector state directory could not be prepared")]
    Io(#[from] std::io::Error),
    #[error("the local collector state was written by a newer collector")]
    TooNew,
    #[error(
        "another collector (the TokenLens app or `collector run`) is already uploading from this computer; stop it first"
    )]
    Busy,
}

/// Held for as long as a process queues and uploads, so the app and
/// `collector run` never share one queue. The OS drops it if the process dies.
pub fn lock_queue(dir: &Path) -> Result<File, StateError> {
    std::fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(StateError::Busy),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub event_key: String,
    pub version: i64,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub generation: String,
    pub rows: Vec<OutboxRow>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Prepared {
    pub device_changed: bool,
    pub generation_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStats {
    pub client: String,
    pub events: u64,
    pub last_event_at: i64,
}

pub struct State {
    conn: Connection,
}

pub fn default_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("tokenlens"))
}

fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

impl State {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(STATE_FILE)
    }

    pub fn exists_in(dir: &Path) -> bool {
        Self::path_in(dir).is_file()
    }

    pub fn open(dir: &Path) -> Result<Self, StateError> {
        std::fs::create_dir_all(dir)?;
        restrict(dir, 0o700)?;
        let path = Self::path_in(dir);
        let conn = Connection::open(&path)?;
        restrict(&path, 0o600)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StateError::TooNew);
        }
        if version == 0 {
            conn.execute_batch(&format!(
                "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
        }
        Ok(Self { conn })
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, StateError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn meta_i64(&self, key: &str) -> Result<Option<i64>, StateError> {
        Ok(self.meta(key)?.and_then(|value| value.parse().ok()))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StateError> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// A different device drops everything; a different parser generation
    /// drops `known` so every event is queued again, but keeps queued rows.
    pub fn prepare(&mut self, device_id: &str, generation: &str) -> Result<Prepared, StateError> {
        let tx = self.conn.transaction()?;
        let get = |key: &str| -> rusqlite::Result<Option<String>> {
            tx.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
        };
        let device_changed = get(meta::DEVICE_ID)?.as_deref() != Some(device_id);
        let generation_changed = get(meta::GENERATION)?.as_deref() != Some(generation);
        if device_changed {
            tx.execute_batch(
                "DELETE FROM known; DELETE FROM outbox;
                 DELETE FROM meta WHERE key IN ('full_scan_generation', 'rejected_total',
                     'upload_state', 'last_upload_at', 'last_heartbeat_at', 'server_store_id');",
            )?;
        } else if generation_changed {
            tx.execute_batch(
                "DELETE FROM known; DELETE FROM meta WHERE key = 'full_scan_generation';",
            )?;
        }
        let set = "INSERT INTO meta (key, value) VALUES (?1, ?2)
                   ON CONFLICT (key) DO UPDATE SET value = excluded.value";
        tx.execute(set, params![meta::DEVICE_ID, device_id])?;
        tx.execute(set, params![meta::GENERATION, generation])?;
        tx.commit()?;
        Ok(Prepared {
            device_changed,
            generation_changed,
        })
    }

    pub fn for_each_known(
        &self,
        mut visit: impl FnMut(String, &str, i64, [u8; 32]),
    ) -> Result<usize, StateError> {
        let mut statement = self
            .conn
            .prepare("SELECT event_key, client, ts, hash FROM known")?;
        let mut rows = statement.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let hash: Vec<u8> = row.get(3)?;
            let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
                continue;
            };
            let client: String = row.get(1)?;
            visit(row.get(0)?, &client, row.get(2)?, hash);
            count += 1;
        }
        Ok(count)
    }

    /// Queues changed events and records them as known, in one transaction.
    /// Returns how many were queued; events without a payload are skipped
    /// and left unknown.
    pub fn record_changes(
        &mut self,
        generation: &str,
        changed: &[ScannedEvent],
        now: i64,
    ) -> Result<u64, StateError> {
        let tx = self.conn.transaction()?;
        let mut queued = 0;
        {
            let mut known = tx.prepare(
                "INSERT INTO known (event_key, client, ts, hash) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (event_key) DO UPDATE
                 SET client = excluded.client, ts = excluded.ts, hash = excluded.hash",
            )?;
            let mut outbox = tx.prepare(
                "INSERT INTO outbox (event_key, version, ts, generation, payload, created_at)
                 VALUES (?1, 1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (event_key) DO UPDATE
                 SET version = version + 1, ts = excluded.ts,
                     generation = excluded.generation, payload = excluded.payload",
            )?;
            for event in changed {
                let Some(payload) = &event.payload else {
                    continue;
                };
                known.execute(params![event.key, event.client, event.ts, &event.hash[..]])?;
                outbox.execute(params![event.key, event.ts, generation, payload, now])?;
                queued += 1;
            }
        }
        tx.commit()?;
        Ok(queued)
    }

    pub fn backlog(&self) -> Result<u64, StateError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Oldest generation first, then oldest events; never mixes generations.
    pub fn next_batch(&self, max_events: usize) -> Result<Option<Batch>, StateError> {
        let generation: Option<String> = self
            .conn
            .query_row(
                "SELECT generation FROM outbox ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(generation) = generation else {
            return Ok(None);
        };
        let mut statement = self.conn.prepare(
            "SELECT event_key, version, payload FROM outbox WHERE generation = ?1
             ORDER BY ts, event_key LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![generation, i64::try_from(max_events).unwrap_or(i64::MAX)],
                |row| {
                    Ok(OutboxRow {
                        event_key: row.get(0)?,
                        version: row.get(1)?,
                        payload: row.get(2)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Batch { generation, rows }))
    }

    /// Removes sent rows unless a newer value was queued meanwhile.
    pub fn ack(&mut self, rows: &[OutboxRow]) -> Result<u64, StateError> {
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        {
            let mut delete =
                tx.prepare("DELETE FROM outbox WHERE event_key = ?1 AND version = ?2")?;
            for row in rows {
                removed += delete.execute(params![row.event_key, row.version])? as u64;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn add_rejected(&self, count: u64) -> Result<(), StateError> {
        if count == 0 {
            return Ok(());
        }
        let total = self.meta_i64(meta::REJECTED_TOTAL)?.unwrap_or(0);
        self.set_meta(
            meta::REJECTED_TOTAL,
            &(total.saturating_add(i64::try_from(count).unwrap_or(i64::MAX))).to_string(),
        )
    }

    pub fn client_stats(&self) -> Result<Vec<ClientStats>, StateError> {
        let mut statement = self.conn.prepare(
            "SELECT client, COUNT(*), MAX(ts) FROM known GROUP BY client ORDER BY client",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(ClientStats {
                    client: row.get(0)?,
                    events: row.get::<_, i64>(1)?.try_into().unwrap_or(0),
                    last_event_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records the server's store id (D031). A different id means the server
    /// lost what it had (restored backup), so `known` is dropped in the same
    /// transaction and every event is queued again; returns whether that happened.
    pub fn adopt_store(&mut self, store_id: &str) -> Result<bool, StateError> {
        let tx = self.conn.transaction()?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [meta::SERVER_STORE_ID],
                |row| row.get(0),
            )
            .optional()?;
        if previous.as_deref() == Some(store_id) {
            return Ok(false);
        }
        let changed = previous.is_some();
        if changed {
            tx.execute_batch(
                "DELETE FROM known; DELETE FROM meta WHERE key = 'full_scan_generation';",
            )?;
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![meta::SERVER_STORE_ID, store_id],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    pub fn backfill_complete(&self, generation: &str) -> Result<bool, StateError> {
        Ok(
            self.meta(meta::FULL_SCAN_GENERATION)?.as_deref() == Some(generation)
                && self.backlog()? == 0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: &str, ts: i64, value: u8) -> ScannedEvent {
        ScannedEvent {
            key: key.to_owned(),
            client: "codex".to_owned(),
            ts,
            hash: [value; 32],
            payload: Some(format!("{{\"v\":{value}}}")),
        }
    }

    #[test]
    fn schema_is_created_once_and_files_are_private() {
        let dir = tempfile::tempdir().expect("dir");
        let state_dir = dir.path().join("tokenlens");
        let state = State::open(&state_dir).expect("open");
        state.set_meta("x", "1").expect("meta");
        drop(state);
        let state = State::open(&state_dir).expect("reopen");
        assert_eq!(state.meta("x").expect("meta").as_deref(), Some("1"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode();
            assert_eq!(mode(&state_dir) & 0o777, 0o700);
            assert_eq!(mode(&State::path_in(&state_dir)) & 0o777, 0o600);
        }
        state
            .conn
            .execute_batch("PRAGMA user_version = 99")
            .expect("bump");
        drop(state);
        assert!(matches!(State::open(&state_dir), Err(StateError::TooNew)));
    }

    #[test]
    fn upserts_bump_versions_and_ack_keeps_newer_values() {
        let dir = tempfile::tempdir().expect("dir");
        let mut state = State::open(dir.path()).expect("open");
        state.prepare("dev_a", "g1").expect("prepare");
        let queued = state
            .record_changes("g1", &[event("b", 2, 1), event("a", 1, 1)], 10)
            .expect("record");
        assert_eq!(queued, 2);
        let batch = state.next_batch(10).expect("batch").expect("some");
        assert_eq!(batch.generation, "g1");
        assert_eq!(
            batch
                .rows
                .iter()
                .map(|row| row.event_key.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        // "a" changes while the batch is in flight.
        state
            .record_changes("g1", &[event("a", 1, 2)], 11)
            .expect("record");
        assert_eq!(state.ack(&batch.rows).expect("ack"), 1);
        let left = state.next_batch(10).expect("batch").expect("some");
        assert_eq!(left.rows.len(), 1);
        assert_eq!(
            (left.rows[0].version, left.rows[0].payload.as_str()),
            (2, "{\"v\":2}")
        );
        let mut payloadless = event("c", 3, 3);
        payloadless.payload = None;
        assert_eq!(
            state
                .record_changes("g1", &[payloadless], 12)
                .expect("record"),
            0
        );
        let mut known = Vec::new();
        state
            .for_each_known(|key, _, _, hash| known.push((key, hash[0])))
            .expect("known");
        known.sort();
        assert_eq!(known, [("a".to_owned(), 2), ("b".to_owned(), 1)]);
    }

    #[test]
    fn generation_and_device_changes() {
        let dir = tempfile::tempdir().expect("dir");
        let mut state = State::open(dir.path()).expect("open");
        assert_eq!(
            state.prepare("dev_a", "g1").expect("prepare"),
            Prepared {
                device_changed: true,
                generation_changed: true
            }
        );
        state
            .record_changes("g1", &[event("a", 1, 1)], 1)
            .expect("record");
        state
            .set_meta(meta::FULL_SCAN_GENERATION, "g1")
            .expect("meta");
        assert!(!state.backfill_complete("g1").expect("backfill"));
        assert_eq!(
            state.prepare("dev_a", "g1").expect("same"),
            Prepared::default()
        );
        // New parser generation: known is cleared, queued rows stay.
        state.prepare("dev_a", "g2").expect("g2");
        assert_eq!(state.for_each_known(|_, _, _, _| {}).expect("known"), 0);
        state
            .record_changes("g2", &[event("b", 0, 1)], 2)
            .expect("record");
        assert_eq!(state.backlog().expect("backlog"), 2);
        let first = state.next_batch(10).expect("batch").expect("some");
        assert_eq!((first.generation.as_str(), first.rows.len()), ("g1", 1));
        state.ack(&first.rows).expect("ack");
        let second = state.next_batch(10).expect("batch").expect("some");
        assert_eq!(second.generation, "g2");
        state.ack(&second.rows).expect("ack");
        assert!(!state.backfill_complete("g2").expect("no full scan yet"));
        state
            .set_meta(meta::FULL_SCAN_GENERATION, "g2")
            .expect("meta");
        assert!(state.backfill_complete("g2").expect("backfill"));
        // A different device starts over.
        state
            .record_changes("g2", &[event("c", 0, 1)], 3)
            .expect("record");
        state.add_rejected(2).expect("rejected");
        state.prepare("dev_b", "g2").expect("dev_b");
        assert_eq!(state.backlog().expect("backlog"), 0);
        assert_eq!(state.for_each_known(|_, _, _, _| {}).expect("known"), 0);
        assert_eq!(state.meta(meta::REJECTED_TOTAL).expect("meta"), None);
        assert_eq!(state.meta(meta::FULL_SCAN_GENERATION).expect("meta"), None);
    }

    #[test]
    fn a_new_server_store_id_forgets_known_but_keeps_the_queue() {
        let dir = tempfile::tempdir().expect("dir");
        let mut state = State::open(dir.path()).expect("open");
        state.prepare("dev_a", "g1").expect("prepare");
        state
            .record_changes("g1", &[event("a", 1, 1), event("b", 2, 1)], 1)
            .expect("record");
        let sent = state.next_batch(1).expect("batch").expect("some");
        state.ack(&sent.rows).expect("ack");
        state
            .set_meta(meta::FULL_SCAN_GENERATION, "g1")
            .expect("meta");
        assert!(!state.adopt_store("s1").expect("first id"));
        assert!(!state.adopt_store("s1").expect("same id"));
        assert_eq!(state.for_each_known(|_, _, _, _| {}).expect("known"), 2);
        assert!(state.adopt_store("s2").expect("restored server"));
        assert_eq!(state.for_each_known(|_, _, _, _| {}).expect("known"), 0);
        assert_eq!(state.backlog().expect("backlog"), 1);
        assert_eq!(state.meta(meta::FULL_SCAN_GENERATION).expect("meta"), None);
        assert_eq!(
            state.meta(meta::SERVER_STORE_ID).expect("meta").as_deref(),
            Some("s2")
        );
        assert!(!state.adopt_store("s2").expect("settled"));
        state.prepare("dev_b", "g1").expect("dev_b");
        assert_eq!(state.meta(meta::SERVER_STORE_ID).expect("meta"), None);
    }

    #[test]
    fn client_stats_come_from_known() {
        let dir = tempfile::tempdir().expect("dir");
        let mut state = State::open(dir.path()).expect("open");
        let mut other = event("z", 50, 1);
        other.client = "claude".into();
        state
            .record_changes("g1", &[event("a", 10, 1), event("b", 30, 1), other], 1)
            .expect("record");
        let stats = state.client_stats().expect("stats");
        assert_eq!(
            stats,
            [
                ClientStats {
                    client: "claude".into(),
                    events: 1,
                    last_event_at: 50
                },
                ClientStats {
                    client: "codex".into(),
                    events: 2,
                    last_event_at: 30
                }
            ]
        );
    }
}
