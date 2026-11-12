use super::*;
use chrono::FixedOffset;
use collector_core::Tokens;
use serde_json::json;

fn d(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

fn key(client: &str, date: &str) -> Key {
    (client.to_owned(), d(date))
}

fn synthetic_hub() -> Value {
    json!({ "devices": [
        {
            "deviceId": "dev-other",
            "displayName": "  ",
            "hostname": "other-host",
            "history": { "daily": [] }
        },
        {
            "deviceId": "dev-synthetic-7",
            "displayName": "Synthetic Laptop Name",
            "hostname": "synthetic-host.local",
            "updatedAt": "2026-11-10T12:00:00.000Z",
            "periodWindows": { "timeZone": "Asia/Shanghai" },
            "historyAvailable": true,
            "history": { "daily": [
                {
                    "date": "2026-11-01",
                    "tokenComponentsAvailable": true,
                    "perClient": {
                        "codex": { "tokens": 1000, "messages": 4, "cost": 0.1,
                                   "cacheReadTokens": 600, "outputTokens": 150,
                                   "unclassifiedTokens": 0 },
                        "qodercn": { "tokens": 500, "messages": 3, "unclassifiedTokens": 500 },
                        "kilocode": { "tokens": 40, "messages": 1 }
                    }
                },
                {
                    "date": "2026-11-02",
                    "tokenComponentsAvailable": false,
                    "perClient": {
                        "claude": { "tokens": 300, "messages": 1 },
                        "codex": { "tokens": 900, "messages": 2 }
                    }
                },
                {
                    "date": "2026-10-01",
                    "tokenComponentsAvailable": true,
                    "perClient": { "codex": { "tokens": 77, "messages": 1 } }
                }
            ] }
        }
    ] })
}

fn day(client: &str, date: &str, tokens: Tokens, messages: i64, precision: &str) -> DailyUsage {
    DailyUsage {
        client: client.to_owned(),
        date: date.to_owned(),
        messages,
        tokens,
        credits: None,
        precision_counts: BTreeMap::from([(precision.to_owned(), messages as u64)]),
    }
}

fn tokens(input: i64, output: i64, cache_read: i64, cache_write: i64, reasoning: i64) -> Tokens {
    Tokens {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: 0,
        reasoning,
        total: input + output + cache_read + cache_write + reasoning,
    }
}

#[test]
fn hub_daily_rows_become_client_day_cells_with_derived_input() {
    let body = synthetic_hub();
    let device = find_device(&body, "dev-synthetic-7").unwrap();
    let grid = hub_grid(device, d("2026-10-15"), d("2026-11-10"));

    assert_eq!(grid.len(), 5, "the 2026-10-01 row is outside the range");
    assert_eq!(
        grid[&key("codex", "2026-11-01")],
        HubCell {
            tokens: 1000,
            messages: 4,
            components: Some(Components {
                input: 250,
                output: 150,
                cache_read: 600,
                cache_write: 0
            }),
        }
    );
    assert_eq!(grid[&key("qodercn", "2026-11-01")].components, None);
    assert_eq!(grid[&key("qodercn", "2026-11-01")].messages, 3);
    // Exact row without an explicit value means fully classified.
    assert_eq!(
        grid[&key("kilo", "2026-11-01")].components,
        Some(Components {
            input: 40,
            ..Components::default()
        })
    );
    // Not exact and no explicit value: the whole amount is unclassified.
    assert_eq!(grid[&key("codex", "2026-11-02")].components, None);
    assert_eq!(grid[&key("claude", "2026-11-02")].tokens, 300);
}

#[test]
fn our_daily_usage_maps_clients_and_applies_the_hub_reasoning_rule() {
    let daily = vec![
        day(
            "codex",
            "2026-11-01",
            tokens(200, 100, 600, 0, 50),
            4,
            "measured",
        ),
        day(
            "claude",
            "2026-11-01",
            tokens(10, 20, 30, 40, 5),
            2,
            "measured",
        ),
        day(
            collector_core::QODER_CLIENT,
            "2026-11-01",
            tokens(100, 50, 300, 0, 7),
            2,
            "estimated",
        ),
        day(
            "kilocode",
            "2026-11-01",
            tokens(40, 0, 0, 0, 0),
            1,
            "unknown",
        ),
        day("codex", "2026-12-01", tokens(1, 1, 1, 1, 1), 1, "measured"),
    ];
    let grid = our_grid(&daily, d("2026-10-15"), d("2026-11-10"));

    let codex = &grid[&key("codex", "2026-11-01")];
    assert_eq!(codex.native_total, 950);
    assert_eq!(
        codex.hub_view_total, 950,
        "codex adds reasoning in the Hub too"
    );
    assert_eq!(codex.components.output, 150);

    let claude = &grid[&key("claude", "2026-11-01")];
    assert_eq!(claude.native_total, 105);
    assert_eq!(
        claude.hub_view_total, 100,
        "the Hub leaves claude reasoning out"
    );
    assert_eq!(claude.components.output, 20);

    let qoder = &grid[&key("qodercn", "2026-11-01")];
    assert_eq!(qoder.hub_view_total, 450);
    assert_eq!(qoder.precision["estimated"], 2);
    assert!(grid.contains_key(&key("kilo", "2026-11-01")));
    assert_eq!(grid.len(), 4);
}

#[test]
fn client_ids_follow_token_monitor_aliases() {
    assert_eq!(hub_client_for("qoder-intl"), "qodercn");
    assert_eq!(hub_client_for("micode-desktop"), "mimo");
    assert_eq!(hub_client_for("vibe"), "mistral");
    assert_eq!(hub_client_for("Codex"), "codex");
    assert!(hub_counts_reasoning("zcode"));
    assert!(!hub_counts_reasoning("claude"));
    assert!(!hub_counts_reasoning("qodercn"));
}

#[test]
fn request_url_never_contains_the_secret() {
    let secret = "synthetic-secret-value-123";
    let request = HubRequest::devices("https://hub.example/", secret).unwrap();
    assert_eq!(request.url, "https://hub.example/api/devices");
    assert!(!request.url.contains(secret));
    assert_eq!(request.authorization(), format!("Bearer {secret}"));
    let debug = format!("{request:?}");
    assert!(!debug.contains(secret));
    assert!(debug.contains("<redacted>"));

    assert_eq!(
        HubRequest::devices("https://hub.example/?secret=x", secret).unwrap_err(),
        RequestError::BadUrl
    );
    assert_eq!(
        HubRequest::devices("https://user:pw@hub.example", secret).unwrap_err(),
        RequestError::BadUrl
    );
    assert_eq!(
        HubRequest::devices("http://hub.example", secret).unwrap_err(),
        RequestError::BadUrl
    );
    assert_eq!(
        HubRequest::devices("https://hub.example", "  ").unwrap_err(),
        RequestError::MissingSecret
    );
    assert_eq!(
        HubRequest::devices("https://hub.example", "a\r\nX-Injected: 1").unwrap_err(),
        RequestError::MissingSecret
    );
}

#[test]
fn device_list_uses_the_first_nonblank_label() {
    let devices = hub_devices(&synthetic_hub());
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].label, "dev-other");
    assert_eq!(devices[1].label, "Synthetic Laptop Name");
    assert_eq!(devices[1].last_update.as_deref(), Some("2026-11-10"));
    assert_eq!(devices[1].time_zone.as_deref(), Some("Asia/Shanghai"));
    assert_eq!(devices[1].history_days, 3);
    assert!(find_device(&synthetic_hub(), "missing").is_none());
}

