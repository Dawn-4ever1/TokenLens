use collector_core::{Config, DateRange, event_key_set_digest, scan};

/// Read-only local-data smoke test. No keys, sessions, paths, titles, or values
/// from an event are printed. The ignored opt-in variant covers today's active logs.
#[test]
fn real_local_scans_have_identical_event_key_set_hashes_and_counts() {
    let config = Config::load(&collector_core::default_config_path().expect("config path"))
        .expect("local config");
    let range = DateRange {
        since: Some(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("fixed epoch")),
        until: Some(chrono::Local::now().date_naive() - chrono::Duration::days(1)),
    };
    let first = scan(&config, range).expect("first read-only local scan");
    let second = scan(&config, range).expect("second read-only local scan");
    let first_digest = event_key_set_digest(&first.events);
    let second_digest = event_key_set_digest(&second.events);
    assert_eq!(first_digest, second_digest);
    println!(
        "two closed-history scans matched: events={}, unique_keys={}, key_set_sha256={}",
        first_digest.event_count, first_digest.unique_event_keys, first_digest.sha256
    );

    let qoder_snapshot = |report: &collector_core::ScanReport| {
        let events = report
            .events
            .iter()
            .filter(|event| event.client == "qoder-intl")
            .cloned()
            .collect::<Vec<_>>();
        let digest = event_key_set_digest(&events);
        let credits = events.iter().filter_map(|event| event.credits).sum::<f64>();
        let estimated = events
            .iter()
            .filter(|event| event.precision == usage_event::Precision::Estimated)
            .count();
        let measured = events
            .iter()
            .filter(|event| event.precision == usage_event::Precision::Measured)
            .count();
        (digest, credits, estimated, measured)
    };
    let first_qoder = qoder_snapshot(&first);
    let second_qoder = qoder_snapshot(&second);
    assert_eq!(first_qoder, second_qoder);
    println!(
        "Qoder aggregate only: events={} unique_keys={} credits={:.10} estimated={} measured={} key_set_sha256={}",
        first_qoder.0.event_count,
        first_qoder.0.unique_event_keys,
        first_qoder.1,
        first_qoder.2,
        first_qoder.3,
        first_qoder.0.sha256
    );
}

/// Read-only local Antigravity count verification. Prints integer counts only:
/// no titles, session IDs, keys, or paths are ever written.
#[test]
#[ignore]
fn real_antigravity_metadata_counts_only() {
    use std::collections::BTreeSet;

    let config = Config::load(&collector_core::default_config_path().expect("config path"))
        .expect("local config");
    let report = scan(&config, DateRange::all_history()).expect("full read-only scan");
    for client in ["antigravity-extension", "antigravity-cli"] {
        let events: Vec<_> = report
            .events
            .iter()
            .filter(|event| event.client == client)
            .collect();
        let roots: BTreeSet<&str> = events
            .iter()
            .map(|event| {
                event
                    .parent_session_id
                    .as_deref()
                    .unwrap_or(event.session_id.as_str())
            })
            .collect();
        let titled_roots: BTreeSet<&str> = events
            .iter()
            .filter(|event| event.session_title.is_some())
            .map(|event| {
                event
                    .parent_session_id
                    .as_deref()
                    .unwrap_or(event.session_id.as_str())
            })
            .collect();
        let subagents: BTreeSet<&str> = events
            .iter()
            .filter(|event| event.parent_session_id.is_some())
            .map(|event| event.session_id.as_str())
            .collect();
        let total_tokens: u64 = events
            .iter()
            .map(|event| {
                event.input + event.output + event.cache_read + event.cache_write + event.reasoning
            })
            .sum();
        println!(
            "{client}: events={} root_sessions={} titled_roots={} subagent_sessions={} total_tokens={total_tokens}",
            events.len(),
            roots.len(),
            titled_roots.len(),
            subagents.len(),
        );
    }
}
