//! In-memory `event_key -> (client, local date, value hash)` snapshot.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::NaiveDate;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::DateRange;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedEvent {
    pub key: String,
    pub client: String,
    pub ts: i64,
    pub hash: [u8; 32],
    /// Canonical whitelisted JSON; only kept for events that may need upload.
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    client: u16,
    day: Option<NaiveDate>,
    hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MergeStats {
    pub new: u64,
    pub updated: u64,
    pub unchanged: u64,
    /// In scope but not seen again; kept, only counted.
    pub vanished: u64,
}

#[derive(Debug, Clone)]
pub struct Scope {
    /// `None` = every client.
    pub clients: Option<BTreeSet<String>>,
    pub range: DateRange,
}

/// Matches the upstream date filter, which buckets by the local timezone.
pub fn local_day(ts: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp_millis(ts)
        .map(|time| time.with_timezone(&chrono::Local).date_naive())
}

fn in_range(day: Option<NaiveDate>, range: DateRange) -> bool {
    let Some(day) = day else {
        return range.since.is_none() && range.until.is_none();
    };
    range.since.is_none_or(|since| day >= since) && range.until.is_none_or(|until| day <= until)
}

#[derive(Debug, Default)]
pub struct Snapshot {
    entries: HashMap<String, Entry>,
    clients: Vec<String>,
}

impl Snapshot {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn client_index(&mut self, client: &str) -> u16 {
        if let Some(index) = self.clients.iter().position(|known| known == client) {
            return index as u16;
        }
        self.clients.push(client.to_owned());
        (self.clients.len() - 1) as u16
    }

    pub fn hash_of(&self, key: &str) -> Option<[u8; 32]> {
        self.entries.get(key).map(|entry| entry.hash)
    }

    /// Restores entries already handed to the outbox in an earlier run.
    pub fn seed(&mut self, key: String, client: &str, ts: i64, hash: [u8; 32]) {
        let entry = Entry {
            client: self.client_index(client),
            day: local_day(ts),
            hash,
        };
        self.entries.insert(key, entry);
    }

