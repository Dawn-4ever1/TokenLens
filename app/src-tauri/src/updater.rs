//! Public, signed app updates (D046). Only Rust performs network requests or
//! handles installer bytes; the window receives labels and progress numbers.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::commands::{self, AppState};

const STATUS_EVENT: &str = "update-changed";
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const CHECK_ERROR: &str = "连不上 GitHub，稍后会再试";
const DOWNLOAD_ERROR: &str = "更新包没能下载完成，请稍后重试";
const VERIFY_ERROR: &str = "更新包校验失败，没有安装；请稍后重试";
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
    candidate: Option<Update>,
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

    fn progress(&self, chunk: usize, total: Option<u64>) {
        let mut inner = lock(&self.inner);
        inner.status.downloaded_bytes = inner.status.downloaded_bytes.saturating_add(chunk as u64);
        inner.status.total_bytes = total;
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

/// The fixed manifest contains only public Release URLs. Reject a rewritten
/// manifest pointing downloads at a local host or an unrelated service.
fn public_download(url: &str) -> bool {
    url.strip_prefix("https://github.com/Dawn-4ever1/TokenLens/releases/download/")
        .is_some_and(|tail| {
            !tail.is_empty()
                && tail.contains('/')
                && !tail.contains(['?', '#', '\\'])
                && !tail.split('/').any(|part| matches!(part, "" | "." | ".."))
        })
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
    let result = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    let status = {
        let mut inner = lock(&state.inner);
        match result {
            Ok(Some(mut update)) if public_download(update.download_url.as_str()) => {
                update.timeout = Some(DOWNLOAD_TIMEOUT);
                inner.status.phase = Phase::Available;
                inner.status.version = Some(update.version.clone());
                inner.status.notes = update.body.clone();
                inner.status.error = None;
                inner.candidate = Some(update);
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

fn download_error(error: &tauri_plugin_updater::Error) -> &'static str {
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

/// Kept small and testable: stopping joins the collector thread and its syncer
/// (which owns the queue lock), before the installer can replace this process.
fn after_collector_stops<T>(stop: impl FnOnce(), install: impl FnOnce() -> T) -> T {
    stop();
    install()
}

async fn download_and_install(app: AppHandle, update: Update) {
    let progress_app = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                progress_app.state::<UpdateState>().progress(chunk, total);
                changed(&progress_app);
            },
            || {},
        )
        .await;
    let bytes = match bytes {
        Ok(bytes) => bytes, // download() has verified the signature already.
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
    // a panicking blocking task) must release the gate using its saved state.
    app.state::<UpdateState>().fail(INSTALL_ERROR);
    let resume = app.state::<AppState>().collector.finish_update();
    if resume {
        commands::start_collector(&app);
    }
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
pub fn set_auto_check_updates(app: AppHandle, enabled: bool) -> UpdateStatus {
    let status = app.state::<UpdateState>().set_auto_check(enabled);
    changed(&app);
    status
}

#[tauri::command]
pub fn install_update(app: AppHandle) -> UpdateStatus {
    let state = app.state::<UpdateState>();
    let (status, update) = {
        let mut inner = lock(&state.inner);
        if inner.status.phase.busy() {
            return inner.status.clone();
        }
        let Some(update) = inner.candidate.clone() else {
            drop(inner);
            return state.fail("请先检查有没有新版本");
        };
        inner.status.phase = Phase::Downloading;
        inner.status.downloaded_bytes = 0;
        inner.status.total_bytes = None;
        inner.status.error = None;
        (inner.status.clone(), update)
    };
    changed(&app);
    tauri::async_runtime::spawn(download_and_install(app.clone(), update));
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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
    fn only_this_public_repositories_https_release_downloads_are_accepted() {
        assert!(public_download(
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/TokenLens_0.4.1_x64-setup.exe"
        ));
        for url in [
            "http://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe",
            "https://github.com/another/repo/releases/download/v0.4.1/app.exe",
            "https://localhost/app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/../app.exe",
            "https://github.com/Dawn-4ever1/TokenLens/releases/download/v0.4.1/app.exe?secret=x",
        ] {
            assert!(!public_download(url));
        }
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
            download_error(&tauri_plugin_updater::Error::MissingSignedVersion),
            VERIFY_ERROR
        );
        assert_eq!(
            download_error(&tauri_plugin_updater::Error::Network(
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
}
