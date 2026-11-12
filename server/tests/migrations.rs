use tokenlens_server::db::{self, DbError, MIGRATIONS, Migration};

const FIRST: Migration = Migration {
    version: 1,
    sql: "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
};
const SECOND: Migration = Migration {
    version: 2,
    sql: "ALTER TABLE notes ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;",
};
const BROKEN: Migration = Migration {
    version: 3,
    sql: "ALTER TABLE missing ADD COLUMN x INTEGER;",
};

fn version_of(path: &std::path::Path) -> u32 {
    let conn = rusqlite::Connection::open(path).expect("open");
    db::schema_version(&conn).expect("version")
}

#[test]
fn a_new_file_gets_the_latest_schema_without_a_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("server.sqlite");
    let opened = db::open(&path).expect("open");
    assert!(opened.backup.is_none());
    assert_eq!(
        db::schema_version(&opened.conn).expect("version"),
        MIGRATIONS.last().expect("migrations").version
    );
    let mode: String = opened
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("mode");
    assert_eq!(mode, "wal");
    drop(opened);
    let reopened = db::open(&path).expect("reopen");
    assert!(reopened.backup.is_none(), "nothing pending, nothing copied");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn pending_migrations_run_after_a_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("server.sqlite");
    let first = db::open_with(&path, &[FIRST]).expect("v1");
    first
        .conn
        .execute("INSERT INTO notes (body) VALUES ('kept')", [])
        .expect("insert");
    drop(first);

    let second = db::open_with(&path, &[FIRST, SECOND]).expect("v2");
    let backup = second.backup.clone().expect("backup taken");
    assert!(
        backup
            .file_name()
            .expect("name")
            .to_string_lossy()
            .ends_with(".pre-migration-1.sqlite")
    );
    assert_eq!(db::schema_version(&second.conn).expect("version"), 2);
    drop(second);
    assert_eq!(version_of(&backup), 1);
    let kept: String = rusqlite::Connection::open(&backup)
        .expect("open backup")
        .query_row("SELECT body FROM notes", [], |row| row.get(0))
        .expect("row");
    assert_eq!(kept, "kept");

    // A failing migration leaves the earlier version and the data intact,
    // and never overwrites an existing backup.
    std::fs::write(
        dir.path().join("server.sqlite.pre-migration-2.sqlite"),
        b"older",
    )
    .expect("seed");
    assert!(db::open_with(&path, &[FIRST, SECOND, BROKEN]).is_err());
    assert_eq!(version_of(&path), 2);
    assert_eq!(
        std::fs::read(dir.path().join("server.sqlite.pre-migration-2.sqlite")).expect("read"),
        b"older"
    );
    let backups = std::fs::read_dir(dir.path())
        .expect("list")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains(".pre-migration-2-")
        })
        .count();
    assert_eq!(backups, 1, "a second, timestamped backup was taken");
}

#[test]
fn a_newer_schema_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("server.sqlite");
    drop(db::open_with(&path, &[FIRST, SECOND]).expect("v2"));
    match db::open_with(&path, &[FIRST]) {
        Err(DbError::NewerSchema {
            found: 2,
            supported: 1,
        }) => {}
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("an older server opened a newer database"),
    }
}
