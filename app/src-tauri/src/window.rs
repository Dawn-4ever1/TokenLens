//! The dashboard window exists only while it is open; closing it destroys
//! the WebView so that a closed app keeps only the collector in memory.

use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use std::sync::{Mutex, PoisonError};

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

    // `CreateIcon` returns a handle owned by the caller. Keep one non-zero
    // process-lifetime handle and reuse it for every dashboard window; the OS
    // reclaims the process resource when the app exits.
    static TRANSPARENT_ICON: Mutex<Option<isize>> = Mutex::new(None);
    let icon = {
        let mut cached = TRANSPARENT_ICON
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cached.is_none() {
            const MASK: [u8; 32] = [0xff; 32];
            const XOR: [u8; 32] = [0; 32];
            // SAFETY: the bitmap buffers are valid for the duration of the call;
            // non-zero HICONs are retained for the process lifetime above.
            let created = unsafe {
                CreateIcon(
                    std::ptr::null_mut(),
                    16,
                    16,
                    1,
                    1,
                    MASK.as_ptr(),
                    XOR.as_ptr(),
                ) as isize
            };
            if created != 0 {
                *cached = Some(created);
            }
        }
        *cached
    };
    let Some(icon) = icon else {
        return;
    };

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
#[cfg(windows)]
static CLOSE_NOTICE_PENDING: AtomicBool = AtomicBool::new(false);

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
    match WebviewWindowBuilder::from_config(app, &config).and_then(|b| b.build()) {
        Ok(window) => {
            #[cfg(windows)]
            remove_windows_titlebar_icon(&window);
            let _ = window.set_focus();
        }
        Err(_) =>
        {
            #[cfg(target_os = "macos")]
            if app.webview_windows().is_empty() {
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
        }
    }
}

pub fn close(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        SUPPRESS_CLOSE_NOTICE.store(true, Ordering::SeqCst);
        if window.close().is_err() {
            SUPPRESS_CLOSE_NOTICE.store(false, Ordering::SeqCst);
        }
    }
}

/// Shows the one-time Windows notice after a user closes the dashboard window.
/// The marker is persisted only after delivery succeeds so a failed toast can
/// retry later. Runs off the UI thread so toast COM calls never block close.
pub fn on_close_requested(app: &AppHandle) {
    if SUPPRESS_CLOSE_NOTICE.swap(false, Ordering::SeqCst) {
        return;
    }
    #[cfg(windows)]
    {
        if CLOSE_NOTICE_PENDING.swap(true, Ordering::SeqCst) {
            return;
        }
        let state_dir = app.state::<AppState>().paths.state_dir.clone();
        let spawned = std::thread::Builder::new()
            .name("close-notice".to_owned())
            .spawn(move || {
                struct ResetGuard;
                impl Drop for ResetGuard {
                    fn drop(&mut self) {
                        CLOSE_NOTICE_PENDING.store(false, Ordering::SeqCst);
                    }
                }
                let _guard = ResetGuard;
                let marker = state_dir.join("window-close-notice-shown");
                if marker.exists() {
                    return;
                }
                if notify_rust::Notification::new()
                    .app_id("dev.tokenlens.desktop")
                    .summary("TokenLens")
                    .body("看板窗口已关闭，TokenLens 仍在后台采集。需要查看数据时，从托盘打开。")
                    .show()
                    .is_ok()
                    && std::fs::create_dir_all(&state_dir).is_ok()
                {
                    let _ = std::fs::write(&marker, b"1");
                }
            });
        if spawned.is_err() {
            CLOSE_NOTICE_PENDING.store(false, Ordering::SeqCst);
        }
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
