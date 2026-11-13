//! The background collector: the same watch loop and upload thread as
//! `collector run`, with its JSON status lines turned into app state.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use collector_core::credentials::Credentials;
use collector_core::schedule::Timing;
use collector_core::snapshot::{ScannedEvent, Scope, Snapshot};
use collector_core::state::{Prepared, StateError};
use collector_core::sync::{Recorded, SyncLine, SyncTiming, Syncer};
use collector_core::transport::{Transport, UreqTransport};
use collector_core::watch::{self, Msg, ScanSync, WatchOptions};
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
    pub server_upload_state: Option<String>,
    pub server_queue_error: bool,
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

enum RunMode {
    #[cfg(test)]
    Single(Destination),
    LocalFirst {
        local: Destination,
        server: Option<Destination>,
        legacy_local_queue: bool,
    },
}

#[derive(Default)]
struct Lifecycle {
    running: Option<Running>,
    /// Some means an updater owns the stop/start gate; the bool remembers
    /// whether collection was running before it acquired the gate.
    update_resume: Option<bool>,
}

pub struct Collector {
    live: Arc<Mutex<Live>>,
    lifecycle: Mutex<Lifecycle>,
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
            lifecycle: Mutex::new(Lifecycle::default()),
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
        lock(&self.lifecycle)
            .running
            .as_ref()
            .is_some_and(|running| !running.handle.is_finished())
    }

    #[cfg(test)]
    pub fn start(
        &self,
        options: WatchOptions,
        state_dir: PathBuf,
        destination: Destination,
        notify: Notify,
    ) {
        self.start_run(options, state_dir, RunMode::Single(destination), notify);
    }

    /// One scan feeds the local database and, when paired, a separate server
    /// queue. The local destination is always active.
    pub fn start_with_server(
        &self,
        options: WatchOptions,
        state_dir: PathBuf,
        local: Destination,
        server: Option<Destination>,
        legacy_local_queue: bool,
        notify: Notify,
    ) {
        self.start_run(
            options,
            state_dir,
            RunMode::LocalFirst {
                local,
                server,
                legacy_local_queue,
            },
            notify,
        );
    }

    fn start_run(&self, options: WatchOptions, state_dir: PathBuf, mode: RunMode, notify: Notify) {
        let mut lifecycle = lock(&self.lifecycle);
        if lifecycle.update_resume.is_some()
            || lifecycle
                .running
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
            .spawn(move || match mode {
                RunMode::LocalFirst {
                    local,
                    server,
                    legacy_local_queue,
                } => {
                    let Some(mut syncer) = Fanout::start(
                        &state_dir,
                        FanoutDestinations {
                            local: &local,
                            server: server.as_ref(),
                            legacy_local_queue,
                        },
                        &loop_sender,
                        &receiver,
                        &live,
                        &notify,
                    ) else {
                        return;
                    };
                    let mut out = StatusSink::new(live, notify);
                    watch::run(&options, loop_sender, receiver, &mut out, Some(&mut syncer));
                    syncer.stop();
                }
                #[cfg(test)]
                RunMode::Single(destination) => {
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
                }
            });
        match spawned {
            Ok(handle) => lifecycle.running = Some(Running { sender, handle }),
            Err(_) => lock(&self.live).last_error = Some(QUEUE_ERROR.to_owned()),
        }
    }

    /// Waits for an in-flight scan and the upload thread to finish.
    pub fn stop(&self) {
        Self::stop_locked(&mut lock(&self.lifecycle));
    }

    fn stop_locked(lifecycle: &mut Lifecycle) {
        if let Some(running) = lifecycle.running.take() {
            let _ = running.sender.send(Msg::Stop);
            // Hold the lifecycle gate until all worker-owned locks have been
            // released. A concurrent stop must not return before this join.
            let _ = running.handle.join();
        }
    }

    /// Atomically prohibit future starts and join the current collector. The
    /// installer can run only after this returns; no other IPC can revive it.
    pub fn begin_update(&self) {
        let mut lifecycle = lock(&self.lifecycle);
        if lifecycle.update_resume.is_none() {
            lifecycle.update_resume = Some(
                lifecycle
                    .running
                    .as_ref()
                    .is_some_and(|running| !running.handle.is_finished()),
            );
        }
        Self::stop_locked(&mut lifecycle);
    }

    /// On install failure, release the gate and report the original running
    /// state. The caller still honors a user's new pause/unpair selection.
    pub fn finish_update(&self) -> bool {
        lock(&self.lifecycle).update_resume.take().unwrap_or(false)
    }
}

