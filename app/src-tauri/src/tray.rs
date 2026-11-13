//! Menu bar (macOS) / tray (Windows) icon with the simple menu from D020:
//! one "today" line, the sync state, and a few actions. No live numbers on
//! the icon itself.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::collector::now_ms;
use crate::commands::{self, AppState, AppStatus};
use crate::format;
use crate::remote;
use crate::window;

const TODAY: &str = "tray-today";
const STATUS: &str = "tray-status";
const OPEN: &str = "tray-open";
const SYNC: &str = "tray-sync";
const PAUSE: &str = "tray-pause";
const AUTOSTART: &str = "tray-autostart";
const QUIT: &str = "tray-quit";

const LABEL_EVERY: Duration = Duration::from_secs(60);
const TODAY_EVERY: Duration = Duration::from_secs(600);
/// Uploads can arrive every 30 s; the server is asked at most this often.
const TODAY_MIN_GAP: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poke {
    Status,
    Uploaded,
    /// Credentials changed: today's number belongs to another account.
    Paired,
}

struct Items {
    today: MenuItem<Wry>,
    status: MenuItem<Wry>,
    pause: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

pub struct Tray<R: tauri::Runtime> {
    items: Items,
    sender: Mutex<Sender<Poke>>,
    marker: std::marker::PhantomData<fn() -> R>,
}

pub fn poke<R: tauri::Runtime>(app: &AppHandle<R>, poke: Poke) {
    if let Some(tray) = app.try_state::<Tray<R>>() {
        let _ = tray
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .send(poke);
    }
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let none = None::<&str>;
    let items = Items {
        today: MenuItem::with_id(app, TODAY, "今日 —", false, none)?,
        status: MenuItem::with_id(app, STATUS, "读取中", false, none)?,
        pause: MenuItem::with_id(app, PAUSE, "暂停采集", true, none)?,
        autostart: CheckMenuItem::with_id(
            app,
            AUTOSTART,
            "开机启动",
            !cfg!(debug_assertions),
            false,
            none,
        )?,
    };
    let open = MenuItem::with_id(app, OPEN, "打开看板", true, none)?;
    let sync = MenuItem::with_id(app, SYNC, "立即同步", true, none)?;
    let quit = MenuItem::with_id(app, QUIT, "退出 TokenLens", true, none)?;
    let menu = Menu::with_items(
        app,
        &[
            &items.today,
            &items.status,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &sync,
            &items.pause,
            &items.autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("TokenLens");
    #[cfg(target_os = "macos")]
    let builder = builder
        .icon(template_icon())
        .icon_as_template(true)
        .show_menu_on_left_click(true);
    #[cfg(not(target_os = "macos"))]
    let builder = {
        use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
        let builder = match app.default_window_icon() {
            Some(icon) => builder.icon(icon.clone()),
            None => builder,
        };
        builder
            .show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    window::open(tray.app_handle());
                }
            })
    };
    builder.build(app)?;
    let (sender, receiver) = mpsc::channel();
    app.manage(Tray::<Wry> {
        items,
        sender: Mutex::new(sender),
        marker: std::marker::PhantomData,
    });
    let handle = app.clone();
    std::thread::Builder::new()
        .name("tray".to_owned())
        .spawn(move || refresh_loop(&handle, &receiver))?;
    Ok(())
}

/// Every menu (tray and app menu) reports here.
pub fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    let id = event.id().as_ref();
    match id {
        OPEN => window::open(app),
        crate::menu::CLOSE_WINDOW => window::close(app),
        SYNC | PAUSE | AUTOSTART => {
            let app = app.clone();
            let id = id.to_owned();
            tauri::async_runtime::spawn_blocking(move || {
                let current = commands::status(&app);
                let _ = match id.as_str() {
                    SYNC => commands::restart_collector(&app),
                    PAUSE => commands::apply_paused(&app, !current.collector.paused),
                    _ => commands::apply_autostart(&app, current.autostart != Some(true)),
                };
                // A refused toggle must not leave the check mark flipped.
                poke(&app, Poke::Status);
            });
        }
        QUIT => {
            let app = app.clone();
            let _ = std::thread::Builder::new()
                .name("quit".to_owned())
                .spawn(move || {
                    window::close(&app);
                    app.state::<AppState>().collector.stop();
                    app.exit(0);
                });
        }
        _ => {}
    }
}

