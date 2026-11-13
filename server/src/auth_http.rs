//! HTTP account/session API (D049). Tenant selection comes only from the
//! authenticated account, never from a request field.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use serde::Serialize;

use crate::accounts::{self, Account, AccountError};
use crate::db;
use crate::http::{self, ApiError, AppState};
use crate::mailer::Mailer;

const BODY_LIMIT: usize = 16 * 1024;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailRequest {
    email: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    email: String,
    code: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RotateRequest {
    #[serde(default)]
    rotate: bool,
}

/// A signed-in app asks for its own device tokens; the account's space is
/// implied by the session, so no tenant or code is accepted here.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewDeviceRequest {
    platform: usage_event::envelope::Platform,
    form_factor: usage_event::envelope::FormFactor,
    display_name: String,
}

#[derive(Serialize)]
struct AccountReply {
    email: String,
    space_name: String,
}

fn json<T: Serialize>(value: &T) -> Response {
    let mut response = axum::Json(value).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn account_reply(account: Account) -> Response {
    json(&AccountReply {
        email: account.email,
        space_name: "我的空间".to_owned(),
    })
}

fn session_cookie(session: &str) -> String {
    format!(
        "tokenlens_session={session}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Lax",
        accounts::SESSION_TTL_MS / 1000
    )
}

fn clear_cookie() -> &'static str {
    "tokenlens_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(token) = http::bearer(headers) {
        return Some(token.to_owned());
    }
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            part.trim()
                .strip_prefix("tokenlens_session=")
                .map(str::to_owned)
        })
}

fn authorized_account(state: &AppState, headers: &HeaderMap) -> Result<Account, ApiError> {
    let Some(token) = bearer(headers) else {
        return Err(ApiError::Unauthorized);
    };
    let db = state.db_handle()?;
    let conn = db.lock().map_err(|_| ApiError::Internal)?;
    accounts::authenticate(&conn, &token, state.now_ms())
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::Unauthorized)
}

