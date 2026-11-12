use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};
use collector_core::credentials::{
    self, CredentialStore, Credentials, DEFAULT_KEYCHAIN_SERVICE, KeychainStore,
};
use collector_core::schedule::Timing;
use collector_core::sync::{self, SyncTiming, Syncer};
use collector_core::transport::UreqTransport;
use collector_core::watch::{self, Msg, WatchOptions};
use collector_core::{Config, DateRange, ScanOptions, ScanReport, Tokens};
use usage_event::envelope::FormFactor;

#[derive(Debug, Parser)]
#[command(name = "collector", version, about = "Read-only local usage summaries")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Scan {
        #[arg(long, help = "Only summarize; do not upload or write local state")]
        dry_run: bool,
        #[arg(long, value_parser = parse_date, help = "Inclusive local date (YYYY-MM-DD)")]
        since: Option<NaiveDate>,
        #[arg(long, value_parser = parse_date, help = "Inclusive local date (YYYY-MM-DD)")]
        until: Option<NaiveDate>,
        #[arg(long, help = "Print machine-readable numeric summaries")]
        json: bool,
        #[arg(
            long,
            help = "Print only event counts and field names/types, never event values"
        )]
        events: bool,
        /// Development only: read agent logs below this directory instead of
        /// the current user's home, ignoring agent root variables.
        #[arg(long, hide = true)]
        home: Option<PathBuf>,
        /// Internal: scan in this process (the parent sets up upstream's
        /// environment and hides the home folder in its diagnostics).
        #[arg(long, hide = true)]
        direct: bool,
    },
    /// Watch agent log directories and keep an in-memory snapshot up to date.
    /// Prints numbers only; nothing is uploaded or written. Ctrl-C to stop.
    Watch(WatchArgs),
    /// Pair this computer with your dashboard server. The one-time code is
    /// read from the terminal (or stdin), never from the command line.
    Pair(PairArgs),
    /// Watch agent logs and upload new usage numbers to the paired server.
    /// Ctrl-C to stop.
    Run(RunArgs),
    /// Show pairing and upload status (numbers only, no tokens).
    Status(StoreArgs),
    /// Forget this computer's pairing and its local upload queue.
    Unpair(StoreArgs),
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum FormFactorArg {
    Laptop,
    Desktop,
}

impl From<FormFactorArg> for FormFactor {
    fn from(value: FormFactorArg) -> Self {
        match value {
            FormFactorArg::Laptop => Self::Laptop,
            FormFactorArg::Desktop => Self::Desktop,
        }
    }
}

#[derive(Debug, Args)]
struct StoreArgs {
    /// Development only: a separate system keychain entry.
    #[arg(long, hide = true, default_value = DEFAULT_KEYCHAIN_SERVICE)]
    keychain_service: String,
    /// Development only: a separate local upload queue.
    #[arg(long, hide = true)]
    state_dir: Option<PathBuf>,
}

impl StoreArgs {
    fn store(&self) -> KeychainStore {
        KeychainStore::new(&self.keychain_service)
    }

    fn state_dir(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        match &self.state_dir {
            Some(dir) => Ok(dir.clone()),
            None => {
                Ok(sync::default_dir().ok_or("cannot determine this project's data directory")?)
            }
        }
    }
}

#[derive(Debug, Args)]
struct PairArgs {
    /// Dashboard server address, e.g. https://dash.example.com
    #[arg(long)]
    server: String,
    #[arg(long, value_enum)]
    form_factor: FormFactorArg,
    /// Name shown on the dashboard (default: generic, e.g. "Mac laptop").
    #[arg(long)]
    name: Option<String>,
    /// Replace an existing pairing on this computer.
    #[arg(long)]
    replace: bool,
    #[command(flatten)]
    store: StoreArgs,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    watch: WatchArgs,
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long, hide = true, default_value_t = 30)]
    upload_gap_secs: u64,
    #[arg(long, hide = true, default_value_t = 300)]
    heartbeat_secs: u64,
    #[arg(long, hide = true, default_value_t = 30)]
    backoff_base_secs: u64,
}

