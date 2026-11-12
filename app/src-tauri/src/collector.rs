//! The background collector: the same watch loop and upload thread as
//! `collector run`, with its JSON status lines turned into app state.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use collector_core::credentials::Credentials;
use collector_core::schedule::Timing;
use collector_core::state::StateError;
use collector_core::sync::{SyncTiming, Syncer};
use collector_core::transport::{Transport, UreqTransport};
use collector_core::watch::{self, Msg, WatchOptions};
use serde_json::Value;

use crate::config::Paths;
use crate::local::{LocalServer, LocalTransport};

pub const SCAN_ERROR: &str = "读取本地 agent 日志失败，稍后会自动重试";
pub const QUEUE_ERROR: &str = "无法打开本地上传队列";
pub const BUSY_ERROR: &str = "这台电脑上已有另一个采集程序（命令行 collector run）在运行，应用暂停采集；它退出后一分钟内自动接上";
pub const KEYCHAIN_ERROR: &str = "无法读取系统钥匙串中的凭据";
pub const SETUP_ERROR: &str = "无法确定这台电脑的用户目录";
pub const LOCAL_ERROR: &str = "无法打开这台电脑上的本机数据库";

const BUSY_RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Live {
    pub last_scan_at: Option<i64>,
    pub last_error: Option<String>,
    pub upload_state: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Changed,
    /// New numbers reached the server.
    Uploaded,
}

pub type Notify = Arc<dyn Fn(Event) + Send + Sync>;

/// Where uploads go: the paired server, or in local mode (D044) the server
/// running inside this app.
#[derive(Clone)]
pub struct Destination {
    pub credentials: Credentials,
    pub local: Option<Arc<LocalServer>>,
}

impl Destination {
    fn transport(&self) -> Box<dyn Transport> {
        match &self.local {
            Some(server) => Box::new(LocalTransport(Arc::clone(server))),
            None => Box::new(UreqTransport::new()),
        }
    }
}

struct Running {
    sender: Sender<Msg>,
    handle: JoinHandle<()>,
}

