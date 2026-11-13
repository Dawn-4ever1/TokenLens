//! In-memory request guards: a global brute-force guard for
//! `POST /api/pair/claim` and a per-device budget for `/api/ingest/*`.

use std::collections::{HashMap, VecDeque};

pub const CLAIM_FAILURE_WINDOW_MS: i64 = 10 * 60 * 1000;
pub const CLAIM_FAILURE_LIMIT: usize = 20;
pub const INGEST_WINDOW_MS: i64 = 60 * 1000;
/// A full backfill sends 2,000-event batches back to back; this still lets
/// it through at about 4 million events a minute.
pub const INGEST_REQUESTS_PER_WINDOW: u32 = 120;

/// Sliding window of failed claims. Once it holds the limit, every claim is
/// refused until the oldest failure leaves the window; refused claims are not
/// recorded, so the window cannot be extended by an attacker.
#[derive(Debug, Default)]
pub struct ClaimLimiter {
    failures: VecDeque<i64>,
}

impl ClaimLimiter {
    fn prune(&mut self, now_ms: i64) {
        while self
            .failures
            .front()
            .is_some_and(|&at| at <= now_ms - CLAIM_FAILURE_WINDOW_MS)
        {
            self.failures.pop_front();
        }
    }

    pub fn is_blocked(&mut self, now_ms: i64) -> bool {
        self.prune(now_ms);
        self.failures.len() >= CLAIM_FAILURE_LIMIT
    }

    pub fn record_failure(&mut self, now_ms: i64) {
        self.prune(now_ms);
        self.failures.push_back(now_ms);
    }
}

/// Fixed one-minute windows per device. Only authenticated requests are
/// counted, so the map holds at most one entry per paired device.
#[derive(Debug, Default)]
pub struct IngestLimiter {
    windows: HashMap<String, (i64, u32)>,
}

impl IngestLimiter {
    /// `Err(retry_after_seconds)` once the device used up this window.
    pub fn check(&mut self, device_id: &str, now_ms: i64) -> Result<(), u64> {
        let window_start = now_ms - now_ms.rem_euclid(INGEST_WINDOW_MS);
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
/// Successful delivery is required before `allow` is called by the endpoint.
#[derive(Debug, Default)]
pub struct AuthLimiter {
    windows: std::collections::HashMap<String, (i64, u32)>,
}

impl AuthLimiter {
    pub fn allow(&mut self, key: &str, now: i64) -> bool {
        let window = now - now.rem_euclid(60_000);
        if self.windows.len() > 10_000 {
            self.windows.retain(|_, (start, _)| *start == window);
        }
        let entry = self.windows.entry(key.to_owned()).or_insert((window, 0));
        if entry.0 != window {
            *entry = (window, 0);
        }
        if entry.1 >= 20 {
            return false;
        }
        entry.1 += 1;
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
            assert!(!limiter.is_blocked(second * 1000));
            limiter.record_failure(second * 1000);
        }
        assert!(limiter.is_blocked(30_000));
        assert!(limiter.is_blocked(CLAIM_FAILURE_WINDOW_MS - 1));
        // The first failure (t = 0) has left the window.
        assert!(!limiter.is_blocked(CLAIM_FAILURE_WINDOW_MS));
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
}
