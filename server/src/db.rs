//! SQLite storage: migrations, pairing, credentials, events and status.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, DatabaseName, OpenFlags, OptionalExtension, Transaction, params};
use usage_event::envelope::{CollectorInfo, Heartbeat};
use usage_event::{Precision, UsageEvent};

use crate::pricing::{self, Pricer, PricingInput};
use crate::secrets::{self, PAIR_CODE_TTL_MS, Secret, TokenKind};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database schema version {found} is newer than this server supports ({supported})")]
    NewerSchema { found: u32, supported: u32 },
    #[error("{} already exists; pick a new file name", .0.display())]
    BackupExists(PathBuf),
    #[error("integrity check failed: {0}")]
    Corrupt(String),
    #[error("a server or restore is using this database; stop it first")]
    Busy,
    #[error("{0}")]
    InvalidTenant(&'static str),
    #[error("tenant {0} already exists")]
    TenantExists(String),
    #[error("no live tenant {0}; `tenant list` shows the ones that exist")]
    NoSuchTenant(String),
}

pub struct Migration {
    pub version: u32,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/0002_counted_events.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/0003_store_id.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/0004_tenants.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/0005_pricing.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("../migrations/0006_counted_flag.sql"),
    },
];

/// How many events carry a `counted` flag that disagrees with the D029 rule
/// worked out from scratch. Always 0 unless the triggers of schema v6 were
/// bypassed; tests and the backup check use it.
pub fn counted_drift(conn: &Connection) -> Result<i64, DbError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM usage_events e
         WHERE e.counted IS NOT (NOT EXISTS (
           SELECT 1
           FROM usage_events o
           JOIN devices od ON od.id = o.device_id
           JOIN devices ed ON ed.id = e.device_id
           WHERE o.event_key = e.event_key
             AND o.tenant_id = e.tenant_id
             AND o.device_id <> e.device_id
             AND (od.revoked_at IS NOT NULL, od.created_at, od.id)
               < (ed.revoked_at IS NOT NULL, ed.created_at, ed.id)))",
        [],
        |row| row.get(0),
    )?)
}

/// A second, read-only connection to a database `open` has already brought
/// up to date. In WAL mode it reads while the main connection writes, so
/// dashboard reads do not queue behind uploads.
pub fn open_reader(path: &Path) -> Result<Connection, DbError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA query_only = ON;")?;
    Ok(conn)
}

const NEW_STORE_ID: &str =
    "UPDATE server_meta SET value = lower(hex(randomblob(16))) WHERE key = 'store_id'";
const STORE_ID_SCHEMA: u32 = 3;
const COUNTED_SCHEMA: u32 = 6;
pub const DEFAULT_TENANT_ID: &str = "tenant_personal";

pub struct Opened {
    pub conn: Connection,
    /// Online backup taken before pending migrations ran on an existing file.
    pub backup: Option<PathBuf>,
}

pub fn open(path: &Path) -> Result<Opened, DbError> {
    open_with(path, MIGRATIONS)
}

pub fn open_with(path: &Path, migrations: &[Migration]) -> Result<Opened, DbError> {
    let existed = path.metadata().is_ok_and(|meta| meta.len() > 0);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut conn = Connection::open(path)?;
    restrict_permissions(path);
    conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
    )?;
    let backup = migrate(&mut conn, path, existed, migrations)?;
    Ok(Opened { conn, backup })
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

pub fn schema_version(conn: &Connection) -> Result<u32, DbError> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Held by `serve` and `restore` for as long as they run, so a restore never
/// swaps the file under a live server. The OS drops it if the process dies.
pub fn lock_database(path: &Path) -> Result<std::fs::File, DbError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sidecar(path, ".lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(DbError::Busy),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Random id of this copy of the database; sent with every ingest reply.
pub fn store_id(conn: &Connection) -> Result<String, DbError> {
    Ok(conn.query_row(
        "SELECT value FROM server_meta WHERE key = 'store_id'",
        [],
        |row| row.get(0),
    )?)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn migrate(
    conn: &mut Connection,
    path: &Path,
    existed: bool,
    migrations: &[Migration],
) -> Result<Option<PathBuf>, DbError> {
    let current = schema_version(conn)?;
    let latest = migrations.last().map_or(0, |migration| migration.version);
    if current > latest {
        return Err(DbError::NewerSchema {
            found: current,
            supported: latest,
        });
    }
    let pending = migrations
        .iter()
        .filter(|migration| migration.version > current)
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(None);
    }
    let backup = if existed {
        Some(backup_before_migration(conn, path, current)?)
    } else {
        None
    };
    for migration in pending {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }
    Ok(backup)
}

/// `<db>.pre-migration-<current version>.sqlite`, never overwriting an
/// earlier backup.
fn backup_before_migration(
    conn: &Connection,
    path: &Path,
    version: u32,
) -> Result<PathBuf, DbError> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "database".to_owned());
    let mut target = path.with_file_name(format!("{file_name}.pre-migration-{version}.sqlite"));
    if target.exists() {
        let stamp = chrono::Utc::now().timestamp_millis();
        target = path.with_file_name(format!(
            "{file_name}.pre-migration-{version}-{stamp}.sqlite"
        ));
    }
    conn.backup(DatabaseName::Main, &target, None)?;
    restrict_permissions(&target);
    Ok(target)
}

