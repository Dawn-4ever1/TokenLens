//! TokenLens sync server: pairing, ingest and dashboard reads.

pub mod clock;
pub mod dashboard;
pub mod db;
pub mod http;
pub mod limiter;
pub mod pricing;
pub mod secrets;

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;

pub const DEFAULT_DATABASE_PATH: &str = "./data/tokenlens.sqlite";
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8787";
pub const DEFAULT_TIME_ZONE: chrono_tz::Tz = chrono_tz::Asia::Shanghai;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub database_path: PathBuf,
    pub listen_addr: SocketAddr,
    pub time_zone: chrono_tz::Tz,
    /// The server's own tokscale directory for price catalogs (D042).
    pub pricing_dir: PathBuf,
    /// Download price catalogs once a day; `PRICING_REFRESH=off` disables it.
    pub pricing_refresh: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("LISTEN_ADDR is not a socket address")]
    BadListenAddr,
    #[error("DASHBOARD_TIMEZONE is not an IANA time zone name such as Asia/Shanghai")]
    BadTimeZone,
    #[error("PRICING_REFRESH must be on or off")]
    BadPricingRefresh,
    #[error(
        "LISTEN_ADDR is not a loopback address; tokens would travel without TLS. \
         Put the server behind a TLS proxy and set LISTEN_ALLOW_NON_LOOPBACK=1"
    )]
    NonLoopback,
}

impl Config {
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let non_empty = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
        let database_path: PathBuf = non_empty("DATABASE_PATH")
            .unwrap_or_else(|| DEFAULT_DATABASE_PATH.to_owned())
            .into();
        let pricing_dir = non_empty("PRICING_DIR").map_or_else(
            || pricing::default_dir(&database_path),
            |dir| PathBuf::from(dir.trim()),
        );
        let pricing_refresh = match non_empty("PRICING_REFRESH").as_deref().map(str::trim) {
            None | Some("on") => true,
            Some("off") => false,
            Some(_) => return Err(ConfigError::BadPricingRefresh),
        };
        let listen_addr = non_empty("LISTEN_ADDR")
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_owned())
            .trim()
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::BadListenAddr)?;
        let allow_public = non_empty("LISTEN_ALLOW_NON_LOOPBACK").is_some_and(|value| value == "1");
        if !listen_addr.ip().is_loopback() && !allow_public {
            return Err(ConfigError::NonLoopback);
        }
        let time_zone = match non_empty("DASHBOARD_TIMEZONE") {
            None => DEFAULT_TIME_ZONE,
            Some(name) => name
                .trim()
                .parse::<chrono_tz::Tz>()
                .map_err(|_| ConfigError::BadTimeZone)?,
        };
        Ok(Self {
            database_path,
            listen_addr,
            time_zone,
            pricing_dir,
            pricing_refresh,
        })
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

/// Serves until `shutdown` resolves, then lets in-flight requests finish.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: http::AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

/// Asks a server listening on `addr` for `/healthz`; an unspecified address
/// such as `0.0.0.0` is reached through loopback. For container health checks.
pub fn check_health(addr: SocketAddr, timeout: std::time::Duration) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    let mut target = addr;
    if target.ip().is_unspecified() {
        target.set_ip(match target.ip() {
            IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    let mut stream = std::net::TcpStream::connect_timeout(&target, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut status_line = [0u8; 12];
    stream.read_exact(&mut status_line)?;
    if status_line.starts_with(b"HTTP/1.1 200") {
        Ok(())
    } else {
        Err(std::io::Error::other("server did not answer 200"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_and_loopback_guard() {
        let defaults = config(&[]).expect("defaults");
        assert_eq!(defaults.database_path, PathBuf::from(DEFAULT_DATABASE_PATH));
        assert_eq!(defaults.listen_addr.to_string(), DEFAULT_LISTEN_ADDR);
        assert_eq!(defaults.time_zone, chrono_tz::Asia::Shanghai);
        assert_eq!(defaults.pricing_dir, PathBuf::from("./data/tokscale"));
        assert!(defaults.pricing_refresh);
        let offline = config(&[("PRICING_DIR", "/srv/prices"), ("PRICING_REFRESH", "off")])
            .expect("offline pricing");
        assert_eq!(offline.pricing_dir, PathBuf::from("/srv/prices"));
        assert!(!offline.pricing_refresh);
        assert_eq!(
            config(&[("PRICING_REFRESH", "sometimes")]),
            Err(ConfigError::BadPricingRefresh)
        );
        assert_eq!(
            config(&[("DASHBOARD_TIMEZONE", "Europe/Berlin")])
                .expect("zone")
                .time_zone,
            chrono_tz::Europe::Berlin
        );
        assert_eq!(
            config(&[("DASHBOARD_TIMEZONE", "Mars/Olympus")]),
            Err(ConfigError::BadTimeZone)
        );
        assert!(config(&[("LISTEN_ADDR", "[::1]:9000")]).is_ok());
        assert_eq!(
            config(&[("LISTEN_ADDR", "0.0.0.0:8787")]),
            Err(ConfigError::NonLoopback)
        );
        assert!(
            config(&[
                ("LISTEN_ADDR", "0.0.0.0:8787"),
                ("LISTEN_ALLOW_NON_LOOPBACK", "1")
            ])
            .is_ok()
        );
        assert_eq!(
            config(&[("LISTEN_ADDR", "localhost")]),
            Err(ConfigError::BadListenAddr)
        );
    }
}
