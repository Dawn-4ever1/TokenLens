//! HTTP API: pairing (6.4), ingest (6.1, 6.3) and dashboard reads (8).
//!
//! Request logs carry only the method, the route template, the status and
//! the latency. Headers, bodies, tokens and pairing codes are never logged.

use std::io::Read;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono_tz::Tz;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use rusqlite::Connection;
use serde::Serialize;
use usage_event::UsageEvent;
use usage_event::envelope::{
    self, Heartbeat, HeartbeatResponse, IngestEnvelope, IngestResponse, PairClaimRequest,
    PairClaimResponse, PairCodeResponse, RejectReason, RejectedEvent,
};

use crate::clock::Clock;
use crate::dashboard::{self, Filters, QueryError};
use crate::db::{self, AuthContext, BatchMeta, NewDevice};
use crate::limiter::{ClaimLimiter, IngestLimiter};
use crate::pricing::Pricer;
use crate::secrets::{self, TokenKind};

pub const MIN_COLLECTOR_VERSION: &str = "0.1.0";
pub const MIN_APP_VERSION: &str = "0.1.0";
const SMALL_BODY_BYTES: usize = 16 * 1024;
const HEARTBEAT_BODY_BYTES: usize = 256 * 1024;
const MAX_HEARTBEAT_CLIENTS: usize = 256;
/// JavaScript numbers are exact up to 2^53 - 1; the dashboard reads them there.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone)]
pub struct AppState {
    db: Arc<Mutex<Connection>>,
    /// Read-only connection for dashboard reads; `None` shares `db`.
    reader: Option<Arc<Mutex<Connection>>>,
    clock: Arc<dyn Clock>,
    limiter: Arc<Mutex<ClaimLimiter>>,
    ingest_limiter: Arc<Mutex<IngestLimiter>>,
    time_zone: Tz,
    pricer: Arc<RwLock<Option<Arc<Pricer>>>>,
    pairing_cipher: Option<Result<crate::pairing_crypto::PairingCipher, &'static str>>,
    github: Option<std::sync::Arc<crate::github::GitHub>>,
    auth_limiter: Arc<Mutex<crate::limiter::AuthLimiter>>,
}

impl AppState {
    pub fn new(conn: Connection, clock: Arc<dyn Clock>) -> Self {
        Self {
            db: Arc::new(Mutex::new(conn)),
            reader: None,
            clock,
            limiter: Arc::new(Mutex::new(ClaimLimiter::default())),
            ingest_limiter: Arc::new(Mutex::new(IngestLimiter::default())),
            time_zone: crate::DEFAULT_TIME_ZONE,
            pricer: Arc::new(RwLock::new(None)),
            pairing_cipher: None,
            github: None,
            auth_limiter: Arc::new(Mutex::new(crate::limiter::AuthLimiter::default())),
        }
    }

    #[must_use]
    pub fn with_pairing_cipher(mut self, cipher: crate::pairing_crypto::PairingCipher) -> Self {
        self.pairing_cipher = Some(Ok(cipher));
        self
    }