pub struct Collector {
    live: Arc<Mutex<Live>>,
    running: Mutex<Option<Running>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Collector {
    pub fn new() -> Self {
        Self {
            live: Arc::new(Mutex::new(Live::default())),
            running: Mutex::new(None),
        }
    }

    pub fn live(&self) -> Live {
        lock(&self.live).clone()
    }

    pub fn set_error(&self, message: Option<&str>) {
        lock(&self.live).last_error = message.map(str::to_owned);
    }

    pub fn reset(&self) {
        *lock(&self.live) = Live::default();
    }

    pub fn is_running(&self) -> bool {
        lock(&self.running)
            .as_ref()
            .is_some_and(|running| !running.handle.is_finished())
    }

    pub fn start(
        &self,
        options: WatchOptions,
        state_dir: PathBuf,
        destination: Destination,
        notify: Notify,
    ) {
        let mut running = lock(&self.running);
        if running
            .as_ref()
            .is_some_and(|run| !run.handle.is_finished())
        {
            return;
        }
        let (sender, receiver) = watch::channel();
        let loop_sender = sender.clone();
        let live = Arc::clone(&self.live);
        let spawned = std::thread::Builder::new()
            .name("collector".to_owned())
            .spawn(move || {
                let Some(mut syncer) = start_syncer(
                    &state_dir,
                    &destination,
                    &loop_sender,
                    &receiver,
                    &live,
                    &*notify,
                    BUSY_RETRY,
                ) else {
                    return;
                };
                let mut out = StatusSink::new(live, notify);
                watch::run(&options, loop_sender, receiver, &mut out, Some(&mut syncer));
                syncer.stop();
            });
        match spawned {
            Ok(handle) => *running = Some(Running { sender, handle }),
            Err(_) => lock(&self.live).last_error = Some(QUEUE_ERROR.to_owned()),
        }
    }

    /// Waits for an in-flight scan and the upload thread to finish.
    pub fn stop(&self) {
        let running = lock(&self.running).take();
        if let Some(running) = running {
            let _ = running.sender.send(Msg::Stop);
            let _ = running.handle.join();
        }
    }
}

/// `None` when stopped while waiting, or when the queue cannot be opened.
fn start_syncer(
    state_dir: &Path,
    destination: &Destination,
    sender: &Sender<Msg>,
    receiver: &Receiver<Msg>,
    live: &Mutex<Live>,
    notify: &(dyn Fn(Event) + Send + Sync),
    retry: Duration,
) -> Option<Syncer> {
    let set_error = |message: Option<&str>| {
        lock(live).last_error = message.map(str::to_owned);
        notify(Event::Changed);
    };
    loop {
        let sink = sender.clone();
        let started = Syncer::start(
            state_dir,
            destination.credentials.clone(),
            destination.transport(),
            SyncTiming::default(),
            Box::new(move |line| {
                let _ = sink.send(Msg::Sync(line));
            }),
        );
        match started {
            Ok(syncer) => {
                if lock(live).last_error.as_deref() == Some(BUSY_ERROR) {
                    set_error(None);
                }
                return Some(syncer);
            }
            Err(StateError::Busy) => {
                if lock(live).last_error.as_deref() != Some(BUSY_ERROR) {
                    set_error(Some(BUSY_ERROR));
                }
                if wait_or_stop(receiver, retry) {
                    return None;
                }
            }
            Err(_) => {
                set_error(Some(QUEUE_ERROR));
                return None;
            }
        }
    }
}

/// True once asked to stop; nothing else is listening yet.
fn wait_or_stop(receiver: &Receiver<Msg>, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(left) {
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return true,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
}

/// Mirrors `collector run`'s defaults (see `collector/src/main.rs`).
pub fn watch_options(paths: &Paths) -> Result<WatchOptions, &'static str> {
    let explicit_home = paths.home.is_some();
    let home = match &paths.home {
        Some(home) => home.clone(),
        None => dirs::home_dir().ok_or(SETUP_ERROR)?,
    };
    let mut ignored = collector_core::roots::own_directories();
    ignored.push(home.join(".config").join("tokenlens"));
    ignored.push(paths.state_dir.clone());
    Ok(WatchOptions {
        home,
        explicit_home,
        use_env_roots: !explicit_home,
        worker_program: std::env::current_exe().map_err(|_| SETUP_ERROR)?,
        timing: Timing {
            debounce: Duration::from_millis(1500),
            max_delay: Duration::from_secs(30),
            cooldown: Duration::from_secs(30),
            full_interval: Duration::from_secs(3600),
            poll_interval: Duration::from_secs(300),
            ..Timing::default()
        },
        worker_timeout: Duration::from_secs(300),
        exit_after: None,
        ignored,
        upstream_cache_dir: Some(
            collector_core::roots::own_upstream_dir(paths.home.as_deref()).ok_or(SETUP_ERROR)?,
        ),
    })
}

/// Receives the watch loop's numeric JSON lines; nothing is logged.
struct StatusSink {
    buffer: Vec<u8>,
    live: Arc<Mutex<Live>>,
    notify: Notify,
}

impl StatusSink {
    fn new(live: Arc<Mutex<Live>>, notify: Notify) -> Self {
        Self {
            buffer: Vec::new(),
            live,
            notify,
        }
    }

    fn handle(&self, line: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let event = apply(&mut lock(&self.live), &value, now_ms());
        if let Some(event) = event {
            (self.notify)(event);
        }
    }
}

fn apply(live: &mut Live, value: &Value, now: i64) -> Option<Event> {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if value.get("scan").is_some() {
        if ok {
            live.last_scan_at = Some(now);
            if live.last_error.as_deref() == Some(SCAN_ERROR) {
                live.last_error = None;
            }
        } else {
            live.last_error = Some(SCAN_ERROR.to_owned());
        }
        return Some(Event::Changed);
    }
    if value.get("state").and_then(Value::as_str) == Some("seed") {
        if !ok {
            live.last_error = Some(QUEUE_ERROR.to_owned());
        }
        return Some(Event::Changed);
    }
    let kind = value.get("sync").and_then(Value::as_str)?;
    if let Some(state) = value.get("state").and_then(Value::as_str) {
        live.upload_state = Some(state.to_owned());
    }
    let accepted = ["accepted", "updated"]
        .iter()
        .filter_map(|field| value.get(*field).and_then(Value::as_u64))
        .sum::<u64>();
    if kind == "upload" && ok && accepted > 0 {
        Some(Event::Uploaded)
    } else {
        Some(Event::Changed)
    }
}

impl Write for StatusSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            self.handle(&line[..end]);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn scan_lines_update_the_last_scan_and_clear_scan_errors() {
        let mut live = Live::default();
        assert_eq!(
            apply(&mut live, &json!({"scan": "full", "ok": false}), 5),
            Some(Event::Changed)
        );
        assert_eq!(live.last_error.as_deref(), Some(SCAN_ERROR));
        assert_eq!(live.last_scan_at, None);
        apply(
            &mut live,
            &json!({"scan": "triggered", "ok": true, "events": 3}),
            9,
        );
        assert_eq!(live.last_scan_at, Some(9));
        assert_eq!(live.last_error, None);
    }

