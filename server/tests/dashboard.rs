mod common;

use common::{Harness, envelope, heartbeat};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokenlens_server::secrets;

fn utc(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .expect("timestamp")
        .timestamp_millis()
}

#[allow(clippy::too_many_arguments)]
fn event(
    seed: &str,
    client: &str,
    session: &str,
    parent: Option<&str>,
    model: &str,
    ts: &str,
    [input, output, cache_read]: [u64; 3],
    cost: Option<f64>,
    precision: &str,
    turn: bool,
) -> Value {
    let mut value = json!({
        "eventKey": secrets::sha256_hex(seed),
        "client": client,
        "sessionId": session,
        "modelId": model,
        "ts": utc(ts),
        "input": input,
        "output": output,
        "cacheRead": cache_read,
        "cacheWrite": 0,
        "cacheWrite1h": 0,
        "reasoning": 0,
        "costSource": "synthetic",
        "precision": precision,
        "messageCount": 1,
        "isTurnStart": turn,
        "durationMs": 1000
    });
    if let Some(parent) = parent {
        value["parentSessionId"] = json!(parent);
    }
    if let Some(cost) = cost {
        value["costUsd"] = json!(cost);
    }
    if precision == "estimated" {
        value["credits"] = json!(2.5);
        value["rateVersion"] = json!("synthetic-rates-v1");
    }
    value
}

struct World {
    h: Harness,
    a: common::Device,
    b: common::Device,
}

/// Shanghai days: e5 on the 13th, e1 at 23:59 on the 14th, the rest on the
/// 15th (`START_MS` is 2027-01-15 16:00 Shanghai, a Friday).
async fn world_in(zone: chrono_tz::Tz) -> World {
    let h = Harness::start_in(zone).await;
    let a = h.pair().await;
    let b = h.pair().await;
    let device_a = vec![
        event(
            "e1",
            "codex",
            "s1",
            None,
            "m1",
            "2027-01-14T15:59:00Z",
            [100, 10, 300],
            Some(0.5),
            "measured",
            true,
        ),
        event(
            "e2",
            "codex",
            "s1",
            None,
            "m2",
            "2027-01-14T16:01:00Z",
            [50, 5, 100],
            None,
            "measured",
            false,
        ),
        event(
            "e3",
            "codex",
            "s1-sub",
            Some("s1"),
            "m1",
            "2027-01-14T16:05:00Z",
            [10, 1, 0],
            Some(0.1),
            "measured",
            true,
        ),
        event(
            "e4",
            "qoder-intl",
            "q1",
            None,
            "auto",
            "2027-01-15T02:00:00Z",
            [400, 100, 0],
            None,
            "estimated",
            true,
        ),
        event(
            "e5",
            "zcode",
            "z1",
            None,
            "glm",
            "2027-01-13T03:00:00Z",
            [20, 2, 0],
            Some(0.01),
            "unknown",
            true,
        ),
    ];
    let (status, body) = h
        .ingest(&a.ingest, &envelope(&a.id, "batch-a", "g1", device_a))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], 5);
    let device_b = vec![event(
        "b1",
        "codex",
        "s1",
        None,
        "m1",
        "2027-01-15T01:00:00Z",
        [1000, 0, 0],
        Some(1.0),
        "measured",
        true,
    )];
    let (status, _) = h
        .ingest(&b.ingest, &envelope(&b.id, "batch-b", "g1", device_b))
        .await;
    assert_eq!(status, StatusCode::OK);
    World { h, a, b }
}

async fn world() -> World {
    world_in(chrono_tz::Asia::Shanghai).await
}

fn totals(values: &Value) -> Vec<i64> {
    values
        .as_array()
        .expect("array")
        .iter()
        .map(|point| point["totalTokens"].as_i64().expect("total"))
        .collect()
}

#[tokio::test]
async fn only_read_tokens_open_the_dashboard() {
    let w = world().await;
    for path in [
        "/api/dashboard/summary",
        "/api/dashboard/timeseries",
        "/api/dashboard/heatmap",
        "/api/dashboard/sessions",
        "/api/dashboard/devices",
        "/api/dashboard/filters",
        "/api/dashboard/health",
    ] {
        assert_eq!(
            w.h.get(path, None).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        assert_eq!(
            w.h.get(path, Some(&w.a.ingest)).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        assert_eq!(
            w.h.get(path, Some(&w.a.read)).await.0,
            StatusCode::OK,
            "{path}"
        );
    }
    let (status, body) = w.h.get("/healthz", None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"status": "ok"})));
}