    pub fn pairing_cipher(
        &self,
    ) -> Option<Result<crate::pairing_crypto::PairingCipher, &'static str>> {
        self.pairing_cipher.clone()
    }

    #[must_use]
    pub fn with_github(mut self, github: crate::github::GitHub) -> Self {
        self.github = Some(std::sync::Arc::new(github));
        self
    }

    pub fn github(&self) -> Option<std::sync::Arc<crate::github::GitHub>> {
        self.github.clone()
    }

    pub fn db_handle(&self) -> Result<Arc<Mutex<Connection>>, ApiError> {
        Ok(Arc::clone(&self.db))
    }

    pub fn allow_auth(&self, key: &str) -> bool {
        self.auth_limiter
            .lock()
            .map(|mut limiter| limiter.allow(key, self.now_ms()))
            .unwrap_or(false)
    }

    /// The price catalog new events are priced with (D042); `None` until one
    /// has loaded.
    pub fn pricer(&self) -> Option<Arc<Pricer>> {
        self.pricer.read().ok().and_then(|guard| guard.clone())
    }

    /// Switches to a new catalog; call `reprice` afterwards so stored events
    /// follow it.
    pub fn set_pricer(&self, pricer: Pricer) {
        if let Ok(mut guard) = self.pricer.write() {
            *guard = Some(Arc::new(pricer));
        }
    }

    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Re-prices every stored event whose price version differs from the
    /// current catalog, a batch per database lock so ingest and reads keep
    /// flowing. Returns how many events changed.
    pub async fn reprice(&self) -> Result<u64, db::DbError> {
        const BATCH: usize = 2_000;
        let Some(pricer) = self.pricer() else {
            return Ok(0);
        };
        let mut total = 0;
        loop {
            let db = Arc::clone(&self.db);
            let current = Arc::clone(&pricer);
            let changed = tokio::task::spawn_blocking(move || {
                let mut conn = db
                    .lock()
                    .map_err(|_| db::DbError::Corrupt("database lock poisoned".to_owned()))?;
                db::reprice_batch(&mut conn, &current, BATCH)
            })
            .await
            .map_err(|_| db::DbError::Corrupt("repricing task failed".to_owned()))??;
            total += changed as u64;
            if changed < BATCH {
                return Ok(total);
            }
        }
    }

    /// Issues a pairing code in `tenant_id` straight from the database. For
    /// the desktop app's built-in local server (D044), which has no operator
    /// CLI; the code is claimed through `/api/pair/claim` like any other.
    pub fn issue_pair_code(&self, tenant_id: &str) -> Result<String, db::DbError> {
        let conn = self
            .db
            .lock()
            .map_err(|_| db::DbError::Corrupt("database lock poisoned".to_owned()))?;
        let issued = db::create_pair_code_for_tenant(&conn, self.clock.now_ms(), tenant_id, None)?;
        Ok(crate::secrets::format_pair_code(&issued.code).into_inner())
    }

    /// Revokes a device and both of its tokens (D044: leaving local mode).
    pub fn revoke_device_now(&self, tenant_id: &str, device_id: &str) -> Result<bool, db::DbError> {
        let mut conn = self
            .db
            .lock()
            .map_err(|_| db::DbError::Corrupt("database lock poisoned".to_owned()))?;
        db::revoke_device(&mut conn, tenant_id, device_id, self.clock.now_ms())
    }

    /// Dashboard reads use this connection (see `db::open_reader`) instead of
    /// waiting for the one that takes uploads.
    #[must_use]
    pub fn with_reader(mut self, reader: Connection) -> Self {
        self.reader = Some(Arc::new(Mutex::new(reader)));
        self
    }

    /// Zone for day, week and month buckets on the dashboard.
    #[must_use]
    pub const fn with_time_zone(mut self, time_zone: Tz) -> Self {
        self.time_zone = time_zone;
        self
    }

    fn check_ingest_budget(&self, device_id: &str) -> Result<(), ApiError> {
        self.ingest_limiter
            .lock()
            .map_err(|_| ApiError::Internal)?
            .check(device_id, self.clock.now_ms())
            .map_err(ApiError::SlowDown)
    }

    /// Runs blocking database work off the async workers.
    async fn with_db<T, F>(&self, work: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, i64) -> Result<T, db::DbError> + Send + 'static,
    {
        let db = Arc::clone(&self.db);
        let now = self.clock.now_ms();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().map_err(|_| ApiError::Internal)?;
            work(&mut conn, now).map_err(|error| {
                tracing::error!(kind = error_kind(&error), "database operation failed");
                ApiError::Internal
            })
        })
        .await
        .map_err(|_| ApiError::Internal)?
    }
}

impl AppState {
    /// Dashboard reads: bad parameters become 400, database errors 500.
    async fn query<T, F>(&self, work: F) -> Result<Response, ApiError>
    where
        T: Serialize + Send + 'static,
        F: FnOnce(&Connection, Tz, i64) -> Result<T, QueryError> + Send + 'static,
    {
        let time_zone = self.time_zone;
        let Some(reader) = self.reader.clone() else {
            let value = self
                .with_db(move |conn, now| match work(conn, time_zone, now) {
                    Ok(value) => Ok(Ok(value)),
                    Err(QueryError::Bad) => Ok(Err(ApiError::BadRequest)),
                    Err(QueryError::Db(error)) => Err(error),
                })
                .await??;
            return Ok(axum::Json(value).into_response());
        };
        let now = self.clock.now_ms();
        let value = tokio::task::spawn_blocking(move || {
            let conn = reader.lock().map_err(|_| ApiError::Internal)?;
            let failed = |error: db::DbError| {
                tracing::error!(kind = error_kind(&error), "database read failed");
                ApiError::Internal
            };
            // One snapshot for the whole answer: an upload committing between
            // two statements must not make the totals disagree with the parts.
            let snapshot = conn
                .unchecked_transaction()
                .map_err(|error| failed(error.into()))?;
            match work(&snapshot, time_zone, now) {
                Ok(value) => Ok(value),
                Err(QueryError::Bad) => Err(ApiError::BadRequest),
                Err(QueryError::Db(error)) => Err(failed(error)),
            }
        })
        .await
        .map_err(|_| ApiError::Internal)??;
        Ok(axum::Json(value).into_response())
    }
}

