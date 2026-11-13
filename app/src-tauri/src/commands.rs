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
    new_install: bool,
    legacy_local_queue: bool,
    /// Read from the keychain once at startup so that status polling never
    /// touches the keychain (and never triggers its prompts).
    local_credentials: Mutex<Option<Credentials>>,
    server_credentials: Mutex<Option<Credentials>>,
    #[cfg(test)]
    credential_override: Option<std::sync::Arc<collector_core::credentials::MemoryStore>>,
    dashboard_mode: Mutex<String>,
    /// Account session and its email (D049); keychain-only, never reaches the
    /// WebView. Status polling reads the cached email without a request.
    account_session: Mutex<Option<String>>,
    account_email: Mutex<Option<String>>,
    account_space: Mutex<Option<String>>,
    /// A started GitHub device flow; the code stays in Rust (D049).
    github_device: Mutex<Option<String>>,
    /// Serializes pairing, disconnects, and credential rotation.
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
        let local_credentials = match initialize_local(&paths, &remote, FormFactor::Laptop, "") {
            Ok(credentials) => Some(credentials),
            Err(_error) => {
                #[cfg(test)]
                eprintln!("test local initialization failed: {_error:?}");
                collector.set_error(Some(LOCAL_ERROR));
                None
            }
        };
        let account_session = crate::account::load_session(&paths.keychain_service);
        // Preserve the view of existing server users; new installs start locally.
        let dashboard_mode = dashboard_mode(&paths.state_dir, server_credentials.is_some());
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
            dashboard_mode: Mutex::new(dashboard_mode.to_owned()),
            account_session: Mutex::new(account_session),
            account_email: Mutex::new(None),
            account_space: Mutex::new(None),
            github_device: Mutex::new(None),
            connection: Mutex::new(()),
        }
    }

    pub fn credentials(&self) -> Option<Credentials> {
        if *lock(&self.dashboard_mode) == "server" {
            // Server reads never fall back to a local credential. Otherwise a
            // damaged remote queue could silently masquerade as local data.
            self.server_credentials()
        } else {
            lock(&self.local_credentials).clone()
        }
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

    fn select_dashboard(&self, mode: &str) -> CmdResult<()> {
        if !matches!(mode, "local" | "server") {
            return Err(CommandError::new("bad_request"));
        }
        if mode == "server" && self.server_credentials().is_none() {
            return Err(CommandError::new("not_paired"));
        }
        std::fs::create_dir_all(&self.paths.state_dir)
            .and_then(|()| std::fs::write(self.paths.state_dir.join("dashboard-mode"), mode))
            .map_err(|_| CommandError::new("server"))?;
        *lock(&self.dashboard_mode) = mode.to_owned();
        Ok(())
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
    /// Selected dashboard source; collection always keeps a local copy (D048).
    pub mode: Option<&'static str>,
    pub device_id: Option<String>,
    /// Connected server address, also present while viewing local data.
    pub server: Option<String>,
    pub app_version: String,
    pub collector: CollectorStatus,
    pub autostart: Option<bool>,
    pub platform: &'static str,
    /// Default hosted service, so the app can pre-select it (D049).
    pub cloud_server: &'static str,
    /// Signed-in account, when this computer has one.
    pub account_email: Option<String>,
    /// Name of the signed-in space, for the account row in the app.
    pub account_space: Option<String>,
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
        .and_then(|_| sync::local_status(&queue).ok().flatten());
    let persisted_upload_state = (!viewing_local && !live.server_queue_error)
        .then(|| local.as_ref().and_then(|l| l.upload_state.clone()))
        .flatten();
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
        app_version: app.package_info().version.to_string(),
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
        platform: platform(),
        cloud_server: crate::account::CLOUD_URL,
        account_email: state.account_email(),
        account_space: lock(&state.account_space).clone(),
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

/// Starts local collection and optional server sync unless the user paused.
pub fn start_collector<R: tauri::Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    if state.is_paused() {
        return;
    }
    let Some(credentials) = lock(&state.local_credentials).clone() else {
        state.collector.set_error(Some(LOCAL_ERROR));
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

fn forget_pairing<R: tauri::Runtime>(app: &AppHandle<R>) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
    // A keychain failure leaves the current connection usable; local collection
    // is never retired or deleted when disconnecting a network server.
    state
        .delete_server_credentials()
        .map_err(|_| CommandError::new("keychain"))?;
    state.collector.stop();
    if let Some(previous) = lock(&state.server_credentials).take() {
        remove_server_queue(&state.paths, &previous.device_id);
    }
    let selected = state.select_dashboard("local");
    state.collector.reset();
    start_collector(app);
    changed(app, Poke::Paired);
    selected
}

fn adopt<R: tauri::Runtime>(
    app: &AppHandle<R>,
    next: Credentials,
    rotating: bool,
) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
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
    let selected = if rotating {
        Ok(())
    } else {
        state.select_dashboard("server")
    };
    if saved.is_err() {
        state.collector.set_error(Some(KEYCHAIN_ERROR));
    }
    start_collector(app);
    changed(app, Poke::Paired);
    saved.and(selected)
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

fn dashboard_mode(state_dir: &std::path::Path, has_server: bool) -> &'static str {
    match std::fs::read_to_string(state_dir.join("dashboard-mode"))
        .ok()
        .as_deref()
    {
        Some("local") => "local",
        _ if has_server => "server",
        _ => "local",
    }
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
        let _ = std::fs::remove_file(&marker);
        start_collector(app);
    }
    changed(app, Poke::Status);
    Ok(status(app))
}

