use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokenlens_server::clock::{Clock, SystemClock};
use tokenlens_server::http::AppState;
use tokenlens_server::{Config, db, pricing, secrets};

#[derive(Parser)]
#[command(version, about = "TokenLens sync server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the HTTP API (DATABASE_PATH, LISTEN_ADDR, DASHBOARD_TIMEZONE,
    /// PRICING_DIR, PRICING_REFRESH).
    Serve,
    /// One-time pairing codes for adding devices.
    PairCode {
        #[command(subcommand)]
        action: PairCodeAction,
    },
    /// Paired devices.
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },
    /// Separate data spaces: devices in one tenant share its dashboard and
    /// never see another tenant's data.
    Tenant {
        #[command(subcommand)]
        action: TenantAction,
    },
    /// Copy the database (DATABASE_PATH) to a new file while the server runs.
    Backup {
        /// Destination file; must not exist yet.
        dest: PathBuf,
    },
    /// Check that a backup file opens and is intact, without changing it.
    VerifyBackup { file: PathBuf },
    /// Replace the database with a backup while the server is stopped; the
    /// current file is moved aside and every computer re-sends its history.
    Restore { file: PathBuf },
    /// Exit 0 if the server at LISTEN_ADDR answers; for container health checks.
    Healthcheck,
    /// Price catalogs for the cost estimates (PRICING_DIR).
    Pricing {
        #[command(subcommand)]
        action: PricingAction,
    },
}

#[derive(Subcommand)]
enum PricingAction {
    /// Download the three public price catalogs now; a running server picks
    /// them up within a day, or at its next start.
    Refresh,
    /// Price every stored event with the downloaded catalogs, without
    /// writing anything, and list which models have no price.
    Report,
}

#[derive(Subcommand)]
enum PairCodeAction {
    /// Print a new code once; it is valid for 10 minutes and a single device.
    Create {
        /// Tenant the new device joins, by id or short name.
        #[arg(long, default_value = db::DEFAULT_TENANT_ID)]
        tenant: String,
    },
    /// Invalidate every unused code.
    RevokeAll,
}

#[derive(Subcommand)]
enum DeviceAction {
    /// List devices that are not revoked, with their tenant.
    List {
        /// Only this tenant's devices, by id or short name.
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Revoke a device; both of its tokens stop working at once.
    Revoke { device_id: String },
}

#[derive(Subcommand)]
enum TenantAction {
    /// Create a tenant. The short name uses a-z, 0-9 and hyphens; the id
    /// becomes `tenant_<short name>`.
    Create { slug: String, display_name: String },
    /// List live tenants and how many devices each has.
    List,
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn open_db(config: &Config) -> Result<rusqlite::Connection, Box<dyn std::error::Error>> {
    let opened = db::open(&config.database_path)?;
    if opened.backup.is_some() {
        tracing::info!("database backed up before migration");
    }
    Ok(opened.conn)
}

fn format_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "-".to_owned())
}

