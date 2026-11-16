//! Public pages must be readable without an account and isolated from private
//! APIs. They must not become a catch-all for assets or a source of tracking.

mod common;

use common::Harness;
use reqwest::{Method, StatusCode, header, redirect::Policy};

#[tokio::test]
async fn pages_and_their_assets_are_public_static_and_csp_restricted() {
    let h = Harness::start().await;
    for (path, content_type, expected) in [
        ("/", "text/html; charset=utf-8", "看清 Token"),
        ("/privacy", "text/html; charset=utf-8", "隐私政策"),
        ("/assets/site.css", "text/css; charset=utf-8", "--primary"),
        ("/assets/tokenlens.svg", "image/svg+xml", "<svg"),
    ] {
        let response = h
            .http
            .get(format!("{}{path}", h.base))
            .send()
            .await
            .expect("public page");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(content_type),
            "{path}"
        );
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .expect("CSP");
        assert!(csp.contains("default-src 'none'"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            response
                .headers()
                .get(header::REFERRER_POLICY)
                .and_then(|value| value.to_str().ok()),
            Some("strict-origin-when-cross-origin")
        );
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        let body = response.text().await.expect("body");
        assert!(body.contains(expected), "{path}");
        assert!(!body.contains("tls_"), "{path}");
    }
}

#[tokio::test]
async fn public_routes_do_not_expose_files_or_relax_private_api_authentication() {
    let h = Harness::start().await;
    // Disable redirects so normalized dot-segments cannot turn into a false
    // pass by landing on a valid route after the server already accepted them.
    let no_redirect = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("client");
    for path in [
        "/index.html",
        "/assets/../public/privacy.html",
        "/assets/not-found",
        "/privacy/../data/tokenlens.sqlite",
    ] {
        let response = no_redirect
            .get(format!("{}{path}", h.base))
            .send()
            .await
            .expect("unknown path");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    for path in ["/", "/privacy", "/assets/site.css"] {
        let response = h
            .http
            .request(Method::POST, format!("{}{path}", h.base))
            .send()
            .await
            .expect("POST public page");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
    }
    let (status, _) = h.get("/api/auth/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = h.get("/api/dashboard/summary", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let health = h
        .http
        .get(format!("{}/healthz", h.base))
        .send()
        .await
        .expect("health");
    assert_eq!(health.status(), StatusCode::OK);
}
