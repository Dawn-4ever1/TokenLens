//! Tauri commands. Replies carry numbers, labels and local state only; the
//! ingest and read tokens never leave this process's Rust side.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use collector_core::credentials::{
    self, CredentialStore, Credentials, KeychainStore, PairError, default_display_name,
    this_platform,
};
use collector_core::state::State;
use collector_core::sync;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use usage_event::envelope::{FormFactor, PairClaimResponse, PairCodeResponse};

use crate::collector::{self, Collector, Destination, Event, KEYCHAIN_ERROR, LOCAL_ERROR, Notify};
use crate::config::Paths;
use crate::local::{self, LocalServer};
use crate::remote::{self, CmdResult, CommandError, Remote};
use crate::tray::{self, Poke};

pub const STATUS_EVENT: &str = "status-changed";
const MAX_NAME_CHARS: usize = 64;

pub struct AppState {
    pub paths: Paths,
    pub collector: Collector,
    pub remote: Remote,
    /// Read from the keychain once at startup so that status polling never
    /// touches the keychain (and never triggers its prompts).
    credentials: Mutex<Option<Credentials>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppState {
    pub fn load(paths: Paths) -> Self {
        let collector = Collector::new();
        let remote = Remote::new();
        let credentials = match KeychainStore::new(&paths.keychain_service).load() {
            Ok(credentials) => credentials,
            Err(_) => {
                collector.set_error(Some(KEYCHAIN_ERROR));
                None
            }
        };
        // Local mode (D044) when no server pairing exists.
        let credentials = credentials.or_else(|| {
            let local = local::load_credentials(&paths.state_dir)?;
            match LocalServer::open(&paths.state_dir) {
                Ok(server) => remote.set_local(Some(server)),
                Err(_) => collector.set_error(Some(LOCAL_ERROR)),
            }
            Some(local)
        });
        Self {
            paths,
            collector,
            remote,
            credentials: Mutex::new(credentials),
        }
    }

    pub fn credentials(&self) -> Option<Credentials> {
        lock(&self.credentials).clone()
    }

    fn require_credentials(&self) -> CmdResult<Credentials> {
        self.credentials().ok_or(CommandError::new("not_paired"))
    }

    fn store(&self) -> KeychainStore {
        KeychainStore::new(&self.paths.keychain_service)
    }

    pub fn is_paused(&self) -> bool {
        self.paths.paused_marker().exists()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorStatus {
    pub running: bool,
    pub paused: bool,
    pub last_scan_at: Option<i64>,
    pub last_upload_at: Option<i64>,
    pub last_heartbeat_at: Option<i64>,
    pub backlog: Option<u64>,
    pub upload_state: Option<String>,
    pub backfill_complete: Option<bool>,
    pub last_error: Option<String>,
}

/// Mirrors `AppStatus` in `app/src/lib/types.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub paired: bool,
    /// `"local"` (D044) or `"server"`; `None` until set up.
    pub mode: Option<&'static str>,
    pub device_id: Option<String>,
    /// The server address; `None` in local mode.
    pub server: Option<String>,
    pub app_version: String,
    pub collector: CollectorStatus,
    pub autostart: Option<bool>,
    pub platform: &'static str,
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

pub fn status(app: &AppHandle) -> AppStatus {
    let state = app.state::<AppState>();
    let credentials = state.credentials();
    let live = state.collector.live();
    let local = credentials
        .as_ref()
        .and_then(|_| sync::local_status(&state.paths.state_dir).ok().flatten());
    AppStatus {
        paired: credentials.is_some(),
        mode: credentials.as_ref().map(|c| {
            if local::is_local(c) {
                "local"
            } else {
                "server"
            }
        }),
        device_id: credentials.as_ref().map(|c| c.device_id.clone()),
        server: credentials
            .as_ref()
            .filter(|c| !local::is_local(c))
            .map(|c| c.server_url.clone()),
        app_version: app.package_info().version.to_string(),
        collector: CollectorStatus {
            running: state.collector.is_running(),
            paused: state.is_paused(),
            last_scan_at: live.last_scan_at,
            last_upload_at: local.as_ref().and_then(|l| l.last_upload_at),
            last_heartbeat_at: local.as_ref().and_then(|l| l.last_heartbeat_at),
            backlog: local.as_ref().map(|l| l.outbox_backlog),
            // The persisted code can be stale until this run's first round.
            upload_state: live
                .upload_state
                .clone()
                .or_else(|| local.as_ref().and_then(|l| l.upload_state.clone())),
            backfill_complete: local.as_ref().map(|l| l.backfill_complete),
            last_error: live.last_error,
        },
        autostart: autostart_enabled(app),
        platform: platform(),
    }
}

/// Tells the window (if open) and the tray that local state moved.
pub fn changed(app: &AppHandle, poke: Poke) {
    let _ = app.emit(STATUS_EVENT, ());
    tray::poke(app, poke);
}

fn notifier(app: &AppHandle) -> Notify {
    let app = app.clone();
    Arc::new(move |event| {
        let poke = match event {
            Event::Uploaded => Poke::Uploaded,
            Event::Changed => Poke::Status,
        };
        changed(&app, poke);
    })
}

/// Starts collecting if this computer is paired and not paused.
pub fn start_collector(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.is_paused() {
        return;
    }
    let Some(credentials) = state.credentials() else {
        return;
    };
    let local = if local::is_local(&credentials) {
        let Some(server) = state.remote.local() else {
            state.collector.set_error(Some(LOCAL_ERROR));
            return;
        };
        Some(server)
    } else {
        None
    };
    match collector::watch_options(&state.paths) {
        Ok(options) => state.collector.start(
            options,
            state.paths.state_dir.clone(),
            Destination { credentials, local },
            notifier(app),
        ),
        Err(message) => state.collector.set_error(Some(message)),
    }
}

fn remove_local_queue(paths: &Paths) {
    // A `collector run` still using the queue keeps it; a later pairing resets it.
    let Ok(_lock) = collector_core::state::lock_queue(&paths.state_dir) else {
        return;
    };
    let file = State::path_in(&paths.state_dir);
    for suffix in ["", "-wal", "-shm"] {
        let mut name = file.clone().into_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(name);
    }
}

fn forget_pairing(app: &AppHandle) -> CmdResult<()> {
    let state = app.state::<AppState>();
    state.collector.stop();
    let previous = lock(&state.credentials).take();
    remove_local_queue(&state.paths);
    let _ = std::fs::remove_file(state.paths.paused_marker());
    state.collector.reset();
    let deleted = match previous {
        // Local mode (D044): retire this computer's local device so a later
        // return starts a fresh one; the local database itself stays.
        Some(previous) if local::is_local(&previous) => {
            if let Some(server) = state.remote.local() {
                server.revoke(&previous.device_id);
            }
            state.remote.set_local(None);
            local::delete_credentials(&state.paths.state_dir);
            Ok(true)
        }
        _ => state.store().delete(),
    };
    changed(app, Poke::Paired);
    deleted
        .map(|_| ())
        .map_err(|_| CommandError::new("keychain"))
}

fn adopt(app: &AppHandle, next: Credentials) -> CmdResult<()> {
    let state = app.state::<AppState>();
    state.collector.stop();
    state.collector.reset();
    let saved = if local::is_local(&next) {
        local::save_credentials(&state.paths.state_dir, &next)
            .map_err(|_| CommandError::new("server"))
    } else {
        state
            .store()
            .save(&next)
            .map_err(|_| CommandError::new("keychain"))
    };
    *lock(&state.credentials) = Some(next);
    if saved.is_err() {
        // Still collect with the new tokens until the app restarts.
        state.collector.set_error(Some(KEYCHAIN_ERROR));
    }
    start_collector(app);
    changed(app, Poke::Paired);
    saved
}

pub fn apply_paused(app: &AppHandle, paused: bool) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    let marker = state.paths.paused_marker();
    if paused {
        std::fs::create_dir_all(&state.paths.state_dir)
            .and_then(|()| std::fs::write(&marker, b""))
            .map_err(|_| CommandError::new("server"))?;
        state.collector.stop();
    } else {
        let _ = std::fs::remove_file(&marker);
        start_collector(app);
    }
    changed(app, Poke::Status);
    Ok(status(app))
}

/// Restarting runs a full scan, an upload round and a heartbeat right away.
pub fn restart_collector(app: &AppHandle) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    state.require_credentials()?;
    if !state.is_paused() {
        state.collector.stop();
        start_collector(app);
    }
    changed(app, Poke::Status);
    Ok(status(app))
}

/// Development builds never register a login item on this computer.
#[cfg(debug_assertions)]
pub fn autostart_enabled(_app: &AppHandle) -> Option<bool> {
    None
}

#[cfg(not(debug_assertions))]
pub fn autostart_enabled(app: &AppHandle) -> Option<bool> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().ok()
}