#[test]
fn time_zone_check_flags_only_differing_offsets() {
    let east8 = FixedOffset::east_opt(8 * 3600).unwrap();
    let same = tz_mismatch_days("Asia/Shanghai", &east8, d("2026-11-01"), d("2026-11-03")).unwrap();
    assert!(same.is_empty());
    let other =
        tz_mismatch_days("America/New_York", &east8, d("2026-11-01"), d("2026-11-03")).unwrap();
    assert_eq!(other.len(), 3);
    assert!(tz_mismatch_days("Not/AZone", &east8, d("2026-11-01"), d("2026-11-01")).is_none());
}

fn our_cell(total: i64, messages: i64, precision: &str) -> OurCell {
    OurCell {
        native_total: total,
        hub_view_total: total,
        messages,
        components: Components {
            input: total,
            ..Components::default()
        },
        reasoning: 0,
        precision: BTreeMap::from([(precision.to_owned(), messages as u64)]),
    }
}

fn hub_cell(tokens: i64, messages: i64) -> HubCell {
    HubCell {
        tokens,
        messages,
        components: Some(Components {
            input: tokens,
            ..Components::default()
        }),
    }
}

#[test]
fn reconciliation_flags_days_and_explains_known_causes() {
    let ours = BTreeMap::from([
        (key("codex", "2026-11-01"), our_cell(1000, 4, "measured")),
        (key("codex", "2026-11-02"), our_cell(1100, 4, "measured")),
        (key("codex", "2026-11-03"), our_cell(900, 4, "measured")),
        (key("codex", "2026-11-04"), our_cell(50, 1, "measured")),
        (key("openclaw", "2026-11-01"), our_cell(500, 1, "measured")),
        (key("dsh", "2026-11-01"), our_cell(10, 1, "measured")),
        (key("dsh", "2026-11-02"), our_cell(20, 1, "measured")),
    ]);
    let hub = BTreeMap::from([
        (key("codex", "2026-11-01"), hub_cell(1005, 4)),
        (key("codex", "2026-11-02"), hub_cell(1000, 4)),
        (key("codex", "2026-11-03"), hub_cell(1000, 4)),
        (key("openclaw", "2026-11-01"), hub_cell(400, 1)),
        (key("cursor", "2026-11-02"), hub_cell(70, 2)),
    ]);
    let clients = BTreeSet::from(["codex".to_owned(), "openclaw".to_owned(), "dsh".to_owned()]);
    let rec = reconcile(
        &clients,
        &ours,
        &hub,
        d("2026-11-01"),
        d("2026-11-04"),
        Some(BTreeSet::new()),
    );

    let row = |client: &str, date: &str| {
        rec.rows
            .iter()
            .find(|r| r.client == client && r.date == d(date))
            .unwrap()
    };
    assert!(row("codex", "2026-11-01").within());
    assert!(!row("codex", "2026-11-02").within());
    assert!(row("codex", "2026-11-02").causes[0].contains("2026-11-03"));
    assert!(row("codex", "2026-11-04").causes[0].contains("Hub 没有"));
    assert!(row("cursor", "2026-11-02").causes[0].contains("本机没有"));
    assert!(
        row("openclaw", "2026-11-01").causes.is_empty(),
        "nothing explains it"
    );
    assert_eq!(row("openclaw", "2026-11-01").percent, Some(25.0));

    let codex = rec.summaries.iter().find(|s| s.client == "codex").unwrap();
    assert_eq!(codex.precision, ClientPrecision::Measured);
    assert_eq!((codex.days, codex.within), (4, 1));
    assert_eq!(
        codex.worst.unwrap().0,
        d("2026-11-04"),
        "Hub zero is the worst"
    );
    let cursor = rec.summaries.iter().find(|s| s.client == "cursor").unwrap();
    assert_eq!(cursor.precision, ClientPrecision::HubOnly);
    assert_eq!(rec.hub_only_clients, BTreeSet::from(["cursor".to_owned()]));

    let text = render(&rec, &QoderSection::default(), d("2026-11-05"), false);
    assert!(text.contains("未归因（差值 +100）"));
    assert!(text.contains("| codex | measured | 4 | 1 |"));
    assert!(text.contains("### dsh：Hub 在整个范围内都没有这个 client"));
    assert!(text.contains("本机有 2 天数据，合计 30"));
    assert!(!text.contains("### dsh 2026-11-01"));
    assert!(text.contains("### codex 2026-11-04"));
}

