//! The dashboard window exists only while it is open; closing it destroys
//! the WebView so that a closed app keeps only the collector in memory.

use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use std::sync::OnceLock;

use tauri::{AppHandle, Manager, WebviewWindowBuilder};

#[cfg(windows)]
use crate::commands::AppState;

pub const MAIN: &str = "main";

/// `hiddenTitle` is macOS-only in Tauri. Windows would otherwise repeat both
/// the app icon and TokenLens above the sidebar's existing brand. Keep the
/// product name, taskbar icon and macOS appearance untouched.
#[cfg(windows)]
fn windows_config_without_native_title(
    mut config: tauri::utils::config::WindowConfig,
) -> tauri::utils::config::WindowConfig {
    config.title.clear();
    config
}

/// Tauri sets only the small icon when creating a window, and has no API to
/// clear it afterwards. A null `WM_SETICON` is not sufficient on Windows: it
/// falls back to the system's default icon when the window has no class icon.
/// Give the small-icon slot a process-lifetime transparent icon instead. This
/// keeps the title bar blank without changing the frame style (or its system
/// menu hit area), while the big taskbar icon and tray icon remain untouched.
#[cfg(windows)]
fn remove_windows_titlebar_icon(window: &tauri::WebviewWindow) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIcon, ICON_SMALL, SendMessageW, WM_SETICON,
    };

    // `CreateIcon` returns a handle owned by the caller. Keep this one tiny
    // process-lifetime handle alive and reuse it for every dashboard window;
    // the OS reclaims the process resource when the app exits. A 1-bit AND
    // mask full of ones makes every pixel transparent.
    static TRANSPARENT_ICON: OnceLock<isize> = OnceLock::new();
    let icon = *TRANSPARENT_ICON.get_or_init(|| {
        const MASK: [u8; 32] = [0xff; 32];
        const XOR: [u8; 32] = [0; 32];
        // SAFETY: the bitmap buffers are valid for the duration of the call;
        // the returned handle is retained for the process lifetime above.
        unsafe {
            CreateIcon(
                std::ptr::null_mut(),
                16,
                16,
                1,
                1,
                MASK.as_ptr(),
                XOR.as_ptr(),
            ) as isize
        }
    });

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: this runs on the window's UI thread while its HWND is alive.
    // Only ICON_SMALL is changed; ICON_BIG (taskbar) is left intact.
    unsafe {
        SendMessageW(
            handle.hwnd.get() as _,
            WM_SETICON,
            ICON_SMALL as usize,
            icon,
        );
    }
}

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
    #[cfg(windows)]
    let config = windows_config_without_native_title(config);
    if let Ok(window) = WebviewWindowBuilder::from_config(app, &config).and_then(|b| b.build()) {
        #[cfg(windows)]
        remove_windows_titlebar_icon(&window);
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

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_windows_native_title_is_removed() {
        let raw = include_str!("../tauri.conf.json");
        let config: serde_json::Value = serde_json::from_str(raw).expect("window config");
        assert_eq!(config["productName"], "TokenLens");
        assert_eq!(config["app"]["windows"][0]["title"], "TokenLens");
        #[cfg(windows)]
        {
            let windows: tauri::utils::config::WindowConfig =
                serde_json::from_value(config["app"]["windows"][0].clone())
                    .expect("Tauri window config");
            let adjusted = super::windows_config_without_native_title(windows);
            assert_eq!(adjusted.title, "");
            assert_eq!(adjusted.label, super::MAIN);
        }
        #[cfg(not(windows))]
        assert_eq!(config["app"]["windows"][0]["titleBarStyle"], "Overlay");
    }
}
