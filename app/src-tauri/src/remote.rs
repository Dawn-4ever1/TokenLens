//! Requests to this project's server. Tokens are added here and never leave
//! the Rust side; only response bodies (numbers, labels) reach the UI.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use collector_core::credentials::Credentials;
use collector_core::transport::{HttpResponse, Transport, UreqTransport};
use serde::Serialize;
use serde_json::Value;

use crate::local::{self, LocalServer, LocalTransport};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandError {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

impl CommandError {
    pub const fn new(kind: &'static str) -> Self {
        Self { kind, status: None }
    }

    pub fn from_status(status: u16) -> Self {
        let kind = match status {
            400 | 413 | 422 => "bad_request",
            401 => "unauthorized",
            403 => "forbidden",
            404 => "not_found",
            429 => "rate_limited",
            _ => "server",
        };
        Self {
            kind,
            status: Some(status),
        }
    }
}

pub type CmdResult<T> = Result<T, CommandError>;

const READ_PATHS: [&str; 7] = [
    "summary",
    "timeseries",
    "heatmap",
    "sessions",
    "devices",
    "filters",
    "health",
];
const MAX_QUERY_PARAMS: usize = 16;
const MAX_QUERY_VALUE: usize = 4096;

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// `dashboard/<path>?<query>` for the read endpoints only; anything else
/// (device actions, pairing, ingest, `..`) is refused before a request.
pub fn dashboard_path(path: &str, query: &BTreeMap<String, String>) -> CmdResult<String> {
    let allowed = READ_PATHS.contains(&path)
        || path.strip_prefix("sessions/").is_some_and(|key| {
            !key.is_empty() && key.len() <= 1024 && key.bytes().all(is_token_char)
        });
    if !allowed {
        return Err(CommandError::new("not_found"));
    }
    if query.len() > MAX_QUERY_PARAMS {
        return Err(CommandError::new("bad_request"));
    }
    let mut url = format!("dashboard/{path}");
    for (index, (key, value)) in query.iter().enumerate() {
        let key_ok =
            !key.is_empty() && key.len() <= 32 && key.bytes().all(|b| b.is_ascii_alphanumeric());
        if !key_ok || value.len() > MAX_QUERY_VALUE {
            return Err(CommandError::new("bad_request"));
        }
        url.push(if index == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    Ok(url)
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Server device ids look like `dev_<base64url>`.
pub fn valid_device_id(id: &str) -> bool {
    id.starts_with("dev_") && id.len() <= 128 && id.bytes().all(is_token_char)
}

fn url_for(credentials: &Credentials, path: &str) -> String {
    format!(
        "{}/api/{path}",
        credentials.server_url.trim_end_matches('/')
    )
}

fn json_body(response: HttpResponse) -> CmdResult<Value> {
    if !(200..300).contains(&response.status) {
        return Err(CommandError::from_status(response.status));
    }
    serde_json::from_slice(&response.body).map_err(|_| CommandError::new("server"))
}

/// Requests go to the paired server, or in local mode (D044) to the
/// server running inside this app; callers do not need to know which.
pub struct Remote {
    http: UreqTransport,
    local: Mutex<Option<Arc<LocalServer>>>,
}

impl Remote {
    pub fn new() -> Self {
        Self {
            http: UreqTransport::new(),
            local: Mutex::new(None),
        }
    }

    pub fn transport(&self) -> &UreqTransport {
        &self.http
    }

    pub fn local(&self) -> Option<Arc<LocalServer>> {
        self.local
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_local(&self, server: Option<Arc<LocalServer>>) {
        *self.local.lock().unwrap_or_else(PoisonError::into_inner) = server;
    }

    fn local_for(&self, credentials: &Credentials) -> CmdResult<Option<LocalTransport>> {
        if !local::is_local(credentials) {
            return Ok(None);
        }
        self.local()
            .map(|server| Some(LocalTransport(server)))
            .ok_or(CommandError::new("server"))
    }

    pub fn get(&self, credentials: &Credentials, path: &str) -> CmdResult<Value> {
        let url = url_for(credentials, path);
        let token = Some(credentials.read_token.as_str());
        let response = match self.local_for(credentials)? {
            Some(local) => local.get(&url, token),
            None => self.http.get(&url, token),
        }
        .map_err(|_| CommandError::new("network"))?;
        json_body(response)
    }

    pub fn post(&self, credentials: &Credentials, path: &str, body: &Value) -> CmdResult<Value> {
        let bytes = serde_json::to_vec(body).map_err(|_| CommandError::new("bad_request"))?;
        let url = url_for(credentials, path);
        let token = Some(credentials.read_token.as_str());
        let response = match self.local_for(credentials)? {
            Some(local) => local.post(&url, token, false, &bytes),
            None => self.http.post(&url, token, false, &bytes),
        }
        .map_err(|_| CommandError::new("network"))?;
        json_body(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn read_paths_are_allowed_and_encoded() {
        assert_eq!(
            dashboard_path("summary", &query(&[])).unwrap(),
            "dashboard/summary"
        );
        assert_eq!(
            dashboard_path(
                "timeseries",
                &query(&[
                    ("from", "2026-11-01"),
                    ("clients", "codex,qoder"),
                    ("q", "a b/中")
                ])
            )
            .unwrap(),
            "dashboard/timeseries?clients=codex%2Cqoder&from=2026-11-01&q=a%20b%2F%E4%B8%AD"
        );
        assert_eq!(
            dashboard_path("sessions/Y29kZXg6YWJj_-", &query(&[])).unwrap(),
            "dashboard/sessions/Y29kZXg6YWJj_-"
        );
    }

    #[test]
    fn other_paths_are_refused() {
        for path in [
            "",
            "pair/codes",
            "../pair/codes",
            "sessions/",
            "sessions/a/b",
            "sessions/..",
            "sessions/a%2Fb",
            "devices/dev_x/revoke",
            "devices/dev_x/rotate",
            "health?x=1",
            "Summary",
        ] {
            assert_eq!(
                dashboard_path(path, &query(&[])),
                Err(CommandError::new("not_found")),
                "{path}"
            );
        }
    }

    #[test]
    fn bad_query_keys_are_refused() {
        for key in ["", "a&b", "a=b", "from ", "x".repeat(33).as_str()] {
            assert_eq!(
                dashboard_path("summary", &query(&[(key, "1")])),
                Err(CommandError::new("bad_request")),
                "{key}"
            );
        }
        let long = "x".repeat(MAX_QUERY_VALUE + 1);
        assert!(dashboard_path("summary", &query(&[("q", &long)])).is_err());
    }

    #[test]
    fn device_ids_are_checked() {
        assert!(valid_device_id("dev_AbC-12_x"));
        assert!(!valid_device_id("dev_../x"));
        assert!(!valid_device_id("abc"));
        assert!(!valid_device_id("dev_a/b"));
    }

    #[test]
    fn statuses_map_to_ui_kinds() {
        assert_eq!(CommandError::from_status(401).kind, "unauthorized");
        assert_eq!(CommandError::from_status(403).kind, "forbidden");
        assert_eq!(CommandError::from_status(429).kind, "rate_limited");
        assert_eq!(CommandError::from_status(503).kind, "server");
    }
}