#[test]
fn qoder_section_splits_the_change_day_and_estimates_missing_usage() {
    let east8 = FixedOffset::east_opt(8 * 3600).unwrap();
    let at = |date: &str, h: u32, m: u32| {
        east8
            .from_local_datetime(&d(date).and_hms_opt(h, m, 0).unwrap())
            .unwrap()
            .timestamp_millis()
    };
    let ours = BTreeMap::from([
        (key("qodercn", "2026-11-09"), our_cell(800, 2, "estimated")),
        (key("qodercn", "2026-11-10"), our_cell(300, 3, "estimated")),
    ]);
    let hub = BTreeMap::from([
        (key("qodercn", "2026-11-09"), hub_cell(1000, 4)),
        (key("qodercn", "2026-11-10"), hub_cell(400, 4)),
    ]);
    let events = [
        (at("2026-11-09", 10, 0), 800),
        (at("2026-11-10", 20, 1), 100),
        (at("2026-11-10", 20, 2), 150),
        (at("2026-11-10", 23, 0), 50),
    ];
    let without = [
        at("2026-11-09", 11, 0),
        at("2026-11-09", 12, 0),
        at("2026-11-10", 21, 0),
    ];
    let section = qoder_section(
        &east8,
        &ours,
        &hub,
        &events,
        &without,
        d("2026-11-08"),
        d("2026-11-10"),
    );

    assert_eq!(section.days.len(), 2);
    assert_eq!(section.days[0].without_usage, 2);
    assert_eq!(section.periods.len(), 2);
    assert_eq!(section.periods[0].estimate, 2 * 1000 / 4);
    assert_eq!(section.periods[1].estimate, 400 / 4);
    assert_eq!(section.change_day_split, Some(((100, 1), (200, 2))));
}