/// Each destination consults its own durable queue before accepting changes.
/// Only the watch loop retains a complete snapshot in memory.
struct Target {
    syncer: Syncer,
}

impl Target {
    fn new(syncer: Syncer) -> Self {
        Self { syncer }
    }

    fn record_scan(
        &mut self,
        full: bool,
        counts: &BTreeMap<String, u64>,
        changed: &[ScannedEvent],
        duration: Duration,
    ) -> Recorded {
        match self.syncer.filter_unknown(changed) {
            Ok(needed) => self.syncer.record_scan(full, counts, needed, duration),
            Err(_) => Recorded {
                line: SyncLine {
                    sync: "queued",
                    ok: false,
                    error: Some("state_read_failed".to_owned()),
                    ..SyncLine::default()
                },
                forget: changed.iter().map(|event| event.key.clone()).collect(),
            },
        }
    }
}

struct Fanout {
    local: Target,
    server: Option<Target>,
    live: Arc<Mutex<Live>>,
    notify: Notify,
    // Also held without a server pairing, and if opening its queue fails.
    _root_lock: Arc<File>,
}

struct FanoutDestinations<'a> {
    local: &'a Destination,
    server: Option<&'a Destination>,
    legacy_local_queue: bool,
}

impl Fanout {
    fn start(
        state_dir: &Path,
        destinations: FanoutDestinations<'_>,
        sender: &Sender<Msg>,
        receiver: &Receiver<Msg>,
        live: &Arc<Mutex<Live>>,
        notify: &Notify,
    ) -> Option<Self> {
        let FanoutDestinations {
            local,
            server,
            legacy_local_queue,
        } = destinations;
        let root_lock = loop {
            match collector_core::state::lock_queue(state_dir) {
                Ok(guard) => break Arc::new(guard),
                Err(StateError::Busy) => {
                    if lock(live).last_error.as_deref() != Some(BUSY_ERROR) {
                        lock(live).last_error = Some(BUSY_ERROR.to_owned());
                        notify(Event::Changed);
                    }
                    if wait_or_stop(receiver, BUSY_RETRY) {
                        return None;
                    }
                }
                Err(_) => {
                    lock(live).last_error = Some(QUEUE_ERROR.to_owned());
                    notify(Event::Changed);
                    return None;
                }
            }
        };
        if lock(live).last_error.as_deref() == Some(BUSY_ERROR) {
            lock(live).last_error = None;
        }
        if legacy_local_queue
            && crate::local::migrate_queue(state_dir, &local.credentials.device_id).is_err()
        {
            lock(live).last_error = Some(QUEUE_ERROR.to_owned());
            notify(Event::Changed);
            return None;
        }
        let local_syncer = start_syncer(
            &state_dir.join("local-queue"),
            local,
            sender,
            receiver,
            live,
            &**notify,
            BUSY_RETRY,
        )?;
        let local = Target::new(local_syncer);
        let server = server.and_then(|destination| {
            let sink = sender.clone();
            let status = Arc::clone(live);
            let started = Syncer::start_with_queue_lock(
                state_dir,
                destination.credentials.clone(),
                destination.transport(),
                SyncTiming::default(),
                Box::new(move |mut line| {
                    apply_server_line(&mut lock(&status), &mut line);
                    // Wake the watch loop (including restore/resync handling)
                    // without overwriting the local upload state.
                    let _ = sink.send(Msg::Sync(line));
                }),
                Arc::clone(&root_lock),
            )
            .map(Target::new);
            match started {
                Ok(target) => Some(target),
                Err(_) => {
                    set_server_queue_status(&mut lock(live), false);
                    notify(Event::Changed);
                    None
                }
            }
        });
        Some(Self {
            local,
            server,
            live: Arc::clone(live),
            notify: Arc::clone(notify),
            _root_lock: root_lock,
        })
    }

