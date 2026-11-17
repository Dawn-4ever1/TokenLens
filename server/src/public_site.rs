//! Public product and privacy pages. These files are compiled into the server
//! and served only from explicit GET routes: no directory traversal, fallback
//! to the dashboard, account cookies, JavaScript, or third-party resources.
//! The home page uses standard browser platform hints only to put the matching
//! installer first and emphasize it; both download choices remain available.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

const INDEX: &str = include_str!("../public/index.html");
const PRIVACY: &str = include_str!("../public/privacy.html");
const STYLES: &str = include_str!("../public/site.css");
const LOGO: &str = include_str!("../public/assets/tokenlens.svg");
const CSP: &str = "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; img-src 'self'; style-src 'self'";

fn page(content: &str, content_type: &'static str, cache: &'static str) -> Response {
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        content.to_owned(),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

pub async fn index(headers: HeaderMap) -> Response {
    let macos = requested_platform(&headers) == Some(DesktopPlatform::Macos);
    let (hero_downloads, bottom_downloads) = if macos {
        (
            [
                download_button(DesktopPlatform::Macos, true, true),
                download_button(DesktopPlatform::Windows, false, true),
            ]
            .join("\n              "),
            [
                download_button(DesktopPlatform::Macos, true, false),
                download_button(DesktopPlatform::Windows, false, false),
            ]
            .join("\n              "),
        )
    } else {
        (
            [
                download_button(DesktopPlatform::Windows, true, true),
                download_button(DesktopPlatform::Macos, false, true),
            ]
            .join("\n              "),
            [
                download_button(DesktopPlatform::Windows, true, false),
                download_button(DesktopPlatform::Macos, false, false),
            ]
            .join("\n              "),
        )
    };
    let content = INDEX
        .replace("{{HERO_DOWNLOAD_BUTTONS}}", &hero_downloads)
        .replace("{{BOTTOM_DOWNLOAD_BUTTONS}}", &bottom_downloads);
    debug_assert!(!content.contains("{{"), "unrendered download placeholder");
    let mut response = page(&content, "text/html; charset=utf-8", "public, max-age=300");
    // The default Windows recommendation is also the conservative no-script
    // fallback. Caches must not serve that variant to recognized Mac browsers.
    response.headers_mut().insert(
        header::VARY,
        header::HeaderValue::from_static("User-Agent, Sec-CH-UA-Platform"),
    );
    response
}

fn download_button(platform: DesktopPlatform, primary: bool, hero: bool) -> String {
    let (platform_name, label, icon) = match platform {
        DesktopPlatform::Macos => ("macos", "下载 Mac 版 · Apple 芯片", false),
        DesktopPlatform::Windows => ("windows", "下载 Windows 版", true),
    };
    let primary_class = if primary { " button-primary" } else { "" };
    let icon = if hero && icon {
        "\n                <svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.7\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"M12 3v12m-5-5 5 5 5-5M4 16v4h16v-4\"/></svg>\n                "
    } else {
        ""
    };
    format!(
        "<a class=\"button{primary_class}\" data-download-platform=\"{platform_name}\" href=\"/downloads/latest/{platform_name}\">{icon}{label}</a>"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopPlatform {
    Windows,
    Macos,
}

fn requested_platform(headers: &HeaderMap) -> Option<DesktopPlatform> {
    // This optional request header is sent by some browsers. The traditional
    // user-agent below covers Safari and browsers that do not send the hint.
    let platform_hint = headers
        .get("sec-ch-ua-platform")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().trim_matches('"').to_ascii_lowercase());
    match platform_hint.as_deref() {
        Some("windows") => return Some(DesktopPlatform::Windows),
        Some("macos") => return Some(DesktopPlatform::Macos),
        // A mobile or other unsupported OS is not a desktop Mac, even if its
        // user-agent happens to contain that word.
        Some("android" | "ios" | "linux" | "chrome os") => return None,
        _ => {}
    }
    let user_agent = headers
        .get(header::USER_AGENT)?
        .to_str()
        .ok()?
        .to_ascii_lowercase();
    if user_agent.contains("iphone")
        || user_agent.contains("ipad")
        || user_agent.contains("android")
    {
        None
    } else if user_agent.contains("windows nt") {
        Some(DesktopPlatform::Windows)
    } else if user_agent.contains("macintosh") || user_agent.contains("mac os x") {
        Some(DesktopPlatform::Macos)
    } else {
        None
    }
}

pub async fn privacy() -> Response {
    page(PRIVACY, "text/html; charset=utf-8", "public, max-age=300")
}

pub async fn styles() -> Response {
    page(STYLES, "text/css; charset=utf-8", "public, max-age=300")
}

pub async fn logo() -> Response {
    page(LOGO, "image/svg+xml", "public, max-age=86400")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::HeaderValue;

    const WINDOWS_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";
    const MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.1 Safari/605.1.15";

    fn headers(user_agent: Option<&str>, platform_hint: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = user_agent {
            headers.insert(
                header::USER_AGENT,
                HeaderValue::from_str(value).expect("UA"),
            );
        }
        if let Some(value) = platform_hint {
            headers.insert(
                "sec-ch-ua-platform",
                HeaderValue::from_str(value).expect("hint"),
            );
        }
        headers
    }

    #[test]
    fn desktop_download_recommendation_uses_browser_platform_hints() {
        for (user_agent, expected) in [
            (WINDOWS_UA, Some(DesktopPlatform::Windows)),
            (MAC_UA, Some(DesktopPlatform::Macos)),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148",
                None,
            ),
            (
                "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 Chrome/131.0.0.0 Mobile Safari/537.36",
                None,
            ),
            ("curl/8.7.1", None),
        ] {
            assert_eq!(
                requested_platform(&headers(Some(user_agent), None)),
                expected
            );
        }
        assert_eq!(
            requested_platform(&headers(Some(MAC_UA), Some("\"Windows\""))),
            Some(DesktopPlatform::Windows)
        );
        assert_eq!(
            requested_platform(&headers(Some(WINDOWS_UA), Some("\"macOS\""))),
            Some(DesktopPlatform::Macos)
        );
        assert_eq!(
            requested_platform(&headers(Some(MAC_UA), Some("\"Android\""))),
            None
        );
    }

    #[tokio::test]
    async fn download_priority_is_rendered_before_the_page_reaches_the_browser() {
        for (request_headers, first, second) in [
            (headers(Some(MAC_UA), None), "macos", "windows"),
            (headers(Some(WINDOWS_UA), None), "windows", "macos"),
            (headers(None, None), "windows", "macos"),
        ] {
            let response = index(request_headers).await;
            assert_eq!(
                response.headers().get(header::VARY).expect("Vary"),
                "User-Agent, Sec-CH-UA-Platform"
            );
            let body = to_bytes(response.into_body(), 100_000).await.expect("body");
            let body = std::str::from_utf8(&body).expect("HTML");
            let first = format!("data-download-platform=\"{first}\"");
            let second = format!("data-download-platform=\"{second}\"");
            let mut positions = body.match_indices(&first);
            let (first_position, _) = positions.next().expect("first platform");
            let (next_position, _) = positions.next().expect("second section");
            let second_position = body.find(&second).expect("other platform");
            assert!(first_position < second_position, "first section");
            assert!(
                next_position < body.rfind(&second).expect("other platform"),
                "second section"
            );
            assert!(
                body.matches("class=\"button button-primary\" data-download-platform")
                    .count()
                    == 2
            );
            assert!(body.contains("/downloads/latest/windows"));
            assert!(body.contains("/downloads/latest/macos"));
            assert!(!body.contains("{{"));
            assert!(!body.contains("<script"));
        }
    }

    #[test]
    fn policy_matches_the_current_collection_boundaries() {
        assert!(INDEX.contains("界面示意，数据为虚构"));
        assert!(INDEX.contains("无需先注册"));
        assert!(PRIVACY.contains("不保存你的 Google 密码或长期 Google 授权令牌"));
        assert!(PRIVACY.contains("退出账号</strong>只撤销当前登录状态"));
        assert!(PRIVACY.contains("Google 和 GitHub 不取得你的 TokenLens 会话"));
        assert!(PRIVACY.contains("当前采集实现不会生成或上传项目目录的哈希或项目标签"));
        assert!(PRIVACY.contains("没有「一键删除账号及全部云端历史」入口"));
        for page in [INDEX, PRIVACY] {
            assert!(!page.contains("<script"));
            assert!(!page.contains("googletagmanager"));
            assert!(!page.contains("google-analytics"));
        }
    }
}
