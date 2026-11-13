//! Account sign-in for the desktop app (D049). The session lives in the
//! system keychain under its own account name; the WebView only ever calls
//! Tauri commands and never sees the session or any device token.

use collector_core::credentials::Credentials;
use collector_core::transport::{self, Transport, UreqTransport};
use serde_json::{Value, json};
use usage_event::envelope::FormFactor;

use crate::remote::{CmdResult, CommandError};

/// The hosted service selected by default; a user may point at their own.
pub const CLOUD_URL: &str = "https://tokenlens.online";

const SESSION_ACCOUNT: &str = "account";
const MAX_EMAIL_CHARS: usize = 254;

fn keyring_entry(service: &str) -> Result<keyring::Entry, CommandError> {
    keyring::Entry::new(service, SESSION_ACCOUNT).map_err(|_| CommandError::new("keychain"))
}

pub fn load_session(service: &str) -> Option<String> {
    let entry = keyring_entry(service).ok()?;
    match entry.get_password() {
        Ok(value) if value.starts_with("tls_") => Some(value),
        _ => None,
    }
}

pub fn save_session(service: &str, session: &str) -> CmdResult<()> {
    keyring_entry(service)?
        .set_password(session)
        .map_err(|_| CommandError::new("keychain"))
}

pub fn delete_session(service: &str) {
    if let Ok(entry) = keyring_entry(service) {
        let _ = entry.delete_credential();
    }
}

pub fn clean_email(email: &str) -> Option<String> {
    let email = email.trim().to_ascii_lowercase();
    let ok = !email.is_empty()
        && email.chars().count() <= MAX_EMAIL_CHARS
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.ends_with('.')
        })
        && !email
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control());
    ok.then_some(email)
}

fn clean_code(code: &str) -> Option<String> {
    let code = code.trim().replace(['-', ' '], "");
    (code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())).then_some(code)
}

fn body_json(response: &collector_core::transport::HttpResponse) -> CmdResult<Value> {
    serde_json::from_slice(&response.body).map_err(|_| CommandError::new("server"))
}

fn status_error(status: u16, body: &[u8]) -> CommandError {
    let code = transport::error_code(body);
    match (status, code.as_deref()) {
        (401, Some("invalid_login_code" | "expired_login_code")) => CommandError::new("bad_code"),
        (429, _) => CommandError::new("rate_limited"),
        (503, _) => CommandError::new("unavailable"),
        (400, Some("bad_email")) => CommandError::new("bad_email"),
        _ => CommandError::from_status(status),
    }
}

fn server_url(server: &str) -> CmdResult<String> {
    transport::validate_server_url(server).map_err(|_| CommandError::new("bad_request"))
}

/// Asks the server to email a short-lived sign-in code.
pub fn request_code(server: &str, email: &str) -> CmdResult<()> {
    let server = server_url(server)?;
    let email = clean_email(email).ok_or(CommandError::new("bad_email"))?;
    let payload = serde_json::to_vec(&json!({ "email": email }))
        .map_err(|_| CommandError::new("bad_request"))?;
    let response = UreqTransport::new()
        .post(&format!("{server}/api/auth/code"), None, false, &payload)
        .map_err(|_| CommandError::new("network"))?;
    if response.status == 200 {
        return Ok(());
    }
    Err(status_error(response.status, &response.body))
}

pub struct SignedIn {
    pub session: String,
    pub email: String,
    pub space_name: String,
}

/// Exchanges an emailed code for a session. A first sign-in creates the space.
pub fn login(server: &str, email: &str, code: &str) -> CmdResult<SignedIn> {
    let server = server_url(server)?;
    let email = clean_email(email).ok_or(CommandError::new("bad_email"))?;
    let code = clean_code(code).ok_or(CommandError::new("bad_code"))?;
    let payload = serde_json::to_vec(&json!({ "email": email, "code": code }))
        .map_err(|_| CommandError::new("bad_request"))?;
    let response = UreqTransport::new()
        .post(&format!("{server}/api/auth/login"), None, false, &payload)
        .map_err(|_| CommandError::new("network"))?;
    if response.status != 200 {
        return Err(status_error(response.status, &response.body));
    }
    let body = body_json(&response)?;
    let session = body["session"]
        .as_str()
        .filter(|value| value.starts_with("tls_"))
        .ok_or(CommandError::new("server"))?
        .to_owned();
    Ok(SignedIn {
        session,
        email: body["email"].as_str().unwrap_or(&email).to_owned(),
        space_name: body["spaceName"].as_str().unwrap_or("我的空间").to_owned(),
    })
}

fn session_get(server: &str, session: &str, path: &str) -> CmdResult<Value> {
    let response = UreqTransport::new()
        .get(&format!("{server}{path}"), Some(session))
        .map_err(|_| CommandError::new("network"))?;
    if response.status != 200 {
        return Err(status_error(response.status, &response.body));
    }
    body_json(&response)
}

fn session_post(server: &str, session: &str, path: &str, body: &Value) -> CmdResult<Value> {
    let payload = serde_json::to_vec(body).map_err(|_| CommandError::new("bad_request"))?;
    let response = UreqTransport::new()
        .post(&format!("{server}{path}"), Some(session), false, &payload)
        .map_err(|_| CommandError::new("network"))?;
    if response.status != 200 {
        return Err(status_error(response.status, &response.body));
    }
    body_json(&response)
}

