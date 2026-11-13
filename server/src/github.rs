//! GitHub device-flow sign-in (RFC 8628). The desktop app never holds a client
//! secret: GitHub returns a short-lived device code and a human code that the
//! user types on github.com. Only verified email addresses are trusted.

use std::time::Duration;

/// One device-flow attempt. `device_code` is a short-lived secret; the app's
/// Rust side keeps it and never passes it to the WebView.
/// GitHub could not be reached or answered something unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubError;

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHub sign-in is not available right now")
    }
}

impl std::error::Error for GitHubError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Pending,
    SlowDown(u64),
    Ready { subject: String, email: String },
    Denied,
    Expired,
    Failed,
}

#[derive(Clone)]
pub struct GitHub {
    client_id: String,
    oauth_base: String,
    api_base: String,
}

impl GitHub {
    pub fn from_env() -> Option<Self> {
        let client_id = std::env::var("TOKENLENS_GITHUB_CLIENT_ID")
            .ok()?
            .trim()
            .to_owned();
        if client_id.is_empty() {
            return None;
        }
        Some(Self {
            client_id,
            oauth_base: std::env::var("TOKENLENS_GITHUB_OAUTH_BASE")
                .unwrap_or_else(|_| "https://github.com".to_owned()),
            api_base: std::env::var("TOKENLENS_GITHUB_API_BASE")
                .unwrap_or_else(|_| "https://api.github.com".to_owned()),
        })
    }

    /// For tests and self-hosted stubs.
    pub fn with_bases(client_id: &str, oauth_base: &str, api_base: &str) -> Self {
        Self {
            client_id: client_id.to_owned(),
            oauth_base: oauth_base.trim_end_matches('/').to_owned(),
            api_base: api_base.trim_end_matches('/').to_owned(),
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .max_redirects(0)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build(),
        )
    }

    fn json_post(
        &self,
        url: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let body = serde_json::to_vec(payload).map_err(|_| GitHubError)?;
        let response = Self::agent()
            .post(url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(&body)
            .map_err(|_| GitHubError)?;
        if !response.status().is_success() {
            return Err(GitHubError);
        }
        let bytes = response
            .into_body()
            .with_config()
            .limit(64 * 1024)
            .read_to_vec()
            .map_err(|_| GitHubError)?;
        serde_json::from_slice(&bytes).map_err(|_| GitHubError)
    }

    pub fn start(&self) -> Result<DeviceStart, GitHubError> {
        let reply = self.json_post(
            &format!("{}/login/device/code", self.oauth_base),
            // GitHub Apps use fine-grained permissions, not OAuth scopes.
            // Sending `read:user user:email` here makes a GitHub App reject
            // the device-code request. The App needs only Account → Email
            // addresses (read) for the `/user/emails` lookup below.
            &serde_json::json!({ "client_id": self.client_id }),
        )?;
        let text = |name: &str| reply[name].as_str().map(str::to_owned);
        let number = |name: &str| reply[name].as_u64();
        Ok(DeviceStart {
            device_code: text("device_code").ok_or(GitHubError)?,
            user_code: text("user_code").ok_or(GitHubError)?,
            verification_uri: text("verification_uri")
                .or_else(|| text("verification_url"))
                .ok_or(GitHubError)?,
            interval_seconds: number("interval").unwrap_or(5),
            expires_in_seconds: number("expires_in").unwrap_or(900),
        })
    }

    /// One poll. GitHub answers 200 with `error` while the user has not
    /// approved yet, so the status code alone is not enough.
    pub fn poll(&self, device_code: &str) -> Poll {
        let reply = self.json_post(
            &format!("{}/login/oauth/access_token", self.oauth_base),
            &serde_json::json!({
                "client_id": self.client_id,
                "device_code": device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
            }),
        );
        let Ok(reply) = reply else {
            return Poll::Failed;
        };
        if let Some(error) = reply["error"].as_str() {
            return match error {
                "authorization_pending" => Poll::Pending,
                "slow_down" => Poll::SlowDown(reply["interval"].as_u64().unwrap_or(5)),
                "expired_token" => Poll::Expired,
                "access_denied" => Poll::Denied,
                _ => Poll::Failed,
            };
        }
        let Some(token) = reply["access_token"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            return Poll::Failed;
        };
        self.identity(token)
    }

    fn identity(&self, token: &str) -> Poll {
        let user = match Self::agent()
            .get(&format!("{}/user", self.api_base))
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", "2026-03-10")
            .call()
        {
            Ok(response) if response.status().is_success() => response
                .into_body()
                .with_config()
                .limit(64 * 1024)
                .read_to_vec()
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()),
            _ => None,
        };
        let Some(user) = user else {
            return Poll::Failed;
        };
        let Some(subject) = user["id"].as_i64().map(|id| id.to_string()) else {
            return Poll::Failed;
        };
        // Only a verified address may create or join a space.
        let emails = Self::agent()
            .get(&format!("{}/user/emails", self.api_base))
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", format!("Bearer {token}"))
            .header("X-GitHub-Api-Version", "2026-03-10")
            .call();
        let Some(list) = emails
            .ok()
            .filter(|response| response.status().is_success())
        else {
            return Poll::Failed;
        };
        let Ok(bytes) = list
            .into_body()
            .with_config()
            .limit(128 * 1024)
            .read_to_vec()
        else {
            return Poll::Failed;
        };
        let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) else {
            return Poll::Failed;
        };
        let verified = |entry: &&serde_json::Value| entry["verified"].as_bool() == Some(true);
        let email = list
            .iter()
            .filter(verified)
            .find(|entry| entry["primary"].as_bool() == Some(true))
            .or_else(|| list.iter().find(verified))
            .and_then(|entry| entry["email"].as_str())
            .map(str::to_owned);
        match email {
            Some(email) => Poll::Ready { subject, email },
            None => Poll::Failed,
        }
    }
}
