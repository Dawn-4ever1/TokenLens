//! Read title metadata already stored by coding agents, not conversation text.
//!
//! tokscale-core owns usage parsing. This adapter only fills title fields that
//! its locked revision does not expose; it never changes keys, tokens, costs,
//! models or attribution. No prompt or assistant content is used as a title.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokscale_core::UnifiedMessage;
use walkdir::WalkDir;

pub const MAX_TITLE_BYTES: usize = 512;
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// A normalized title event can be repeated on every message in its session.
pub fn metadata_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty() && value.len() <= MAX_TITLE_BYTES && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    let tail = &needle[1..];
    let limit = haystack.len() - needle.len();
    let mut offset = 0;
    while offset <= limit {
        let Some(found) = haystack[offset..=limit]
            .iter()
            .position(|&byte| byte == first)
        else {
            return false;
        };
        let start = offset + found;
        if haystack[start + 1..start + needle.len()] == *tail {
            return true;
        }
        offset = start + 1;
    }
    false
}

fn matches_any(haystack: &[u8], needles: &[&[u8]]) -> bool {
    needles.is_empty()
        || needles
            .iter()
            .any(|needle| bytes_contains(haystack, needle))
}

fn parse_json_bytes(record: &[u8]) -> Option<Value> {
    let start = record.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let end = record
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())?
        + 1;
    let trimmed = &record[start..end];
    if let Ok(value) = serde_json::from_slice::<Value>(trimmed) {
        return Some(value);
    }
    let text = String::from_utf8_lossy(trimmed);
    serde_json::from_str::<Value>(text.trim()).ok()
}

/// Streams a plain or zstd-compressed JSONL file with bounded memory and only
/// invokes JSON parsing on lines matching at least one byte needle.
fn visit_matching_json_lines(path: &Path, needles: &[&[u8]], mut on_value: impl FnMut(Value)) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let Ok(metadata) = file.metadata() else {
        return;
    };
    let length = metadata.len();
    if length > MAX_FILE_BYTES {
        return;
    }
    let mut input = BufReader::new(file.take(length));
    let Ok(prefix) = input.fill_buf() else {
        return;
    };
    let compressed = prefix.starts_with(&ZSTD_MAGIC);
    let mut reader: Box<dyn BufRead> = if compressed {
        // A bounded decoder is sufficient for plain-text title events; do not
        // allocate an entire transcript or retain any of its message content.
        let Ok(mut decoder) = zstd::stream::read::Decoder::with_buffer(input) else {
            return;
        };
        if decoder.window_log_max(26).is_err() {
            return;
        }
        Box::new(BufReader::new(decoder.take(MAX_DECODED_BYTES)))
    } else {
        Box::new(input.take(MAX_DECODED_BYTES))
    };

    let mut record = Vec::with_capacity(8 * 1024);
    let mut decoded: u64 = 0;
    loop {
        record.clear();
        if record.capacity() > 256 * 1024 {
            record.shrink_to(64 * 1024);
        }
        match reader
            .by_ref()
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut record)
        {
            Ok(0) => break,
            Ok(count) => {
                decoded = decoded.saturating_add(count as u64);
                if record.len() as u64 > MAX_RECORD_BYTES {
                    // Read errors or oversized records stop this metadata lane;
                    // usage parsing remains upstream's responsibility.
                    break;
                }
                if matches_any(&record, needles)
                    && let Some(value) = parse_json_bytes(&record)
                {
                    on_value(value);
                }
                if decoded >= MAX_DECODED_BYTES {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
fn json_line(line: &str) -> Option<Value> {
    (!line.is_empty())
        .then(|| serde_json::from_str::<Value>(line).ok())
        .flatten()
}

fn title_from_dsh_line(value: &Value) -> Option<String> {
    (value.get("type")?.as_str()? == "session/title")
        .then(|| metadata_text(value.pointer("/data/title")?.as_str()))
        .flatten()
}

fn dsh_file(path: &Path) -> Option<String> {
    let mut title = None;
    visit_matching_json_lines(
        path,
        &[b"\"session/title\"", b"\"session\\/title\""],
        |value| {
            if let Some(next) = title_from_dsh_line(&value) {
                title = Some(next);
            }
        },
    );
    title
}

fn transcript_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

fn dsh_transcript(path: &Path) -> bool {
    let Some(name) = transcript_name(path) else {
        return false;
    };
    let base = name.strip_suffix(".zstd").unwrap_or(name);
    base == "session.jsonl"
        || base
            .strip_prefix("session.v")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
            .is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
            })
}

fn metadata_files(root: &Path, accept: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut paths = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && accept(entry.path()))
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[cfg(test)]
fn read_dsh(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    read_dsh_for(home, use_env_roots, None)
}

fn read_dsh_for(
    home: &Path,
    use_env_roots: bool,
    needed: Option<&HashSet<String>>,
) -> BTreeMap<String, String> {
    let root = if use_env_roots {
        std::env::var_os("DSH_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".dsh"))
    } else {
        home.join(".dsh")
    }
    .join("sessions");
    metadata_files(&root, |path| {
        if !dsh_transcript(path) {
            return false;
        }
        match needed {
            Some(ids) => path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|id| ids.contains(id)),
            None => true,
        }
    })
    .into_iter()
    .filter_map(|path| {
        let id = path.parent()?.file_name()?.to_str()?.to_owned();
        dsh_file(&path).map(|title| (id, title))
    })
    .collect()
}

/// Last title record wins. Forked DSH logs retain the parent's title records;
/// assigning by their own root session keeps that copied history identical to
/// the original without merging sessions or changing usage identity.
fn title_priority(kind: &str) -> u8 {
    match kind {
        "summary" | "ai-title" => 1,
        "custom-title" => 2,
        _ => 0,
    }
}

fn title_from_claude_line(value: &Value) -> Option<(u8, String)> {
    let kind = value.get("type")?.as_str()?;
    let title = match kind {
        "summary" | "ai-title" => value
            .get("summary")
            .or_else(|| value.get("title"))
            .and_then(Value::as_str),
        "custom-title" => value
            .get("customTitle")
            .or_else(|| value.get("title"))
            .and_then(Value::as_str),
        _ => None,
    };
    metadata_text(title).map(|title| (title_priority(kind), title))
}

fn claude_file(path: &Path) -> BTreeMap<String, (u8, String)> {
    let fallback = path
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let mut titles = BTreeMap::new();
    visit_matching_json_lines(
        path,
        &[b"\"summary\"", b"\"ai-title\"", b"\"custom-title\""],
        |value| {
            let Some((priority, title)) = title_from_claude_line(&value) else {
                return;
            };
            let session = value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| fallback.clone());
            let Some(session) = session else {
                return;
            };
            if titles
                .get(&session)
                .is_none_or(|(current, _): &(u8, String)| priority >= *current)
            {
                titles.insert(session, (priority, title));
            }
        },
    );
    titles
}