    /// Drops entries so the next scan reports them as new again.
    pub fn forget<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) {
        for key in keys {
            self.entries.remove(key);
        }
    }

    pub fn merge(&mut self, scope: &Scope, scanned: Vec<ScannedEvent>) -> MergeStats {
        self.merge_changes(scope, scanned).0
    }

    /// Like `merge`, also returning the new and updated events.
    pub fn merge_changes(
        &mut self,
        scope: &Scope,
        scanned: Vec<ScannedEvent>,
    ) -> (MergeStats, Vec<ScannedEvent>) {
        let mut stats = MergeStats::default();
        let mut changed = Vec::new();
        let mut seen = HashSet::with_capacity(scanned.len());
        for event in scanned {
            let entry = Entry {
                client: self.client_index(&event.client),
                day: local_day(event.ts),
                hash: event.hash,
            };
            let is_change = match self.entries.get(&event.key) {
                Some(existing) if existing.hash == entry.hash => {
                    stats.unchanged += 1;
                    false
                }
                Some(_) => {
                    stats.updated += 1;
                    true
                }
                None => {
                    stats.new += 1;
                    true
                }
            };
            seen.insert(event.key.clone());
            self.entries.insert(event.key.clone(), entry);
            if is_change {
                changed.push(event);
            }
        }
        let clients = &self.clients;
        stats.vanished = self
            .entries
            .iter()
            .filter(|(key, entry)| {
                !seen.contains(*key)
                    && in_range(entry.day, scope.range)
                    && scope.clients.as_ref().is_none_or(|selected| {
                        selected.contains(&clients[usize::from(entry.client)])
                    })
            })
            .count() as u64;
        (stats, changed)
    }

    pub fn clients_with_events(&self) -> BTreeSet<String> {
        self.entries
            .values()
            .map(|entry| self.clients[usize::from(entry.client)].clone())
            .collect()
    }

    /// Same construction as `event_key_set_digest`.
    pub fn key_set_sha256(&self) -> String {
        let keys = self.entries.keys().collect::<BTreeSet<_>>();
        let mut hasher = Sha256::new();
        for key in keys {
            hasher.update(key.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Same construction as `event_value_digest`.
    pub fn value_sha256(&self) -> String {
        crate::value_digest_of_pairs(
            self.entries
                .iter()
                .map(|(key, entry)| (key.as_str(), entry.hash)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    fn event(key: &str, client: &str, ts: i64, value: u8) -> ScannedEvent {
        ScannedEvent {
            key: key.to_owned(),
            client: client.to_owned(),
            ts,
            hash: [value; 32],
            payload: None,
        }
    }

    fn scope(clients: &[&str], since: i64, until: i64) -> Scope {
        Scope {
            clients: Some(clients.iter().map(|client| (*client).to_owned()).collect()),
            range: DateRange {
                since: local_day(since),
                until: local_day(until),
            },
        }
    }

    #[test]
    fn merge_counts_new_updated_unchanged_and_keeps_vanished() {
        let t = 1_793_577_600_000;
        let mut snapshot = Snapshot::default();
        let all = Scope {
            clients: None,
            range: DateRange::all_history(),
        };
        let stats = snapshot.merge(
            &all,
            vec![
                event("old", "codex", t - 10 * DAY_MS, 1),
                event("a", "codex", t, 1),
                event("b", "codex", t, 2),
                event("c", "claude", t, 3),
            ],
        );
        assert_eq!(stats.new, 4);
        let triggered = scope(&["codex"], t - DAY_MS, t);
        let stats = snapshot.merge(
            &triggered,
            vec![event("a", "codex", t, 1), event("d", "codex", t, 4)],
        );
        // "b" is in scope and missing; "old" is out of range; "c" is another client.
        assert_eq!(
            stats,
            MergeStats {
                new: 1,
                updated: 0,
                unchanged: 1,
                vanished: 1
            }
        );
        assert_eq!(snapshot.len(), 5);
        let (stats, changed) = snapshot.merge_changes(
            &triggered,
            vec![event("a", "codex", t, 9), event("d", "codex", t, 4)],
        );
        assert_eq!((stats.updated, stats.unchanged, stats.vanished), (1, 1, 1));
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].key, "a");
    }

    #[test]
    fn seeded_entries_are_unchanged_and_forgotten_ones_are_new() {
        let t = 1_793_577_600_000;
        let mut snapshot = Snapshot::default();
        snapshot.seed("a".into(), "codex", t, [1; 32]);
        snapshot.seed("b".into(), "codex", t, [2; 32]);
        assert_eq!(snapshot.hash_of("a"), Some([1; 32]));
        snapshot.forget(["b"]);
        let all = Scope {
            clients: None,
            range: DateRange::all_history(),
        };
        let (stats, changed) = snapshot.merge_changes(
            &all,
            vec![event("a", "codex", t, 1), event("b", "codex", t, 2)],
        );
        assert_eq!((stats.new, stats.unchanged), (1, 1));
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn digests_match_the_scan_digest_functions() {
        let mut snapshot = Snapshot::default();
        let all = Scope {
            clients: None,
            range: DateRange::all_history(),
        };
        let mut message = crate::map_message(
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
        .expect("event");
        let first = message.clone();
        message.event_key = "synthetic-second".into();
        let events = vec![first, message];
        snapshot.merge(
            &all,
            events
                .iter()
                .map(|event| ScannedEvent {
                    key: event.event_key.clone(),
                    client: event.client.clone(),
                    ts: event.ts,
                    hash: crate::event_value_hash(event),
                    payload: None,
                })
                .collect(),
        );
        let digest = crate::EventDigests::of(&events);
        assert_eq!(snapshot.key_set_sha256(), digest.key_set_sha256);
        assert_eq!(snapshot.value_sha256(), digest.value_sha256);
        assert_eq!(snapshot.clients_with_events(), ["codex".to_owned()].into());
    }
}