fn error_kind(error: &db::DbError) -> &'static str {
    match error {
        db::DbError::Sqlite(_) => "sqlite",
        db::DbError::Io(_) => "io",
        db::DbError::NewerSchema { .. } => "schema",
        db::DbError::BackupExists(_) | db::DbError::Corrupt(_) => "backup",
        db::DbError::Busy => "busy",
        db::DbError::InvalidTenant(_)
        | db::DbError::TenantExists(_)
        | db::DbError::NoSuchTenant(_) => "tenant",
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/auth/code", post(crate::auth_http::request_code))
        .route("/api/auth/login", post(crate::auth_http::login))
        .route("/api/auth/me", get(crate::auth_http::me))
        .route("/api/auth/logout", post(crate::auth_http::logout))
        .route(
            "/api/auth/github/start",
            post(crate::auth_http::github_start),
        )
        .route("/api/auth/github/poll", post(crate::auth_http::github_poll))
        .route(
            "/api/account/pair-code",
            get(crate::auth_http::pairing_code).post(crate::auth_http::pairing_code),
        )
        .route("/api/account/devices", post(crate::auth_http::pair_device))
        .route("/api/pair/claim", post(pair_claim))
        .route("/api/pair/codes", post(pair_codes))
        .route("/api/ingest/events", post(ingest_events))
        .route("/api/ingest/heartbeat", post(ingest_heartbeat))
        .route("/api/dashboard/summary", get(dashboard_summary))
        .route("/api/dashboard/timeseries", get(dashboard_timeseries))
        .route("/api/dashboard/heatmap", get(dashboard_heatmap))
        .route("/api/dashboard/sessions", get(dashboard_sessions))
        .route("/api/dashboard/sessions/{key}", get(dashboard_session))
        .route("/api/dashboard/devices", get(dashboard_devices))
        .route("/api/dashboard/devices/{id}/rename", post(device_rename))
        .route("/api/dashboard/devices/{id}/rotate", post(device_rotate))
        .route("/api/dashboard/devices/{id}/revoke", post(device_revoke))
        .route("/api/dashboard/filters", get(dashboard_filters))
        .route("/api/dashboard/health", get(dashboard_health))
        .route("/healthz", get(healthz))
        .layer(middleware::from_fn(log_request))
        .with_state(state)
}

async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |path| path.as_str().to_owned());
    let start = Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        method = %method,
        route = %route,
        status = response.status().as_u16(),
        elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "request"
    );
    response
}

// ----------------------------------------------------------------- errors --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    BadRequest,
    InvalidPairCode,
    Unauthorized,
    DeviceMismatch,
    NotFound,
    PayloadTooLarge,
    UnsupportedEncoding,
    TooManyRequests,
    /// Per-device ingest budget used up; seconds until the next window.
    SlowDown(u64),
    Internal,
}