async fn shutdown_signal() {
    // `docker stop` sends SIGTERM, which PID 1 otherwise ignores until killed.
    #[cfg(unix)]
    if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        tracing::info!("shutting down");
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let now = SystemClock.now_ms();
    match cli.command {
        Command::Serve => {
            let _lock = db::lock_database(&config.database_path)?;
            let conn = open_db(&config)?;
            let reader = db::open_reader(&config.database_path)?;
            std::fs::create_dir_all(&config.pricing_dir)?;
            // SAFETY: no other thread exists yet; the runtime starts below.
            unsafe { pricing::configure_dir(&config.pricing_dir) };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
                tracing::info!(addr = %listener.local_addr()?, "listening");
                let state = AppState::new(conn, Arc::new(SystemClock))
                    .with_reader(reader)
                    .with_time_zone(config.time_zone);
                tokio::spawn(pricing::maintain(
                    state.clone(),
                    config.pricing_dir.clone(),
                    config.pricing_refresh,
                ));
                tokenlens_server::serve(listener, state, shutdown_signal()).await
            })?;
        }
        Command::PairCode {
            action: PairCodeAction::Create { tenant },
        } => {
            let conn = open_db(&config)?;
            let tenant = db::resolve_tenant(&conn, &tenant)?;
            let issued = db::create_pair_code_for_tenant(&conn, now, &tenant, None)?;
            // The operator's terminal is the only place this code ever appears.
            println!(
                "Pairing code: {}",
                secrets::format_pair_code(&issued.code).expose()
            );
            println!(
                "Valid until {} for one device, which joins {tenant}.",
                format_ms(issued.expires_at)
            );
        }
        Command::PairCode {
            action: PairCodeAction::RevokeAll,
        } => {
            let conn = open_db(&config)?;
            let revoked = db::revoke_open_pair_codes(&conn, now)?;
            println!("Revoked {revoked} unused pairing code(s).");
        }
        Command::Device {
            action: DeviceAction::List { tenant },
        } => {
            let conn = open_db(&config)?;
            let tenant = tenant
                .map(|name| db::resolve_tenant(&conn, &name))
                .transpose()?;
            let devices = db::list_devices(&conn, tenant.as_deref())?;
            if devices.is_empty() {
                println!("No paired devices.");
            }
            for device in devices {
                println!(
                    "{}  {}  {}  {}  last seen {}",
                    device.id,
                    device.tenant_id,
                    device.platform.as_deref().unwrap_or("-"),
                    device.form_factor.as_deref().unwrap_or("-"),
                    device
                        .last_seen_at
                        .map_or_else(|| "never".to_owned(), format_ms)
                );
            }
        }
        Command::Tenant {
            action: TenantAction::Create { slug, display_name },
        } => {
            let conn = open_db(&config)?;
            let id = db::create_tenant(&conn, slug.trim(), &display_name, now)?;
            println!("Created tenant {id} ({}).", display_name.trim());
            println!("Add its first device with: pair-code create --tenant {id}");
        }
        Command::Tenant {
            action: TenantAction::List,
        } => {
            let conn = open_db(&config)?;
            for tenant in db::list_tenants(&conn)? {
                println!(
                    "{}  {}  {}  {} device(s)",
                    tenant.id, tenant.slug, tenant.display_name, tenant.active_devices
                );
            }
        }
        Command::Device {
            action: DeviceAction::Revoke { device_id },
        } => {
            let mut conn = open_db(&config)?;
            let Some(tenant) = db::device_tenant(&conn, &device_id)? else {
                return Err("no such active device".into());
            };
            if db::revoke_device(&mut conn, &tenant, &device_id, now)? {
                println!("Revoked {device_id} in {tenant}; its tokens no longer work.");
            } else {
                return Err("no such active device".into());
            }
        }
        Command::Backup { dest } => {
            let summary = db::backup_to(&config.database_path, &dest)?;
            println!("Backed up to {}: {}", dest.display(), describe(&summary));
        }
        Command::VerifyBackup { file } => {
            let summary = db::verify_backup(&file)?;
            println!("{} is intact: {}", file.display(), describe(&summary));
        }
        Command::Restore { file } => {
            let _lock = db::lock_database(&config.database_path)?;
            let restored = db::restore_from(&file, &config.database_path)?;
            println!(
                "Restored {}: {}",
                file.display(),
                describe(&restored.summary)
            );
            if let Some(previous) = restored.previous {
                println!("The replaced database was kept as {}.", previous.display());
            }
            println!(
                "Computers re-send what the backup is missing after their next upload or heartbeat;"
            );
            println!("a computer paired after this backup must be paired again.");
        }
        Command::Pricing { action } => {
            std::fs::create_dir_all(&config.pricing_dir)?;
            // SAFETY: still single-threaded; the runtime below starts after.
            unsafe { pricing::configure_dir(&config.pricing_dir) };
            match action {
                PricingAction::Refresh => {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()?;
                    let outcome = runtime.block_on(pricing::refresh());
                    let version = pricing::catalog_version(&config.pricing_dir)?;
                    match outcome {
                        Ok(()) => println!("downloaded; price version {version}"),
                        Err(error) => {
                            println!("price version {version}; some catalogs failed: {error}");
                        }
                    }
                }
                PricingAction::Report => {
                    let Some(pricer) = pricing::load(&config.pricing_dir, None)? else {
                        return Err("no price catalog yet; run `pricing refresh` first".into());
                    };
                    let conn = open_db(&config)?;
                    let report = db::price_report(&conn, &pricer)?;
                    println!(
                        "price version {}, catalogs from {}",
                        pricer.version(),
                        format_ms(pricer.as_of_ms())
                    );
                    println!(
                        "{:<9} {:>8} {:>15} {:>12}  client / model / provider",
                        "status", "events", "tokens", "usd"
                    );
                    for row in report {
                        println!(
                            "{:<9} {:>8} {:>15} {:>12.2}  {} / {} / {}",
                            row.status,
                            row.events,
                            row.tokens,
                            row.cost_usd,
                            row.client,
                            row.model_id,
                            row.provider_id.as_deref().unwrap_or("-")
                        );
                        if let Some(explanation) = row.explanation {
                            println!("{:>48}{explanation}", "");
                        }
                    }
                }
            }
        }
        Command::Healthcheck => {
            tokenlens_server::check_health(config.listen_addr, Duration::from_secs(3))?;
        }
    }
    Ok(())
}

fn describe(summary: &db::BackupSummary) -> String {
    format!(
        "schema v{}, {} active device(s), {} event(s).",
        summary.schema_version, summary.active_devices, summary.events
    )
}

fn main() -> ExitCode {
    init_logging();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tokenlens-server: {error}");
            ExitCode::FAILURE
        }
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
}