#[derive(Debug, Args)]
struct WatchArgs {
    #[arg(long, hide = true)]
    home: Option<PathBuf>,
    #[arg(long, hide = true, default_value_t = 1500)]
    debounce_ms: u64,
    #[arg(long, hide = true, default_value_t = 30_000)]
    max_delay_ms: u64,
    #[arg(long, hide = true, default_value_t = 30_000)]
    cooldown_ms: u64,
    #[arg(long, hide = true, default_value_t = 3600)]
    full_interval_secs: u64,
    #[arg(long, hide = true, default_value_t = 300)]
    poll_interval_secs: u64,
    #[arg(long, hide = true)]
    exit_after_secs: Option<u64>,
    /// Parse every scan without upstream's persistent source cache.
    #[arg(long, hide = true)]
    no_upstream_cache: bool,
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "expected a valid date in YYYY-MM-DD format".to_owned())
}

fn print_table(report: &ScanReport) {
    println!(
        "{:<20} {:<12} {:>12} {:>12} {:>10} {:>10} {:>10} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14}",
        "client",
        "date",
        "messages",
        "credits",
        "estimated",
        "measured",
        "unknown",
        "input",
        "output",
        "cache_read",
        "cache_write",
        "cache_write_1h",
        "reasoning",
        "total"
    );
    for row in &report.daily {
        print_row(
            &row.client,
            &row.date,
            row.messages,
            row.tokens,
            row.credits.unwrap_or(0.0),
            &row.precision_counts,
        );
    }
    println!("\nClient totals");
    for row in &report.client_totals {
        print_row(
            &row.client,
            "ALL",
            row.messages,
            row.tokens,
            row.credits.unwrap_or(0.0),
            &row.precision_counts,
        );
    }
    println!("\nSelected clients: {}", report.selected_clients.join(","));
    println!("Scanned clients: {}", report.scanned_clients);
    println!("Detected clients: {}", report.detected_clients.join(","));
    println!("Elapsed: {} ms", report.elapsed_ms);
}

fn print_row(
    client: &str,
    date: &str,
    messages: i64,
    tokens: Tokens,
    credits: f64,
    precision_counts: &std::collections::BTreeMap<String, u64>,
) {
    let estimated = precision_counts.get("estimated").copied().unwrap_or(0);
    let measured = precision_counts.get("measured").copied().unwrap_or(0);
    let unknown = precision_counts.get("unknown").copied().unwrap_or(0);
    println!(
        "{client:<20} {date:<12} {messages:>12} {credits:>12.4} {estimated:>10} {measured:>10} {unknown:>10} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14}",
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.cache_write_1h,
        tokens.reasoning,
        tokens.total
    );
}

fn scan_options(
    home: Option<PathBuf>,
    range: DateRange,
) -> Result<(ScanOptions, PathBuf), Box<dyn std::error::Error>> {
    let options = match home {
        Some(home) => ScanOptions {
            home,
            use_env_roots: false,
            clients: None,
            range,
            upstream_cache: false,
        },
        None => ScanOptions::for_current_user(range)?,
    };
    let config_path = collector_core::config_path_in(&options.home);
    Ok((options, config_path))
}

fn run_scan(
    dry_run: bool,
    range: DateRange,
    json: bool,
    events: bool,
    home: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dry_run {
        return Err("only `scan --dry-run` is implemented; no data was uploaded".into());
    }
    let (options, config_path) = scan_options(home, range)?;
    let config = Config::load(&config_path)?;
    let report = collector_core::scan_with(&config, &options)?;
    if events {
        let summary = collector_core::event_summary(&report)?;
        if json {
            println!("{}", serde_json::to_string(&summary)?);
        } else {
            println!("Events: {}", summary.total);
            println!("Scanned clients: {}", report.scanned_clients);
            println!("Detected clients: {}", report.detected_clients.join(","));
            for row in summary.by_client {
                println!(
                    "{}: {} (measured {}, estimated {}, unknown {})",
                    row.client,
                    row.count,
                    row.precision_counts.get("measured").copied().unwrap_or(0),
                    row.precision_counts.get("estimated").copied().unwrap_or(0),
                    row.precision_counts.get("unknown").copied().unwrap_or(0)
                );
            }
            println!("Example (field types only; no event values):");
            println!("{}", serde_json::to_string_pretty(&summary.example)?);
        }
    } else if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_table(&report);
    }
    Ok(())
}

