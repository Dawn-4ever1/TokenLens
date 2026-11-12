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