impl ApiError {
    pub const fn status(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::InvalidPairCode | Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::DeviceMismatch => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedEncoding => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::TooManyRequests | Self::SlowDown(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::InvalidPairCode => "invalid_pair_code",
            Self::Unauthorized => "unauthorized",
            Self::DeviceMismatch => "device_mismatch",
            Self::NotFound => "not_found",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedEncoding => "unsupported_encoding",
            Self::TooManyRequests | Self::SlowDown(_) => "too_many_requests",
            Self::Internal => "internal",
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response =
            (self.status(), axum::Json(ErrorBody { error: self.code() })).into_response();
        if self.status() == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Self::SlowDown(seconds) = self {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

/// Responses carrying a credential must never be cached anywhere.
fn no_store<T: Serialize>(value: &T) -> Response {
    let mut response = axum::Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// ------------------------------------------------------------------- auth --

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    (!token.is_empty() && token.len() <= 128).then_some(token)
}

/// Wrong, revoked or wrong-kind credentials all give the same 401.
async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    kind: TokenKind,
) -> Result<AuthContext, ApiError> {
    let Some(token) = bearer(headers) else {
        return Err(ApiError::Unauthorized);
    };
    let token = secrets::Secret::from_header(token);
    state
        .with_db(move |conn, now| db::authenticate(conn, token.expose(), kind, now))
        .await?
        .ok_or(ApiError::Unauthorized)
}

// ------------------------------------------------------------------ bodies --

#[derive(Clone, Copy)]
enum Encoding {
    Identity,
    Gzip,
}

fn content_encoding(headers: &HeaderMap) -> Result<Encoding, ApiError> {
    match headers.get(header::CONTENT_ENCODING) {
        None => Ok(Encoding::Identity),
        Some(value) => match value.to_str().map(|text| text.trim().to_ascii_lowercase()) {
            Ok(text) if text == "identity" => Ok(Encoding::Identity),
            Ok(text) if text == "gzip" => Ok(Encoding::Gzip),
            _ => Err(ApiError::UnsupportedEncoding),
        },
    }
}

async fn read_limited(body: Body, limit: usize) -> Result<Bytes, ApiError> {
    match Limited::new(body, limit).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            Err(ApiError::PayloadTooLarge)
        }
        Err(_) => Err(ApiError::BadRequest),
    }
}

/// Inflates while counting, and stops as soon as the output passes `limit`.
fn decode(bytes: &[u8], encoding: Encoding, limit: usize) -> Result<Vec<u8>, ApiError> {
    match encoding {
        Encoding::Identity => {
            if bytes.len() > limit {
                return Err(ApiError::PayloadTooLarge);
            }
            Ok(bytes.to_vec())
        }
        Encoding::Gzip => {
            let mut out = Vec::new();
            let mut decoder = flate2::read::GzDecoder::new(bytes).take(limit as u64 + 1);
            decoder
                .read_to_end(&mut out)
                .map_err(|_| ApiError::BadRequest)?;
            if out.len() > limit {
                return Err(ApiError::PayloadTooLarge);
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------- validity --

fn is_code_like(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn is_version_like(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-:".contains(&byte))
}

fn is_batch_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn is_plain_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control)
}

pub(crate) fn valid_display_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty() && name.chars().count() <= 64 && is_plain_text(name, 256))
        .then(|| name.to_owned())
}

fn collector_is_valid(collector: &envelope::CollectorInfo) -> bool {
    is_version_like(&collector.version)
        && is_version_like(&collector.tokscale_rev)
        && is_version_like(&collector.parser_generation)
        && collector
            .qoder_rate_version
            .as_deref()
            .is_none_or(is_version_like)
}

/// Server-side bounds on top of `UsageEvent::validate()`.
fn event_is_storable(event: &UsageEvent) -> bool {
    let key_ok = event.event_key.len() == 64
        && event
            .event_key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let tokens_ok = [
        event.input,
        event.output,
        event.cache_read,
        event.cache_write,
        event.cache_write_1h,
        event.reasoning,
    ]
    .iter()
    .all(|&value| value <= MAX_SAFE_INTEGER);
    let texts_ok = [
        Some(event.session_id.as_str()),
        event.parent_session_id.as_deref(),
        Some(event.model_id.as_str()),
        event.provider_id.as_deref(),
        Some(event.cost_source.as_str()),
        event.rate_version.as_deref(),
        event.project_hash.as_deref(),
        event.project_label.as_deref(),
        event.pricing_model_id.as_deref(),
        event.billing_provider.as_deref(),
        event.service_tier.as_deref(),
    ]
    .into_iter()
    .flatten()
    .all(|text| is_plain_text(text, MAX_TEXT_BYTES));
    key_ok
        && tokens_ok
        && texts_ok
        && is_code_like(&event.client, 64)
        && (0..=MAX_SAFE_INTEGER as i64).contains(&event.ts)
}

fn classify(raw: serde_json::Value) -> Result<UsageEvent, RejectReason> {
    let event = serde_json::from_value::<UsageEvent>(raw).map_err(|error| {
        if error.to_string().starts_with("unknown field") {
            RejectReason::UnknownField
        } else {
            RejectReason::InvalidEvent
        }
    })?;
    if event.validate().is_err() || !event_is_storable(&event) {
        return Err(RejectReason::InvalidValue);
    }
    Ok(event)
}

// --------------------------------------------------------------- handlers --

async fn pair_claim(State(state): State<AppState>, body: Body) -> Result<Response, ApiError> {
    if state
        .limiter
        .lock()
        .map_err(|_| ApiError::Internal)?
        .is_blocked(state.clock.now_ms())
    {
        return Err(ApiError::TooManyRequests);
    }
    let bytes = read_limited(body, SMALL_BODY_BYTES).await?;
    let request =
        serde_json::from_slice::<PairClaimRequest>(&bytes).map_err(|_| ApiError::BadRequest)?;
    let display_name = valid_display_name(&request.display_name).ok_or(ApiError::BadRequest)?;
    let claimed = match secrets::normalize_pair_code(&request.code) {
        None => None,
        Some(code) => {
            let hash = secrets::pair_code_hash(&code);
            let platform = request.platform.as_str();
            let form_factor = request.form_factor.as_str();
            state
                .with_db(move |conn, now| {
                    let one_time = db::claim_pair_code(
                        conn,
                        &hash,
                        now,
                        &NewDevice {
                            platform,
                            form_factor,
                            display_name: &display_name,
                        },
                    )?;
                    if one_time.is_some() {
                        return Ok(one_time);
                    }
                    crate::accounts::claim_tenant_code(
                        conn,
                        &hash,
                        now,
                        &NewDevice {
                            platform,
                            form_factor,
                            display_name: &display_name,
                        },
                    )
                    .map_err(|error| match error {
                        crate::accounts::AccountError::Unavailable => db::DbError::Corrupt(
                            "persistent pairing code could not be used".to_owned(),
                        ),
                        _ => db::DbError::Corrupt("pairing failed".to_owned()),
                    })
                })
                .await?
        }
    };
    let Some(claimed) = claimed else {
        state
            .limiter
            .lock()
            .map_err(|_| ApiError::Internal)?
            .record_failure(state.clock.now_ms());
        return Err(ApiError::InvalidPairCode);
    };
    tracing::info!(device = %claimed.device_id, "device paired");
    Ok(no_store(&PairClaimResponse {
        device_id: claimed.device_id,
        ingest_token: claimed.ingest_token.into_inner(),
        read_token: claimed.read_token.into_inner(),
    }))
}

async fn pair_codes(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let bytes = read_limited(body, SMALL_BODY_BYTES).await?;
    if !bytes.iter().all(u8::is_ascii_whitespace) {
        serde_json::from_slice::<EmptyObject>(&bytes).map_err(|_| ApiError::BadRequest)?;
    }
    let creator = caller.device_id.clone();
    let issued = state
        .with_db(move |conn, now| db::create_pair_code(conn, now, Some(&creator)))
        .await?;
    tracing::info!(device = %caller.device_id, tenant = %caller.tenant_id, "pairing code issued");
    Ok(no_store(&PairCodeResponse {
        code: secrets::format_pair_code(&issued.code).into_inner(),
        expires_at: issued.expires_at,
    }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyObject {}

async fn ingest_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Ingest).await?;
    state.check_ingest_budget(&caller.device_id)?;
    let encoding = content_encoding(&headers)?;
    let bytes = read_limited(body, envelope::MAX_BODY_BYTES).await?;
    let expected_device = caller.device_id.clone();
    let parsed =
        tokio::task::spawn_blocking(move || parse_batch(&bytes, encoding, &expected_device))
            .await
            .map_err(|_| ApiError::Internal)??;
    let ParsedBatch {
        envelope,
        events,
        rejected,
    } = parsed;
    let writer_device = caller.device_id.clone();
    let writer_tenant = caller.tenant_id.clone();
    let rejected_count = rejected.len();
    let pricer = state.pricer();
    let (counts, store_id) = state
        .with_db(move |conn, now| {
            let counts = db::write_batch(
                conn,
                &writer_tenant,
                &writer_device,
                &BatchMeta {
                    batch_id: &envelope.batch_id,
                    collector: &envelope.collector,
                    scan_generation: &envelope.scan_generation,
                    rejected: rejected_count,
                },
                &events,
                pricer.as_deref(),
                now,
            )?;
            Ok((counts, db::store_id(conn)?))
        })
        .await?;
    tracing::info!(
        device = %caller.device_id,
        accepted = counts.accepted,
        updated = counts.updated,
        duplicates = counts.duplicates,
        rejected = rejected_count,
        "batch stored"
    );
    Ok(axum::Json(IngestResponse {
        accepted: counts.accepted,
        updated: counts.updated,
        duplicates: counts.duplicates,
        rejected,
        min_collector_version: MIN_COLLECTOR_VERSION.to_owned(),
        min_app_version: MIN_APP_VERSION.to_owned(),
        store_id: Some(store_id),
    })
    .into_response())
}

struct ParsedBatch {
    envelope: IngestEnvelope,
    events: Vec<UsageEvent>,
    rejected: Vec<RejectedEvent>,
}

fn parse_batch(bytes: &[u8], encoding: Encoding, device_id: &str) -> Result<ParsedBatch, ApiError> {
    let json = decode(bytes, encoding, envelope::MAX_DECOMPRESSED_BYTES)?;
    let mut envelope =
        serde_json::from_slice::<IngestEnvelope>(&json).map_err(|_| ApiError::BadRequest)?;
    if envelope.schema_version != envelope::SCHEMA_VERSION
        || !is_batch_id(&envelope.batch_id)
        || !is_version_like(&envelope.scan_generation)
        || !collector_is_valid(&envelope.collector)
    {
        return Err(ApiError::BadRequest);
    }
    if envelope.events.len() > envelope::MAX_EVENTS_PER_BATCH {
        return Err(ApiError::PayloadTooLarge);
    }
    if envelope.device_id != device_id {
        return Err(ApiError::DeviceMismatch);
    }
    let mut events = Vec::with_capacity(envelope.events.len());
    let mut rejected = Vec::new();
    for (index, raw) in std::mem::take(&mut envelope.events).into_iter().enumerate() {
        match classify(raw) {
            Ok(event) => events.push(event),
            Err(reason) => rejected.push(RejectedEvent { index, reason }),
        }
    }
    Ok(ParsedBatch {
        envelope,
        events,
        rejected,
    })
}

async fn ingest_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Ingest).await?;
    state.check_ingest_budget(&caller.device_id)?;
    let encoding = content_encoding(&headers)?;
    let bytes = read_limited(body, HEARTBEAT_BODY_BYTES).await?;
    let json = decode(&bytes, encoding, HEARTBEAT_BODY_BYTES)?;
    let heartbeat = serde_json::from_slice::<Heartbeat>(&json).map_err(|_| ApiError::BadRequest)?;
    let valid = heartbeat.schema_version == envelope::SCHEMA_VERSION
        && collector_is_valid(&heartbeat.collector)
        && heartbeat.clients.len() <= MAX_HEARTBEAT_CLIENTS
        && heartbeat
            .os_name
            .as_deref()
            .is_none_or(|text| is_plain_text(text, 64))
        && heartbeat
            .os_version
            .as_deref()
            .is_none_or(|text| is_plain_text(text, 64))
        && heartbeat.clients.iter().all(|client| {
            is_code_like(&client.client, 64)
                && client
                    .error_code
                    .as_deref()
                    .is_none_or(|code| is_code_like(code, 64))
        });
    if !valid {
        return Err(ApiError::BadRequest);
    }
    if heartbeat.device_id != caller.device_id {
        return Err(ApiError::DeviceMismatch);
    }
    let writer_device = caller.device_id.clone();
    let writer_tenant = caller.tenant_id.clone();
    let store_id = state
        .with_db(move |conn, now| {
            db::record_heartbeat(conn, &writer_tenant, &writer_device, &heartbeat, now)?;
            db::store_id(conn)
        })
        .await?;
    tracing::info!(device = %caller.device_id, "heartbeat stored");
    Ok(axum::Json(HeartbeatResponse {
        min_collector_version: MIN_COLLECTOR_VERSION.to_owned(),
        min_app_version: MIN_APP_VERSION.to_owned(),
        store_id: Some(store_id),
    })
    .into_response())
}

// -------------------------------------------------------------- dashboard --

fn filters(raw: Option<&str>, extra: &[&str]) -> Result<(dashboard::Query, Filters), ApiError> {
    let query = dashboard::parse_query(raw, extra).map_err(|_| ApiError::BadRequest)?;
    let filters = Filters::from_query(&query).map_err(|_| ApiError::BadRequest)?;
    Ok((query, filters))
}

fn bad(_: QueryError) -> ApiError {
    ApiError::BadRequest
}

async fn dashboard_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let (_, filters) = filters(raw.as_deref(), &[])?;
    let pricer = state.pricer();
    state
        .query(move |conn, tz, now| {
            let pricing = pricer.as_deref().and_then(|pricer| {
                dashboard::PricingInfo::new(tz, pricer.as_of_ms(), pricer.version())
            });
            dashboard::summary(conn, tz, &filters, &caller.tenant_id, pricing, now)
        })
        .await
}

