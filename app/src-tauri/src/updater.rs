//! Public, signed app updates (D046). Only Rust performs network requests or
//! handles installer bytes; the window receives labels and progress numbers.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};
use url::Url;

use crate::commands::{self, AppState};

const STATUS_EVENT: &str = "update-changed";
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const CHECK_ERROR: &str = "暂时连不上更新服务，请稍后再试";
const DOWNLOAD_ERROR: &str = "更新包没能下载完成，请稍后重试";
const GITHUB_ORIGIN: &str = "https://github.com";
const MIRROR_ORIGIN: &str = "https://tokenlens.online";
const GITHUB_MANIFEST: &str =
    "https://github.com/Dawn-4ever1/TokenLens/releases/latest/download/latest.json";
const MIRROR_MANIFEST: &str = "https://tokenlens.online/downloads/latest.json";
const GITHUB_RELEASE_PATH: &str = "/Dawn-4ever1/TokenLens/releases/download/";
const VERIFY_ERROR: &str = "更新包校验失败，没有安装；请稍后重试";
const FALLBACK_ERROR: &str = "更新服务提供的下载地址无效，没有安装";
const INSTALL_ERROR: &str = "更新没能安装，请稍后重试";
const SETTINGS_ERROR: &str = "没能保存自动检查设置，请再试一次";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Checking,
    Available,
    Latest,
    Downloading,
    Installing,
    Error,
}

impl Phase {
    fn busy(self) -> bool {
        matches!(self, Self::Checking | Self::Downloading | Self::Installing)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub auto_check: bool,
    pub phase: Phase,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<&'static str>,
    pub checked_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    auto_check: bool,
}

struct Inner {
    status: UpdateStatus,
    candidate: Option<UpdateCandidate>,
    /// Monotonic time, so a clock adjustment cannot trigger repeated checks.
    last_attempt: Option<Instant>,
}

pub struct UpdateState {
    settings_path: PathBuf,
    inner: Mutex<Inner>,
}

impl UpdateState {
    pub fn load(state_dir: &Path) -> Self {
        let settings_path = state_dir.join("update-settings.json");
        let settings = std::fs::read(&settings_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok());
        Self {
            settings_path,
            inner: Mutex::new(Inner {
                status: UpdateStatus {
                    auto_check: settings.is_none_or(|settings| settings.auto_check),
                    phase: Phase::Idle,
                    version: None,
                    notes: None,
                    downloaded_bytes: 0,
                    total_bytes: None,
                    error: None,
                    checked_at: None,
                },
                candidate: None,
                last_attempt: None,
            }),
        }
    }

    fn status(&self) -> UpdateStatus {
        lock(&self.inner).status.clone()
    }

    fn set_auto_check(&self, enabled: bool) -> UpdateStatus {
        let mut inner = lock(&self.inner);
        // Save only this non-secret preference, never pairing or credentials.
        let bytes = serde_json::to_vec(&Settings {
            auto_check: enabled,
        })
        .expect("boolean settings serialize");
        let save = || -> std::io::Result<()> {
            if let Some(parent) = self.settings_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let pending = self.settings_path.with_extension("json.pending");
            std::fs::write(&pending, &bytes)?;
            std::fs::rename(pending, &self.settings_path)
        };
        if save().is_ok() {
            inner.status.auto_check = enabled;
            if inner.status.error == Some(SETTINGS_ERROR) {
                inner.status.error = None;
                if inner.status.phase == Phase::Error {
                    inner.status.phase = if inner.candidate.is_some() {
                        Phase::Available
                    } else {
                        Phase::Idle
                    };
                }
            }
        } else {
            inner.status.error = Some(SETTINGS_ERROR);
            if !inner.status.phase.busy() {
                inner.status.phase = Phase::Error;
            }
        }
        inner.status.clone()
    }

    fn fail(&self, message: &'static str) -> UpdateStatus {
        let mut inner = lock(&self.inner);
        inner.status.phase = Phase::Error;
        inner.status.error = Some(message);
        inner.status.clone()
    }

    fn start_download_attempt(&self) {
        let mut inner = lock(&self.inner);
        inner.status.downloaded_bytes = 0;
        inner.status.total_bytes = None;
    }

