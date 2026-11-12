//! HTTP transport (blocking, small) and server URL rules.

use std::io::Write;
use std::time::Duration;

/// Responses larger than this are treated as failures.
pub const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: Vec<u8>,
}

/// No URL or response text is kept: the server URL is configuration and
/// error bodies could echo request details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("the server could not be reached")]
    Network,
    #[error("the server response could not be read")]
    Body,
}

pub trait Transport: Send {
    fn post(
        &self,
        url: &str,
        bearer: Option<&str>,
        gzip: bool,
        body: &[u8],
    ) -> Result<HttpResponse, TransportError>;
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A redirect could carry the bearer token to another host.
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(60)))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(if cfg!(any(target_os = "windows", target_os = "macos")) {
                        ureq::tls::TlsProvider::NativeTls
                    } else {
                        ureq::tls::TlsProvider::Rustls
                    })
                    .build(),
            )
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

/// Dashboard replies (session lists, coverage matrices) can be larger than
/// ingest replies.
pub const MAX_GET_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

impl UreqTransport {
    pub fn get(&self, url: &str, bearer: Option<&str>) -> Result<HttpResponse, TransportError> {
        let mut request = self.agent.get(url).header("Accept", "application/json");
        if let Some(token) = bearer {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = request.call().map_err(|_| TransportError::Network)?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_GET_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|_| TransportError::Body)?;
        Ok(HttpResponse {
            status,
            retry_after: None,
            body,
        })
    }
}

impl Transport for UreqTransport {
    fn post(
        &self,
        url: &str,
        bearer: Option<&str>,
        gzip: bool,
        body: &[u8],
    ) -> Result<HttpResponse, TransportError> {
        let mut request = self
            .agent
            .post(url)
            .header("Content-Type", "application/json");
        if gzip {
            request = request.header("Content-Encoding", "gzip");
        }
        if let Some(token) = bearer {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = request.send(body).map_err(|_| TransportError::Network)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|_| TransportError::Body)?;
        Ok(HttpResponse {
            status,
            retry_after,
            body,
        })
    }
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to a Vec cannot fail.
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error(
        "the server address must start with https:// (http:// is only allowed for this computer)"
    )]
    Scheme,
    #[error("the server address must not contain a user, query or fragment")]
    Shape,
}

/// `https://host[:port][/prefix]`, or plain http for a loopback host only.
/// Returns the address without a trailing `/`.
pub fn validate_server_url(input: &str) -> Result<String, UrlError> {
    let (scheme, rest) = input.trim().split_once("://").ok_or(UrlError::Scheme)?;
    let rest = rest.trim_end_matches('/');
    if rest.is_empty()
        || rest
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '@' | '?' | '#' | '\\'))
    {
        return Err(UrlError::Shape);
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().map(|host| format!("[{host}]"))
    } else {
        authority.split(':').next().map(str::to_owned)
    }
    .filter(|host| !host.is_empty())
    .ok_or(UrlError::Shape)?;
    match scheme {
        "https" => {}
        "http" if matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") => {}
        _ => return Err(UrlError::Scheme),
    }
    Ok(format!("{scheme}://{rest}"))
}

/// Parses `{"error": "<code>"}` and keeps the code only if it looks like one.
pub fn error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let code = value.get("error")?.as_str()?;
    (!code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| code.to_owned())
}

/// `true` when `ours` is a lower `major.minor.patch` than `minimum`.
/// Unparseable versions never block.
pub fn version_below(ours: &str, minimum: &str) -> bool {
    let parse = |text: &str| -> Option<Vec<u64>> {
        let core = text.trim().split(['-', '+']).next()?;
        core.split('.').map(|part| part.parse().ok()).collect()
    };
    match (parse(ours), parse(minimum)) {
        (Some(ours), Some(minimum)) => ours < minimum,
        _ => false,
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    pub struct Sent {
        pub url: String,
        pub bearer: Option<String>,
        pub gzip: bool,
        pub body: Vec<u8>,
    }

    /// Replies from a script; records what was sent.
    #[derive(Clone, Default)]
    pub struct FakeTransport {
        pub replies: Arc<Mutex<VecDeque<Result<HttpResponse, TransportError>>>>,
        pub sent: Arc<Mutex<Vec<Sent>>>,
    }

    impl FakeTransport {
        pub fn push(&self, status: u16, body: serde_json::Value) {
            self.replies
                .lock()
                .expect("lock")
                .push_back(Ok(HttpResponse {
                    status,
                    retry_after: None,
                    body: serde_json::to_vec(&body).expect("json"),
                }));
        }

        pub fn push_raw(&self, reply: Result<HttpResponse, TransportError>) {
            self.replies.lock().expect("lock").push_back(reply);
        }

        pub fn sent(&self) -> Vec<Sent> {
            self.sent.lock().expect("lock").clone()
        }
    }

    impl Transport for FakeTransport {
        fn post(
            &self,
            url: &str,
            bearer: Option<&str>,
            gzip: bool,
            body: &[u8],
        ) -> Result<HttpResponse, TransportError> {
            self.sent.lock().expect("lock").push(Sent {
                url: url.to_owned(),
                bearer: bearer.map(str::to_owned),
                gzip,
                body: body.to_vec(),
            });
            self.replies
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or(Err(TransportError::Network))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls_are_https_or_loopback_http() {
        assert_eq!(
            validate_server_url(" https://dash.example.com/ "),
            Ok("https://dash.example.com".to_owned())
        );
        assert!(validate_server_url("https://dash.example.com:8443/prefix").is_ok());
        for ok in [
            "http://127.0.0.1:8787",
            "http://localhost:8787/",
            "http://[::1]:8787",
        ] {
            assert!(validate_server_url(ok).is_ok(), "{ok}");
        }
        for (bad, error) in [
            ("http://dash.example.com", UrlError::Scheme),
            ("http://127.0.0.1.example.com", UrlError::Scheme),
            ("ftp://dash.example.com", UrlError::Scheme),
            ("dash.example.com", UrlError::Scheme),
            ("https://user@dash.example.com", UrlError::Shape),
            ("https://dash.example.com/?token=x", UrlError::Shape),
            ("https://dash.example.com/#x", UrlError::Shape),
            ("https://", UrlError::Shape),
            ("https://dash example.com", UrlError::Shape),
        ] {
            assert_eq!(validate_server_url(bad), Err(error), "{bad}");
        }
    }

    #[test]
    fn error_codes_and_versions() {
        assert_eq!(
            error_code(br#"{"error":"invalid_pair_code"}"#).as_deref(),
            Some("invalid_pair_code")
        );
        assert_eq!(error_code(br#"{"error":"/some/path"}"#), None);
        assert_eq!(error_code(b"not json"), None);
        assert!(version_below("0.1.0", "0.2.0"));
        assert!(version_below("0.9.9", "0.10.0"));
        assert!(!version_below("0.1.0", "0.1.0"));
        assert!(!version_below("1.0.0-beta", "0.9.0"));
        assert!(!version_below("x", "0.1.0"));
    }

    #[test]
    fn gzip_round_trips() {
        use std::io::Read;
        let packed = gzip(b"hello hello hello");
        let mut out = String::new();
        flate2::read::GzDecoder::new(packed.as_slice())
            .read_to_string(&mut out)
            .expect("gunzip");
        assert_eq!(out, "hello hello hello");
    }
}
