//! Subscription quotas that the agents already keep on this computer (D082).
//!
//! Read on demand while the window asks for them: no background task, no
//! network request and no credentials. Codex records `rate_limits` in the
//! `token_count` events of its session logs; Claude Code caches its own usage
//! lookup as `cachedUsageUtilization` in its global config file. Only
//! percentages, window lengths, reset times and a plan name leave this
//! module, never an account id, e-mail address, path or log content.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::commands::AppState;
use crate::remote::{CmdResult, CommandError};

const SESSION_MINUTES: u32 = 5 * 60;
const WEEK_MINUTES: u32 = 7 * 24 * 60;
/// Every Codex turn writes a `token_count` event, so the latest reading sits
/// near the end of the newest log; logs are read from their end only.
const CODEX_TAIL_BYTES: u64 = 4 * 1024 * 1024;
const CODEX_MAX_LOGS: usize = 50_000;
const CODEX_MAX_READS: usize = 24;
/// File times and event times come from the same clock, give or take a write.
const CODEX_CLOCK_SLACK: Duration = Duration::from_secs(60);
const CLAUDE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const LABEL_MAX_CHARS: usize = 24;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quotas {
    pub subscriptions: Vec<Subscription>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    /// Client id as the dashboard names it.
    pub client: &'static str,
    pub plan: Option<String>,
    /// When the agent itself last read its quota (RFC 3339, UTC).
    pub updated_at: Option<String>,
    pub status: Status,
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    /// The agent keeps a quota, but in a shape this version cannot read.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Window length; `None` when the agent does not say.
    pub minutes: Option<u32>,
    /// The models a separate allowance covers ("Opus"); `None` for all.
    pub scope: Option<String>,
    /// 0–100.
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

/// Where each agent keeps its quota, resolved like the collector's roots.
pub struct Sources {
    pub codex_home: PathBuf,
    pub claude_config: PathBuf,
}

impl Sources {
    pub fn resolve(home: &Path, use_env_roots: bool) -> Self {
        let env = |name: &str| {
            use_env_roots
                .then(|| std::env::var_os(name))
                .flatten()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        Self {
            codex_home: env("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            claude_config: env("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| home.to_path_buf())
                .join(".claude.json"),
        }
    }
}

pub fn read(sources: &Sources) -> Quotas {
    let mut subscriptions = Vec::new();
    subscriptions.extend(claude(&sources.claude_config));
    subscriptions.extend(codex(&sources.codex_home));
    Quotas { subscriptions }
}

#[tauri::command]
pub async fn subscription_quotas(app: AppHandle) -> CmdResult<Quotas> {
    let (home, use_env_roots) = match app.state::<AppState>().paths.home.clone() {
        Some(home) => (home, false),
        None => match dirs::home_dir() {
            Some(home) => (home, true),
            None => {
                return Ok(Quotas {
                    subscriptions: Vec::new(),
                });
            }
        },
    };
    tauri::async_runtime::spawn_blocking(move || read(&Sources::resolve(&home, use_env_roots)))
        .await
        .map_err(|_| CommandError::new("server"))
}

// ---- Codex ----------------------------------------------------------------

struct CodexReading {
    at: DateTime<Utc>,
    plan: Option<String>,
    windows: Vec<Window>,
}

enum Parsed {
    Found(CodexReading),
    /// Sessions signed in with an API key log empty limits.
    Empty,
    Unreadable,
}

fn codex(codex_home: &Path) -> Option<Subscription> {
    let mut logs = Vec::new();
    collect_logs(&codex_home.join("sessions"), 4, &mut logs);
    collect_logs(&codex_home.join("archived_sessions"), 1, &mut logs);
    logs.sort_by_key(|log| std::cmp::Reverse(log.0));
    let mut best: Option<CodexReading> = None;
    let mut unreadable = false;
    for (modified, path) in logs.iter().take(CODEX_MAX_READS) {
        if let Some(found) = &best {
            // A log last written before the best reading cannot hold a newer one.
            let written = DateTime::<Utc>::from(*modified + CODEX_CLOCK_SLACK);
            if written < found.at {
                break;
            }
        }
        match codex_log(path) {
            Parsed::Found(reading) => {
                if best.as_ref().is_none_or(|found| reading.at > found.at) {
                    best = Some(reading);
                }
            }
            Parsed::Unreadable => unreadable = true,
            Parsed::Empty => {}
        }
    }
    match best {
        Some(reading) => Some(Subscription {
            client: "codex",
            plan: reading.plan,
            updated_at: Some(iso(reading.at)),
            status: Status::Ok,
            windows: sorted(reading.windows),
        }),
        None if unreadable => Some(Subscription {
            client: "codex",
            plan: None,
            updated_at: None,
            status: Status::Unreadable,
            windows: Vec::new(),
        }),
        None => None,
    }
}

/// `*.jsonl` files below `dir`, without following symbolic links.
fn collect_logs(dir: &Path, depth: u32, out: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= CODEX_MAX_LOGS {
            return;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth > 1 {
                collect_logs(&path, depth - 1, out);
            }
        } else if kind.is_file()
            && path.extension().is_some_and(|ext| ext == "jsonl")
            && let Ok(modified) = entry.metadata().and_then(|meta| meta.modified())
        {
            out.push((modified, path));
        }
    }
}

/// The latest reading in one log, scanning its end backwards.
fn codex_log(path: &Path) -> Parsed {
    let Ok(text) = read_tail(path, CODEX_TAIL_BYTES) else {
        return Parsed::Empty;
    };
    let mut unreadable = false;
    for line in text.lines().rev() {
        if !line.contains("\"rate_limits\"") {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            unreadable = true;
            continue;
        };
        match parse_codex(&event) {
            Parsed::Found(reading) => return Parsed::Found(reading),
            Parsed::Unreadable => unreadable = true,
            Parsed::Empty => {}
        }
    }
    if unreadable {
        Parsed::Unreadable
    } else {
        Parsed::Empty
    }
}

/// The last `limit` bytes of a file, from the first complete line.
fn read_tail(path: &Path, limit: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(limit);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    file.take(limit).read_to_end(&mut bytes)?;
    if start > 0 {
        let first = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..first);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_codex(event: &Value) -> Parsed {
    let Some(limits) = event
        .get("payload")
        .and_then(|payload| payload.get("rate_limits"))
    else {
        return Parsed::Empty;
    };
    if limits.is_null() {
        return Parsed::Empty;
    }
    if !limits.is_object() {
        return Parsed::Unreadable;
    }
    let Some(at) = event
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_time)
    else {
        return Parsed::Unreadable;
    };
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(window) = limits.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(used) = window.get("used_percent").and_then(Value::as_f64) else {
            return Parsed::Unreadable;
        };
        let minutes = window
            .get("window_minutes")
            .and_then(Value::as_u64)
            .and_then(|minutes| u32::try_from(minutes).ok())
            .filter(|minutes| *minutes > 0);
        let resets_at = window
            .get("resets_at")
            .and_then(Value::as_i64)
            .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
            .or_else(|| {
                let seconds = window.get("resets_in_seconds").and_then(Value::as_i64)?;
                at.checked_add_signed(chrono::Duration::try_seconds(seconds)?)
            });
        windows.push(Window {
            minutes,
            scope: None,
            used_percent: percent(used),
            resets_at: resets_at.map(iso),
        });
    }
    if windows.is_empty() {
        return Parsed::Empty;
    }
    let plan = limits
        .get("plan_type")
        .and_then(Value::as_str)
        .and_then(codex_plan);
    Parsed::Found(CodexReading { at, plan, windows })
}

