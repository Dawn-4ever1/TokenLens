//! In-memory request guards: per-client brute-force guards for
//! `POST /api/pair/claim` and `/api/auth/*`, plus a per-device budget for
//! `/api/ingest/*`. Client keys exist only in memory and are never logged or
//! persisted.

use std::collections::{HashMap, VecDeque};

pub const CLAIM_FAILURE_WINDOW_MS: i64 = 10 * 60 * 1000;
pub const CLAIM_FAILURE_LIMIT: usize = 20;
/// High global safety ceiling so a distributed flood still hits a cap without
/// letting one client lock out everyone else.
pub const CLAIM_GLOBAL_FAILURE_LIMIT: usize = 200;
pub const INGEST_WINDOW_MS: i64 = 60 * 1000;
/// A full backfill sends 2,000-event batches back to back; this still lets
/// it through at about 4 million events a minute.
pub const INGEST_REQUESTS_PER_WINDOW: u32 = 120;
pub const AUTH_WINDOW_MS: i64 = 60 * 1000;
pub const AUTH_REQUESTS_PER_WINDOW: u32 = 20;
pub const AUTH_GLOBAL_REQUESTS_PER_WINDOW: u32 = 300;
const MAX_TRACKED_KEYS: usize = 10_000;

/// Sliding window of failed claims per client key, backed by a higher global
/// safety cap. Refused claims are not recorded, so an attacker cannot extend
/// the lockout window indefinitely.
#[derive(Debug, Default)]
pub struct ClaimLimiter {
    per_key: HashMap<String, VecDeque<i64>>,
    global: VecDeque<i64>,
}

impl ClaimLimiter {
    fn prune_queue(queue: &mut VecDeque<i64>, now_ms: i64) {
        while queue
            .front()
            .is_some_and(|&at| at <= now_ms - CLAIM_FAILURE_WINDOW_MS)
        {
            queue.pop_front();
        }
    }

    fn prune(&mut self, key: &str, now_ms: i64) {
        Self::prune_queue(&mut self.global, now_ms);
        if let Some(queue) = self.per_key.get_mut(key) {
            Self::prune_queue(queue, now_ms);
            if queue.is_empty() {
                self.per_key.remove(key);
            }
        }
        if self.per_key.len() >= MAX_TRACKED_KEYS {
            self.per_key.retain(|_, queue| {
                Self::prune_queue(queue, now_ms);
                !queue.is_empty()
            });
        }
    }

    pub fn is_blocked(&mut self, key: &str, now_ms: i64) -> bool {
        self.prune(key, now_ms);
        self.global.len() >= CLAIM_GLOBAL_FAILURE_LIMIT
            || self
                .per_key
                .get(key)
                .is_some_and(|queue| queue.len() >= CLAIM_FAILURE_LIMIT)
            || (self.per_key.len() >= MAX_TRACKED_KEYS && !self.per_key.contains_key(key))
    }

    pub fn record_failure(&mut self, key: &str, now_ms: i64) {
        self.prune(key, now_ms);
        self.global.push_back(now_ms);
        self.per_key
            .entry(key.to_owned())
            .or_default()
            .push_back(now_ms);
    }
}

/// Fixed one-minute windows per device. Only authenticated requests are
/// counted, so the map holds at most one entry per active device.
#[derive(Debug, Default)]
pub struct IngestLimiter {
    windows: HashMap<String, (i64, u32)>,
}

impl IngestLimiter {
    /// `Err(retry_after_seconds)` once the device used up this window.
    pub fn check(&mut self, device_id: &str, now_ms: i64) -> Result<(), u64> {
        let window_start = now_ms - now_ms.rem_euclid(INGEST_WINDOW_MS);
        if self.windows.len() > MAX_TRACKED_KEYS {
            self.windows.retain(|_, (start, _)| *start == window_start);
        }
        let entry = self
            .windows
            .entry(device_id.to_owned())
            .or_insert((window_start, 0));
        if entry.0 != window_start {
            *entry = (window_start, 0);
        }
        if entry.1 >= INGEST_REQUESTS_PER_WINDOW {
            let remaining_ms = u64::try_from(window_start + INGEST_WINDOW_MS - now_ms).unwrap_or(1);
            return Err(remaining_ms.div_ceil(1000).max(1));
        }
        entry.1 += 1;
        Ok(())
    }
}

/// Bounded in-memory guard for account code requests and login attempts.
/// Enforces a per-key budget plus an optional higher global safety cap for a
/// route group so one caller cannot lock out unrelated users.
#[derive(Debug, Default)]
pub struct AuthLimiter {
    windows: HashMap<String, (i64, u32)>,
    global: HashMap<&'static str, (i64, u32)>,
}

impl AuthLimiter {
    pub fn allow(&mut self, key: &str, now: i64) -> bool {
        self.allow_with(key, now, AUTH_REQUESTS_PER_WINDOW)
    }

    pub fn allow_with(&mut self, key: &str, now: i64, limit: u32) -> bool {
        let window = now - now.rem_euclid(AUTH_WINDOW_MS);
        if self.windows.len() >= MAX_TRACKED_KEYS {
            self.windows.retain(|_, (start, _)| *start == window);
            if self.windows.len() >= MAX_TRACKED_KEYS && !self.windows.contains_key(key) {
                return false;
            }
        }
        let entry = self.windows.entry(key.to_owned()).or_insert((window, 0));
        if entry.0 != window {
            *entry = (window, 0);
        }
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }

    pub fn allow_scoped(&mut self, scope: &'static str, key: &str, now: i64) -> bool {
        self.allow_scoped_with(
            scope,
            key,
            now,
            AUTH_REQUESTS_PER_WINDOW,
            AUTH_GLOBAL_REQUESTS_PER_WINDOW,
        )
    }