#[cfg(debug_assertions)]
fn write_autostart(_app: &AppHandle, _enabled: bool) -> CmdResult<()> {
    Ok(())
}

#[cfg(not(debug_assertions))]
fn write_autostart(app: &AppHandle, enabled: bool) -> CmdResult<()> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|_| CommandError::new("server"))
}

pub fn apply_autostart(app: &AppHandle, enabled: bool) -> CmdResult<AppStatus> {
    write_autostart(app, enabled)?;
    changed(app, Poke::Status);
    Ok(status(app))
}

fn pair_error(error: PairError) -> CommandError {
    match error {
        PairError::Url(_) => CommandError::new("bad_request"),
        PairError::Network => CommandError::new("network"),
        PairError::InvalidCode => CommandError::new("unauthorized"),
        PairError::TooManyAttempts => CommandError::new("rate_limited"),
        PairError::Refused { status, .. } => CommandError::from_status(status),
        PairError::BadResponse => CommandError::new("server"),
    }
}

/// `None` for an unusable name; empty means "use the generic default".
fn clean_name(name: &str) -> Option<String> {
    let name = name.trim();
    let ok = name.chars().count() <= MAX_NAME_CHARS && !name.chars().any(char::is_control);
    ok.then(|| name.to_owned())
}

fn token_ok(token: &str, prefix: &str) -> bool {
    token.starts_with(prefix) && token.len() <= 256 && token.bytes().all(|b| b.is_ascii_graphic())
}