// ----------------------------------------------------------------- backup --

#[derive(Debug, PartialEq, Eq)]
pub struct BackupSummary {
    pub schema_version: u32,
    pub active_devices: i64,
    pub events: i64,
}

/// Copies the database at `source` to `dest` with SQLite's online backup, so
/// the server keeps running. The copy is a single self-contained file that is
/// integrity-checked before it gets its final name; `dest` is never overwritten.
pub fn backup_to(source: &Path, dest: &Path) -> Result<BackupSummary, DbError> {
    if dest.exists() {
        return Err(DbError::BackupExists(dest.to_owned()));
    }
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    // READ_WRITE without CREATE: a mistyped path fails instead of backing up
    // a brand-new empty database.
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup".to_owned());
    let partial = dest.with_file_name(format!("{name}.partial"));
    remove_if_present(&partial)?;
    let result = conn
        .backup(DatabaseName::Main, &partial, None)
        .map_err(DbError::from)
        .and_then(|()| finish_backup(&partial));
    match result {
        Ok(summary) => {
            std::fs::rename(&partial, dest)?;
            Ok(summary)
        }
        Err(error) => {
            let _ = remove_if_present(&partial);
            Err(error)
        }
    }
}

fn finish_backup(path: &Path) -> Result<BackupSummary, DbError> {
    restrict_permissions(path);
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // The copy inherits WAL mode; DELETE keeps it one file with no -wal/-shm.
    conn.query_row("PRAGMA journal_mode = DELETE", [], |row| {
        row.get::<_, String>(0)
    })?;
    // A copy put back by hand must still look new to the computers (D031).
    if schema_version(&conn)? >= STORE_ID_SCHEMA {
        conn.execute(NEW_STORE_ID, [])?;
    }
    verify(&conn)
}

#[derive(Debug)]
pub struct Restored {
    pub summary: BackupSummary,
    /// Where the replaced database was moved; it is never deleted.
    pub previous: Option<PathBuf>,
}

/// Puts a copy of `backup` at `target` under a new store id, so every
/// computer re-sends whatever the backup is missing (D031). The caller holds
/// [`lock_database`]; `backup` itself is left unchanged.
pub fn restore_from(backup: &Path, target: &Path) -> Result<Restored, DbError> {
    verify_backup(backup)?;
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let staging = sidecar(target, ".restoring");
    let clean = |path: &Path| -> std::io::Result<()> {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            remove_if_present(&sidecar(path, suffix))?;
        }
        Ok(())
    };
    clean(&staging)?;
    let summary = copy_database(backup, &staging).and_then(|()| prepare_restored(&staging));
    let summary = match summary {
        Ok(summary) => summary,
        Err(error) => {
            let _ = clean(&staging);
            return Err(error);
        }
    };
    let previous = if target.exists() {
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%3f");
        let aside = sidecar(target, &format!(".before-restore-{stamp}.sqlite"));
        std::fs::rename(target, &aside)?;
        // Uncheckpointed writes live in -wal; keep them with the moved file.
        let wal = sidecar(target, "-wal");
        if wal.exists() {
            std::fs::rename(&wal, sidecar(&aside, "-wal"))?;
        }
        remove_if_present(&sidecar(target, "-shm"))?;
        Some(aside)
    } else {
        None
    };
    std::fs::rename(&staging, target)?;
    Ok(Restored { summary, previous })
}

/// SQLite's backup API also carries pages still in a `-wal` file.
fn copy_database(source: &Path, dest: &Path) -> Result<(), DbError> {
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.backup(DatabaseName::Main, dest, None)?;
    Ok(())
}

fn prepare_restored(path: &Path) -> Result<BackupSummary, DbError> {
    restrict_permissions(path);
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.query_row("PRAGMA journal_mode = DELETE", [], |row| {
        row.get::<_, String>(0)
    })?;
    // The backup file is the pre-migration copy, so none is taken here.
    migrate(&mut conn, path, false, MIGRATIONS)?;
    conn.execute(NEW_STORE_ID, [])?;
    verify(&conn)
}

/// Opens a backup read-only and checks it is intact; a restore drill.
pub fn verify_backup(path: &Path) -> Result<BackupSummary, DbError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    verify(&conn)
}

