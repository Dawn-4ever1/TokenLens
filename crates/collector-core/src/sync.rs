//! Outbox upload, backfill and heartbeat (architecture 5.6–5.8).
//!
//! The watch loop records changed events in the local state database; a
//! background thread with its own connection uploads them in batches and
//! sends heartbeats. Printed lines carry numbers and short codes only.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;
use usage_event::envelope::{
    self, ClientStatus, CollectorInfo, Heartbeat, HeartbeatResponse, IngestEnvelope, IngestResponse,
};

use crate::credentials::Credentials;
use crate::snapshot::{ScannedEvent, Scope, Snapshot};
use crate::state::{self, Batch, Prepared, State, StateError, meta};
use crate::transport::{self, Transport};

pub const COLLECTOR_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Raw envelope cap; well below the server's decompressed-body limit.
const MAX_RAW_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEARTBEAT_CLIENTS: usize = 256;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncTiming {
    /// Minimum time between the starts of two upload rounds.
    pub min_round_gap: Duration,
    pub heartbeat_interval: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for SyncTiming {
    fn default() -> Self {
        Self {
            min_round_gap: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(300),
            backoff_base: Duration::from_secs(30),
            backoff_max: Duration::from_secs(1800),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadState {
    Ok,
    /// Retrying later (network, server error, rate limit).
    Backoff(String),
    /// The server no longer accepts this device's token.
    PairingRequired,
    UpgradeRequired,
    /// A request the server will keep refusing; uploads stop until restart.
    Blocked(String),
}

impl UploadState {
    pub fn code(&self) -> String {
        match self {
            Self::Ok => "ok".to_owned(),
            Self::Backoff(code) => format!("backoff:{code}"),
            Self::PairingRequired => "pairing_required".to_owned(),
            Self::UpgradeRequired => "upgrade_required".to_owned(),
            Self::Blocked(code) => format!("blocked:{code}"),
        }
    }

    pub fn uploads_allowed(&self) -> bool {
        matches!(self, Self::Ok | Self::Backoff(_))
    }

    pub fn heartbeats_allowed(&self) -> bool {
        !matches!(self, Self::PairingRequired)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncLine {
    pub sync: &'static str,
    pub ok: bool,
    /// Always the upload state's code; a failed step's cause goes in `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicates: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
    /// The server was restored from a backup; every event is queued again (D031).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resync: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientScan {
    pub detected: bool,
    pub last_scan_events: Option<u64>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStatus {
    pub last_scan_at: Option<i64>,
    pub last_scan_duration_ms: Option<u64>,
    pub clients: BTreeMap<String, ClientScan>,
}

pub type SharedScan = Arc<Mutex<ScanStatus>>;

pub fn collector_info() -> CollectorInfo {
    CollectorInfo {
        version: COLLECTOR_VERSION.to_owned(),
        tokscale_rev: crate::TOKSCALE_REV[..8].to_owned(),
        parser_generation: crate::PARSER_GENERATION.to_owned(),
        qoder_rate_version: crate::qoder::rate_version(),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn new_batch_id() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Uniqueness only matters for log correlation on the server.
        bytes[..8].copy_from_slice(&now_ms().to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn os_name() -> &'static str {
    crate::credentials::this_platform().as_str()
}

#[cfg(target_os = "macos")]
pub fn os_version() -> Option<String> {
    let mut buffer = [0_u8; 64];
    let mut length = buffer.len();
    // SAFETY: the buffer and length describe valid writable memory.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    let text = std::str::from_utf8(buffer.get(..length)?).ok()?;
    let text = text.trim_end_matches('\0').trim();
    (!text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.'))
    .then(|| text.to_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn os_version() -> Option<String> {
    None
}

fn is_client_id(client: &str) -> bool {
    !client.is_empty()
        && client.len() <= 64
        && client.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

enum Step {
    Sent(SyncLine),
    Shrunk,
    Stop(SyncLine),
}

/// Upload and heartbeat logic, driven by the background thread (or tests).
pub struct Uploader {
    state: State,
    transport: Box<dyn Transport>,
    credentials: Arc<Credentials>,
    generation: String,
    scan: SharedScan,
    timing: SyncTiming,
    upload_state: UploadState,
    /// Last code written to the state file, so `status` sees the latest one.
    persisted_state: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
    batch_limit: usize,
    resync: Arc<AtomicBool>,
}

/// Server store ids are random lowercase hex; anything else is ignored.
fn is_store_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

impl Uploader {
    pub fn new(
        state: State,
        transport: Box<dyn Transport>,
        credentials: Arc<Credentials>,
        scan: SharedScan,
        timing: SyncTiming,
    ) -> Self {
        let persisted_state = state.meta(meta::UPLOAD_STATE).ok().flatten();
        Self {
            state,
            transport,
            credentials,
            generation: crate::PARSER_GENERATION.to_owned(),
            scan,
            timing,
            upload_state: UploadState::Ok,
            persisted_state,
            failures: 0,
            retry_at: None,
            batch_limit: envelope::MAX_EVENTS_PER_BATCH,
            resync: Arc::default(),
        }
    }

    /// Raised when a restored server needs every event again; the watch loop
    /// clears it and rebuilds its snapshot.
    pub fn resync_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.resync)
    }

    fn observe_store(&mut self, store_id: Option<&str>) -> Option<bool> {
        let id = store_id.filter(|id| is_store_id(id))?;
        let changed = self.state.adopt_store(id).ok()?;
        if changed {
            self.resync.store(true, Ordering::SeqCst);
        }
        changed.then_some(true)
    }

    pub fn upload_state(&self) -> &UploadState {
        &self.upload_state
    }

    pub fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    pub fn backlog(&self) -> u64 {
        self.state.backlog().unwrap_or(0)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.credentials.server_url)
    }

    fn line(&self, sync: &'static str, ok: bool) -> SyncLine {
        SyncLine {
            sync,
            ok,
            state: Some(self.upload_state.code()),
            ..SyncLine::default()
        }
    }

    fn set_state(&mut self, next: UploadState) {
        let code = next.code();
        if self.persisted_state.as_deref() != Some(code.as_str())
            && self.state.set_meta(meta::UPLOAD_STATE, &code).is_ok()
        {
            self.persisted_state = Some(code);
        }
        self.upload_state = next;
    }

    fn backoff(&mut self, code: &str, now: Instant) -> Step {
        self.failures = self.failures.saturating_add(1);
        let exponent = self.failures.saturating_sub(1).min(16);
        let base = self
            .timing
            .backoff_base
            .saturating_mul(1 << exponent)
            .min(self.timing.backoff_max);
        let jitter = 750 + getrandom::u32().unwrap_or(0) % 501;
        let delay = base.mul_f64(f64::from(jitter) / 1000.0);
        self.retry_at = Some(now + delay);
        self.set_state(UploadState::Backoff(code.to_owned()));
        let mut line = self.line("upload", false);
        line.retry_in_ms = Some(millis(delay));
        Step::Stop(line)
    }

    fn check_version(&mut self, minimum: &str) {
        if transport::version_below(COLLECTOR_VERSION, minimum) {
            self.set_state(UploadState::UpgradeRequired);
        }
    }

    /// Sends batches until the outbox is empty or a request fails.
    pub fn upload_round(&mut self, now: Instant) -> Vec<SyncLine> {
        let mut lines = Vec::new();
        while self.upload_state.uploads_allowed() && self.retry_at.is_none_or(|at| now >= at) {
            let batch = match self.state.next_batch(self.batch_limit) {
                Ok(Some(batch)) => batch,
                Ok(None) => break,
                Err(_) => {
                    let mut line = self.line("upload", false);
                    line.error = Some("state_read_failed".to_owned());
                    lines.push(line);
                    break;
                }
            };
            match self.send(&batch, now) {
                Step::Sent(line) => lines.push(line),
                Step::Shrunk => {}
                Step::Stop(line) => {
                    lines.push(line);
                    break;
                }
            }
        }
        lines
    }

    fn shrink(&mut self, sent: usize) -> Option<Step> {
        (sent > 1).then(|| {
            self.batch_limit = (sent / 2).max(1);
            Step::Shrunk
        })
    }

    fn send(&mut self, batch: &Batch, now: Instant) -> Step {
        let sent = batch.rows.len();
        let events = batch
            .rows
            .iter()
            // A corrupt row is sent as-is; the server rejects it and it is acked.
            .map(|row| serde_json::from_str(&row.payload).unwrap_or(serde_json::Value::Null))
            .collect();
        let body = IngestEnvelope {
            schema_version: envelope::SCHEMA_VERSION,
            device_id: self.credentials.device_id.clone(),
            batch_id: new_batch_id(),
            collector: collector_info(),
            scan_generation: batch.generation.clone(),
            events,
        };
        let Ok(raw) = serde_json::to_vec(&body) else {
            self.set_state(UploadState::Blocked("encode".to_owned()));
            return Step::Stop(self.line("upload", false));
        };
        if raw.len() > MAX_RAW_BATCH_BYTES
            && let Some(step) = self.shrink(sent)
        {
            return step;
        }
        let packed = transport::gzip(&raw);
        if packed.len() > envelope::MAX_BODY_BYTES
            && let Some(step) = self.shrink(sent)
        {
            return step;
        }
        let url = self.url("/api/ingest/events");
        let token = self.credentials.ingest_token.clone();
        let response = match self.transport.post(&url, Some(&token), true, &packed) {
            Ok(response) => response,
            Err(_) => return self.backoff("network", now),
        };
        match response.status {
            200 => {
                let Ok(reply) = serde_json::from_slice::<IngestResponse>(&response.body) else {
                    return self.backoff("bad_response", now);
                };
                let rejected = reply.rejected.len() as u64;
                let counted = reply.accepted + reply.updated + reply.duplicates + rejected;
                if counted != sent as u64 || reply.rejected.iter().any(|item| item.index >= sent) {
                    return self.backoff("bad_response", now);
                }
                if self.state.ack(&batch.rows).is_err() {
                    let mut line = self.line("upload", false);
                    line.error = Some("state_write_failed".to_owned());
                    return Step::Stop(line);
                }
                let _ = self.state.add_rejected(rejected);
                let _ = self
                    .state
                    .set_meta(meta::LAST_UPLOAD_AT, &now_ms().to_string());
                self.failures = 0;
                self.retry_at = None;
                self.set_state(UploadState::Ok);
                self.check_version(&reply.min_collector_version);
                let resync = self.observe_store(reply.store_id.as_deref());
                let mut line = self.line("upload", true);
                line.resync = resync;
                line.events = Some(sent as u64);
                line.accepted = Some(reply.accepted);
                line.updated = Some(reply.updated);
                line.duplicates = Some(reply.duplicates);
                line.rejected = Some(rejected);
                line.backlog = Some(self.backlog());
                Step::Sent(line)
            }
            401 => {
                self.set_state(UploadState::PairingRequired);
                Step::Stop(self.line("upload", false))
            }
            413 => {
                if let Some(step) = self.shrink(sent) {
                    return step;
                }
                self.set_state(UploadState::Blocked("http_413".to_owned()));
                Step::Stop(self.line("upload", false))
            }
            429 => {
                let delay = response.retry_after.unwrap_or(DEFAULT_RETRY_AFTER);
                self.retry_at = Some(now + delay);
                self.set_state(UploadState::Backoff("rate_limited".to_owned()));
                let mut line = self.line("upload", false);
                line.retry_in_ms = Some(millis(delay));
                Step::Stop(line)
            }
            400 | 403 | 415 => {
                let code = transport::error_code(&response.body)
                    .unwrap_or_else(|| format!("http_{}", response.status));
                self.set_state(UploadState::Blocked(code));
                Step::Stop(self.line("upload", false))
            }
            status => self.backoff(&format!("http_{status}"), now),
        }
    }

    pub fn build_heartbeat(&self) -> Result<Heartbeat, StateError> {
        let scan = self
            .scan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut clients = scan
            .clients
            .into_iter()
            .map(|(client, status)| {
                let entry = ClientStatus {
                    client: client.clone(),
                    detected: status.detected,
                    last_event_at: None,
                    last_scan_events: status.last_scan_events,
                    error_code: status.error_code,
                };
                (client, entry)
            })
            .collect::<BTreeMap<_, _>>();
        for stats in self.state.client_stats()? {
            let entry = clients
                .entry(stats.client.clone())
                .or_insert_with(|| ClientStatus {
                    client: stats.client.clone(),
                    detected: true,
                    last_event_at: None,
                    last_scan_events: None,
                    error_code: None,
                });
            entry.last_event_at = Some(stats.last_event_at);
        }
        Ok(Heartbeat {
            schema_version: envelope::SCHEMA_VERSION,
            device_id: self.credentials.device_id.clone(),
            collector: collector_info(),
            os_name: Some(os_name().to_owned()),
            os_version: os_version(),
            last_scan_at: scan.last_scan_at,
            last_scan_duration_ms: scan.last_scan_duration_ms,
            outbox_backlog: self.state.backlog()?,
            backfill_complete: self.state.backfill_complete(&self.generation)?,
            clients: clients
                .into_values()
                .filter(|status| is_client_id(&status.client))
                .take(MAX_HEARTBEAT_CLIENTS)
                .collect(),
        })
    }

    pub fn heartbeat(&mut self) -> SyncLine {
        if !self.upload_state.heartbeats_allowed() {
            return self.line("heartbeat", false);
        }
        let Ok(heartbeat) = self.build_heartbeat() else {
            let mut line = self.line("heartbeat", false);
            line.error = Some("state_read_failed".to_owned());
            return line;
        };
        let backlog = heartbeat.outbox_backlog;
        let Ok(body) = serde_json::to_vec(&heartbeat) else {
            return self.line("heartbeat", false);
        };
        let url = self.url("/api/ingest/heartbeat");
        let token = self.credentials.ingest_token.clone();
        let mut line = match self.transport.post(&url, Some(&token), false, &body) {
            Ok(response) if response.status == 200 => {
                let mut resync = None;
                if let Ok(reply) = serde_json::from_slice::<HeartbeatResponse>(&response.body) {
                    self.check_version(&reply.min_collector_version);
                    resync = self.observe_store(reply.store_id.as_deref());
                }
                let _ = self
                    .state
                    .set_meta(meta::LAST_HEARTBEAT_AT, &now_ms().to_string());
                let mut line = self.line("heartbeat", true);
                line.resync = resync;
                line
            }
            Ok(response) if response.status == 401 => {
                self.set_state(UploadState::PairingRequired);
                self.line("heartbeat", false)
            }
            Ok(response) => {
                let mut line = self.line("heartbeat", false);
                line.error = Some(format!("http_{}", response.status));
                line
            }
            Err(_) => {
                let mut line = self.line("heartbeat", false);
                line.error = Some("network".to_owned());
                line
            }
        };
        line.backlog = Some(backlog);
        line
    }

    fn run(mut self, receiver: Receiver<Wake>, sink: LineSink) {
        let timing = self.timing;
        let mut pending = true;
        let mut last_round: Option<Instant> = None;
        let mut next_heartbeat = Some(Instant::now() + timing.heartbeat_interval);
        loop {
            let now = Instant::now();
            let upload_at = (pending && self.upload_state.uploads_allowed()).then(|| {
                let gap = last_round.map_or(now, |at| at + timing.min_round_gap);
                self.retry_at.map_or(gap, |retry| retry.max(gap))
            });
            let heartbeat_at = next_heartbeat.filter(|_| self.upload_state.heartbeats_allowed());
            let deadline = [upload_at, heartbeat_at].into_iter().flatten().min();
            let message = match deadline {
                Some(at) if at <= now => Err(RecvTimeoutError::Timeout),
                Some(at) => receiver.recv_timeout(at - now),
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(Wake::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Wake::Data) => pending = true,
                Ok(Wake::Heartbeat) => next_heartbeat = Some(now),
                Err(RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if upload_at.is_some_and(|at| at <= now) {
                        let lines = self.upload_round(now);
                        pending = self.backlog() > 0;
                        // Only a round that sent something delays the next one.
                        if !lines.is_empty() {
                            last_round = Some(now);
                            next_heartbeat = Some(now);
                        }
                        for line in lines {
                            sink(line);
                        }
                    }
                    if self.upload_state.heartbeats_allowed()
                        && next_heartbeat.is_some_and(|at| at <= Instant::now())
                    {
                        sink(self.heartbeat());
                        next_heartbeat = Some(Instant::now() + timing.heartbeat_interval);
                    }
                }
            }
        }
    }
}

enum Wake {
    Data,
    Heartbeat,
    Stop,
}

pub type LineSink = Box<dyn Fn(SyncLine) + Send>;

pub struct Recorded {
    pub line: SyncLine,
    /// Keys the snapshot must forget so a later scan offers them again.
    pub forget: Vec<String>,
}

/// Watch-loop side: records scans and wakes the upload thread.
pub struct Syncer {
    state: State,
    generation: String,
    scan: SharedScan,
    wake: Sender<Wake>,
    thread: Option<JoinHandle<()>>,
    full_scans: u64,
    resync: Arc<AtomicBool>,
    pub prepared: Prepared,
    _lock: Arc<File>,
}

impl Syncer {
    /// Fails with [`StateError::Busy`] while another process uploads from `state_dir`.
    pub fn start(
        state_dir: &Path,
        credentials: Credentials,
        transport: Box<dyn Transport>,
        timing: SyncTiming,
        sink: LineSink,
    ) -> Result<Self, StateError> {
        let lock = Arc::new(state::lock_queue(state_dir)?);
        Self::start_with_queue_lock(state_dir, credentials, transport, timing, sink, lock)
    }

    /// Starts under a queue lock already held by the app. The caller must
    /// obtain this guard from `state::lock_queue(state_dir)` and may keep a
    /// clone so a failed server queue never releases the app/CLI exclusion.
    pub fn start_with_queue_lock(
        state_dir: &Path,
        credentials: Credentials,
        transport: Box<dyn Transport>,
        timing: SyncTiming,
        sink: LineSink,
        lock: Arc<File>,
    ) -> Result<Self, StateError> {
        let mut state = State::open(state_dir)?;
        let prepared = state.prepare(&credentials.device_id, crate::PARSER_GENERATION)?;
        let scan = SharedScan::default();
        let uploader = Uploader::new(
            State::open(state_dir)?,
            transport,
            Arc::new(credentials),
            scan.clone(),
            timing,
        );
        let resync = uploader.resync_flag();
        let (wake, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("tokenlens-upload".to_owned())
            .spawn(move || uploader.run(receiver, sink))?;
        Ok(Self {
            state,
            generation: crate::PARSER_GENERATION.to_owned(),
            scan,
            wake,
            thread: Some(thread),
            full_scans: 0,
            resync,
            prepared,
            _lock: lock,
        })
    }

    /// True once after the server reported a new store id.
    pub fn take_resync(&self) -> bool {
        self.resync.swap(false, Ordering::SeqCst)
    }

    pub fn seed(&self, snapshot: &mut Snapshot) -> Result<usize, StateError> {
        self.state
            .for_each_known(|key, client, ts, hash| snapshot.seed(key, client, ts, hash))
    }

    /// Selects changes this destination has not durably queued yet. Used when
    /// one scan feeds multiple destinations with different upload histories.
    pub fn filter_unknown(
        &self,
        changed: &[ScannedEvent],
    ) -> Result<Vec<ScannedEvent>, StateError> {
        let mut needed = Vec::new();
        for event in changed {
            if self.state.known_hash(&event.key)? != Some(event.hash) {
                needed.push(event.clone());
            }
        }
        Ok(needed)
    }

    pub fn record_scan(
        &mut self,
        full: bool,
        counts: &BTreeMap<String, u64>,
        changed: Vec<ScannedEvent>,
        duration: Duration,
    ) -> Recorded {
        let now = now_ms();
        let (line, forget, queued) =
            match self.state.record_changes(&self.generation, &changed, now) {
                Ok(queued) => {
                    if full {
                        let _ = self
                            .state
                            .set_meta(meta::FULL_SCAN_GENERATION, &self.generation);
                    }
                    let forget = changed
                        .iter()
                        .filter(|event| event.payload.is_none())
                        .map(|event| event.key.clone())
                        .collect();
                    let line = SyncLine {
                        sync: "queued",
                        ok: true,
                        events: Some(queued),
                        backlog: self.state.backlog().ok(),
                        ..SyncLine::default()
                    };
                    (line, forget, queued)
                }
                Err(_) => {
                    let line = SyncLine {
                        sync: "queued",
                        ok: false,
                        error: Some("state_write_failed".to_owned()),
                        ..SyncLine::default()
                    };
                    (
                        line,
                        changed.into_iter().map(|event| event.key).collect(),
                        0,
                    )
                }
            };
        {
            let mut scan = self.scan.lock().unwrap_or_else(PoisonError::into_inner);
            scan.last_scan_at = Some(now);
            scan.last_scan_duration_ms = Some(millis(duration));
            for (client, count) in counts {
                let entry = scan.clients.entry(client.clone()).or_default();
                entry.detected = if full {
                    *count > 0
                } else {
                    entry.detected || *count > 0
                };
                entry.last_scan_events = Some(*count);
                entry.error_code = None;
            }
        }
        if queued > 0 {
            let _ = self.wake.send(Wake::Data);
        }
        if full {
            self.full_scans += 1;
            if self.full_scans == 1 {
                let _ = self.wake.send(Wake::Heartbeat);
            }
        }
        Recorded { line, forget }
    }

    pub fn record_failure(&mut self, scope: &Scope, code: &str) {
        let mut scan = self.scan.lock().unwrap_or_else(PoisonError::into_inner);
        let clients: Vec<String> = match &scope.clients {
            Some(clients) => clients.iter().cloned().collect(),
            None => scan.clients.keys().cloned().collect(),
        };
        for client in clients {
            scan.clients.entry(client).or_default().error_code = Some(code.to_owned());
        }
    }

    /// Stops the upload thread; an in-flight request may take up to its timeout.
    pub fn stop(mut self) {
        let _ = self.wake.send(Wake::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Numbers and codes for `collector status`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStatus {
    pub outbox_backlog: u64,
    pub backfill_complete: bool,
    pub upload_state: Option<String>,
    pub last_upload_at: Option<i64>,
    pub last_heartbeat_at: Option<i64>,
    pub rejected_total: i64,
    pub known_events: u64,
}

pub fn local_status(state_dir: &Path) -> Result<Option<LocalStatus>, StateError> {
    if !State::exists_in(state_dir) {
        return Ok(None);
    }
    let state = State::open(state_dir)?;
    Ok(Some(LocalStatus {
        outbox_backlog: state.backlog()?,
        backfill_complete: state.backfill_complete(crate::PARSER_GENERATION)?,
        upload_state: state.meta(meta::UPLOAD_STATE)?,
        last_upload_at: state.meta_i64(meta::LAST_UPLOAD_AT)?,
        last_heartbeat_at: state.meta_i64(meta::LAST_HEARTBEAT_AT)?,
        rejected_total: state.meta_i64(meta::REJECTED_TOTAL)?.unwrap_or(0),
        known_events: state.client_stats()?.iter().map(|stats| stats.events).sum(),
    }))
}

pub use state::default_dir;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpResponse;
    use crate::transport::fake::FakeTransport;
    use serde_json::json;

    fn credentials() -> Arc<Credentials> {
        Arc::new(Credentials {
            server_url: "http://127.0.0.1:9".into(),
            device_id: "dev_synthetic".into(),
            ingest_token: "tdi_synthetic".into(),
            read_token: "tdr_synthetic".into(),
        })
    }

    fn event(index: u32) -> ScannedEvent {
        ScannedEvent {
            key: format!("k{index:04}"),
            client: "codex".into(),
            ts: i64::from(index),
            hash: [1; 32],
            payload: Some(format!("{{\"eventKey\":\"k{index:04}\"}}")),
        }
    }

    struct Setup {
        uploader: Uploader,
        fake: FakeTransport,
        _dir: tempfile::TempDir,
    }

    fn setup(events: u32) -> Setup {
        let dir = tempfile::tempdir().expect("dir");
        let mut state = State::open(dir.path()).expect("open");
        state
            .prepare("dev_synthetic", crate::PARSER_GENERATION)
            .expect("prepare");
        let changed = (0..events).map(event).collect::<Vec<_>>();
        state
            .record_changes(crate::PARSER_GENERATION, &changed, 1)
            .expect("record");
        let fake = FakeTransport::default();
        let uploader = Uploader::new(
            state,
            Box::new(fake.clone()),
            credentials(),
            SharedScan::default(),
            SyncTiming::default(),
        );
        Setup {
            uploader,
            fake,
            _dir: dir,
        }
    }

    fn ok_reply(accepted: u64, rejected: &[usize], min: &str) -> serde_json::Value {
        json!({
            "accepted": accepted, "updated": 0, "duplicates": 0,
            "rejected": rejected.iter().map(|index| json!({"index": index, "reason": "invalid_event"})).collect::<Vec<_>>(),
            "minCollectorVersion": min, "minAppVersion": "0.1.0"
        })
    }

    fn gunzip(bytes: &[u8]) -> serde_json::Value {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        serde_json::from_reader(&mut decoder).expect("gzip json")
    }

    fn known(uploader: &Uploader) -> usize {
        uploader
            .state
            .for_each_known(|_, _, _, _| {})
            .expect("known")
    }

    #[test]
    fn a_changed_server_store_id_queues_everything_again() {
        let mut setup = setup(2);
        let resync = setup.uploader.resync_flag();
        let mut reply = ok_reply(2, &[], "0.1.0");
        reply["storeId"] = json!("aaaa");
        setup.fake.push(200, reply);
        let lines = setup.uploader.upload_round(Instant::now());
        assert_eq!(lines[0].resync, None);
        assert!(!resync.load(Ordering::SeqCst));
        assert_eq!(known(&setup.uploader), 2);
        // Missing, malformed or unchanged ids change nothing.
        for body in [
            json!({"ok": true, "minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0"}),
            json!({"ok": true, "minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0", "storeId": "../x"}),
            json!({"ok": true, "minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0", "storeId": "aaaa"}),
        ] {
            setup.fake.push(200, body);
            let line = setup.uploader.heartbeat();
            assert_eq!((line.ok, line.resync), (true, None));
        }
        assert_eq!(known(&setup.uploader), 2);
        setup.fake.push(
            200,
            json!({"ok": true, "minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0", "storeId": "bbbb"}),
        );
        let line = setup.uploader.heartbeat();
        assert_eq!((line.ok, line.resync), (true, Some(true)));
        assert!(resync.swap(false, Ordering::SeqCst));
        assert_eq!(known(&setup.uploader), 0);
        assert_eq!(
            setup
                .uploader
                .state
                .meta(meta::SERVER_STORE_ID)
                .expect("meta")
                .as_deref(),
            Some("bbbb")
        );
    }

    #[test]
    fn successful_round_acks_everything_including_rejected() {
        let mut setup = setup(3);
        setup.fake.push(200, ok_reply(2, &[1], "0.1.0"));
        let lines = setup.uploader.upload_round(Instant::now());
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (
                lines[0].ok,
                lines[0].accepted,
                lines[0].rejected,
                lines[0].backlog
            ),
            (true, Some(2), Some(1), Some(0))
        );
        let sent = setup.fake.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].gzip);
        assert_eq!(sent[0].bearer.as_deref(), Some("tdi_synthetic"));
        assert_eq!(sent[0].url, "http://127.0.0.1:9/api/ingest/events");
        let body = gunzip(&sent[0].body);
        assert_eq!(body["deviceId"], "dev_synthetic");
        assert_eq!(body["scanGeneration"], crate::PARSER_GENERATION);
        assert_eq!(body["events"].as_array().map(Vec::len), Some(3));
        assert_eq!(body["batchId"].as_str().map(str::len), Some(32));
        assert_eq!(
            setup
                .uploader
                .state
                .meta_i64(meta::REJECTED_TOTAL)
                .expect("meta"),
            Some(1)
        );
        assert_eq!(*setup.uploader.upload_state(), UploadState::Ok);
        // Nothing left: the next round sends nothing.
        assert!(setup.uploader.upload_round(Instant::now()).is_empty());
    }

    #[test]
    fn mismatched_counts_and_server_errors_back_off_and_keep_rows() {
        let mut setup = setup(2);
        setup.fake.push(200, ok_reply(1, &[], "0.1.0"));
        let now = Instant::now();
        let lines = setup.uploader.upload_round(now);
        assert_eq!(lines[0].state.as_deref(), Some("backoff:bad_response"));
        let first = setup.uploader.retry_at().expect("retry") - now;
        assert!(first >= Duration::from_millis(22_500) && first <= Duration::from_millis(37_500));
        assert!(setup.uploader.upload_round(now).is_empty(), "still waiting");
        let later = now + Duration::from_secs(3600);
        setup.fake.push(503, json!({"error": "internal"}));
        let lines = setup.uploader.upload_round(later);
        assert_eq!(lines[0].state.as_deref(), Some("backoff:http_503"));
        let second = setup.uploader.retry_at().expect("retry") - later;
        assert!(second >= Duration::from_millis(45_000));
        let lines = setup
            .uploader
            .upload_round(later + Duration::from_secs(3600));
        assert_eq!(lines[0].state.as_deref(), Some("backoff:network"));
        assert_eq!(setup.uploader.backlog(), 2);
    }

    #[test]
    fn a_failed_heartbeat_keeps_the_upload_state() {
        let mut setup = setup(1);
        setup.uploader.upload_round(Instant::now());
        let line = setup.uploader.heartbeat();
        assert!(!line.ok);
        assert_eq!(line.state.as_deref(), Some("backoff:network"));
        assert_eq!(line.error.as_deref(), Some("network"));
        setup.fake.push(500, json!({}));
        let line = setup.uploader.heartbeat();
        assert_eq!(line.state.as_deref(), Some("backoff:network"));
        assert_eq!(line.error.as_deref(), Some("http_500"));
    }

    #[test]
    fn rate_limit_waits_for_retry_after() {
        let mut setup = setup(1);
        setup.fake.push_raw(Ok(HttpResponse {
            status: 429,
            retry_after: Some(Duration::from_secs(7)),
            body: Vec::new(),
        }));
        let now = Instant::now();
        let lines = setup.uploader.upload_round(now);
        assert_eq!(lines[0].retry_in_ms, Some(7000));
        assert_eq!(lines[0].state.as_deref(), Some("backoff:rate_limited"));
        assert_eq!(
            setup.uploader.retry_at(),
            Some(now + Duration::from_secs(7))
        );
    }

    #[test]
    fn too_large_halves_the_batch() {
        let mut four = setup(4);
        four.fake.push(413, json!({"error": "payload_too_large"}));
        four.fake.push(200, ok_reply(2, &[], "0.1.0"));
        four.fake.push(200, ok_reply(2, &[], "0.1.0"));
        let lines = four.uploader.upload_round(Instant::now());
        assert_eq!(lines.len(), 2);
        let sizes = four
            .fake
            .sent()
            .iter()
            .map(|sent| gunzip(&sent.body)["events"].as_array().map_or(0, Vec::len))
            .collect::<Vec<_>>();
        assert_eq!(sizes, [4, 2, 2]);
        let mut single = setup(1);
        single.fake.push(413, json!({}));
        single.uploader.upload_round(Instant::now());
        assert_eq!(
            *single.uploader.upload_state(),
            UploadState::Blocked("http_413".into())
        );
    }

    #[test]
    fn unauthorized_requires_pairing_and_stops_heartbeats() {
        let mut setup = setup(1);
        setup.fake.push(401, json!({"error": "unauthorized"}));
        setup.uploader.upload_round(Instant::now());
        assert_eq!(*setup.uploader.upload_state(), UploadState::PairingRequired);
        assert!(setup.uploader.upload_round(Instant::now()).is_empty());
        let line = setup.uploader.heartbeat();
        assert!(!line.ok);
        assert_eq!(setup.fake.sent().len(), 1, "no request after 401");
    }

    #[test]
    fn bad_request_blocks_uploads_but_not_heartbeats() {
        let mut setup = setup(1);
        setup.fake.push(400, json!({"error": "bad_request"}));
        setup.uploader.upload_round(Instant::now());
        assert_eq!(
            *setup.uploader.upload_state(),
            UploadState::Blocked("bad_request".into())
        );
        setup.fake.push(
            200,
            json!({"minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0"}),
        );
        assert!(setup.uploader.heartbeat().ok);
        assert_eq!(setup.uploader.backlog(), 1);
    }

    #[test]
    fn newer_minimum_version_requires_upgrade() {
        let mut setup = setup(2);
        setup.fake.push(200, ok_reply(2, &[], "99.0.0"));
        setup.uploader.upload_round(Instant::now());
        assert_eq!(*setup.uploader.upload_state(), UploadState::UpgradeRequired);
        assert_eq!(setup.uploader.backlog(), 0, "the accepted batch is acked");
    }

    #[test]
    fn first_changes_after_start_upload_without_waiting_for_the_gap() {
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeTransport::default();
        fake.push(200, ok_reply(1, &[], "0.1.0"));
        fake.push(
            200,
            json!({"minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0"}),
        );
        let (lines, received) = mpsc::channel();
        let mut syncer = Syncer::start(
            dir.path(),
            (*credentials()).clone(),
            Box::new(fake.clone()),
            SyncTiming::default(),
            Box::new(move |line| {
                let _ = lines.send(line);
            }),
        )
        .expect("start");
        std::thread::sleep(Duration::from_millis(100));
        let counts = BTreeMap::from([("codex".to_owned(), 1)]);
        let recorded = syncer.record_scan(false, &counts, vec![event(1)], Duration::ZERO);
        assert_eq!(recorded.line.events, Some(1));
        let line = received
            .recv_timeout(Duration::from_secs(5))
            .expect("upload line");
        assert_eq!((line.sync, line.accepted), ("upload", Some(1)));
        syncer.stop();
    }

    #[test]
    fn a_second_uploader_on_the_same_queue_is_refused_until_the_first_stops() {
        let dir = tempfile::tempdir().expect("dir");
        let start = || {
            Syncer::start(
                dir.path(),
                (*credentials()).clone(),
                Box::new(FakeTransport::default()),
                SyncTiming::default(),
                Box::new(|_| {}),
            )
        };
        let first = start().expect("first");
        assert!(matches!(start(), Err(StateError::Busy)));
        assert!(
            local_status(dir.path()).expect("status").is_some(),
            "status still reads the queue"
        );
        first.stop();
        start().expect("after the first stopped").stop();
    }

    #[test]
    fn heartbeat_reports_numbers_only() {
        let mut setup = setup(2);
        {
            let mut scan = setup.uploader.scan.lock().expect("lock");
            scan.last_scan_at = Some(5);
            scan.clients.insert(
                "zcode".into(),
                ClientScan {
                    detected: false,
                    last_scan_events: Some(0),
                    error_code: None,
                },
            );
            scan.clients
                .insert("Bad Client".into(), ClientScan::default());
        }
        let heartbeat = setup.uploader.build_heartbeat().expect("heartbeat");
        assert_eq!(heartbeat.outbox_backlog, 2);
        assert!(!heartbeat.backfill_complete);
        assert_eq!(heartbeat.os_name.as_deref(), Some(os_name()));
        let clients = heartbeat
            .clients
            .iter()
            .map(|client| {
                (
                    client.client.as_str(),
                    client.detected,
                    client.last_event_at,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(clients, [("codex", true, Some(1)), ("zcode", false, None)]);
        setup.fake.push(
            200,
            json!({"minCollectorVersion": "0.1.0", "minAppVersion": "0.1.0"}),
        );
        assert!(setup.uploader.heartbeat().ok);
        let sent = setup.fake.sent();
        assert!(!sent[0].gzip);
        assert_eq!(sent[0].url, "http://127.0.0.1:9/api/ingest/heartbeat");
    }
}
