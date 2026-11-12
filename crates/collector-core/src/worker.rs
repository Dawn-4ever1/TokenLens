//! `scan-worker` stdout protocol and the parent-side runner.
//!
//! The child prints one JSON object per line. Its stderr may contain upstream
//! messages with paths, so the parent only counts those lines.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use usage_event::UsageEvent;

use crate::snapshot::ScannedEvent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkerLine {
    Event {
        event: Box<UsageEvent>,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        counts: BTreeMap<String, u64>,
        /// Whether upstream's persistent source cache was used.
        cached: bool,
        elapsed_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Spawn,
    Exit(Option<i32>),
    Timeout,
    Protocol,
    InvalidEvent,
    MissingDone,
}

impl FailureKind {
    pub fn code(self) -> String {
        match self {
            Self::Spawn => "spawn".to_owned(),
            Self::Exit(Some(code)) => format!("exit-{code}"),
            Self::Exit(None) => "signal".to_owned(),
            Self::Timeout => "timeout".to_owned(),
            Self::Protocol => "protocol".to_owned(),
            Self::InvalidEvent => "invalid-event".to_owned(),
            Self::MissingDone => "missing-done".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildUsage {
    pub wall: Duration,
    pub cpu: Option<Duration>,
    pub max_rss_bytes: Option<u64>,
    pub stderr_lines: u64,
}

#[derive(Debug)]
pub struct WorkerOutput {
    pub events: Vec<ScannedEvent>,
    pub counts: BTreeMap<String, u64>,
    pub cached: bool,
    pub usage: ChildUsage,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerFailure {
    pub kind: FailureKind,
    pub usage: ChildUsage,
}

/// Decides, per `(event_key, value hash)`, whether the event's canonical
/// JSON is kept for upload. Unchanged events are dropped right away so a full
/// scan never holds every payload in memory.
pub type KeepPayload = Arc<dyn Fn(&str, &[u8; 32]) -> bool + Send + Sync>;

#[derive(Default)]
struct Parsed {
    events: Vec<ScannedEvent>,
    done: Option<(BTreeMap<String, u64>, bool)>,
    error: Option<FailureKind>,
}

/// Parse and validate every line; keep draining after an error so the child
/// never blocks on a full pipe.
fn parse_stdout(reader: impl Read, keep: Option<&KeepPayload>) -> Parsed {
    let mut parsed = Parsed::default();
    for line in BufReader::new(reader).split(b'\n') {
        let Ok(line) = line else {
            parsed.error.get_or_insert(FailureKind::Protocol);
            break;
        };
        if parsed.error.is_some() || line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if parsed.done.is_some() {
            parsed.error = Some(FailureKind::Protocol);
            continue;
        }
        match serde_json::from_slice::<WorkerLine>(&line) {
            Ok(WorkerLine::Event { event }) => {
                let event = *event;
                if event.validate().is_err() {
                    parsed.error = Some(FailureKind::InvalidEvent);
                    continue;
                }
                let hash = crate::event_value_hash(&event);
                let payload = keep
                    .filter(|keep| keep(&event.event_key, &hash))
                    .and_then(|_| event.to_json_value().ok())
                    .map(|value| crate::canonical_json(&value));
                parsed.events.push(ScannedEvent {
                    hash,
                    key: event.event_key,
                    client: event.client,
                    ts: event.ts,
                    payload,
                });
            }
            Ok(WorkerLine::Done { counts, cached, .. }) => parsed.done = Some((counts, cached)),
            Err(_) => parsed.error = Some(FailureKind::Protocol),
        }
    }
    parsed
}

fn count_lines(reader: impl Read) -> u64 {
    let mut reader = BufReader::new(reader);
    let mut count = 0;
    let mut buffer = Vec::new();
    while let Ok(read) = reader.read_until(b'\n', &mut buffer) {
        if read == 0 {
            break;
        }
        count += 1;
        buffer.clear();
    }
    count
}

#[cfg(unix)]
fn wait_child(
    child: &mut std::process::Child,
    deadline: Instant,
) -> (Option<Option<i32>>, Option<Duration>, Option<u64>) {
    let pid = child.id() as libc::pid_t;
    let mut killed = false;
    loop {
        let mut status = 0;
        // SAFETY: rusage is plain data and the pid is our own unreaped child.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let flags = if killed { 0 } else { libc::WNOHANG };
        let result = unsafe { libc::wait4(pid, &mut status, flags, &mut usage) };
        if result == pid {
            let to_duration = |time: libc::timeval| {
                Duration::from_secs(time.tv_sec as u64) + Duration::from_micros(time.tv_usec as u64)
            };
            let cpu = to_duration(usage.ru_utime) + to_duration(usage.ru_stime);
            // macOS reports bytes; Linux reports kilobytes.
            let rss = if cfg!(target_os = "macos") {
                usage.ru_maxrss as u64
            } else {
                (usage.ru_maxrss as u64).saturating_mul(1024)
            };
            let exit = if killed {
                None
            } else if libc::WIFEXITED(status) {
                Some(Some(libc::WEXITSTATUS(status)))
            } else {
                Some(None)
            };
            return (exit, Some(cpu), Some(rss));
        }
        if result < 0 {
            return (Some(None), None, None);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            killed = true;
            continue;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(unix))]
fn wait_child(
    child: &mut std::process::Child,
    deadline: Instant,
) -> (Option<Option<i32>>, Option<Duration>, Option<u64>) {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status.code()), None, None),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (None, None, None);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return (Some(None), None, None),
        }
    }
}

/// Lower the priority of worker processes before they are created (Windows)
/// and hide their console window.
pub fn configure_priority(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW);
    }
    // A separate process group keeps a terminal Ctrl-C from killing a scan
    // midway; the parent stops after the running scan finishes.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}

/// Lower this process's own priority (Mac/Linux worker side): nice 10.
pub fn lower_own_priority() {
    #[cfg(unix)]
    // SAFETY: setpriority only changes this process's scheduling priority.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS as _, 0, 10);
    }
}

