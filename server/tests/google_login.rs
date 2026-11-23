//! Google authorization-code sign-in (D051): a stub Google proves the client
//! secret never leaves the server, PKCE binds the exchange to the started
//! flow, only verified emails join a space, and repeat sign-ins reuse it.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, atomic::AtomicUsize};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::Harness;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "stub-google-client";
const CLIENT_SECRET: &str = "stub-google-secret";

/// A minimal stand-in for Google's token and userinfo endpoints.
struct Stub {
    base: String,
    userinfo: Arc<Mutex<Value>>,
    verifiers: Arc<Mutex<Vec<String>>>,
    exchanges: Arc<AtomicUsize>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let userinfo = Arc::new(Mutex::new(json!({
            "sub": "10421042",
            "email": "guser@example.com",
            "email_verified": true,
            "name": "G User",
            "picture": "https://lh3.googleusercontent.com/a/ACg8ocJx=s96-c"
        })));
        let verifiers = Arc::new(Mutex::new(Vec::new()));
        let exchanges = Arc::new(AtomicUsize::new(0));
        let profile = Arc::clone(&userinfo);
        let captured = Arc::clone(&verifiers);
        let counter = Arc::clone(&exchanges);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                let mut length = 0_usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    request.push_str(&line);
                }
                let mut body_bytes = vec![0_u8; length];
                let _ = std::io::Read::read_exact(&mut reader, &mut body_bytes);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.starts_with("/token") {
                    let form = String::from_utf8(body_bytes).expect("form body");
                    assert!(form.contains("grant_type=authorization_code"), "{form}");
                    assert!(form.contains(&format!("client_id={CLIENT_ID}")), "{form}");
                    assert!(
                        form.contains(&format!("client_secret={CLIENT_SECRET}")),
                        "{form}"
                    );
                    assert!(form.contains("code=stub-auth-code"), "{form}");
                    assert!(
                        form.contains("redirect_uri=http%3A%2F%2F127.0.0.1"),
                        "{form}"
                    );
                    let verifier = form
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("code_verifier="))
                        .expect("code_verifier")
                        .to_owned();
                    assert!(!verifier.is_empty());
                    captured.lock().expect("lock").push(verifier);
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    json!({ "access_token": "ya29.stub", "token_type": "Bearer", "expires_in": 3600 })
                } else if path.starts_with("/v1/userinfo") {
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("authorization: bearer ya29.stub"),
                        "{request}"
                    );
                    profile.lock().expect("lock").clone()
                } else {
                    json!({ "error": "not_found" })
                };
                let text = serde_json::to_string(&body).expect("json");
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    text.len(),
                    text
                );
                let _ = stream.flush();
            }
        });
        Self {
            base,
            userinfo,
            verifiers,
            exchanges,
        }
    }
}

async fn harness_with(stub: &Stub) -> Harness {
    Harness::start_with_google(CLIENT_ID, CLIENT_SECRET, &stub.base, &stub.base).await
}

fn query_param(url: &str, name: &str) -> String {
    url.split_once('?')
        .expect("auth url has a query")
        .1
        .split('&')
        .find_map(|pair| {
            pair.split_once('=')
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| value.to_owned())
        })
        .unwrap_or_else(|| panic!("missing {name} in {url}"))
}

