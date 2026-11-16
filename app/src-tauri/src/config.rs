use std::path::{Path, PathBuf};

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

/// Whether the user explicitly asked for a number beside the Mac menu-bar
/// icon. Windows is deliberately left icon-only. Missing/corrupt settings
/// default to the requested display without blocking startup.
pub fn menu_bar_total_enabled(state_dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(state_dir.join("menu-bar-total.json")) else {
        return true;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("enabled")?.as_bool())
        .unwrap_or(true)
}

pub fn save_menu_bar_total_enabled(state_dir: &Path, enabled: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join("menu-bar-total.json");
    let temporary = state_dir.join("menu-bar-total.json.tmp");
    std::fs::write(&temporary, format!("{{\"enabled\":{enabled}}}\n"))?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn menu_bar_preference_defaults_persists_and_survives_corruption() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(super::menu_bar_total_enabled(dir.path()));
        super::save_menu_bar_total_enabled(dir.path(), false).expect("save off");
        assert!(!super::menu_bar_total_enabled(dir.path()));
        super::save_menu_bar_total_enabled(dir.path(), true).expect("save on");
        assert!(super::menu_bar_total_enabled(dir.path()));
        std::fs::write(dir.path().join("menu-bar-total.json"), b"not json").expect("fixture");
        assert!(super::menu_bar_total_enabled(dir.path()));
    }
}
