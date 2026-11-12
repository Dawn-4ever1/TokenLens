//! Local mode (D044): the sync server's own code, run inside the app so a
//! single computer needs no server. The collector "uploads" to it and the
//! dashboard reads from it through the same `/api/*` routes, called in
//! process (no port, no network). Only the public price catalogs are fetched
//! from the internet, once a day.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, header};
use collector_core::credentials::{self, Credentials, PairError};
use collector_core::transport::{
    HttpResponse, MAX_GET_RESPONSE_BYTES, MAX_RESPONSE_BYTES, Transport, TransportError,
};
use tokenlens_server::clock::SystemClock;
use tokenlens_server::db::{self, DEFAULT_TENANT_ID};
use tokenlens_server::{http, pricing};
use tower::ServiceExt;
use usage_event::envelope::FormFactor;

/// `server_url` of local-mode credentials. Never a real address: requests
/// with this prefix go to [`LocalServer`] and nowhere else.
pub const LOCAL_SERVER_URL: &str = "local://this-computer";
const DATABASE_FILE: &str = "local.sqlite";
const CREDENTIALS_FILE: &str = "local-device.json";

pub fn is_local(credentials: &Credentials) -> bool {
    credentials.server_url == LOCAL_SERVER_URL
}

/// Price catalogs for local mode, in the per-user cache folder. Also the
/// process's `TOKSCALE_CONFIG_DIR` (set in `main` before any thread starts).
pub fn prices_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|cache| cache.join("tokenlens").join("prices"))
}

pub fn database_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DATABASE_FILE)
}

fn credentials_path(state_dir: &Path) -> PathBuf {
    state_dir.join(CREDENTIALS_FILE)
}

/// Local credentials only open the local database, which this user can read
/// anyway; they are not network credentials, so they stay out of the
/// keychain (D044, item 4).
pub fn load_credentials(state_dir: &Path) -> Option<Credentials> {
    let text = std::fs::read_to_string(credentials_path(state_dir)).ok()?;
    serde_json::from_str::<Credentials>(&text)
        .ok()
        .filter(is_local)
}

pub fn save_credentials(state_dir: &Path, credentials: &Credentials) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let path = credentials_path(state_dir);
    let temporary = path.with_extension("json.tmp");
    let text = serde_json::to_vec(credentials).map_err(std::io::Error::other)?;
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, &path)
}

pub fn delete_credentials(state_dir: &Path) {
    let _ = std::fs::remove_file(credentials_path(state_dir));
}

/// The computer's IANA zone for day buckets (D044, item 6); UTC if unknown.
fn local_time_zone() -> chrono_tz::Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse().ok())
        .unwrap_or(chrono_tz::UTC)
}

#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("the local database could not be opened")]
    Database,
    #[error("this computer could not be set up")]
    Pair,
}

pub struct LocalServer {
    state: http::AppState,
    router: Router,
    /// Its own small runtime: callers block on requests from the app's
    /// blocking pool, and the routes need blocking threads of their own.
    runtime: Option<tokio::runtime::Runtime>,
    _lock: std::fs::File,
}

impl LocalServer {
    /// Opens (and migrates) the local database and starts the daily price
    /// catalog refresh. Call from a blocking thread.
    pub fn open(state_dir: &Path) -> Result<Arc<Self>, LocalError> {
        Self::open_with(state_dir, prices_dir())
    }