    fn stop(self) {
        self.local.syncer.stop();
        if let Some(server) = self.server {
            server.syncer.stop();
        }
    }
}

impl ScanSync for Fanout {
    fn prepared(&self) -> Prepared {
        let local = self.local.syncer.prepared;
        let server = self
            .server
            .as_ref()
            .map(|target| target.syncer.prepared)
            .unwrap_or_default();
        Prepared {
            device_changed: local.device_changed || server.device_changed,
            generation_changed: local.generation_changed || server.generation_changed,
        }
    }

    fn seed(&self, snapshot: &mut Snapshot) -> Result<usize, StateError> {
        *snapshot = Snapshot::default();
        if let Err(error) = self.local.syncer.seed(snapshot) {
            *snapshot = Snapshot::default();
            return Err(error);
        }
        if let Some(server) = &self.server {
            let mut known = Snapshot::default();
            if server.syncer.seed(&mut known).is_err() {
                // Keep all payloads so a later scan can repair the remote
                // queue, while the local destination remains usable.
                *snapshot = Snapshot::default();
                set_server_queue_status(&mut lock(&self.live), false);
                (self.notify)(Event::Changed);
                return Ok(0);
            }
            if set_server_queue_status(&mut lock(&self.live), true) {
                (self.notify)(Event::Changed);
            }
            snapshot.retain_matching(&known);
        }
        Ok(snapshot.len())
    }

    fn take_resync(&mut self) -> bool {
        // Always consume both flags: `||` here would defer the second one.
        let local = self.local.syncer.take_resync();
        let server = self
            .server
            .as_mut()
            .is_some_and(|target| target.syncer.take_resync());
        local || server
    }

    fn record_scan(
        &mut self,
        full: bool,
        counts: &BTreeMap<String, u64>,
        changed: Vec<ScannedEvent>,
        duration: Duration,
    ) -> Recorded {
        let mut recorded = self.local.record_scan(full, counts, &changed, duration);
        let status_changed = {
            let mut live = lock(&self.live);
            if !recorded.line.ok && live.last_error.as_deref() != Some(QUEUE_ERROR) {
                live.last_error = Some(QUEUE_ERROR.to_owned());
                true
            } else if recorded.line.ok && live.last_error.as_deref() == Some(QUEUE_ERROR) {
                live.last_error = None;
                true
            } else {
                false
            }
        };
        if status_changed {
            (self.notify)(Event::Changed);
        }
        if let Some(server) = &mut self.server {
            let remote = server.record_scan(full, counts, &changed, duration);
            recorded.forget.extend(remote.forget);
            if set_server_queue_status(&mut lock(&self.live), remote.line.ok) {
                (self.notify)(Event::Changed);
            }
        }
        recorded
    }

    fn record_failure(&mut self, scope: &Scope, code: &str) {
        self.local.syncer.record_failure(scope, code);
        if let Some(server) = &mut self.server {
            server.syncer.record_failure(scope, code);
        }
    }
}

/// Queue failures stay visible until a queue operation verifies recovery.
/// Network heartbeats cannot establish whether scan data can be persisted.
fn set_server_queue_status(live: &mut Live, okay: bool) -> bool {
    let previous = live.server_queue_error;
    live.server_queue_error = !okay;
    if okay {
        if live.server_upload_state.as_deref() == Some("queue_error") {
            live.server_upload_state = None;
        }
    } else {
        live.server_upload_state = Some("queue_error".to_owned());
    }
    previous != live.server_queue_error
}