#[tokio::test]
async fn summary_adds_up_the_five_buckets() {
    let w = world().await;
    let (status, s) = w.h.get("/api/dashboard/summary", Some(&w.a.read)).await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["inputTokens"], 1580);
    assert_eq!(s["outputTokens"], 118);
    assert_eq!(s["cacheReadTokens"], 400);
    assert_eq!(s["totalTokens"], 2098);
    assert_eq!(s["costUsd"], 1.61);
    // Qoder's 500 tokens carry credits, so they are not "unpriced" (D042).
    assert_eq!(s["unpricedTokens"], 155);
    assert_eq!(s["credits"], 2.5);
    assert_eq!(s["messageCount"], 6);
    // Two devices' "s1" stay apart; the subagent folds into its parent.
    assert_eq!(s["sessionCount"], 4);
    assert_eq!(s["turnCount"], 4);
    assert_eq!(s["activeDays"], 3);
    assert_eq!(s["cacheHitRate"], 0.202020);
    assert_eq!(s["precision"], "mixed");
    assert_eq!(
        s["range"],
        json!({"from": "2027-01-13", "to": "2027-01-15", "timeZone": "Asia/Shanghai"})
    );
    let clients = s["byClient"].as_array().expect("clients");
    assert_eq!(
        clients
            .iter()
            .map(|c| c["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["codex", "qoder-intl", "zcode"]
    );
    assert_eq!(totals(&s["byClient"]), [1576, 500, 22]);
    assert_eq!(clients[1]["precision"], "estimated");
    assert_eq!(clients[1]["costUsd"], Value::Null, "unpriced is not $0");
    assert_eq!(s["byDevice"].as_array().unwrap().len(), 2);
    assert_eq!(s["byProject"], json!([]));
    let codes = s["caveats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    for code in [
        "estimated_qoder",
        "unknown_precision",
        "estimated_cost",
        "unpriced_tokens",
    ] {
        assert!(codes.contains(&code.to_owned()), "{code} in {codes:?}");
    }
    assert!(
        !codes.contains(&"backfill_incomplete".to_owned()),
        "a device that never reported has nothing to backfill yet"
    );
}

#[tokio::test]
async fn days_follow_the_dashboard_time_zone() {
    let w = world().await;
    let (_, s) =
        w.h.get(
            "/api/dashboard/summary?from=2027-01-15&to=2027-01-15",
            Some(&w.a.read),
        )
        .await;
    assert_eq!(
        s["totalTokens"],
        155 + 11 + 500 + 1000,
        "e1 at 23:59 is the 14th"
    );

    let utc = world_in(chrono_tz::UTC).await;
    let (_, s) = utc
        .h
        .get(
            "/api/dashboard/summary?from=2027-01-15&to=2027-01-15",
            Some(&utc.a.read),
        )
        .await;
    assert_eq!(
        s["totalTokens"],
        500 + 1000,
        "16:01Z is still the 14th in UTC"
    );
    assert_eq!(s["range"]["timeZone"], "UTC");

    // UTC+5:30: 15:59Z, 16:01Z and 16:05Z all land in 21:00–22:00 local.
    let india = world_in(chrono_tz::Asia::Kolkata).await;
    let (_, ts) = india
        .h
        .get("/api/dashboard/timeseries", Some(&india.a.read))
        .await;
    assert_eq!(
        ts["buckets"],
        json!(["2027-01-13", "2027-01-14", "2027-01-15"])
    );
    assert_eq!(totals(&ts["series"][0]["points"]), [22, 576, 1500]);
    let (_, hm) = india
        .h
        .get(
            "/api/dashboard/heatmap?kind=hour_weekday",
            Some(&india.a.read),
        )
        .await;
    let cell = |weekday: i64, hour: i64| {
        hm["cells"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["weekday"] == weekday && c["hour"] == hour)
            .map(|c| c["totalTokens"].as_i64().unwrap())
    };
    assert_eq!(cell(3, 21), Some(576));
    assert_eq!(cell(4, 6), Some(1000));
    assert_eq!(cell(4, 7), Some(500));
}

#[tokio::test]
async fn filters_narrow_every_figure() {
    let w = world().await;
    let (_, s) =
        w.h.get("/api/dashboard/summary?clients=qoder-intl", Some(&w.a.read))
            .await;
    assert_eq!(s["totalTokens"], 500);
    assert_eq!(s["precision"], "estimated");
    assert_eq!(s["costUsd"], Value::Null);
    let path = format!("/api/dashboard/summary?devices={}", w.b.id);
    let (_, s) = w.h.get(&path, Some(&w.a.read)).await;
    assert_eq!(s["totalTokens"], 1000);
    assert_eq!(s["precision"], "measured");
    let (_, s) =
        w.h.get(
            "/api/dashboard/summary?models=m1,m2&clients=codex",
            Some(&w.a.read),
        )
        .await;
    assert_eq!(s["totalTokens"], 1576);

    for bad in [
        "/api/dashboard/summary?form=2027-01-01",
        "/api/dashboard/summary?from=2027-01-02&to=2027-01-01",
        "/api/dashboard/summary?from=yesterday",
        "/api/dashboard/summary?clients=a&clients=b",
        "/api/dashboard/timeseries?bucket=minute",
        "/api/dashboard/timeseries?groupBy=user",
        "/api/dashboard/timeseries?limit=0",
        "/api/dashboard/timeseries?bucket=hour&from=2026-01-01&to=2026-12-31",
        "/api/dashboard/heatmap?kind=pie",
        "/api/dashboard/sessions?sort=name",
        "/api/dashboard/sessions?cursor=-1",
        "/api/dashboard/sessions?limit=1000",
    ] {
        assert_eq!(
            w.h.get(bad, Some(&w.a.read)).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn timeseries_buckets_are_dense_and_ranked() {
    let w = world().await;
    let (status, t) =
        w.h.get(
            "/api/dashboard/timeseries?from=2027-01-13&to=2027-01-15&groupBy=client",
            Some(&w.a.read),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(
        t["buckets"],
        json!(["2027-01-13", "2027-01-14", "2027-01-15"])
    );
    let series = t["series"].as_array().unwrap();
    assert_eq!(series[0]["key"], "codex");
    assert_eq!(totals(&series[0]["points"]), [0, 410, 1166]);
    assert_eq!(totals(&series[1]["points"]), [0, 0, 500]);
    assert_eq!(totals(&series[2]["points"]), [22, 0, 0]);

    let (_, t) =
        w.h.get(
            "/api/dashboard/timeseries?from=2027-01-13&to=2027-01-15&groupBy=client&limit=1",
            Some(&w.a.read),
        )
        .await;
    let series = t["series"].as_array().unwrap();
    assert_eq!(series.len(), 2);
    assert_eq!(series[1]["key"], "__other__");
    assert_eq!(totals(&series[1]["points"]), [22, 0, 500]);

    let (_, t) =
        w.h.get(
            "/api/dashboard/timeseries?bucket=hour&from=2027-01-15&to=2027-01-15",
            Some(&w.a.read),
        )
        .await;
    assert_eq!(t["buckets"].as_array().unwrap().len(), 24);
    assert_eq!(t["buckets"][0], "2027-01-15T00:00");
    let points = totals(&t["series"][0]["points"]);
    assert_eq!((points[0], points[9], points[10]), (166, 1000, 500));

    let (_, t) =
        w.h.get("/api/dashboard/timeseries?bucket=week", Some(&w.a.read))
            .await;
    assert_eq!(t["buckets"], json!(["2027-01-11"]), "weeks start on Monday");
    let (_, t) =
        w.h.get("/api/dashboard/timeseries?bucket=month", Some(&w.a.read))
            .await;
    assert_eq!(t["buckets"], json!(["2027-01"]));
    assert_eq!(totals(&t["series"][0]["points"]), [2098]);
}

#[tokio::test]
async fn heatmaps_use_local_days_and_hours() {
    let w = world().await;
    let (_, m) =
        w.h.get("/api/dashboard/heatmap?kind=calendar", Some(&w.a.read))
            .await;
    assert_eq!(totals(&m["days"]), [22, 410, 1666]);
    assert_eq!(m["maxTotalTokens"], 1666);
    let (_, m) =
        w.h.get("/api/dashboard/heatmap?kind=hour_weekday", Some(&w.a.read))
            .await;
    let cells = m["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 7 * 24);
    let at =
        |weekday: usize, hour: usize| cells[weekday * 24 + hour]["totalTokens"].as_i64().unwrap();
    assert_eq!(at(3, 23), 410, "Thursday 23:59");
    assert_eq!(at(4, 0), 166);
    assert_eq!(at(4, 9), 1000);
    assert_eq!(at(4, 10), 500);
    assert_eq!(at(2, 11), 22);
    assert_eq!(
        cells
            .iter()
            .map(|c| c["totalTokens"].as_i64().unwrap())
            .sum::<i64>(),
        2098
    );
}

#[tokio::test]
async fn sessions_group_subagents_sort_and_page() {
    let w = world().await;
    let (status, page) =
        w.h.get(
            "/api/dashboard/sessions?sort=tokens&limit=2",
            Some(&w.a.read),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 4);
    assert_eq!(page["nextCursor"], "2");
    let first = &page["sessions"][0];
    assert_eq!(
        (first["deviceId"].as_str(), first["totalTokens"].as_i64()),
        (Some(w.b.id.as_str()), Some(1000))
    );
    let parent = &page["sessions"][1];
    assert_eq!(parent["sessionId"], "s1");
    assert_eq!(parent["totalTokens"], 576);
    assert_eq!(parent["subagentCount"], 1);
    assert_eq!(parent["turnCount"], 1);
    assert_eq!(parent["models"], json!(["m1", "m2"]));
    assert_eq!(parent["startedAt"], "2027-01-14T15:59:00.000Z");

    let (_, rest) =
        w.h.get(
            "/api/dashboard/sessions?sort=tokens&limit=2&cursor=2",
            Some(&w.a.read),
        )
        .await;
    assert_eq!(totals(&rest["sessions"]), [500, 22]);
    assert_eq!(rest["nextCursor"], Value::Null);

    let (_, recent) = w.h.get("/api/dashboard/sessions", Some(&w.a.read)).await;
    assert_eq!(recent["sessions"][0]["client"], "qoder-intl");
    assert_eq!(recent["sessions"][0]["credits"], 2.5);

    let search = |q: &'static str| {
        let h = &w.h;
        let token = w.a.read.clone();
        async move {
            let (_, page) = h
                .get(&format!("/api/dashboard/sessions?q={q}"), Some(&token))
                .await;
            page["total"].as_i64().unwrap()
        }
    };
    assert_eq!(search("SUB").await, 1, "child session ids are searchable");
    assert_eq!(search("auto").await, 1, "so are models");
    assert_eq!(search("%25").await, 0, "wildcards are literal");
    assert_eq!(search("s1").await, 2);
}

#[tokio::test]
async fn session_detail_lists_turns_without_content() {
    let w = world().await;
    let (_, page) =
        w.h.get("/api/dashboard/sessions?q=sub", Some(&w.a.read))
            .await;
    let key = page["sessions"][0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();
    // Detail ignores range filters: it is always the whole session.
    let (status, d) =
        w.h.get(&format!("/api/dashboard/sessions/{key}"), Some(&w.a.read))
            .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["session"]["totalTokens"], 576);
    assert_eq!(d["session"]["turnCount"], 1);
    let turns = d["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(
        (turns[0]["index"].as_i64(), turns[0]["totalTokens"].as_i64()),
        (Some(1), Some(565))
    );
    assert_eq!(turns[0]["models"], json!(["m1", "m2"]));
    assert_eq!(turns[0]["durationMs"], 2000);
    assert_eq!(turns[0]["isSubagent"], false);
    assert_eq!(turns[1]["isSubagent"], true);
    assert_eq!(turns[1]["subagentSessionId"], "s1-sub");
    assert_eq!(d["subagents"][0]["sessionId"], "s1-sub");
    assert_eq!(d["truncated"], false);
    let text = d.to_string();
    for forbidden in ["prompt", "content", "title", "path"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    for missing in ["nope", "ZGV2X3gfY29kZXgfczE"] {
        let (status, _) =
            w.h.get(
                &format!("/api/dashboard/sessions/{missing}"),
                Some(&w.a.read),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    }
    let (status, _) =
        w.h.get(
            &format!("/api/dashboard/sessions/{key}?from=2027-01-01"),
            Some(&w.a.read),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn devices_report_coverage_and_connection() {
    let w = world().await;
    let (status, _) =
        w.h.post(
            "/api/ingest/heartbeat",
            Some(&w.a.ingest),
            serde_json::to_vec(&heartbeat(&w.a.id)).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut still_uploading = heartbeat(&w.b.id);
    still_uploading["backfillComplete"] = json!(false);
    let (status, _) =
        w.h.post(
            "/api/ingest/heartbeat",
            Some(&w.b.ingest),
            serde_json::to_vec(&still_uploading).unwrap(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let silent = w.h.pair().await;
    let (_, d) = w.h.get("/api/dashboard/devices", Some(&w.a.read)).await;
    let devices = d["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 3);
    let a = &devices[0];
    let find = |id: &str| devices.iter().find(|d| d["deviceId"] == id).unwrap();
    assert_eq!(a["deviceId"], w.a.id.as_str());
    assert_eq!(
        (
            a["isCurrent"].as_bool(),
            find(&w.b.id)["isCurrent"].as_bool()
        ),
        (Some(true), Some(false))
    );
    assert_eq!(a["connection"], "online");
    assert_eq!(a["backfillComplete"], true);
    assert_eq!(a["outdated"], false);
    assert_eq!(a["osName"], "macOS");
    assert_eq!(a["clients"].as_array().unwrap().len(), 3);
    assert_eq!(
        find(&w.b.id)["connection"],
        "online",
        "an upload counts as contact"
    );
    assert_eq!(
        find(&silent.id)["connection"],
        "offline",
        "never seen is waiting, not lost"
    );
    assert_eq!(d["usage"][0]["totalTokens"], 1098);
    assert_eq!(totals(&d["usage"][0]["byClient"]), [576, 500, 22]);
    assert_eq!(
        d["clients"],
        json!(["claude", "codex", "qoder-intl", "zcode"])
    );
    let codes = |d: &Value| {
        d["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert!(codes(&d).contains(&"client_error".to_owned()));
    let backfill = d["caveats"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["code"] == "backfill_incomplete")
        .map(|c| c["deviceId"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(backfill, [w.b.id.as_str()]);
    assert!(!codes(&d).contains(&"device_long_offline".to_owned()));

    w.h.clock
        .advance_ms(tokenlens_server::dashboard::ONLINE_WINDOW_MS + 1);
    let (_, d) = w.h.get("/api/dashboard/devices", Some(&w.a.read)).await;
    assert_eq!(d["devices"][0]["connection"], "offline");
    w.h.clock
        .advance_ms(tokenlens_server::dashboard::LONG_OFFLINE_MS);
    let (_, d) = w.h.get("/api/dashboard/devices", Some(&w.a.read)).await;
    assert_eq!(d["devices"][0]["connection"], "long_offline");
    assert!(codes(&d).contains(&"device_long_offline".to_owned()));
}

async fn act(h: &Harness, id: &str, verb: &str, token: &str) -> StatusCode {
    let path = format!("/api/dashboard/devices/{id}/{verb}");
    h.post(&path, Some(token), Vec::new(), None).await.0
}

#[tokio::test]
async fn devices_can_be_renamed_rotated_and_revoked() {
    let w = world().await;
    let rename = |id: String, name: &'static str| {
        let h = &w.h;
        let token = w.a.read.clone();
        async move {
            h.post(
                &format!("/api/dashboard/devices/{id}/rename"),
                Some(&token),
                serde_json::to_vec(&json!({"displayName": name})).unwrap(),
                None,
            )
            .await
            .0
        }
    };
    assert_eq!(rename(w.b.id.clone(), "  Office PC ").await, StatusCode::OK);
    assert_eq!(rename(w.b.id.clone(), "   ").await, StatusCode::BAD_REQUEST);
    assert_eq!(
        rename("dev_missing".to_owned(), "X").await,
        StatusCode::NOT_FOUND
    );
    let (_, s) =
        w.h.get(
            &format!("/api/dashboard/summary?devices={}", w.b.id),
            Some(&w.a.read),
        )
        .await;
    assert_eq!(s["byDevice"][0]["label"], "Office PC");

    assert_eq!(
        act(&w.h, &w.b.id, "rotate", &w.a.read).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        act(&w.h, &w.a.id, "rotate", &w.a.ingest).await,
        StatusCode::UNAUTHORIZED
    );
    let response =
        w.h.http
            .post(format!(
                "{}/api/dashboard/devices/{}/rotate",
                w.h.base, w.a.id
            ))
            .bearer_auth(&w.a.read)
            .send()
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let fresh = response.json::<Value>().await.unwrap();
    let (new_read, new_ingest) = (
        fresh["readToken"].as_str().unwrap(),
        fresh["ingestToken"].as_str().unwrap(),
    );
    assert_eq!(
        w.h.get("/api/dashboard/health", Some(&w.a.read)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.h.get("/api/dashboard/health", Some(new_read)).await.0,
        StatusCode::OK
    );
    let beat = serde_json::to_vec(&heartbeat(&w.a.id)).unwrap();
    assert_eq!(
        w.h.post(
            "/api/ingest/heartbeat",
            Some(&w.a.ingest),
            beat.clone(),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.h.post("/api/ingest/heartbeat", Some(new_ingest), beat, None)
            .await
            .0,
        StatusCode::OK
    );

    assert_eq!(act(&w.h, &w.b.id, "revoke", new_read).await, StatusCode::OK);
    assert_eq!(
        act(&w.h, &w.b.id, "revoke", new_read).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        w.h.get("/api/dashboard/health", Some(&w.b.read)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, d) = w.h.get("/api/dashboard/devices", Some(new_read)).await;
    assert_eq!(d["devices"].as_array().unwrap().len(), 1);
    let (_, s) = w.h.get("/api/dashboard/summary", Some(new_read)).await;
    assert_eq!(s["totalTokens"], 2098, "a revoked device's history stays");
}

#[tokio::test]
async fn a_re_paired_computer_counts_each_event_once() {
    let w = world().await;
    assert_eq!(
        act(&w.h, &w.b.id, "revoke", &w.a.read).await,
        StatusCode::OK
    );
    w.h.clock.advance_ms(60_000);
    let c = w.h.pair().await;
    let again = |seed: &str, input: u64| {
        event(
            seed,
            "codex",
            "s1",
            None,
            "m1",
            "2027-01-15T01:00:00Z",
            [input, 0, 0],
            Some(1.0),
            "measured",
            true,
        )
    };
    let (status, body) =
        w.h.ingest(
            &c.ingest,
            &envelope(
                &c.id,
                "batch-c",
                "g1",
                vec![again("b1", 1000), again("c1", 7)],
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], 2, "stored per device as before");
    let by_device = |s: &Value| {
        s["byDevice"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["key"].as_str().unwrap().to_owned(),
                    d["totalTokens"].as_i64().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let (_, s) = w.h.get("/api/dashboard/summary", Some(&w.a.read)).await;
    assert_eq!(s["totalTokens"], 2098 + 7);
    assert_eq!(s["messageCount"], 7);
    assert_eq!(
        by_device(&s),
        [(w.a.id.clone(), 1098), (c.id.clone(), 1007)],
        "the active replacement owns the shared event"
    );

    // Two active devices with the same event: the older one owns it.
    let (status, _) =
        w.h.ingest(
            &w.a.ingest,
            &envelope(&w.a.id, "batch-a2", "g1", vec![again("c1", 7)]),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, s) = w.h.get("/api/dashboard/summary", Some(&w.a.read)).await;
    assert_eq!(s["totalTokens"], 2098 + 7);
    assert_eq!(
        by_device(&s),
        [(w.a.id.clone(), 1105), (c.id.clone(), 1000)]
    );
    let (_, d) = w.h.get("/api/dashboard/sessions", Some(&w.a.read)).await;
    let session_total = d["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["totalTokens"].as_i64().unwrap())
        .sum::<i64>();
    assert_eq!(session_total, 2098 + 7);
}

#[tokio::test]
async fn health_and_filter_options() {
    let w = world().await;
    let (_, health) = w.h.get("/api/dashboard/health", Some(&w.a.read)).await;
    assert_eq!(health["eventCount"], 6);
    assert_eq!(health["deviceCount"], 2);
    assert_eq!(health["schemaVersion"], 5);
    assert_eq!(health["timeZone"], "Asia/Shanghai");
    assert!(
        health.get("databaseBytes").is_none(),
        "the file size would reveal other tenants"
    );
    assert!(health["lastIngestAt"].is_string());
    let (_, options) = w.h.get("/api/dashboard/filters", Some(&w.a.read)).await;
    assert_eq!(
        options["clients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["codex", "qoder-intl", "zcode"]
    );
    assert_eq!(options["devices"].as_array().unwrap().len(), 2);
    assert_eq!(options["firstEventDate"], "2027-01-13");
}
