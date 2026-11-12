//! Pure scan-trigger logic; the clock is always passed in so tests can fake it.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Quiet period after the most recent change.
    pub debounce: Duration,
    /// Upper bound from the first unscanned change, so steady writes that are
    /// faster than the debounce cannot postpone a scan forever.
    pub max_delay: Duration,
    /// Minimum time between two scan starts.
    pub cooldown: Duration,
    pub full_interval: Duration,
    pub poll_interval: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(1500),
            max_delay: Duration::from_secs(30),
            cooldown: Duration::from_secs(30),
            full_interval: Duration::from_secs(3600),
            poll_interval: Duration::from_secs(300),
            backoff_initial: Duration::from_secs(60),
            backoff_max: Duration::from_secs(900),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanKind {
    /// Every client, every date; also re-selects the watched directories.
    Full,
    /// Only the given clients, from yesterday to today.
    Triggered {
        clients: BTreeSet<String>,
        polled: bool,
    },
}

impl ScanKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Triggered { polled: false, .. } => "triggered",
            Self::Triggered { polled: true, .. } => "poll",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    timing: Timing,
    pending: BTreeSet<String>,
    first_change: Option<Instant>,
    last_change: Option<Instant>,
    full_due: Instant,
    /// Set by `request_full`, so a full scan already running does not count.
    full_requested: bool,
    running: Option<ScanKind>,
    last_start: Option<Instant>,
    backoff: Option<Duration>,
    retry_at: Option<Instant>,
    polling: bool,
    next_poll: Option<Instant>,
    detected: BTreeSet<String>,
}

impl Scheduler {
    /// The first scan is always a full scan that builds the snapshot.
    pub fn new(now: Instant, timing: Timing) -> Self {
        Self {
            timing,
            pending: BTreeSet::new(),
            first_change: None,
            last_change: None,
            full_due: now,
            full_requested: false,
            running: None,
            last_start: None,
            backoff: None,
            retry_at: None,
            polling: false,
            next_poll: None,
            detected: BTreeSet::new(),
        }
    }

    pub fn timing(&self) -> Timing {
        self.timing
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn is_polling(&self) -> bool {
        self.polling
    }

    pub fn set_detected(&mut self, clients: BTreeSet<String>) {
        self.detected = clients;
    }

    pub fn on_change(&mut self, now: Instant, client: &str) {
        self.pending.insert(client.to_owned());
        self.first_change.get_or_insert(now);
        self.last_change = Some(now);
    }

    /// Runs a full scan as soon as the cooldown allows.
    pub fn request_full(&mut self, now: Instant) {
        self.full_due = self.full_due.min(now);
        self.full_requested = true;
    }

    pub fn on_watch_failed(&mut self, now: Instant) {
        if !self.polling {
            self.polling = true;
            self.next_poll = Some(now + self.timing.poll_interval);
        }
    }

    pub fn on_watch_ok(&mut self) {
        self.polling = false;
        self.next_poll = None;
    }

    fn triggered_due(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            return None;
        }
        let debounced = self.last_change? + self.timing.debounce;
        let capped = self.first_change? + self.timing.max_delay;
        Some(debounced.min(capped))
    }

