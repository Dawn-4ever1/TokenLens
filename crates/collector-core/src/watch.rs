//! Foreground `watch` loop: notify-based change detection, scan scheduling,
//! short-lived worker processes, and the in-memory snapshot.
//!
//! Output is numbers only: no path, key, title, or content is ever printed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, PoisonError, RwLock, RwLockWriteGuard};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;

use crate::DateRange;
use crate::roots::{RootMap, WatchRoot, watch_roots};
use crate::schedule::{ScanKind, Scheduler, Timing};
use crate::snapshot::{MergeStats, ScannedEvent, Scope, Snapshot};
use crate::state::{Prepared, StateError};
use crate::sync::{Recorded, SyncLine, Syncer};
use crate::worker::{self, KeepPayload, WorkerFailure, WorkerOutput};

/// Agent root overrides read by the locked upstream rev (clients.rs `PathRoot::EnvVar`).
/// They are removed from worker environments when an explicit home is used.
pub const AGENT_ROOT_ENV_VARS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GEMINI_CLI_HOME",
    "HERMES_HOME",
    "CODEBUFF_DATA_DIR",
    "GJC_CODING_AGENT_DIR",
    "GROK_HOME",
    "JCODE_HOME",
    "SENPI_CODING_AGENT_DIR",
    "KIMCHI_CODING_AGENT_DIR",
    "PRIME_AGENT_CODING_AGENT_DIR",
    "FREEBUFF_DATA_DIR",
    "DSH_HOME",
    "LM_STUDIO_HOME",
    "UNSLOTH_STUDIO_HOME",
    "HINDSIGHT_HOME",
    "VIBE_HOME",
    "BB_DATA_DIR",
    "QODER_CONFIG_DIR",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
];

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub home: PathBuf,
    /// `true` passes `--home` to workers and isolates their environment.
    pub explicit_home: bool,
    pub use_env_roots: bool,
    pub worker_program: PathBuf,
    pub timing: Timing,
    pub worker_timeout: Duration,
    pub exit_after: Option<Duration>,
    /// This project's own directories; changes there never trigger scans.
    pub ignored: Vec<PathBuf>,
    /// Upstream persistent cache directory for workers (`TOKSCALE_CONFIG_DIR`).
    /// `None` keeps the uncached upstream API.
    pub upstream_cache_dir: Option<PathBuf>,
}

pub enum Msg {
    Change(String),
    WatchError,
    Done {
        kind: ScanKind,
        scope: Scope,
        result: Result<WorkerOutput, WorkerFailure>,
    },
    /// A line from the upload thread, printed by the watch loop.
    Sync(SyncLine),
    Stop,
}