    pub fn allow_scoped_with(
        &mut self,
        scope: &'static str,
        key: &str,
        now: i64,
        per_key_limit: u32,
        global_limit: u32,
    ) -> bool {
        let window = now - now.rem_euclid(AUTH_WINDOW_MS);
        let global_entry = self.global.entry(scope).or_insert((window, 0));
        if global_entry.0 != window {
            *global_entry = (window, 0);
        }
        if global_entry.1 >= global_limit {
            return false;
        }
        let scoped_key = format!("{scope}:{key}");
        if !self.allow_with(&scoped_key, now, per_key_limit) {
            return false;
        }
        if let Some(entry) = self.global.get_mut(scope) {
            entry.1 += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_the_limit_until_the_window_passes() {
        let mut limiter = ClaimLimiter::default();
        for second in 0..CLAIM_FAILURE_LIMIT as i64 {
            assert!(!limiter.is_blocked("203.0.113.1", second * 1000));
            limiter.record_failure("203.0.113.1", second * 1000);
        }
        assert!(limiter.is_blocked("203.0.113.1", 30_000));
        assert!(limiter.is_blocked("203.0.113.1", CLAIM_FAILURE_WINDOW_MS - 1));
        // Another client IP is not locked out by the first client's failures.
        assert!(!limiter.is_blocked("203.0.113.2", 30_000));
        // The first failure (t = 0) has left the window.
        assert!(!limiter.is_blocked("203.0.113.1", CLAIM_FAILURE_WINDOW_MS));
    }

    #[test]
    fn claim_limiter_has_a_global_safety_ceiling() {
        let mut limiter = ClaimLimiter::default();
        for index in 0..CLAIM_GLOBAL_FAILURE_LIMIT {
            let key = format!("198.51.100.{}", index % 250);
            limiter.record_failure(&key, 1_000);
        }
        assert!(limiter.is_blocked("203.0.113.99", 1_000));
        assert!(!limiter.is_blocked("203.0.113.99", 1_000 + CLAIM_FAILURE_WINDOW_MS));
    }

    #[test]
    fn auth_limiter_isolates_callers_while_keeping_a_global_cap() {
        let mut limiter = AuthLimiter::default();
        for _ in 0..AUTH_REQUESTS_PER_WINDOW {
            assert!(limiter.allow_scoped("github-start", "203.0.113.1", 60_000));
        }
        assert!(!limiter.allow_scoped("github-start", "203.0.113.1", 60_000));
        // A different client is unaffected.
        assert!(limiter.allow_scoped("github-start", "203.0.113.2", 60_000));
        // Next minute resets the window.
        assert!(limiter.allow_scoped("github-start", "203.0.113.1", 120_000));
    }

    #[test]
    fn ingest_budget_is_per_device_and_per_window() {
        let mut limiter = IngestLimiter::default();
        let start = 5 * INGEST_WINDOW_MS + 1_500;
        for _ in 0..INGEST_REQUESTS_PER_WINDOW {
            assert_eq!(limiter.check("dev_a", start), Ok(()));
        }
        assert_eq!(limiter.check("dev_a", start), Err(59));
        assert_eq!(limiter.check("dev_a", 6 * INGEST_WINDOW_MS - 1), Err(1));
        assert_eq!(limiter.check("dev_b", start), Ok(()));
        assert_eq!(limiter.check("dev_a", 6 * INGEST_WINDOW_MS), Ok(()));
    }

    #[test]
    fn claim_limiter_fails_closed_when_tracked_keys_saturate_without_clearing() {
        let mut limiter = ClaimLimiter::default();
        for index in 0..MAX_TRACKED_KEYS {
            let key = format!("k_{index}");
            for _ in 0..CLAIM_FAILURE_LIMIT {
                limiter.record_failure(&key, 1_000);
            }
        }
        assert!(limiter.is_blocked("k_0", 1_000));
        assert!(
            limiter.is_blocked("untracked_new_ip", 1_000),
            "saturation must fail closed instead of clearing existing lockouts"
        );
        // Existing tracked keys recover after expiry, freeing capacity cleanly.
        assert!(!limiter.is_blocked("untracked_new_ip", 1_000 + CLAIM_FAILURE_WINDOW_MS));
    }

    #[test]
    fn auth_limiter_fails_closed_when_tracked_keys_saturate_without_clearing() {
        let mut limiter = AuthLimiter::default();
        for index in 0..MAX_TRACKED_KEYS {
            assert!(limiter.allow(&format!("u_{index}"), 60_000));
        }
        assert!(
            !limiter.allow("untracked_user", 60_000),
            "untracked keys must fail closed once tracked capacity is full"
        );
        assert!(
            limiter.allow("u_0", 60_000),
            "already tracked keys retain their remaining window budget"
        );
        // Next window evicts stale keys and allows new callers again.
        assert!(limiter.allow("untracked_user", 120_000));
    }

    #[test]
    fn auth_limiter_allow_scoped_with_respects_custom_limits() {
        let mut limiter = AuthLimiter::default();
        for _ in 0..3 {
            assert!(limiter.allow_scoped_with("poll", "ip-1", 60_000, 3, 5));
        }
        assert!(!limiter.allow_scoped_with("poll", "ip-1", 60_000, 3, 5));
        assert!(limiter.allow_scoped_with("poll", "ip-2", 60_000, 3, 5));
        assert!(limiter.allow_scoped_with("poll", "ip-2", 60_000, 3, 5));
        assert!(
            !limiter.allow_scoped_with("poll", "ip-3", 60_000, 3, 5),
            "global ceiling still caps total requests across IPs"
        );
    }
}