fn codex_plan(raw: &str) -> Option<String> {
    let known = match raw.trim().to_ascii_lowercase().as_str() {
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "pro" => "Pro",
        "team" => "Team",
        "business" => "Business",
        "enterprise" => "Enterprise",
        "edu" | "education" => "Edu",
        _ => return clean_label(raw),
    };
    Some(known.to_owned())
}

// ---- Claude Code ----------------------------------------------------------

fn claude(config: &Path) -> Option<Subscription> {
    let meta = std::fs::metadata(config).ok()?;
    if !meta.is_file() || meta.len() > CLAUDE_MAX_BYTES {
        return None;
    }
    // Claude Code rewrites this file often; a read that lands mid-write is
    // retried once instead of dropping the card for a refresh.
    let root = read_json(config).or_else(|| {
        std::thread::sleep(Duration::from_millis(80));
        read_json(config)
    })?;
    parse_claude(&root)
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn parse_claude(root: &Value) -> Option<Subscription> {
    let cache = root
        .get("cachedUsageUtilization")
        .filter(|value| value.is_object())?;
    let account = root.get("oauthAccount");
    let current = account
        .and_then(|a| a.get("accountUuid"))
        .and_then(Value::as_str);
    // A cache left behind by another account, or after signing out, is not
    // this computer's current subscription.
    if let Some(cached) = cache.get("accountUuid").and_then(Value::as_str)
        && current != Some(cached)
    {
        return None;
    }
    let updated_at = cache
        .get("fetchedAtMs")
        .and_then(Value::as_i64)
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .map(iso);
    let plan = account
        .and_then(|a| a.get("organizationRateLimitTier"))
        .and_then(Value::as_str)
        .and_then(claude_plan);
    let utilization = cache.get("utilization").filter(|value| value.is_object());
    let known = utilization.is_some_and(|u| {
        ["limits", "five_hour", "seven_day"]
            .iter()
            .any(|key| u.get(key).is_some())
    });
    if !known {
        return Some(Subscription {
            client: "claude",
            plan,
            updated_at,
            status: Status::Unreadable,
            windows: Vec::new(),
        });
    }
    let windows = utilization.map(claude_windows).unwrap_or_default();
    // Accounts without subscription limits (API billing) have nothing to show.
    if windows.is_empty() {
        return None;
    }
    Some(Subscription {
        client: "claude",
        plan,
        updated_at,
        status: Status::Ok,
        windows: sorted(windows),
    })
}

fn claude_windows(utilization: &Value) -> Vec<Window> {
    // The meter list Claude Code's own `/usage` draws from; when it has rows
    // they are the whole answer, even if none of them is shown.
    if let Some(items) = utilization
        .get("limits")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    {
        return items.iter().filter_map(claude_limit).collect();
    }
    // The older shape, which `/api/oauth/usage` still returns.
    [
        ("five_hour", SESSION_MINUTES, None),
        ("seven_day", WEEK_MINUTES, None),
        ("seven_day_opus", WEEK_MINUTES, Some("Opus")),
        ("seven_day_sonnet", WEEK_MINUTES, Some("Sonnet")),
        // The week of the models that draw on usage credits past it, which
        // Claude Code calls the Fable limit.
        ("seven_day_overage_included", WEEK_MINUTES, Some("Fable")),
    ]
    .into_iter()
    .filter_map(|(key, minutes, scope)| {
        let window = utilization.get(key)?;
        Some(Window {
            minutes: Some(minutes),
            scope: scope.map(str::to_owned),
            used_percent: percent(window.get("utilization").and_then(Value::as_f64)?),
            resets_at: window
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_time)
                .map(iso),
        })
    })
    .collect()
}

