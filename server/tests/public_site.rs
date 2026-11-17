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
        if path == "/" {
            assert!(body.contains(
                "class=\"nav-download\" href=\"https://github.com/Dawn-4ever1/TokenLens\" rel=\"noopener noreferrer\">GitHub <span aria-hidden=\"true\">↗</span></a>"
            ));
            assert!(!body.contains("href=\"#download-title\">下载应用"));
        }
        assert!(body.contains(expected), "{path}");
        if path == "/assets/tokenlens.svg" {
            let current = include_bytes!("../../app/public/favicon.svg");
            assert_eq!(
                body.as_bytes(),
                current,
                "website icon must match the current app artwork"
            );
            assert!(body.contains("fill=\"#1a1714\""));
            assert!(!body.contains("fill=\"#1e2b4a\""));
        }
        if path == "/assets/site.css" {
            assert!(body.contains(".download-section { background: #1a1714; color: #f6f1e7; }"));
        }
        assert!(!body.contains("tls_"), "{path}");
    }
}

#[tokio::test]
async fn the_home_page_puts_the_visitors_desktop_download_first_without_scripts() {
    let h = Harness::start().await;
    for (user_agent, recommended, other) in [
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.1 Safari/605.1.15",
            "macos",
            "windows",
        ),
        (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
            "windows",
            "macos",
        ),
    ] {
        let response = h
            .http
            .get(format!("{}/", h.base))
            .header(header::USER_AGENT, user_agent)
            .send()
            .await
            .expect("home page");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            Some("User-Agent, Sec-CH-UA-Platform")
        );
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        let body = response.text().await.expect("body");
        let primary = |platform: &str| {
            format!("class=\"button button-primary\" data-download-platform=\"{platform}\"")
        };
        assert_eq!(body.matches(&primary(recommended)).count(), 2);
        assert_eq!(body.matches(&primary(other)).count(), 0);
        for section in body.split("class=\"actions download-actions\"").skip(1) {
            let section = section.split("</div>").next().expect("hero downloads");
            let first = format!("data-download-platform=\"{recommended}\"");
            let second = format!("data-download-platform=\"{other}\"");
            assert!(section.find(&first) < section.find(&second), "hero");
        }
        for section in body.split("<div class=\"download-buttons\">").skip(1) {
            let first = format!("data-download-platform=\"{recommended}\"");
            let second = format!("data-download-platform=\"{other}\"");
            if section.contains(&first) && section.contains(&second) {
                assert!(section.find(&first) < section.find(&second), "bottom");
            }
        }
        assert!(body.contains("href=\"/downloads/latest/windows\""));
        assert!(body.contains("href=\"/downloads/latest/macos\""));
        assert!(!body.contains("<script"));
        assert!(!body.contains("{{"));
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