async fn dashboard_timeseries(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let (query, filters) = filters(raw.as_deref(), &["bucket", "groupBy", "limit"])?;
    let bucket = dashboard::choice(
        &query,
        "bucket",
        dashboard::Bucket::Day,
        &dashboard::BUCKETS,
    )
    .map_err(bad)?;
    let group_by = dashboard::choice(
        &query,
        "groupBy",
        dashboard::GroupBy::None,
        &dashboard::GROUPS,
    )
    .map_err(bad)?;
    let request = dashboard::SeriesQuery {
        bucket,
        group_by,
        limit: dashboard::number(&query, "limit", 8, 20).map_err(bad)?,
    };
    state
        .query(move |conn, tz, now| {
            dashboard::timeseries(conn, tz, &filters, request, &caller.tenant_id, now)
        })
        .await
}

async fn dashboard_heatmap(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let (query, filters) = filters(raw.as_deref(), &["kind"])?;
    let kind = dashboard::choice(
        &query,
        "kind",
        dashboard::HeatmapKind::Calendar,
        &dashboard::HEATMAPS,
    )
    .map_err(bad)?;
    state
        .query(move |conn, tz, now| {
            dashboard::heatmap(conn, tz, &filters, kind, &caller.tenant_id, now)
        })
        .await
}

