//! Tauri commands. Replies carry numbers, labels and local state only; the
//! ingest and read tokens never leave this process's Rust side.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::config::{self, Paths};
use crate::local::{self, LocalServer};
use crate::remote::{self, CmdResult, CommandError, Remote};
use crate::tray::{self, Poke};

pub const STATUS_EVENT: &str = "status-changed";
const MAX_NAME_CHARS: usize = 64;

pub struct AppState {
    pub paths: Paths,
    pub collector: Collector,
    pub remote: Remote,
    new_install: bool,
    legacy_local_queue: bool,
    /// Read from the keychain once at startup so that status polling never
    /// touches the keychain (and never triggers its prompts).
    local_credentials: Mutex<Option<Credentials>>,
    server_credentials: Mutex<Option<Credentials>>,
    #[cfg(test)]
    credential_override: Option<std::sync::Arc<collector_core::credentials::MemoryStore>>,
    /// Account session and its email (D049); keychain-only, never reaches the
    /// WebView. Status polling reads the cached email without a request.
    account_session: Mutex<Option<String>>,
    account_email: Mutex<Option<String>>,
    account_provider: Mutex<Option<String>>,
    account_display_name: Mutex<Option<String>>,
    account_avatar_url: Mutex<Option<String>>,
    account_space: Mutex<Option<String>>,
    /// A started GitHub device flow; the code stays in Rust (D049).
    github_device: Mutex<Option<String>>,
    /// A started Google authorization-code flow; the id stays in Rust (D051).
    google_flow: Mutex<Option<String>>,
    /// Cached macOS menu-bar preference so status polling avoids disk reads.
    menu_bar_total: AtomicBool,
    /// Serializes pairing, disconnects, credential rotation and account changes.
    connection: Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(not(test))]
fn load_saved_server(paths: &Paths) -> Result<Option<Credentials>, credentials::CredentialError> {
    KeychainStore::new(&paths.keychain_service).load()
}

#[cfg(test)]
fn load_saved_server(_paths: &Paths) -> Result<Option<Credentials>, credentials::CredentialError> {
    Ok(None)
}

#[cfg(not(test))]
fn load_saved_session(paths: &Paths) -> Option<String> {
    crate::account::load_session(&paths.keychain_service)
}

#[cfg(test)]
fn load_saved_session(_paths: &Paths) -> Option<String> {
    None
}

impl AppState {
    pub fn load(paths: Paths) -> Self {
        let collector = Collector::new();
        let remote = Remote::new();
        #[cfg(test)]
        let credential_override = Some(std::sync::Arc::new(
            collector_core::credentials::MemoryStore::default(),
        ));
        let (server_credentials, keychain_available) = match load_saved_server(&paths) {
            Ok(credentials) => (credentials, true),
            Err(_) => {
                collector.set_error(Some(KEYCHAIN_ERROR));
                (None, false)
            }
        };
        let legacy_local_queue = legacy_queue_required(&paths.state_dir);
        let new_install = keychain_available
            && server_credentials.is_none()
            && !paths.state_dir.join("local-device.json").exists()
            && !paths.state_dir.join("app-initialized").exists();
        let local_credentials =
            match initialize_local(&paths, &remote, crate::device::detected_form_factor(), "") {
                Ok(credentials) => Some(credentials),
                Err(_error) => {
                    #[cfg(test)]
                    eprintln!("test local initialization failed: {_error:?}");
                    collector.set_error(Some(LOCAL_ERROR));
                    None
                }
            };
        let menu_bar_total = AtomicBool::new(config::menu_bar_total_enabled(&paths.state_dir));
        let account_session = load_saved_session(&paths);
        Self {
            paths,
            collector,
            remote,
            new_install,
            legacy_local_queue,
            local_credentials: Mutex::new(local_credentials),
            server_credentials: Mutex::new(server_credentials),
            #[cfg(test)]
            credential_override,
            account_session: Mutex::new(account_session),
            account_email: Mutex::new(None),
            account_provider: Mutex::new(None),
            account_display_name: Mutex::new(None),
            account_avatar_url: Mutex::new(None),
            account_space: Mutex::new(None),
            github_device: Mutex::new(None),
            google_flow: Mutex::new(None),
            menu_bar_total,
            connection: Mutex::new(()),
        }
    }

    pub fn menu_bar_total(&self) -> bool {
        self.menu_bar_total.load(Ordering::Relaxed)
    }

    fn save_account_session(&self, session: &str) -> CmdResult<()> {
        #[cfg(test)]
        if self.credential_override.is_some() {
            return Ok(());
        }
        crate::account::save_session(&self.paths.keychain_service, session)
    }

    fn delete_account_session(&self) -> CmdResult<()> {
        #[cfg(test)]
        if self.credential_override.is_some() {
            return Ok(());
        }
        crate::account::delete_session(&self.paths.keychain_service)
    }

    pub fn credentials(&self) -> Option<Credentials> {
        // The server is the complete view whenever connected; the local server
        // is the complete view otherwise. Never fall back from a failing server
        // to local credentials, which could silently masquerade as the same data.
        self.server_credentials()
            .or_else(|| lock(&self.local_credentials).clone())
    }

    fn server_credentials(&self) -> Option<Credentials> {
        lock(&self.server_credentials).clone()
    }

    /// The signed-in account's session, if this computer has one. Used only
    /// by Rust-side account calls (D049).
    fn account_session(&self) -> Option<String> {
        lock(&self.account_session).clone()
    }

    fn account_email(&self) -> Option<String> {
        lock(&self.account_email).clone()
    }

    fn require_credentials(&self) -> CmdResult<Credentials> {
        self.credentials().ok_or(CommandError::new("not_paired"))
    }

    fn require_server(&self) -> CmdResult<Credentials> {
        self.server_credentials()
            .ok_or(CommandError::new("not_paired"))
    }

    fn save_server_credentials(
        &self,
        next: &Credentials,
    ) -> Result<(), credentials::CredentialError> {
        #[cfg(test)]
        if let Some(store) = &self.credential_override {
            return store.save(next);
        }
        KeychainStore::new(&self.paths.keychain_service).save(next)
    }

    fn delete_server_credentials(&self) -> Result<bool, credentials::CredentialError> {
        #[cfg(test)]
        if let Some(store) = &self.credential_override {
            return store.delete();
        }
        KeychainStore::new(&self.paths.keychain_service).delete()
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
    /// Active data source: the connected server includes all its devices,
    /// while an unconnected app reads the local device (D053).
    pub mode: Option<&'static str>,
    pub device_id: Option<String>,
    /// Connected server address, independent of dashboard read failures.
    pub server: Option<String>,
    pub app_version: String,
    pub collector: CollectorStatus,
    pub autostart: Option<bool>,
    /// Explicit user preference, macOS only; the tray remains icon-only on Windows.
    pub menu_bar_total: Option<bool>,
    pub platform: &'static str,
    /// Best-effort product-family detection; users can always correct it.
    pub form_factor: FormFactor,
    /// Default hosted service, so the app can pre-select it (D049).
    pub cloud_server: &'static str,
    /// Signed-in account, when this computer has one.
    pub account_email: Option<String>,
    /// Sign-in method ("github"/"google") when the server reports one (D051).
    pub account_provider: Option<String>,
    /// Public provider profile used for the sidebar, not a login credential.
    pub account_display_name: Option<String>,
    pub account_avatar_url: Option<String>,
    /// Name of the signed-in space, for the account row in the app.
    pub account_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_app_version: Option<String>,
    pub app_outdated: bool,
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

pub fn status<R: tauri::Runtime>(app: &AppHandle<R>) -> AppStatus {
    let state = app.state::<AppState>();
    let credentials = state.credentials();
    let live = state.collector.live();
    let viewing_local = credentials.as_ref().is_some_and(local::is_local);
    let queue = if viewing_local {
        state.paths.state_dir.join("local-queue")
    } else {
        state.paths.state_dir.clone()
    };
    let local = credentials
        .as_ref()
        .and_then(|_| sync::local_summary(&queue).ok().flatten());
    let persisted_upload_state = (!viewing_local && !live.server_queue_error)
        .then(|| local.as_ref().and_then(|l| l.upload_state.clone()))
        .flatten();
    let app_version = app.package_info().version.to_string();
    let min_app_version = (!viewing_local)
        .then_some(live.server_min_app_version)
        .flatten();
    let app_outdated = min_app_version
        .as_deref()
        .is_some_and(|min| collector_core::transport::version_below(&app_version, min));
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
        server: state.server_credentials().map(|c| c.server_url),
        app_version,
        collector: CollectorStatus {
            running: state.collector.is_running(),
            paused: state.is_paused(),
            last_scan_at: live.last_scan_at,
            last_upload_at: local.as_ref().and_then(|l| l.last_upload_at),
            last_heartbeat_at: local.as_ref().and_then(|l| l.last_heartbeat_at),
            backlog: local.as_ref().map(|l| l.outbox_backlog),
            // The persisted code can be stale until this run's first round.
            upload_state: (if viewing_local {
                live.upload_state
            } else if live.server_queue_error {
                Some("queue_error".to_owned())
            } else {
                live.server_upload_state
            })
            .or(persisted_upload_state),
            backfill_complete: local.as_ref().map(|l| l.backfill_complete),
            last_error: live.last_error,
        },
        autostart: autostart_enabled(app),
        menu_bar_total: cfg!(target_os = "macos").then(|| state.menu_bar_total()),
        platform: platform(),
        form_factor: crate::device::detected_form_factor(),
        cloud_server: crate::account::CLOUD_URL,
        account_email: state.account_email(),
        account_provider: lock(&state.account_provider).clone(),
        account_display_name: lock(&state.account_display_name).clone(),
        account_avatar_url: lock(&state.account_avatar_url).clone(),
        account_space: lock(&state.account_space).clone(),
        min_app_version,
        app_outdated,
    }
}

/// Tells the window (if open) and the tray that local state moved.
pub fn changed<R: tauri::Runtime>(app: &AppHandle<R>, poke: Poke) {
    let _ = app.emit(STATUS_EVENT, ());
    tray::poke(app, poke);
}

fn notifier<R: tauri::Runtime>(app: &AppHandle<R>) -> Notify {
    let app = app.clone();
    Arc::new(move |event| {
        let poke = match event {
            Event::Uploaded => Poke::Uploaded,
            Event::Changed => Poke::Status,
        };
        changed(&app, poke);
    })
}

fn expire_connection_notifier<R: tauri::Runtime>(app: &AppHandle<R>) -> collector::Expired {
    let app = app.clone();
    Arc::new(move |credentials| expire_server_connection(&app, credentials))
}

fn ensure_local_initialized(state: &AppState) -> Option<Credentials> {
    if let Some(credentials) = lock(&state.local_credentials).clone() {
        return Some(credentials);
    }
    match initialize_local(
        &state.paths,
        &state.remote,
        crate::device::detected_form_factor(),
        "",
    ) {
        Ok(credentials) => {
            *lock(&state.local_credentials) = Some(credentials.clone());
            if state.collector.live().last_error.as_deref() == Some(LOCAL_ERROR) {
                state.collector.set_error(None);
            }
            Some(credentials)
        }
        Err(_) => {
            state.collector.set_error(Some(LOCAL_ERROR));
            None
        }
    }
}

/// Starts local collection and optional server sync unless the user paused.
pub fn start_collector<R: tauri::Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    if state.is_paused() {
        return;
    }
    let Some(credentials) = ensure_local_initialized(&state) else {
        return;
    };
    let Some(server) = state.remote.local() else {
        state.collector.set_error(Some(LOCAL_ERROR));
        return;
    };
    let destination = Destination {
        credentials,
        local: Some(server),
    };
    let network = state.server_credentials().map(|credentials| Destination {
        credentials,
        local: None,
    });
    match collector::watch_options(&state.paths) {
        Ok(options) => state.collector.start_with_server(
            options,
            state.paths.state_dir.clone(),
            destination,
            network,
            state.legacy_local_queue,
            notifier(app),
            expire_connection_notifier(app),
        ),
        Err(message) => state.collector.set_error(Some(message)),
    }
}