    #[test]
    fn other_errors_survive_a_good_scan() {
        let mut live = Live {
            last_error: Some(QUEUE_ERROR.to_owned()),
            ..Live::default()
        };
        apply(&mut live, &json!({"scan": "full", "ok": true}), 1);
        assert_eq!(live.last_error.as_deref(), Some(QUEUE_ERROR));
    }

    #[test]
    fn sync_lines_track_upload_state_and_new_numbers() {
        let mut live = Live::default();
        let queued = json!({"sync": "queued", "ok": true, "events": 4, "backlog": 4});
        assert_eq!(apply(&mut live, &queued, 1), Some(Event::Changed));
        let uploaded = json!({"sync": "upload", "ok": true, "state": "ok", "accepted": 4});
        assert_eq!(apply(&mut live, &uploaded, 2), Some(Event::Uploaded));
        assert_eq!(live.upload_state.as_deref(), Some("ok"));
        let duplicates = json!({"sync": "upload", "ok": true, "state": "ok", "duplicates": 4});
        assert_eq!(apply(&mut live, &duplicates, 3), Some(Event::Changed));
        let failed =
            json!({"sync": "upload", "ok": false, "state": "backoff:network", "retryInMs": 30000});
        apply(&mut live, &failed, 4);
        assert_eq!(live.upload_state.as_deref(), Some("backoff:network"));
        let heartbeat = json!({"sync": "heartbeat", "ok": false, "state": "backoff:network", "error": "network"});
        apply(&mut live, &heartbeat, 4);
        assert_eq!(live.upload_state.as_deref(), Some("backoff:network"));
        assert_eq!(
            apply(
                &mut live,
                &json!({"watch": "roots", "roots": 3, "ok": true}),
                5
            ),
            None
        );
    }

    #[test]
    fn the_sink_splits_lines_across_writes() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let live = Arc::new(Mutex::new(Live::default()));
        let mut sink = StatusSink::new(
            Arc::clone(&live),
            Arc::new(move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
        );
        sink.write_all(b"{\"scan\":\"full\",\"ok\":tr").unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        sink.write_all(b"ue}\nnot json\n{\"state\":\"seed\",\"ok\":false}\n")
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let live = lock(&live).clone();
        assert!(live.last_scan_at.is_some());
        assert_eq!(live.last_error.as_deref(), Some(QUEUE_ERROR));
    }

    fn synthetic_credentials() -> Credentials {
        Credentials {
            server_url: "http://127.0.0.1:9".into(),
            device_id: "dev_synthetic".into(),
            ingest_token: "tdi_synthetic".into(),
            read_token: "tdr_synthetic".into(),
        }
    }

    #[test]
    fn a_busy_queue_is_reported_and_retried_until_it_frees_up() {
        let dir = tempfile::tempdir().unwrap();
        let held = collector_core::state::lock_queue(dir.path()).unwrap();
        let (sender, receiver) = watch::channel();
        let live = Arc::new(Mutex::new(Live::default()));
        let (seen, events) = std::sync::mpsc::channel();
        let notify = move |event| {
            let _ = seen.send(event);
        };
        let state_dir = dir.path().to_owned();
        let thread_live = Arc::clone(&live);
        let thread_sender = sender.clone();
        let waiting = std::thread::spawn(move || {
            let destination = Destination {
                credentials: synthetic_credentials(),
                local: None,
            };
            start_syncer(
                &state_dir,
                &destination,
                &thread_sender,
                &receiver,
                &thread_live,
                &notify,
                Duration::from_millis(50),
            )
            .map(Syncer::stop)
            .is_some()
        });
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(lock(&live).last_error.as_deref(), Some(BUSY_ERROR));
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            events.try_recv().is_err(),
            "retries do not repeat the notice"
        );
        drop(held);
        assert!(waiting.join().unwrap(), "starts once the other one stops");
        assert_eq!(lock(&live).last_error, None);
    }

    #[test]
    fn stopping_while_the_queue_is_busy_returns_quickly() {
        let dir = tempfile::tempdir().unwrap();
        let _held = collector_core::state::lock_queue(dir.path()).unwrap();
        let (sender, receiver) = watch::channel();
        let live = Mutex::new(Live::default());
        sender.send(Msg::Change("ignored".into())).unwrap();
        sender.send(Msg::Stop).unwrap();
        let started = std::time::Instant::now();
        let syncer = start_syncer(
            dir.path(),
            &Destination {
                credentials: synthetic_credentials(),
                local: None,
            },
            &sender,
            &receiver,
            &live,
            &|_| {},
            BUSY_RETRY,
        );
        assert!(syncer.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