/// One `limits` row, shown exactly when Claude Code's `/usage` shows it:
/// the session, the week of all models, and a week scoped to one model
/// (`scope.model.display_name`, such as "Fable"). Rows scoped to a product
/// surface and kinds it does not know are left out, as `/usage` does.
fn claude_limit(item: &Value) -> Option<Window> {
    let used = item.get("percent").and_then(Value::as_f64)?;
    let (minutes, scope) = match item.get("kind").and_then(Value::as_str)? {
        "session" => (SESSION_MINUTES, None),
        "weekly_all" => (WEEK_MINUTES, None),
        "weekly_scoped" => (WEEK_MINUTES, Some(model_scope(item.get("scope")?)?)),
        _ => return None,
    };
    Some(Window {
        minutes: Some(minutes),
        scope,
        used_percent: percent(used),
        resets_at: item
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(parse_time)
            .map(iso),
    })
}

fn model_scope(scope: &Value) -> Option<String> {
    let name = scope.get("model")?.get("display_name")?.as_str()?.trim();
    // `/usage` skips a name it cannot put in its own parentheses.
    if name.contains(['(', ')']) {
        return None;
    }
    clean_label(name)
}

fn claude_plan(tier: &str) -> Option<String> {
    let tier = tier.to_ascii_lowercase();
    if tier.contains("max_20x") {
        Some("Max 20x".to_owned())
    } else if tier.contains("max_5x") {
        Some("Max 5x".to_owned())
    } else {
        None
    }
}

// ---- Shared ---------------------------------------------------------------

fn sorted(mut windows: Vec<Window>) -> Vec<Window> {
    windows.sort_by(|a, b| {
        (a.minutes.unwrap_or(u32::MAX), a.scope.is_some(), &a.scope).cmp(&(
            b.minutes.unwrap_or(u32::MAX),
            b.scope.is_some(),
            &b.scope,
        ))
    });
    windows
}