pub fn channel() -> (Sender<Msg>, Receiver<Msg>) {
    mpsc::channel()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSummary {
    pub snapshot_events: usize,
    pub key_set_sha256: String,
    pub value_sha256: String,
    pub full_scans: u64,
    pub triggered_scans: u64,
    pub poll_scans: u64,
    pub failures: u64,
    pub vanished: u64,
    pub watched_roots: usize,
    pub watch_errors: u64,
    pub change_events: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanLine<'a> {
    scan: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    clients: usize,
    events: usize,
    #[serde(flatten)]
    merge: MergeStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached: Option<bool>,
    wall_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peak_rss_kb: Option<u64>,
    stderr_lines: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeedLine {
    state: &'static str,
    ok: bool,
    known: usize,
    device_changed: bool,
    generation_changed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootsLine {
    watch: &'static str,
    roots: usize,
    ok: bool,
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn local_today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

fn scope_for(kind: &ScanKind) -> Scope {
    match kind {
        ScanKind::Full => Scope {
            clients: None,
            range: DateRange::all_history(),
        },
        ScanKind::Triggered { clients, .. } => {
            let today = local_today();
            Scope {
                clients: Some(clients.clone()),
                range: DateRange {
                    since: today.pred_opt(),
                    until: Some(today),
                },
            }
        }
    }
}

/// The exact worker command for one scan.
pub fn worker_command(options: &WatchOptions, scope: &Scope) -> Command {
    let mut command = Command::new(&options.worker_program);
    command.arg("scan-worker");
    if options.explicit_home {
        command.arg("--home").arg(&options.home);
        // Keep workers on the given home even if upstream consults HOME.
        command.env("HOME", &options.home);
        command.env("USERPROFILE", &options.home);
        for name in AGENT_ROOT_ENV_VARS {
            command.env_remove(name);
        }
    }
    if !options.use_env_roots {
        command.arg("--no-env-roots");
    }
    if let Some(clients) = &scope.clients {
        let list = clients.iter().cloned().collect::<Vec<_>>().join(",");
        command.arg("--clients").arg(list);
    }
    if let Some(since) = scope.range.since {
        command.arg("--since").arg(since.to_string());
    }
    if let Some(until) = scope.range.until {
        command.arg("--until").arg(until.to_string());
    }
    // Upstream's recovery overlay reads a ledger from its shared config dir;
    // workers must only ever see raw agent logs.
    command.env("TOKSCALE_RECOVERY_DISABLE", "1");
    match &options.upstream_cache_dir {
        Some(dir) => {
            command.arg("--upstream-cache");
            command.env("TOKSCALE_CONFIG_DIR", dir);
        }
        None => {
            command.env_remove("TOKSCALE_CONFIG_DIR");
        }
    }
    worker::configure_priority(&mut command);
    command
}

/// Environment the worker would receive, for tests (names and values).
pub fn worker_env(command: &Command) -> Vec<(OsString, Option<OsString>)> {
    command
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
        .collect()
}

fn build_watcher(
    roots: &[WatchRoot],
    ignored: &[PathBuf],
    sender: Sender<Msg>,
) -> Result<RecommendedWatcher, ()> {
    let map = RootMap::new(roots, ignored);
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        match result {
            Ok(event) => {
                // Reads (including our own workers') must never trigger scans.
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let clients = event
                    .paths
                    .iter()
                    .filter_map(|path| map.client_for(path))
                    .collect::<BTreeSet<_>>();
                for client in clients {
                    let _ = sender.send(Msg::Change(client.to_owned()));
                }
            }
            Err(_) => {
                let _ = sender.send(Msg::WatchError);
            }
        }
    })
    .map_err(|_| ())?;
    for root in roots {
        watcher
            .watch(&root.path, RecursiveMode::Recursive)
            .map_err(|_| ())?;
    }
    Ok(watcher)
}

fn print_line(out: &mut dyn Write, value: &impl Serialize) {
    if let Ok(text) = serde_json::to_string(value) {
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}

// A panic elsewhere must not stop the loop; the snapshot stays usable.
fn write(snapshot: &RwLock<Snapshot>) -> RwLockWriteGuard<'_, Snapshot> {
    snapshot.write().unwrap_or_else(PoisonError::into_inner)
}

/// A scan can feed one destination or several independent upload queues.
pub trait ScanSync {
    fn prepared(&self) -> Prepared;
    fn seed(&self, snapshot: &mut Snapshot) -> Result<usize, StateError>;
    fn take_resync(&mut self) -> bool;
    fn record_scan(
        &mut self,
        full: bool,
        counts: &BTreeMap<String, u64>,
        changed: Vec<ScannedEvent>,
        duration: Duration,
    ) -> Recorded;
    fn record_failure(&mut self, scope: &Scope, code: &str);
}

impl ScanSync for Syncer {
    fn prepared(&self) -> Prepared {
        self.prepared
    }

    fn seed(&self, snapshot: &mut Snapshot) -> Result<usize, StateError> {
        Syncer::seed(self, snapshot)
    }

    fn take_resync(&mut self) -> bool {
        Syncer::take_resync(self)
    }

    fn record_scan(
        &mut self,
        full: bool,
        counts: &BTreeMap<String, u64>,
        changed: Vec<ScannedEvent>,
        duration: Duration,
    ) -> Recorded {
        Syncer::record_scan(self, full, counts, changed, duration)
    }

    fn record_failure(&mut self, scope: &Scope, code: &str) {
        Syncer::record_failure(self, scope, code);
    }
}

/// With a `ScanSync`, the snapshot starts from the events already queued in an
/// earlier run, and every new or changed event is handed to the outbox.
pub fn run(
    options: &WatchOptions,
    sender: Sender<Msg>,
    receiver: Receiver<Msg>,
    out: &mut dyn Write,
    mut sync: Option<&mut dyn ScanSync>,
) -> WatchSummary {
    let started = Instant::now();
    let exit_at = options.exit_after.map(|after| started + after);
    let mut scheduler = Scheduler::new(started, options.timing);
    let snapshot = Arc::new(RwLock::new(Snapshot::default()));
    if let Some(syncer) = sync.as_deref_mut() {
        let seeded = syncer.seed(&mut write(&snapshot));
        print_line(
            out,
            &SeedLine {
                state: "seed",
                ok: seeded.is_ok(),
                known: seeded.unwrap_or(0),
                device_changed: syncer.prepared().device_changed,
                generation_changed: syncer.prepared().generation_changed,
            },
        );
    }
    let keep: Option<KeepPayload> = sync.is_some().then(|| {
        let snapshot = Arc::clone(&snapshot);
        Arc::new(move |key: &str, hash: &[u8; 32]| {
            let snapshot = snapshot.read().unwrap_or_else(PoisonError::into_inner);
            snapshot.hash_of(key) != Some(*hash)
        }) as KeepPayload
    });
    let mut summary = WatchSummary::default();
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut roots: Vec<WatchRoot> = Vec::new();
    let mut stopping = false;
    loop {
        if let Some(syncer) = sync.as_deref_mut()
            && syncer.take_resync()
        {
            let mut snapshot = write(&snapshot);
            *snapshot = Snapshot::default();
            let _ = syncer.seed(&mut snapshot);
            scheduler.request_full(Instant::now());
        }
        let now = Instant::now();
        if !stopping
            && !scheduler.is_running()
            && let Some(kind) = scheduler.poll(now)
        {
            let scope = scope_for(&kind);
            let command = worker_command(options, &scope);
            let timeout = options.worker_timeout;
            let done = sender.clone();
            let keep = keep.clone();
            std::thread::spawn(move || {
                let result = worker::run(command, timeout, keep);
                let _ = done.send(Msg::Done {
                    kind,
                    scope,
                    result,
                });
            });
        }
        if stopping && !scheduler.is_running() {
            break;
        }
        let deadline = match (scheduler.next_deadline(), exit_at.filter(|_| !stopping)) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        let message = match deadline {
            Some(deadline) => receiver.recv_timeout(deadline.saturating_duration_since(now)),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(Msg::Change(client)) => {
                summary.change_events += 1;
                if !stopping {
                    scheduler.on_change(Instant::now(), &client);
                }
            }
            Ok(Msg::WatchError) => {
                summary.watch_errors += 1;
                watcher = None;
                scheduler.on_watch_failed(Instant::now());
            }
            Ok(Msg::Stop) => stopping = true,
            Ok(Msg::Sync(line)) => print_line(out, &line),
            Ok(Msg::Done {
                kind,
                scope,
                result,
            }) => {
                let label = kind.label();
                match &kind {
                    ScanKind::Full => summary.full_scans += 1,
                    ScanKind::Triggered { polled: false, .. } => summary.triggered_scans += 1,
                    ScanKind::Triggered { polled: true, .. } => summary.poll_scans += 1,
                }
                let clients = scope.clients.as_ref().map_or(0, BTreeSet::len);
                match result {
                    Ok(output) => {
                        let events = output.events.len();
                        let (merge, changed) =
                            write(&snapshot).merge_changes(&scope, output.events);
                        summary.vanished += merge.vanished;
                        let recorded = sync.as_deref_mut().map(|syncer| {
                            syncer.record_scan(
                                kind == ScanKind::Full,
                                &output.counts,
                                changed,
                                output.usage.wall,
                            )
                        });
                        let clients = if scope.clients.is_none() {
                            output.counts.len()
                        } else {
                            clients
                        };
                        print_line(
                            out,
                            &ScanLine {
                                scan: label,
                                ok: true,
                                error: None,
                                clients,
                                events,
                                merge,
                                cached: Some(output.cached),
                                wall_ms: millis(output.usage.wall),
                                cpu_ms: output.usage.cpu.map(millis),
                                peak_rss_kb: output.usage.max_rss_bytes.map(|bytes| bytes / 1024),
                                stderr_lines: output.usage.stderr_lines,
                            },
                        );
                        if let Some(recorded) = recorded {
                            write(&snapshot).forget(recorded.forget.iter().map(String::as_str));
                            print_line(out, &recorded.line);
                        }
                        scheduler.on_finished(Instant::now(), true);
                        if kind == ScanKind::Full {
                            let detected = output
                                .counts
                                .iter()
                                .filter(|(_, count)| **count > 0)
                                .map(|(client, _)| client.clone())
                                .collect::<BTreeSet<_>>();
                            let next = watch_roots(&options.home, options.use_env_roots, &detected);
                            scheduler.set_detected(detected);
                            if watcher.is_none() || next != roots {
                                // Build the new watcher before dropping the old one.
                                let built = build_watcher(&next, &options.ignored, sender.clone());
                                let ok = built.is_ok();
                                watcher = built.ok();
                                roots = if ok { next } else { Vec::new() };
                                if ok {
                                    scheduler.on_watch_ok();
                                } else {
                                    summary.watch_errors += 1;
                                    scheduler.on_watch_failed(Instant::now());
                                }
                                summary.watched_roots = roots.len();
                                print_line(
                                    out,
                                    &RootsLine {
                                        watch: "roots",
                                        roots: roots.len(),
                                        ok,
                                    },
                                );
                            }
                        }
                    }
                    Err(failure) => {
                        summary.failures += 1;
                        if let Some(syncer) = sync.as_deref_mut() {
                            syncer.record_failure(&scope, &failure.kind.code());
                        }
                        print_line(
                            out,
                            &ScanLine {
                                scan: label,
                                ok: false,
                                error: Some(failure.kind.code()),
                                clients,
                                events: 0,
                                merge: MergeStats::default(),
                                cached: None,
                                wall_ms: millis(failure.usage.wall),
                                cpu_ms: failure.usage.cpu.map(millis),
                                peak_rss_kb: failure.usage.max_rss_bytes.map(|bytes| bytes / 1024),
                                stderr_lines: failure.usage.stderr_lines,
                            },
                        );
                        scheduler.on_finished(Instant::now(), false);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if exit_at.is_some_and(|at| Instant::now() >= at) {
                    stopping = true;
                }
            }
            Err(RecvTimeoutError::Disconnected) => stopping = true,
        }
    }
    drop(watcher);
    let snapshot = write(&snapshot);
    summary.snapshot_events = snapshot.len();
    summary.key_set_sha256 = snapshot.key_set_sha256();
    summary.value_sha256 = snapshot.value_sha256();
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(cache: Option<PathBuf>) -> WatchOptions {
        WatchOptions {
            home: PathBuf::from("/synthetic-home"),
            explicit_home: true,
            use_env_roots: false,
            worker_program: PathBuf::from("/synthetic-collector"),
            timing: Timing::default(),
            worker_timeout: Duration::from_secs(300),
            exit_after: None,
            ignored: Vec::new(),
            upstream_cache_dir: cache,
        }
    }

    fn env_value(command: &Command, name: &str) -> Option<Option<OsString>> {
        worker_env(command)
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    #[test]
    fn triggered_scope_is_yesterday_through_today_for_the_given_clients() {
        let clients = ["codex".to_owned(), "zcode".to_owned()]
            .into_iter()
            .collect();
        let scope = scope_for(&ScanKind::Triggered {
            clients,
            polled: false,
        });
        let today = local_today();
        assert_eq!(scope.range.until, Some(today));
        assert_eq!(scope.range.since, today.pred_opt());
        let args = worker_command(&options(None), &scope)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--clients", "codex,zcode"])
        );
        assert!(args.contains(&"--no-env-roots".to_owned()));
        assert!(!args.contains(&"--upstream-cache".to_owned()));
        let full = scope_for(&ScanKind::Full);
        assert!(full.clients.is_none());
        assert_eq!(full.range, DateRange::all_history());
    }

    #[test]
    fn worker_environment_is_isolated_and_cache_dir_is_ours() {
        let uncached = worker_command(&options(None), &scope_for(&ScanKind::Full));
        assert_eq!(env_value(&uncached, "TOKSCALE_CONFIG_DIR"), Some(None));
        assert_eq!(
            env_value(&uncached, "TOKSCALE_RECOVERY_DISABLE"),
            Some(Some("1".into()))
        );
        assert_eq!(env_value(&uncached, "CODEX_HOME"), Some(None));
        assert_eq!(
            env_value(&uncached, "HOME"),
            Some(Some("/synthetic-home".into()))
        );
        let own = PathBuf::from("/synthetic-cache/tokenlens/tokscale");
        let cached = worker_command(&options(Some(own.clone())), &scope_for(&ScanKind::Full));
        assert_eq!(
            env_value(&cached, "TOKSCALE_CONFIG_DIR"),
            Some(Some(own.into_os_string()))
        );
        assert!(
            cached
                .get_args()
                .any(|arg| arg == std::ffi::OsStr::new("--upstream-cache"))
        );
    }
}
