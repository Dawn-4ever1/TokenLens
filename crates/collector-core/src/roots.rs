//! Directories each client is read from at the locked upstream rev, and the
//! mapping from a changed path back to its client.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use tokscale_core::ClientId;

use crate::{QODER_CLIENT, QODER_CN_CLIENT};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WatchRoot {
    pub client: String,
    pub path: PathBuf,
    /// A title-only index update must refresh historical events for this
    /// client, not just the yesterday-through-today window.
    pub metadata_only: bool,
}

/// Opening a WAL-mode SQLite database read-only still maps and rewrites its
/// `-shm` shared-memory index. Watching that sidecar causes a self-triggering
/// scan loop; real writes always touch the database file itself or `-wal`.
pub(crate) fn is_sqlite_shm_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    [".db-shm", ".sqlite-shm", ".sqlite3-shm", ".vscdb-shm"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn same_component(left: Component<'_>, right: Component<'_>, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return left == right;
    }
    match (left, right) {
        (Component::Prefix(a), Component::Prefix(b)) => a
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy()),
        (Component::Normal(a), Component::Normal(b)) => a
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => left == right,
    }
}

fn path_starts_with_mode(path: &Path, base: &Path, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return path.starts_with(base);
    }
    let mut path_components = path.components();
    for base_component in base.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        if !same_component(path_component, base_component, true) {
            return false;
        }
    }
    true
}

fn same_path_mode(left: &Path, right: &Path, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return left == right;
    }
    let mut left_components = left.components();
    let mut right_components = right.components();
    loop {
        match (left_components.next(), right_components.next()) {
            (Some(a), Some(b)) if same_component(a, b, true) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn is_sqlite_wal_for_mode(path: &Path, root: &Path, case_insensitive: bool) -> bool {
    let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if ![".db", ".sqlite", ".sqlite3", ".vscdb"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return false;
    }
    let mut wal = root.as_os_str().to_os_string();
    wal.push("-wal");
    same_path_mode(path, Path::new(&wal), case_insensitive)
}

pub(crate) fn is_forbidden(path: &Path, client: &str) -> bool {
    path.components().any(
        |component| matches!(component, Component::Normal(part) if part.eq_ignore_ascii_case(".qoder-cn")),
    ) && client != QODER_CN_CLIENT
}

pub(crate) fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_dir())
}

pub(crate) fn is_real_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn t3_base_dir_from(home: &Path, use_env_roots: bool, env_value: Option<&OsStr>) -> PathBuf {
    if use_env_roots && let Some(raw) = env_value {
        let trimmed = raw.to_string_lossy();
        let trimmed = trimmed.trim();
        if !trimmed.is_empty() {
            if trimmed == "~" {
                return home.to_path_buf();
            }
            if let Some(relative) = trimmed
                .strip_prefix("~/")
                .or_else(|| trimmed.strip_prefix("~\\"))
            {
                let mut path = home.to_path_buf();
                for component in Path::new(relative).components() {
                    path.push(component.as_os_str());
                }
                return path;
            }
            let candidate = PathBuf::from(trimmed);
            if candidate.is_absolute() {
                return candidate;
            }
        }
    }
    home.join(".t3")
}

fn t3_base_dir(home: &Path, use_env_roots: bool) -> PathBuf {
    t3_base_dir_from(
        home,
        use_env_roots,
        std::env::var_os("T3CODE_HOME").as_deref(),
    )
}