pub fn account_email(server: &str, session: &str) -> CmdResult<String> {
    let body = session_get(server, session, "/api/auth/me")?;
    body["email"]
        .as_str()
        .map(str::to_owned)
        .ok_or(CommandError::new("server"))
}

/// Creates this computer's device inside the signed-in space and returns the
/// usual device credentials, which still go to the keychain.
pub fn create_device(
    server: &str,
    session: &str,
    form_factor: FormFactor,
    name: &str,
) -> CmdResult<Credentials> {
    let server = server_url(server)?;
    let body = session_post(
        &server,
        session,
        "/api/account/devices",
        &json!({
            "platform": collector_core::credentials::this_platform().as_str(),
            "formFactor": form_factor.as_str(),
            "displayName": name.trim(),
        }),
    )?;
    let text = |name: &str| body[name].as_str().map(str::to_owned);
    let credential = Credentials {
        server_url: server,
        device_id: text("deviceId").ok_or(CommandError::new("server"))?,
        ingest_token: text("ingestToken").ok_or(CommandError::new("server"))?,
        read_token: text("readToken").ok_or(CommandError::new("server"))?,
    };
    let valid = |token: &str, prefix: &str| {
        token.starts_with(prefix)
            && token.len() <= 256
            && token.bytes().all(|b| b.is_ascii_graphic())
    };
    if !valid(&credential.ingest_token, "tdi_") || !valid(&credential.read_token, "tdr_") {
        return Err(CommandError::new("server"));
    }
    Ok(credential)
}

/// The space's replaceable pairing code, shown only after signing in.
pub fn pair_code(server: &str, session: &str, rotate: bool) -> CmdResult<String> {
    let body = session_post(
        server,
        session,
        "/api/account/pair-code",
        &json!({ "rotate": rotate }),
    )?;
    body["code"]
        .as_str()
        .map(str::to_owned)
        .ok_or(CommandError::new("server"))
}

/// Starts GitHub's device flow. The device code stays in Rust; the WebView
/// only receives the short human code and the page to open.
pub struct GitHubStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
}

pub enum GitHubPoll {
    Pending,
    SlowDown(u64),
    Expired,
    Denied,
    Failed,
    SignedIn(SignedIn),
}

pub fn github_start(server: &str) -> CmdResult<GitHubStart> {
    let server = server_url(server)?;
    let response = UreqTransport::new()
        .post(
            &format!("{server}/api/auth/github/start"),
            None,
            false,
            b"{}",
        )
        .map_err(|_| CommandError::new("network"))?;
    if response.status != 200 {
        return Err(status_error(response.status, &response.body));
    }
    let body = body_json(&response)?;
    let text = |name: &str| body[name].as_str().map(str::to_owned);
    Ok(GitHubStart {
        device_code: text("deviceCode").ok_or(CommandError::new("server"))?,
        user_code: text("userCode").ok_or(CommandError::new("server"))?,
        verification_uri: text("verificationUri").ok_or(CommandError::new("server"))?,
    })
}

pub fn github_poll(server: &str, device_code: &str) -> CmdResult<GitHubPoll> {
    let server = server_url(server)?;
    let payload = serde_json::to_vec(&json!({ "deviceCode": device_code }))
        .map_err(|_| CommandError::new("bad_request"))?;
    let response = UreqTransport::new()
        .post(
            &format!("{server}/api/auth/github/poll"),
            None,
            false,
            &payload,
        )
        .map_err(|_| CommandError::new("network"))?;
    if response.status != 200 {
        return Err(status_error(response.status, &response.body));
    }
    let body = body_json(&response)?;
    Ok(match body["status"].as_str().unwrap_or("failed") {
        "pending" => GitHubPoll::Pending,
        "slow_down" => GitHubPoll::SlowDown(body["retryAfterSeconds"].as_u64().unwrap_or(5)),
        "expired" => GitHubPoll::Expired,
        "denied" => GitHubPoll::Denied,
        "ok" => GitHubPoll::SignedIn(SignedIn {
            session: body["session"]
                .as_str()
                .filter(|value| value.starts_with("tls_"))
                .ok_or(CommandError::new("server"))?
                .to_owned(),
            email: body["email"].as_str().unwrap_or_default().to_owned(),
            space_name: body["spaceName"].as_str().unwrap_or("我的空间").to_owned(),
        }),
        _ => GitHubPoll::Failed,
    })
}

pub fn logout(server: &str, session: &str) {
    let _ = session_post(server, session, "/api/auth/logout", &json!({}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_and_codes_are_checked_before_any_request() {
        assert_eq!(
            clean_email(" User@Example.COM ").as_deref(),
            Some("user@example.com")
        );
        for value in ["", "user", "user@localhost", "a b@example.com"] {
            assert!(clean_email(value).is_none(), "{value}");
        }
        assert_eq!(clean_code(" 12-34 56 ").as_deref(), Some("123456"));
        assert!(clean_code("12345").is_none());
        assert!(clean_code("abcdef").is_none());
    }

    #[test]
    fn the_cloud_service_is_the_default_and_is_https() {
        assert!(CLOUD_URL.starts_with("https://"));
        assert!(transport::validate_server_url(CLOUD_URL).is_ok());
    }
}