    fn progress(&self, chunk: usize, total: Option<u64>) -> (u64, Option<u64>) {
        let mut inner = lock(&self.inner);
        inner.status.downloaded_bytes = inner.status.downloaded_bytes.saturating_add(chunk as u64);
        inner.status.total_bytes = total;
        (inner.status.downloaded_bytes, inner.status.total_bytes)
    }
}

fn changed(app: &AppHandle) {
    let _ = app.emit(STATUS_EVENT, ());
}

fn check_due(enabled: bool, busy: bool, elapsed: Duration, since_check: Option<Duration>) -> bool {
    enabled
        && !busy
        && elapsed >= FIRST_CHECK
        && since_check.is_none_or(|since| since >= CHECK_INTERVAL)
}

#[derive(Debug)]
enum DownloadError {
    Updater(tauri_plugin_updater::Error),
    InvalidFallback,
}

impl From<tauri_plugin_updater::Error> for DownloadError {
    fn from(error: tauri_plugin_updater::Error) -> Self {
        Self::Updater(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    GitHub,
    Mirror,
}

impl Source {
    fn other(self) -> Self {
        match self {
            Self::GitHub => Self::Mirror,
            Self::Mirror => Self::GitHub,
        }
    }
}

#[derive(Clone)]
struct UpdateCandidate {
    update: Update,
    source: Source,
}

impl UpdateCandidate {
    fn new(mut update: Update) -> Option<Self> {
        let source = download_source(&update.download_url)?;
        let version = match source {
            Source::GitHub => github_parts(&update.download_url)?.0,
            Source::Mirror => mirror_parts(&update.download_url)?.0,
        };
        if version != update.version {
            return None;
        }
        update.timeout = Some(DOWNLOAD_TIMEOUT);
        Some(Self { update, source })
    }
}

fn parse_origin(origin: &str) -> Url {
    Url::parse(origin).expect("compiled update origin is a valid URL")
}

/// Origins are compiled constants; manifests can only supply an official path.
fn allowed_origin(url: &Url, origin: &str) -> bool {
    let origin = parse_origin(origin);
    let raw = url.as_str();
    raw.is_ascii()
        && !raw.contains(['%', '\\'])
        && url.scheme() == "https"
        && url.host_str() == origin.host_str()
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !url
            .path_segments()
            .is_some_and(|mut segments| segments.any(|segment| matches!(segment, "." | "..")))
}

fn stable_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let valid = parts
        .by_ref()
        .take(3)
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    valid && parts.next().is_none() && version.matches('.').count() == 2
}

fn official_asset_filename(version: &str, filename: &str) -> bool {
    // URL already percent-encodes generated paths, so encoded separators or
    // encoded dots must not be used to smuggle a different asset identity.
    stable_version(version)
        && filename.is_ascii()
        && !filename.contains('%')
        && (filename == format!("TokenLens_{version}_x64-setup.exe")
            || filename == format!("TokenLens_{version}_aarch64.app.tar.gz"))
}

fn github_parts(url: &Url) -> Option<(&str, &str)> {
    let rest = url.path().strip_prefix(GITHUB_RELEASE_PATH)?;
    let rest = rest.strip_prefix('v')?;
    let (version, filename) = rest.split_once('/')?;
    official_asset_filename(version, filename).then_some((version, filename))
}

fn mirror_parts(url: &Url) -> Option<(&str, &str)> {
    let rest = url.path().strip_prefix("/downloads/v")?;
    let (version, filename) = rest.split_once('/')?;
    official_asset_filename(version, filename).then_some((version, filename))
}

fn download_source(url: &Url) -> Option<Source> {
    if allowed_origin(url, GITHUB_ORIGIN) && github_parts(url).is_some() {
        return Some(Source::GitHub);
    }
    if allowed_origin(url, MIRROR_ORIGIN) && mirror_parts(url).is_some() {
        return Some(Source::Mirror);
    }
    None
}

/// Parse before checking: an official-looking prefix is not an allowlist.
#[cfg(test)]
fn public_download(url: &str) -> bool {
    // Reject non-ASCII ambiguity in the official ASCII-only paths; inspect
    // parsed components so port and userinfo cannot spoof the host.
    url.is_ascii()
        && !url.contains(['%', '\\'])
        && !url.contains("/./")
        && !url.contains("/../")
        && Url::parse(url)
            .ok()
            .and_then(|url| download_source(&url))
            .is_some()
}

fn manifest_source(url: &Url) -> Option<Source> {
    match url.as_str() {
        GITHUB_MANIFEST => Some(Source::GitHub),
        MIRROR_MANIFEST => Some(Source::Mirror),
        _ => None,
    }
}

fn source_download_url(source: Source, version: &str, filename: &str) -> Option<Url> {
    if !official_asset_filename(version, filename) {
        return None;
    }
    let url = match source {
        Source::GitHub => format!("{GITHUB_ORIGIN}{GITHUB_RELEASE_PATH}v{version}/{filename}"),
        Source::Mirror => format!("{MIRROR_ORIGIN}/downloads/v{version}/{filename}"),
    };
    let url = Url::parse(&url).ok()?;
    (download_source(&url) == Some(source)).then_some(url)
}

/// Change only the selected platform's URL. The signature and signed-version
/// policy still come from the same manifest and the official updater config.
fn rewritten_manifest(raw_json: &Value, target: &str, source: Source) -> Option<Value> {
    let version = raw_json.get("version")?.as_str()?;
    let platforms = raw_json.get("platforms").and_then(Value::as_object);
    let platform = platforms.and_then(|platforms| platforms.get(target));
    let current_url = platform
        .and_then(|platform| platform.get("url"))
        .or_else(|| raw_json.get("url"))?
        .as_str()?;
    let current_url = Url::parse(current_url).ok()?;
    download_source(&current_url)?;
    let filename = current_url.path_segments()?.next_back()?;
    let download_url = source_download_url(source, version, filename)?;

    let mut rewritten = raw_json.clone();
    if platform.is_some() {
        rewritten
            .get_mut("platforms")?
            .get_mut(target)?
            .get_mut("url")?
            .clone_from(&Value::String(download_url.into()));
    } else {
        rewritten
            .get_mut("url")?
            .clone_from(&Value::String(download_url.into()));
    }
    Some(rewritten)
}

fn restore_candidate(
    updater: &Updater,
    candidate: &UpdateCandidate,
) -> Result<UpdateCandidate, DownloadError> {
    let source = candidate.source.other();
    let raw_json = rewritten_manifest(&candidate.update.raw_json, &candidate.update.target, source)
        .ok_or(DownloadError::InvalidFallback)?;
    let update = updater
        .restore_update(raw_json)?
        .filter(|update| update.target == candidate.update.target)
        .ok_or(DownloadError::InvalidFallback)?;
    UpdateCandidate::new(update)
        .filter(|fallback| fallback.source == source)
        .ok_or(DownloadError::InvalidFallback)
}

fn can_retry_download(error: &DownloadError) -> bool {
    use tauri_plugin_updater::Error;
    // Network covers non-success HTTP statuses; Reqwest also covers timeouts.
    matches!(
        error,
        DownloadError::Updater(Error::Network(_) | Error::Reqwest(_))
    )
}

fn download_error(error: &DownloadError) -> &'static str {
    match error {
        DownloadError::InvalidFallback => FALLBACK_ERROR,
        DownloadError::Updater(error) => download_updater_error(error),
    }
}

fn download_updater_error(error: &tauri_plugin_updater::Error) -> &'static str {
    use tauri_plugin_updater::Error;
    match error {
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => VERIFY_ERROR,
        _ => DOWNLOAD_ERROR,
    }
}

