#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod collector;
mod commands;
mod config;
mod format;
mod local;
mod menu;
mod remote;
mod tray;
mod updater;
mod window;

use std::ffi::OsString;
use std::time::Duration;

use collector_core::worker_cli;
use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_window_state::StateFlags;

use crate::commands::AppState;

/// Passed by the login item: start collecting without opening the window.
const BACKGROUND_ARG: &str = "--background";

fn main() {
    // Upstream's recovery overlay merges a ledger from its shared config
    // directory; this project only counts raw agent logs.
    // SAFETY: set before any other thread exists; scan workers inherit it.
    unsafe { std::env::set_var("TOKSCALE_RECOVERY_DISABLE", "1") };
    let args: Vec<OsString> = std::env::args_os().collect();
    // This binary is also the watch loop's short-lived scan worker.
    if worker_cli::requested(&args) {
        if let Err(error) = worker_cli::serve(args.into_iter().skip(2)) {
            eprintln!("scan-worker: {error}");
            std::process::exit(1);
        }
        return;
    }
    let background = args.iter().any(|arg| arg == BACKGROUND_ARG);
    // Local mode (D044) prices events with tokscale, which reads its catalogs
    // from `TOKSCALE_CONFIG_DIR`; scan workers always get their own value.
    if let Some(dir) = local::prices_dir() {
        // SAFETY: no other thread exists yet; the runtime starts below.
        unsafe { tokenlens_server::pricing::configure_dir(&dir) };
    }

    // Tauri's default runtime starts one worker per core; the app only needs
    // a couple for IPC while the window is open.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .thread_keep_alive(Duration::from_secs(10))
        .enable_all()
        .build()
        .expect("the async runtime could not start");
    tauri::async_runtime::set(runtime.handle().clone());

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::open(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![BACKGROUND_ARG]),
        ))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::all() - StateFlags::VISIBLE)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::load(config::Paths::resolve()))
        .manage(updater::UpdateState::load(
            &config::Paths::resolve().state_dir,
        ))
        .on_menu_event(|app, event| tray::on_menu_event(app, &event))
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) && window.label() == window::MAIN
            {
                window::on_close_requested(window.app_handle());
            }
            if matches!(event, WindowEvent::Destroyed) && window.label() == window::MAIN {
                window::on_destroyed(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_status,
            commands::dashboard_get,
            commands::pair_device,
            commands::use_local_mode,
            commands::unpair_device,
            commands::set_paused,
            commands::sync_now,
            commands::set_autostart,
            commands::create_pair_code,
            commands::rename_device,
            commands::rotate_credentials,
            commands::revoke_device,
            updater::update_status,
            updater::check_update,
            updater::install_update,
            updater::set_auto_check_updates,
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                let menu = menu::build(app.handle())?;
                app.set_menu(menu)?;
            }
            let handle = app.handle().clone();
            tray::install(&handle)?;
            commands::start_collector(&handle);
            updater::start_schedule(&handle);
            if !background {
                window::open(&handle);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not start");

    app.run(|app, event| match event {
        // Closing the last window keeps the collector running; only the
        // tray's quit item exits (with an explicit code).
        RunEvent::ExitRequested {
            code: None, api, ..
        } => api.prevent_exit(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => window::open(app),
        RunEvent::Exit => app.state::<AppState>().collector.stop(),
        _ => {}
    });
}