fn claude_root(home: &Path, use_env_roots: bool) -> PathBuf {
    if use_env_roots {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"))
    } else {
        home.join(".claude")
    }
    .join("projects")
}

fn read_claude_for(
    home: &Path,
    use_env_roots: bool,
    needed: Option<&HashSet<String>>,
) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    for path in metadata_files(&claude_root(home, use_env_roots), |path| {
        path.extension()
            .is_some_and(|extension| extension == "jsonl")
    }) {
        for (id, (priority, title)) in claude_file(&path) {
            if needed.is_some_and(|ids| !ids.contains(id.as_str())) {
                continue;
            }
            if titles
                .get(&id)
                .is_none_or(|(current, _): &(u8, String)| priority >= *current)
            {
                titles.insert(id, (priority, title));
            }
        }
    }
    titles
        .into_iter()
        .map(|(id, (_, title))| (id, title))
        .collect()
}

fn root_path(home: &Path, use_env_roots: bool, env: &str, fallback: &str) -> PathBuf {
    if use_env_roots {
        std::env::var_os(env)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(fallback))
    } else {
        home.join(fallback)
    }
}

fn json_file(path: &Path) -> Option<Value> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn explicit_title(value: &Value) -> Option<String> {
    metadata_text(
        value
            .get("title")
            .or_else(|| value.get("sessionTitle"))
            .or_else(|| value.get("displayName"))
            .and_then(Value::as_str),
    )
}

fn read_json_tree_titles(
    root: &Path,
    accepted_name: impl Fn(&Path) -> bool,
    title_id: impl Fn(&Path, &Value) -> Option<String>,
) -> BTreeMap<String, String> {
    metadata_files(root, accepted_name)
        .into_iter()
        .filter_map(|path| {
            let value = json_file(&path)?;
            let id = title_id(&path, &value)?;
            explicit_title(&value).map(|title| (id, title))
        })
        .collect()
}