fn account_error(error: AccountError) -> Response {
    let (status, code) = match error {
        AccountError::BadEmail => (StatusCode::BAD_REQUEST, "bad_email"),
        AccountError::BadCode => (StatusCode::UNAUTHORIZED, "invalid_login_code"),
        AccountError::ExpiredCode => (StatusCode::UNAUTHORIZED, "expired_login_code"),
        AccountError::TooManyAttempts => (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"),
        AccountError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
        AccountError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    let mut response = (status, axum::Json(serde_json::json!({"error": code}))).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn read_body(body: Body) -> Result<Vec<u8>, ApiError> {
    Limited::new(body.into_data_stream(), BODY_LIMIT)
        .collect()
        .await
        .map(|bytes| bytes.to_bytes().to_vec())
        .map_err(|error| {
            if error.downcast_ref::<LengthLimitError>().is_some() {
                ApiError::PayloadTooLarge
            } else {
                ApiError::BadRequest
            }
        })
}

pub async fn request_code(State(state): State<AppState>, body: Body) -> Response {
    let Ok(bytes) = read_body(body).await else {
        return ApiError::BadRequest.into_response();
    };
    let Ok(request) = serde_json::from_slice::<EmailRequest>(&bytes) else {
        return ApiError::BadRequest.into_response();
    };
    let Some(email) = accounts::normalize_email(&request.email) else {
        return account_error(AccountError::BadEmail);
    };
    let Some(mailer) = Mailer::from_env() else {
        return account_error(AccountError::Unavailable);
    };
    if !state.allow_auth(&email) {
        return account_error(AccountError::TooManyRequests);
    }
    let staged = {
        let Ok(db) = state.db_handle() else {
            return ApiError::Internal.into_response();
        };
        let Ok(conn) = db.lock() else {
            return ApiError::Internal.into_response();
        };
        accounts::stage_code(&conn, &email, state.now_ms())
    };
    let code = match staged {
        Ok(code) => code,
        Err(error) => return account_error(error),
    };
    if mailer.send_code(&email, code.expose()).is_err() {
        if let Ok(db) = state.db_handle()
            && let Ok(conn) = db.lock()
        {
            let _ = accounts::delete_challenge(&conn, &email);
        }
        return account_error(AccountError::Unavailable);
    }
    if let Ok(db) = state.db_handle()
        && let Ok(conn) = db.lock()
    {
        let _ = accounts::mark_sent(&conn, &email);
    }
    json(&serde_json::json!({"sent": true, "retryAfterSeconds": accounts::SEND_INTERVAL_MS / 1000}))
}

pub async fn login(State(state): State<AppState>, body: Body) -> Response {
    let Ok(bytes) = read_body(body).await else {
        return ApiError::BadRequest.into_response();
    };
    let Ok(request) = serde_json::from_slice::<LoginRequest>(&bytes) else {
        return ApiError::BadRequest.into_response();
    };
    let Some(email) = accounts::normalize_email(&request.email) else {
        return account_error(AccountError::BadEmail);
    };
    if !state.allow_auth(&email) {
        return account_error(AccountError::TooManyRequests);
    }
    let outcome = {
        let Ok(db) = state.db_handle() else {
            return ApiError::Internal.into_response();
        };
        let Ok(mut conn) = db.lock() else {
            return ApiError::Internal.into_response();
        };
        accounts::login(&mut conn, &email, &request.code, state.now_ms())
    };
    match outcome {
        Ok(login) => {
            // The desktop app keeps this session in the system keychain; the
            // WebView only ever calls Tauri commands (D022). A future web
            // front end switches to the HttpOnly cookie below instead.
            let mut response = json(&serde_json::json!({
                "email": login.account.email,
                "spaceName": "我的空间",
                "session": login.session.expose(),
            }));
            if let Ok(cookie) =
                header::HeaderValue::from_str(&session_cookie(login.session.expose()))
            {
                response.headers_mut().append(header::SET_COOKIE, cookie);
            }
            response
        }
        Err(error) => account_error(error),
    }
}

pub async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match authorized_account(&state, &headers) {
        Ok(account) => account_reply(account),
        Err(error) => error.into_response(),
    }
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = bearer(&headers)
        && let Ok(db) = state.db_handle()
        && let Ok(conn) = db.lock()
    {
        let _ = accounts::logout(&conn, &token, state.now_ms());
    }
    let mut response = json(&serde_json::json!({"loggedOut": true}));
    response.headers_mut().append(
        header::SET_COOKIE,
        header::HeaderValue::from_static(clear_cookie()),
    );
    response
}

pub async fn pairing_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let account = match authorized_account(&state, &headers) {
        Ok(account) => account,
        Err(error) => return error.into_response(),
    };
    let rotate = match read_body(body).await {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => false,
        Ok(bytes) => match serde_json::from_slice::<RotateRequest>(&bytes) {
            Ok(request) => request.rotate,
            Err(_) => return ApiError::BadRequest.into_response(),
        },
        Err(_) => return ApiError::BadRequest.into_response(),
    };
    let Some(cipher) = state.pairing_cipher().and_then(|value| value.ok()) else {
        return ApiError::Internal.into_response();
    };
    let result = {
        let Ok(db) = state.db_handle() else {
            return ApiError::Internal.into_response();
        };
        let Ok(mut conn) = db.lock() else {
            return ApiError::Internal.into_response();
        };
        accounts::tenant_code(
            &mut conn,
            &account.tenant_id,
            rotate,
            state.now_ms(),
            &cipher,
        )
    };
    match result {
        Ok(code) => json(
            &serde_json::json!({"code": code.expose(), "persistent": true, "replaceable": true}),
        ),
        Err(error) => account_error(error),
    }
}

pub async fn pair_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let account = match authorized_account(&state, &headers) {
        Ok(account) => account,
        Err(error) => return error.into_response(),
    };
    let Ok(bytes) = read_body(body).await else {
        return ApiError::BadRequest.into_response();
    };
    let Ok(request) = serde_json::from_slice::<NewDeviceRequest>(&bytes) else {
        return ApiError::BadRequest.into_response();
    };
    let Some(name) = http::valid_display_name(&request.display_name) else {
        return ApiError::BadRequest.into_response();
    };
    let device = db::NewDevice {
        platform: request.platform.as_str(),
        form_factor: request.form_factor.as_str(),
        display_name: &name,
    };
    let outcome = {
        let Ok(db) = state.db_handle() else {
            return ApiError::Internal.into_response();
        };
        let Ok(mut conn) = db.lock() else {
            return ApiError::Internal.into_response();
        };
        accounts::create_device_for_account(&mut conn, &account.tenant_id, state.now_ms(), &device)
    };
    match outcome {
        Ok(device) => json(&usage_event::envelope::PairClaimResponse {
            device_id: device.device_id,
            ingest_token: device.ingest_token.into_inner(),
            read_token: device.read_token.into_inner(),
        }),
        Err(error) => account_error(error),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitHubPollRequest {
    device_code: String,
}

/// Starts a GitHub device-flow sign-in. The client id is configuration; no
/// client secret is involved and nothing is stored yet.
pub async fn github_start(State(state): State<AppState>) -> Response {
    let Some(github) = state.github() else {
        return account_error(AccountError::Unavailable);
    };
    if !state.allow_auth("github-start") {
        return account_error(AccountError::TooManyRequests);
    }
    match github.start() {
        Ok(start) => json(&serde_json::json!({
            "deviceCode": start.device_code,
            "userCode": start.user_code,
            "verificationUri": start.verification_uri,
            "intervalSeconds": start.interval_seconds,
            "expiresInSeconds": start.expires_in_seconds,
        })),
        Err(_) => account_error(AccountError::Unavailable),
    }
}

/// One poll of a started device flow. GitHub's answer decides whether the
/// user is still pending, approved, or refused.
pub async fn github_poll(State(state): State<AppState>, body: Body) -> Response {
    let Some(github) = state.github() else {
        return account_error(AccountError::Unavailable);
    };
    let Ok(bytes) = read_body(body).await else {
        return ApiError::BadRequest.into_response();
    };
    let Ok(request) = serde_json::from_slice::<GitHubPollRequest>(&bytes) else {
        return ApiError::BadRequest.into_response();
    };
    if request.device_code.is_empty() || request.device_code.len() > 256 {
        return ApiError::BadRequest.into_response();
    }
    match github.poll(&request.device_code) {
        crate::github::Poll::Pending => json(&serde_json::json!({ "status": "pending" })),
        crate::github::Poll::SlowDown(seconds) => {
            json(&serde_json::json!({ "status": "slow_down", "retryAfterSeconds": seconds }))
        }
        crate::github::Poll::Expired => json(&serde_json::json!({ "status": "expired" })),
        crate::github::Poll::Denied => json(&serde_json::json!({ "status": "denied" })),
        crate::github::Poll::Failed => json(&serde_json::json!({ "status": "failed" })),
        crate::github::Poll::Ready { subject, email } => {
            let outcome = {
                let Ok(db) = state.db_handle() else {
                    return ApiError::Internal.into_response();
                };
                let Ok(mut conn) = db.lock() else {
                    return ApiError::Internal.into_response();
                };
                accounts::login_with_identity(&mut conn, "github", &subject, &email, state.now_ms())
            };
            match outcome {
                Ok(login) => json(&serde_json::json!({
                    "status": "ok",
                    "email": login.account.email,
                    "spaceName": "我的空间",
                    "session": login.session.expose(),
                    "via": "github",
                })),
                Err(error) => account_error(error),
            }
        }
    }
}