fn remove_server_queue(paths: &Paths, device_id: &str) {
    // A `collector run` still using the queue keeps it; a later pairing resets it.
    let Ok(_lock) = collector_core::state::lock_queue(&paths.state_dir) else {
        return;
    };
    if !State::exists_in(&paths.state_dir) {
        return;
    }
    let Ok(queue) = State::open(&paths.state_dir) else {
        return;
    };
    if queue
        .meta(collector_core::state::meta::DEVICE_ID)
        .ok()
        .flatten()
        .as_deref()
        != Some(device_id)
    {
        return;
    }
    drop(queue);
    let file = State::path_in(&paths.state_dir);
    for suffix in ["", "-wal", "-shm"] {
        let mut name = file.clone().into_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(name);
    }
}

/// Disconnects only the network destination, preserving the local database,
/// credentials, queue and pause preference. The caller holds `connection`.
fn forget_pairing_locked<R: tauri::Runtime>(app: &AppHandle<R>) -> CmdResult<()> {
    let state = app.state::<AppState>();
    // A keychain failure leaves the current connection usable; local collection
    // is never retired or deleted when disconnecting a network server.
    state
        .delete_server_credentials()
        .map_err(|_| CommandError::new("keychain"))?;
    state.collector.stop();
    if let Some(previous) = lock(&state.server_credentials).take() {
        remove_server_queue(&state.paths, &previous.device_id);
    }
    state.collector.reset();
    // Starting is still gated by pause and the updater's own lifecycle gate.
    start_collector(app);
    Ok(())
}

#[cfg(test)]
fn forget_pairing<R: tauri::Runtime>(app: &AppHandle<R>) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
    forget_pairing_locked(app)?;
    changed(app, Poke::Paired);
    Ok(())
}

/// Only an actual server 401 reaches this routine. Match the whole credential,
/// not just the device id, so a delayed old request cannot delete a new binding.
pub(crate) fn handle_server_unauthorized<R: tauri::Runtime>(
    app: &AppHandle<R>,
    observed: Credentials,
) -> CmdResult<()> {
    let state = app.state::<AppState>();
    debug_assert!(!local::is_local(&observed));
    {
        let _connection = lock(&state.connection);
        if state.server_credentials().as_ref() != Some(&observed) {
            return Err(CommandError::new("unauthorized"));
        }
        forget_pairing_locked(app)?;
        let _ = state.delete_account_session();
        clear_account_memory(&state);
    }
    changed(app, Poke::Paired);
    // The rejected read keeps its error; this response already contains only
    // local state and cannot masquerade as the former cloud dashboard.
    Err(CommandError::new("unauthorized"))
}

/// This runs off the collector/uploader threads. `stop()` joins those threads,
/// so it must never be called synchronously from their status sinks.
fn expire_server_connection<R: tauri::Runtime>(app: &AppHandle<R>, observed: Credentials) {
    let app = app.clone();
    drop(tauri::async_runtime::spawn_blocking(move || {
        let _ = handle_server_unauthorized(&app, observed);
    }));
}

fn adopt_locked<R: tauri::Runtime>(
    app: &AppHandle<R>,
    next: Credentials,
    rotating: bool,
) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let saved = state
        .save_server_credentials(&next)
        .map_err(|_| CommandError::new("keychain"));
    // Rotation already invalidated the old credentials on the server: keep the
    // replacement in memory even when the keychain cannot persist it.
    if saved.is_err() && !rotating {
        return saved;
    }
    state.collector.stop();
    state.collector.reset();
    *lock(&state.server_credentials) = Some(next);
    if saved.is_err() {
        state.collector.set_error(Some(KEYCHAIN_ERROR));
    }
    start_collector(app);
    saved
}

fn adopt<R: tauri::Runtime>(
    app: &AppHandle<R>,
    next: Credentials,
    rotating: bool,
) -> CmdResult<()> {
    let saved = {
        let state = app.state::<AppState>();
        let _connection = lock(&state.connection);
        adopt_locked(app, next, rotating)
    };
    if saved.is_ok() || rotating {
        changed(app, Poke::Paired);
    }
    saved
}

fn legacy_queue_required(state_dir: &std::path::Path) -> bool {
    local::load_credentials(state_dir).is_some()
        && State::exists_in(state_dir)
        && !State::exists_in(&state_dir.join("local-queue"))
}

fn initialize_local(
    paths: &Paths,
    remote: &Remote,
    form_factor: FormFactor,
    name: &str,
) -> CmdResult<Credentials> {
    let server = match remote.local() {
        Some(server) => server,
        None => {
            let server =
                LocalServer::open(&paths.state_dir).map_err(|_| CommandError::new("server"))?;
            remote.set_local(Some(server.clone()));
            server
        }
    };
    let previous = local::load_credentials(&paths.state_dir);
    if let Some(credentials) = &previous {
        match remote.get(credentials, "dashboard/health") {
            Ok(_) => {
                // The on-disk pairing is authoritative. Returning a different
                // in-memory device would leave its queue and dashboard split.
                local::save_credentials(&paths.state_dir, credentials)
                    .map_err(|_| CommandError::new("server"))?;
                return Ok(credentials.clone());
            }
            // A restored/recreated database may no longer contain this local
            // device. Re-pair locally without ever replacing network tokens.
            Err(error) if error.kind == "unauthorized" => {
                // A revoked device must not keep a stale local pairing.
                let _ = std::fs::remove_file(paths.state_dir.join("local-device.json"));
            }
            Err(error) => return Err(error),
        }
    }
    if previous.is_none() {
        // Remember the layout before writing the new local credentials. A
        // restart while the root queue is busy must not mistake a server's
        // old queue for a legacy local outbox.
        std::fs::create_dir_all(&paths.state_dir)
            .and_then(|()| std::fs::write(paths.state_dir.join("local-queue-v2"), b""))
            .map_err(|_| CommandError::new("server"))?;
    }
    let name = if name.is_empty() {
        default_display_name(this_platform(), form_factor)
    } else {
        name.to_owned()
    };
    let next = server
        .pair(form_factor, &name)
        .map_err(|_| CommandError::new("server"))?;
    if local::save_credentials(&paths.state_dir, &next).is_err() {
        server.revoke(&next.device_id);
        return Err(CommandError::new("server"));
    }
    Ok(next)
}