fn verify(conn: &Connection) -> Result<BackupSummary, DbError> {
    let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(DbError::Corrupt(check));
    }
    // quick_check does not look at foreign keys; a row pointing at a missing
    // device or tenant would otherwise restore silently.
    let dangling = conn
        .query_row("PRAGMA foreign_key_check", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    if let Some(table) = dangling {
        return Err(DbError::Corrupt(format!("dangling reference in {table}")));
    }
    if schema_version(conn)? >= COUNTED_SCHEMA {
        let drift = counted_drift(conn)?;
        if drift != 0 {
            return Err(DbError::Corrupt(format!(
                "{drift} events disagree with the dedup rule"
            )));
        }
    }
    Ok(BackupSummary {
        schema_version: schema_version(conn)?,
        active_devices: conn.query_row(
            "SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL",
            [],
            |row| row.get(0),
        )?,
        events: conn.query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))?,
    })
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- pairing --

pub struct IssuedCode {
    pub code: Secret,
    pub expires_at: i64,
}

pub fn create_pair_code(
    conn: &Connection,
    now: i64,
    created_by: Option<&str>,
) -> Result<IssuedCode, DbError> {
    let tenant_id = match created_by {
        Some(device_id) => conn.query_row(
            "SELECT tenant_id FROM devices WHERE id = ?1 AND revoked_at IS NULL",
            params![device_id],
            |row| row.get::<_, String>(0),
        )?,
        None => DEFAULT_TENANT_ID.to_owned(),
    };
    create_pair_code_for_tenant(conn, now, &tenant_id, created_by)
}

/// Issues a code in an explicitly selected, live tenant. The caller is only
/// used for audit/revocation; a device can never select a different tenant.
pub fn create_pair_code_for_tenant(
    conn: &Connection,
    now: i64,
    tenant_id: &str,
    created_by: Option<&str>,
) -> Result<IssuedCode, DbError> {
    let live = conn
        .query_row(
            "SELECT 1 FROM tenants WHERE id = ?1 AND revoked_at IS NULL",
            params![tenant_id],
            |_| Ok(()),
        )
        .optional()?;
    if live.is_none() {
        return Err(DbError::NoSuchTenant(tenant_id.to_owned()));
    }
    if let Some(device_id) = created_by {
        let belongs = conn
            .query_row(
                "SELECT 1 FROM devices WHERE id = ?1 AND tenant_id = ?2 AND revoked_at IS NULL",
                params![device_id, tenant_id],
                |_| Ok(()),
            )
            .optional()?;
        if belongs.is_none() {
            return Err(rusqlite::Error::QueryReturnedNoRows.into());
        }
    }
    let code = secrets::new_pair_code();
    let expires_at = now + PAIR_CODE_TTL_MS;
    conn.execute(
        "INSERT INTO pair_codes
           (code_hash, created_at, expires_at, created_by_device_id, tenant_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            secrets::pair_code_hash(&code),
            now,
            expires_at,
            created_by,
            tenant_id
        ],
    )?;
    Ok(IssuedCode { code, expires_at })
}

/// Revokes every unused, unexpired code; returns how many.
pub fn revoke_open_pair_codes(conn: &Connection, now: i64) -> Result<usize, DbError> {
    Ok(conn.execute(
        "UPDATE pair_codes SET revoked_at = ?1
         WHERE used_at IS NULL AND revoked_at IS NULL AND expires_at > ?1",
        params![now],
    )?)
}

pub struct NewDevice<'a> {
    pub platform: &'a str,
    pub form_factor: &'a str,
    pub display_name: &'a str,
}

pub struct ClaimedDevice {
    pub device_id: String,
    pub ingest_token: Secret,
    pub read_token: Secret,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub device_id: String,
    pub tenant_id: String,
}

/// `None` when the code is unknown, used, expired or revoked; the caller
/// must not tell these apart.
pub fn claim_pair_code(
    conn: &mut Connection,
    code_hash: &str,
    now: i64,
    device: &NewDevice<'_>,
) -> Result<Option<ClaimedDevice>, DbError> {
    let tx = conn.transaction()?;
    let tenant_id = tx
        .query_row(
            "SELECT p.tenant_id FROM pair_codes p
             JOIN tenants t ON t.id = p.tenant_id AND t.revoked_at IS NULL
             WHERE p.code_hash = ?1 AND p.used_at IS NULL AND p.revoked_at IS NULL
               AND p.expires_at > ?2
               AND (p.created_by_device_id IS NULL
                    OR p.created_by_device_id IN
                       (SELECT id FROM devices WHERE revoked_at IS NULL))",
            params![code_hash, now],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(tenant_id) = tenant_id else {
        return Ok(None);
    };
    let used = tx.execute(
        "UPDATE pair_codes SET used_at = ?2
         WHERE code_hash = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
        params![code_hash, now],
    )?;
    if used == 0 {
        return Ok(None);
    }
    let device_id = secrets::new_device_id();
    tx.execute(
        "INSERT INTO devices (id, display_name, platform, form_factor, created_at, tenant_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            device_id,
            device.display_name,
            device.platform,
            device.form_factor,
            now,
            tenant_id
        ],
    )?;
    let ingest_token = insert_token(&tx, &tenant_id, &device_id, TokenKind::Ingest, now)?;
    let read_token = insert_token(&tx, &tenant_id, &device_id, TokenKind::Read, now)?;
    tx.execute(
        "UPDATE pair_codes SET claimed_device_id = ?2 WHERE code_hash = ?1",
        params![code_hash, device_id],
    )?;
    tx.commit()?;
    Ok(Some(ClaimedDevice {
        device_id,
        ingest_token,
        read_token,
    }))
}