fn watch_options(args: &WatchArgs) -> Result<WatchOptions, Box<dyn std::error::Error>> {
    let explicit_home = args.home.is_some();
    let home = match &args.home {
        Some(home) => home.clone(),
        None => dirs::home_dir().ok_or("cannot determine the current user's home directory")?,
    };
    let mut ignored = collector_core::roots::own_directories();
    ignored.push(home.join(".config").join("tokenlens"));
    // Workers fall back to the uncached parse for Claude scans; see
    // `collector_core::upstream_cache_is_exact`.
    let upstream_cache_dir = if args.no_upstream_cache {
        None
    } else {
        Some(
            collector_core::roots::own_upstream_dir(args.home.as_deref())
                .ok_or("cannot determine this project's cache directory")?,
        )
    };
    Ok(WatchOptions {
        home,
        explicit_home,
        use_env_roots: !explicit_home,
        worker_program: std::env::current_exe()?,
        timing: Timing {
            debounce: Duration::from_millis(args.debounce_ms),
            max_delay: Duration::from_millis(args.max_delay_ms),
            cooldown: Duration::from_millis(args.cooldown_ms),
            full_interval: Duration::from_secs(args.full_interval_secs),
            poll_interval: Duration::from_secs(args.poll_interval_secs),
            ..Timing::default()
        },
        worker_timeout: Duration::from_secs(300),
        exit_after: args.exit_after_secs.map(Duration::from_secs),
        ignored,
        upstream_cache_dir,
    })
}

fn stop_on_ctrl_c(sender: &std::sync::mpsc::Sender<Msg>) -> Result<(), ctrlc::Error> {
    let stop = sender.clone();
    ctrlc::set_handler(move || {
        let _ = stop.send(Msg::Stop);
    })
}

fn run_watch(args: WatchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let options = watch_options(&args)?;
    let (sender, receiver) = watch::channel();
    stop_on_ctrl_c(&sender)?;
    let mut stdout = std::io::stdout();
    let summary = watch::run(&options, sender, receiver, &mut stdout, None);
    println!("{}", serde_json::json!({ "summary": summary }));
    Ok(())
}

fn not_paired() -> Box<dyn std::error::Error> {
    "this computer is not paired yet; run `collector pair` first".into()
}

fn run_sync(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    let credentials = args.store.store().load()?.ok_or_else(not_paired)?;
    let state_dir = args.store.state_dir()?;
    let mut options = watch_options(&args.watch)?;
    options.ignored.push(state_dir.clone());
    let (sender, receiver) = watch::channel();
    stop_on_ctrl_c(&sender)?;
    let sink_sender = sender.clone();
    let timing = SyncTiming {
        min_round_gap: Duration::from_secs(args.upload_gap_secs),
        heartbeat_interval: Duration::from_secs(args.heartbeat_secs),
        backoff_base: Duration::from_secs(args.backoff_base_secs),
        ..SyncTiming::default()
    };
    let mut syncer = Syncer::start(
        &state_dir,
        credentials,
        Box::new(UreqTransport::new()),
        timing,
        Box::new(move |line| {
            let _ = sink_sender.send(Msg::Sync(line));
        }),
    )?;
    let mut stdout = std::io::stdout();
    let summary = watch::run(&options, sender, receiver, &mut stdout, Some(&mut syncer));
    syncer.stop();
    println!("{}", serde_json::json!({ "summary": summary }));
    Ok(())
}

fn read_pair_code() -> Result<String, Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    let code = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Pairing code: ")?
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    };
    let code = code.trim().to_owned();
    if code.is_empty() {
        return Err("no pairing code was entered".into());
    }
    Ok(code)
}