/// Isolated `antigravity-acp` profile directories written by T3 Code under
/// `<T3CODE_HOME|~/.t3>/userdata/providers/antigravity/<instance>/antigravity-acp`.
pub(crate) fn t3_antigravity_acp_dirs(home: &Path, use_env_roots: bool) -> Vec<PathBuf> {
    let base = t3_base_dir(home, use_env_roots);
    if base == home || home.starts_with(&base) || !is_real_dir(&base) {
        return Vec::new();
    }
    let userdata = base.join("userdata");
    if !is_real_dir(&userdata) {
        return Vec::new();
    }
    let providers = userdata.join("providers");
    if !is_real_dir(&providers) {
        return Vec::new();
    }
    let ag_root = providers.join("antigravity");
    if !is_real_dir(&ag_root) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&ag_root) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let acp = entry.path().join("antigravity-acp");
        if !is_real_dir(&acp) {
            continue;
        }
        if !is_forbidden(&acp, "antigravity-extension") && acp != home && !home.starts_with(&acp) {
            dirs.push(acp);
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Isolated Antigravity conversation directories written by T3 Code under
/// `<T3CODE_HOME|~/.t3>/userdata/providers/antigravity/<instance>/antigravity-acp/conversations`.
pub fn t3_antigravity_conversation_dirs(home: &Path, use_env_roots: bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for acp in t3_antigravity_acp_dirs(home, use_env_roots) {
        let conv = acp.join("conversations");
        if is_real_dir(&conv)
            && !is_forbidden(&conv, "antigravity-extension")
            && conv != home
            && !home.starts_with(&conv)
        {
            dirs.push(conv);
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

pub(crate) fn t3_state_db_path(home: &Path, use_env_roots: bool) -> Option<PathBuf> {
    let base = t3_base_dir(home, use_env_roots);
    if base == home || home.starts_with(&base) || !is_real_dir(&base) {
        return None;
    }
    let userdata = base.join("userdata");
    if !is_real_dir(&userdata) {
        return None;
    }
    let db = userdata.join("statev2.sqlite");
    (is_real_file(&db) && !is_forbidden(&db, "antigravity-extension") && db != home).then_some(db)
}

pub(crate) fn standalone_antigravity_summary_db(home: &Path) -> Option<PathBuf> {
    let gemini = home.join(".gemini");
    let antigravity = gemini.join("antigravity");
    if is_symlink(&gemini) || is_symlink(&antigravity) {
        return None;
    }
    let db = antigravity.join("conversation_summaries.db");
    (!is_symlink(&db) && !is_forbidden(&db, "antigravity-extension")).then_some(db)
}

pub fn candidate_roots(home: &Path, use_env_roots: bool) -> Vec<WatchRoot> {
    let home_text = home.to_string_lossy();
    let mut roots = Vec::new();
    let mut push = |client: &str, path: PathBuf| {
        roots.push(WatchRoot {
            client: client.to_owned(),
            path,
            metadata_only: false,
        });
    };
    for client in ClientId::iter() {
        if client.as_str() == "qoder" || !client.parse_local() {
            continue;
        }
        // clients.rs:286 is the primary root that scanner.rs pushes per client.
        let primary = client
            .data()
            .resolve_path_with_env_strategy(&home_text, use_env_roots);
        push(client.as_str(), PathBuf::from(primary));
    }
    // scanner.rs:2639-2663 also reads <CODEX_HOME>/archived_sessions.
    let codex_home = use_env_roots
        .then(|| {
            std::env::var_os("CODEX_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .flatten()
        .unwrap_or_else(|| home.join(".codex"));
    push("codex", codex_home.join("archived_sessions"));
    // scanner.rs:2704-2712: legacy OpenClaw rebrand directories.
    for legacy in [".clawdbot", ".moltbot", ".moldbot"] {
        push("openclaw", home.join(legacy).join("agents"));
    }
    // scanner.rs:3008-3013: the newer ZCode SQLite store (+ -wal/-shm).
    push("zcode", home.join(".zcode").join("cli").join("db"));
    // Title metadata sources live outside some usage roots and may change on
    // their own. Keep these roots shared with the actual metadata readers.
    push(
        QODER_CLIENT,
        crate::qoder::projects_root_intl(home, use_env_roots),
    );
    push(
        QODER_CN_CLIENT,
        crate::qoder::projects_root_cn(home, use_env_roots),
    );
    // Qoder IDE's SQLite store is distinct from the newer Desktop transcripts.
    // Watch its parent for local.db and -wal; -shm is ignored to prevent
    // read-only SQLite opens from re-triggering scans.
    if let Some(parent) = crate::qoder_ide::database_path(home, use_env_roots).parent() {
        push(QODER_CLIENT, parent.to_path_buf());
    }
    if let Some(parent) = crate::qoder_ide::database_path_cn(home, use_env_roots).parent() {
        push(crate::QODER_CN_CLIENT, parent.to_path_buf());
    }
    for dir in t3_antigravity_conversation_dirs(home, use_env_roots) {
        push("antigravity-extension", dir);
    }
    roots.extend(crate::titles::metadata_watch_roots(home, use_env_roots));
    roots.retain(|root| {
        !is_forbidden(&root.path, &root.client)
            // A root equal to (or above) home would mean watching everything.
            && root.path != home
            && !home.starts_with(&root.path)
    });
    // Sort places `metadata_only: false` before `true` for the same
    // `(client, path)`, so `dedup_by` keeps the primary usage root rather than
    // registering duplicate watches or turning normal log writes into full
    // metadata rescans.
    roots.sort();
    roots.dedup_by(|later, earlier| later.client == earlier.client && later.path == earlier.path);
    roots
}

/// Existing directories of the detected clients; nested duplicates of the
/// same client and kind are dropped because directory watches are recursive.
pub fn watch_roots(
    home: &Path,
    use_env_roots: bool,
    detected: &BTreeSet<String>,
) -> Vec<WatchRoot> {
    let existing = candidate_roots(home, use_env_roots)
        .into_iter()
        .filter(|root| {
            detected.contains(&root.client) && (is_real_dir(&root.path) || is_real_file(&root.path))
        })
        .collect::<Vec<_>>();
    existing
        .iter()
        .filter(|root| {
            !existing.iter().any(|other| {
                other.client == root.client
                    && other.metadata_only == root.metadata_only
                    && !same_path_mode(&other.path, &root.path, cfg!(windows))
                    && path_starts_with_mode(&root.path, &other.path, cfg!(windows))
            })
        })
        .cloned()
        .collect()
}

fn canonicalize_best_effort(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Some(canonical);
    }
    let mut missing = Vec::new();
    let mut ancestor = path;
    while let Some(name) = ancestor.file_name() {
        missing.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() {
            break;
        }
        if let Ok(mut canonical) = std::fs::canonicalize(parent) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
        ancestor = parent;
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct RootMap {
    roots: Vec<(PathBuf, String, bool, bool)>,
    ignored: Vec<PathBuf>,
}

impl RootMap {
    /// `ignored` holds this project's own directories.
    pub fn new(roots: &[WatchRoot], ignored: &[PathBuf]) -> Self {
        let with_canonical = |path: &Path| {
            let mut variants = vec![path.to_path_buf()];
            if let Some(canonical) = canonicalize_best_effort(path)
                && canonical != path
            {
                variants.push(canonical);
            }
            variants
        };
        Self {
            roots: roots
                .iter()
                .flat_map(|root| {
                    let is_dir = is_real_dir(&root.path);
                    with_canonical(&root.path)
                        .into_iter()
                        .map(move |path| (path, root.client.clone(), root.metadata_only, is_dir))
                })
                .collect(),
            ignored: ignored
                .iter()
                .flat_map(|path| with_canonical(path))
                .collect(),
        }
    }

    /// The client whose root is the longest prefix of `path`.
    pub fn client_for(&self, path: &Path) -> Option<(&str, bool)> {
        self.client_for_with_case(path, cfg!(windows))
    }

    fn client_for_with_case(&self, path: &Path, case_insensitive: bool) -> Option<(&str, bool)> {
        if self
            .ignored
            .iter()
            .any(|own| path_starts_with_mode(path, own, case_insensitive))
            || is_sqlite_shm_sidecar(path)
        {
            return None;
        }
        self.roots
            .iter()
            .filter(|(root, _, metadata_only, is_dir)| {
                if *metadata_only {
                    same_path_mode(path, root, case_insensitive)
                        || is_sqlite_wal_for_mode(path, root, case_insensitive)
                        || (*is_dir && path_starts_with_mode(path, root, case_insensitive))
                } else {
                    path_starts_with_mode(path, root, case_insensitive)
                        || is_sqlite_wal_for_mode(path, root, case_insensitive)
                }
            })
            .max_by_key(|(root, _, _, _)| root.components().count())
            .map(|(_, client, metadata_only, _)| {
                let is_openclaw_index = client == "openclaw"
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            if case_insensitive {
                                name.eq_ignore_ascii_case("sessions.json")
                            } else {
                                name == "sessions.json"
                            }
                        });
                let is_metadata = *metadata_only || is_openclaw_index;
                (client.as_str(), is_metadata)
            })
    }
}

/// Directories written by this project, which must never trigger scans.
pub fn own_directories() -> Vec<PathBuf> {
    [dirs::data_dir(), dirs::cache_dir(), dirs::config_dir()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join("tokenlens"))
        .chain(dirs::home_dir().map(|home| home.join(".config").join("tokenlens")))
        .collect()
}

/// Upstream's config folder (`TOKSCALE_CONFIG_DIR`) for every scan this
/// project runs, so upstream never reads or writes `~/.config/tokscale`.
pub fn own_upstream_dir(explicit_home: Option<&Path>) -> Option<PathBuf> {
    match explicit_home {
        Some(home) => Some(home.join(".cache")),
        None => dirs::cache_dir(),
    }
    .map(|cache| cache.join("tokenlens").join("tokscale"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(clients: &[&str]) -> BTreeSet<String> {
        clients.iter().map(|client| (*client).to_owned()).collect()
    }

    #[test]
    fn title_indexes_are_watched_without_watching_the_agent_home() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".codex/sessions")).expect("sessions");
        std::fs::create_dir_all(home.path().join(".zcode/v2")).expect("desktop");
        std::fs::write(home.path().join(".codex/session_index.jsonl"), "{}\n").expect("index");
        std::fs::write(home.path().join(".zcode/v2/tasks-index.sqlite"), "").expect("tasks");
        let roots = watch_roots(home.path(), false, &detected(&["codex", "zcode"]));
        let map = RootMap::new(&roots, &[]);
        assert_eq!(
            map.client_for(&home.path().join(".codex/session_index.jsonl")),
            Some(("codex", true))
        );
        assert_eq!(
            map.client_for(&home.path().join(".zcode/v2/tasks-index.sqlite")),
            Some(("zcode", true))
        );
        assert_eq!(
            map.client_for(&home.path().join(".zcode/v2/tasks-index.sqlite-wal")),
            Some(("zcode", true))
        );
        assert_eq!(
            map.client_for(&home.path().join(".zcode/v2/tasks-index.sqlite-shm")),
            None
        );
        assert_eq!(
            map.client_for(&home.path().join(".zcode/v2/unrelated.json")),
            None
        );
        assert_eq!(
            map.client_for(&home.path().join(".codex/unrelated-state.json")),
            None
        );
    }
    #[test]
    fn cn_projects_are_watched_only_for_qoder_cn() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".qoder-cn/projects")).expect("cn");
        std::fs::create_dir_all(home.path().join(".qoder/projects")).expect("intl");
        let roots = watch_roots(
            home.path(),
            false,
            &detected(&[QODER_CLIENT, QODER_CN_CLIENT]),
        );
        assert_eq!(roots.len(), 2);
        let map = RootMap::new(&roots, &[]);
        assert_eq!(
            map.client_for(&home.path().join(".qoder-cn/projects/a.jsonl")),
            Some((QODER_CN_CLIENT, false))
        );
        assert_eq!(
            map.client_for(&home.path().join(".qoder/projects/a.jsonl")),
            Some((QODER_CLIENT, false))
        );
        assert!(candidate_roots(home.path(), false).iter().all(|root| {
            root.client != "qoder"
                && (root.client != QODER_CLIENT
                    || !root.path.to_string_lossy().contains(".qoder-cn"))
        }));
    }

    #[test]
    fn ide_wal_changes_map_to_qoder_without_selecting_cn_or_desktop() {
        let home = tempfile::tempdir().expect("home");
        let database = crate::qoder_ide::database_path(home.path(), false);
        let directory = database.parent().expect("IDE database directory");
        std::fs::create_dir_all(directory).expect("IDE fixture directory");
        let roots = watch_roots(home.path(), false, &detected(&[QODER_CLIENT]));
        let map = RootMap::new(&roots, &[]);
        for sidecar in ["local.db", "local.db-wal"] {
            assert_eq!(
                map.client_for(&directory.join(sidecar)),
                Some((QODER_CLIENT, false))
            );
        }
        assert_eq!(map.client_for(&directory.join("local.db-shm")), None);
        assert!(
            roots
                .iter()
                .all(|root| !root.path.to_string_lossy().contains("QoderCN"))
        );
        assert!(
            map.client_for(&home.path().join(".qoder-cn/projects/a.jsonl"))
                .is_none()
        );
        assert!(
            map.client_for(
                &home
                    .path()
                    .join("Library/Application Support/com.qoder.app.stable/main.sqlite")
            )
            .is_none()
        );
    }

    #[test]
    fn zcode_wal_changes_map_to_zcode_and_own_files_are_ignored() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".zcode/cli/db")).expect("zcode");
        std::fs::create_dir_all(home.path().join(".codex/sessions/2026")).expect("codex");
        let own = home.path().join(".codex/sessions/tokenlens-own");
        let roots = watch_roots(home.path(), false, &detected(&["zcode", "codex"]));
        let map = RootMap::new(&roots, std::slice::from_ref(&own));
        for sidecar in ["db.sqlite", "db.sqlite-wal"] {
            assert_eq!(
                map.client_for(&home.path().join(".zcode/cli/db").join(sidecar)),
                Some(("zcode", false))
            );
        }
        assert_eq!(
            map.client_for(&home.path().join(".zcode/cli/db/db.sqlite-shm")),
            None
        );
        assert_eq!(
            map.client_for(&home.path().join(".codex/sessions/2026/r.jsonl")),
            Some(("codex", false))
        );
        assert_eq!(map.client_for(&own.join("outbox.sqlite-wal")), None);
        assert_eq!(map.client_for(&home.path().join(".bashrc")), None);
    }

    #[test]
    fn sqlite_shm_sidecars_never_trigger_and_same_path_metadata_roots_do_not_shadow_usage() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".openclaw/agents/main/sessions"))
            .expect("openclaw");
        std::fs::create_dir_all(home.path().join(".gemini/antigravity/conversations"))
            .expect("antigravity");
        let roots = watch_roots(
            home.path(),
            false,
            &detected(&["openclaw", "antigravity-extension"]),
        );
        assert_eq!(
            roots.iter().filter(|r| r.client == "openclaw").count(),
            1,
            "shared usage/metadata path must not register duplicate watches"
        );
        let map = RootMap::new(&roots, &[]);
        assert_eq!(
            map.client_for(&home.path().join(".openclaw/agents/main/sessions/a.jsonl")),
            Some(("openclaw", false))
        );
        assert_eq!(
            map.client_for(
                &home
                    .path()
                    .join(".openclaw/agents/main/sessions/sessions.json")
            ),
            Some(("openclaw", true))
        );
        assert_eq!(
            map.client_for(
                &home
                    .path()
                    .join(".gemini/antigravity/conversations/c.db-wal")
            ),
            Some(("antigravity-extension", false))
        );
        assert_eq!(
            map.client_for(
                &home
                    .path()
                    .join(".gemini/antigravity/conversations/c.db-shm")
            ),
            None
        );
    }

    #[test]
    fn only_detected_existing_directories_are_watched() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".codex/sessions")).expect("codex");
        std::fs::create_dir_all(home.path().join(".dsh")).expect("dsh");
        let roots = watch_roots(home.path(), false, &detected(&["codex"]));
        assert!(roots.iter().all(|root| root.client == "codex"));
        assert_eq!(roots.len(), 1);
        assert!(
            candidate_roots(home.path(), false)
                .iter()
                .all(|root| root.path != home.path() && root.path.starts_with(home.path()))
        );
    }

    #[test]
    fn longest_prefix_wins_for_nested_roots() {
        let roots = [
            WatchRoot {
                client: "outer".into(),
                path: PathBuf::from("/synthetic/a"),
                metadata_only: false,
            },
            WatchRoot {
                client: "inner".into(),
                path: PathBuf::from("/synthetic/a/b"),
                metadata_only: false,
            },
        ];
        let map = RootMap::new(&roots, &[]);
        assert_eq!(
            map.client_for(Path::new("/synthetic/a/b/c")),
            Some(("inner", false))
        );
        assert_eq!(
            map.client_for(Path::new("/synthetic/a/c")),
            Some(("outer", false))
        );
    }

    #[test]
    fn t3_antigravity_conversations_are_watched_without_watching_noisy_siblings() {
        let home = tempfile::tempdir().expect("home");
        let conv_a = home
            .path()
            .join(".t3/userdata/providers/antigravity/inst-a/antigravity-acp/conversations");
        let conv_b = home
            .path()
            .join(".t3/userdata/providers/antigravity/inst-b/antigravity-acp/conversations");
        let log_dir = home
            .path()
            .join(".t3/userdata/providers/antigravity/inst-a/antigravity-acp/log");
        let brain_dir = home
            .path()
            .join(".t3/userdata/providers/antigravity/inst-a/antigravity-acp/brain/id");
        std::fs::create_dir_all(&conv_a).expect("conv_a");
        std::fs::create_dir_all(&conv_b).expect("conv_b");
        std::fs::create_dir_all(&log_dir).expect("log_dir");
        std::fs::create_dir_all(&brain_dir).expect("brain_dir");
        std::fs::write(home.path().join(".t3/userdata/statev2.sqlite"), "").expect("statev2");

        let roots = watch_roots(home.path(), false, &detected(&["antigravity-extension"]));
        let paths: Vec<PathBuf> = roots.iter().map(|root| root.path.clone()).collect();
        assert_eq!(roots.len(), 2);
        assert!(paths.contains(&conv_a));
        assert!(paths.contains(&conv_b));

        let map = RootMap::new(&roots, &[]);
        assert_eq!(
            map.client_for(&conv_a.join("s.db")),
            Some(("antigravity-extension", false))
        );
        assert_eq!(
            map.client_for(&conv_b.join("s.db-wal")),
            Some(("antigravity-extension", false))
        );
        assert_eq!(map.client_for(&conv_a.join("s.db-shm")), None);
        assert_eq!(map.client_for(&log_dir.join("acp.log")), None);
        assert_eq!(map.client_for(&brain_dir.join("transcript.jsonl")), None);
        assert_eq!(
            map.client_for(&home.path().join(".t3/userdata/statev2.sqlite-wal")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn t3_antigravity_rejects_symlinked_profile_or_conversations_dirs() {
        let home = tempfile::tempdir().expect("home");
        let outside_conv = home.path().join("outside/conversations");
        let outside_inst_conv = home
            .path()
            .join("outside-inst/antigravity-acp/conversations");
        let real_inst_acp = home
            .path()
            .join(".t3/userdata/providers/antigravity/real-inst/antigravity-acp");
        let ag_root = home.path().join(".t3/userdata/providers/antigravity");
        std::fs::create_dir_all(&outside_conv).expect("outside conv");
        std::fs::create_dir_all(&outside_inst_conv).expect("outside inst conv");
        std::fs::create_dir_all(&real_inst_acp).expect("real inst acp");
        std::os::unix::fs::symlink(&outside_conv, real_inst_acp.join("conversations"))
            .expect("symlink conversations");
        std::os::unix::fs::symlink(
            home.path().join("outside-inst"),
            ag_root.join("linked-inst"),
        )
        .expect("symlink instance");

        assert!(t3_antigravity_conversation_dirs(home.path(), false).is_empty());
    }

    #[test]
    fn t3_base_dir_resolution() {
        let home = tempfile::tempdir().expect("home");
        let custom_abs = home.path().join("custom-t3-abs");
        assert_eq!(
            t3_base_dir_from(home.path(), false, Some(custom_abs.as_os_str())),
            home.path().join(".t3")
        );
        assert_eq!(
            t3_base_dir_from(home.path(), true, Some(OsStr::new("~/custom-t3"))),
            home.path().join("custom-t3")
        );
        assert_eq!(
            t3_base_dir_from(home.path(), true, Some(custom_abs.as_os_str())),
            custom_abs
        );
        assert_eq!(
            t3_base_dir_from(home.path(), true, Some(OsStr::new("relative/path"))),
            home.path().join(".t3")
        );
        assert_eq!(
            t3_base_dir_from(home.path(), true, Some(OsStr::new("   "))),
            home.path().join(".t3")
        );
        assert_eq!(
            t3_base_dir_from(home.path(), true, Some(OsStr::new("~"))),
            home.path().to_path_buf()
        );
    }

    #[test]
    fn antigravity_summary_databases_are_watched_without_watching_t3_state_or_noisy_siblings() {
        let home = tempfile::tempdir().expect("home");
        let standalone_conv = home.path().join(".gemini/antigravity/conversations");
        let standalone_sum = home
            .path()
            .join(".gemini/antigravity/conversation_summaries.db");
        let cli_conv = home.path().join(".gemini/antigravity-cli/conversations");
        let cli_sum = home
            .path()
            .join(".gemini/antigravity-cli/conversation_summaries.db");
        let t3_acp = home
            .path()
            .join(".t3/userdata/providers/antigravity/inst-a/antigravity-acp");
        let t3_conv = t3_acp.join("conversations");
        let t3_sum = t3_acp.join("conversation_summaries.db");
        let t3_log = t3_acp.join("log");
        let t3_brain = t3_acp.join("brain/id");
        let t3_state = home.path().join(".t3/userdata/statev2.sqlite");

        std::fs::create_dir_all(&standalone_conv).expect("standalone conv");
        std::fs::create_dir_all(&cli_conv).expect("cli conv");
        std::fs::create_dir_all(&t3_conv).expect("t3 conv");
        std::fs::create_dir_all(&t3_log).expect("t3 log");
        std::fs::create_dir_all(&t3_brain).expect("t3 brain");
        std::fs::write(&standalone_sum, "").expect("standalone sum");
        std::fs::write(&cli_sum, "").expect("cli sum");
        std::fs::write(&t3_sum, "").expect("t3 sum");
        std::fs::write(&t3_state, "").expect("t3 state");

        let roots = watch_roots(
            home.path(),
            false,
            &detected(&["antigravity-extension", "antigravity-cli"]),
        );
        let map = RootMap::new(&roots, &[]);

        assert_eq!(
            map.client_for(&standalone_sum),
            Some(("antigravity-extension", true))
        );
        assert_eq!(
            map.client_for(
                &home
                    .path()
                    .join(".gemini/antigravity/conversation_summaries.db-wal")
            ),
            Some(("antigravity-extension", true))
        );
        assert_eq!(map.client_for(&cli_sum), Some(("antigravity-cli", true)));
        assert_eq!(
            map.client_for(
                &home
                    .path()
                    .join(".gemini/antigravity-cli/conversation_summaries.db-wal")
            ),
            Some(("antigravity-cli", true))
        );
        assert_eq!(
            map.client_for(&t3_sum),
            Some(("antigravity-extension", true))
        );
        assert_eq!(
            map.client_for(&t3_acp.join("conversation_summaries.db-wal")),
            Some(("antigravity-extension", true))
        );

        assert_eq!(
            map.client_for(&t3_acp.join("conversation_summaries.db-shm")),
            None
        );
        assert_eq!(
            map.client_for(&t3_acp.join("jetbox_summaries_proto.pb")),
            None
        );
        assert_eq!(map.client_for(&t3_log.join("acp.log")), None);
        assert_eq!(map.client_for(&t3_brain.join("transcript.jsonl")), None);
        assert_eq!(map.client_for(&t3_state), None);
        assert_eq!(
            map.client_for(&home.path().join(".t3/userdata/statev2.sqlite-wal")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_symlinked_summary_and_state_dbs_are_rejected() {
        let home = tempfile::tempdir().expect("home");
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        let real_outside_db = outside.join("outside.db");
        std::fs::write(&real_outside_db, "").expect("outside db");

        let standalone_dir = home.path().join(".gemini/antigravity");
        let t3_acp = home
            .path()
            .join(".t3/userdata/providers/antigravity/inst-a/antigravity-acp");
        std::fs::create_dir_all(standalone_dir.join("conversations")).expect("standalone conv");
        std::fs::create_dir_all(t3_acp.join("conversations")).expect("t3 conv");

        std::os::unix::fs::symlink(
            &real_outside_db,
            standalone_dir.join("conversation_summaries.db"),
        )
        .expect("symlink standalone sum");
        std::os::unix::fs::symlink(&real_outside_db, t3_acp.join("conversation_summaries.db"))
            .expect("symlink t3 sum");
        std::os::unix::fs::symlink(
            &real_outside_db,
            home.path().join(".t3/userdata/statev2.sqlite"),
        )
        .expect("symlink statev2");

        let roots = watch_roots(home.path(), false, &detected(&["antigravity-extension"]));
        assert!(
            roots.iter().all(|root| !root.metadata_only),
            "symlinked summary databases must never be watched"
        );
        assert!(t3_state_db_path(home.path(), false).is_none());
    }

    #[test]
    fn case_insensitive_root_matching_and_nonexistent_ignored_canonicalization_work() {
        let dir = tempfile::tempdir().expect("dir");
        let root_dir = dir.path().join("OpenClaw/Agents");
        std::fs::create_dir_all(&root_dir).expect("openclaw");
        let missing_ignored = dir.path().join("future/tokenlens/state");
        let map = RootMap::new(
            &[WatchRoot {
                client: "openclaw".into(),
                path: root_dir.clone(),
                metadata_only: false,
            }],
            std::slice::from_ref(&missing_ignored),
        );
        let mixed_case_file = dir.path().join("openclaw/agents/main/SESSIONS.JSON");
        assert_eq!(
            map.client_for_with_case(&mixed_case_file, true),
            Some(("openclaw", true))
        );
        if let Ok(canon_parent) = std::fs::canonicalize(dir.path()) {
            let canonical_ignored = canon_parent.join("future/tokenlens/state/collector.lock");
            assert_eq!(map.client_for(&canonical_ignored), None);
        }
    }
}
