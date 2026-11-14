//! Account sign-in for the desktop app (D049). The session lives in the
//! system keychain under its own account name; the WebView only ever calls
//! Tauri commands and never sees the session or any device token.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use collector_core::credentials::Credentials;
use collector_core::transport::{self, Transport, UreqTransport};
use serde_json::{Value, json};
use usage_event::envelope::FormFactor;

use crate::remote::{CmdResult, CommandError};

/// The hosted service selected by default; a user may point at their own.
pub const CLOUD_URL: &str = "https://tokenlens.online";

const SESSION_ACCOUNT: &str = "account";
const MAX_EMAIL_CHARS: usize = 254;
const MAX_AVATAR_BYTES: u64 = 256 * 1024;

/// The WebView only receives a bounded raster data URL. Fetch public GitHub
/// avatars here, with a separate anonymous client, never the session transport.
fn avatar_data_url(value: &Value) -> Option<String> {
    let avatar = github_avatar_url(value.as_str()?)?;
    let agent = ureq::Agent::new_with_config(avatar_config(true));
    download_avatar(&agent, &avatar)
}

fn github_avatar_url(input: &str) -> Option<String> {
    if input.len() > 2048
        || input
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
    {
        return None;
    }
    let url = url::Url::parse(input).ok()?;
    let id = url.path().strip_prefix("/u/")?;
    if url.scheme() != "https"
        || url.host_str() != Some("avatars.githubusercontent.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || id.is_empty()
        || id.len() > 20
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    // Keep the provider's numeric avatar id only; do not forward an arbitrary
    // server-provided query string or allow it to request an oversized image.
    Some(format!("https://avatars.githubusercontent.com/u/{id}?s=64"))
}

fn avatar_config(https_only: bool) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .https_only(https_only)
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(3)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(if cfg!(any(target_os = "windows", target_os = "macos")) {
                    ureq::tls::TlsProvider::NativeTls
                } else {
                    ureq::tls::TlsProvider::Rustls
                })
                .build(),
        )
        .build()
}

fn download_avatar(agent: &ureq::Agent, url: &str) -> Option<String> {
    let mut response = agent
        .get(url)
        .header("Accept", "image/png,image/jpeg,image/gif,image/webp")
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mime = response
        .headers()
        .get("content-type")?
        .to_str()
        .ok()?
        .split(';')
        .next()?
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return None;
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_AVATAR_BYTES)
        .read_to_vec()
        .ok()?;
    let raster = match mime.as_str() {
        "image/png" => body.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => body.starts_with(b"\xff\xd8\xff"),
        "image/gif" => body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a"),
        "image/webp" => body.starts_with(b"RIFF") && body.get(8..12) == Some(b"WEBP"),
        _ => false,
    };
    raster.then(|| format!("data:{mime};base64,{}", STANDARD.encode(body)))
}

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
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
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
        display_name: body["displayName"].as_str().map(str::to_owned),
        avatar_url: avatar_data_url(&body["avatarUrl"]),
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

pub fn account_info(
    server: &str,
    session: &str,
) -> CmdResult<(String, Option<String>, Option<String>)> {
    let body = session_get(server, session, "/api/auth/me")?;
    let email = body["email"]
        .as_str()
        .map(str::to_owned)
        .ok_or(CommandError::new("server"))?;
    Ok((
        email,
        body["displayName"].as_str().map(str::to_owned),
        avatar_data_url(&body["avatarUrl"]),
    ))
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
            display_name: body["displayName"].as_str().map(str::to_owned),
            avatar_url: avatar_data_url(&body["avatarUrl"]),
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn avatar_fixture(status: &str, mime: &str, body: &[u8]) -> (Option<String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let url = format!("http://{}/avatar", listener.local_addr().expect("address"));
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nLocation: https://example.com/redirected\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            body.len()
        );
        let body = body.to_vec();
        let fixture = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("write timeout");
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 8192);
                stream.read_exact(&mut byte).expect("fixture request");
                request.push(byte[0]);
            }
            // Rejected responses can close before all bytes are written.
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.write_all(b"\r\n0\r\n\r\n");
            String::from_utf8(request).expect("request text")
        });
        // Only this transport fixture permits local HTTP. Production callers
        // validate GitHub HTTPS URLs before using the anonymous client.
        let agent = ureq::Agent::new_with_config(avatar_config(false));
        let result = download_avatar(&agent, &url);
        (result, fixture.join().expect("fixture completed"))
    }

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

    #[test]
    fn avatar_urls_only_keep_a_github_numeric_id_and_fixed_small_size() {
        assert_eq!(
            github_avatar_url(
                "https://avatars.githubusercontent.com/u/123?v=4&arbitrary=discarded"
            ),
            Some("https://avatars.githubusercontent.com/u/123?s=64".to_owned())
        );
        for input in [
            "http://avatars.githubusercontent.com/u/123",
            "https://avatars.githubusercontent.com.evil.example/u/123",
            "https://example.com/avatars.githubusercontent.com/u/123",
            "https://user:password@avatars.githubusercontent.com/u/123",
            "https://avatars.githubusercontent.com:444/u/123",
            "https://avatars.githubusercontent.com/u/123#fragment",
            "https://avatars.githubusercontent.com/u/not-an-id",
            "https://avatars.githubusercontent.com/u/",
            "https://avatars.githubusercontent.com/arbitrary/path",
            "data:image/png;base64,placeholder",
            "https://avatars.githubusercontent.com\\@example.com/u/123",
        ] {
            assert!(github_avatar_url(input).is_none(), "{input}");
        }
        assert!(avatar_data_url(&Value::Null).is_none());
        assert!(avatar_data_url(&json!("https://example.com/u/123")).is_none());
    }

    #[test]
    fn avatar_transport_is_bounded_and_never_follows_redirects() {
        let config = avatar_config(true);
        assert!(config.https_only());
        assert_eq!(config.max_redirects(), 0);
        assert_eq!(config.timeouts().global, Some(Duration::from_secs(3)));
        assert_eq!(config.timeouts().connect, Some(Duration::from_secs(2)));
    }

    #[test]
    fn an_avatar_becomes_inline_image_data_without_sending_credentials() {
        let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aPa0AAAAASUVORK5CYII=").expect("PNG fixture");
        let (data, request) = avatar_fixture("200 OK", "image/png; charset=binary", &png);
        assert_eq!(
            data,
            Some(format!("data:image/png;base64,{}", STANDARD.encode(&png)))
        );
        let request = request.to_ascii_lowercase();
        assert!(request.starts_with("get /avatar http/1.1\r\n"));
        assert!(!request.contains("authorization:"));
        assert!(!request.contains("cookie:"));
    }

    #[test]
    fn unsafe_oversized_redirected_or_failed_avatar_responses_fall_back() {
        for (status, mime, body) in [
            ("200 OK", "image/svg+xml", b"<svg/>".to_vec()),
            (
                "200 OK",
                "image/png",
                b"<script>invalid raster</script>".to_vec(),
            ),
            ("302 Found", "image/png", b"\x89PNG\r\n\x1a\n".to_vec()),
            ("404 Not Found", "image/png", b"\x89PNG\r\n\x1a\n".to_vec()),
            ("200 OK", "image/png", {
                let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
                bytes.resize(MAX_AVATAR_BYTES as usize + 1, 0);
                bytes
            }),
        ] {
            assert!(
                avatar_fixture(status, mime, &body).0.is_none(),
                "{status} {mime}"
            );
        }
    }
}
