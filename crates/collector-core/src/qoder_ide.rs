//! Read-only adapter for Qoder IDE's legacy SharedClientCache SQLite store.
//!
//! The IDE is separate from Qoder Desktop's `~/.qoder/projects` transcripts.
//! Only message identifiers, timestamps, model names and token_info numbers
//! are read; conversation bodies and project paths never enter a UsageEvent.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use usage_event::{Precision, UsageEvent, qoder_cn_event_key, qoder_event_key};

const CLIENT_ID: &str = "qoder-intl";
const DEFAULT_MODEL: &str = "qoder-agent";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edition {
    International,
    China,
}

impl Edition {
    fn client_id(self) -> &'static str {
        match self {
            Self::International => CLIENT_ID,
            Self::China => "qoder-cn",
        }
    }

    fn app_dir(self) -> &'static str {
        match self {
            Self::International => "Qoder",
            Self::China => "QoderCN",
        }
    }
}

fn assert_allowed_path(path: &Path, edition: Edition) -> Result<(), crate::qoder::QoderError> {
    if path.components().any(|component| {
        matches!(
            component,
            Component::Normal(part)
                if part.eq_ignore_ascii_case(".qoder-cn")
                    || part.eq_ignore_ascii_case("com.qodercn.app.stable")
                    || (edition == Edition::International
                        && part.eq_ignore_ascii_case("QoderCN"))
        )
    }) {
        Err(crate::qoder::QoderError::ForbiddenMirrorPath)
    } else {
        Ok(())
    }
}

/// The IDE store, not Qoder Desktop's `com.qoder.app.stable/main.sqlite`.
pub fn database_path_in(root: &Path) -> PathBuf {
    root.join("SharedClientCache")
        .join("cache")
        .join("db")
        .join("local.db")
}

fn application_root(home: &Path, use_env_roots: bool, edition: Edition) -> PathBuf {
    let home_key = match edition {
        Edition::International => "QODER_HOME",
        Edition::China => "QODER_CN_HOME",
    };
    if use_env_roots
        && let Some(root) = std::env::var_os(home_key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    {
        return root;
    }
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join(edition.app_dir())
    } else if cfg!(target_os = "windows") {
        use_env_roots
            .then(|| {
                std::env::var_os("APPDATA")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            })
            .flatten()
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join(edition.app_dir())
    } else {
        home.join(".config").join(edition.app_dir())
    }
}

pub fn database_path(home: &Path, use_env_roots: bool) -> PathBuf {
    database_path_for(home, use_env_roots, Edition::International)
}

pub fn database_path_cn(home: &Path, use_env_roots: bool) -> PathBuf {
    database_path_for(home, use_env_roots, Edition::China)
}