fn read_amp(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    let mut titles = read_json_tree_titles(
        &root_path(home, use_env_roots, "XDG_DATA_HOME", ".local/share").join("amp/threads"),
        |path| {
            transcript_name(path)
                .is_some_and(|name| name.starts_with("T-") && name.ends_with(".json"))
        },
        |path, value| {
            value
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| path.file_stem()?.to_str().map(str::to_owned))
        },
    );
    if use_env_roots {
        if let Some(root) = std::env::var_os("TOKSCALE_CONFIG_DIR") {
            titles.extend(read_json_tree_titles(
                &PathBuf::from(root).join("amp-cache/threads"),
                |path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                },
                |path, value| {
                    value
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| path.file_stem()?.to_str().map(str::to_owned))
                },
            ));
        }
    } else {
        titles.extend(read_json_tree_titles(
            &home.join(".config/tokscale/amp-cache/threads"),
            |path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            },
            |path, value| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| path.file_stem()?.to_str().map(str::to_owned))
            },
        ));
    }
    titles
}

fn read_kiro(home: &Path) -> BTreeMap<String, String> {
    read_json_tree_titles(
        &home.join(".kiro/sessions"),
        |path| transcript_name(path) == Some("session.json"),
        |_path, value| value.get("id").and_then(Value::as_str).map(str::to_owned),
    )
}

fn sqlite_json_titles(path: &Path, query: &str) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    if !path.is_file() {
        return titles;
    }
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return titles;
    };
    let _ = connection.pragma_update(None, "query_only", "ON");
    let Ok(mut statement) = connection.prepare(query) else {
        return titles;
    };
    let Ok(mut rows) = statement.query([]) else {
        return titles;
    };
    while let Ok(Some(row)) = rows.next() {
        let id = row.get::<_, Option<String>>(0).ok().flatten();
        let data = row.get::<_, Option<Vec<u8>>>(1).ok().flatten();
        let data_type = row
            .get::<_, Option<String>>(2)
            .ok()
            .flatten()
            .unwrap_or_else(|| "json".into());
        let Some((id, data)) = id.zip(data) else {
            continue;
        };
        let bytes = if data_type.eq_ignore_ascii_case("zstd") {
            zstd::stream::read::Decoder::new(data.as_slice())
                .ok()
                .map(|decoder| decoder.take(32 * 1024 * 1024 + 1))
                .and_then(|mut decoder| {
                    let mut bytes = Vec::new();
                    decoder.read_to_end(&mut bytes).ok().map(|_| bytes)
                })
        } else if data_type.eq_ignore_ascii_case("json") {
            Some(data)
        } else {
            None
        };
        if let Some(value) = bytes
            .filter(|bytes| bytes.len() <= 32 * 1024 * 1024)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            && let Some(title) = explicit_title(&value)
        {
            titles.insert(id, title);
        }
    }
    titles
}

fn data_root(home: &Path, use_env_roots: bool) -> PathBuf {
    if use_env_roots {
        if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
            return PathBuf::from(path);
        }
        #[cfg(target_os = "windows")]
        if let Some(path) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
            return PathBuf::from(path);
        }
    }
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        home.join("AppData/Local")
    } else {
        home.join(".local/share")
    }
}

fn read_zed(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    sqlite_json_titles(
        &data_root(home, use_env_roots).join("zed/threads/threads.db"),
        "SELECT id, data, data_type FROM threads",
    )
}