pub fn apply_paused(app: &AppHandle, paused: bool) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
    let marker = state.paths.paused_marker();
    if paused {
        std::fs::create_dir_all(&state.paths.state_dir)
            .and_then(|()| std::fs::write(&marker, b""))
            .map_err(|_| CommandError::new("server"))?;
        state.collector.stop();
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CommandError::new("server")),
        }
        start_collector(app);
    }
    changed(app, Poke::Status);
    Ok(status(app))
}

/// Releases the updater's stop/start gate under the connection lock and
/// resumes collection when either it was running before or the user unpaused.
pub fn finish_update_and_resume<R: tauri::Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
    let resume = state.collector.finish_update();
    if (resume || !state.is_paused()) && state.credentials().is_some() {
        start_collector(app);
    }
}

/// Restarting runs a full scan, an upload round and a heartbeat right away.
pub fn restart_collector(app: &AppHandle) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
    if !state.is_paused() {
        let _ = ensure_local_initialized(&state);
    }
    if state.collector.live().last_error.as_deref() == Some(KEYCHAIN_ERROR) {
        match load_saved_server(&state.paths) {
            Ok(None) => state.collector.set_error(None),
            Ok(Some(credentials)) => {
                if state.server_credentials().is_none() {
                    *lock(&state.server_credentials) = Some(credentials);
                }
                state.collector.set_error(None);
            }
            Err(_) => {}
        }
    }
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
pub fn autostart_enabled<R: tauri::Runtime>(_app: &AppHandle<R>) -> Option<bool> {
    None
}

#[cfg(not(debug_assertions))]
pub fn autostart_enabled<R: tauri::Runtime>(app: &AppHandle<R>) -> Option<bool> {
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

/// Preserve the existing default-on behavior only for a new installation;
/// later launches never overwrite a user's explicit autostart choice.
pub fn initialize_autostart(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.new_install
        && lock(&state.local_credentials).is_some()
        && write_autostart(app, true).is_ok()
    {
        let _ = std::fs::write(state.paths.state_dir.join("app-initialized"), b"");
    }
}

pub fn apply_autostart(app: &AppHandle, enabled: bool) -> CmdResult<AppStatus> {
    write_autostart(app, enabled)?;
    changed(app, Poke::Status);
    Ok(status(app))
}

#[cfg(target_os = "macos")]
pub fn apply_menu_bar_total(app: &AppHandle, enabled: bool) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    config::save_menu_bar_total_enabled(&state.paths.state_dir, enabled)
        .map_err(|_| CommandError::new("server"))?;
    state.menu_bar_total.store(enabled, Ordering::Relaxed);
    changed(app, Poke::Status);
    Ok(status(app))
}

#[cfg(not(target_os = "macos"))]
pub fn apply_menu_bar_total(_app: &AppHandle, _enabled: bool) -> CmdResult<AppStatus> {
    Err(CommandError::new("unavailable"))
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

async fn blocking<R: tauri::Runtime, T, F>(app: AppHandle<R>, work: F) -> CmdResult<T>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle<R>) -> CmdResult<T> + Send + 'static,
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
    let health_check = path == "health";
    let url = remote::dashboard_path(&path, &query.unwrap_or_default())?;
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let credentials = state.require_credentials()?;
        match state.remote.get(&credentials, &url) {
            Err(error) if !local::is_local(&credentials) && error.kind == "unauthorized" => {
                handle_server_unauthorized(app, credentials)?;
                unreachable!("a rejected connection always returns its original error")
            }
            Ok(value) => {
                if health_check
                    && !local::is_local(&credentials)
                    && let Some(min_app) = value
                        .get("minAppVersion")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                {
                    state.collector.set_server_min_app_version(Some(min_app));
                }
                Ok(value)
            }
            Err(error) => Err(error),
        }
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
        adopt(app, next, false)?;
        if autostart {
            let _ = write_autostart(app, true);
        }
        Ok(status(app))
    })
    .await
}

/// Sends a short-lived sign-in code to the account's email (D049).
#[tauri::command]
pub async fn request_login_code(app: AppHandle, server: String, email: String) -> CmdResult<()> {
    blocking(app, move |_app| {
        crate::account::request_code(&server, &email)
    })
    .await
}

/// Existing cloud credentials already identify this computer. Account login
/// and device pairing are deliberately separate: refreshing a session must not
/// create another device, change its tenant or replace its queue.
fn reusable_server_credentials(state: &AppState, server: &str) -> CmdResult<Option<Credentials>> {
    let Some(existing) = state.server_credentials() else {
        return Ok(None);
    };
    if existing.server_url.trim_end_matches('/') != server.trim_end_matches('/') {
        return Ok(None);
    }
    // A normal re-login keeps the connection without any network writes. Only
    // an actually revoked server device is explicitly rebound, so both sides
    // cannot keep different ideas of the current device after an outage.
    match state.remote.get(&existing, "dashboard/health") {
        Ok(_) => Ok(Some(existing)),
        Err(error) if error.kind == "unauthorized" => {
            // The health check does not mutate anything. Complete sign-in will
            // clear the old session below and replace the revoked connection.
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Clears any signed-in profile after switching or revoking an account,
/// while preserving `server_credentials` updates for the caller.
fn set_signed_in_account(state: &AppState, signed: crate::account::SignedIn) {
    *lock(&state.account_session) = Some(signed.session);
    *lock(&state.account_email) = Some(signed.email);
    *lock(&state.account_provider) = signed.provider;
    *lock(&state.account_display_name) = signed.display_name;
    *lock(&state.account_avatar_url) = signed.avatar_url;
    *lock(&state.account_space) = Some(signed.space_name);
}

fn spawn_logout(server: String, session: String) {
    drop(tauri::async_runtime::spawn_blocking(move || {
        crate::account::logout(&server, &session);
    }));
}

/// Finishes any sign-in: reuses this computer's existing cloud connection, or
/// pairs exactly once for a new server. The account session never selects a
/// tenant, and only Rust sees either credential.
fn complete_account_sign_in(
    app: &AppHandle,
    state: &AppState,
    server: &str,
    signed: crate::account::SignedIn,
    form_factor: FormFactor,
    name: &str,
    autostart: bool,
) -> CmdResult<AppStatus> {
    let server = collector_core::transport::validate_server_url(server)
        .map_err(|_| CommandError::new("bad_request"))?;
    let previous_session = state.account_session().map(|session| {
        let prev_server = state
            .server_credentials()
            .map_or_else(|| server.clone(), |c| c.server_url);
        (prev_server, session)
    });
    let new_session = signed.session.clone();
    let mut replaced_device = None;
    let next = if let Some(existing) = reusable_server_credentials(state, &server)? {
        // A previous session belongs to this exact device. If the account is
        // different or unverified, do not silently retain another space.
        let bound_email = match state.account_email() {
            Some(email) => Some(email),
            None => previous_session
                .as_ref()
                .filter(|(prev_server, _)| {
                    prev_server.trim_end_matches('/') == server.trim_end_matches('/')
                })
                .and_then(|(_, old_session)| {
                    crate::account::account_info(&server, old_session)
                        .ok()
                        .map(|profile| profile.email)
                }),
        };
        if bound_email.as_deref() != Some(signed.email.as_str()) {
            // Do not silently connect a different or unverified account to this
            // space. Keep the working device untouched until explicitly reset.
            return Err(CommandError::new("account_mismatch"));
        }
        let _connection = lock(&state.connection);
        if state.server_credentials().as_ref() != Some(&existing) {
            return Err(CommandError::new("server"));
        }
        state.save_account_session(&signed.session)?;
        set_signed_in_account(state, signed);
        existing
    } else {
        // Persist the random id before asking for a device. A crash or a lost
        // response can then safely retry without creating a second identity.
        let binding = crate::device::client_binding_id(&state.paths.state_dir)
            .map_err(|_| CommandError::new("server"))?;
        let next =
            crate::account::create_device(&server, &signed.session, form_factor, name, &binding)?;
        {
            let _connection = lock(&state.connection);
            let replaced = state.server_credentials();
            state.save_account_session(&signed.session)?;
            if let Err(error) = adopt_locked(app, next.clone(), false) {
                if let Some((_, ref prev)) = previous_session {
                    let _ = state.save_account_session(prev);
                } else {
                    let _ = state.delete_account_session();
                }
                return Err(error);
            }
            set_signed_in_account(state, signed);
            replaced_device = replaced;
        }
        next
    };
    if let Some(previous) = replaced_device.filter(|old| old.device_id != next.device_id) {
        let path = format!("dashboard/devices/{}/revoke", previous.device_id);
        let _ = state.remote.post(&previous, &path, &json!({}));
    }
    debug_assert_eq!(
        next.server_url.trim_end_matches('/'),
        server.trim_end_matches('/')
    );
    // Only after the replacement session is safely persisted do we retire the
    // captured old session on its original server. Never revoke the new one,
    // and do not block sign-in if the old server is unreachable.
    if let Some((prev_server, old_session)) = previous_session
        && old_session != new_session
    {
        spawn_logout(prev_server, old_session);
    }
    if autostart {
        let _ = write_autostart(app, true);
    }
    changed(app, Poke::Paired);
    Ok(status(app))
}

/// Signs in, creates this computer's device in the account's space and starts
/// syncing. A first sign-in creates the space on the server.
#[tauri::command]
pub async fn login_account(
    app: AppHandle,
    server: String,
    email: String,
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
        let signed = crate::account::login(&server, &email, &code)?;
        complete_account_sign_in(app, &state, &server, signed, form_factor, &name, autostart)
    })
    .await
}

fn clear_account_memory(state: &AppState) {
    lock(&state.account_session).take();
    lock(&state.account_email).take();
    lock(&state.account_provider).take();
    lock(&state.account_display_name).take();
    lock(&state.account_avatar_url).take();
    lock(&state.account_space).take();
}

/// Handles a revoked account session and its matching old cloud connection
/// together, while preserving a newly created connection or account session.
fn handle_account_refresh<R: tauri::Runtime>(
    app: &AppHandle<R>,
    session: &str,
    server: Option<Credentials>,
    reply: CmdResult<crate::account::AccountProfile>,
) -> CmdResult<Option<String>> {
    let state = app.state::<AppState>();
    if reply
        .as_ref()
        .is_err_and(|error| error.kind == "unauthorized")
        && server.as_ref().is_some_and(|old| {
            !local::is_local(old) && state.server_credentials().as_ref() == Some(old)
        })
    {
        let _connection = lock(&state.connection);
        if state.account_session().as_deref() == Some(session)
            && state.server_credentials() == server
        {
            let result = forget_pairing_locked(app);
            if result.is_ok() {
                let _ = state.delete_account_session();
                clear_account_memory(&state);
            }
            drop(_connection);
            result?;
            changed(app, Poke::Paired);
            return Ok(None);
        }
    }
    Ok(apply_account_refresh(&state, session, reply))
}

/// Applies a response only while its session is still current. Account writes
/// share the connection lock, but the network request never holds it.
fn apply_account_refresh(
    state: &AppState,
    session: &str,
    reply: CmdResult<crate::account::AccountProfile>,
) -> Option<String> {
    let _connection = lock(&state.connection);
    if state.account_session().as_deref() != Some(session) {
        return state.account_email();
    }
    match reply {
        Ok(profile) => {
            *lock(&state.account_email) = Some(profile.email.clone());
            *lock(&state.account_provider) = profile.provider;
            *lock(&state.account_display_name) = profile.display_name;
            *lock(&state.account_avatar_url) = profile.avatar_url;
            if let Some(space) = profile.space_name {
                *lock(&state.account_space) = Some(space);
            }
            Some(profile.email)
        }
        // A revoked or expired session must not linger as "signed in".
        Err(error) if error.kind == "unauthorized" => {
            let _ = state.delete_account_session();
            clear_account_memory(state);
            None
        }
        Err(_) => state.account_email(),
    }
}

/// Confirms the saved account session still works and caches its public profile.
#[tauri::command]
pub async fn refresh_account(app: AppHandle) -> CmdResult<Option<String>> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        let Some(session) = state.account_session() else {
            return Ok(None);
        };
        let connection = state.server_credentials();
        let server = connection.as_ref().map_or_else(
            || crate::account::CLOUD_URL.to_owned(),
            |c| c.server_url.clone(),
        );
        let reply = crate::account::account_info(&server, &session);
        handle_account_refresh(app, &session, connection, reply)
    })
    .await
}

