//! Public product and privacy pages. These files are compiled into the server
//! and served only from explicit GET routes: no directory traversal, fallback
//! to the dashboard, account cookies, JavaScript, or third-party resources.
//! The home page uses standard browser platform hints only to put the matching
//! installer first and emphasize it; both download choices remain available.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::http::AppState;
use crate::site_stats::{self, DailyCounts, DayRow};

const INDEX: &str = include_str!("../public/index.html");
const PRIVACY: &str = include_str!("../public/privacy.html");
const STYLES: &str = include_str!("../public/site.css");
const LOGO: &str = include_str!("../public/assets/tokenlens.svg");
const OVERVIEW: &[u8] = include_bytes!("../public/assets/overview.webp");
const TRENDS: &[u8] = include_bytes!("../public/assets/trends.webp");
const SESSION_DETAIL: &[u8] = include_bytes!("../public/assets/session-detail.webp");
const DEVICES: &[u8] = include_bytes!("../public/assets/devices.webp");
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

async fn render_index(headers: HeaderMap) -> Response {
    let macos = requested_platform(&headers) == Some(DesktopPlatform::Macos);
    let (hero_downloads, bottom_downloads) = if macos {
        (
            [
                download_button(DesktopPlatform::Macos, true, true),
                download_button(DesktopPlatform::Windows, false, true),
            ]
            .join("\n            "),
            [
                download_button(DesktopPlatform::Macos, true, false),
                download_button(DesktopPlatform::Windows, false, false),
            ]
            .join("\n            "),
        )
    } else {
        (
            [
                download_button(DesktopPlatform::Windows, true, true),
                download_button(DesktopPlatform::Macos, false, true),
            ]
            .join("\n            "),
            [
                download_button(DesktopPlatform::Windows, true, false),
                download_button(DesktopPlatform::Macos, false, false),
            ]
            .join("\n            "),
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

pub async fn index(State(state): State<AppState>, method: Method, headers: HeaderMap) -> Response {
    let should_count = method == Method::GET && site_stats::should_count_request(&headers);
    let response = render_index(headers).await;
    if should_count && response.status() == StatusCode::OK {
        state.record_site_metric(site_stats::HOME_VIEW);
    }
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

async fn render_privacy() -> Response {
    page(PRIVACY, "text/html; charset=utf-8", "public, max-age=300")
}

pub async fn privacy(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let should_count = method == Method::GET && site_stats::should_count_request(&headers);
    let response = render_privacy().await;
    if should_count && response.status() == StatusCode::OK {
        state.record_site_metric(site_stats::PRIVACY_VIEW);
    }
    response
}

pub async fn site_stats_page(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let Some(expected) = state.stats_token() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !token_matches(expected, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(rows) = state.site_stats_report(30).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "统计暂时不可用").into_response();
    };
    let content = render_stats_page(&rows, &state.local_time_label());
    let mut response = page(&content, "text/html; charset=utf-8", "no-store");
    let headers = response.headers_mut();
    headers.insert(
        "x-robots-tag",
        header::HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}

fn token_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.len() != provided.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(provided) {
        difference |= left ^ right;
    }
    difference == 0
}

fn render_stats_page(rows: &[DayRow], updated: &str) -> String {
    let today = rows
        .last()
        .map_or_else(DailyCounts::default, |row| row.counts);
    let week = rows
        .iter()
        .rev()
        .take(7)
        .fold(DailyCounts::default(), |total, row| total + row.counts);
    let month = rows
        .iter()
        .fold(DailyCounts::default(), |total, row| total + row.counts);

    let mut table_rows = String::new();
    for row in rows {
        table_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            row.day,
            row.counts.home_views,
            row.counts.download_clicks_macos,
            row.counts.download_clicks_windows,
            row.counts.downloads_macos,
            row.counts.downloads_windows,
        ));
    }

    format!(
        r##"<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="robots" content="noindex,nofollow">
    <meta name="theme-color" content="#f8f6f1">
    <link rel="icon" type="image/svg+xml" href="/assets/tokenlens.svg">
    <link rel="stylesheet" href="/assets/site.css">
    <title>官网统计 · TokenLens</title>
  </head>
  <body>
    <header class="site-header">
      <div class="container header-inner">
        <a class="brand" href="/" aria-label="TokenLens 首页">
          <img src="/assets/tokenlens.svg" alt="" width="36" height="36">
          <span>TokenLens</span>
        </a>
        <nav class="site-nav" aria-label="主导航">
          <a href="/">产品</a>
          <a href="/privacy">隐私政策</a>
        </nav>
      </div>
    </header>
    <main class="policy-main">
      <article class="container policy stats">
        <header class="policy-header">
          <p class="eyebrow">TokenLens · 仅管理员</p>
          <h1>官网访问统计</h1>
          <p class="policy-updated">更新至 {updated}（北京时间）· 只累计次数，不识别访客</p>
          <p class="policy-lead">这里只记录每天官网被打开和安装包被下载的次数，不保存 IP、浏览器标识或 Cookie，无法知道具体是谁。发布验收、自动检查和镜像同步也可能计入，所以这些数字不是“人数”。</p>
        </header>
        <div class="stats-summary">
          <div><small>今日首页打开</small><strong>{today_home}</strong></div>
          <div><small>近 7 天首页打开</small><strong>{week_home}</strong></div>
          <div><small>近 7 天下载按钮点击</small><strong>{week_clicks}</strong></div>
          <div><small>近 7 天安装包下载</small><strong>{week_downloads}</strong></div>
        </div>
        <h2>最近 30 天</h2>
        <div class="stats-table-wrap">
          <table class="stats-table">
            <thead><tr><th>日期</th><th>首页打开</th><th>Mac 按钮</th><th>Windows 按钮</th><th>Mac 安装包</th><th>Windows 安装包</th></tr></thead>
            <tbody>{table_rows}</tbody>
          </table>
        </div>
        <p class="policy-note">统计从 2026 年 11 月 19 日开始；在此之前官网没有记录访问数据，无法补回。近 30 天合计：首页打开 {month_home} 次，下载按钮点击 {month_clicks} 次，安装包下载 {month_downloads} 次。</p>
      </article>
    </main>
    <footer class="site-footer">
      <div class="container footer-inner"><span>TokenLens</span><span>仅管理员页面 · 不对外链接</span></div>
    </footer>
  </body>
</html>
"##,
        today_home = today.home_views,
        week_home = week.home_views,
        week_clicks = week.download_clicks(),
        week_downloads = week.downloads(),
        table_rows = table_rows,
        month_home = month.home_views,
        month_clicks = month.download_clicks(),
        month_downloads = month.downloads(),
    )
}

pub async fn styles() -> Response {
    page(STYLES, "text/css; charset=utf-8", "public, max-age=300")
}

pub async fn logo() -> Response {
    page(LOGO, "image/svg+xml", "public, max-age=86400")
}

fn image(content: &'static [u8]) -> Response {
    let mut response = page("", "image/webp", "public, max-age=86400");
    *response.body_mut() = content.into();
    response
}

pub async fn overview() -> Response {
    image(OVERVIEW)
}

pub async fn trends() -> Response {
    image(TRENDS)
}

pub async fn session_detail() -> Response {
    image(SESSION_DETAIL)
}

pub async fn devices() -> Response {
    image(DEVICES)
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
            let response = render_index(request_headers).await;
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
        assert!(INDEX.contains("三种视觉风格"));
        assert!(INDEX.contains("星期 × 小时"));
        assert!(INDEX.contains("不是网页版看板"));
        assert!(INDEX.contains("云服务 · 目前免费"));
        assert!(INDEX.contains("不代表你的实际用量或账单"));
        for asset in ["overview", "trends", "session-detail", "devices"] {
            assert!(INDEX.contains(&format!("/assets/{asset}.webp")));
        }
        assert!(INDEX.contains("对话内容不上传"));
        assert!(INDEX.contains("首次打开遇到系统安全提示怎么办"));
        assert!(INDEX.contains("xattr -cr /Applications/TokenLens.app"));
        assert!(INDEX.contains("SmartScreen"));
        assert!(PRIVACY.contains("不保存你的 Google 密码或长期 Google 授权令牌"));
        assert!(PRIVACY.contains("退出账号</strong>会撤销当前登录状态，并断开这台电脑的云端连接"));
        assert!(PRIVACY.contains("也不会删除云端设备或历史"));
        assert!(PRIVACY.contains("Google 和 GitHub 不取得你的 TokenLens 会话"));
        assert!(PRIVACY.contains("项目统计只保留项目名称和工作区标识的不可逆哈希"));
        assert!(PRIVACY.contains("只按天累加首页打开次数和各平台下载次数"));
        assert!(PRIVACY.contains("不保存 IP 地址、浏览器标识、来源页面或 Cookie"));
        assert!(PRIVACY.contains("没有「一键删除账号及全部云端历史」入口"));
        for page in [INDEX, PRIVACY] {
            assert!(!page.contains("<script"));
            assert!(!page.contains("googletagmanager"));
            assert!(!page.contains("google-analytics"));
        }
    }
}