async fn blocking<T, F>(app: AppHandle, work: F) -> CmdResult<T>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle) -> CmdResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || work(&app))
        .await
        .map_err(|_| CommandError::new("server"))?
}

#[tauri::command]
pub async fn app_status(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| Ok(status(app))).await
}

#[tauri::command]
pub async fn dashboard_get(
    app: AppHandle,
    path: String,
    query: Option<BTreeMap<String, String>>,
) -> CmdResult<Value> {
    let url = remote::dashboard_path(&path, &query.unwrap_or_default())?;
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let credentials = state.require_credentials()?;
        state.remote.get(&credentials, &url)
    })
    .await
}

#[tauri::command]
pub async fn pair_device(
    app: AppHandle,
    server: String,
    code: String,
    form_factor: FormFactor,
    name: String,
    autostart: bool,
) -> CmdResult<AppStatus> {
    let name = clean_name(&name).ok_or(CommandError::new("bad_request"))?;
    let name = if name.is_empty() {
        default_display_name(this_platform(), form_factor)
    } else {
        name
    };
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let next = credentials::claim(state.remote.transport(), &server, &code, form_factor, &name)
            .map_err(pair_error)?;
        let _ = std::fs::remove_file(state.paths.paused_marker());
        adopt(app, next)?;
        if autostart {
            let _ = write_autostart(app, true);
        }
        Ok(status(app))
    })
    .await
}

/// Sets this computer up in local mode (D044): no server, data stays here.
#[tauri::command]
pub async fn use_local_mode(
    app: AppHandle,
    form_factor: FormFactor,
    name: String,
    autostart: bool,
) -> CmdResult<AppStatus> {
    let name = clean_name(&name).ok_or(CommandError::new("bad_request"))?;
    let name = if name.is_empty() {
        default_display_name(this_platform(), form_factor)
    } else {
        name
    };
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        if state.credentials().is_some() {
            return Err(CommandError::new("bad_request"));
        }
        let server = match state.remote.local() {
            Some(server) => server,
            None => {
                let server = LocalServer::open(&state.paths.state_dir)
                    .map_err(|_| CommandError::new("server"))?;
                state.remote.set_local(Some(server.clone()));
                server
            }
        };
        let next = server
            .pair(form_factor, &name)
            .map_err(|_| CommandError::new("server"))?;
        let _ = std::fs::remove_file(state.paths.paused_marker());
        adopt(app, next)?;
        if autostart {
            let _ = write_autostart(app, true);
        }
        Ok(status(app))
    })
    .await
}

#[tauri::command]
pub async fn unpair_device(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        forget_pairing(app)?;
        Ok(status(app))
    })
    .await
}

#[tauri::command]
pub async fn set_paused(app: AppHandle, paused: bool) -> CmdResult<AppStatus> {
    blocking(app, move |app| apply_paused(app, paused)).await
}

#[tauri::command]
pub async fn sync_now(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, restart_collector).await
}

#[tauri::command]
pub async fn set_autostart(app: AppHandle, enabled: bool) -> CmdResult<AppStatus> {
    blocking(app, move |app| apply_autostart(app, enabled)).await
}

#[tauri::command]
pub async fn create_pair_code(app: AppHandle) -> CmdResult<Value> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        let credentials = state.require_credentials()?;
        // Other computers cannot reach a server that lives inside this app.
        if local::is_local(&credentials) {
            return Err(CommandError::new("bad_request"));
        }
        let reply = state.remote.post(&credentials, "pair/codes", &json!({}))?;
        pair_code_reply(reply).ok_or(CommandError::new("server"))
    })
    .await
}