fn percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn iso(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A short display name from agent data: letters, digits, spaces, `.` and
/// `-` only, so nothing else from the source file reaches the window.
fn clean_label(raw: &str) -> Option<String> {
    let mut label: String = raw
        .trim()
        .chars()
        .map(|c| if c == '_' { ' ' } else { c })
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-'))
        .take(LABEL_MAX_CHARS)
        .collect();
    label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = label.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn codex_event(at: &str, limits: Value) -> String {
        json!({
            "timestamp": at,
            "type": "event_msg",
            "payload": { "type": "token_count", "info": null, "rate_limits": limits },
        })
        .to_string()
    }

    fn codex_limits(primary: f64, secondary: f64) -> Value {
        json!({
            "limit_id": "codex",
            "primary": { "used_percent": primary, "window_minutes": 300, "resets_at": 1_795_800_000 },
            "secondary": { "used_percent": secondary, "window_minutes": 10080, "resets_at": 1_796_300_000 },
            "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
            "plan_type": "plus",
        })
    }

    fn empty_limits() -> Value {
        json!({ "limit_id": "codex", "primary": null, "secondary": null, "plan_type": null })
    }

    fn write_log(dir: &Path, name: &str, lines: &[String], modified: SystemTime) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file.set_modified(modified).unwrap();
        path
    }

    fn at(text: &str) -> SystemTime {
        parse_time(text).unwrap().into()
    }

    #[test]
    fn codex_uses_the_latest_subscription_reading_across_logs() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/11/27");
        write_log(
            &day,
            "rollout-old.jsonl",
            &[
                r#"{"timestamp":"2026-11-27T01:00:00Z","type":"session_meta","payload":{}}"#
                    .to_owned(),
                codex_event("2026-11-27T01:00:05Z", codex_limits(1.0, 2.0)),
                codex_event("2026-11-27T02:00:00Z", codex_limits(3.0, 6.0)),
            ],
            at("2026-11-27T02:00:01Z"),
        );
        // A later session signed in with an API key logs only empty limits.
        write_log(
            &day,
            "rollout-new.jsonl",
            &[codex_event("2026-11-27T03:00:00Z", empty_limits())],
            at("2026-11-27T03:00:01Z"),
        );
        let found = codex(home.path()).unwrap();
        assert_eq!(found.status, Status::Ok);
        assert_eq!(found.plan.as_deref(), Some("Plus"));
        assert_eq!(found.updated_at.as_deref(), Some("2026-11-27T02:00:00Z"));
        assert_eq!(
            found.windows,
            vec![
                Window {
                    minutes: Some(300),
                    scope: None,
                    used_percent: 3.0,
                    resets_at: Some("2026-11-27T17:20:00Z".to_owned()),
                },
                Window {
                    minutes: Some(10080),
                    scope: None,
                    used_percent: 6.0,
                    resets_at: Some("2026-12-03T12:13:20Z".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn codex_prefers_newer_events_over_file_order_and_reads_archives() {
        let home = tempfile::tempdir().unwrap();
        write_log(
            &home.path().join("sessions/2026/11/26"),
            "rollout-a.jsonl",
            &[codex_event(
                "2026-11-26T10:00:00Z",
                codex_limits(10.0, 20.0),
            )],
            at("2026-11-26T10:00:00Z"),
        );
        write_log(
            &home.path().join("archived_sessions"),
            "rollout-b.jsonl",
            &[codex_event(
                "2026-11-26T12:00:00Z",
                codex_limits(40.0, 50.0),
            )],
            at("2026-11-26T12:00:00Z"),
        );
        let found = codex(home.path()).unwrap();
        assert_eq!(found.windows[0].used_percent, 40.0);
        assert_eq!(found.updated_at.as_deref(), Some("2026-11-26T12:00:00Z"));
    }

    #[test]
    fn codex_reads_relative_resets_and_skips_a_cut_first_line() {
        let home = tempfile::tempdir().unwrap();
        let limits = json!({
            "primary": { "used_percent": 120.0, "window_minutes": 300, "resets_in_seconds": 600 },
            "secondary": null,
        });
        let filler = format!(r#"{{"rate_limits": "{}"}}"#, "x".repeat(200));
        let mut lines = vec![filler; 10];
        lines.push(codex_event("2026-11-27T05:00:00Z", limits));
        let path = write_log(
            &home.path().join("sessions/2026/11/27"),
            "rollout.jsonl",
            &lines,
            at("2026-11-27T05:00:00Z"),
        );
        // A tail that starts inside the filler must not misread it.
        let text = read_tail(&path, 700).unwrap();
        assert!(text.lines().all(|line| line.starts_with('{')));
        let Parsed::Found(reading) = codex_log(&path) else {
            panic!("expected a reading");
        };
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0].used_percent, 100.0);
        assert_eq!(
            reading.windows[0].resets_at.as_deref(),
            Some("2026-11-27T05:10:00Z")
        );
        assert_eq!(reading.plan, None);
    }

    #[test]
    fn codex_without_any_subscription_reading_shows_nothing() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(codex(home.path()), None);
        write_log(
            &home.path().join("sessions/2026/11/27"),
            "rollout.jsonl",
            &[codex_event("2026-11-27T05:00:00Z", empty_limits())],
            at("2026-11-27T05:00:00Z"),
        );
        assert_eq!(codex(home.path()), None);
    }

    #[test]
    fn codex_in_an_unknown_shape_is_reported_as_unreadable() {
        let home = tempfile::tempdir().unwrap();
        write_log(
            &home.path().join("sessions/2026/11/27"),
            "rollout.jsonl",
            &[codex_event(
                "2026-11-27T05:00:00Z",
                json!({ "primary": { "share": 0.2 } }),
            )],
            at("2026-11-27T05:00:00Z"),
        );
        let found = codex(home.path()).unwrap();
        assert_eq!(found.status, Status::Unreadable);
        assert!(found.windows.is_empty());
    }

    fn claude_root(cache: Value) -> Value {
        json!({
            "oauthAccount": {
                "accountUuid": "acc-1",
                "emailAddress": "someone@example.com",
                "organizationRateLimitTier": "default_claude_max_5x",
            },
            "cachedUsageUtilization": cache,
        })
    }

    #[test]
    fn claude_shows_the_same_meters_as_its_own_usage_screen() {
        // Rows as Claude Code 2.1.328 keeps them (`kind`, `scope.model`).
        let root = claude_root(json!({
            "fetchedAtMs": 1_795_768_760_279_i64,
            "accountUuid": "acc-1",
            "utilization": {
                "five_hour": { "utilization": 1, "resets_at": "2026-11-27T11:30:00.066237+00:00" },
                "limits": [
                    { "kind": "weekly_all", "group": "weekly", "percent": 23, "resets_at": "2026-12-03T05:59:59.702296+00:00", "scope": null },
                    { "kind": "session", "group": "session", "percent": 30, "resets_at": "2026-11-27T11:29:59.702271+00:00", "scope": null, "is_active": true },
                    { "kind": "weekly_scoped", "group": "weekly", "percent": 4, "resets_at": null, "scope": { "model": { "display_name": "Fable" } } },
                    { "kind": "weekly_scoped", "group": "weekly", "percent": 9, "resets_at": null, "scope": { "surface": { "display_name": "Claude Design" } } },
                    { "kind": "weekly_scoped", "group": "weekly", "percent": 11, "scope": { "model": { "display_name": "Fable (preview)" } } },
                    { "kind": "weekly_scoped", "group": "weekly", "percent": 12, "scope": null },
                    { "kind": "new_meter", "group": "x", "percent": 50, "resets_at": null },
                ],
            },
        }));
        let found = parse_claude(&root).unwrap();
        assert_eq!(found.status, Status::Ok);
        assert_eq!(found.plan.as_deref(), Some("Max 5x"));
        assert_eq!(found.updated_at.as_deref(), Some("2026-11-27T08:39:20Z"));
        let summary: Vec<_> = found
            .windows
            .iter()
            .map(|w| {
                (
                    w.minutes,
                    w.scope.as_deref(),
                    w.used_percent,
                    w.resets_at.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (Some(300), None, 30.0, Some("2026-11-27T11:29:59Z")),
                (Some(10080), None, 23.0, Some("2026-12-03T05:59:59Z")),
                (Some(10080), Some("Fable"), 4.0, None),
            ]
        );
    }

    #[test]
    fn claude_rows_it_would_not_show_do_not_bring_back_the_old_shape() {
        let root = claude_root(json!({
            "accountUuid": "acc-1",
            "utilization": {
                "five_hour": { "utilization": 99, "resets_at": "2026-11-27T11:30:00+00:00" },
                "limits": [{ "kind": "new_meter", "group": "x", "percent": 50 }],
            },
        }));
        assert_eq!(parse_claude(&root), None);
    }

    #[test]
    fn claude_falls_back_to_the_usage_endpoint_shape() {
        let root = claude_root(json!({
            "fetchedAtMs": 1_795_768_760_279_i64,
            "utilization": {
                "five_hour": { "utilization": 24, "resets_at": "2026-11-27T11:30:00+00:00" },
                "seven_day": { "utilization": 22, "resets_at": "2026-12-03T06:00:00+00:00" },
                "seven_day_opus": null,
                "seven_day_overage_included": { "utilization": 5, "resets_at": "2026-12-03T06:00:00+00:00" },
                "limits": [],
            },
        }));
        let found = parse_claude(&root).unwrap();
        assert_eq!(
            found
                .windows
                .iter()
                .map(|w| w.used_percent)
                .collect::<Vec<_>>(),
            vec![24.0, 22.0, 5.0]
        );
        assert_eq!(found.windows[2].scope.as_deref(), Some("Fable"));
    }

    #[test]
    fn claude_ignores_another_accounts_cache_and_signed_out_caches() {
        let cache = json!({ "accountUuid": "acc-2", "utilization": { "limits": [
            { "kind": "session", "group": "session", "percent": 30 },
        ] } });
        assert_eq!(parse_claude(&claude_root(cache.clone())), None);
        let signed_out = json!({ "cachedUsageUtilization": cache });
        assert_eq!(parse_claude(&signed_out), None);
        assert_eq!(parse_claude(&json!({ "oauthAccount": {} })), None);
    }

    #[test]
    fn claude_shapes_this_version_does_not_know_are_unreadable() {
        let found = parse_claude(&claude_root(json!({
            "accountUuid": "acc-1",
            "utilization": { "windows_v9": [] },
        })))
        .unwrap();
        assert_eq!(found.status, Status::Unreadable);
        // Known shape without subscription limits: an API-billed account.
        let api = claude_root(json!({
            "accountUuid": "acc-1",
            "utilization": { "five_hour": null, "seven_day": null, "limits": [] },
        }));
        assert_eq!(parse_claude(&api), None);
    }

    #[test]
    fn output_never_carries_account_details_or_paths() {
        let home = tempfile::tempdir().unwrap();
        let root = claude_root(json!({
            "fetchedAtMs": 1_795_768_760_279_i64,
            "accountUuid": "acc-1",
            "utilization": { "limits": [
                { "kind": "session", "group": "session", "percent": 30, "scope": null },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 4, "scope": { "model": { "display_name": "someone@example.com" } } },
            ] },
        }));
        std::fs::write(home.path().join(".claude.json"), root.to_string()).unwrap();
        write_log(
            &home.path().join(".codex/sessions/2026/11/27"),
            "rollout.jsonl",
            &[codex_event("2026-11-27T05:00:00Z", codex_limits(3.0, 6.0))],
            at("2026-11-27T05:00:00Z"),
        );
        let quotas = read(&Sources::resolve(home.path(), false));
        assert_eq!(
            quotas
                .subscriptions
                .iter()
                .map(|s| s.client)
                .collect::<Vec<_>>(),
            vec!["claude", "codex"]
        );
        let text = serde_json::to_string(&quotas).unwrap();
        for secret in [
            "acc-1",
            "someone@example.com",
            "@",
            &home.path().to_string_lossy(),
        ] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }

    #[test]
    fn sources_follow_the_agents_own_directories() {
        let sources = Sources::resolve(Path::new("/home/u"), false);
        assert_eq!(sources.codex_home, PathBuf::from("/home/u/.codex"));
        assert_eq!(sources.claude_config, PathBuf::from("/home/u/.claude.json"));
    }

    /// `cargo test -p tokenlens-app real_machine_quotas -- --ignored --nocapture`
    /// prints this computer's readings (numbers only) and how long they took.
    #[test]
    #[ignore = "reads this computer's agent files"]
    fn real_machine_quotas() {
        let home = dirs::home_dir().unwrap();
        let started = std::time::Instant::now();
        let quotas = read(&Sources::resolve(&home, true));
        let elapsed = started.elapsed();
        println!("{}", serde_json::to_string_pretty(&quotas).unwrap());
        println!("read in {elapsed:?}");
    }

    #[test]
    fn labels_keep_only_plain_text() {
        assert_eq!(clean_label("  opus_4 "), Some("Opus 4".to_owned()));
        assert_eq!(clean_label("<script>"), Some("Script".to_owned()));
        assert_eq!(clean_label("///"), None);
        assert_eq!(codex_plan("PRO").as_deref(), Some("Pro"));
        assert_eq!(codex_plan("team_plus").as_deref(), Some("Team plus"));
        assert_eq!(claude_plan("default_claude_ai"), None);
    }
}
