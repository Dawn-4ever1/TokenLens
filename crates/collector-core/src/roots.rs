//! Directories each client is read from at the locked upstream rev, and the
//! mapping from a changed path back to its client.

use std::collections::BTreeSet;
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

fn is_sqlite_wal_for(path: &Path, root: &Path) -> bool {
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
    path.as_os_str() == wal.as_os_str()
}

fn is_forbidden(path: &Path, client: &str) -> bool {
    path.components()
        .any(|component| matches!(component, Component::Normal(part) if part == ".qoder-cn"))
        && client != QODER_CN_CLIENT
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
        .then(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
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
    push(QODER_CLIENT, crate::qoder::projects_root(home));
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
            detected.contains(&root.client) && (root.path.is_dir() || root.path.is_file())
        })
        .collect::<Vec<_>>();
    existing
        .iter()
        .filter(|root| {
            !existing.iter().any(|other| {
                other.client == root.client
                    && other.metadata_only == root.metadata_only
                    && other.path != root.path
                    && root.path.starts_with(&other.path)
            })
        })
        .cloned()
        .collect()
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
            if let Ok(canonical) = std::fs::canonicalize(path)
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
                    let is_dir = root.path.is_dir();
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
        if self.ignored.iter().any(|own| path.starts_with(own)) || is_sqlite_shm_sidecar(path) {
            return None;
        }
        self.roots
            .iter()
            .filter(|(root, _, metadata_only, is_dir)| {
                if *metadata_only {
                    path == root
                        || is_sqlite_wal_for(path, root)
                        || (*is_dir && path.starts_with(root))
                } else {
                    path.starts_with(root) || is_sqlite_wal_for(path, root)
                }
            })
            .max_by_key(|(root, _, _, _)| root.components().count())
            .map(|(_, client, metadata_only, _)| {
                let is_metadata = *metadata_only
                    || (client == "openclaw"
                        && path.file_name() == Some(std::ffi::OsStr::new("sessions.json")));
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
}