fn read_crush(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    for registry in crush_registries(home, use_env_roots) {
        let Some(Value::Object(list)) = json_file(&registry) else {
            continue;
        };
        let Some(projects) = list.get("projects").and_then(Value::as_array) else {
            continue;
        };
        for project in projects {
            let Some(project_path) = project.get("path").and_then(Value::as_str) else {
                continue;
            };
            let Some(data_dir) = project
                .get("data_dir")
                .or_else(|| project.get("dataDir"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let data_dir = PathBuf::from(data_dir);
            let data_dir = if data_dir.is_absolute() {
                data_dir
            } else {
                PathBuf::from(project_path).join(data_dir)
            };
            let db_path = data_dir.join("crush.db");
            for (id, title) in sqlite_titles(&db_path, "SELECT id, NULLIF(title, '') FROM sessions")
            {
                titles.insert(format!("{}:{id}", db_path.to_string_lossy()), title);
            }
        }
    }
    titles
}

fn crush_registries(home: &Path, use_env_roots: bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if use_env_roots
        && let Some(path) = std::env::var_os("CRUSH_GLOBAL_DATA").filter(|value| !value.is_empty())
    {
        paths.push(PathBuf::from(path).join("projects.json"));
    }
    paths.push(data_root(home, use_env_roots).join("crush/projects.json"));
    paths.push(home.join("AppData/Local/crush/projects.json"));
    paths.sort();
    paths.dedup();
    paths
}

fn read_devin_for(
    home: &Path,
    use_env_roots: bool,
    needed: Option<&HashSet<String>>,
) -> BTreeMap<String, String> {
    let path = data_root(home, use_env_roots).join("devin/cli/sessions.db");
    let mut titles = sqlite_titles(&path, "SELECT id, NULLIF(title, '') FROM sessions");
    let root = home.join("Library/Application Support/Devin/User/acp-events");
    for path in metadata_files(&root, |path| {
        if !path
            .extension()
            .is_some_and(|extension| extension == "ndjson")
        {
            return false;
        }
        match needed {
            Some(ids) => path
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|id| ids.contains(id)),
            None => true,
        }
    }) {
        let file_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        visit_matching_json_lines(&path, &[b"\"session_info_update\""], |value| {
            if value
                .pointer("/notification/sessionUpdate")
                .and_then(Value::as_str)
                == Some("session_info_update")
                && let Some(title) =
                    metadata_text(value.pointer("/notification/title").and_then(Value::as_str))
            {
                titles.insert(file_id.to_owned(), title);
            }
        });
    }
    titles
}
fn openclaw_index(path: &Path) -> BTreeMap<String, String> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return BTreeMap::new();
    }
    let Ok(Value::Object(index)) = serde_json::from_slice::<Value>(&bytes) else {
        return BTreeMap::new();
    };
    let mut titles = BTreeMap::new();
    for value in index.values() {
        let Some(object) = value.as_object() else {
            continue;
        };
        let id = metadata_text(
            object
                .get("sessionId")
                .or_else(|| object.get("session_id"))
                .and_then(Value::as_str),
        );
        let title = metadata_text(
            object
                .get("displayName")
                .or_else(|| object.get("display_name"))
                .or_else(|| object.get("title"))
                .and_then(Value::as_str),
        );
        if let (Some(id), Some(title)) = (id, title) {
            titles.insert(id, title);
        }
    }
    titles
}

fn read_openclaw(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    let root = root_path(home, use_env_roots, "OPENCLAW_HOME", ".openclaw").join("agents");
    let mut titles = BTreeMap::new();
    for path in metadata_files(&root, |path| transcript_name(path) == Some("sessions.json")) {
        titles.extend(openclaw_index(&path));
    }
    titles
}

/// Read only a known metadata column. Unsupported/older schemas return no
/// title rather than guessing or opening a database read-write.
fn sqlite_titles(path: &Path, query: &str) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    if !path.is_file() {
        return titles;
    }
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return titles;
    };
    let _ = connection.pragma_update(None, "query_only", "ON");
    let Ok(mut statement) = connection.prepare(query) else {
        return titles;
    };
    let Ok(rows) = statement.query([]) else {
        return titles;
    };
    let mut rows = rows;
    while let Ok(Some(row)) = rows.next() {
        let id = row
            .get::<_, Option<String>>(0)
            .ok()
            .flatten()
            .and_then(|value| metadata_text(Some(&value)));
        let title = row
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .and_then(|value| metadata_text(Some(&value)));
        if let (Some(id), Some(title)) = (id, title) {
            titles.insert(id, title);
        }
    }
    titles
}

fn read_codex_for(
    home: &Path,
    use_env_roots: bool,
    needed: Option<&HashSet<String>>,
) -> BTreeMap<String, String> {
    let path = root_path(home, use_env_roots, "CODEX_HOME", ".codex").join("session_index.jsonl");
    let mut titles = BTreeMap::new();
    visit_matching_json_lines(&path, &[b"\"thread_name\""], |value| {
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            return;
        };
        if needed.is_some_and(|ids| !ids.contains(id)) {
            return;
        }
        if let Some(title) = metadata_text(value.get("thread_name").and_then(Value::as_str)) {
            titles.insert(id.to_owned(), title);
        }
    });
    titles
}

#[cfg(test)]
fn read_senpi(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    read_senpi_for(home, use_env_roots, None)
}

