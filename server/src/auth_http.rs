//! HTTP account/session API (D049). Tenant selection comes only from the
//! authenticated account, never from a request field.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, LengthLimitError, Limited};

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

fn json<T: serde::Serialize>(value: &T) -> Response {
    let mut response = axum::Json(value).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn profile_json(state: &AppState, account: &Account) -> serde_json::Value {
    let profile = state.db_handle().ok().and_then(|db| {
        db.lock().ok().and_then(|conn| {
            accounts::identity_profile(&conn, &account.id)
                .ok()
                .flatten()
        })
    });
    let (provider, name, avatar) = profile.unwrap_or((None, None, None));
    serde_json::json!({ "provider": provider, "displayName": name, "avatarUrl": avatar })
}

fn account_reply(state: &AppState, account: Account) -> Response {
    let profile = profile_json(state, &account);
    json(&serde_json::json!({
        "email": account.email,
        "spaceName": "我的空间",
        "provider": profile["provider"],
        "displayName": profile["displayName"],
        "avatarUrl": profile["avatarUrl"],
    }))
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
                "provider": serde_json::Value::Null,
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
        Ok(account) => account_reply(&state, account),
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
        crate::github::Poll::Ready {
            subject,
            email,
            display_name,
            avatar_url,
        } => {
            let outcome = {
                let Ok(db) = state.db_handle() else {
                    return ApiError::Internal.into_response();
                };
                let Ok(mut conn) = db.lock() else {
                    return ApiError::Internal.into_response();
                };
                accounts::login_with_identity(
                    &mut conn,
                    "github",
                    &subject,
                    &email,
                    display_name.as_deref(),
                    avatar_url.as_deref(),
                    state.now_ms(),
                )
            };
            match outcome {
                Ok(login) => json(&serde_json::json!({
                    "status": "ok",
                    "email": login.account.email,
                    "spaceName": "我的空间",
                    "session": login.session.expose(),
                    "via": "github",
                    "provider": "github",
                    "displayName": display_name,
                    "avatarUrl": avatar_url,
                })),
                Err(error) => account_error(error),
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GooglePollRequest {
    flow_id: String,
}

/// A minimal static page for the browser that Google redirects back to.
/// No scripts, no external resources, and no request data is interpolated.
fn html_page(status: StatusCode, title: &str, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title><style>\
         body{{font-family:system-ui,sans-serif;display:flex;min-height:100vh;\
         align-items:center;justify-content:center;margin:0;background:#faf7f2;color:#2b2620}}\
         main{{text-align:center;padding:2rem}}h1{{font-size:1.25rem}}p{{color:#6b6257}}\
         </style></head><body><main><h1>{title}</h1><p>{message}</p></main></body></html>"
    );
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Starts a Google authorization-code flow (D051). The client secret stays
/// here; the app only receives the poll id and the consent page to open.
pub async fn google_start(State(state): State<AppState>) -> Response {
    let Some(google) = state.google() else {
        return account_error(AccountError::Unavailable);
    };
    if !state.allow_auth("google-start") {
        return account_error(AccountError::TooManyRequests);
    }
    let Some(started) = state.google_flows().start(state.now_ms()) else {
        return account_error(AccountError::TooManyRequests);
    };
    json(&serde_json::json!({
        "flowId": started.flow_id,
        "authUrl": google.auth_url(&started.state, &started.challenge),
        "intervalSeconds": crate::google::FLOW_INTERVAL_SECONDS,
        "expiresInSeconds": crate::google::FLOW_TTL_MS / 1000,
    }))
}

/// Google redirects the browser here after consent. The exchange and profile
/// lookup run outside the database lock, exactly like the GitHub poll.
pub async fn google_callback(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if state.google().is_none() {
        return html_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "登录不可用",
            "服务器还没有配置 Google 登录。",
        );
    }
    let flow_state = params.get("state").map(String::as_str).unwrap_or_default();
    if flow_state.is_empty() || flow_state.len() > 256 {
        return html_page(
            StatusCode::BAD_REQUEST,
            "登录没有完成",
            "授权信息不对，请回到 TokenLens 重新开始。",
        );
    }
    if let Some(error) = params.get("error") {
        let (outcome, message) = if error == "access_denied" {
            (
                crate::google::Outcome::Denied,
                "你取消了授权。请回到 TokenLens 重新开始。",
            )
        } else {
            (
                crate::google::Outcome::Failed,
                "Google 授权没有成功，请回到 TokenLens 重试。",
            )
        };
        state
            .google_flows()
            .complete(flow_state, outcome, state.now_ms());
        return html_page(StatusCode::OK, "登录没有完成", message);
    }
    let Some(code) = params
        .get("code")
        .filter(|code| !code.is_empty() && code.len() <= 2048)
    else {
        return html_page(
            StatusCode::BAD_REQUEST,
            "登录没有完成",
            "授权信息不对，请回到 TokenLens 重新开始。",
        );
    };
    let Some(verifier) = state.google_flows().verifier(flow_state, state.now_ms()) else {
        return html_page(
            StatusCode::BAD_REQUEST,
            "登录没有完成",
            "授权已过期或不存在，请回到 TokenLens 重新开始。",
        );
    };
    let Some(google) = state.google() else {
        return html_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "登录不可用",
            "服务器还没有配置 Google 登录。",
        );
    };
    let identity = google
        .exchange(code, &verifier)
        .and_then(|token| google.identity(&token));
    let Ok(identity) = identity else {
        state
            .google_flows()
            .complete(flow_state, crate::google::Outcome::Failed, state.now_ms());
        return html_page(
            StatusCode::OK,
            "登录没有完成",
            "Google 授权没有成功，请回到 TokenLens 重试。",
        );
    };
    let outcome = {
        let Ok(db) = state.db_handle() else {
            return ApiError::Internal.into_response();
        };
        let Ok(mut conn) = db.lock() else {
            return ApiError::Internal.into_response();
        };
        accounts::login_with_identity(
            &mut conn,
            "google",
            &identity.subject,
            &identity.email,
            identity.display_name.as_deref(),
            identity.avatar_url.as_deref(),
            state.now_ms(),
        )
    };
    match outcome {
        Ok(login) => {
            state.google_flows().complete(
                flow_state,
                crate::google::Outcome::Ready {
                    session: login.session,
                    email: login.account.email,
                    display_name: identity.display_name,
                    avatar_url: identity.avatar_url,
                },
                state.now_ms(),
            );
            html_page(
                StatusCode::OK,
                "登录成功",
                "请回到 TokenLens 应用，登录会自动完成。",
            )
        }
        Err(_) => {
            state.google_flows().complete(
                flow_state,
                crate::google::Outcome::Failed,
                state.now_ms(),
            );
            html_page(
                StatusCode::OK,
                "登录没有完成",
                "Google 授权没有成功，请回到 TokenLens 重试。",
            )
        }
    }
}

/// One poll of a started Google flow. The session is delivered once.
pub async fn google_poll(State(state): State<AppState>, body: Body) -> Response {
    if state.google().is_none() {
        return account_error(AccountError::Unavailable);
    }
    let Ok(bytes) = read_body(body).await else {
        return ApiError::BadRequest.into_response();
    };
    let Ok(request) = serde_json::from_slice::<GooglePollRequest>(&bytes) else {
        return ApiError::BadRequest.into_response();
    };
    if request.flow_id.is_empty() || request.flow_id.len() > 256 {
        return ApiError::BadRequest.into_response();
    }
    match state.google_flows().poll(&request.flow_id, state.now_ms()) {
        crate::google::FlowPoll::Pending => json(&serde_json::json!({ "status": "pending" })),
        crate::google::FlowPoll::Denied => json(&serde_json::json!({ "status": "denied" })),
        crate::google::FlowPoll::Failed => json(&serde_json::json!({ "status": "failed" })),
        crate::google::FlowPoll::Expired => json(&serde_json::json!({ "status": "expired" })),
        crate::google::FlowPoll::Ready {
            session,
            email,
            display_name,
            avatar_url,
        } => json(&serde_json::json!({
            "status": "ok",
            "email": email,
            "spaceName": "我的空间",
            "session": session.expose(),
            "provider": "google",
            "displayName": display_name,
            "avatarUrl": avatar_url,
        })),
    }
}