fn run_pair(args: PairArgs) -> Result<(), Box<dyn std::error::Error>> {
    let store = args.store.store();
    if !args.replace && store.load()?.is_some() {
        return Err("this computer is already paired; use --replace or `collector unpair`".into());
    }
    let server = collector_core::transport::validate_server_url(&args.server)?;
    let form_factor = FormFactor::from(args.form_factor);
    let name = match args.name {
        Some(name) => name,
        None => credentials::default_display_name(credentials::this_platform(), form_factor),
    };
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("the device name must be 1-64 characters of plain text".into());
    }
    let code = read_pair_code()?;
    let paired = credentials::claim(&UreqTransport::new(), &server, &code, form_factor, name)?;
    store.save(&paired)?;
    println!(
        "{}",
        serde_json::json!({ "paired": true, "deviceId": paired.device_id, "server": paired.server_url })
    );
    Ok(())
}

fn run_status(args: StoreArgs) -> Result<(), Box<dyn std::error::Error>> {
    let paired: Option<Credentials> = args.store().load()?;
    let local = sync::local_status(&args.state_dir()?)?;
    let status = serde_json::json!({
        "paired": paired.is_some(),
        "deviceId": paired.as_ref().map(|paired| paired.device_id.clone()),
        "server": paired.as_ref().map(|paired| paired.server_url.clone()),
        "collectorVersion": sync::COLLECTOR_VERSION,
        "parserGeneration": collector_core::PARSER_GENERATION,
        "local": local,
    });
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

fn run_unpair(args: StoreArgs) -> Result<(), Box<dyn std::error::Error>> {
    let state_dir = args.state_dir()?;
    let _lock = match collector_core::state::lock_queue(&state_dir) {
        Err(collector_core::state::StateError::Busy) => {
            return Err(
                "the TokenLens app or `collector run` is still uploading; quit it before unpairing"
                    .into(),
            );
        }
        lock => lock?,
    };
    let removed = args.store().delete()?;
    let state = collector_core::state::State::path_in(&state_dir);
    let mut queue_removed = false;
    for suffix in ["", "-wal", "-shm"] {
        let mut path = state.clone().into_os_string();
        path.push(suffix);
        match std::fs::remove_file(&path) {
            Ok(()) => queue_removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the local upload queue could not be removed".into()),
        }
    }
    println!(
        "{}",
        serde_json::json!({ "pairingRemoved": removed, "localQueueRemoved": queue_removed })
    );
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if collector_core::worker_cli::requested(&args) {
        return collector_core::worker_cli::serve(args.into_iter().skip(2));
    }
    match Cli::parse().command {
        Command::Scan {
            dry_run,
            since,
            until,
            json,
            events,
            home,
            direct,
        } => {
            if !direct {
                let code =
                    collector_core::interactive::rerun_isolated("--direct", home.as_deref())?;
                std::process::exit(code);
            }
            let range = if since.is_none() && until.is_none() {
                DateRange::last_30_days()
            } else {
                DateRange { since, until }
            };
            run_scan(dry_run, range, json, events, home)
        }
        Command::Watch(args) => run_watch(args),
        Command::Pair(args) => run_pair(args),
        Command::Run(args) => run_sync(args),
        Command::Status(args) => run_status(args),
        Command::Unpair(args) => run_unpair(args),
    }
}

fn main() {
    // Upstream's recovery overlay merges a ledger from its shared config
    // directory; this project only counts raw agent logs.
    // SAFETY: set before any other thread exists; children inherit it.
    unsafe { std::env::set_var("TOKSCALE_RECOVERY_DISABLE", "1") };
    if let Err(error) = run() {
        // Configuration and upstream errors are intentionally not accompanied
        // by source paths, request bodies, session titles, or environment data.
        eprintln!("collector: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn date_parser_is_strict() {
        assert!(parse_date("2026-11-10").is_ok());
        assert!(parse_date("2026-11-10T00:00:00").is_err());
        assert!(parse_date("2026-02-30").is_err());
    }
}