#[test]
fn report_is_deterministic_and_has_no_device_identity() {
    let body = synthetic_hub();
    let device = find_device(&body, "dev-synthetic-7").unwrap();
    let since = d("2026-10-15");
    let until = d("2026-11-10");
    let daily = vec![
        day(
            "codex",
            "2026-11-01",
            tokens(250, 150, 600, 0, 0),
            4,
            "measured",
        ),
        day(
            collector_core::QODER_CLIENT,
            "2026-11-01",
            tokens(100, 50, 300, 0, 0),
            2,
            "estimated",
        ),
    ];
    let build = || {
        let ours = our_grid(&daily, since, until);
        let hub = hub_grid(device, since, until);
        let clients: BTreeSet<String> = daily.iter().map(|d| d.client.clone()).collect();
        let east8 = FixedOffset::east_opt(8 * 3600).unwrap();
        let tz = tz_mismatch_days(hub_time_zone(device).unwrap(), &east8, since, until);
        let rec = reconcile(&clients, &ours, &hub, since, until, tz);
        let qoder = qoder_section(&east8, &ours, &hub, &[], &[], since, until);
        render(&rec, &qoder, d("2026-11-11"), true)
    };
    let first = build();
    assert_eq!(first, build());
    for forbidden in [
        "dev-synthetic-7",
        "Synthetic Laptop Name",
        "synthetic-host",
        "dev-other",
        "other-host",
    ] {
        assert!(!first.contains(forbidden), "report leaked {forbidden}");
    }
    assert!(first.contains("| qoder-intl | qodercn |"));
    assert!(first.contains("| codex | measured | 2 | 1 |"));
    assert!(first.contains("整个范围内偏移一致"));
}

#[test]
fn number_formatting_is_stable() {
    assert_eq!(group(0), "0");
    assert_eq!(group(1234567), "1,234,567");
    assert_eq!(group(-1000), "-1,000");
    assert_eq!(format_percent(Some(0.001)), "0.00%");
    assert_eq!(format_percent(Some(-2.5)), "-2.50%");
    assert_eq!(format_percent(None), "—（Hub 为 0）");
}