fn database_path_for(home: &Path, use_env_roots: bool, edition: Edition) -> PathBuf {
    let db_key = match edition {
        Edition::International => "QODER_DB_PATH",
        Edition::China => "QODER_CN_DB_PATH",
    };
    if use_env_roots
        && let Some(path) = std::env::var_os(db_key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    {
        return path;
    }
    database_path_in(&application_root(home, use_env_roots, edition))
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn json_object(text: &str) -> Result<Option<serde_json::Map<String, Value>>, ()> {
    match serde_json::from_str::<Value>(text).map_err(|_| ())? {
        Value::Object(object) => Ok(Some(object)),
        Value::Null => Ok(None),
        _ => Err(()),
    }
}

fn counter(object: &serde_json::Map<String, Value>, name: &str) -> Result<u64, ()> {
    let Some(value) = object.get(name) else {
        return Ok(0);
    };
    let Some(value) = value.as_f64() else {
        return Err(());
    };
    if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
        return Err(());
    }
    // Counters are integers; tolerate SQLite JSON producers serializing 1.0.
    Ok(value.round() as u64)
}

fn model_from_row(
    model_info: Option<String>,
    record_extra: Option<String>,
    preferred_model_info: Option<String>,
) -> Result<String, ()> {
    for (text_value, paths) in [model_info, record_extra, preferred_model_info]
        .into_iter()
        .zip([
            &[&["model_key"][..], &["modelKey"][..]][..],
            &[&["modelConfig", "key"][..], &["model_config", "key"][..]][..],
            &[
                &["model_key"][..],
                &["modelKey"][..],
                &["preferred_model"][..],
                &["preferredModel"][..],
            ][..],
        ])
    {
        let Some(text_value) = text_value else {
            continue;
        };
        let Some(object) = json_object(&text_value)? else {
            continue;
        };
        for path in paths {
            let mut value = None;
            for (index, key) in path.iter().enumerate() {
                value = if index == 0 {
                    object.get(*key)
                } else {
                    value
                        .and_then(Value::as_object)
                        .and_then(|object| object.get(*key))
                };
            }
            if let Some(model) = text(value) {
                return Ok(model.to_owned());
            }
        }
    }
    Ok(DEFAULT_MODEL.to_owned())
}

fn timestamp_ms(value: rusqlite::types::Value) -> Result<i64, ()> {
    use rusqlite::types::Value as SqlValue;
    match value {
        SqlValue::Integer(value) if value > 0 => Ok(if value >= 1_000_000_000_000 {
            value
        } else {
            value.saturating_mul(1000)
        }),
        SqlValue::Real(value) if value.is_finite() && value > 0.0 => {
            let value = if value >= 1_000_000_000_000.0 {
                value
            } else {
                value * 1000.0
            };
            if value > i64::MAX as f64 {
                Err(())
            } else {
                Ok(value.round() as i64)
            }
        }
        SqlValue::Text(value) => crate::qoder::qoder_timestamp(&value).ok_or(()),
        SqlValue::Null | SqlValue::Blob(_) | SqlValue::Integer(_) | SqlValue::Real(_) => Err(()),
    }
}

fn nullable_text(value: rusqlite::types::Value) -> Result<Option<String>, ()> {
    match value {
        rusqlite::types::Value::Null => Ok(None),
        rusqlite::types::Value::Text(value) => Ok(Some(value)),
        _ => Err(()),
    }
}

fn row_id(value: rusqlite::types::Value) -> Result<i64, ()> {
    match value {
        rusqlite::types::Value::Integer(value) if value >= 0 => Ok(value),
        _ => Err(()),
    }
}

fn event_from_values(
    values: [rusqlite::types::Value; 8],
    edition: Edition,
) -> Result<Option<UsageEvent>, ()> {
    let [
        row,
        id,
        session,
        request,
        token_info,
        model_info,
        created,
        extra,
        ..,
    ] = values;
    let row = row_id(row)?;
    let id = nullable_text(id)?.filter(|id| !id.trim().is_empty());
    let session = nullable_text(session)?
        .filter(|session| !session.trim().is_empty())
        .unwrap_or_else(|| format!("qoder-ide-row-{row}"));
    let request = nullable_text(request)?.filter(|id| !id.trim().is_empty());
    let Some(token_info) = nullable_text(token_info)? else {
        return Ok(None);
    };
    let Some(usage) = json_object(&token_info)? else {
        return Ok(None);
    };
    let prompt = counter(&usage, "prompt_tokens")?;
    let cached = counter(&usage, "cached_tokens")?.min(prompt);
    let output = counter(&usage, "completion_tokens")?;
    if prompt == 0 && output == 0 {
        return Ok(None);
    }
    let ts = timestamp_ms(created)?;
    let model = model_from_row(nullable_text(model_info)?, nullable_text(extra)?, None)?;
    // `prompt_tokens` includes cached tokens. A distinct IDE prefix avoids
    // changing the transcript's existing key, even if an imported row shares
    // its message id with a transcript.
    let turn_key = id
        .map(|id| format!("ide-msg:{id}"))
        .or_else(|| request.map(|id| format!("ide-req:{id}")))
        .unwrap_or_else(|| format!("ide-row:{row}"));
    let event_key = match edition {
        Edition::International => qoder_event_key(&session, Some(&turn_key), ts, &model, 0),
        Edition::China => qoder_cn_event_key(&session, Some(&turn_key), ts, &model, 0),
    };
    Ok(Some(UsageEvent {
        event_key,
        client: edition.client_id().to_owned(),
        session_id: session,
        session_title: None,
        parent_session_id: None,
        model_id: model,
        provider_id: Some("qoder".to_owned()),
        ts,
        input: prompt.saturating_sub(cached),
        output,
        cache_read: cached,
        cache_write: 0,
        cache_write_1h: 0,
        reasoning: 0,
        cost_usd: None,
        cost_source: "unknown".to_owned(),
        precision: Precision::Measured,
        credits: None,
        rate_version: None,
        project_hash: None,
        project_label: None,
        message_count: 1,
        // The IDE schema has no user-turn marker. Each measured assistant
        // row is a root turn; multiple messages in a request do not merge.
        is_turn_start: true,
        duration_ms: None,
        pricing_model_id: None,
        billing_provider: None,
        service_tier: None,
    }))
}

pub fn scan_database(
    path: &Path,
    range: crate::DateRange,
) -> Result<Vec<UsageEvent>, crate::qoder::QoderError> {
    scan_database_for(path, range, Edition::International)
}

fn scan_database_for(
    path: &Path,
    range: crate::DateRange,
    edition: Edition,
) -> Result<Vec<UsageEvent>, crate::qoder::QoderError> {
    assert_allowed_path(path, edition)?;
    if !crate::roots::is_real_file(path) {
        return Ok(Vec::new());
    }
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = rusqlite::Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(std::time::Duration::from_millis(500))?;
    connection.pragma_update(None, "query_only", "ON")?;
    // Even a corrupt URI-looking filename is opened as a plain path here.
    // Do not read the application's main.sqlite or transcript mirror.
    let table_exists = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'chat_message')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !table_exists {
        return Ok(Vec::new());
    }
    let record_table_exists = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'chat_record')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let session_titles = crate::titles::qoder_session_titles(&connection);
    let query = if record_table_exists {
        "SELECT cm.rowid, cm.id, cm.session_id, cm.request_id, \
         cm.token_info, cm.model_info, cm.gmt_create, \
         (SELECT cr.extra FROM chat_record AS cr \
          WHERE cm.request_id IS NOT NULL AND trim(cm.request_id) <> '' \
            AND cr.request_id = cm.request_id \
          ORDER BY cr.rowid DESC LIMIT 1) AS extra \
         FROM chat_message AS cm \
         WHERE cm.role = 'assistant' \
         AND cm.token_info IS NOT NULL AND trim(cm.token_info) NOT IN ('', '{}') \
         ORDER BY cm.gmt_create, cm.rowid"
    } else {
        "SELECT cm.rowid, cm.id, cm.session_id, cm.request_id, \
         cm.token_info, cm.model_info, cm.gmt_create, NULL AS extra \
         FROM chat_message AS cm \
         WHERE cm.role = 'assistant' \
         AND cm.token_info IS NOT NULL AND trim(cm.token_info) NOT IN ('', '{}') \
         ORDER BY cm.gmt_create, cm.rowid"
    };
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query([])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let values = [
            row.get::<_, rusqlite::types::Value>(0)?,
            row.get::<_, rusqlite::types::Value>(1)?,
            row.get::<_, rusqlite::types::Value>(2)?,
            row.get::<_, rusqlite::types::Value>(3)?,
            row.get::<_, rusqlite::types::Value>(4)?,
            row.get::<_, rusqlite::types::Value>(5)?,
            row.get::<_, rusqlite::types::Value>(6)?,
            row.get::<_, rusqlite::types::Value>(7)?,
        ];
        let Ok(Some(mut event)) = event_from_values(values, edition) else {
            continue;
        };
        event.session_title = session_titles.get(&event.session_id).cloned();
        if crate::qoder::in_range(event.ts, range) {
            events.push(event);
        }
    }
    Ok(events)
}