/// Starts GitHub sign-in and returns only the short human code.
#[tauri::command]
pub async fn start_github_login(app: AppHandle, server: String) -> CmdResult<Value> {
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let start = crate::account::github_start(&server)?;
        *lock(&state.github_device) = Some(start.device_code.clone());
        Ok(json!({
            "userCode": start.user_code,
            "verificationUri": start.verification_uri,
            "intervalSeconds": start.interval_seconds,
            "expiresInSeconds": start.expires_in_seconds,
        }))
    })
    .await
}

fn clear_if_matches(slot: &Mutex<Option<String>>, expected: &str) {
    let mut guard = lock(slot);
    if guard.as_deref() == Some(expected) {
        guard.take();
    }
}

/// Polls the started flow. On approval this creates the space's device and
/// switches the dashboard to the server, exactly like an email sign-in.
#[tauri::command]
pub async fn poll_github_login(
    app: AppHandle,
    server: String,
    form_factor: FormFactor,
    name: String,
    autostart: bool,
) -> CmdResult<Value> {
    let name = clean_name(&name).ok_or(CommandError::new("bad_request"))?;
    let name = if name.is_empty() {
        default_display_name(this_platform(), form_factor)
    } else {
        name
    };
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let device = lock(&state.github_device)
            .clone()
            .ok_or(CommandError::new("bad_request"))?;
        match crate::account::github_poll(&server, &device)? {
            crate::account::GitHubPoll::Pending => Ok(json!({ "status": "pending" })),
            crate::account::GitHubPoll::SlowDown(seconds) => {
                Ok(json!({ "status": "slow_down", "retryAfterSeconds": seconds }))
            }
            crate::account::GitHubPoll::Expired => {
                clear_if_matches(&state.github_device, &device);
                Ok(json!({ "status": "expired" }))
            }
            crate::account::GitHubPoll::Denied => {
                clear_if_matches(&state.github_device, &device);
                Ok(json!({ "status": "denied" }))
            }
            crate::account::GitHubPoll::Failed(code) => {
                clear_if_matches(&state.github_device, &device);
                Ok(json!({ "status": "failed", "failureCode": code }))
            }
            crate::account::GitHubPoll::SignedIn(signed) => {
                let next = complete_account_sign_in(
                    app,
                    &state,
                    &server,
                    signed,
                    form_factor,
                    &name,
                    autostart,
                )?;
                clear_if_matches(&state.github_device, &device);
                Ok(json!({ "status": "ok", "app": next }))
            }
        }
    })
    .await
}

/// The GitHub device page is the only GitHub address this app opens.
fn github_device_url_allowed(url: &str) -> bool {
    url == "https://github.com/login/device" || url.starts_with("https://github.com/login/device?")
}

/// The Google consent page is the only Google address this app opens.
fn google_auth_url_allowed(url: &str) -> bool {
    url.starts_with("https://accounts.google.com/")
}

/// Opens an allowlisted address in the system browser. Pass the complete URL
/// directly to Windows' registered handler: cmd truncates OAuth queries at `&`,
/// and explorer can exit without opening the browser (T32).
#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> std::io::Result<()> {
    if std::process::Command::new("open")
        .arg(url)
        .status()?
        .success()
    {
        Ok(())
    } else {
        Err(std::io::Error::other("browser opener failed"))
    }
}

#[cfg(target_os = "windows")]
fn browser_command(url: &str) -> std::io::Result<()> {
    windows_browser_command(url, |address| {
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                window: *mut std::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> isize;
        }
        // SAFETY: address is a live, NUL-terminated UTF-16 buffer for the whole
        // call. Null selects the default "open" operation and no extra args.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                address,
                std::ptr::null(),
                std::ptr::null(),
                1, // SW_SHOWNORMAL
            )
        }
    })
}

#[cfg(any(target_os = "windows", test))]
fn windows_browser_command(
    url: &str,
    open: impl FnOnce(*const u16) -> isize,
) -> std::io::Result<()> {
    if url.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "browser address contains NUL",
        ));
    }
    let address: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let result = open(address.as_ptr());
    // ShellExecuteW reports errors in its return value, not GetLastError.
    if result > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Windows browser opener failed ({result})"
        )))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn browser_command(url: &str) -> std::io::Result<()> {
    if std::process::Command::new("xdg-open")
        .arg(url)
        .status()?
        .success()
    {
        Ok(())
    } else {
        Err(std::io::Error::other("browser opener failed"))
    }
}

/// Opens the GitHub verification page in the system browser. Only that one
/// address is allowed, so this cannot become a general URL opener.
#[tauri::command]
pub async fn open_github_device_page(app: AppHandle, url: String) -> CmdResult<()> {
    if !github_device_url_allowed(&url) {
        return Err(CommandError::new("bad_request"));
    }
    blocking(app, move |_app| {
        browser_command(&url).map_err(|_| CommandError::new("server"))?;
        Ok(())
    })
    .await
}

/// Starts Google sign-in and returns only the consent page to open (D051).
#[tauri::command]
pub async fn start_google_login(app: AppHandle, server: String) -> CmdResult<Value> {
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let start = crate::account::google_start(&server)?;
        *lock(&state.google_flow) = Some(start.flow_id.clone());
        Ok(json!({ "authUrl": start.auth_url }))
    })
    .await
}

/// Polls the started Google flow. On approval this creates the space's device
/// and switches the dashboard to the server, exactly like an email sign-in.
#[tauri::command]
pub async fn poll_google_login(
    app: AppHandle,
    server: String,
    form_factor: FormFactor,
    name: String,
    autostart: bool,
) -> CmdResult<Value> {
    let name = clean_name(&name).ok_or(CommandError::new("bad_request"))?;
    let name = if name.is_empty() {
        default_display_name(this_platform(), form_factor)
    } else {
        name
    };
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let flow = lock(&state.google_flow)
            .clone()
            .ok_or(CommandError::new("bad_request"))?;
        match crate::account::google_poll(&server, &flow)? {
            crate::account::GooglePoll::Pending => Ok(json!({ "status": "pending" })),
            crate::account::GooglePoll::Expired => {
                clear_if_matches(&state.google_flow, &flow);
                Ok(json!({ "status": "expired" }))
            }
            crate::account::GooglePoll::Denied => {
                clear_if_matches(&state.google_flow, &flow);
                Ok(json!({ "status": "denied" }))
            }
            crate::account::GooglePoll::Failed => {
                clear_if_matches(&state.google_flow, &flow);
                Ok(json!({ "status": "failed" }))
            }
            crate::account::GooglePoll::SignedIn(signed) => {
                let next = complete_account_sign_in(
                    app,
                    &state,
                    &server,
                    signed,
                    form_factor,
                    &name,
                    autostart,
                )?;
                clear_if_matches(&state.google_flow, &flow);
                Ok(json!({ "status": "ok", "app": next }))
            }
        }
    })
    .await
}