/// Mirrors `collectorState` in `app/src/components/Shell.tsx`.
pub fn state_label(status: &AppStatus) -> &'static str {
    let c = &status.collector;
    if !status.paired {
        return "未配对";
    }
    if c.paused {
        return "采集已暂停";
    }
    if c.last_error.is_some() {
        return "采集出错";
    }
    if !c.running {
        return "采集未运行";
    }
    let upload = c.upload_state.as_deref().unwrap_or("ok");
    if upload == "queue_error" {
        "服务器同步出错，本机继续记录"
    } else if upload == "pairing_required" {
        "凭据失效，需重新配对"
    } else if upload == "upgrade_required" {
        "需要升级应用"
    } else if upload.starts_with("blocked:") {
        "上传被服务器拒绝"
    } else if upload.starts_with("backoff:") {
        "等待重试上传"
    } else {
        "后台采集中"
    }
}

fn status_line(status: &AppStatus, now: i64) -> String {
    let label = state_label(status);
    if !status.paired {
        return label.to_owned();
    }
    let last = status
        .collector
        .last_upload_at
        .or(status.collector.last_heartbeat_at);
    format!("{label} · 最后同步 {}", format::relative(now, last))
}

fn today_line(total: Option<f64>) -> String {
    match total {
        Some(total) => format!("今日 {} tokens", format::compact(total)),
        None => "今日 —".to_owned(),
    }
}

fn fetch_today(app: &AppHandle) -> Option<f64> {
    let state = app.state::<AppState>();
    let credentials = state.credentials()?;
    // The server buckets by its own time zone; for the people using this
    // project it is the same as this computer's.
    let day = chrono::Local::now().date_naive().to_string();
    let query = [("from", &day), ("to", &day)]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect();
    let path = remote::dashboard_path("summary", &query).ok()?;
    let reply = state.remote.get(&credentials, &path).ok()?;
    reply.get("totalTokens").and_then(Value::as_f64)
}