/// Starts a flow and returns `(flow_id, state, auth_url)`.
async fn start_flow(h: &Harness) -> (String, String, String) {
    let (status, start) = h
        .post("/api/auth/google/start", None, b"{}".to_vec(), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let flow_id = start["flowId"].as_str().expect("flow id").to_owned();
    let auth_url = start["authUrl"].as_str().expect("auth url").to_owned();
    assert!(auth_url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    assert!(auth_url.contains(&format!("client_id={CLIENT_ID}")));
    assert!(auth_url.contains("scope=openid+email+profile"));
    assert!(auth_url.contains("code_challenge_method=S256"));
    assert!(auth_url.contains("prompt=select_account"));
    assert!(!auth_url.contains(CLIENT_SECRET));
    assert_eq!(start["expiresInSeconds"], 600);
    let state = query_param(&auth_url, "state");
    (flow_id, state, auth_url)
}

/// The browser leg: Google redirects back to the harness callback.
async fn callback(h: &Harness, query: &str) -> (StatusCode, String) {
    let response = h
        .http
        .get(format!("{}/api/auth/google/callback?{query}", h.base))
        .send()
        .await
        .expect("callback");
    let status = response.status();
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(|v| v.as_bytes()),
        Some(&b"no-store"[..])
    );
    let text = response.text().await.expect("page");
    (status, text)
}

async fn poll(h: &Harness, flow_id: &str) -> (StatusCode, Value) {
    h.post(
        "/api/auth/google/poll",
        None,
        serde_json::to_vec(&json!({ "flowId": flow_id })).expect("encode"),
        None,
    )
    .await
}

#[tokio::test]
async fn authorization_code_flow_signs_in_and_reuses_the_same_space() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, state, auth_url) = start_flow(&h).await;

    // Before consent the app keeps polling.
    let (status, body) = poll(&h, &flow_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "pending");

    let (status, page) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("登录成功"), "{page}");
    assert!(!page.contains(CLIENT_SECRET));

    // PKCE: the verifier sent to Google hashes to the challenge the app opened.
    let verifier = stub
        .verifiers
        .lock()
        .expect("lock")
        .last()
        .cloned()
        .expect("verifier");
    assert_eq!(
        query_param(&auth_url, "code_challenge"),
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    );

    let (status, body) = poll(&h, &flow_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["provider"], "google");
    assert_eq!(body["email"], "guser@example.com");
    assert_eq!(body["displayName"], "G User");
    assert_eq!(
        body["avatarUrl"],
        "https://lh3.googleusercontent.com/a/ACg8ocJx=s96-c"
    );
    let session = body["session"].as_str().expect("session").to_owned();
    assert!(session.starts_with("tls_"));
    assert!(!body.to_string().contains(CLIENT_SECRET));

    let (status, me) = h.get("/api/auth/me", Some(&session)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], "guser@example.com");
    assert_eq!(me["provider"], "google");
    assert_eq!(me["displayName"], "G User");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM tenants WHERE id <> 'tenant_personal'"),
        1
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM account_identities"), 1);

    // A second sign-in with the same Google user is the same account.
    let (flow_id, state, _) = start_flow(&h).await;
    let (status, _) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 1);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM tenants WHERE id <> 'tenant_personal'"),
        1,
        "no second space for the same Google identity"
    );
    assert!(stub.exchanges.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn an_unverified_google_email_cannot_create_a_space() {
    let stub = Stub::start();
    stub.userinfo.lock().expect("lock")["email_verified"] = json!(false);
    let h = harness_with(&stub).await;
    let (flow_id, state, _) = start_flow(&h).await;
    let (status, page) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("登录没有完成"), "{page}");
    let (status, body) = poll(&h, &flow_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "failed");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 0);
}

#[tokio::test]
async fn google_sign_in_is_unavailable_until_configured() {
    let h = Harness::start().await;
    let (status, body) = h
        .post("/api/auth/google/start", None, b"{}".to_vec(), None)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, body) = poll(&h, "x").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, page) = callback(&h, "code=x&state=y").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{page}");
}

#[tokio::test]
async fn a_callback_with_an_unknown_state_is_rejected() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, _, _) = start_flow(&h).await;
    let (status, page) = callback(&h, "code=stub-auth-code&state=not-a-real-state").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{page}");
    assert!(page.contains("登录没有完成"), "{page}");
    assert_eq!(stub.exchanges.load(std::sync::atomic::Ordering::SeqCst), 0);
    // The real flow is untouched and still pending.
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "pending");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 0);
}

#[tokio::test]
async fn denying_consent_ends_the_flow() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, state, _) = start_flow(&h).await;
    let (status, page) = callback(&h, &format!("error=access_denied&state={state}")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("取消了授权"), "{page}");
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "denied");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 0);
}

#[tokio::test]
async fn an_expired_flow_cannot_sign_in() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, state, _) = start_flow(&h).await;
    h.clock
        .advance_ms(tokenlens_server::google::FLOW_TTL_MS + 1);
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "expired");
    let (status, page) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{page}");
    assert_eq!(stub.exchanges.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 0);
}

#[tokio::test]
async fn the_session_is_delivered_only_once() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, state, _) = start_flow(&h).await;
    let (status, _) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "ok");
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(
        body["status"], "expired",
        "a second poll must not replay the session"
    );
}

