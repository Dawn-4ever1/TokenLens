//! Public product and privacy pages. These files are compiled into the server
//! and served only from explicit GET routes: no directory traversal, fallback
//! to the dashboard, account cookies, JavaScript, or third-party resources.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

const INDEX: &str = include_str!("../public/index.html");
const PRIVACY: &str = include_str!("../public/privacy.html");
const STYLES: &str = include_str!("../public/site.css");
const LOGO: &str = include_str!("../public/assets/tokenlens.svg");
const CSP: &str = "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; img-src 'self'; style-src 'self'";

fn page(content: &'static str, content_type: &'static str, cache: &'static str) -> Response {
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        content,
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

pub async fn index() -> Response {
    page(INDEX, "text/html; charset=utf-8", "public, max-age=300")
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