/// Opens the Google consent page in the system browser. Only that one host is
/// allowed, so this cannot become a general URL opener.
#[tauri::command]
pub async fn open_google_auth_page(app: AppHandle, url: String) -> CmdResult<()> {
    if !google_auth_url_allowed(&url) {
        return Err(CommandError::new("bad_request"));
    }
    blocking(app, move |_app| {
        browser_command(&url).map_err(|_| CommandError::new("server"))?;
        Ok(())
    })
    .await
}

/// The space's persistent, replaceable pairing code. Requires a sign-in.
#[tauri::command]
pub async fn account_pair_code(app: AppHandle, rotate: bool) -> CmdResult<Value> {
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let session = state
            .account_session()
            .ok_or(CommandError::new("not_signed_in"))?;
        let server = state
            .server_credentials()
            .map_or_else(|| crate::account::CLOUD_URL.to_owned(), |c| c.server_url);
        let code = crate::account::pair_code(&server, &session, rotate)?;
        Ok(json!({ "code": code, "replaceable": true }))
    })
    .await
}

/// Signs out of the account and disconnects this computer from its cloud space.
/// Local collection, the local database and the stable account-device binding
/// stay intact; no server device or event is revoked or deleted. The session
/// revocation runs outside the connection lock; its failure cannot undo the
/// already completed return to local data.
fn logout_account_with<R: tauri::Runtime>(
    app: &AppHandle<R>,
    revoke_session: impl FnOnce(&str, &str),
) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    let (signed_out, disconnected) = {
        let _connection = lock(&state.connection);
        let server = state
            .server_credentials()
            .map_or_else(|| crate::account::CLOUD_URL.to_owned(), |c| c.server_url);
        let session = state.account_session();
        let disconnected = state.server_credentials().is_some();
        // If either keychain credential cannot be removed, leave the connection
        // intact rather than pretending sign-out succeeded.
        if session.is_some() {
            state.delete_account_session()?;
        }
        if disconnected && let Err(error) = forget_pairing_locked(app) {
            if let Some(ref previous) = session {
                let _ = state.save_account_session(previous);
            }
            return Err(error);
        }
        clear_account_memory(&state);
        (session.map(|session| (server, session)), disconnected)
    };
    if let Some((server, session)) = signed_out {
        revoke_session(&server, &session);
    }
    changed(
        app,
        if disconnected {
            Poke::Paired
        } else {
            Poke::Status
        },
    );
    Ok(status(app))
}

/// Account sign-out returns every dashboard to this computer. Device tokens
/// are separate from the session, so explicitly retire this cloud connection;
/// otherwise its read token would keep exposing the account's other devices.
#[tauri::command]
pub async fn logout_account(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        logout_account_with(app, |server, session| {
            // Local sign-out must not wait for a slow or offline network. The
            // captured session is independent of any later account sign-in.
            spawn_logout(server.to_owned(), session.to_owned());
        })
    })
    .await
}

/// Retry initialization if this computer's storage was temporarily unavailable.
#[tauri::command]
pub async fn use_local_mode(
    app: AppHandle,
    form_factor: FormFactor,
    name: String,
    autostart: bool,
) -> CmdResult<AppStatus> {
    let name = clean_name(&name).ok_or(CommandError::new("bad_request"))?;
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let _connection = lock(&state.connection);
        if lock(&state.local_credentials).is_none() {
            let next = initialize_local(&state.paths, &state.remote, form_factor, &name)?;
            *lock(&state.local_credentials) = Some(next);
        }
        if matches!(
            state.collector.live().last_error.as_deref(),
            Some(LOCAL_ERROR | KEYCHAIN_ERROR)
        ) {
            state.collector.set_error(None);
        }
        start_collector(app);
        if autostart {
            let _ = write_autostart(app, true);
        }
        changed(app, Poke::Paired);
        Ok(status(app))
    })
    .await
}

/// Retires only this computer's local record. Network credentials, queues and
/// server history are deliberately left untouched.
fn remove_local_device_impl<R: tauri::Runtime>(app: &AppHandle<R>) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
    if state.server_credentials().is_some() {
        return Err(CommandError::new("bad_request"));
    }
    let credentials = lock(&state.local_credentials)
        .clone()
        .filter(local::is_local)
        .ok_or(CommandError::new("not_paired"))?;
    let local = state.remote.local().ok_or(CommandError::new("server"))?;
    let marker = state.paths.paused_marker();
    let was_paused = state.is_paused();
    std::fs::create_dir_all(&state.paths.state_dir)
        .and_then(|()| std::fs::write(&marker, b""))
        .map_err(|_| CommandError::new("server"))?;
    state.collector.stop();
    match local.retire(&credentials.device_id) {
        Ok(true) => {}
        Ok(false) => {
            if !was_paused {
                let _ = std::fs::remove_file(&marker);
                start_collector(app);
            }
            return Err(CommandError::new("not_found"));
        }
        Err(error) => {
            if !was_paused {
                let _ = std::fs::remove_file(&marker);
                start_collector(app);
            }
            return Err(error.into());
        }
    }
    local::delete_credentials(&state.paths.state_dir).map_err(|_| CommandError::new("server"))?;
    lock(&state.local_credentials).take();
    state.collector.reset();
    changed(app, Poke::Paired);
    Ok(status(app))
}

#[tauri::command]
pub async fn remove_local_device(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, remove_local_device_impl).await
}