pub fn scan_home(
    home: &Path,
    use_env_roots: bool,
    range: crate::DateRange,
) -> Result<Vec<UsageEvent>, crate::qoder::QoderError> {
    scan_database(&database_path(home, use_env_roots), range)
}

pub fn scan_home_cn(
    home: &Path,
    use_env_roots: bool,
    range: crate::DateRange,
) -> Result<Vec<UsageEvent>, crate::qoder::QoderError> {
    scan_database_for(
        &database_path_cn(home, use_env_roots),
        range,
        Edition::China,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMESTAMP: i64 = 1_784_681_696_263;

    fn synthetic_database() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("fixture directory");
        let path = temp.path().join("local.db");
        let db = rusqlite::Connection::open(path).expect("synthetic db");
        db.execute_batch(
            "CREATE TABLE chat_message (\
             id TEXT, session_id TEXT, request_id TEXT, role TEXT, \
             token_info TEXT, model_info TEXT, gmt_create INTEGER, body TEXT);\
             CREATE TABLE chat_record (request_id TEXT, extra TEXT, content TEXT);\
             CREATE TABLE chat_session (session_id TEXT, project_uri TEXT, title TEXT);",
        )
        .expect("synthetic schema");
        db.execute(
            "INSERT INTO chat_record VALUES ('synthetic-request', '{\"modelConfig\":{\"key\":\"fallback-model\"}}', 'not read')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_session VALUES ('synthetic-session', '/private/path/not/read', 'Qoder session title')",
            [],
        )
        .unwrap();
        for (id, session, request, tokens, model, created) in [
            (
                "synthetic-message-1",
                "synthetic-session",
                "synthetic-request",
                "{\"prompt_tokens\":100,\"cached_tokens\":80,\"completion_tokens\":10}",
                "{\"model_key\":\"synthetic-model\"}",
                TIMESTAMP,
            ),
            (
                "synthetic-message-2",
                "synthetic-session",
                "synthetic-request",
                "{\"prompt_tokens\":90,\"cached_tokens\":0,\"completion_tokens\":5}",
                "{}",
                TIMESTAMP + 1000,
            ),
        ] {
            db.execute(
                "INSERT INTO chat_message VALUES (?1, ?2, ?3, 'assistant', ?4, ?5, ?6, 'never read')",
                rusqlite::params![id, session, request, tokens, model, created],
            )
            .unwrap();
        }
        // No usage, user content and malformed usage must not affect totals.
        db.execute(
            "INSERT INTO chat_message VALUES ('user', 'synthetic-session', NULL, 'user', NULL, NULL, ?1, 'private')",
            [TIMESTAMP],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_message VALUES ('bad', 'synthetic-session', NULL, 'assistant', '{bad', NULL, ?1, 'private')",
            [TIMESTAMP],
        )
        .unwrap();
        temp
    }

    #[test]
    fn ide_tokens_are_measured_and_cached_input_is_not_counted_twice() {
        let fixture = synthetic_database();
        let events = scan_database(
            &fixture.path().join("local.db"),
            crate::DateRange::all_history(),
        )
        .expect("synthetic IDE database");
        assert_eq!(events.len(), 2);
        assert_eq!(
            (
                events[0].input,
                events[0].cache_read,
                events[0].output,
                events[0].cache_write,
                events[0].reasoning,
            ),
            (20, 80, 10, 0, 0)
        );
        assert_eq!(
            (events[1].input, events[1].cache_read, events[1].output),
            (90, 0, 5)
        );
        assert_eq!(events[1].model_id, "fallback-model");
        assert!(
            events
                .iter()
                .all(|event| event.session_title.as_deref() == Some("Qoder session title"))
        );
        assert!(events.iter().all(|event| {
            event.precision == Precision::Measured
                && event.credits.is_none()
                && event.client == CLIENT_ID
        }));
        assert_ne!(events[0].event_key, events[1].event_key);
        for event in &events {
            let json = event.to_json_value().expect("valid upload");
            assert!(
                json.as_object()
                    .unwrap()
                    .keys()
                    .all(|key| { usage_event::UPLOAD_FIELDS.contains(&key.as_str()) })
            );
            assert!(!json.to_string().contains("private"));
            assert!(!json.to_string().contains("never read"));
        }
    }

    #[test]
    fn repeat_scan_and_empty_database_are_stable() {
        let fixture = synthetic_database();
        let path = fixture.path().join("local.db");
        let first = scan_database(&path, crate::DateRange::all_history()).unwrap();
        let second = scan_database(&path, crate::DateRange::all_history()).unwrap();
        let keys = |events: &[UsageEvent]| {
            events
                .iter()
                .map(|event| event.event_key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&first), keys(&second));
        let db = rusqlite::Connection::open(fixture.path().join("other.db")).unwrap();
        db.execute_batch("CREATE TABLE unrelated (id INTEGER)")
            .unwrap();
        assert!(
            scan_database(
                &fixture.path().join("other.db"),
                crate::DateRange::all_history()
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            scan_database(
                &fixture.path().join("missing.db"),
                crate::DateRange::all_history()
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            scan_database(
                &fixture.path().join("local.db"),
                crate::DateRange {
                    since: None,
                    until: chrono::NaiveDate::from_ymd_opt(2025, 1, 1),
                }
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn desktop_and_cn_locations_are_not_accepted_as_ide_data() {
        for path in [
            PathBuf::from("/synthetic/.qoder-cn/projects"),
            PathBuf::from("/synthetic/Library/Application Support/QoderCN"),
            PathBuf::from(
                "/synthetic/Library/Application Support/com.qodercn.app.stable/main.sqlite",
            ),
        ] {
            assert!(assert_allowed_path(&path, Edition::International).is_err());
            assert!(scan_database(&path, crate::DateRange::all_history()).is_err());
        }
    }

    #[test]
    fn cn_ide_reads_its_own_database_not_the_transcript_mirror() {
        let temp = tempfile::tempdir().expect("home");
        let fixture = synthetic_database();
        let path = database_path_cn(temp.path(), false);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(fixture.path().join("local.db"), &path).unwrap();
        let report = crate::scan_with(
            &crate::Config::default(),
            &crate::ScanOptions {
                home: temp.path().to_path_buf(),
                use_env_roots: false,
                clients: Some(std::collections::BTreeSet::from([
                    crate::QODER_CN_CLIENT.to_owned()
                ])),
                range: crate::DateRange::all_history(),
                upstream_cache: false,
                project_salt: None,
            },
        )
        .expect("CN IDE collector");
        let cn = report
            .client_totals
            .iter()
            .find(|client| client.client == crate::QODER_CN_CLIENT)
            .expect("CN IDE detected");
        assert_eq!((cn.messages, cn.tokens.total), (2, 205));
        assert!(
            report
                .client_totals
                .iter()
                .all(|client| client.client != crate::QODER_CLIENT)
        );
    }

    #[test]
    fn full_collector_includes_ide_and_existing_transcript_events() {
        let temp = tempfile::tempdir().expect("home");
        let database = database_path(temp.path(), false);
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        // Copy only the synthetic fixture, never an actual Qoder directory.
        let fixture = synthetic_database();
        std::fs::copy(fixture.path().join("local.db"), database).unwrap();
        let report = crate::scan_with(
            &crate::Config::default(),
            &crate::ScanOptions {
                home: temp.path().to_path_buf(),
                use_env_roots: false,
                clients: Some(std::collections::BTreeSet::from([
                    crate::QODER_CLIENT.to_owned()
                ])),
                range: crate::DateRange::all_history(),
                upstream_cache: false,
                project_salt: None,
            },
        )
        .expect("full collector");
        let qoder = report
            .client_totals
            .iter()
            .find(|client| client.client == crate::QODER_CLIENT)
            .expect("Qoder IDE detected");
        assert_eq!(qoder.messages, 2);
        assert_eq!(qoder.tokens.input, 110);
        assert_eq!(qoder.tokens.cache_read, 80);
        assert_eq!(qoder.tokens.output, 15);
        assert_eq!(qoder.tokens.total, 205);
        assert_eq!(qoder.precision_counts.get("measured"), Some(&2));
        assert_eq!(report.event_digest.event_count, 2);
        assert_eq!(report.event_digest.unique_event_keys, 2);
    }

    #[test]
    fn ide_path_is_the_shared_client_cache_not_desktop_main_database() {
        let root = Path::new("/synthetic/Qoder");
        assert_eq!(
            database_path_in(root),
            root.join("SharedClientCache/cache/db/local.db")
        );
        assert!(
            !database_path_in(root)
                .to_string_lossy()
                .contains("main.sqlite")
        );
    }

    #[test]
    fn cn_key_namespace_duplicate_chat_record_and_missing_chat_record_table_are_safe() {
        let fixture = synthetic_database();
        let path = fixture.path().join("local.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            "INSERT INTO chat_record VALUES ('synthetic-request', '{\"modelConfig\":{\"key\":\"other-model\"}}', 'x')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO chat_record VALUES ('', '{}', 'x')", [])
            .unwrap();
        db.execute("INSERT INTO chat_record VALUES ('', '{}', 'x')", [])
            .unwrap();
        db.execute(
            "INSERT INTO chat_message VALUES ('empty-req', 'synthetic-session', '', 'assistant', '{\"prompt_tokens\":10,\"cached_tokens\":0,\"completion_tokens\":2}', '{\"model_key\":\"m1\"}', ?1, 'x')",
            [TIMESTAMP + 2000],
        )
        .unwrap();
        drop(db);

        let intl = scan_database_for(
            &path,
            crate::DateRange::all_history(),
            Edition::International,
        )
        .expect("intl scan");
        let cn = scan_database_for(&path, crate::DateRange::all_history(), Edition::China)
            .expect("cn scan");
        assert_eq!(intl.len(), 3);
        assert_eq!(cn.len(), 3);
        for (i, c) in intl.iter().zip(cn.iter()) {
            assert_ne!(i.event_key, c.event_key);
        }

        let no_record = fixture.path().join("no_record.db");
        let db = rusqlite::Connection::open(&no_record).unwrap();
        db.execute_batch(
            "CREATE TABLE chat_message (id TEXT, session_id TEXT, request_id TEXT, role TEXT, token_info TEXT, model_info TEXT, gmt_create INTEGER);
             INSERT INTO chat_message VALUES ('m1', 's1', NULL, 'assistant', '{\"prompt_tokens\":5,\"cached_tokens\":0,\"completion_tokens\":1}', '{\"model_key\":\"m\"}', 1784681696263);",
        )
        .unwrap();
        let solo = scan_database_for(
            &no_record,
            crate::DateRange::all_history(),
            Edition::International,
        )
        .expect("scan without chat_record");
        assert_eq!(solo.len(), 1);
    }
}
