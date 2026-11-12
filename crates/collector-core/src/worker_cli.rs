//! The `scan-worker` child entry point, shared by every binary that the watch
//! loop may start as its worker program (the CLI and the desktop app).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;

use crate::worker::{WorkerLine, lower_own_priority};
use crate::{Config, DateRange, ScanOptions};

pub const SUBCOMMAND: &str = "scan-worker";

/// `true` when this process was started as a scan worker.
pub fn requested(args: &[OsString]) -> bool {
    args.get(1).map(OsString::as_os_str) == Some(OsStr::new(SUBCOMMAND))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    home: Option<PathBuf>,
    no_env_roots: bool,
    clients: Option<Vec<String>>,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    upstream_cache: bool,
}

fn value(args: &mut impl Iterator<Item = OsString>, name: &str) -> Result<OsString, String> {
    args.next().ok_or_else(|| format!("{name} needs a value"))
}

fn text(value: OsString) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| "worker arguments must be UTF-8".to_owned())
}

fn date(value: OsString) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(&text(value)?, "%Y-%m-%d")
        .map_err(|_| "expected a valid date in YYYY-MM-DD format".to_owned())
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--home") => parsed.home = Some(PathBuf::from(value(&mut args, "--home")?)),
            Some("--no-env-roots") => parsed.no_env_roots = true,
            Some("--upstream-cache") => parsed.upstream_cache = true,
            Some("--clients") => {
                let list = text(value(&mut args, "--clients")?)?;
                parsed.clients = Some(list.split(',').map(str::to_owned).collect());
            }
            Some("--since") => parsed.since = Some(date(value(&mut args, "--since")?)?),
            Some("--until") => parsed.until = Some(date(value(&mut args, "--until")?)?),
            _ => return Err("unknown scan-worker argument".to_owned()),
        }
    }
    Ok(parsed)
}

/// The persistent upstream cache is only allowed in this project's own
/// directory, set by the parent on this process alone.
fn assert_own_cache_dir() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::var_os("TOKSCALE_CONFIG_DIR").filter(|dir| !dir.is_empty());
    match dir {
        Some(dir) if Path::new(&dir).ends_with(Path::new("tokenlens").join("tokscale")) => Ok(()),
        _ => Err("the upstream cache requires this project's own cache directory".into()),
    }
}

/// Runs one scan and prints the worker protocol to stdout. `args` are the
/// arguments after the subcommand.
pub fn serve(args: impl IntoIterator<Item = OsString>) -> Result<(), Box<dyn std::error::Error>> {
    lower_own_priority();
    let start = Instant::now();
    let args = parse(args)?;
    if args.upstream_cache {
        assert_own_cache_dir()?;
    }
    let range = DateRange {
        since: args.since,
        until: args.until,
    };
    let mut options = match args.home {
        Some(home) => ScanOptions {
            home,
            use_env_roots: false,
            clients: None,
            range,
            upstream_cache: false,
        },
        None => ScanOptions::for_current_user(range)?,
    };
    if args.no_env_roots {
        options.use_env_roots = false;
    }
    options.upstream_cache = args.upstream_cache;
    options.clients = args.clients.map(|clients| {
        clients
            .iter()
            .map(|id| crate::normalize_client_id(id))
            .collect::<BTreeSet<_>>()
    });
    let config = Config::load(&crate::config_path_in(&options.home))?;
    let report = crate::scan_with(&config, &options)?;
    let mut counts = report
        .selected_clients
        .iter()
        .map(|client| (client.clone(), 0_u64))
        .collect::<BTreeMap<_, _>>();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for event in &report.events {
        *counts.entry(event.client.clone()).or_default() += 1;
        let value = event
            .to_json_value()
            .map_err(|_| "an event failed validation")?;
        let line = serde_json::json!({"type": "event", "event": value});
        serde_json::to_writer(&mut out, &line)?;
        out.write_all(b"\n")?;
    }
    let done = WorkerLine::Done {
        counts,
        cached: report.upstream_cache_used,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    serde_json::to_writer(&mut out, &done)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_the_arguments_the_watch_loop_passes() {
        let parsed = parse(os(&[
            "--home",
            "/tmp/h",
            "--no-env-roots",
            "--clients",
            "codex,claude",
            "--since",
            "2026-11-01",
            "--until",
            "2026-11-10",
            "--upstream-cache",
        ]))
        .unwrap();
        assert_eq!(parsed.home, Some(PathBuf::from("/tmp/h")));
        assert!(parsed.no_env_roots && parsed.upstream_cache);
        assert_eq!(parsed.clients.unwrap(), vec!["codex", "claude"]);
        assert_eq!(parsed.since.unwrap().to_string(), "2026-11-01");
        assert_eq!(parsed.until.unwrap().to_string(), "2026-11-10");
        assert_eq!(parse(Vec::new()).unwrap(), Args::default());
    }

    #[test]
    fn rejects_unknown_or_malformed_arguments() {
        assert!(parse(os(&["--bogus"])).is_err());
        assert!(parse(os(&["--since"])).is_err());
        assert!(parse(os(&["--since", "2026-02-30"])).is_err());
        assert!(requested(&os(&["app", "scan-worker"])));
        assert!(!requested(&os(&["app", "--background"])));
    }
}