pub fn run(
    mut command: Command,
    timeout: Duration,
    keep: Option<KeepPayload>,
) -> Result<WorkerOutput, WorkerFailure> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let failure = |kind, usage| WorkerFailure { kind, usage };
    let mut child = command
        .spawn()
        .map_err(|_| failure(FailureKind::Spawn, ChildUsage::default()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        stdout
            .map(|stdout| parse_stdout(stdout, keep.as_ref()))
            .unwrap_or_default()
    });
    let stderr_thread = std::thread::spawn(move || stderr.map(count_lines).unwrap_or(0));
    let (exit, cpu, max_rss_bytes) = wait_child(&mut child, start + timeout);
    let parsed = stdout_thread.join().unwrap_or_default();
    let usage = ChildUsage {
        wall: start.elapsed(),
        cpu,
        max_rss_bytes,
        stderr_lines: stderr_thread.join().unwrap_or(0),
    };
    match exit {
        None => return Err(failure(FailureKind::Timeout, usage)),
        Some(Some(0)) => {}
        Some(code) => return Err(failure(FailureKind::Exit(code), usage)),
    }
    if let Some(kind) = parsed.error {
        return Err(failure(kind, usage));
    }
    let Some((counts, cached)) = parsed.done else {
        return Err(failure(FailureKind::MissingDone, usage));
    };
    Ok(WorkerOutput {
        events: parsed.events,
        counts,
        cached,
        usage,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn synthetic_event() -> UsageEvent {
        crate::map_message(
            &tokscale_core::UnifiedMessage::new_with_dedup(
                "codex",
                "synthetic-model",
                "synthetic-provider",
                "synthetic-session",
                1_793_577_600_000,
                tokscale_core::TokenBreakdown {
                    input: 1,
                    output: 2,
                    cache_read: 3,
                    cache_write: 0,
                    cache_write_1h: 0,
                    reasoning: 0,
                },
                0.0,
                Some("synthetic-dedup".into()),
            ),
            0,
        )
        .expect("event")
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }

    fn lines() -> (String, String) {
        let event = serde_json::to_string(&WorkerLine::Event {
            event: Box::new(synthetic_event()),
        })
        .expect("event line");
        let done = serde_json::to_string(&WorkerLine::Done {
            counts: [("codex".to_owned(), 1)].into(),
            cached: false,
            elapsed_ms: 5,
        })
        .expect("done line");
        (event, done)
    }

    #[test]
    fn event_lines_round_trip_and_stderr_is_only_counted() {
        let (event, done) = lines();
        let parsed: WorkerLine = serde_json::from_str(&event).expect("parse");
        assert_eq!(
            parsed,
            WorkerLine::Event {
                event: Box::new(synthetic_event())
            }
        );
        let script = format!(
            "echo '/synthetic/secret/path' >&2; echo 'second' >&2; printf '%s\\n%s\\n' '{event}' '{done}'"
        );
        let output = run(shell(&script), Duration::from_secs(10), None).expect("worker");
        assert_eq!(output.events.len(), 1);
        assert_eq!(
            output.events[0].hash,
            crate::event_value_hash(&synthetic_event())
        );
        assert_eq!(output.counts["codex"], 1);
        assert_eq!(output.usage.stderr_lines, 2);
        assert!(output.usage.cpu.is_some());
        assert!(output.events[0].payload.is_none());
        let keep: KeepPayload = Arc::new(|_, _| true);
        let output = run(shell(&script), Duration::from_secs(10), Some(keep)).expect("worker");
        let payload = output.events[0].payload.as_deref().expect("kept");
        let expected = synthetic_event().to_json_value().expect("valid");
        assert_eq!(payload, crate::canonical_json(&expected));
    }

    #[test]
    fn unknown_fields_and_invalid_events_are_rejected() {
        let (event, done) = lines();
        let extra = event.replacen("{\"type\"", "{\"title\":\"x\",\"type\"", 1);
        let nested = event.replacen("\"eventKey\"", "\"workspace\":\"x\",\"eventKey\"", 1);
        for bad in [extra, nested] {
            let script = format!("printf '%s\\n%s\\n' '{bad}' '{done}'");
            let failure = run(shell(&script), Duration::from_secs(10), None).expect_err("rejected");
            assert_eq!(failure.kind, FailureKind::Protocol);
        }
        let mut invalid = synthetic_event();
        invalid.rate_version = Some("synthetic-rate".into());
        let invalid = serde_json::to_string(&WorkerLine::Event {
            event: Box::new(invalid),
        })
        .expect("line");
        let script = format!("printf '%s\\n%s\\n' '{invalid}' '{done}'");
        let failure = run(shell(&script), Duration::from_secs(10), None).expect_err("invalid");
        assert_eq!(failure.kind, FailureKind::InvalidEvent);
        let script = format!("printf '%s\\n' '{event}'");
        let failure = run(shell(&script), Duration::from_secs(10), None).expect_err("no done");
        assert_eq!(failure.kind, FailureKind::MissingDone);
    }

    #[test]
    fn nonzero_exit_and_timeout_are_failures() {
        let (event, done) = lines();
        let script = format!("printf '%s\\n%s\\n' '{event}' '{done}'; exit 3");
        let failure = run(shell(&script), Duration::from_secs(10), None).expect_err("exit");
        assert_eq!(failure.kind, FailureKind::Exit(Some(3)));
        let failure = run(shell("sleep 5"), Duration::from_millis(200), None).expect_err("timeout");
        assert_eq!(failure.kind, FailureKind::Timeout);
        assert!(failure.usage.wall < Duration::from_secs(4));
    }
}
