//! Directories each client is read from at the locked upstream rev, and the
//! mapping from a changed path back to its client.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use tokscale_core::ClientId;

use crate::QODER_CLIENT;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WatchRoot {
    pub client: String,
    pub path: PathBuf,
}

fn is_forbidden(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::Normal(part) if part == ".qoder-cn"))
}

/// Every candidate directory, whether or not it exists.
pub fn candidate_roots(home: &Path, use_env_roots: bool) -> Vec<WatchRoot> {
    let home_text = home.to_string_lossy();
    let mut roots = Vec::new();
    let mut push = |client: &str, path: PathBuf| {
        roots.push(WatchRoot {
            client: client.to_owned(),
            path,
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
    push(QODER_CLIENT, crate::qoder::projects_root(home));
    roots.retain(|root| {
        !is_forbidden(&root.path)
            // A root equal to (or above) home would mean watching everything.
            && root.path != home
            && !home.starts_with(&root.path)
    });
    roots.sort();
    roots.dedup();
    roots
}

/// Existing directories of the detected clients; nested duplicates of the
/// same client are dropped because watches are recursive.
pub fn watch_roots(
    home: &Path,
    use_env_roots: bool,
    detected: &BTreeSet<String>,
) -> Vec<WatchRoot> {
    let existing = candidate_roots(home, use_env_roots)
        .into_iter()
        .filter(|root| detected.contains(&root.client) && root.path.is_dir())
        .collect::<Vec<_>>();
    existing
        .iter()
        .filter(|root| {
            !existing.iter().any(|other| {
                other.client == root.client
                    && other.path != root.path
                    && root.path.starts_with(&other.path)
            })
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct RootMap {
    roots: Vec<(PathBuf, String)>,
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
                    with_canonical(&root.path)
                        .into_iter()
                        .map(|path| (path, root.client.clone()))
                })
                .collect(),
            ignored: ignored
                .iter()
                .flat_map(|path| with_canonical(path))
                .collect(),
        }
    }

    /// The client whose root is the longest prefix of `path`.
    pub fn client_for(&self, path: &Path) -> Option<&str> {
        if is_forbidden(path) || self.ignored.iter().any(|own| path.starts_with(own)) {
            return None;
        }
        self.roots
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(_, client)| client.as_str())
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
    fn mirror_directory_is_never_selected_even_when_present() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".qoder-cn/projects")).expect("mirror");
        std::fs::create_dir_all(home.path().join(".qoder/projects")).expect("intl");
        let roots = watch_roots(home.path(), false, &detected(&[QODER_CLIENT, "qoder"]));
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, home.path().join(".qoder/projects"));
        assert!(
            candidate_roots(home.path(), false)
                .iter()
                .all(|root| { !is_forbidden(&root.path) && root.client != "qoder" })
        );
        let map = RootMap::new(&roots, &[]);
        assert_eq!(
            map.client_for(&home.path().join(".qoder-cn/projects/a.jsonl")),
            None
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
        for sidecar in ["db.sqlite", "db.sqlite-wal", "db.sqlite-shm"] {
            assert_eq!(
                map.client_for(&home.path().join(".zcode/cli/db").join(sidecar)),
                Some("zcode")
            );
        }
        assert_eq!(
            map.client_for(&home.path().join(".codex/sessions/2026/r.jsonl")),
            Some("codex")
        );
        assert_eq!(map.client_for(&own.join("outbox.sqlite-wal")), None);
        assert_eq!(map.client_for(&home.path().join(".bashrc")), None);
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
            },
            WatchRoot {
                client: "inner".into(),
                path: PathBuf::from("/synthetic/a/b"),
            },
        ];
        let map = RootMap::new(&roots, &[]);
        assert_eq!(map.client_for(Path::new("/synthetic/a/b/c")), Some("inner"));
        assert_eq!(map.client_for(Path::new("/synthetic/a/c")), Some("outer"));
    }
}