/// The pairing API sends milliseconds; the UI gets the dashboard's ISO times.
fn pair_code_reply(reply: Value) -> Option<Value> {
    let reply: PairCodeResponse = serde_json::from_value(reply).ok()?;
    let expires = chrono::DateTime::from_timestamp_millis(reply.expires_at)?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Some(json!({ "code": reply.code, "expiresAt": expires }))
}

#[tauri::command]
pub async fn rename_device(
    app: AppHandle,
    device_id: String,
    display_name: String,
) -> CmdResult<Value> {
    if !remote::valid_device_id(&device_id) {
        return Err(CommandError::new("not_found"));
    }
    let name = clean_name(&display_name)
        .filter(|name| !name.is_empty())
        .ok_or(CommandError::new("bad_request"))?;
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let credentials = state.require_credentials()?;
        let path = format!("dashboard/devices/{device_id}/rename");
        let reply = state
            .remote
            .post(&credentials, &path, &json!({ "displayName": name }))?;
        changed(app, Poke::Status);
        Ok(reply)
    })
    .await
}

#[tauri::command]
pub async fn rotate_credentials(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        let current = state.require_credentials()?;
        let path = format!("dashboard/devices/{}/rotate", current.device_id);
        let reply = state.remote.post(&current, &path, &json!({}))?;
        let rotated: PairClaimResponse =
            serde_json::from_value(reply).map_err(|_| CommandError::new("server"))?;
        if rotated.device_id != current.device_id
            || !token_ok(&rotated.ingest_token, "tdi_")
            || !token_ok(&rotated.read_token, "tdr_")
        {
            return Err(CommandError::new("server"));
        }
        adopt(
            app,
            Credentials {
                server_url: current.server_url,
                device_id: rotated.device_id,
                ingest_token: rotated.ingest_token,
                read_token: rotated.read_token,
            },
        )?;
        Ok(status(app))
    })
    .await
}

#[tauri::command]
pub async fn revoke_device(app: AppHandle, device_id: String) -> CmdResult<AppStatus> {
    if !remote::valid_device_id(&device_id) {
        return Err(CommandError::new("not_found"));
    }
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let credentials = state.require_credentials()?;
        let path = format!("dashboard/devices/{device_id}/revoke");
        state.remote.post(&credentials, &path, &json!({}))?;
        if device_id == credentials.device_id {
            forget_pairing(app)?;
        } else {
            changed(app, Poke::Status);
        }
        Ok(status(app))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(clean_name("  Mac 笔记本 ").as_deref(), Some("Mac 笔记本"));
        assert_eq!(clean_name("   ").as_deref(), Some(""));
        assert_eq!(
            clean_name(&"名".repeat(64)).map(|n| n.chars().count()),
            Some(64)
        );
        assert_eq!(clean_name(&"名".repeat(65)), None);
        assert_eq!(clean_name("a\u{0007}b"), None);
        assert_eq!(clean_name("a\nb"), None);
    }

    #[test]
    fn pairing_errors_map_to_ui_kinds() {
        assert_eq!(pair_error(PairError::InvalidCode).kind, "unauthorized");
        assert_eq!(pair_error(PairError::Network).kind, "network");
        assert_eq!(pair_error(PairError::TooManyAttempts).kind, "rate_limited");
        assert_eq!(
            pair_error(PairError::Refused {
                status: 400,
                code: None
            })
            .kind,
            "bad_request"
        );
        assert_eq!(pair_error(PairError::BadResponse).kind, "server");
    }

    #[test]
    fn pair_codes_get_iso_expiry_times() {
        let reply = json!({ "code": "ABCD-EFGH-JKLM", "expiresAt": 1_762_819_200_000_i64 });
        assert_eq!(
            pair_code_reply(reply),
            Some(json!({ "code": "ABCD-EFGH-JKLM", "expiresAt": "2025-11-11T00:00:00.000Z" }))
        );
        assert_eq!(
            pair_code_reply(json!({ "code": "X", "expiresAt": "soon" })),
            None
        );
        assert_eq!(pair_code_reply(json!({ "expiresAt": 1 })), None);
    }

    #[test]
    fn rotated_tokens_are_checked() {
        assert!(token_ok("tdi_abc", "tdi_"));
        assert!(!token_ok("tdr_abc", "tdi_"));
        assert!(!token_ok("tdi_a b", "tdi_"));
        assert!(!token_ok(&format!("tdi_{}", "x".repeat(260)), "tdi_"));
    }
}