/// Run at most two downloads, and do not even prepare the other source for a
/// signature error. Generic operation keeps ordering tests entirely offline.
async fn download_with_one_fallback<T: Clone, E, Fut>(
    primary: T,
    fallback: impl FnOnce() -> Option<T>,
    mut download: impl FnMut(T) -> Fut,
    can_retry: impl Fn(&E) -> bool,
) -> Result<(T, Vec<u8>), E>
where
    Fut: std::future::Future<Output = Result<Vec<u8>, E>>,
{
    let succeeded_with = primary.clone();
    match download(primary).await {
        Ok(bytes) => Ok((succeeded_with, bytes)),
        Err(error) if can_retry(&error) => match fallback() {
            Some(fallback) => {
                let succeeded_with = fallback.clone();
                download(fallback)
                    .await
                    .map(|bytes| (succeeded_with, bytes))
            }
            None => Err(error),
        },
        Err(error) => Err(error),
    }
}

async fn check(app: &AppHandle, automatic: bool) -> UpdateStatus {
    let state = app.state::<UpdateState>();
    {
        let mut inner = lock(&state.inner);
        if inner.status.phase.busy() || (automatic && !inner.status.auto_check) {
            return inner.status.clone();
        }
        inner.status.phase = Phase::Checking;
        inner.status.error = None;
        inner.status.checked_at = Some(chrono::Utc::now().timestamp_millis());
        inner.last_attempt = Some(Instant::now());
    }
    changed(app);
    // No device id, credentials, analytics headers or user input is supplied.
    let updater = app.updater_builder().timeout(CHECK_TIMEOUT).build();
    let result = match updater {
        Ok(_) => {
            let mut checked = None;
            let mut failed = false;
            let mut invalid_manifest = false;
            for endpoint in [GITHUB_MANIFEST, MIRROR_MANIFEST] {
                let Ok(endpoint) = Url::parse(endpoint) else {
                    continue;
                };
                let Some(source) = manifest_source(&endpoint) else {
                    unreachable!("compiled update endpoints are allowlisted");
                };
                let endpoint_updater = app
                    .updater_builder()
                    .timeout(CHECK_TIMEOUT)
                    .endpoints(vec![endpoint])
                    .and_then(|builder| builder.build());
                match endpoint_updater {
                    Ok(endpoint_updater) => match endpoint_updater.check().await {
                        Ok(Some(update))
                            if download_source(&update.download_url) == Some(source) =>
                        {
                            match UpdateCandidate::new(update) {
                                Some(candidate) => {
                                    checked = Some(Some(candidate));
                                    break;
                                }
                                None => invalid_manifest = true,
                            }
                        }
                        Ok(None) => {
                            checked = Some(None);
                            break;
                        }
                        Ok(Some(_)) => {
                            // A successful but invalid manifest is not selected;
                            // the other fixed manifest may still be usable.
                            invalid_manifest = true;
                        }
                        Err(_) => failed = true,
                    },
                    Err(_) => failed = true,
                }
            }
            match checked {
                Some(candidate) => Ok(candidate),
                None if failed || invalid_manifest => Err(()),
                None => Ok(None),
            }
        }
        Err(_) => Err(()),
    };
    let status = {
        let mut inner = lock(&state.inner);
        match result {
            Ok(Some(candidate)) => {
                inner.status.phase = Phase::Available;
                inner.status.version = Some(candidate.update.version.clone());
                inner.status.notes = candidate.update.body.clone();
                inner.status.error = None;
                inner.candidate = Some(candidate);
            }
            Ok(None) => {
                inner.status.phase = Phase::Latest;
                inner.status.version = None;
                inner.status.notes = None;
                inner.status.error = None;
                inner.candidate = None;
            }
            // Never serialize or log plugin errors: they can contain URLs or
            // local install paths. A known candidate survives a failed check.
            _ => {
                inner.status.phase = Phase::Error;
                inner.status.error = Some(CHECK_ERROR);
            }
        }
        inner.status.clone()
    };
    changed(app);
    status
}

