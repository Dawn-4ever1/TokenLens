//! GitHub device-flow sign-in (D049): a stub GitHub proves the app never sees a
//! client secret, only verified emails join a space, and repeat sign-ins reuse
//! the same space.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use common::Harness;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// A minimal stand-in for GitHub's OAuth and API endpoints.
struct Stub {
    base: String,
    polls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    emails: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let emails = std::sync::Arc::new(std::sync::Mutex::new(vec![json!({
            "email": "octo@example.com",
            "primary": true,
            "verified": true
        })]));
        let counter = std::sync::Arc::clone(&polls);
        let mail = std::sync::Arc::clone(&emails);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                // Drain headers, then the body: closing early would reset the
                // client while it is still writing a POST body.
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
                if path.starts_with("/login/device/code") {
                    // A GitHub App does not take OAuth scopes: it uses the
                    // fine-grained permissions configured in its settings.
                    let payload: Value = serde_json::from_slice(&body_bytes).expect("request json");
                    assert_eq!(payload, json!({ "client_id": "stub-client-id" }));
                }
                let body = if path.starts_with("/login/device/code") {
                    json!({
                        "device_code": "device-secret-1",
                        "user_code": "ABCD-1234",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 1,
                        "expires_in": 900
                    })
                } else if path.starts_with("/login/oauth/access_token") {
                    // First poll is still pending, the second approves.
                    let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n == 0 {
                        json!({ "error": "authorization_pending" })
                    } else {
                        json!({ "access_token": "gho_stub", "token_type": "bearer" })
                    }
                } else if path.starts_with("/user/emails") {
                    Value::Array(mail.lock().expect("lock").clone())
                } else if path.starts_with("/user") {
                    json!({ "id": 4242, "login": "octocat" })
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
            polls,
            emails,
        }
    }
}

async fn harness_with(stub: &Stub) -> Harness {
    Harness::start_with_github("stub-client-id", &stub.base, &stub.base).await
}

#[tokio::test]
async fn device_flow_signs_in_and_reuses_the_same_space() {
    let stub = Stub::start();
    let h = harness_with(&stub).await;

    let (status, start) = h
        .post("/api/auth/github/start", None, b"{}".to_vec(), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let code = start["userCode"].as_str().expect("user code").to_owned();
    assert_eq!(code, "ABCD-1234");
    assert_eq!(start["verificationUri"], "https://github.com/login/device");
    let device = start["deviceCode"]
        .as_str()
        .expect("device code")
        .to_owned();

    let poll = |device: String| {
        let h = &h;
        async move {
            h.post(
                "/api/auth/github/poll",
                None,
                serde_json::to_vec(&json!({ "deviceCode": device })).expect("encode"),
                None,
            )
            .await
        }
    };
    let (status, body) = poll(device.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "pending");

    let (status, body) = poll(device.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["via"], "github");
    let session = body["session"].as_str().expect("session").to_owned();
    assert!(session.starts_with("tls_"));

    let (status, me) = h.get("/api/auth/me", Some(&session)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], "octo@example.com");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM tenants WHERE id <> 'tenant_personal'"),
        1
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM account_identities"), 1);

    // A second sign-in with the same GitHub user is the same account.
    let (_, start) = h
        .post("/api/auth/github/start", None, b"{}".to_vec(), None)
        .await;
    let device = start["deviceCode"].as_str().expect("device").to_owned();
    let _ = poll(device.clone()).await;
    let (_, body) = poll(device).await;
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 1);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM tenants WHERE id <> 'tenant_personal'"),
        1,
        "no second space for the same GitHub identity"
    );
    assert!(stub.polls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn an_unverified_github_email_cannot_create_a_space() {
    let stub = Stub::start();
    *stub.emails.lock().expect("lock") = vec![json!({
        "email": "unverified@example.com",
        "primary": true,
        "verified": false
    })];
    let h = harness_with(&stub).await;
    let (_, start) = h
        .post("/api/auth/github/start", None, b"{}".to_vec(), None)
        .await;
    let device = start["deviceCode"].as_str().expect("device").to_owned();
    let _ = h
        .post(
            "/api/auth/github/poll",
            None,
            serde_json::to_vec(&json!({ "deviceCode": device.clone() })).expect("encode"),
            None,
        )
        .await;
    let (status, body) = h
        .post(
            "/api/auth/github/poll",
            None,
            serde_json::to_vec(&json!({ "deviceCode": device })).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "failed");
    assert_eq!(h.count("SELECT COUNT(*) FROM accounts"), 0);
}

#[tokio::test]
async fn github_sign_in_is_unavailable_until_a_client_id_is_configured() {
    let h = Harness::start().await;
    let (status, body) = h
        .post("/api/auth/github/start", None, b"{}".to_vec(), None)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, body) = h
        .post(
            "/api/auth/github/poll",
            None,
            serde_json::to_vec(&json!({ "deviceCode": "x" })).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}
