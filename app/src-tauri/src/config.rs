use std::path::PathBuf;

use collector_core::credentials::DEFAULT_KEYCHAIN_SERVICE;
use collector_core::sync;

/// Where this app keeps its pairing and upload queue. Shared with the
/// `collector` CLI so either can pair the computer.
pub struct Paths {
    pub keychain_service: String,
    pub state_dir: PathBuf,
    /// Debug builds only: read agent logs below this directory instead of
    /// the real home, ignoring agent root variables.
    pub home: Option<PathBuf>,
}

fn debug_override(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

impl Paths {
    pub fn resolve() -> Self {
        let keychain_service = debug_override("TOKENLENS_KEYCHAIN_SERVICE")
            .unwrap_or_else(|| DEFAULT_KEYCHAIN_SERVICE.to_owned());
        let state_dir = debug_override("TOKENLENS_STATE_DIR")
            .map(PathBuf::from)
            .or_else(sync::default_dir)
            .expect("this system has no per-user data directory");
        let home = debug_override("TOKENLENS_HOME").map(PathBuf::from);
        Self {
            keychain_service,
            state_dir,
            home,
        }
    }

    pub fn paused_marker(&self) -> PathBuf {
        self.state_dir.join("app-paused")
    }
}