pub fn start_schedule(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let started = Instant::now();
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let due = {
                let state = app.state::<UpdateState>();
                let inner = lock(&state.inner);
                check_due(
                    inner.status.auto_check,
                    inner.status.phase.busy(),
                    started.elapsed(),
                    inner.last_attempt.map(|at| at.elapsed()),
                )
            };
            if due {
                check(&app, true).await;
            }
            // Timer only; no background network traffic while disabled. This
            // also notices re-enabling without keeping an extra OS thread.
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

/// Kept small and testable: stopping joins the collector thread and its syncer
/// (which owns the queue lock), before the installer can replace this process.
fn after_collector_stops<T>(stop: impl FnOnce(), install: impl FnOnce() -> T) -> T {
    stop();
    install()
}

async fn download_candidate(
    app: &AppHandle,
    candidate: UpdateCandidate,
) -> Result<(Update, Vec<u8>), DownloadError> {
    let updater = app.updater_builder().timeout(DOWNLOAD_TIMEOUT).build()?;
    let original = candidate.clone();
    download_with_one_fallback(
        candidate,
        move || restore_candidate(&updater, &original).ok(),
        move |attempt| {
            let app = app.clone();
            async move {
                let state = app.state::<UpdateState>();
                state.start_download_attempt();
                changed(&app);
                let progress_app = app.clone();
                let last_emit = Mutex::new(None::<Instant>);
                attempt
                    .update
                    .download(
                        move |chunk, total| {
                            let (downloaded, total) =
                                progress_app.state::<UpdateState>().progress(chunk, total);
                            let complete = total.is_some_and(|expected| downloaded >= expected);
                            let should_emit = {
                                let mut guard = lock(&last_emit);
                                let now = Instant::now();
                                if complete
                                    || guard.is_none_or(|prev| {
                                        now.duration_since(prev) >= Duration::from_millis(100)
                                    })
                                {
                                    *guard = Some(now);
                                    true
                                } else {
                                    false
                                }
                            };
                            if should_emit {
                                changed(&progress_app);
                            }
                        },
                        || {},
                    )
                    .await
                    .map_err(DownloadError::Updater)
            }
        },
        can_retry_download,
    )
    .await
    .map(|(candidate, bytes)| (candidate.update, bytes))
}

async fn download_and_install(app: AppHandle, candidate: UpdateCandidate) {
    let (update, bytes) = match download_candidate(&app, candidate).await {
        Ok(downloaded) => downloaded, // download() has verified the signature already.
        Err(error) => {
            app.state::<UpdateState>().fail(download_error(&error));
            changed(&app);
            return;
        }
    };
    {
        let state = app.state::<UpdateState>();
        let mut inner = lock(&state.inner);
        inner.status.phase = Phase::Installing;
        inner.status.error = None;
    }
    changed(&app);
    let install_app = app.clone();
    let _result = tauri::async_runtime::spawn_blocking(move || {
        let app_state = install_app.state::<AppState>();
        let collector = &app_state.collector;
        let result = after_collector_stops(|| collector.begin_update(), || update.install(bytes));
        // On Windows install() starts NSIS (/UPDATE /P /R), then exits. The
        // passive installer relaunches the app, so do not start a second copy.
        #[cfg(not(windows))]
        if result.is_ok() {
            install_app.restart();
        }
        result
    })
    .await;
    // A successful desktop install exits/restarts above. Any return (including
    // a panicking blocking task) must release the gate under the connection lock.
    app.state::<UpdateState>().fail(INSTALL_ERROR);
    let resume_app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        commands::finish_update_and_resume(&resume_app);
    })
    .await;
    commands::changed(&app, crate::tray::Poke::Status);
    changed(&app);
}

#[tauri::command]
pub fn update_status(app: AppHandle) -> UpdateStatus {
    app.state::<UpdateState>().status()
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> UpdateStatus {
    check(&app, false).await
}

#[tauri::command]
pub async fn set_auto_check_updates(app: AppHandle, enabled: bool) -> UpdateStatus {
    let fallback_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let status = app.state::<UpdateState>().set_auto_check(enabled);
        changed(&app);
        status
    })
    .await
    .unwrap_or_else(|_| fallback_app.state::<UpdateState>().status())
}