async fn dashboard_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let (query, filters) = filters(raw.as_deref(), &["q", "sort", "cursor", "limit"])?;
    let request = dashboard::SessionQuery {
        search: dashboard::text(&query, "q").map_err(bad)?,
        sort: dashboard::choice(
            &query,
            "sort",
            dashboard::SessionSort::Recent,
            &dashboard::SESSION_SORTS,
        )
        .map_err(bad)?,
        offset: dashboard::parse_cursor(query.get("cursor").cloned()).map_err(bad)?,
        limit: dashboard::number(
            &query,
            "limit",
            dashboard::DEFAULT_PAGE,
            dashboard::MAX_PAGE,
        )
        .map_err(bad)?,
    };
    state
        .query(move |conn, tz, now| {
            dashboard::sessions(conn, tz, &filters, &request, &caller.tenant_id, now)
        })
        .await
}

async fn dashboard_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    dashboard::parse_query(raw.as_deref(), &[])
        .ok()
        .filter(dashboard::Query::is_empty)
        .ok_or(ApiError::BadRequest)?;
    let detail = state
        .with_db(move |conn, now| {
            match dashboard::session_detail(conn, &caller.tenant_id, &key, now) {
                Ok(detail) => Ok(detail),
                Err(QueryError::Bad) => Ok(None),
                Err(QueryError::Db(error)) => Err(error),
            }
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(axum::Json(detail).into_response())
}

async fn dashboard_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let (_, filters) = filters(raw.as_deref(), &[])?;
    state
        .query(move |conn, tz, now| {
            dashboard::devices(
                conn,
                tz,
                &filters,
                &caller.tenant_id,
                &caller.device_id,
                now,
            )
        })
        .await
}

