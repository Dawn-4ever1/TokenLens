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
    /// Random per-installation request id. It is not a device secret or a
    /// hardware fingerprint and is never accepted as tenant identity.
    #[serde(default)]
    client_binding_id: Option<String>,
}

fn valid_client_binding_id(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn json<T: serde::Serialize>(value: &T) -> Response {
    let mut response = axum::Json(value).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn run_blocking<T, F>(work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| ApiError::Internal)
}

async fn run_db<T, F>(state: &AppState, work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&mut rusqlite::Connection, i64) -> T + Send + 'static,
{
    let db = state.db_handle()?;
    let now = state.now_ms();
    run_blocking(move || {
        let mut conn = db.lock().map_err(|_| ApiError::Internal)?;
        Ok(work(&mut conn, now))
    })
    .await?
}

async fn profile_json(state: &AppState, account: &Account) -> serde_json::Value {
    let account_id = account.id.clone();
    let profile = run_db(state, move |conn, _| {
        accounts::identity_profile(conn, &account_id).ok().flatten()
    })
    .await
    .ok()
    .flatten();
    let (provider, name, avatar) = profile.unwrap_or((None, None, None));
    serde_json::json!({ "provider": provider, "displayName": name, "avatarUrl": avatar })
}

async fn account_reply(state: &AppState, account: Account) -> Response {
    let profile = profile_json(state, &account).await;
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

async fn authorized_account(state: &AppState, headers: &HeaderMap) -> Result<Account, ApiError> {
    let Some(token) = bearer(headers) else {
        return Err(ApiError::Unauthorized);
    };
    run_db(state, move |conn, now| {
        accounts::authenticate(conn, &token, now)
            .map_err(|_| ApiError::Internal)?
            .ok_or(ApiError::Unauthorized)
    })
    .await?
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
    let stage_email = email.clone();
    let staged = match run_db(&state, move |conn, now| {
        accounts::stage_code(conn, &stage_email, now)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => return error.into_response(),
    };
    let code = match staged {
        Ok(code) => code,
        Err(error) => return account_error(error),
    };
    let send_email = email.clone();
    let send_result = run_blocking(move || mailer.send_code(&send_email, code.expose())).await;
    if !matches!(send_result, Ok(Ok(()))) {
        let delete_email = email.clone();
        let _ = run_db(&state, move |conn, _| {
            let _ = accounts::delete_challenge(conn, &delete_email);
        })
        .await;
        return account_error(AccountError::Unavailable);
    }
    let mark_email = email;
    let _ = run_db(&state, move |conn, _| {
        let _ = accounts::mark_sent(conn, &mark_email);
    })
    .await;
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
    let code = request.code;
    let outcome = match run_db(&state, move |conn, now| {
        accounts::login(conn, &email, &code, now)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => return error.into_response(),
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
    match authorized_account(&state, &headers).await {
        Ok(account) => account_reply(&state, account).await,
        Err(error) => error.into_response(),
    }
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = bearer(&headers) {
        let _ = run_db(&state, move |conn, now| {
            let _ = accounts::logout(conn, &token, now);
        })
        .await;
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
    let account = match authorized_account(&state, &headers).await {
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
    let tenant_id = account.tenant_id;
    let result = match run_db(&state, move |conn, now| {
        accounts::tenant_code(conn, &tenant_id, rotate, now, &cipher)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => return error.into_response(),
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
    let account = match authorized_account(&state, &headers).await {
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
    if request
        .client_binding_id
        .as_deref()
        .is_some_and(|id| !valid_client_binding_id(id))
    {
        return ApiError::BadRequest.into_response();
    }
    let platform = request.platform.as_str();
    let form_factor = request.form_factor.as_str();
    let binding_id = request.client_binding_id;
    let outcome = match run_db(&state, move |conn, now| {
        let device = db::NewDevice {
            platform,
            form_factor,
            display_name: &name,
        };
        accounts::create_device_for_account(
            conn,
            &account.id,
            &account.tenant_id,
            now,
            &device,
            binding_id.as_deref(),
        )
    })
    .await
    {
        Ok(result) => result,
        Err(error) => return error.into_response(),
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

/// The signed-in account's public avatar, fetched server-side (T31 follow-up):
/// some networks cannot reach Google's picture hosts directly, while the app
/// always reaches this server. Only the stored, validated provider URL is
/// fetched, bounded and cached in memory.
pub async fn account_avatar(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let account = match authorized_account(&state, &headers).await {
        Ok(account) => account,
        Err(error) => return error.into_response(),
    };
    let account_id = account.id;
    let profile = match run_db(&state, move |conn, _| {
        accounts::identity_profile(conn, &account_id).ok().flatten()
    })
    .await
    {
        Ok(profile) => profile,
        Err(error) => return error.into_response(),
    };
    let Some((_, _, Some(url))) = profile else {
        return ApiError::NotFound.into_response();
    };
    let avatar_data = state.avatar_data();
    let now = state.now_ms();
    let fetched = match run_blocking(move || avatar_data.image(&url, now)).await {
        Ok(fetched) => fetched,
        Err(error) => return error.into_response(),
    };
    let Some((bytes, mime)) = fetched else {
        return ApiError::NotFound.into_response();
    };
    let mut response = (StatusCode::OK, [(header::CONTENT_TYPE, mime)], bytes).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, max-age=3600"),
    );
    response
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitHubPollRequest {
    device_code: String,
}

/// Starts a GitHub device-flow sign-in. The client id is configuration; no
/// client secret is involved and nothing is stored yet.
pub async fn github_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(github) = state.github() else {
        return account_error(AccountError::Unavailable);
    };
    let client_key = http::client_rate_key(&headers);
    if !state.allow_auth_scoped("github-start", &client_key) {
        return account_error(AccountError::TooManyRequests);
    }
    match run_blocking(move || github.start()).await {
        Ok(Ok(start)) => json(&serde_json::json!({
            "deviceCode": start.device_code,
            "userCode": start.user_code,
            "verificationUri": start.verification_uri,
            "intervalSeconds": start.interval_seconds,
            "expiresInSeconds": start.expires_in_seconds,
        })),
        Ok(Err(_)) | Err(_) => account_error(AccountError::Unavailable),
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
    let device_code = request.device_code;
    let polled = match run_blocking(move || github.poll(&device_code)).await {
        Ok(polled) => polled,
        Err(_) => return account_error(AccountError::Unavailable),
    };
    match polled {
        crate::github::Poll::Pending => json(&serde_json::json!({ "status": "pending" })),
        crate::github::Poll::SlowDown(seconds) => {
            json(&serde_json::json!({ "status": "slow_down", "retryAfterSeconds": seconds }))
        }
        crate::github::Poll::Expired => json(&serde_json::json!({ "status": "expired" })),
        crate::github::Poll::Denied => json(&serde_json::json!({ "status": "denied" })),
        crate::github::Poll::Failed(failure) => {
            tracing::warn!(failure = failure.code(), "GitHub device flow failed");
            json(&serde_json::json!({
                "status": "failed",
                "failureCode": failure.code(),
            }))
        }
        crate::github::Poll::Ready {
            subject,
            email,
            display_name,
            avatar_url,
        } => {
            let reply_name = display_name.clone();
            let reply_avatar = avatar_url.clone();
            let outcome = match run_db(&state, move |conn, now| {
                accounts::login_with_identity(
                    conn,
                    "github",
                    &subject,
                    &email,
                    display_name.as_deref(),
                    avatar_url.as_deref(),
                    now,
                )
            })
            .await
            {
                Ok(result) => result,
                Err(error) => return error.into_response(),
            };
            match outcome {
                Ok(login) => json(&serde_json::json!({
                    "status": "ok",
                    "email": login.account.email,
                    "spaceName": "我的空间",
                    "session": login.session.expose(),
                    "via": "github",
                    "provider": "github",
                    "displayName": reply_name,
                    "avatarUrl": reply_avatar,
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
pub async fn google_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(google) = state.google() else {
        return account_error(AccountError::Unavailable);
    };
    let client_key = http::client_rate_key(&headers);
    if !state.allow_auth_scoped("google-start", &client_key) {
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
/// lookup run outside the database lock and off the async runtime workers,
/// exactly like the GitHub poll.
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
    let code = code.clone();
    let identity = run_blocking(move || {
        google
            .exchange(&code, &verifier)
            .and_then(|token| google.identity(&token))
    })
    .await;
    let Ok(Ok(identity)) = identity else {
        state
            .google_flows()
            .complete(flow_state, crate::google::Outcome::Failed, state.now_ms());
        return html_page(
            StatusCode::OK,
            "登录没有完成",
            "Google 授权没有成功，请回到 TokenLens 重试。",
        );
    };
    let reply_name = identity.display_name.clone();
    let reply_avatar = identity.avatar_url.clone();
    let outcome = run_db(&state, move |conn, now| {
        accounts::login_with_identity(
            conn,
            "google",
            &identity.subject,
            &identity.email,
            identity.display_name.as_deref(),
            identity.avatar_url.as_deref(),
            now,
        )
    })
    .await;
    match outcome {
        Ok(Ok(login)) => {
            state.google_flows().complete(
                flow_state,
                crate::google::Outcome::Ready {
                    session: login.session,
                    email: login.account.email,
                    display_name: reply_name,
                    avatar_url: reply_avatar,
                },
                state.now_ms(),
            );
            html_page(
                StatusCode::OK,
                "登录成功",
                "请回到 TokenLens 应用，登录会自动完成。",
            )
        }
        Ok(Err(_)) | Err(_) => {
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