#[tokio::test]
async fn the_same_verified_email_merges_github_and_google() {
    // A GitHub stub whose verified email matches the Google stub's.
    let github_listener = TcpListener::bind("127.0.0.1:0").expect("bind github");
    let github_base = format!("http://{}", github_listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for stream in github_listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let mut length = 0_usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body_bytes = vec![0_u8; length];
            let _ = std::io::Read::read_exact(&mut reader, &mut body_bytes);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let body = if path.starts_with("/login/device/code") {
                json!({
                    "device_code": "device-secret-1",
                    "user_code": "ABCD-1234",
                    "verification_uri": "https://github.com/login/device",
                    "interval": 1,
                    "expires_in": 900
                })
            } else if path.starts_with("/login/oauth/access_token") {
                json!({ "access_token": "gho_stub", "token_type": "bearer" })
            } else if path.starts_with("/user/emails") {
                json!([{ "email": "guser@example.com", "primary": true, "verified": true }])
            } else if path.starts_with("/user") {
                json!({ "id": 4242, "login": "octocat", "avatar_url": "https://avatars.githubusercontent.com/u/4242?v=4" })
            } else {
                json!({ "error": "not_found" })
            };
            let text = serde_json::to_string(&body).expect("json");
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                text.len(),
                text
            );
            let _ = stream.flush();
        }
    });

    let google = Stub::start();
    let h = Harness::start_with_github_and_google(
        "stub-client-id",
        &github_base,
        &github_base,
        CLIENT_ID,
        CLIENT_SECRET,
        &google.base,
        &google.base,
    )
    .await;

    // GitHub first.
    let (_, start) = h
        .post("/api/auth/github/start", None, b"{}".to_vec(), None)
        .await;
    let device = start["deviceCode"].as_str().expect("device").to_owned();
    let (_, body) = h
        .post(
            "/api/auth/github/poll",
            None,
            serde_json::to_vec(&json!({ "deviceCode": device })).expect("encode"),
            None,
        )
        .await;
    assert_eq!(body["status"], "ok", "{body}");
    let github_session = body["session"].as_str().expect("session").to_owned();

    // Then Google with the same verified email: same account, new identity.
    // Real sign-ins are moments apart; the profile follows the latest one.
    h.clock.advance_ms(60_000);
    let (flow_id, state, _) = start_flow(&h).await;
    let (status, _) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(body["email"], "guser@example.com");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 1);
    assert_eq!(h.count("SELECT COUNT(*) FROM account_identities"), 2);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM tenants WHERE id <> 'tenant_personal'"),
        1
    );

    // The profile now follows the most recently used identity.
    let (_, me) = h.get("/api/auth/me", Some(&github_session)).await;
    assert_eq!(me["provider"], "google");
    assert_eq!(me["displayName"], "G User");
}

#[tokio::test]
async fn the_client_secret_never_leaves_the_server() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, state, auth_url) = start_flow(&h).await;
    assert!(!auth_url.contains(CLIENT_SECRET));
    let (_, page) = callback(&h, &format!("code=stub-auth-code&state={state}")).await;
    assert!(!page.contains(CLIENT_SECRET));
    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "ok");
    assert!(!body.to_string().contains(CLIENT_SECRET));
    let session = body["session"].as_str().expect("session").to_owned();
    let (_, me) = h.get("/api/auth/me", Some(&session)).await;
    assert!(!me.to_string().contains(CLIENT_SECRET));
    // Nothing in the identity rows carries the secret either.
    let db = h.side_db();
    let mut statement = db
        .prepare(
            "SELECT provider, subject, email, display_name, avatar_url FROM account_identities",
        )
        .expect("prepare");
    let rows = statement
        .query_map([], |row| {
            Ok(format!(
                "{} {} {} {} {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    for row in rows {
        assert!(!row.contains(CLIENT_SECRET), "{row}");
    }
}

#[tokio::test]
async fn google_callback_sets_security_headers_rejects_replay_and_caps_per_client() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;
    let (flow_id, state, _) = start_flow(&h).await;

    let resp = h
        .http
        .get(format!(
            "{}/api/auth/google/callback?code=stub-auth-code&state={state}",
            h.base
        ))
        .send()
        .await
        .expect("callback");
    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers();
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; style-src 'unsafe-inline'"
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-frame-options"], "DENY");

    // Replaying the same callback state must fail and not re-exchange with Google.
    let replay = h
        .http
        .get(format!(
            "{}/api/auth/google/callback?code=stub-auth-code&state={state}",
            h.base
        ))
        .send()
        .await
        .expect("replay");
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(stub.verifiers.lock().expect("lock").len(), 1);

    let (_, body) = poll(&h, &flow_id).await;
    assert_eq!(body["status"], "ok", "{body}");

    // Per-IP active flow limit caps concurrent flows before AUTH_REQUESTS_PER_WINDOW.
    for _ in 0..8 {
        let started = h
            .http
            .post(format!("{}/api/auth/google/start", h.base))
            .header("x-forwarded-for", "203.0.113.77")
            .json(&json!({}))
            .send()
            .await
            .expect("start");
        assert_eq!(started.status(), StatusCode::OK);
    }
    let blocked = h
        .http
        .post(format!("{}/api/auth/google/start", h.base))
        .header("x-forwarded-for", "203.0.113.77")
        .json(&json!({}))
        .send()
        .await
        .expect("blocked start");
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);

    let other_client = h
        .http
        .post(format!("{}/api/auth/google/start", h.base))
        .header("x-forwarded-for", "198.51.100.77")
        .json(&json!({}))
        .send()
        .await
        .expect("other client start");
    assert_eq!(other_client.status(), StatusCode::OK);

    // Poll rate limit is independent per IP.
    for _ in 0..120 {
        let resp = h
            .http
            .post(format!("{}/api/auth/google/poll", h.base))
            .header("x-forwarded-for", "203.0.113.99")
            .json(&json!({ "flowId": "" }))
            .send()
            .await
            .expect("poll");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
    let limited_poll = h
        .http
        .post(format!("{}/api/auth/google/poll", h.base))
        .header("x-forwarded-for", "203.0.113.99")
        .json(&json!({ "flowId": "" }))
        .send()
        .await
        .expect("limited poll");
    assert_eq!(limited_poll.status(), StatusCode::TOO_MANY_REQUESTS);
}