#[tauri::command]
pub fn install_update(app: AppHandle) -> UpdateStatus {
    let state = app.state::<UpdateState>();
    let (status, candidate) = {
        let mut inner = lock(&state.inner);
        if inner.status.phase.busy() {
            return inner.status.clone();
        }
        let Some(candidate) = inner.candidate.clone() else {
            drop(inner);
            return state.fail("请先检查有没有新版本");
        };
        inner.status.phase = Phase::Downloading;
        inner.status.downloaded_bytes = 0;
        inner.status.total_bytes = None;
        inner.status.error = None;
        (inner.status.clone(), candidate)
    };
    changed(&app);
    tauri::async_runtime::spawn(download_and_install(app.clone(), candidate));
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;
    use std::cell::RefCell;

    // A throwaway updater test key pair from the official plugin. The signed
    // payload is the four bytes `test`; no production credential is involved.
    const TEST_PUBLIC_KEY: &str = "untrusted comment: minisign public key: A49EF4746FA6E6D7\nRWTX5qZvdPSepDGWg2bz8n6ArQnBMwgQK1geVosorcbLzcunD+KJ+cYe";
    const UPSTREAM_PUBLIC_KEY: &str = "untrusted comment: minisign public key: 8F2C6C3E2D6C0F28\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const UPSTREAM_UNVERSIONED_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from tauri secret key\nRUTX5qZvdPSepPF2zsjvP/L/TQtBLRnWRvaq+4omK1UEugtQp/USrGiI1v762sDy3id3aor7bRC9ETyeot6lMHHrWoZ5hjUM1Qc=\ntrusted comment: timestamp:1791649051\tfile:payload\tversion:2.0.0\nDDq2CLOEl5cQmC2eJRHlnZPVViOZsu1CjpPKQoei4A/XBfkDqGO0B+QMrcrHgEQ5qV3mne75tdFWhJpqUXh9AA==";

    fn encoded(value: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(value)
    }

    /// A mock app whose updater config is complete; `mock_context` otherwise
    /// leaves `plugins.updater` null and plugin initialization fails.
    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        mock_app_with_pubkey(TEST_PUBLIC_KEY)
    }

    fn mock_app_with_pubkey(public_key: &str) -> tauri::App<tauri::test::MockRuntime> {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            json!({
                "pubkey": encoded(public_key),
                "endpoints": [GITHUB_MANIFEST],
            }),
        );
        tauri::test::mock_builder()
            .plugin(
                tauri_plugin_updater::Builder::new()
                    .pubkey(encoded(public_key))
                    .build(),
            )
            .build(context)
            .unwrap()
    }

    fn restore_updater_for(target: &str) -> Updater {
        let app = mock_app();
        app.updater_builder()
            .target(target)
            .executable_path("runtime-path/app")
            .configure_client(|_| panic!("unexpected network request"))
            .endpoints(vec![Url::parse(GITHUB_MANIFEST).unwrap()])
            .unwrap()
            .build()
            .unwrap()
    }

    fn restore_updater() -> Updater {
        restore_updater_for("darwin-aarch64")
    }

    fn restore_fixture(source: Source) -> UpdateCandidate {
        let (target, asset, current_url, other_url) = match source {
            Source::GitHub => (
                "darwin-aarch64",
                "TokenLens_2.0.0_aarch64.app.tar.gz",
                "https://github.com/Dawn-4ever1/TokenLens/releases/download/v2.0.0/TokenLens_2.0.0_aarch64.app.tar.gz",
                "https://tokenlens.online/downloads/v2.0.0/TokenLens_2.0.0_x64-setup.exe",
            ),
            Source::Mirror => (
                "windows-x86_64",
                "TokenLens_2.0.0_x64-setup.exe",
                "https://tokenlens.online/downloads/v2.0.0/TokenLens_2.0.0_x64-setup.exe",
                "https://github.com/Dawn-4ever1/TokenLens/releases/download/v2.0.0/TokenLens_2.0.0_aarch64.app.tar.gz",
            ),
        };
        let mut raw_json = json!({
            "version": "2.0.0",
            "notes": "保留这些更新说明",
            "pub_date": "2025-01-01T00:00:00Z",
            "platforms": {
                "darwin-aarch64": {
                    "url": "https://github.com/Dawn-4ever1/TokenLens/releases/download/v2.0.0/TokenLens_2.0.0_aarch64.app.tar.gz",
                    "signature": encoded(TEST_SIGNATURE)
                },
                "windows-x86_64": {
                    "url": "https://github.com/Dawn-4ever1/TokenLens/releases/download/v2.0.0/TokenLens_2.0.0_x64-setup.exe",
                    "signature": encoded(TEST_SIGNATURE)
                }
            }
        });
        raw_json["platforms"][target]["url"] = json!(current_url);
        raw_json["platforms"][target]["signature"] = json!(encoded(TEST_SIGNATURE));
        let other_target = if target == "darwin-aarch64" {
            "windows-x86_64"
        } else {
            "darwin-aarch64"
        };
        raw_json["platforms"][other_target]["url"] = json!(other_url);
        raw_json["platforms"][other_target]["signature"] =
            json!("other-platform-signature-must-not-change");
        let update = restore_updater_for(target)
            .restore_update(raw_json)
            .unwrap()
            .unwrap();
        assert_eq!(
            asset,
            update
                .download_url
                .path_segments()
                .unwrap()
                .next_back()
                .unwrap()
        );
        UpdateCandidate::new(update).unwrap()
    }

    #[test]
    fn real_restore_rebuilds_only_the_selected_platform_and_keeps_signed_metadata() {
        for source in [Source::GitHub, Source::Mirror] {
            let primary = restore_fixture(source);
            let updater = restore_updater_for(&primary.update.target);
            let fallback = restore_candidate(&updater, &primary).unwrap();
            let (target, other_target, expected) = if source == Source::GitHub {
                (
                    "darwin-aarch64",
                    "windows-x86_64",
                    "https://tokenlens.online/downloads/v2.0.0/TokenLens_2.0.0_aarch64.app.tar.gz",
                )
            } else {
                (
                    "windows-x86_64",
                    "darwin-aarch64",
                    "https://github.com/Dawn-4ever1/TokenLens/releases/download/v2.0.0/TokenLens_2.0.0_x64-setup.exe",
                )
            };
            assert_eq!(fallback.source, source.other());
            assert_eq!(fallback.update.download_url.as_str(), expected);
            assert_eq!(fallback.update.version, "2.0.0");
            assert_eq!(fallback.update.body.as_deref(), Some("保留这些更新说明"));
            assert_eq!(fallback.update.signature, encoded(TEST_SIGNATURE));
            assert_eq!(fallback.update.target, target);
            assert_eq!(fallback.update.timeout, Some(DOWNLOAD_TIMEOUT));
            assert_eq!(
                fallback.update.raw_json["platforms"][other_target],
                primary.update.raw_json["platforms"][other_target]
            );
            let mut expected_raw = primary.update.raw_json.clone();
            expected_raw["platforms"][target]["url"] = json!(expected);
            assert_eq!(fallback.update.raw_json, expected_raw);
            // Official download verification still applies after restoration.
            fallback.update.verify(b"test").unwrap();
            assert!(fallback.update.verify(b"tampered").is_err());
        }
    }

    #[test]
    fn real_restore_always_enforces_signed_version_even_if_runtime_config_did_not() {
        // The mock config starts with the plugin default false; restore_update
        // must turn the signed-version requirement on by itself.
        let app = mock_app_with_pubkey(UPSTREAM_PUBLIC_KEY);
        let updater = app
            .updater_builder()
            .target("darwin-aarch64")
            .executable_path("runtime-path/app")
            .endpoints(vec![Url::parse(GITHUB_MANIFEST).unwrap()])
            .unwrap()
            .build()
            .unwrap();
        let mut unversioned = restore_fixture(Source::GitHub).update.raw_json.clone();
        unversioned["platforms"]["darwin-aarch64"]["signature"] =
            json!(encoded(UPSTREAM_UNVERSIONED_SIGNATURE));
        unversioned["platforms"]["darwin-aarch64"]["url"] =
            json!("https://tokenlens.online/downloads/v2.0.0/TokenLens_2.0.0_aarch64.app.tar.gz");
        let update = updater.restore_update(unversioned).unwrap().unwrap();
        assert!(matches!(
            update.verify(b"test"),
            Err(tauri_plugin_updater::Error::MissingSignedVersion)
        ));
    }

    #[test]
    fn real_restore_rejects_signed_version_tampering_and_invalid_fallback_urls() {
        let updater = restore_updater();
        let mut primary = restore_fixture(Source::GitHub);
        primary.update.raw_json["version"] = json!("3.0.0");
        // There is no valid 3.0.0 mirror asset name in this manifest, so URL
        // construction fails before an Update could be returned.
        assert!(restore_candidate(&updater, &primary).is_err());

        let mut primary = restore_fixture(Source::GitHub);
        primary.update.raw_json["platforms"]["darwin-aarch64"]["url"] =
            json!("https://evil.example/downloads/v2.0.0/TokenLens_2.0.0_aarch64.app.tar.gz");
        assert!(restore_candidate(&updater, &primary).is_err());

        let mut tampered = restore_fixture(Source::GitHub);
        tampered.update.raw_json["version"] = json!("2.0.1");
        tampered.update.raw_json["platforms"]["darwin-aarch64"]["url"] =
            json!("https://tokenlens.online/downloads/v2.0.1/TokenLens_2.0.1_aarch64.app.tar.gz");
        let rebuilt = updater
            .restore_update(tampered.update.raw_json)
            .unwrap()
            .unwrap();
        assert!(matches!(
            rebuilt.verify(b"test"),
            Err(tauri_plugin_updater::Error::SignedVersionMismatch { .. })
        ));
    }

    #[test]
    fn automatic_checks_wait_ten_seconds_then_a_whole_day_and_honor_the_switch() {
        assert!(!check_due(true, false, Duration::from_secs(9), None));
        assert!(check_due(true, false, FIRST_CHECK, None));
        assert!(!check_due(false, false, FIRST_CHECK, None));
        assert!(!check_due(true, true, CHECK_INTERVAL, None));
        assert!(!check_due(
            true,
            false,
            CHECK_INTERVAL,
            Some(CHECK_INTERVAL - Duration::from_secs(1))
        ));
        assert!(check_due(true, false, CHECK_INTERVAL, Some(CHECK_INTERVAL)));
    }

    #[test]
    fn automatic_check_preference_defaults_on_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let state = UpdateState::load(dir.path());
        assert!(state.status().auto_check);
        assert_eq!(state.status().phase, Phase::Idle);
        assert!(!state.set_auto_check(false).auto_check);
        let reopened = UpdateState::load(dir.path());
        assert!(!reopened.status().auto_check);
        assert!(reopened.set_auto_check(true).auto_check);
        assert!(UpdateState::load(dir.path()).status().auto_check);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("update-settings.json")).unwrap(),
            r#"{"autoCheck":true}"#
        );
    }

    #[test]
    fn settings_failure_does_not_claim_the_preference_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, b"fixture").unwrap();
        let state = UpdateState::load(&file);
        let status = state.set_auto_check(false);
        assert!(status.auto_check);
        assert_eq!(status.phase, Phase::Error);
        assert_eq!(status.error, Some(SETTINGS_ERROR));
    }

    #[test]
    fn saving_the_preference_does_not_unlock_an_in_flight_update() {
        let dir = tempfile::tempdir().unwrap();
        let state = UpdateState::load(dir.path());
        {
            let mut inner = lock(&state.inner);
            inner.status.phase = Phase::Downloading;
            inner.status.error = Some(SETTINGS_ERROR);
        }
        let status = state.set_auto_check(false);
        assert!(!status.auto_check);
        assert_eq!(status.phase, Phase::Downloading);
        assert_eq!(status.error, None);
    }

    #[test]
    fn github_and_mirror_downloads_require_the_exact_public_origin_and_path() {
        for url in [
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/TokenLens_0.4.1_x64-setup.exe",
            "https://github.com:443/Dawn-4ever1/TokenLens/releases/download/v0.4.1/TokenLens_0.4.1_aarch64.app.tar.gz",
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online:443/downloads/v0.5.12/TokenLens_0.5.12_aarch64.app.tar.gz",
        ] {
            assert!(public_download(url), "expected official URL: {url}");
        }
        for url in [
            "http://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://github.com:444/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://user@github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://user:pass@github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://github.com/another/repo/releases/download/v0.4.1/app.exe",
            "https://localhost/app.exe",
            "https://evil.example/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://github.com.evil.example/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/../app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/./app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/%2e%2e/app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe%3Fsecret=x",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe?secret=x",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe#fragment",
            "http://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online:444/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe",
            "https://user@tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe?x=1",
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe#x",
            "https://tokenlens.online/downloads/v0.5.12/../evil.exe",
            "https://tokenlens.online/downloads/v0.5.12/%2e%2e/evil.exe",
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe%23evil",
            "https://tokenlens.online/downloads/./v0.5.12/TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online/downloads/v0.5.12/extra/TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online/downloads/v0.5.13/TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe.sig",
            "https://tokenlens.online/downloads/v0.5.12/arbitrary.exe",
            "https://tokenlens.online/downloads/v0.5.12//TokenLens_0.5.12_x64-setup.exe",
            "https://tokenlens.online.evil.example/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe",
        ] {
            assert!(!public_download(url), "expected rejected URL: {url}");
        }
    }

    #[test]
    fn only_the_two_fixed_https_manifest_endpoints_select_a_source() {
        assert_eq!(
            manifest_source(&Url::parse(GITHUB_MANIFEST).unwrap()),
            Some(Source::GitHub)
        );
        assert_eq!(
            manifest_source(&Url::parse(MIRROR_MANIFEST).unwrap()),
            Some(Source::Mirror)
        );
        assert_eq!(
            manifest_source(&Url::parse("https://evil.example/latest.json").unwrap()),
            None
        );
    }

    #[tokio::test]
    async fn download_fallback_uses_each_source_order_and_stops_after_two() {
        for primary in [Source::GitHub, Source::Mirror] {
            let attempts = RefCell::new(Vec::new());
            let result = download_with_one_fallback(
                primary,
                || Some(primary.other()),
                |source| {
                    attempts.borrow_mut().push(source);
                    async { Err::<Vec<u8>, _>("network") }
                },
                |error| *error == "network",
            )
            .await;
            assert!(result.is_err());
            assert_eq!(*attempts.borrow(), [primary, primary.other()]);
        }
    }

    #[tokio::test]
    async fn verification_failures_never_fallback_and_primary_success_stops() {
        let attempts = RefCell::new(Vec::new());
        let result = download_with_one_fallback(
            Source::GitHub,
            || panic!("signature errors must not prepare a mirror"),
            |source| {
                attempts.borrow_mut().push(source);
                async { Err::<Vec<u8>, _>("signature") }
            },
            |error| *error == "network",
        )
        .await;
        assert!(result.is_err());
        assert_eq!(*attempts.borrow(), [Source::GitHub]);

        let result = download_with_one_fallback(
            Source::Mirror,
            || panic!("a successful primary must not prepare a fallback"),
            |_| async { Ok(vec![1, 2, 3]) },
            |_: &tauri_plugin_updater::Error| true,
        )
        .await;
        assert_eq!(result.unwrap(), (Source::Mirror, vec![1, 2, 3]));
    }

    #[test]
    fn fallback_mirror_urls_use_the_release_version_and_official_asset_name() {
        assert_eq!(
            source_download_url(Source::Mirror, "0.5.12", "TokenLens_0.5.12_x64-setup.exe")
                .unwrap()
                .as_str(),
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_x64-setup.exe"
        );
        assert_eq!(
            source_download_url(
                Source::Mirror,
                "0.5.12",
                "TokenLens_0.5.12_aarch64.app.tar.gz"
            )
            .unwrap()
            .as_str(),
            "https://tokenlens.online/downloads/v0.5.12/TokenLens_0.5.12_aarch64.app.tar.gz"
        );
        for (version, filename) in [
            ("0.5.12", "arbitrary.exe"),
            ("0.5.13", "TokenLens_0.5.12_x64-setup.exe"),
            ("0.5.12-beta", "TokenLens_0.5.12-beta_x64-setup.exe"),
            ("0.5", "TokenLens_0.5_x64-setup.exe"),
        ] {
            assert!(source_download_url(Source::Mirror, version, filename).is_none());
        }
    }

    #[test]
    fn only_network_errors_are_download_fallback_reasons() {
        use tauri_plugin_updater::Error;
        let network = DownloadError::Updater(Error::Network("HTTP status detail".into()));
        assert!(can_retry_download(&network));
        let io = DownloadError::Updater(Error::Io(std::io::Error::other("local detail")));
        assert!(!can_retry_download(&io));
        let utf8 = DownloadError::Updater(Error::SignatureUtf8("signature detail".into()));
        assert!(!can_retry_download(&utf8));
        assert_eq!(download_error(&utf8), VERIFY_ERROR);
        let missing = DownloadError::Updater(Error::MissingSignedVersion);
        assert!(!can_retry_download(&missing));
        let mismatch = DownloadError::Updater(Error::SignedVersionMismatch {
            signed: "2.0.0".into(),
            announced: "2.0.1".into(),
        });
        assert!(!can_retry_download(&mismatch));
        assert_eq!(download_error(&mismatch), VERIFY_ERROR);
        assert!(!can_retry_download(&DownloadError::InvalidFallback));
        assert_eq!(
            download_error(&DownloadError::InvalidFallback),
            FALLBACK_ERROR
        );
    }

    #[test]
    fn production_manifest_order_and_signed_version_setting_are_preserved() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let updater = &config["plugins"]["updater"];
        assert_eq!(updater["requireSignedVersion"], true);
        assert_eq!(
            updater["endpoints"],
            json!([GITHUB_MANIFEST, MIRROR_MANIFEST])
        );
        assert_eq!(DOWNLOAD_TIMEOUT, Duration::from_secs(10 * 60));
        assert!(!CHECK_ERROR.contains("GitHub"));
    }

    #[test]
    fn progress_resets_before_another_source_and_never_adds_the_two_byte_counts() {
        let dir = tempfile::tempdir().unwrap();
        let state = UpdateState::load(dir.path());
        state.start_download_attempt();
        state.progress(12, None);
        state.progress(8, Some(40));
        assert_eq!(state.status().downloaded_bytes, 20);
        assert_eq!(state.status().total_bytes, Some(40));
        state.start_download_attempt();
        assert_eq!(state.status().downloaded_bytes, 0);
        assert_eq!(state.status().total_bytes, None);
        state.progress(3, Some(60));
        assert_eq!(state.status().downloaded_bytes, 3);
        assert_eq!(state.status().total_bytes, Some(60));
    }

    #[test]
    fn progress_supports_unknown_lengths_and_errors_stay_plain_language() {
        let dir = tempfile::tempdir().unwrap();
        let state = UpdateState::load(dir.path());
        state.progress(12, None);
        assert_eq!(state.status().downloaded_bytes, 12);
        assert_eq!(state.status().total_bytes, None);
        state.progress(8, Some(40));
        assert_eq!(state.status().downloaded_bytes, 20);
        assert_eq!(state.status().total_bytes, Some(40));
        assert_eq!(
            download_updater_error(&tauri_plugin_updater::Error::MissingSignedVersion),
            VERIFY_ERROR
        );
        assert_eq!(
            download_updater_error(&tauri_plugin_updater::Error::Network(
                "private detail".into()
            )),
            DOWNLOAD_ERROR
        );
        assert_eq!(state.fail(VERIFY_ERROR).error, Some(VERIFY_ERROR));
    }

    #[test]
    fn collector_stops_before_the_installer_even_when_installation_fails() {
        let calls = RefCell::new(Vec::new());
        let result = after_collector_stops(
            || calls.borrow_mut().push("stop-and-release-lock"),
            || {
                calls.borrow_mut().push("install");
                Err::<(), _>("fixture failure")
            },
        );
        assert!(result.is_err());
        assert_eq!(*calls.borrow(), ["stop-and-release-lock", "install"]);
    }

    #[test]
    fn status_has_only_labels_progress_and_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let status = UpdateState::load(dir.path()).status();
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["autoCheck"], true);
        assert_eq!(value["phase"], "idle");
        assert!(value.get("candidate").is_none());
        assert!(value.get("signature").is_none());
        assert!(value.get("downloadUrl").is_none());
    }

    #[test]
    fn download_source_and_candidate_enforce_strict_github_asset_paths_and_version_match() {
        for bad in [
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/TokenLens_0.4.1_x64-setup.exe%23extra",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.2/TokenLens_0.4.1_x64-setup.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/TokenLens_0.4.1_x64-setup.exe.sig",
        ] {
            let parsed = Url::parse(bad).unwrap();
            assert_eq!(download_source(&parsed), None, "{bad}");
        }

        let mut mismatch = restore_fixture(Source::GitHub).update.raw_json.clone();
        mismatch["platforms"]["darwin-aarch64"]["url"] = json!(
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v2.0.1/TokenLens_2.0.1_aarch64.app.tar.gz"
        );
        let update = restore_updater().restore_update(mismatch).unwrap().unwrap();
        assert!(UpdateCandidate::new(update).is_none());
    }
}
