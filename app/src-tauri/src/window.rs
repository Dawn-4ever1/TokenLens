//! The dashboard window exists only while it is open; closing it destroys
//! the WebView so that a closed app keeps only the collector in memory.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewWindowBuilder};

use crate::commands::AppState;

pub const MAIN: &str = "main";

// Programmatic closes (the tray's Quit item and macOS's app menu) must not
// trigger the one-time Windows background notice meant for clicking X.
static SUPPRESS_CLOSE_NOTICE: AtomicBool = AtomicBool::new(false);

pub fn open(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || open_now(&handle));
}

fn open_now(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN)
        .cloned()
    else {
        return;
    };
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Ok(window) = WebviewWindowBuilder::from_config(app, &config).and_then(|b| b.build()) {
        let _ = window.set_focus();
    }
}

pub fn close(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        SUPPRESS_CLOSE_NOTICE.store(true, Ordering::SeqCst);
        let _ = window.close();
    }
}

/// Shows the one-time Windows notice after a user closes the dashboard window.
/// The marker is persisted with the app state so reopening the app does not
/// repeat the message. Notification failures never block closing the window.
pub fn on_close_requested(app: &AppHandle) {
    if SUPPRESS_CLOSE_NOTICE.swap(false, Ordering::SeqCst) {
        return;
    }
    #[cfg(windows)]
    {
        let state = app.state::<AppState>();
        let marker = state.paths.state_dir.join("window-close-notice-shown");
        if marker.exists() {
            return;
        }
        if std::fs::create_dir_all(&state.paths.state_dir)
            .and_then(|()| std::fs::write(&marker, b"1"))
            .is_err()
        {
            return;
        }
        let _ = notify_rust::Notification::new()
            .summary("TokenLens")
            .body("看板窗口已关闭，TokenLens 仍在后台采集。需要查看数据时，从托盘打开。")
            .show();
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// macOS: the Dock icon is shown only while the window exists.
pub fn on_destroyed(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if app.webview_windows().is_empty() {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}