fn apply_server_line(live: &mut Live, line: &mut SyncLine) {
    let state = server_sync_state(line);
    if state.as_deref() == Some("queue_error") {
        set_server_queue_status(live, false);
    } else if live.server_queue_error {
        live.server_upload_state = Some("queue_error".to_owned());
    } else if let Some(state) = state {
        live.server_upload_state = Some(state);
    }
}

/// Server failures are separate from local collection. In particular a failed
/// heartbeat can carry `state = ok`, so its actual failure must remain visible.
fn server_sync_state(line: &mut SyncLine) -> Option<String> {
    let state = line.state.take();
    if !line.ok
        && let Some(error) = &line.error
    {
        if error.starts_with("state_") {
            return Some("queue_error".to_owned());
        }
        if state.as_deref() == Some("ok") {
            return Some(format!("backoff:{error}"));
        }
    }
    state
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
    use collector_core::credentials::Credentials;
    use collector_core::state::State;
    use collector_core::transport::{HttpResponse, TransportError};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use usage_event::envelope::FormFactor;

    struct TestTransport {
        local: LocalTransport,
        online: Arc<AtomicBool>,
        uploads: Arc<AtomicUsize>,
        failed_requests: Arc<AtomicUsize>,
        store_id: Arc<Mutex<String>>,
    }

    impl Transport for TestTransport {
        fn post(
            &self,
            url: &str,
            bearer: Option<&str>,
            gzip: bool,
            body: &[u8],
        ) -> Result<HttpResponse, TransportError> {
            if !self.online.load(Ordering::SeqCst) {
                self.failed_requests.fetch_add(1, Ordering::SeqCst);
                return Err(TransportError::Network);
            }
            let mut reply = self.local.post(url, bearer, gzip, body)?;
            if reply.status == 200 {
                if url.ends_with("/events") {
                    self.uploads.fetch_add(1, Ordering::SeqCst);
                }
                let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
                value["storeId"] = json!(lock(&self.store_id).clone());
                reply.body = serde_json::to_vec(&value).unwrap();
            }
            Ok(reply)
        }
    }

    struct TestDestination {
        server: Arc<LocalServer>,
        credentials: Credentials,
        online: Arc<AtomicBool>,
        uploads: Arc<AtomicUsize>,
        failed_requests: Arc<AtomicUsize>,
        store_id: Arc<Mutex<String>>,
        restored: Arc<AtomicBool>,
    }

    impl TestDestination {
        fn new(path: &Path, online: bool) -> Self {
            let server = LocalServer::open_with(path, None).unwrap();
            let credentials = server.pair(FormFactor::Laptop, "Fixture computer").unwrap();
            Self {
                server,
                credentials,
                online: Arc::new(AtomicBool::new(online)),
                uploads: Arc::new(AtomicUsize::new(0)),
                failed_requests: Arc::new(AtomicUsize::new(0)),
                store_id: Arc::new(Mutex::new("aaaa".to_owned())),
                restored: Arc::new(AtomicBool::new(false)),
            }
        }

        fn target(&self, queue: &Path, guard: Arc<File>) -> Target {
            let restored = Arc::clone(&self.restored);
            let syncer = Syncer::start_with_queue_lock(
                queue,
                self.credentials.clone(),
                Box::new(TestTransport {
                    local: LocalTransport(Arc::clone(&self.server)),
                    online: Arc::clone(&self.online),
                    uploads: Arc::clone(&self.uploads),
                    failed_requests: Arc::clone(&self.failed_requests),
                    store_id: Arc::clone(&self.store_id),
                }),
                SyncTiming {
                    min_round_gap: Duration::from_millis(5),
                    heartbeat_interval: Duration::from_millis(20),
                    backoff_base: Duration::from_millis(5),
                    backoff_max: Duration::from_millis(20),
                },
                Box::new(move |line| {
                    if line.resync == Some(true) {
                        restored.store(true, Ordering::SeqCst);
                    }
                }),
                guard,
            )
            .unwrap();
            Target::new(syncer)
        }

        fn input(&self) -> i64 {
            let response = LocalTransport(Arc::clone(&self.server))
                .get(
                    "local://this-computer/api/dashboard/summary?from=2023-11-01&to=2023-11-30",
                    Some(&self.credentials.read_token),
                )
                .unwrap();
            let value: Value = serde_json::from_slice(&response.body).unwrap();
            value["inputTokens"].as_i64().unwrap()
        }
    }

    fn fixture_events() -> Vec<ScannedEvent> {
        let values: Vec<usage_event::UsageEvent> =
            serde_json::from_str(include_str!("../../../fixtures/usage-events.json")).unwrap();
        values
            .into_iter()
            .map(|event| ScannedEvent {
                key: event.event_key.clone(),
                client: event.client.clone(),
                ts: event.ts,
                hash: collector_core::event_value_hash(&event),
                payload: Some(collector_core::canonical_json(
                    &serde_json::to_value(event).unwrap(),
                )),
            })
            .collect()
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let limit = std::time::Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                std::time::Instant::now() < limit,
                "background sync timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_disconnected_server_does_not_block_local_data_and_both_recover_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let local = TestDestination::new(&root.path().join("local-store"), true);
        let server = TestDestination::new(&root.path().join("server-store"), false);
        let queue = root.path().join("queue");
        let local_queue = queue.join("local-queue");
        let root_guard = Arc::new(collector_core::state::lock_queue(&queue).unwrap());
        let local_guard = Arc::new(collector_core::state::lock_queue(&local_queue).unwrap());
        let counts = BTreeMap::from([
            ("synthetic-client".to_owned(), 3),
            ("qoder-intl".to_owned(), 1),
        ]);
        let events = fixture_events();
        let mut local_target = local.target(&local_queue, local_guard);
        assert_eq!(
            local_target
                .record_scan(true, &counts, &events, Duration::ZERO)
                .line
                .events,
            Some(4)
        );
        wait_until(|| local.input() == 540 && local.uploads.load(Ordering::SeqCst) == 1);
        // Connecting a server keeps the already populated local queue.
        let mut fanout = Fanout {
            local: local_target,
            server: Some(server.target(&queue, Arc::clone(&root_guard))),
            live: Arc::new(Mutex::new(Live::default())),
            notify: Arc::new(|_| {}),
            _root_lock: root_guard,
        };
        let mut snapshot = Snapshot::default();
        assert_eq!(
            fanout.seed(&mut snapshot).unwrap(),
            0,
            "new server needs all existing payloads"
        );
        let recorded = fanout.record_scan(true, &counts, events.clone(), Duration::ZERO);
        assert!(recorded.line.ok);
        assert_eq!(
            recorded.line.events,
            Some(0),
            "existing local values are not uploaded again"
        );
        wait_until(|| local.input() == 540);
        assert_eq!(
            server.input(),
            0,
            "a failed remote request cannot stop the local destination"
        );
        assert_eq!(
            collector_core::sync::local_status(&queue)
                .unwrap()
                .unwrap()
                .outbox_backlog,
            4
        );
        wait_until(|| server.failed_requests.load(Ordering::SeqCst) > 0);

        // Even when another destination needs a payload, this destination
        // never queues its own unchanged value for upload again.
        let again = fanout.record_scan(true, &counts, events.clone(), Duration::ZERO);
        assert_eq!(again.line.events, Some(0));
        assert_eq!(local.uploads.load(Ordering::SeqCst), 1);
        server.online.store(true, Ordering::SeqCst);
        wait_until(|| server.input() == 540);
        wait_until(|| {
            collector_core::sync::local_status(&queue)
                .unwrap()
                .unwrap()
                .outbox_backlog
                == 0
        });
        assert_eq!(server.uploads.load(Ordering::SeqCst), 1);

        // Restoring only the server must not re-upload unchanged local data.
        *lock(&server.store_id) = "bbbb".to_owned();
        wait_until(|| server.restored.load(Ordering::SeqCst));
        assert!(fanout.take_resync());
        assert!(!fanout.take_resync());
        assert_eq!(fanout.seed(&mut snapshot).unwrap(), 0);
        assert_eq!(
            fanout
                .record_scan(true, &counts, events.clone(), Duration::ZERO)
                .line
                .events,
            Some(0)
        );
        wait_until(|| server.uploads.load(Ordering::SeqCst) == 2);
        wait_until(|| {
            collector_core::sync::local_status(&queue)
                .unwrap()
                .unwrap()
                .outbox_backlog
                == 0
        });
        assert_eq!(local.uploads.load(Ordering::SeqCst), 1);
        assert_eq!(local.input(), 540);
        assert_eq!(server.input(), 540);

        // Only a verified queue operation can clear a sticky write error.
        set_server_queue_status(&mut lock(&fanout.live), false);
        let mut heartbeat = SyncLine {
            sync: "heartbeat",
            ok: true,
            state: Some("ok".to_owned()),
            ..SyncLine::default()
        };
        apply_server_line(&mut lock(&fanout.live), &mut heartbeat);
        assert!(lock(&fanout.live).server_queue_error);
        assert_eq!(
            lock(&fanout.live).server_upload_state.as_deref(),
            Some("queue_error")
        );
        fanout.record_scan(true, &counts, events.clone(), Duration::ZERO);
        assert!(!lock(&fanout.live).server_queue_error);
        assert_ne!(
            lock(&fanout.live).server_upload_state.as_deref(),
            Some("queue_error")
        );
        assert_eq!(local.uploads.load(Ordering::SeqCst), 1);
        assert_eq!(server.uploads.load(Ordering::SeqCst), 2);
        set_server_queue_status(&mut lock(&fanout.live), false);
        assert_eq!(fanout.seed(&mut snapshot).unwrap(), 4);
        assert!(!lock(&fanout.live).server_queue_error);

        // Both restore signals must be consumed in the same watch turn.
        local.restored.store(false, Ordering::SeqCst);
        server.restored.store(false, Ordering::SeqCst);
        *lock(&local.store_id) = "cccc".to_owned();
        *lock(&server.store_id) = "cccc".to_owned();
        wait_until(|| {
            local.restored.load(Ordering::SeqCst) && server.restored.load(Ordering::SeqCst)
        });
        assert!(fanout.take_resync());
        let mut known = Snapshot::default();
        assert_eq!(fanout.local.syncer.seed(&mut known).unwrap(), 0);
        assert_eq!(
            fanout
                .server
                .as_ref()
                .unwrap()
                .syncer
                .seed(&mut known)
                .unwrap(),
            0
        );
        assert!(!fanout.take_resync());
        assert_eq!(fanout.seed(&mut snapshot).unwrap(), 0);
        fanout.record_scan(true, &counts, events.clone(), Duration::ZERO);
        wait_until(|| {
            local.uploads.load(Ordering::SeqCst) == 2 && server.uploads.load(Ordering::SeqCst) == 3
        });
        assert_eq!(local.input(), 540);
        assert_eq!(server.input(), 540);
        fanout.stop();
        assert!(collector_core::state::lock_queue(&queue).is_ok());
        assert!(collector_core::state::lock_queue(&local_queue).is_ok());

        let root_guard = Arc::new(collector_core::state::lock_queue(&queue).unwrap());
        let local_guard = Arc::new(collector_core::state::lock_queue(&local_queue).unwrap());
        let mut restarted = Fanout {
            local: local.target(&local_queue, local_guard),
            server: Some(server.target(&queue, Arc::clone(&root_guard))),
            live: Arc::new(Mutex::new(Live::default())),
            notify: Arc::new(|_| {}),
            _root_lock: root_guard,
        };
        assert_eq!(
            restarted.seed(&mut snapshot).unwrap(),
            4,
            "restart seeds the common values"
        );
        assert_eq!(
            restarted
                .record_scan(true, &counts, events, Duration::ZERO)
                .line
                .events,
            Some(0)
        );
        assert_eq!(local.uploads.load(Ordering::SeqCst), 2);
        assert_eq!(server.uploads.load(Ordering::SeqCst), 3);
        restarted.stop();
    }

    #[test]
    fn a_corrupt_server_queue_does_not_block_a_new_local_destination() {
        let root = tempfile::tempdir().unwrap();
        let local = TestDestination::new(&root.path().join("local-store"), true);
        let queue = root.path().join("queue");
        std::fs::create_dir_all(&queue).unwrap();
        std::fs::write(State::path_in(&queue), b"synthetic invalid SQLite database").unwrap();
        let local_destination = Destination {
            credentials: local.credentials.clone(),
            local: Some(Arc::clone(&local.server)),
        };
        let server_destination = Destination {
            credentials: synthetic_credentials(),
            local: None,
        };
        let (sender, receiver) = watch::channel();
        let live = Arc::new(Mutex::new(Live::default()));
        let notify: Notify = Arc::new(|_| {});
        let mut fanout = Fanout::start(
            &queue,
            FanoutDestinations {
                local: &local_destination,
                server: Some(&server_destination),
                legacy_local_queue: false,
            },
            &sender,
            &receiver,
            &live,
            &notify,
        )
        .expect("local destination starts despite the damaged server queue");
        assert!(fanout.server.is_none());
        assert!(lock(&live).server_queue_error);
        assert_eq!(lock(&live).last_error, None);
        let counts = BTreeMap::from([
            ("synthetic-client".to_owned(), 3),
            ("qoder-intl".to_owned(), 1),
        ]);
        assert!(
            fanout
                .record_scan(true, &counts, fixture_events(), Duration::ZERO)
                .line
                .ok
        );
        wait_until(|| local.input() == 540);
        let mut heartbeat = SyncLine {
            sync: "heartbeat",
            ok: true,
            state: Some("ok".to_owned()),
            ..SyncLine::default()
        };
        apply_server_line(&mut lock(&live), &mut heartbeat);
        assert!(lock(&live).server_queue_error);
        assert_eq!(
            lock(&live).server_upload_state.as_deref(),
            Some("queue_error")
        );
        assert_eq!(
            lock(&live).last_error,
            None,
            "remote errors must not mark local collection broken"
        );
        assert!(collector_core::state::lock_queue(&queue).is_err());
        fanout.stop();
        assert!(collector_core::state::lock_queue(&queue).is_ok());
    }

    #[test]
    fn server_heartbeat_failures_do_not_replace_the_local_upload_state() {
        let mut line = SyncLine {
            sync: "heartbeat",
            ok: false,
            state: Some("ok".to_owned()),
            error: Some("network".to_owned()),
            ..SyncLine::default()
        };
        assert_eq!(
            server_sync_state(&mut line).as_deref(),
            Some("backoff:network")
        );
        assert_eq!(line.state, None);
        let mut live = Live {
            upload_state: Some("ok".to_owned()),
            ..Live::default()
        };
        apply(&mut live, &serde_json::to_value(line).unwrap(), 1);
        assert_eq!(live.upload_state.as_deref(), Some("ok"));
    }

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

    /// A real thread owns the real queue lock, but reads no agent data and
    /// performs no HTTP. Its shutdown pauses until the test releases it.
    fn simulated_running(collector: &Collector, state_dir: &Path) -> (Receiver<()>, Sender<()>) {
        let queue_lock = collector_core::state::lock_queue(state_dir).unwrap();
        let (sender, receiver) = watch::channel();
        let (stopping, stopped) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _queue_lock = queue_lock;
            assert!(matches!(receiver.recv().unwrap(), Msg::Stop));
            stopping.send(()).unwrap();
            let _ = released.recv();
        });
        lock(&collector.lifecycle).running = Some(Running { sender, handle });
        (stopped, release)
    }

    fn isolated_start(collector: &Collector, root: &Path) {
        collector.start(
            WatchOptions {
                home: root.join("empty-home"),
                explicit_home: true,
                use_env_roots: false,
                worker_program: root.join("nonexistent-test-worker"),
                timing: Timing::default(),
                worker_timeout: Duration::from_millis(100),
                exit_after: Some(Duration::from_secs(1)),
                ignored: vec![],
                upstream_cache_dir: Some(root.join("isolated-cache")),
            },
            root.join("queue"),
            Destination {
                credentials: synthetic_credentials(),
                local: None,
            },
            Arc::new(|_| {}),
        );
    }

    #[test]
    fn concurrent_stops_both_wait_for_the_worker_and_queue_lock_to_be_released() {
        let dir = tempfile::tempdir().unwrap();
        let collector = Arc::new(Collector::new());
        let queue = dir.path().join("queue");
        let (stopping, release) = simulated_running(&collector, &queue);
        let first_collector = Arc::clone(&collector);
        let (first_done, first_finished) = std::sync::mpsc::channel();
        let first = std::thread::spawn(move || {
            first_collector.stop();
            first_done.send(()).unwrap();
        });
        stopping.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(collector_core::state::lock_queue(&queue).is_err());

        let second_collector = Arc::clone(&collector);
        let (attempt, attempted) = std::sync::mpsc::channel();
        let (second_done, second_finished) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            attempt.send(()).unwrap();
            second_collector.stop();
            second_done.send(()).unwrap();
        });
        attempted.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            second_finished
                .recv_timeout(Duration::from_millis(150))
                .is_err(),
            "a second stop must not return while the first one is joining"
        );
        assert!(first_finished.try_recv().is_err());
        release.send(()).unwrap();
        first_finished.recv_timeout(Duration::from_secs(5)).unwrap();
        second_finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        first.join().unwrap();
        second.join().unwrap();
        assert!(!collector.is_running());
        assert!(collector_core::state::lock_queue(&queue).is_ok());
    }

    #[test]
    fn update_gate_joins_the_worker_and_rejects_concurrent_and_later_starts() {
        let dir = tempfile::tempdir().unwrap();
        let collector = Arc::new(Collector::new());
        let queue = dir.path().join("queue");
        let (stopping, release) = simulated_running(&collector, &queue);
        let updating = Arc::clone(&collector);
        let (installed, install_ready) = std::sync::mpsc::channel();
        let update = std::thread::spawn(move || {
            updating.begin_update();
            installed.send(()).unwrap();
        });
        stopping.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(install_ready.try_recv().is_err());
        assert!(collector_core::state::lock_queue(&queue).is_err());

        let starting = Arc::clone(&collector);
        let root = dir.path().to_owned();
        let (attempt, attempted) = std::sync::mpsc::channel();
        let (started, start_finished) = std::sync::mpsc::channel();
        let start = std::thread::spawn(move || {
            attempt.send(()).unwrap();
            isolated_start(&starting, &root);
            started.send(()).unwrap();
        });
        attempted.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            start_finished
                .recv_timeout(Duration::from_millis(150))
                .is_err()
        );
        release.send(()).unwrap();
        install_ready.recv_timeout(Duration::from_secs(5)).unwrap();
        start_finished.recv_timeout(Duration::from_secs(5)).unwrap();
        update.join().unwrap();
        start.join().unwrap();
        assert!(!collector.is_running());
        assert!(lock(&collector.lifecycle).running.is_none());
        assert!(collector_core::state::lock_queue(&queue).is_ok());
        isolated_start(&collector, dir.path());
        assert!(lock(&collector.lifecycle).running.is_none());

        // A failed update restores exactly the pre-update running state.
        assert!(collector.finish_update());
        isolated_start(&collector, dir.path());
        assert!(collector.is_running());
        collector.stop();
        assert!(!collector.finish_update(), "the gate is released only once");
    }

    #[test]
    fn a_failed_update_does_not_request_a_restart_for_a_stopped_collector() {
        let collector = Collector::new();
        collector.begin_update();
        collector.stop();
        assert!(!collector.finish_update());
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