fn read_senpi_for(
    home: &Path,
    use_env_roots: bool,
    needed: Option<&HashSet<String>>,
) -> BTreeMap<String, String> {
    let root = root_path(
        home,
        use_env_roots,
        "SENPI_CODING_AGENT_DIR",
        ".senpi/agent",
    )
    .join("sessions");
    let mut titles = BTreeMap::new();
    for path in metadata_files(&root, |path| {
        path.extension()
            .is_none_or(|extension| extension == "jsonl" || extension == "zst")
    }) {
        let mut id = None;
        let mut title = None;
        visit_matching_json_lines(
            &path,
            &[b"\"session\"", b"\"session_info\""],
            |value| match value.get("type").and_then(Value::as_str) {
                Some("session") => id = value.get("id").and_then(Value::as_str).map(str::to_owned),
                Some("session_info") => {
                    title = metadata_text(value.get("name").and_then(Value::as_str))
                }
                _ => {}
            },
        );
        if let (Some(id), Some(title)) = (id, title)
            && needed.is_none_or(|ids| ids.contains(id.as_str()))
        {
            titles.insert(id, title);
        }
    }
    titles
}

fn read_copilot(home: &Path, use_env_roots: bool) -> BTreeMap<String, String> {
    let root = if use_env_roots {
        std::env::var_os("COPILOT_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".copilot"))
    } else {
        home.join(".copilot")
    };
    sqlite_titles(
        &root.join("data.db"),
        "SELECT id, NULLIF(title, '') FROM sessions",
    )
}

fn read_zcode(home: &Path) -> BTreeMap<String, String> {
    let path = home.join(".zcode").join("cli").join("db").join("db.sqlite");
    let mut titles = sqlite_titles(
        &path,
        "SELECT NULLIF(id, ''), NULLIF(title, '') FROM session",
    );
    titles.extend(sqlite_titles(
        &home.join(".zcode/v2/tasks-index.sqlite"),
        "SELECT task_id, NULLIF(title, '') FROM tasks WHERE COALESCE(deleted, 0) = 0",
    ));
    titles
}

fn read_qoder_metadata(connection: &rusqlite::Connection) -> BTreeMap<String, String> {
    sqlite_titles_on(
        connection,
        "SELECT NULLIF(session_id, ''), NULLIF(title, '') FROM chat_session",
    )
}

fn sqlite_titles_on(connection: &rusqlite::Connection, query: &str) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    let Ok(mut statement) = connection.prepare(query) else {
        return titles;
    };
    let Ok(mut rows) = statement.query([]) else {
        return titles;
    };
    while let Ok(Some(row)) = rows.next() {
        let id = row
            .get::<_, Option<String>>(0)
            .ok()
            .flatten()
            .and_then(|value| metadata_text(Some(&value)));
        let title = row
            .get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .and_then(|value| metadata_text(Some(&value)));
        if let (Some(id), Some(title)) = (id, title) {
            titles.insert(id, title);
        }
    }
    titles
}

pub fn qoder_session_titles(connection: &rusqlite::Connection) -> BTreeMap<String, String> {
    read_qoder_metadata(connection)
}