async fn dashboard_filters(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    state
        .query(move |conn, tz, _| dashboard::filter_options(conn, tz, &caller.tenant_id))
        .await
}

async fn dashboard_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    state
        .query(move |conn, tz, now| dashboard::health(conn, tz, &caller.tenant_id, now))
        .await
}

/// Liveness for the container healthcheck; reveals nothing.
async fn healthz() -> Response {
    axum::Json(serde_json::json!({"status": "ok"})).into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenameRequest {
    display_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenameResponse {
    device_id: String,
    display_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevokeResponse {
    device_id: String,
    revoked: bool,
}

fn device_id_param(id: &str) -> Result<String, ApiError> {
    (id.starts_with("dev_") && is_batch_id(id))
        .then(|| id.to_owned())
        .ok_or(ApiError::NotFound)
}

async fn device_rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let id = device_id_param(&id)?;
    let bytes = read_limited(body, SMALL_BODY_BYTES).await?;
    let request =
        serde_json::from_slice::<RenameRequest>(&bytes).map_err(|_| ApiError::BadRequest)?;
    let name = valid_display_name(&request.display_name).ok_or(ApiError::BadRequest)?;
    let (target, stored, tenant) = (id.clone(), name.clone(), caller.tenant_id.clone());
    let renamed = state
        .with_db(move |conn, _| db::rename_device(conn, &tenant, &target, &stored))
        .await?;
    if !renamed {
        return Err(ApiError::NotFound);
    }
    tracing::info!(device = %caller.device_id, target = %id, tenant = %caller.tenant_id, "device renamed");
    Ok(axum::Json(RenameResponse {
        device_id: id,
        display_name: name,
    })
    .into_response())
}

/// Only a device's own read token may rotate it, so a stolen read token of
/// one device cannot mint working credentials for another.
async fn device_rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let id = device_id_param(&id)?;
    if id != caller.device_id {
        return Err(ApiError::DeviceMismatch);
    }
    let target = id.clone();
    let tenant = caller.tenant_id.clone();
    let rotated = state
        .with_db(move |conn, now| db::rotate_tokens(conn, &tenant, &target, now))
        .await?
        .ok_or(ApiError::NotFound)?;
    tracing::info!(device = %id, "device tokens rotated");
    Ok(no_store(&PairClaimResponse {
        device_id: id,
        ingest_token: rotated.ingest_token.into_inner(),
        read_token: rotated.read_token.into_inner(),
    }))
}

async fn device_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let caller = authenticate(&state, &headers, TokenKind::Read).await?;
    let id = device_id_param(&id)?;
    let target = id.clone();
    let tenant = caller.tenant_id.clone();
    let revoked = state
        .with_db(move |conn, now| db::revoke_device(conn, &tenant, &target, now))
        .await?;
    if !revoked {
        return Err(ApiError::NotFound);
    }
    tracing::info!(device = %caller.device_id, target = %id, tenant = %caller.tenant_id, "device revoked");
    Ok(axum::Json(RevokeResponse {
        device_id: id,
        revoked: true,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_decoding_stops_at_the_limit() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&vec![b' '; 4096]).expect("write");
        let compressed = encoder.finish().expect("finish");
        assert_eq!(
            decode(&compressed, Encoding::Gzip, 4096).map(|out| out.len()),
            Ok(4096)
        );
        assert_eq!(
            decode(&compressed, Encoding::Gzip, 4095),
            Err(ApiError::PayloadTooLarge)
        );
        assert_eq!(
            decode(b"not gzip", Encoding::Gzip, 10),
            Err(ApiError::BadRequest)
        );
    }

    #[test]
    fn unknown_fields_are_told_apart_from_other_event_errors() {
        let fixtures = serde_json::from_str::<serde_json::Value>(include_str!(
            "../../fixtures/usage-events.json"
        ))
        .expect("fixture");
        let mut raw = fixtures[0].clone();
        assert!(classify(raw.clone()).is_ok());
        raw["prompt"] = serde_json::json!("x");
        assert_eq!(classify(raw.clone()), Err(RejectReason::UnknownField));
        raw.as_object_mut().expect("object").remove("prompt");
        raw["input"] = serde_json::json!(-1);
        assert_eq!(classify(raw.clone()), Err(RejectReason::InvalidEvent));
        raw["input"] = serde_json::json!(MAX_SAFE_INTEGER + 1);
        assert_eq!(classify(raw), Err(RejectReason::InvalidValue));
    }
}