fn refresh_loop(app: &AppHandle, receiver: &Receiver<Poke>) {
    let tray = app.state::<Tray<Wry>>();
    let mut today: Option<f64> = None;
    let mut fetched_at: Option<Instant> = None;
    let mut fetch_at = Instant::now();
    loop {
        let status = commands::status(app);
        let now = Instant::now();
        if now >= fetch_at {
            today = if status.paired {
                fetch_today(app).or(today)
            } else {
                None
            };
            fetched_at = Some(now);
            fetch_at = now + TODAY_EVERY;
        }
        let items = &tray.items;
        let _ = items.today.set_text(today_line(today));
        let _ = items.status.set_text(status_line(&status, now_ms()));
        let _ = items.pause.set_text(if status.collector.paused {
            "恢复采集"
        } else {
            "暂停采集"
        });
        let _ = items.pause.set_enabled(status.paired);
        let _ = items.autostart.set_checked(status.autostart == Some(true));
        let wait = LABEL_EVERY.min(fetch_at.saturating_duration_since(Instant::now()));
        let first = match receiver.recv_timeout(wait) {
            Ok(poke) => poke,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Coalesce bursts (one scan sends several lines).
        for poke in std::iter::once(first).chain(receiver.try_iter()) {
            match poke {
                Poke::Uploaded => {
                    let earliest = fetched_at.map_or(now, |at| at + TODAY_MIN_GAP);
                    fetch_at = fetch_at.min(earliest.max(Instant::now()));
                }
                Poke::Paired => {
                    today = None;
                    fetched_at = None;
                    // Asking before the first upload lands would show 0.
                    fetch_at = Instant::now() + TODAY_MIN_GAP;
                }
                Poke::Status => {}
            }
        }
    }
}

/// 18 pt @2x monochrome version of the app icon (a lens that is also a
/// three-part breakdown ring, see `icons/source.svg`); macOS tints template
/// images for the menu bar.
#[cfg(any(target_os = "macos", test))]
fn template_icon() -> tauri::image::Image<'static> {
    const SIZE: u32 = 36;
    const CENTER: f32 = 18.0;
    const RING_IN: f32 = 10.5;
    const RING_OUT: f32 = 15.5;
    const DOT: f32 = 4.5;
    const HALF_GAP: f32 = 1.1;
    // Segment boundaries in turns, clockwise from 12 o'clock (same split as the app icon).
    const CUTS: [f32; 3] = [0.0, 0.74, 0.88];
    let inside = |x: f32, y: f32| {
        let (dx, dy) = (x - CENTER, y - CENTER);
        let distance = dx.hypot(dy);
        if distance <= DOT {
            return true;
        }
        if !(RING_IN..=RING_OUT).contains(&distance) {
            return false;
        }
        // Parallel-edged cuts along each boundary ray.
        !CUTS.iter().any(|&turn| {
            let (sin, cos) = (turn * std::f32::consts::TAU).sin_cos();
            let along = dx * sin - dy * cos;
            let across = (dx * cos + dy * sin).abs();
            along > 0.0 && across < HALF_GAP
        })
    };
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for py in 0..SIZE {
        for px in 0..SIZE {
            let mut hits = 0_u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = px as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = py as f32 + (sy as f32 + 0.5) / 4.0;
                    hits += u32::from(inside(x, y));
                }
            }
            let alpha = (hits * 255 / 16) as u8;
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    tauri::image::Image::new_owned(rgba, SIZE, SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CollectorStatus;

    fn sample() -> AppStatus {
        AppStatus {
            paired: true,
            mode: Some("server"),
            device_id: Some("dev_x".into()),
            server: Some("http://127.0.0.1:8787".into()),
            app_version: "0.1.0".into(),
            collector: CollectorStatus {
                running: true,
                paused: false,
                last_scan_at: None,
                last_upload_at: Some(1_000),
                last_heartbeat_at: Some(5_000),
                backlog: Some(0),
                upload_state: Some("ok".into()),
                backfill_complete: Some(true),
                last_error: None,
            },
            autostart: None,
            platform: "macos",
            cloud_server: crate::account::CLOUD_URL,
            account_email: None,
            account_space: None,
        }
    }

    #[test]
    fn labels_follow_the_window_rules() {
        let mut status = sample();
        assert_eq!(state_label(&status), "后台采集中");
        assert_eq!(
            status_line(&status, 1_000 + 5 * 60_000),
            "后台采集中 · 最后同步 5 分钟前"
        );
        status.collector.upload_state = Some("backoff:network".into());
        assert_eq!(state_label(&status), "等待重试上传");
        status.collector.upload_state = Some("pairing_required".into());
        assert_eq!(state_label(&status), "凭据失效，需重新配对");
        status.collector.last_error = Some("x".into());
        assert_eq!(state_label(&status), "采集出错");
        status.collector.paused = true;
        assert_eq!(state_label(&status), "采集已暂停");
        status.paired = false;
        assert_eq!(status_line(&status, 0), "未配对");
    }

    #[test]
    fn today_uses_compact_numbers() {
        assert_eq!(today_line(None), "今日 —");
        assert_eq!(today_line(Some(1_234_567.0)), "今日 1.23M tokens");
    }

    #[test]
    fn the_template_icon_is_a_monochrome_lens_ring() {
        let icon = template_icon();
        assert_eq!((icon.width(), icon.height()), (36, 36));
        let rgba = icon.rgba();
        let alpha = |x: usize, y: usize| rgba[(y * 36 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corner is empty");
        assert_eq!(alpha(17, 17), 255, "centre dot");
        assert_eq!(alpha(25, 18), 0, "space between dot and ring");
        assert_eq!(alpha(31, 18), 255, "ring on the right");
        assert_eq!(alpha(18, 4), 0, "cut at 12 o'clock");
        assert!(rgba.chunks(4).all(|px| px[..3] == [0, 0, 0]));
    }
}