pub fn metadata_watch_roots(home: &Path, use_env_roots: bool) -> Vec<crate::roots::WatchRoot> {
    let mut roots = Vec::new();
    let mut push = |client: &str, path: PathBuf| {
        roots.push(crate::roots::WatchRoot {
            client: client.to_owned(),
            path,
            metadata_only: true,
        })
    };
    let codex = root_path(home, use_env_roots, "CODEX_HOME", ".codex");
    push("codex", codex.join("session_index.jsonl"));
    // Watch only the title database (and its -wal via RootMap), not the whole
    // ~/.zcode/v2 directory, so unrelated desktop files never trigger scans.
    push("zcode", home.join(".zcode/v2/tasks-index.sqlite"));
    push("kiro", home.join(".kiro/sessions"));
    push("copilot", home.join(".copilot/data.db"));
    push(
        "openclaw",
        root_path(home, use_env_roots, "OPENCLAW_HOME", ".openclaw").join("agents"),
    );
    for path in crush_registries(home, use_env_roots) {
        push("crush", path.clone());
        let Some(Value::Object(list)) = json_file(&path) else {
            continue;
        };
        for project in list
            .get("projects")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(project_path) = project.get("path").and_then(Value::as_str) else {
                continue;
            };
            let Some(data_dir) = project
                .get("data_dir")
                .or_else(|| project.get("dataDir"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let data_dir = PathBuf::from(data_dir);
            push(
                "crush",
                if data_dir.is_absolute() {
                    data_dir
                } else {
                    PathBuf::from(project_path).join(data_dir)
                },
            );
        }
    }
    push(
        "devin-cli",
        data_root(home, use_env_roots).join("devin/cli/sessions.db"),
    );
    push(
        "devin-cli",
        home.join("AppData/Roaming/devin/cli/sessions.db"),
    );
    push("devin-desktop", home.join(".config/Devin/User/acp-events"));
    push("devin-desktop", home.join(".config/devin/User/acp-events"));
    push(
        "devin-desktop",
        home.join("AppData/Roaming/Devin/User/acp-events"),
    );
    push(
        "devin-desktop",
        home.join("Library/Application Support/Devin/User/acp-events"),
    );
    roots.sort_by(|a, b| a.path.cmp(&b.path));
    roots.dedup_by(|a, b| a.path == b.path && a.client == b.client);
    roots
}

/// The locked Codex parser prefixes the logical thread UUID with a stable
/// 28-byte scope. Only the canonical UUID suffix is a valid index key; never
/// use title or first-message fields to guess an association.
fn codex_uuid_suffix(session_id: &str) -> Option<&str> {
    (session_id.len() == 64)
        .then(|| session_id.get(28..))
        .flatten()
        .filter(|id| {
            id.len() == 36
                && [8, 13, 18, 23]
                    .iter()
                    .all(|position| id.as_bytes()[*position] == b'-')
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        })
}

pub fn enrich_messages(messages: &mut [UnifiedMessage], home: &Path, use_env_roots: bool) {
    let mut needed_by_client: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for message in messages.iter() {
        if message.session_title.is_some() {
            continue;
        }
        let client = message.client.as_str();
        if !matches!(
            client,
            "devin-cli"
                | "devin-desktop"
                | "codex"
                | "senpi"
                | "copilot"
                | "amp"
                | "kiro"
                | "zed"
                | "crush"
                | "dsh"
                | "claude"
                | "openclaw"
                | "zcode"
        ) {
            continue;
        }
        let ids = needed_by_client.entry(client.to_owned()).or_default();
        ids.insert(message.session_id.to_string());
        if client == "codex"
            && let Some(uuid) = codex_uuid_suffix(message.session_id.as_str())
        {
            ids.insert(uuid.to_owned());
        }
    }
    if needed_by_client.is_empty() {
        return;
    }

    let mut by_client: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (client, needed) in &needed_by_client {
        let titles = match client.as_str() {
            "devin-cli" | "devin-desktop" => read_devin_for(home, use_env_roots, Some(needed)),
            "codex" => read_codex_for(home, use_env_roots, Some(needed)),
            "senpi" => read_senpi_for(home, use_env_roots, Some(needed)),
            "copilot" => read_copilot(home, use_env_roots),
            "amp" => read_amp(home, use_env_roots),
            "kiro" => read_kiro(home),
            "zed" => read_zed(home, use_env_roots),
            "crush" => read_crush(home, use_env_roots),
            "dsh" => read_dsh_for(home, use_env_roots, Some(needed)),
            "claude" => read_claude_for(home, use_env_roots, Some(needed)),
            "openclaw" => read_openclaw(home, use_env_roots),
            "zcode" => read_zcode(home),
            _ => continue,
        };
        by_client.insert(client.clone(), titles);
    }

    for message in messages.iter_mut() {
        if message.session_title.is_some() {
            continue;
        }
        let client = message.client.as_str();
        let Some(titles) = by_client.get(client) else {
            continue;
        };
        if let Some(title) = titles.get(message.session_id.as_str()).or_else(|| {
            (client == "codex")
                .then(|| codex_uuid_suffix(message.session_id.as_str()))
                .flatten()
                .and_then(|id| titles.get(id))
        }) {
            message.session_title = Some(title.as_str().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::{TokenBreakdown, sessions::SharedStr};

    #[test]
    fn dsh_reads_latest_log_title_without_using_message_content() {
        let first = r#"{"type":"session/title","seq":4,"data":{"title":"Initial summary","source":{"kind":"provider"}},"private":"do not copy"}"#;
        let second = r#"{"type":"session/title","seq":8,"data":{"title":"Renamed session","source":{"kind":"user"}}}"#;
        let private = r#"{"type":"user/message","data":{"content":[{"text":"not a title"}]}}"#;
        assert_eq!(
            title_from_dsh_line(&json_line(first).unwrap()).as_deref(),
            Some("Initial summary")
        );
        assert_eq!(
            title_from_dsh_line(&json_line(second).unwrap()).as_deref(),
            Some("Renamed session")
        );
        assert!(title_from_dsh_line(&json_line(private).unwrap()).is_none());
    }

    #[test]
    fn plain_and_compressed_dsh_logs_use_latest_title() {
        let home = tempfile::tempdir().unwrap();
        for compressed in [false, true] {
            let root = home.path().join(if compressed {
                ".dsh/sessions/project/compressed"
            } else {
                ".dsh/sessions/project/plain"
            });
            std::fs::create_dir_all(&root).unwrap();
            let text = concat!(
                "{\"type\":\"session\",\"id\":\"unused\",\"cwd\":\"/private\"}\n",
                "{\"type\":\"user/message\",\"data\":{\"content\":[{\"text\":\"never copy\"}]}}\n",
                "{\"type\":\"session/title\",\"seq\":4,\"data\":{\"title\":\"Agent summary\"}}\n",
                "{\"type\":\"session/title\",\"seq\":8,\"data\":{\"title\":\"Renamed by user\"}}\n"
            );
            let path = root.join(if compressed {
                "session.v2.jsonl.zstd"
            } else {
                "session.jsonl"
            });
            if compressed {
                std::fs::write(&path, zstd::encode_all(text.as_bytes(), 1).unwrap()).unwrap();
            } else {
                std::fs::write(&path, text).unwrap();
            }
            assert_eq!(dsh_file(&path).as_deref(), Some("Renamed by user"));
        }
        let titles = read_dsh(home.path(), false);
        assert_eq!(
            titles.get("compressed").map(String::as_str),
            Some("Renamed by user")
        );
        assert_eq!(
            titles.get("plain").map(String::as_str),
            Some("Renamed by user")
        );
    }

    #[test]
    fn metadata_is_bounded_and_never_control_text() {
        assert!(metadata_text(None).is_none());
        assert!(metadata_text(Some(" \t ")).is_none());
        assert!(metadata_text(Some("bad\ntitle")).is_none());
        assert!(metadata_text(Some(&"x".repeat(MAX_TITLE_BYTES + 1))).is_none());
        assert_eq!(
            metadata_text(Some("  Project title  ")).as_deref(),
            Some("Project title")
        );
    }

    #[test]
    fn enrichment_preserves_every_usage_field_and_event_identity() {
        let home = tempfile::tempdir().unwrap();
        let session = home.path().join(".dsh/sessions/project/dsh-session");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("session.jsonl"),
            "{\"type\":\"session/title\",\"data\":{\"title\":\"A stored title\"}}\n",
        )
        .unwrap();
        let mut messages = vec![UnifiedMessage::new_with_dedup(
            "dsh",
            "deepseek-test",
            "synthetic-provider",
            "dsh-session",
            1_700_000_000_000,
            TokenBreakdown {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write: 4,
                cache_write_1h: 0,
                reasoning: 5,
            },
            0.25,
            Some("synthetic-dedup".into()),
        )];
        let before = serde_json::to_string(&messages[0]).unwrap();
        enrich_messages(&mut messages, home.path(), false);
        assert_eq!(messages[0].session_title.as_deref(), Some("A stored title"));
        let mut after: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&messages[0]).unwrap()).unwrap();
        after.as_object_mut().unwrap().remove("sessionTitle");
        let before: serde_json::Value = serde_json::from_str(&before).unwrap();
        after
            .as_object_mut()
            .unwrap()
            .insert("session_title".into(), Value::Null);
        assert_eq!(after, before);
    }

    #[test]
    fn explicit_claude_title_beats_automatic_summary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, concat!(
            "{\"type\":\"summary\",\"summary\":\"Auto title\",\"sessionId\":\"s1\"}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"Chosen title\",\"sessionId\":\"s1\"}\n",
            "{\"type\":\"ai-title\",\"title\":\"Later auto title\",\"sessionId\":\"s1\"}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"never title\"}}\n"
        )).unwrap();
        assert_eq!(
            claude_file(&path)
                .get("s1")
                .map(|(_, title)| title.as_str()),
            Some("Chosen title")
        );
    }

    #[test]
    fn openclaw_reads_only_explicit_display_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        std::fs::write(&path, r#"{"agent:main:main":{"sessionId":"session-a","displayName":"Chosen name","prompt":"not read"},"bad":{"title":"No id"}}"#).unwrap();
        assert_eq!(
            openclaw_index(&path).get("session-a").map(String::as_str),
            Some("Chosen name")
        );
    }

    #[test]
    fn senpi_uses_header_identity_and_last_explicit_name_not_the_filename() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".senpi/agent/sessions/project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("file-name-is-not-session-id.jsonl"),
            concat!(
                "{\"type\":\"session\",\"id\":\"actual-session-id\"}\n",
                "{\"type\":\"session_info\",\"name\":\"First title\"}\n",
                "{\"type\":\"session_info\",\"name\":\"Latest title\"}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            read_senpi(home.path(), false)
                .get("actual-session-id")
                .map(String::as_str),
            Some("Latest title")
        );
        assert!(!read_senpi(home.path(), false).contains_key("file-name-is-not-session-id"));
    }

    #[test]
    fn codex_only_matches_a_canonical_thread_uuid_suffix() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex/session_index.jsonl"), concat!(
            "{\"id\":\"12345678-1234-1234-1234-123456789abc\",\"thread_name\":\"Named thread\"}\n",
            "{\"id\":\"12345678-1234-1234-1234-123456789abc\",\"thread_name\":\"Renamed thread\"}\n"
        )).unwrap();
        let make = |id: &str| {
            UnifiedMessage::new("codex", "m", "p", id, 1, TokenBreakdown::default(), 0.0)
        };
        let mut messages = vec![
            make(&format!(
                "{}{}",
                "a".repeat(28),
                "12345678-1234-1234-1234-123456789abc"
            )),
            make("12345678-1234-1234-1234-123456789abc"),
            make("unrelated"),
        ];
        enrich_messages(&mut messages, home.path(), false);
        assert_eq!(messages[0].session_title.as_deref(), Some("Renamed thread"));
        assert_eq!(messages[1].session_title.as_deref(), Some("Renamed thread"));
        assert!(messages[2].session_title.is_none());
    }
    #[test]
    fn zcode_and_qoder_titles_are_optional_and_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE session (id TEXT, title TEXT, prompt TEXT); INSERT INTO session VALUES ('z1', 'ZCode title', 'never read'); CREATE TABLE chat_session (session_id TEXT, title TEXT); INSERT INTO chat_session VALUES ('q1', 'Qoder title');").unwrap();
        let titles = sqlite_titles(&path, "SELECT id, title FROM session");
        assert_eq!(titles.get("z1").map(String::as_str), Some("ZCode title"));
        assert_eq!(
            qoder_session_titles(&connection)
                .get("q1")
                .map(String::as_str),
            Some("Qoder title")
        );
    }

    #[test]
    fn upstream_title_has_priority() {
        let mut messages = vec![UnifiedMessage::new(
            "dsh",
            "m",
            "p",
            "missing",
            1,
            TokenBreakdown::default(),
            0.0,
        )];
        messages[0].session_title = Some(SharedStr::from("Upstream title"));
        enrich_messages(&mut messages, Path::new("/not-a-real-home"), false);
        assert_eq!(messages[0].session_title.as_deref(), Some("Upstream title"));
    }

    #[test]
    fn dsh_enrichment_reads_only_requested_sessions_and_skips_unrelated_lines() {
        let home = tempfile::tempdir().unwrap();
        let wanted = home.path().join(".dsh/sessions/project/wanted-session");
        let ignored = home.path().join(".dsh/sessions/project/ignored-session");
        std::fs::create_dir_all(&wanted).unwrap();
        std::fs::create_dir_all(&ignored).unwrap();
        std::fs::write(
            wanted.join("session.jsonl"),
            concat!(
                "{\"type\":\"user/message\",\"data\":{\"text\":\"not json title\"}}\n",
                "{\"type\":\"session/title\",\"data\":{\"title\":\"Targeted DSH title\"}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            ignored.join("session.jsonl"),
            "{\"type\":\"session/title\",\"data\":{\"title\":\"Should not be loaded\"}}\n",
        )
        .unwrap();
        let only_wanted = HashSet::from(["wanted-session".to_owned()]);
        let loaded = read_dsh_for(home.path(), false, Some(&only_wanted));
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get("wanted-session").map(String::as_str),
            Some("Targeted DSH title")
        );
        assert!(!loaded.contains_key("ignored-session"));
        assert!(bytes_contains(
            b"abc\"session/title\"def",
            b"\"session/title\""
        ));
        assert!(!bytes_contains(
            b"abc\"user/message\"def",
            b"\"session/title\""
        ));
    }
}
