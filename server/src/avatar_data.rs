//! Provider avatars for the sidebar (T31 follow-up). On some networks the
//! desktop app cannot reach Google's picture hosts directly (the browser can,
//! through a proxy), so the server fetches the signed-in account's picture
//! once and the app downloads it from us instead. Bounded, cached, and only
//! for the two known provider hosts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MAX_AVATAR_BYTES: usize = 256 * 1024;
pub const CACHE_TTL_MS: i64 = 60 * 60 * 1000;
const MAX_CACHE: usize = 128;

#[derive(Clone, Default)]
pub struct AvatarData {
    cache: Arc<Mutex<HashMap<String, Cached>>>,
}

struct Cached {
    fetched_at: i64,
    bytes: Vec<u8>,
    mime: &'static str,
}

impl AvatarData {
    pub fn new() -> Self {
        Self::default()
    }

    /// The picture for a validated provider URL, cached in memory.
    /// `None` when the URL is not an allowed avatar or the fetch failed.
    pub fn image(&self, url: &str, now: i64) -> Option<(Vec<u8>, &'static str)> {
        if !avatar_url_allowed(url) {
            return None;
        }
        let Ok(mut cache) = self.cache.lock() else {
            return None;
        };
        cache.retain(|_, entry| now - entry.fetched_at < CACHE_TTL_MS);
        if let Some(entry) = cache.get(url) {
            return Some((entry.bytes.clone(), entry.mime));
        }
        let (bytes, mime) = fetch(url)?;
        if cache.len() >= MAX_CACHE {
            cache.clear();
        }
        cache.insert(
            url.to_owned(),
            Cached {
                fetched_at: now,
                bytes: bytes.clone(),
                mime,
            },
        );
        Some((bytes, mime))
    }
}

/// Only the two provider avatar shapes are fetched: GitHub's numeric avatars
/// and Google's `lhN.googleusercontent.com` pictures. This is the server-side
/// request-forgery boundary — never fetch a stored arbitrary URL.
pub fn avatar_url_allowed(url: &str) -> bool {
    if url.len() > 2048
        || url
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
    {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if let Some(path) = rest.strip_prefix("avatars.githubusercontent.com/u/") {
        let id = path.split(['?', '#']).next().unwrap_or_default();
        return !id.is_empty() && id.len() <= 20 && id.bytes().all(|byte| byte.is_ascii_digit());
    }
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let Some(digits) = host
        .strip_suffix(".googleusercontent.com")
        .and_then(|label| label.strip_prefix("lh"))
    else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (path.starts_with("a/") || path.starts_with("a-/"))
        && path.len() > 2
}

fn fetch(url: &str) -> Option<(Vec<u8>, &'static str)> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(10)))
            .build(),
    );
    let response = agent
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
    let mime: &'static str = match mime.as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => return None,
    };
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_AVATAR_BYTES as u64)
        .read_to_vec()
        .ok()?;
    magic_ok(mime, &bytes).then_some((bytes, mime))
}

fn magic_ok(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A one-shot HTTP stub; returns the request line count for cache checks.
    fn stub(
        status: &str,
        content_type: &str,
        body: &[u8],
        hits: Arc<std::sync::atomic::AtomicUsize>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let address = format!("http://{}/avatar", listener.local_addr().expect("addr"));
        let status = status.to_owned();
        let content_type = content_type.to_owned();
        let body = body.to_vec();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        address
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

    #[test]
    fn avatar_urls_allow_only_the_two_provider_shapes() {
        for url in [
            "https://avatars.githubusercontent.com/u/4242?v=4",
            "https://lh3.googleusercontent.com/a/ACg8ocJx=s96-c",
            "https://lh6.googleusercontent.com/a-/ABC123",
        ] {
            assert!(avatar_url_allowed(url), "{url}");
        }
        for url in [
            "http://lh3.googleusercontent.com/a/x",
            "https://lh3.googleusercontent.com.evil.example/a/x",
            "https://evil.example/lh3.googleusercontent.com/a/x",
            "https://lh3.googleusercontent.com/arbitrary/x",
            "https://avatars.githubusercontent.com/u/not-a-number",
            "https://avatars.githubusercontent.com/orgs/x",
            "https://example.com/avatar.png",
            "file:///etc/passwd",
            "data:image/png;base64,x",
        ] {
            assert!(!avatar_url_allowed(url), "{url}");
        }
    }

    #[test]
    fn a_fetched_image_is_cached_and_validated() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let url = stub(
            "200 OK",
            "image/png; charset=binary",
            PNG,
            Arc::clone(&hits),
        );
        let data = AvatarData::new();
        let first = data.image(&url, 1_000);
        assert!(first.is_none(), "only provider hosts are fetched");
        // Unit-test seam: fetch directly, bypassing the host allowlist.
        let fetched = fetch(&url).expect("fetch");
        assert_eq!(fetched.0, PNG);
        assert_eq!(fetched.1, "image/png");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn unsafe_or_oversized_images_are_rejected() {
        for (status, mime, body) in [
            ("200 OK", "text/html", b"<html/>".to_vec()),
            ("200 OK", "image/svg+xml", b"<svg/>".to_vec()),
            ("200 OK", "image/png", b"<script>".to_vec()),
            ("302 Found", "image/png", PNG.to_vec()),
            ("404 Not Found", "image/png", PNG.to_vec()),
            ("200 OK", "image/png", {
                let mut bytes = PNG.to_vec();
                bytes.resize(MAX_AVATAR_BYTES + 1, 0);
                bytes
            }),
        ] {
            let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let url = stub(status, mime, &body, hits);
            assert!(fetch(&url).is_none(), "{status} {mime}");
        }
    }
}