fn insert_token(
    tx: &Transaction<'_>,
    tenant_id: &str,
    device_id: &str,
    kind: TokenKind,
    now: i64,
) -> Result<Secret, DbError> {
    let token = secrets::new_token(kind);
    tx.execute(
        "INSERT INTO device_tokens (id, device_id, kind, token_hash, created_at, tenant_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            secrets::new_token_id(),
            device_id,
            kind.as_str(),
            secrets::token_hash(token.expose()),
            now,
            tenant_id
        ],
    )?;
    Ok(token)
}

/// The device of a live token of exactly `kind`; `None` for anything else.
pub fn authenticate(
    conn: &Connection,
    token: &str,
    kind: TokenKind,
    now: i64,
) -> Result<Option<AuthContext>, DbError> {
    if !token.starts_with(kind.prefix()) {
        return Ok(None);
    }
    let hash = secrets::token_hash(token);
    let found = conn
        .query_row(
            "SELECT t.device_id, d.tenant_id, t.kind
             FROM device_tokens t
             JOIN devices d ON d.id = t.device_id AND d.tenant_id = t.tenant_id
             JOIN tenants n ON n.id = d.tenant_id AND n.revoked_at IS NULL
             WHERE t.token_hash = ?1 AND t.revoked_at IS NULL AND d.revoked_at IS NULL",
            params![hash],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((device_id, tenant_id, stored_kind)) = found else {
        return Ok(None);
    };
    if TokenKind::parse(&stored_kind) != Some(kind) {
        return Ok(None);
    }
    conn.execute(
        "UPDATE device_tokens SET last_used_at = ?2 WHERE token_hash = ?1",
        params![hash, now],
    )?;
    Ok(Some(AuthContext {
        device_id,
        tenant_id,
    }))
}

// ---------------------------------------------------------------- tenants --

const TENANT_SLUG_MAX: usize = 32;
const TENANT_NAME_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantRow {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub created_at: i64,
    pub active_devices: i64,
}

/// Lowercase ASCII letters and digits, with single hyphens between them;
/// at most 32 characters. The slug also forms the id (`tenant_<slug>`).
pub fn valid_tenant_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= TENANT_SLUG_MAX
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

/// Creates a live tenant and returns its id. Only the operator CLI calls
/// this; no HTTP route can create or pick a tenant.
pub fn create_tenant(
    conn: &Connection,
    slug: &str,
    display_name: &str,
    now: i64,
) -> Result<String, DbError> {
    if !valid_tenant_slug(slug) {
        return Err(DbError::InvalidTenant(
            "the short name may use a-z, 0-9 and single hyphens, at most 32 characters",
        ));
    }
    let display_name = display_name.trim();
    if display_name.is_empty()
        || display_name.chars().count() > TENANT_NAME_MAX
        || display_name.chars().any(char::is_control)
    {
        return Err(DbError::InvalidTenant(
            "the display name must be 1-64 characters without control characters",
        ));
    }
    let id = format!("tenant_{slug}");
    let taken = conn
        .query_row(
            "SELECT 1 FROM tenants WHERE id = ?1 OR slug = ?2",
            params![id, slug],
            |_| Ok(()),
        )
        .optional()?;
    if taken.is_some() {
        return Err(DbError::TenantExists(slug.to_owned()));
    }
    conn.execute(
        "INSERT INTO tenants (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, slug, display_name, now],
    )?;
    Ok(id)
}

/// The id of a live tenant named by its id or its slug.
pub fn resolve_tenant(conn: &Connection, id_or_slug: &str) -> Result<String, DbError> {
    conn.query_row(
        "SELECT id FROM tenants WHERE (id = ?1 OR slug = ?1) AND revoked_at IS NULL",
        params![id_or_slug],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| DbError::NoSuchTenant(id_or_slug.to_owned()))
}

/// Live tenants, oldest first, with how many live devices each has.
pub fn list_tenants(conn: &Connection) -> Result<Vec<TenantRow>, DbError> {
    let mut statement = conn.prepare(
        "SELECT t.id, t.slug, t.display_name, t.created_at,
           (SELECT COUNT(*) FROM devices d WHERE d.tenant_id = t.id AND d.revoked_at IS NULL)
         FROM tenants t WHERE t.revoked_at IS NULL ORDER BY t.created_at, t.id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(TenantRow {
                id: row.get(0)?,
                slug: row.get(1)?,
                display_name: row.get(2)?,
                created_at: row.get(3)?,
                active_devices: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------- devices --

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub tenant_id: String,
    pub platform: Option<String>,
    pub form_factor: Option<String>,
    pub last_seen_at: Option<i64>,
}

/// Devices that are not revoked, oldest first; every tenant's when `tenant_id`
/// is `None` (operator CLI only).
pub fn list_devices(conn: &Connection, tenant_id: Option<&str>) -> Result<Vec<DeviceRow>, DbError> {
    let mut statement = conn.prepare(
        "SELECT id, tenant_id, platform, form_factor, last_seen_at FROM devices
         WHERE (?1 IS NULL OR tenant_id = ?1) AND revoked_at IS NULL
         ORDER BY created_at, id",
    )?;
    let rows = statement
        .query_map(params![tenant_id], |row| {
            Ok(DeviceRow {
                id: row.get(0)?,
                tenant_id: row.get(1)?,
                platform: row.get(2)?,
                form_factor: row.get(3)?,
                last_seen_at: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The tenant of a live device, for the operator CLI.
pub fn device_tenant(conn: &Connection, device_id: &str) -> Result<Option<String>, DbError> {
    Ok(conn
        .query_row(
            "SELECT tenant_id FROM devices WHERE id = ?1 AND revoked_at IS NULL",
            params![device_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Revokes the device, both of its tokens and the unused codes it issued.
/// Returns `false` if no such live device exists.
pub fn revoke_device(
    conn: &mut Connection,
    tenant_id: &str,
    device_id: &str,
    now: i64,
) -> Result<bool, DbError> {
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE devices SET revoked_at = ?3
         WHERE tenant_id = ?1 AND id = ?2 AND revoked_at IS NULL",
        params![tenant_id, device_id, now],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE device_tokens SET revoked_at = ?3
         WHERE tenant_id = ?1 AND device_id = ?2 AND revoked_at IS NULL",
        params![tenant_id, device_id, now],
    )?;
    tx.execute(
        "UPDATE pair_codes SET revoked_at = ?3
         WHERE tenant_id = ?1 AND created_by_device_id = ?2
           AND used_at IS NULL AND revoked_at IS NULL",
        params![tenant_id, device_id, now],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Returns `false` if no such live device exists.
pub fn rename_device(
    conn: &Connection,
    tenant_id: &str,
    device_id: &str,
    display_name: &str,
) -> Result<bool, DbError> {
    Ok(conn.execute(
        "UPDATE devices SET display_name = ?3
         WHERE tenant_id = ?1 AND id = ?2 AND revoked_at IS NULL",
        params![tenant_id, device_id, display_name],
    )? > 0)
}

pub struct RotatedTokens {
    pub ingest_token: Secret,
    pub read_token: Secret,
}

/// Replaces both tokens of a live device; the old ones stop working at once.
pub fn rotate_tokens(
    conn: &mut Connection,
    tenant_id: &str,
    device_id: &str,
    now: i64,
) -> Result<Option<RotatedTokens>, DbError> {
    let tx = conn.transaction()?;
    let live = tx
        .query_row(
            "SELECT 1 FROM devices
             WHERE tenant_id = ?1 AND id = ?2 AND revoked_at IS NULL",
            params![tenant_id, device_id],
            |_| Ok(()),
        )
        .optional()?;
    if live.is_none() {
        return Ok(None);
    }
    tx.execute(
        "UPDATE device_tokens SET revoked_at = ?3
         WHERE tenant_id = ?1 AND device_id = ?2 AND revoked_at IS NULL",
        params![tenant_id, device_id, now],
    )?;
    let ingest_token = insert_token(&tx, tenant_id, device_id, TokenKind::Ingest, now)?;
    let read_token = insert_token(&tx, tenant_id, device_id, TokenKind::Read, now)?;
    tx.commit()?;
    Ok(Some(RotatedTokens {
        ingest_token,
        read_token,
    }))
}

// ----------------------------------------------------------------- ingest --

pub struct BatchMeta<'a> {
    pub batch_id: &'a str,
    pub collector: &'a CollectorInfo,
    pub scan_generation: &'a str,
    pub rejected: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestCounts {
    pub accepted: u64,
    pub updated: u64,
    pub duplicates: u64,
}

const EVENT_VALUE_COLUMNS: &str = "client, session_id, parent_session_id, model_id, provider_id, \
     ts, input, output, cache_read, cache_write, cache_write_1h, reasoning, cost_usd, \
     cost_source, precision, credits, rate_version, project_hash, project_label, \
     message_count, is_turn_start, duration_ms, pricing_model_id, billing_provider, \
     service_tier, scan_generation";

fn precision_from_db(text: &str) -> Precision {
    match text {
        "measured" => Precision::Measured,
        "estimated" => Precision::Estimated,
        _ => Precision::Unknown,
    }
}

fn int(value: u64) -> i64 {
    // Callers reject token counts above 2^53 before reaching storage.
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn uint(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn stored_event(
    row: &rusqlite::Row<'_>,
    event_key: &str,
) -> rusqlite::Result<(UsageEvent, String)> {
    let event = UsageEvent {
        event_key: event_key.to_owned(),
        client: row.get(0)?,
        session_id: row.get(1)?,
        parent_session_id: row.get(2)?,
        model_id: row.get(3)?,
        provider_id: row.get(4)?,
        ts: row.get(5)?,
        input: uint(row.get(6)?),
        output: uint(row.get(7)?),
        cache_read: uint(row.get(8)?),
        cache_write: uint(row.get(9)?),
        cache_write_1h: uint(row.get(10)?),
        reasoning: uint(row.get(11)?),
        cost_usd: row.get(12)?,
        cost_source: row.get(13)?,
        precision: precision_from_db(&row.get::<_, String>(14)?),
        credits: row.get(15)?,
        rate_version: row.get(16)?,
        project_hash: row.get(17)?,
        project_label: row.get(18)?,
        message_count: row.get(19)?,
        is_turn_start: row.get(20)?,
        duration_ms: row.get(21)?,
        pricing_model_id: row.get(22)?,
        billing_provider: row.get(23)?,
        service_tier: row.get(24)?,
    };
    Ok((event, row.get(25)?))
}

/// Upserts every event in one transaction. An event whose stored values and
/// scan generation are identical counts as a duplicate and is left untouched.
pub fn write_batch(
    conn: &mut Connection,
    tenant_id: &str,
    device_id: &str,
    meta: &BatchMeta<'_>,
    events: &[UsageEvent],
    pricer: Option<&Pricer>,
    now: i64,
) -> Result<IngestCounts, DbError> {
    let mut counts = IngestCounts::default();
    let tx = conn.transaction()?;
    {
        let mut select = tx.prepare_cached(&format!(
            "SELECT {EVENT_VALUE_COLUMNS} FROM usage_events
             WHERE tenant_id = ?1 AND device_id = ?2 AND event_key = ?3"
        ))?;
        let mut insert = tx.prepare_cached(&format!(
            "INSERT INTO usage_events (tenant_id, device_id, event_key, {EVENT_VALUE_COLUMNS},
               collector_version, tokscale_rev, ingested_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
               ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31,
               ?32, ?32)"
        ))?;
        let mut set_price = tx.prepare_cached(
            "UPDATE usage_events SET est_cost_usd = ?4, price_status = ?5, price_version = ?6
             WHERE tenant_id = ?1 AND device_id = ?2 AND event_key = ?3",
        )?;
        let mut update = tx.prepare_cached(
            "UPDATE usage_events SET client = ?3, session_id = ?4, parent_session_id = ?5,
               model_id = ?6, provider_id = ?7, ts = ?8, input = ?9, output = ?10,
               cache_read = ?11, cache_write = ?12, cache_write_1h = ?13, reasoning = ?14,
               cost_usd = ?15, cost_source = ?16, precision = ?17, credits = ?18,
               rate_version = ?19, project_hash = ?20, project_label = ?21,
               message_count = ?22, is_turn_start = ?23, duration_ms = ?24,
               scan_generation = ?25, collector_version = ?26, tokscale_rev = ?27,
               updated_at = ?28, pricing_model_id = ?30, billing_provider = ?31,
               service_tier = ?32
             WHERE device_id = ?1 AND event_key = ?2 AND tenant_id = ?29",
        )?;
        for event in events {
            let existing = select
                .query_row(params![tenant_id, device_id, event.event_key], |row| {
                    stored_event(row, &event.event_key)
                })
                .optional()?;
            if let Some((stored, generation)) = &existing
                && stored == event
                && generation == meta.scan_generation
            {
                counts.duplicates += 1;
                continue;
            }
            if existing.is_some() {
                let update_values = params![
                    device_id,
                    event.event_key,
                    event.client,
                    event.session_id,
                    event.parent_session_id,
                    event.model_id,
                    event.provider_id,
                    event.ts,
                    int(event.input),
                    int(event.output),
                    int(event.cache_read),
                    int(event.cache_write),
                    int(event.cache_write_1h),
                    int(event.reasoning),
                    event.cost_usd,
                    event.cost_source,
                    event.precision.as_str(),
                    event.credits,
                    event.rate_version,
                    event.project_hash,
                    event.project_label,
                    event.message_count,
                    event.is_turn_start,
                    event.duration_ms,
                    meta.scan_generation,
                    meta.collector.version,
                    meta.collector.tokscale_rev,
                    now,
                    tenant_id,
                    event.pricing_model_id,
                    event.billing_provider,
                    event.service_tier,
                ];
                update.execute(update_values)?;
                counts.updated += 1;
            } else {
                let insert_values = params![
                    tenant_id,
                    device_id,
                    event.event_key,
                    event.client,
                    event.session_id,
                    event.parent_session_id,
                    event.model_id,
                    event.provider_id,
                    event.ts,
                    int(event.input),
                    int(event.output),
                    int(event.cache_read),
                    int(event.cache_write),
                    int(event.cache_write_1h),
                    int(event.reasoning),
                    event.cost_usd,
                    event.cost_source,
                    event.precision.as_str(),
                    event.credits,
                    event.rate_version,
                    event.project_hash,
                    event.project_label,
                    event.message_count,
                    event.is_turn_start,
                    event.duration_ms,
                    event.pricing_model_id,
                    event.billing_provider,
                    event.service_tier,
                    meta.scan_generation,
                    meta.collector.version,
                    meta.collector.tokscale_rev,
                    now,
                ];
                insert.execute(insert_values)?;
                counts.accepted += 1;
            }
            let priced = pricing::price(pricer, &PricingInput::from(event));
            set_price.execute(params![
                tenant_id,
                device_id,
                event.event_key,
                priced.est_cost_usd,
                priced.status.map(pricing::PriceStatus::as_str),
                priced.version,
            ])?;
        }
    }
    // A batch id reused by a different device must not overwrite its record.
    tx.execute(
        "INSERT INTO ingest_batches
           (tenant_id, batch_id, device_id, received_at, accepted, updated, duplicates, rejected)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(tenant_id, device_id, batch_id) DO UPDATE SET received_at = excluded.received_at,
           accepted = excluded.accepted, updated = excluded.updated,
           duplicates = excluded.duplicates, rejected = excluded.rejected",
        params![
            tenant_id,
            meta.batch_id,
            device_id,
            now,
            int(counts.accepted),
            int(counts.updated),
            int(counts.duplicates),
            i64::try_from(meta.rejected).unwrap_or(i64::MAX),
        ],
    )?;
    tx.execute(
        "UPDATE devices SET last_upload_at = ?2, last_seen_at = ?2,
           collector_version = ?3, tokscale_rev = ?4, parser_generation = ?5
         WHERE tenant_id = ?6 AND id = ?1",
        params![
            device_id,
            now,
            meta.collector.version,
            meta.collector.tokscale_rev,
            meta.collector.parser_generation,
            tenant_id
        ],
    )?;
    tx.commit()?;
    Ok(counts)
}

/// Re-prices up to `limit` events whose price version differs from
/// `pricer`'s (D042), tenant by tenant, in one transaction. Returns how many
/// changed; fewer than `limit` means none are left.
pub fn reprice_batch(
    conn: &mut Connection,
    pricer: &Pricer,
    limit: usize,
) -> Result<usize, DbError> {
    let tx = conn.transaction()?;
    let mut changed = 0;
    {
        let tenants = tx
            .prepare("SELECT id FROM tenants ORDER BY id")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut select = tx.prepare_cached(
            "SELECT device_id, event_key, client, model_id, provider_id, pricing_model_id,
               billing_provider, service_tier, message_count, input, output, cache_read,
               cache_write, cache_write_1h, reasoning, cost_usd, credits
             FROM usage_events
             WHERE tenant_id = ?1 AND price_version IS NOT ?2
             LIMIT ?3",
        )?;
        let mut update = tx.prepare_cached(
            "UPDATE usage_events SET est_cost_usd = ?4, price_status = ?5, price_version = ?6
             WHERE tenant_id = ?1 AND device_id = ?2 AND event_key = ?3",
        )?;
        for tenant_id in &tenants {
            let room = limit.saturating_sub(changed);
            if room == 0 {
                break;
            }
            let rows = select
                .query_map(
                    params![
                        tenant_id,
                        pricer.version(),
                        i64::try_from(room).unwrap_or(i64::MAX)
                    ],
                    StoredPricing::read,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            for row in rows {
                let priced = pricing::price(Some(pricer), &row.input());
                update.execute(params![
                    tenant_id,
                    row.device_id,
                    row.event_key,
                    priced.est_cost_usd,
                    priced.status.map(pricing::PriceStatus::as_str),
                    priced.version,
                ])?;
                changed += 1;
            }
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// One line of `pricing report`: events of one client, model and provider
/// that end up with the same price status under `pricer`.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceReportRow {
    pub status: &'static str,
    pub client: String,
    pub model_id: String,
    pub provider_id: Option<String>,
    pub events: u64,
    pub tokens: u64,
    pub cost_usd: f64,
    /// tokscale's explanation for one partial or unpriced event of the group.
    pub explanation: Option<String>,
}

/// Prices every stored event with `pricer` without writing anything; for
/// checking which models a catalog covers. Largest token counts first.
pub fn price_report(conn: &Connection, pricer: &Pricer) -> Result<Vec<PriceReportRow>, DbError> {
    use std::collections::BTreeMap;
    let tenants = conn
        .prepare("SELECT id FROM tenants ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut select = conn.prepare(
        "SELECT device_id, event_key, client, model_id, provider_id, pricing_model_id,
           billing_provider, service_tier, message_count, input, output, cache_read,
           cache_write, cache_write_1h, reasoning, cost_usd, credits
         FROM counted_events WHERE tenant_id = ?1",
    )?;
    type Group = (u64, u64, f64, Option<String>);
    let mut groups = BTreeMap::<(&'static str, String, String, Option<String>), Group>::new();
    for tenant_id in &tenants {
        let rows = select
            .query_map(params![tenant_id], StoredPricing::read)?
            .collect::<Result<Vec<_>, _>>()?;
        for row in rows {
            let input = row.input();
            let priced = pricing::price(Some(pricer), &input);
            let status = priced
                .status
                .map_or("pending", pricing::PriceStatus::as_str);
            let [prompt, output, cache_read, cache_write, _, reasoning] = row.tokens;
            let entry = groups
                .entry((
                    status,
                    row.client.clone(),
                    row.model_id.clone(),
                    row.provider_id.clone(),
                ))
                .or_default();
            entry.0 += 1;
            entry.1 += prompt + output + cache_read + cache_write + reasoning;
            entry.2 += priced.est_cost_usd.or(row.cost_usd).unwrap_or(0.0);
            if entry.3.is_none() && matches!(status, "partial" | "unpriced") {
                entry.3 = Some(pricer.explain(&input));
            }
        }
    }
    let mut report = groups
        .into_iter()
        .map(
            |((status, client, model_id, provider_id), (events, tokens, cost_usd, explanation))| {
                PriceReportRow {
                    status,
                    client,
                    model_id,
                    provider_id,
                    events,
                    tokens,
                    cost_usd,
                    explanation,
                }
            },
        )
        .collect::<Vec<_>>();
    report.sort_by_key(|row| std::cmp::Reverse(row.tokens));
    Ok(report)
}

struct StoredPricing {
    device_id: String,
    event_key: String,
    client: String,
    model_id: String,
    provider_id: Option<String>,
    pricing_model_id: Option<String>,
    billing_provider: Option<String>,
    service_tier: Option<String>,
    message_count: i64,
    tokens: [u64; 6],
    cost_usd: Option<f64>,
    credits: Option<f64>,
}

impl StoredPricing {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            device_id: row.get(0)?,
            event_key: row.get(1)?,
            client: row.get(2)?,
            model_id: row.get(3)?,
            provider_id: row.get(4)?,
            pricing_model_id: row.get(5)?,
            billing_provider: row.get(6)?,
            service_tier: row.get(7)?,
            message_count: row.get(8)?,
            tokens: [
                uint(row.get(9)?),
                uint(row.get(10)?),
                uint(row.get(11)?),
                uint(row.get(12)?),
                uint(row.get(13)?),
                uint(row.get(14)?),
            ],
            cost_usd: row.get(15)?,
            credits: row.get(16)?,
        })
    }

    fn input(&self) -> PricingInput<'_> {
        PricingInput {
            client: &self.client,
            model_id: &self.model_id,
            provider_id: self.provider_id.as_deref(),
            pricing_model_id: self.pricing_model_id.as_deref(),
            billing_provider: self.billing_provider.as_deref(),
            service_tier: self.service_tier.as_deref(),
            message_count: self.message_count,
            tokens: self.tokens,
            cost_usd: self.cost_usd,
            credits: self.credits,
        }
    }
}

pub fn record_heartbeat(
    conn: &mut Connection,
    tenant_id: &str,
    device_id: &str,
    heartbeat: &Heartbeat,
    now: i64,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE devices SET last_seen_at = ?2, collector_version = ?3, tokscale_rev = ?4,
           parser_generation = ?5, backfill_complete = ?6, last_scan_at = ?7,
           last_scan_duration_ms = ?8, outbox_backlog = ?9,
           os_name = COALESCE(?10, os_name), os_version = COALESCE(?11, os_version)
         WHERE tenant_id = ?12 AND id = ?1",
        params![
            device_id,
            now,
            heartbeat.collector.version,
            heartbeat.collector.tokscale_rev,
            heartbeat.collector.parser_generation,
            heartbeat.backfill_complete,
            heartbeat.last_scan_at,
            heartbeat.last_scan_duration_ms.map(int),
            int(heartbeat.outbox_backlog),
            heartbeat.os_name,
            heartbeat.os_version,
            tenant_id,
        ],
    )?;
    {
        let mut upsert = tx.prepare_cached(
            "INSERT INTO device_client_status
               (tenant_id, device_id, client, detected, last_event_at, last_scan_events, error_code, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(device_id, client) DO UPDATE SET detected = excluded.detected,
               last_event_at = excluded.last_event_at, last_scan_events = excluded.last_scan_events,
               error_code = excluded.error_code, updated_at = excluded.updated_at",
        )?;
        for client in &heartbeat.clients {
            upsert.execute(params![
                tenant_id,
                device_id,
                client.client,
                client.detected,
                client.last_event_at,
                client.last_scan_events.map(int),
                client.error_code,
                now,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}