    /// `prices: None` never prices events and never goes online (tests).
    fn open_with(state_dir: &Path, prices: Option<PathBuf>) -> Result<Arc<Self>, LocalError> {
        let path = database_path(state_dir);
        let lock = db::lock_database(&path).map_err(|_| LocalError::Database)?;
        let opened = db::open(&path).map_err(|_| LocalError::Database)?;
        let state = http::AppState::new(opened.conn, Arc::new(SystemClock))
            .with_time_zone(local_time_zone());
        let router = http::router(state.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(4)
            .thread_name("tokenlens-local")
            .thread_keep_alive(Duration::from_secs(10))
            .enable_all()
            .build()
            .map_err(|_| LocalError::Database)?;
        if let Some(dir) = prices {
            runtime.spawn(pricing::maintain(state.clone(), dir, true));
        }
        Ok(Arc::new(Self {
            state,
            router,
            runtime: Some(runtime),
            _lock: lock,
        }))
    }

    /// Runs one request through the server's router, in this process.
    pub fn request(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        gzip: bool,
        body: &[u8],
    ) -> Result<HttpResponse, TransportError> {
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if method == Method::POST {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        if gzip {
            builder = builder.header(header::CONTENT_ENCODING, "gzip");
        }
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder
            .body(Body::from(body.to_vec()))
            .map_err(|_| TransportError::Network)?;
        let limit = if method == Method::GET {
            MAX_GET_RESPONSE_BYTES
        } else {
            MAX_RESPONSE_BYTES
        };
        let router = self.router.clone();
        let runtime = self.runtime.as_ref().ok_or(TransportError::Network)?;
        runtime.block_on(async move {
            let response = router
                .oneshot(request)
                .await
                .map_err(|_| TransportError::Network)?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let body = axum::body::to_bytes(response.into_body(), limit)
                .await
                .map_err(|_| TransportError::Body)?;
            Ok(HttpResponse {
                status,
                retry_after,
                body: body.to_vec(),
            })
        })
    }

    /// Creates this computer's device in the personal tenant and returns its
    /// local credentials, through the same claim route a server uses.
    pub fn pair(
        self: &Arc<Self>,
        form_factor: FormFactor,
        display_name: &str,
    ) -> Result<Credentials, LocalError> {
        let code = self
            .state
            .issue_pair_code(DEFAULT_TENANT_ID)
            .map_err(|_| LocalError::Pair)?;
        credentials::claim_validated(
            &LocalTransport(Arc::clone(self)),
            LOCAL_SERVER_URL.to_owned(),
            &code,
            form_factor,
            display_name,
        )
        .map_err(|_: PairError| LocalError::Pair)
    }

    /// Revokes this computer's local device when leaving local mode, so a
    /// later return creates a fresh one instead of listing two.
    pub fn revoke(&self, device_id: &str) {
        let _ = self.state.revoke_device_now(DEFAULT_TENANT_ID, device_id);
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        // Stops the price refresh without waiting; safe on any thread.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// `{LOCAL_SERVER_URL}/api/...` to `/api/...`; anything else is refused.
fn local_path(url: &str) -> Result<&str, TransportError> {
    url.strip_prefix(LOCAL_SERVER_URL)
        .filter(|path| path.starts_with("/api/"))
        .ok_or(TransportError::Network)
}

/// Uploads and pairing for local mode: [`Transport`] over [`LocalServer`].
pub struct LocalTransport(pub Arc<LocalServer>);

impl LocalTransport {
    pub fn get(&self, url: &str, bearer: Option<&str>) -> Result<HttpResponse, TransportError> {
        self.0
            .request(Method::GET, local_path(url)?, bearer, false, &[])
    }
}

impl Transport for LocalTransport {
    fn post(
        &self,
        url: &str,
        bearer: Option<&str>,
        gzip: bool,
        body: &[u8],
    ) -> Result<HttpResponse, TransportError> {
        self.0
            .request(Method::POST, local_path(url)?, bearer, gzip, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn json_of(response: &HttpResponse) -> Value {
        serde_json::from_slice(&response.body).expect("json body")
    }

    #[test]
    fn the_built_in_server_pairs_stores_and_answers_the_dashboard() {
        let dir = tempfile::tempdir().expect("dir");
        let server = LocalServer::open_with(dir.path(), None).expect("open");
        let credentials = server
            .pair(FormFactor::Laptop, "Test laptop")
            .expect("pair");
        assert!(is_local(&credentials));
        let transport = LocalTransport(Arc::clone(&server));

        let events: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/usage-events.json"))
                .expect("fixture");
        let envelope = json!({
            "schemaVersion": 1,
            "deviceId": credentials.device_id,
            "batchId": "batch-1",
            "collector": {"version": "0.2.0", "tokscaleRev": "test", "parserGeneration": "test"},
            "scanGeneration": "test",
            "events": events,
        });
        let body = serde_json::to_vec(&envelope).expect("envelope");
        let url = format!("{LOCAL_SERVER_URL}/api/ingest/events");
        let stored = transport
            .post(&url, Some(&credentials.ingest_token), false, &body)
            .expect("ingest");
        assert_eq!(stored.status, 200);
        assert_eq!(json_of(&stored)["accepted"], 4);
        let again = transport
            .post(&url, Some(&credentials.ingest_token), false, &body)
            .expect("ingest again");
        assert_eq!(
            json_of(&again)["duplicates"],
            4,
            "same events are stored once"
        );

        let summary_url =
            format!("{LOCAL_SERVER_URL}/api/dashboard/summary?from=2023-11-01&to=2023-11-30");
        let summary = transport
            .get(&summary_url, Some(&credentials.read_token))
            .expect("summary");
        assert_eq!(summary.status, 200);
        assert_eq!(json_of(&summary)["inputTokens"], 540);
        let wrong_token = transport
            .get(&summary_url, Some(&credentials.ingest_token))
            .expect("summary with the ingest token");
        assert_eq!(wrong_token.status, 401, "the ingest token cannot read");

        server.revoke(&credentials.device_id);
        let revoked = transport
            .get(&summary_url, Some(&credentials.read_token))
            .expect("summary after revoke");
        assert_eq!(revoked.status, 401);
        drop(transport);
        drop(server);

        // Reopening keeps the data; a new device sees it once (D029).
        let server = LocalServer::open_with(dir.path(), None).expect("reopen");
        let next = server
            .pair(FormFactor::Laptop, "Test laptop")
            .expect("pair again");
        assert_ne!(next.device_id, credentials.device_id);
        let summary = LocalTransport(Arc::clone(&server))
            .get(&summary_url, Some(&next.read_token))
            .expect("summary");
        assert_eq!(json_of(&summary)["inputTokens"], 540);
    }

    #[test]
    fn a_second_open_of_the_same_database_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let _first = LocalServer::open_with(dir.path(), None).expect("open");
        assert!(LocalServer::open_with(dir.path(), None).is_err());
    }

    #[test]
    fn only_local_api_paths_are_routed() {
        assert_eq!(
            local_path("local://this-computer/api/ingest/events"),
            Ok("/api/ingest/events")
        );
        assert!(local_path("https://example.com/api/ingest/events").is_err());
        assert!(local_path("local://this-computer/healthz").is_err());
        assert!(local_path("local://this-computerevil/api/x").is_err());
    }

    #[test]
    fn credentials_file_round_trip_keeps_only_local_ones() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(load_credentials(dir.path()).is_none());
        let local = Credentials {
            server_url: LOCAL_SERVER_URL.to_owned(),
            device_id: "dev_local".to_owned(),
            ingest_token: "tdi_x".to_owned(),
            read_token: "tdr_y".to_owned(),
        };
        save_credentials(dir.path(), &local).expect("save");
        assert_eq!(load_credentials(dir.path()), Some(local.clone()));
        let remote = Credentials {
            server_url: "https://example.com".to_owned(),
            ..local
        };
        save_credentials(dir.path(), &remote).expect("save");
        assert!(load_credentials(dir.path()).is_none());
        delete_credentials(dir.path());
        assert!(load_credentials(dir.path()).is_none());
    }
}