/// Restarting runs a full scan, an upload round and a heartbeat right away.
pub fn restart_collector(app: &AppHandle) -> CmdResult<AppStatus> {
    let state = app.state::<AppState>();
    let _connection = lock(&state.connection);
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
        let next = crate::account::create_device(&server, &signed.session, form_factor, &name)?;
        adopt(app, next, false)?;
        crate::account::save_session(&state.paths.keychain_service, &signed.session)?;
        *lock(&state.account_session) = Some(signed.session);
        *lock(&state.account_email) = Some(signed.email.clone());
        *lock(&state.account_space) = Some(signed.space_name.clone());
        if autostart {
            let _ = write_autostart(app, true);
        }
        changed(app, Poke::Paired);
        Ok(status(app))
    })
    .await
}

/// Confirms the saved account session still works and caches its email.
#[tauri::command]
pub async fn refresh_account(app: AppHandle) -> CmdResult<Option<String>> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        let Some(session) = state.account_session() else {
            return Ok(None);
        };
        let server = state
            .server_credentials()
            .map_or_else(|| crate::account::CLOUD_URL.to_owned(), |c| c.server_url);
        match crate::account::account_email(&server, &session) {
            Ok(email) => {
                *lock(&state.account_email) = Some(email.clone());
                Ok(Some(email))
            }
            // A revoked or expired session must not linger as "signed in".
            Err(error) if error.kind == "unauthorized" => {
                crate::account::delete_session(&state.paths.keychain_service);
                let _ = lock(&state.account_session).take();
                let _ = lock(&state.account_email).take();
                Ok(None)
            }
            Err(_) => Ok(state.account_email()),
        }
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
        }))
    })
    .await
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
                let _ = lock(&state.github_device).take();
                Ok(json!({ "status": "expired" }))
            }
            crate::account::GitHubPoll::Denied => Ok(json!({ "status": "denied" })),
            crate::account::GitHubPoll::Failed => Ok(json!({ "status": "failed" })),
            crate::account::GitHubPoll::SignedIn(signed) => {
                let next =
                    crate::account::create_device(&server, &signed.session, form_factor, &name)?;
                adopt(app, next, false)?;
                crate::account::save_session(&state.paths.keychain_service, &signed.session)?;
                *lock(&state.account_session) = Some(signed.session);
                *lock(&state.account_email) = Some(signed.email.clone());
                *lock(&state.account_space) = Some(signed.space_name.clone());
                let _ = lock(&state.github_device).take();
                if autostart {
                    let _ = write_autostart(app, true);
                }
                changed(app, Poke::Paired);
                Ok(json!({ "status": "ok", "app": status(app) }))
            }
        }
    })
    .await
}

/// Opens the GitHub verification page in the system browser. Only that one
/// address is allowed, so this cannot become a general URL opener.
#[tauri::command]
pub async fn open_github_device_page(app: AppHandle, url: String) -> CmdResult<()> {
    if !url.starts_with("https://github.com/login/device") {
        return Err(CommandError::new("bad_request"));
    }
    blocking(app, move |_app| {
        #[cfg(target_os = "macos")]
        let opened = std::process::Command::new("open").arg(&url).status();
        #[cfg(target_os = "windows")]
        let opened = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&url)
            .status();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let opened = std::process::Command::new("xdg-open").arg(&url).status();
        opened.map_err(|_| CommandError::new("server"))?;
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

/// Forgets the account session here. Device collection keeps running, because
/// its upload tokens are separate; only account features need a new sign-in.
#[tauri::command]
pub async fn logout_account(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
        lock(&state.account_email).take();
        lock(&state.account_space).take();
        if let Some(session) = lock(&state.account_session).take() {
            let server = state
                .server_credentials()
                .map_or_else(|| crate::account::CLOUD_URL.to_owned(), |c| c.server_url);
            crate::account::logout(&server, &session);
            crate::account::delete_session(&state.paths.keychain_service);
        }
        changed(app, Poke::Status);
        Ok(status(app))
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
            state.collector.set_error(None);
        }
        state.select_dashboard("local")?;
        start_collector(app);
        if autostart {
            let _ = write_autostart(app, true);
        }
        changed(app, Poke::Paired);
        Ok(status(app))
    })
    .await
}

/// Selects a dashboard source without changing either collector destination.
#[tauri::command]
pub async fn set_dashboard_mode(app: AppHandle, mode: String) -> CmdResult<AppStatus> {
    blocking(app, move |app| {
        let state = app.state::<AppState>();
        let _connection = lock(&state.connection);
        state.select_dashboard(&mode)?;
        changed(app, Poke::Paired);
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
        let _connection = lock(&state.connection);
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

#[tauri::command]
pub async fn rotate_credentials(app: AppHandle) -> CmdResult<AppStatus> {
    blocking(app, |app| {
        let state = app.state::<AppState>();
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
        adopt(
            app,
            Credentials {
                server_url: current.server_url,
                device_id: rotated.device_id,
                ingest_token: rotated.ingest_token,
                read_token: rotated.read_token,
            },
            true,
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
        let credentials = state.require_server()?;
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
    fn dashboard_defaults_preserve_existing_server_users() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dashboard_mode(dir.path(), false), "local");
        assert_eq!(dashboard_mode(dir.path(), true), "server");
        std::fs::write(dir.path().join("dashboard-mode"), "local").unwrap();
        assert_eq!(dashboard_mode(dir.path(), true), "local");
        std::fs::write(dir.path().join("dashboard-mode"), "server").unwrap();
        assert_eq!(dashboard_mode(dir.path(), false), "local");
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
                dashboard_mode: Mutex::new("local".to_owned()),
                account_session: Mutex::new(None),
                account_email: Mutex::new(None),
                account_space: Mutex::new(None),
                github_device: Mutex::new(None),
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