#[tauri::command]
pub async fn unpair_device(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        logout_account_with(app, |server, session| {
            spawn_logout(server.to_owned(), session.to_owned());
        })
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
pub async fn set_menu_bar_total(app: AppHandle, enabled: bool) -> CmdResult<AppStatus> {
    blocking(app, move |app| apply_menu_bar_total(app, enabled)).await
}

#[tauri::command]
pub async fn create_pair_code(app: AppHandle) -> CmdResult<Value> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        let credentials = state.require_server()?;
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

fn save_device_form_factor<R: tauri::Runtime>(
    app: &AppHandle<R>,
    device_id: String,
    form_factor: FormFactor,
) -> CmdResult<Value> {
    if !remote::valid_device_id(&device_id) {
        return Err(CommandError::new("not_found"));
    }
    let state = app.state::<AppState>();
    let credentials = state.require_credentials()?;
    let path = format!("dashboard/devices/{device_id}/form-factor");
    let reply = state.remote.post(
        &credentials,
        &path,
        &json!({ "formFactor": form_factor.as_str() }),
    )?;
    changed(app, Poke::Status);
    Ok(reply)
}

#[tauri::command]
pub async fn set_device_form_factor(
    app: AppHandle,
    device_id: String,
    form_factor: FormFactor,
) -> CmdResult<Value> {
    blocking(app, move |app| {
        save_device_form_factor(app, device_id, form_factor)
    })
    .await
}

#[tauri::command]
pub async fn rotate_credentials(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        {
            let _connection = lock(&state.connection);
            let current = state.require_server()?;
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
            adopt_locked(
                app,
                Credentials {
                    server_url: current.server_url,
                    device_id: rotated.device_id,
                    ingest_token: rotated.ingest_token,
                    read_token: rotated.read_token,
                },
                true,
            )?;
        }
        changed(app, Poke::Paired);
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
        let credentials = state.require_server()?;
        let path = format!("dashboard/devices/{device_id}/revoke");
        state.remote.post(&credentials, &path, &json!({}))?;
        if device_id == credentials.device_id {
            let signed_out = {
                let _connection = lock(&state.connection);
                if state.server_credentials().as_ref() != Some(&credentials) {
                    return Ok(status(app));
                }
                let session = state.account_session();
                let server = credentials.server_url;
                forget_pairing_locked(app)?;
                let _ = state.delete_account_session();
                clear_account_memory(&state);
                session.map(|session| (server, session))
            };
            if let Some((server, session)) = signed_out {
                spawn_logout(server, session);
            }
            changed(app, Poke::Paired);
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

    fn wait_for_path(path: &std::path::Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("test path was not created: {}", path.display());
    }

    fn prepare_queue(dir: &std::path::Path, device_id: &str) {
        for _ in 0..100 {
            match State::open(dir).and_then(|mut state| {
                state.prepare(device_id, "generation-test")?;
                Ok(())
            }) {
                Ok(()) => return,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        panic!("test queue stayed busy: {}", dir.display());
    }

    #[test]
    fn account_refresh_updates_the_current_profile_and_keeps_it_when_offline() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::load(Paths {
            keychain_service: "unused-test-store".into(),
            state_dir: dir.path().to_path_buf(),
            home: None,
        });
        *lock(&state.account_session) = Some("tls_fixture_current".into());
        let email = "current@example.test".to_owned();
        let avatar = "data:image/png;base64,fixture".to_owned();
        assert_eq!(
            apply_account_refresh(
                &state,
                "tls_fixture_current",
                Ok(crate::account::AccountProfile {
                    email: email.clone(),
                    provider: Some("google".into()),
                    display_name: Some("Current account".into()),
                    avatar_url: Some(avatar.clone()),
                    space_name: Some("Refreshed space".into()),
                })
            ),
            Some(email.clone())
        );
        assert_eq!(lock(&state.account_provider).as_deref(), Some("google"));
        assert_eq!(
            lock(&state.account_display_name).as_deref(),
            Some("Current account")
        );
        assert_eq!(
            lock(&state.account_avatar_url).as_deref(),
            Some(avatar.as_str())
        );
        assert_eq!(
            lock(&state.account_space).as_deref(),
            Some("Refreshed space")
        );
        assert_eq!(
            apply_account_refresh(
                &state,
                "tls_fixture_current",
                Err(CommandError::new("network"))
            ),
            Some(email)
        );
        assert_eq!(
            state.account_session().as_deref(),
            Some("tls_fixture_current")
        );
    }

    #[test]
    fn delayed_account_refresh_cannot_restore_a_logout_or_clear_a_new_sign_in() {
        for signed_in in [false, true] {
            for unauthorized in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let state = AppState::load(Paths {
                    keychain_service: "unused-test-store".into(),
                    state_dir: dir.path().to_path_buf(),
                    home: None,
                });
                *lock(&state.account_session) = Some("tls_fixture_old".into());
                let (started, request_started) = std::sync::mpsc::channel();
                let (respond, response) = std::sync::mpsc::channel();
                std::thread::scope(|scope| {
                    let pending_state = &state;
                    let pending = scope.spawn(move || {
                        let session = pending_state.account_session().unwrap();
                        started.send(()).unwrap();
                        response.recv().unwrap();
                        let reply = if unauthorized {
                            Err(CommandError::new("unauthorized"))
                        } else {
                            Ok(crate::account::AccountProfile {
                                email: "old@example.test".into(),
                                provider: Some("github".into()),
                                display_name: Some("Old account".into()),
                                avatar_url: None,
                                space_name: Some("Old space".into()),
                            })
                        };
                        apply_account_refresh(pending_state, &session, reply)
                    });
                    request_started.recv().unwrap();
                    let expected_email = signed_in.then(|| "new@example.test".to_owned());
                    {
                        // The same publication lock is used by email/GitHub
                        // sign-in and logout while this old request is pending.
                        let _connection = lock(&state.connection);
                        *lock(&state.account_session) = signed_in.then(|| "tls_fixture_new".into());
                        *lock(&state.account_email) = expected_email.clone();
                        *lock(&state.account_display_name) =
                            signed_in.then(|| "New account".into());
                        respond.send(()).unwrap();
                    }
                    assert_eq!(pending.join().unwrap(), expected_email);
                    assert_eq!(state.account_email(), expected_email);
                    assert_eq!(
                        state.account_session().as_deref(),
                        signed_in.then_some("tls_fixture_new")
                    );
                    assert_eq!(
                        lock(&state.account_display_name).as_deref(),
                        signed_in.then_some("New account")
                    );
                });
            }
        }
    }

    #[test]
    fn an_existing_cloud_device_is_reused_for_repeated_account_sign_ins() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let credentials = state
            .remote
            .local()
            .unwrap()
            .pair(FormFactor::Desktop, "Mac mini")
            .unwrap();
        adopt(&app, credentials.clone(), false).unwrap();
        assert!(
            reusable_server_credentials(&state, &credentials.server_url)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            state.server_credentials().unwrap().device_id,
            credentials.device_id
        );
        let devices = state.remote.get(&credentials, "dashboard/devices").unwrap();
        assert_eq!(
            devices["devices"].as_array().unwrap().len(),
            1 + usize::from(lock(&state.local_credentials).is_some())
        );
        assert!(
            reusable_server_credentials(&state, "https://another.example")
                .unwrap()
                .is_none()
        );
        state.remote.local().unwrap().revoke(&credentials.device_id);
        assert!(
            reusable_server_credentials(&state, &credentials.server_url)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn local_initialization_preserves_valid_devices_and_repairs_a_recreated_database() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            keychain_service: "unused-test-store".into(),
            state_dir: dir.path().to_path_buf(),
            home: None,
        };
        let remote = Remote::new();
        remote.set_local(Some(LocalServer::open_with(dir.path(), None).unwrap()));
        let first =
            initialize_local(&paths, &remote, FormFactor::Laptop, "Fixture computer").unwrap();
        assert!(paths.state_dir.join("local-queue-v2").exists());
        assert!(
            !legacy_queue_required(dir.path()),
            "a restart before collection starts must retain the new layout"
        );
        let same = initialize_local(&paths, &remote, FormFactor::Desktop, "Other name").unwrap();
        assert_eq!(same.device_id, first.device_id);
        remote.local().unwrap().revoke(&first.device_id);
        let repaired =
            initialize_local(&paths, &remote, FormFactor::Laptop, "Fixture computer").unwrap();
        assert_ne!(repaired.device_id, first.device_id);
        assert!(remote.get(&repaired, "dashboard/health").is_ok());
    }

    #[test]
    fn local_form_factor_editing_preserves_device_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let original = state.require_credentials().unwrap();
        let before = state.remote.get(&original, "dashboard/devices").unwrap();
        let name = before["devices"][0]["displayName"].clone();

        let reply =
            save_device_form_factor(&app, original.device_id.clone(), FormFactor::Desktop).unwrap();
        assert_eq!(reply["formFactor"], "desktop");
        let after = state.remote.get(&original, "dashboard/devices").unwrap();
        assert_eq!(after["devices"][0]["formFactor"], "desktop");
        assert_eq!(after["devices"][0]["displayName"], name);
        let saved = local::load_credentials(&state.paths.state_dir).unwrap();
        assert_eq!(saved.device_id, original.device_id);
        assert_eq!(saved.ingest_token, original.ingest_token);
        assert_eq!(saved.read_token, original.read_token);
        assert_eq!(
            save_device_form_factor(&app, "dev_missing".to_owned(), FormFactor::Laptop)
                .unwrap_err()
                .kind,
            "not_found"
        );
    }

    #[test]
    fn removing_an_unconnected_local_device_retires_it_and_pauses_collection() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let credentials = state.require_credentials().unwrap();
        let removed = remove_local_device_impl(&app);
        assert!(removed.is_ok(), "local removal failed: {removed:?}");
        let removed = removed.unwrap();
        assert!(!removed.paired);
        assert_eq!(removed.mode, None);
        assert!(removed.collector.paused);
        assert!(state.local_credentials.lock().unwrap().is_none());
        assert!(local::load_credentials(&state.paths.state_dir).is_none());
        assert_eq!(
            state
                .remote
                .get(&credentials, "dashboard/health")
                .unwrap_err()
                .kind,
            "unauthorized"
        );
        assert!(
            !state
                .remote
                .local()
                .unwrap()
                .retire(&credentials.device_id)
                .unwrap()
        );
        assert_eq!(
            remove_local_device_impl(&app).unwrap_err().kind,
            "not_paired"
        );
        let next =
            initialize_local(&state.paths, &state.remote, FormFactor::Desktop, "Fresh").unwrap();
        assert_ne!(next.device_id, credentials.device_id);
        assert!(
            state.is_paused(),
            "retrying must not silently resume collection"
        );
    }

    #[test]
    fn a_marker_without_a_local_queue_still_requires_interrupted_migration() {
        let dir = tempfile::tempdir().unwrap();
        let credentials = Credentials {
            server_url: local::LOCAL_SERVER_URL.to_owned(),
            device_id: "dev_legacy_local".to_owned(),
            ingest_token: "tdi_fixture".to_owned(),
            read_token: "tdr_fixture".to_owned(),
        };
        local::save_credentials(dir.path(), &credentials).unwrap();
        std::fs::write(dir.path().join("local-queue-v2"), b"").unwrap();
        let mut root = State::open(dir.path()).unwrap();
        root.prepare(&credentials.device_id, "generation-test")
            .unwrap();
        drop(root);
        assert!(legacy_queue_required(dir.path()));
        std::fs::create_dir_all(dir.path().join("local-queue")).unwrap();
        State::open(&dir.path().join("local-queue")).unwrap();
        assert!(!legacy_queue_required(dir.path()));
    }

    #[test]
    fn existing_local_credentials_without_a_layout_marker_require_queue_migration() {
        let dir = tempfile::tempdir().unwrap();
        local::save_credentials(
            dir.path(),
            &Credentials {
                server_url: local::LOCAL_SERVER_URL.to_owned(),
                device_id: "dev_legacy_local".to_owned(),
                ingest_token: "tdi_fixture".to_owned(),
                read_token: "tdr_fixture".to_owned(),
            },
        )
        .unwrap();
        assert!(!legacy_queue_required(dir.path()));
        State::open(dir.path()).unwrap();
        assert!(legacy_queue_required(dir.path()));
        State::open(&dir.path().join("local-queue")).unwrap();
        assert!(!legacy_queue_required(dir.path()));
    }

    #[test]
    fn dashboard_always_reads_the_complete_view() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let local = state.require_credentials().unwrap();
        let server = state
            .remote
            .local()
            .unwrap()
            .pair(FormFactor::Laptop, "Cloud")
            .unwrap();
        let server = Credentials {
            server_url: "https://dash.example.com".into(),
            ..server
        };
        *lock(&state.server_credentials) = Some(server.clone());
        // A preference written by an older release must not revive a local-only
        // view: the cloud view already includes this computer.
        std::fs::write(dir.path().join("dashboard-mode"), "local").unwrap();
        assert_eq!(state.credentials().unwrap().device_id, server.device_id);
        assert_eq!(status(&app).mode, Some("server"));
        assert!(!local::is_local(&state.credentials().unwrap()));
        *lock(&state.server_credentials) = None;
        assert_eq!(state.credentials().unwrap().device_id, local.device_id);
        assert_eq!(status(&app).mode, Some("local"));
    }

    #[test]
    fn removing_a_server_queue_does_not_delete_a_paused_legacy_local_queue() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            keychain_service: "unused-test-store".into(),
            state_dir: dir.path().to_path_buf(),
            home: None,
        };
        let mut queue = State::open(dir.path()).unwrap();
        queue.prepare("dev_local", "generation-test").unwrap();
        drop(queue);
        remove_server_queue(&paths, "dev_server");
        assert!(State::exists_in(dir.path()));
        assert_eq!(
            State::open(dir.path())
                .unwrap()
                .meta(collector_core::state::meta::DEVICE_ID)
                .unwrap()
                .as_deref(),
            Some("dev_local")
        );
    }

    #[test]
    fn failed_server_pairing_keeps_the_local_credentials_and_view() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let local = lock(&state.local_credentials).clone().unwrap();
        let next = credentials::claim(
            state.remote.transport(),
            "http://127.0.0.1:1",
            "synthetic-invalid",
            FormFactor::Laptop,
            "Fixture computer",
        );
        assert!(next.is_err());
        assert_eq!(status(&app).mode, Some("local"));
        assert_eq!(status(&app).device_id, Some(local.device_id));
        assert!(state.server_credentials().is_none());
        assert!(local::load_credentials(&state.paths.state_dir).is_some());
    }

    #[test]
    fn pairing_and_disconnect_keep_the_local_destination_and_clear_network_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            keychain_service: "unused-test-store".into(),
            state_dir: dir.path().to_path_buf(),
            home: None,
        };
        let remote = Remote::new();
        remote.set_local(Some(LocalServer::open_with(dir.path(), None).unwrap()));
        let local =
            initialize_local(&paths, &remote, FormFactor::Laptop, "Fixture computer").unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState {
                paths,
                collector: Collector::new(),
                remote,
                new_install: false,
                legacy_local_queue: false,
                local_credentials: Mutex::new(Some(local.clone())),
                server_credentials: Mutex::new(None),
                credential_override: Some(std::sync::Arc::new(
                    collector_core::credentials::MemoryStore::default(),
                )),
                account_session: Mutex::new(None),
                account_email: Mutex::new(None),
                account_provider: Mutex::new(None),
                account_display_name: Mutex::new(None),
                account_avatar_url: Mutex::new(None),
                account_space: Mutex::new(None),
                github_device: Mutex::new(None),
                google_flow: Mutex::new(None),
                menu_bar_total: AtomicBool::new(true),
                connection: Mutex::new(()),
            })
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let server = state.remote.local().unwrap().clone();
        let credentials = server.pair(FormFactor::Laptop, "Fixture server").unwrap();
        let credentials = Credentials {
            server_url: "http://127.0.0.1:18787".to_owned(),
            ..credentials
        };
        let adopted = adopt(&app, credentials.clone(), false);
        assert!(
            adopted.is_ok(),
            "test pairing should use the in-memory store: {adopted:?}"
        );
        assert!(state.server_credentials().is_some());
        assert_eq!(status(&app).mode, Some("server"));
        assert!(state.paths.state_dir.join("local-queue-v2").exists());
        assert!(local::load_credentials(&state.paths.state_dir).is_some());
        // Disconnect uses the same state transition even when the platform
        // keychain is unavailable; production still returns its keychain error.
        let _ = forget_pairing(&app);
        assert!(state.server_credentials().is_none());
        assert_eq!(status(&app).mode, Some("local"));
        assert_eq!(status(&app).device_id, Some(local.device_id));
    }

    #[test]
    fn a_revoked_cloud_connection_returns_to_local_without_touching_local_history() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let local = lock(&state.local_credentials).clone().unwrap();
        let server = state
            .remote
            .local()
            .unwrap()
            .pair(FormFactor::Desktop, "Old cloud")
            .unwrap();
        let server = Credentials {
            server_url: "https://old.example".into(),
            ..server
        };
        adopt(&app, server.clone(), false).unwrap();
        prepare_queue(dir.path(), &server.device_id);
        std::fs::write(dir.path().join("account-device-binding"), b"binding").unwrap();
        *lock(&state.account_session) = Some("tls_old".into());
        *lock(&state.account_email) = Some("old@example.test".into());
        *lock(&state.account_space) = Some("Old space".into());
        let local_queue = dir.path().join("local-queue");
        let local_database = local::database_path(dir.path());
        wait_for_path(&local_queue);
        wait_for_path(&local_database);
        assert!(local_queue.exists());
        assert!(local_database.exists());

        let error = handle_server_unauthorized(&app, server).unwrap_err();
        assert_eq!(error.kind, "unauthorized");
        assert!(state.server_credentials().is_none());
        assert!(
            state
                .credential_override
                .as_ref()
                .unwrap()
                .load()
                .unwrap()
                .is_none()
        );
        assert!(!State::exists_in(dir.path()));
        assert!(local_queue.exists());
        assert!(local_database.exists());
        assert!(local::load_credentials(dir.path()).is_some());
        assert!(dir.path().join("account-device-binding").exists());
        assert_eq!(status(&app).mode, Some("local"));
        assert_eq!(status(&app).device_id, Some(local.device_id));
        assert_eq!(state.account_session(), None);
        assert_eq!(state.account_email(), None);
        assert_eq!(*lock(&state.account_space), None);
    }

    #[test]
    fn account_logout_returns_to_local_without_revoking_cloud_devices_or_history() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let local = lock(&state.local_credentials).clone().unwrap();
        let cloud_dir = dir.path().join("cloud-fixture");
        std::fs::create_dir_all(&cloud_dir).unwrap();
        let cloud_server = LocalServer::open_with(&cloud_dir, None).unwrap();
        let cloud_get = |credentials: &Credentials, path: &str| {
            let response = local::LocalTransport(cloud_server.clone())
                .get(
                    &format!(
                        "{}/api/{path}",
                        credentials.server_url.trim_end_matches('/')
                    ),
                    Some(&credentials.read_token),
                )
                .unwrap();
            assert_eq!(response.status, 200);
            serde_json::from_slice::<Value>(&response.body).unwrap()
        };
        // Route the synthetic cloud registry through the in-process transport.
        // Only the account-session callback is replaced; the state transition
        // and all device/event reads are the production implementation.
        let cloud = cloud_server
            .pair(FormFactor::Desktop, "This computer in the cloud")
            .unwrap();
        let other = cloud_server
            .pair(FormFactor::Laptop, "Another computer")
            .unwrap();
        adopt(&app, cloud.clone(), false).unwrap();
        *lock(&state.account_session) = Some("tls_fixture_logout".into());
        *lock(&state.account_email) = Some("member@example.test".into());
        *lock(&state.account_provider) = Some("github".into());
        *lock(&state.account_display_name) = Some("Fixture account".into());
        *lock(&state.account_space) = Some("Fixture space".into());
        let binding = dir.path().join("account-device-binding");
        std::fs::write(&binding, b"stable-fixture-binding").unwrap();
        prepare_queue(dir.path(), &cloud.device_id);

        // Keep real cloud events and another device in the registry. Returning
        // to local must not turn either into a revoked or deleted cloud record.
        let events: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/usage-events.json")).unwrap();
        let envelope = json!({
            "schemaVersion": 1,
            "deviceId": cloud.device_id,
            "batchId": "batch-account-logout-fixture",
            "collector": {"version": "test", "tokscaleRev": "test", "parserGeneration": "test"},
            "scanGeneration": "test",
            "events": events,
        });
        let uploaded = cloud_server
            .request(
                axum::http::Method::POST,
                "/api/ingest/events",
                Some(&cloud.ingest_token),
                false,
                &serde_json::to_vec(&envelope).unwrap(),
            )
            .unwrap();
        assert_eq!(uploaded.status, 200);
        let summary_path = "dashboard/summary?from=2023-11-01&to=2023-11-30";
        let before = cloud_get(&cloud, summary_path);
        assert_eq!(before["inputTokens"], 540);
        assert_eq!(
            cloud_get(&cloud, "dashboard/devices")["devices"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let revoked_sessions = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured_sessions = std::sync::Arc::clone(&revoked_sessions);
        let result = logout_account_with(&app, move |server, session| {
            lock(&captured_sessions).push((server.to_owned(), session.to_owned()));
        })
        .unwrap();
        let revoked_sessions = lock(&revoked_sessions);
        assert_eq!(revoked_sessions.len(), 1);
        assert_eq!(result.mode, Some("local"));
        assert_eq!(result.device_id, Some(local.device_id.clone()));
        assert_eq!(result.account_email, None);
        assert_eq!(result.account_display_name, None);
        assert_eq!(result.account_space, None);
        assert!(state.server_credentials().is_none());
        assert!(state.account_session().is_none());
        assert_eq!(local::load_credentials(dir.path()), Some(local.clone()));
        assert_eq!(state.credentials(), Some(local.clone()));
        assert!(binding.exists(), "re-login still reuses the cloud device");
        assert!(
            !State::exists_in(dir.path()),
            "only the cloud queue is retired"
        );
        assert!(State::exists_in(&dir.path().join("local-queue")));
        assert!(local::database_path(dir.path()).exists());
        assert!(!state.is_paused());

        let local_devices = state.remote.get(&local, "dashboard/devices").unwrap();
        let local_devices = local_devices["devices"].as_array().unwrap();
        assert_eq!(local_devices.len(), 1);
        assert_eq!(local_devices[0]["deviceId"], local.device_id);

        // The app's local sign-out does not revoke cloud device credentials.
        // The real session endpoint is covered by the server account tests.
        assert!(cloud_get(&cloud, "dashboard/health")["status"].is_string());
        assert!(cloud_get(&other, "dashboard/health")["status"].is_string());
        let after = cloud_get(&other, summary_path);
        assert_eq!(after["inputTokens"], before["inputTokens"]);
    }

    #[test]
    fn a_delayed_rejection_cannot_clear_a_newer_cloud_connection() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let pair = |name| {
            let credentials = state
                .remote
                .local()
                .unwrap()
                .pair(FormFactor::Desktop, name)
                .unwrap();
            Credentials {
                server_url: "https://old.example".into(),
                ..credentials
            }
        };
        let old = pair("Old cloud");
        adopt(&app, old.clone(), false).unwrap();
        let mut queue = State::open(dir.path()).unwrap();
        queue.prepare(&old.device_id, "generation-test").unwrap();
        drop(queue);
        // Rotation can keep the device id, so the complete credential matters.
        let newer = Credentials {
            read_token: "tdr_rotated_fixture".into(),
            ingest_token: "tdi_rotated_fixture".into(),
            ..old.clone()
        };
        adopt(&app, newer.clone(), false).unwrap();
        *lock(&state.account_session) = Some("tls_new".into());
        *lock(&state.account_email) = Some("new@example.test".into());

        assert!(handle_server_unauthorized(&app, old).is_err());
        assert_eq!(state.server_credentials(), Some(newer));
        assert!(State::exists_in(dir.path()));
        assert_eq!(state.account_session().as_deref(), Some("tls_new"));
        assert_eq!(state.account_email().as_deref(), Some("new@example.test"));
    }

    #[test]
    fn only_a_current_revoked_account_session_also_disconnects_its_cloud() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let old = Credentials {
            server_url: "https://old.example".into(),
            ..state
                .remote
                .local()
                .unwrap()
                .pair(FormFactor::Desktop, "Old cloud")
                .unwrap()
        };
        adopt(&app, old.clone(), false).unwrap();
        *lock(&state.account_session) = Some("tls_new".into());
        *lock(&state.account_email) = Some("new@example.test".into());
        let rejected = Err(CommandError::new("unauthorized"));
        assert_eq!(
            handle_account_refresh(&app, "tls_old", Some(old.clone()), rejected).unwrap(),
            Some("new@example.test".into())
        );
        assert_eq!(state.server_credentials(), Some(old.clone()));

        for kind in ["network", "server"] {
            assert_eq!(
                handle_account_refresh(
                    &app,
                    "tls_new",
                    Some(old.clone()),
                    Err(CommandError::new(kind)),
                )
                .unwrap(),
                Some("new@example.test".into())
            );
            assert_eq!(state.server_credentials(), Some(old.clone()));
        }
        assert_eq!(
            handle_account_refresh(
                &app,
                "tls_new",
                Some(old),
                Err(CommandError::new("unauthorized")),
            )
            .unwrap(),
            None
        );
        assert!(state.server_credentials().is_none());
        assert_eq!(status(&app).mode, Some("local"));
    }

    #[test]
    fn windows_browser_receives_the_complete_utf16_url() {
        let url = "https://accounts.google.com/o/oauth2/v2/auth?client_id=fixture\
            &redirect_uri=https%3A%2F%2Ftokenlens.online%2Fcallback\
            &response_type=code&scope=openid+email+profile&state=fixture&label=测试🦀";
        let expected: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        windows_browser_command(url, |address| {
            // SAFETY: the injected call runs while the known-length URL buffer
            // is alive, exactly as ShellExecuteW does.
            let received = unsafe { std::slice::from_raw_parts(address, expected.len()) };
            assert_eq!(received, expected);
            33
        })
        .unwrap();
    }

    #[test]
    fn windows_browser_propagates_every_shell_failure() {
        for result in 0..=32 {
            assert!(windows_browser_command("https://accounts.google.com/", |_| result).is_err());
        }
        assert!(windows_browser_command("https://accounts.google.com/", |_| 33).is_ok());
    }

    #[test]
    fn windows_browser_rejects_nul_before_calling_the_shell() {
        let error = windows_browser_command("https://accounts.google.com/\0ignored", |_| {
            panic!("an address containing NUL must not reach the shell")
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn oauth_urls_keep_their_query_parameters_and_pass_the_allowlist() {
        // The server builds these addresses. The Windows opener once went
        // through cmd.exe, which split the query at the first `&` and dropped
        // `response_type` (D051 hotfix); pin the full shape and the allowlist.
        let google = "https://accounts.google.com/o/oauth2/v2/auth?client_id=abc\
            &redirect_uri=https%3A%2F%2Ftokenlens.online%2Fapi%2Fauth%2Fgoogle%2Fcallback\
            &response_type=code&scope=openid+email+profile&state=xyz\
            &code_challenge=abc&code_challenge_method=S256&prompt=select_account";
        assert!(google_auth_url_allowed(google));
        assert!(google.contains("response_type=code"));
        assert!(github_device_url_allowed("https://github.com/login/device"));
        for bad in [
            "https://accounts.google.com.evil.example/o/oauth2/v2/auth",
            "https://example.com/https://accounts.google.com/",
            "http://accounts.google.com/o/oauth2/v2/auth",
            "https://github.com/login/device.evil.example",
            "https://github.com.evil.example/login/device",
        ] {
            assert!(!google_auth_url_allowed(bad), "{bad}");
            assert!(!github_device_url_allowed(bad), "{bad}");
        }
    }

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

    #[test]
    fn clear_if_matches_preserves_newer_oauth_flow_id() {
        let slot = Mutex::new(Some("flow-new".to_owned()));
        clear_if_matches(&slot, "flow-old");
        assert_eq!(lock(&slot).as_deref(), Some("flow-new"));
        clear_if_matches(&slot, "flow-new");
        assert!(lock(&slot).is_none());
    }

    #[test]
    fn finish_update_and_resume_respects_current_pause_state() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();

        // Paused before update; unpaused mid-install -> resumes cleanly.
        std::fs::write(state.paths.paused_marker(), b"").unwrap();
        state.collector.begin_update();
        assert!(!state.collector.is_running());
        std::fs::remove_file(state.paths.paused_marker()).unwrap();
        finish_update_and_resume(&app);
        assert!(state.collector.is_running());

        // Paused after finish test -> stays stopped when paused mid-install.
        state.collector.begin_update();
        std::fs::write(state.paths.paused_marker(), b"").unwrap();
        finish_update_and_resume(&app);
        assert!(!state.collector.is_running());
        state.collector.stop();
    }

    #[test]
    fn server_min_app_version_flags_app_outdated_only_in_server_mode() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        state
            .collector
            .set_server_min_app_version(Some("99.0.0".to_owned()));
        assert!(!status(&app).app_outdated);

        let server = Credentials {
            server_url: "https://dash.example.com".into(),
            ..state
                .remote
                .local()
                .unwrap()
                .pair(FormFactor::Desktop, "Cloud")
                .unwrap()
        };
        *lock(&state.server_credentials) = Some(server);
        let outdated = status(&app);
        assert_eq!(outdated.min_app_version.as_deref(), Some("99.0.0"));
        assert!(outdated.app_outdated);

        state
            .collector
            .set_server_min_app_version(Some("0.1.0".to_owned()));
        assert!(!status(&app).app_outdated);
    }

    #[test]
    fn disconnecting_clears_signed_in_account_fields() {
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .manage(AppState::load(Paths {
                keychain_service: "unused-test-store".into(),
                state_dir: dir.path().to_path_buf(),
                home: None,
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let app = app.handle().clone();
        let state = app.state::<AppState>();
        let server = Credentials {
            server_url: "https://dash.example.com".into(),
            ..state
                .remote
                .local()
                .unwrap()
                .pair(FormFactor::Desktop, "Cloud")
                .unwrap()
        };
        adopt(&app, server, false).unwrap();
        set_signed_in_account(
            &state,
            crate::account::SignedIn {
                session: "tls_fixture".into(),
                email: "user@example.test".into(),
                space_name: "Space".into(),
                provider: Some("github".into()),
                display_name: Some("User".into()),
                avatar_url: None,
            },
        );
        let result = logout_account_with(&app, |_, _| {}).unwrap();
        assert_eq!(result.mode, Some("local"));
        assert_eq!(result.account_email, None);
        assert_eq!(result.account_space, None);
        assert!(state.account_session().is_none());
        assert!(state.server_credentials().is_none());
    }
}