    fn gate(&self) -> Option<Instant> {
        let cooldown = self.last_start.map(|start| start + self.timing.cooldown);
        match (cooldown, self.retry_at) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        }
    }

    /// When the caller should call `poll` next; `None` while a scan runs.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.running.is_some() {
            return None;
        }
        let mut wanted = self.full_due;
        if let Some(due) = self.triggered_due() {
            wanted = wanted.min(due);
        }
        if let Some(due) = self.next_poll.filter(|_| self.polling) {
            wanted = wanted.min(due);
        }
        Some(self.gate().map_or(wanted, |gate| wanted.max(gate)))
    }

    /// Returns the scan to start now, if any, and marks it as running.
    pub fn poll(&mut self, now: Instant) -> Option<ScanKind> {
        if self.running.is_some() || self.gate().is_some_and(|gate| gate > now) {
            return None;
        }
        let kind = if self.full_due <= now {
            // A full scan covers every pending change and any poll.
            self.pending.clear();
            self.first_change = None;
            self.last_change = None;
            self.full_requested = false;
            if self.polling {
                self.next_poll = Some(now + self.timing.poll_interval);
            }
            ScanKind::Full
        } else if self.triggered_due().is_some_and(|due| due <= now) {
            self.first_change = None;
            self.last_change = None;
            ScanKind::Triggered {
                clients: std::mem::take(&mut self.pending),
                polled: false,
            }
        } else if self.polling && self.next_poll.is_some_and(|due| due <= now) {
            self.next_poll = Some(now + self.timing.poll_interval);
            if self.detected.is_empty() {
                return None;
            }
            ScanKind::Triggered {
                clients: self.detected.clone(),
                polled: true,
            }
        } else {
            return None;
        };
        self.running = Some(kind.clone());
        self.last_start = Some(now);
        Some(kind)
    }

    pub fn on_finished(&mut self, now: Instant, success: bool) {
        let Some(kind) = self.running.take() else {
            return;
        };
        if success {
            self.backoff = None;
            self.retry_at = None;
            if kind == ScanKind::Full && !self.full_requested {
                let started = self.last_start.unwrap_or(now);
                self.full_due = started + self.timing.full_interval;
            }
            return;
        }
        let backoff = self
            .backoff
            .map_or(self.timing.backoff_initial, |previous| {
                (previous * 2).min(self.timing.backoff_max)
            });
        self.backoff = Some(backoff);
        self.retry_at = Some(now + backoff);
        // A failed full scan stays due; a failed triggered scan is re-queued.
        if let ScanKind::Triggered { clients, .. } = kind {
            self.pending.extend(clients);
            self.first_change = Some(self.first_change.map_or(now, |first| first.min(now)));
            self.last_change.get_or_insert(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(value: f64) -> Duration {
        Duration::from_secs_f64(value)
    }

    fn started(t0: Instant) -> Scheduler {
        let mut scheduler = Scheduler::new(t0, Timing::default());
        assert_eq!(scheduler.poll(t0), Some(ScanKind::Full));
        scheduler.on_finished(t0 + secs(1.0), true);
        scheduler
    }

    fn triggered(clients: &[&str]) -> ScanKind {
        ScanKind::Triggered {
            clients: clients.iter().map(|client| (*client).to_owned()).collect(),
            polled: false,
        }
    }

    #[test]
    fn startup_is_a_full_scan_and_idle_waits_for_the_hourly_full() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(3600.0)));
        assert_eq!(scheduler.poll(t0 + secs(3599.0)), None);
        assert_eq!(scheduler.poll(t0 + secs(3600.0)), Some(ScanKind::Full));
    }

    #[test]
    fn a_requested_full_scan_survives_the_full_scan_already_running() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        scheduler.request_full(t0 + secs(40.0));
        assert_eq!(scheduler.poll(t0 + secs(40.0)), Some(ScanKind::Full));
        scheduler.request_full(t0 + secs(41.0));
        scheduler.on_finished(t0 + secs(42.0), true);
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(70.0)));
        assert_eq!(scheduler.poll(t0 + secs(70.0)), Some(ScanKind::Full));
        scheduler.on_finished(t0 + secs(71.0), true);
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(3670.0)));
    }

    #[test]
    fn every_change_restarts_the_debounce() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        scheduler.on_change(t0 + secs(40.0), "codex");
        assert_eq!(scheduler.poll(t0 + secs(41.0)), None);
        scheduler.on_change(t0 + secs(41.0), "codex");
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(42.5)));
        assert_eq!(scheduler.poll(t0 + secs(42.0)), None);
        assert_eq!(scheduler.poll(t0 + secs(42.5)), Some(triggered(&["codex"])));
    }

    #[test]
    fn scan_starts_are_at_least_the_cooldown_apart() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        scheduler.on_change(t0 + secs(2.0), "claude");
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(30.0)));
        assert_eq!(scheduler.poll(t0 + secs(10.0)), None);
        assert_eq!(
            scheduler.poll(t0 + secs(30.0)),
            Some(triggered(&["claude"]))
        );
    }

    #[test]
    fn steady_writes_cannot_starve_scans_past_the_max_delay() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        let mut fired = None;
        for second in 40..=80 {
            let now = t0 + secs(f64::from(second));
            scheduler.on_change(now, "claude");
            if let Some(kind) = scheduler.poll(now) {
                fired = Some((second, kind));
                break;
            }
        }
        assert_eq!(fired, Some((70, triggered(&["claude"]))));
    }

    #[test]
    fn a_change_during_a_scan_causes_one_more_scan() {
        let t0 = Instant::now();
        let mut scheduler = Scheduler::new(t0, Timing::default());
        assert_eq!(scheduler.poll(t0), Some(ScanKind::Full));
        scheduler.on_change(t0 + secs(0.5), "zcode");
        assert_eq!(scheduler.next_deadline(), None);
        scheduler.on_finished(t0 + secs(1.0), true);
        assert_eq!(scheduler.poll(t0 + secs(30.0)), Some(triggered(&["zcode"])));
        scheduler.on_finished(t0 + secs(31.0), true);
        assert_eq!(scheduler.poll(t0 + secs(120.0)), None);
    }

    #[test]
    fn changes_for_several_clients_are_scanned_together() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        scheduler.on_change(t0 + secs(40.0), "codex");
        scheduler.on_change(t0 + secs(40.2), "qoder-intl");
        assert_eq!(
            scheduler.poll(t0 + secs(42.0)),
            Some(triggered(&["codex", "qoder-intl"]))
        );
    }

    #[test]
    fn watch_failure_falls_back_to_five_minute_polls_of_detected_clients() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        scheduler.set_detected(["codex".to_owned(), "dsh".to_owned()].into());
        scheduler.on_watch_failed(t0 + secs(10.0));
        assert!(scheduler.is_polling());
        assert_eq!(scheduler.poll(t0 + secs(309.0)), None);
        let polled = ScanKind::Triggered {
            clients: ["codex".to_owned(), "dsh".to_owned()].into(),
            polled: true,
        };
        assert_eq!(scheduler.poll(t0 + secs(310.0)), Some(polled.clone()));
        scheduler.on_finished(t0 + secs(311.0), true);
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(610.0)));
        assert_eq!(scheduler.poll(t0 + secs(610.0)), Some(polled));
        scheduler.on_finished(t0 + secs(611.0), true);
        // The hourly full scan still happens while polling.
        assert_eq!(scheduler.poll(t0 + secs(3600.0)), Some(ScanKind::Full));
        scheduler.on_finished(t0 + secs(3601.0), true);
        scheduler.on_watch_ok();
        assert_eq!(scheduler.next_deadline(), Some(t0 + secs(7200.0)));
    }

    #[test]
    fn failures_back_off_from_one_minute_doubling_to_fifteen_minutes() {
        let t0 = Instant::now();
        let mut scheduler = Scheduler::new(t0, Timing::default());
        let mut now = t0;
        let mut waits = Vec::new();
        for _ in 0..6 {
            assert_eq!(scheduler.poll(now), Some(ScanKind::Full));
            scheduler.on_finished(now, false);
            let next = scheduler.next_deadline().expect("retry");
            waits.push((next - now).as_secs());
            now = next;
        }
        assert_eq!(waits, [60, 120, 240, 480, 900, 900]);
        assert_eq!(scheduler.poll(now), Some(ScanKind::Full));
        scheduler.on_finished(now, true);
        scheduler.on_change(now + secs(40.0), "codex");
        assert_eq!(scheduler.next_deadline(), Some(now + secs(41.5)));
    }

    #[test]
    fn a_failed_triggered_scan_is_requeued_without_losing_clients() {
        let t0 = Instant::now();
        let mut scheduler = started(t0);
        scheduler.on_change(t0 + secs(40.0), "codex");
        assert_eq!(scheduler.poll(t0 + secs(42.0)), Some(triggered(&["codex"])));
        scheduler.on_change(t0 + secs(42.5), "claude");
        scheduler.on_finished(t0 + secs(43.0), false);
        assert_eq!(scheduler.poll(t0 + secs(100.0)), None);
        assert_eq!(
            scheduler.poll(t0 + secs(103.0)),
            Some(triggered(&["claude", "codex"]))
        );
    }
}
