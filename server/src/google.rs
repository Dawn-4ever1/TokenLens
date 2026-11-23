//! Google sign-in (D051). Google has no device-code flow for desktop apps, so
//! the server runs a standard authorization-code flow with PKCE: the app opens
//! Google's consent page in the system browser, Google redirects back to this
//! server's callback, and the app polls for the result. The client secret
//! never leaves the server; only verified email addresses are trusted.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::secrets::Secret;

pub const FLOW_TTL_MS: i64 = 10 * 60 * 1000;
pub const FLOW_INTERVAL_SECONDS: u64 = 5;
const MAX_FLOWS: usize = 256;
const MAX_FLOWS_PER_CLIENT: usize = 8;
const DEFAULT_REDIRECT_URI: &str = "https://tokenlens.online/api/auth/google/callback";

/// Google could not be reached or answered something unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoogleError;

impl std::fmt::Display for GoogleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Google sign-in is not available right now")
    }
}

impl std::error::Error for GoogleError {}

/// The signed-in Google user. `subject` is Google's own user id (`sub`), the
/// only identity key; the email must be verified by Google.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone)]
pub struct Google {
    client_id: String,
    client_secret: Secret,
    redirect_uri: String,
    auth_base: String,
    token_base: String,
    userinfo_base: String,
    agent: ureq::Agent,
}

impl Google {
    pub fn from_env() -> Option<Self> {
        let client_id = std::env::var("TOKENLENS_GOOGLE_CLIENT_ID")
            .ok()?
            .trim()
            .to_owned();
        let client_secret = std::env::var("TOKENLENS_GOOGLE_CLIENT_SECRET")
            .ok()?
            .trim()
            .to_owned();
        if client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        let redirect_uri = std::env::var("TOKENLENS_GOOGLE_REDIRECT_URI")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_owned());
        let env_base = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        Some(Self {
            client_id,
            client_secret: Secret::from_header(&client_secret),
            redirect_uri,
            auth_base: env_base("TOKENLENS_GOOGLE_AUTH_BASE", "https://accounts.google.com"),
            token_base: env_base(
                "TOKENLENS_GOOGLE_TOKEN_BASE",
                "https://oauth2.googleapis.com",
            ),
            userinfo_base: env_base(
                "TOKENLENS_GOOGLE_USERINFO_BASE",
                "https://openidconnect.googleapis.com",
            ),
            agent: Self::build_agent(),
        })
    }

    /// For tests and self-hosted stubs.
    pub fn with_bases(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        auth_base: &str,
        token_base: &str,
        userinfo_base: &str,
    ) -> Self {
        Self {
            client_id: client_id.to_owned(),
            client_secret: Secret::from_header(client_secret),
            redirect_uri: redirect_uri.to_owned(),
            auth_base: auth_base.trim_end_matches('/').to_owned(),
            token_base: token_base.trim_end_matches('/').to_owned(),
            userinfo_base: userinfo_base.trim_end_matches('/').to_owned(),
            agent: Self::build_agent(),
        }
    }

    fn build_agent() -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .max_redirects(0)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build(),
        )
    }

    /// The consent page the app opens in the system browser. PKCE binds the
    /// later code exchange to this server's flow; `prompt=select_account`
    /// lets people with several Google accounts pick one.
    pub fn auth_url(&self, state: &str, challenge: &str) -> String {
        let query = serde_urlencoded::to_string([
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", "openid email profile"),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ])
        .expect("query pairs are strings");
        format!("{}/o/oauth2/v2/auth?{query}", self.auth_base)
    }

    /// Exchanges the callback's authorization code for an access token. The
    /// token is used once for the profile lookup below and never stored.
    pub fn exchange(&self, code: &str, verifier: &str) -> Result<String, GoogleError> {
        let body = serde_urlencoded::to_string([
            ("code", code),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.expose()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier),
        ])
        .map_err(|_| GoogleError)?;
        let response = self
            .agent
            .post(&format!("{}/token", self.token_base))
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body.as_bytes())
            .map_err(|_| GoogleError)?;
        if !response.status().is_success() {
            return Err(GoogleError);
        }
        let bytes = response
            .into_body()
            .with_config()
            .limit(64 * 1024)
            .read_to_vec()
            .map_err(|_| GoogleError)?;
        let reply: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| GoogleError)?;
        reply["access_token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or(GoogleError)
    }

    pub fn identity(&self, token: &str) -> Result<Identity, GoogleError> {
        let response = self
            .agent
            .get(&format!("{}/v1/userinfo", self.userinfo_base))
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .map_err(|_| GoogleError)?;
        if !response.status().is_success() {
            return Err(GoogleError);
        }
        let bytes = response
            .into_body()
            .with_config()
            .limit(64 * 1024)
            .read_to_vec()
            .map_err(|_| GoogleError)?;
        let user: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| GoogleError)?;
        let subject = user["sub"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .ok_or(GoogleError)?
            .to_owned();
        // Only a Google-verified address may create or join a space (D049).
        if user["email_verified"].as_bool() != Some(true) {
            return Err(GoogleError);
        }
        let email = user["email"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or(GoogleError)?
            .to_owned();
        let display_name = user["name"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().count() <= 64)
            .map(str::to_owned);
        let avatar_url = user["picture"]
            .as_str()
            .filter(|url| google_avatar_url(url))
            .map(str::to_owned);
        Ok(Identity {
            subject,
            email,
            display_name,
            avatar_url,
        })
    }
}

/// Google profile pictures live on `lhN.googleusercontent.com`. The desktop
/// app re-validates and resizes before downloading; this keeps junk out of
/// the database.
fn google_avatar_url(input: &str) -> bool {
    if input.len() > 2048
        || input
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
    {
        return false;
    }
    let Some(rest) = input.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let Some(label) = host.strip_suffix(".googleusercontent.com") else {
        return false;
    };
    let Some(digits) = label.strip_prefix("lh") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) && !path.is_empty()
}

fn random_b64(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).expect("operating system random source unavailable");
    URL_SAFE_NO_PAD.encode(buffer)
}

pub enum Outcome {
    Pending,
    Exchanging,
    Ready {
        session: Secret,
        email: String,
        display_name: Option<String>,
        avatar_url: Option<String>,
    },
    Denied,
    Failed,
}

struct Flow {
    client_key: String,
    state: String,
    verifier: String,
    expires_at: i64,
    outcome: Outcome,
}

/// A freshly started flow. `flow_id` is the poll secret the app's Rust side
/// keeps; `state` travels through the browser and back; the PKCE verifier
/// stays in the flow table, never in a response.
pub struct Started {
    pub flow_id: String,
    pub state: String,
    pub challenge: String,
}

pub enum FlowPoll {
    Pending,
    Ready {
        session: Secret,
        email: String,
        display_name: Option<String>,
        avatar_url: Option<String>,
    },
    Denied,
    Failed,
    Expired,
}

/// In-flight Google flows, in memory only (like `AuthLimiter`): a restart
/// forgets them and the user simply starts again. The session is delivered
/// once, to the first poll that finds it ready.
#[derive(Default)]
pub struct GoogleFlows {
    flows: Mutex<HashMap<String, Flow>>,
}

impl GoogleFlows {
    fn prune(flows: &mut HashMap<String, Flow>, now: i64) {
        flows.retain(|_, flow| flow.expires_at > now);
    }

    pub fn start(&self, now: i64) -> Option<Started> {
        self.start_for("default", now)
    }

    pub fn start_for(&self, client_key: &str, now: i64) -> Option<Started> {
        let mut flows = self.flows.lock().ok()?;
        Self::prune(&mut flows, now);
        let client_flows = flows
            .values()
            .filter(|flow| flow.client_key == client_key)
            .count();
        if flows.len() >= MAX_FLOWS || client_flows >= MAX_FLOWS_PER_CLIENT {
            return None;
        }
        let flow_id = random_b64(32);
        let state = random_b64(32);
        let verifier = random_b64(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        flows.insert(
            flow_id.clone(),
            Flow {
                client_key: client_key.to_owned(),
                state: state.clone(),
                verifier,
                expires_at: now + FLOW_TTL_MS,
                outcome: Outcome::Pending,
            },
        );
        Some(Started {
            flow_id,
            state,
            challenge,
        })
    }

    /// Atomically claims the PKCE verifier for a pending callback `state`,
    /// transitioning the flow to `Outcome::Exchanging` so concurrent duplicate
    /// callbacks cannot replay the authorization code.
    pub fn claim_verifier(&self, state: &str, now: i64) -> Option<String> {
        let mut flows = self.flows.lock().ok()?;
        Self::prune(&mut flows, now);
        let flow = flows
            .values_mut()
            .find(|flow| flow.state == state && matches!(flow.outcome, Outcome::Pending))?;
        flow.outcome = Outcome::Exchanging;
        Some(flow.verifier.clone())
    }

    /// Records the callback's result. Only a pending or exchanging flow accepts
    /// one, so a repeated or late callback cannot replace a delivered login.
    pub fn complete(&self, state: &str, outcome: Outcome, now: i64) -> bool {
        let Ok(mut flows) = self.flows.lock() else {
            return false;
        };
        Self::prune(&mut flows, now);
        let Some(flow) = flows.values_mut().find(|flow| {
            flow.state == state && matches!(flow.outcome, Outcome::Pending | Outcome::Exchanging)
        }) else {
            return false;
        };
        flow.outcome = outcome;
        true
    }

    pub fn poll(&self, flow_id: &str, now: i64) -> FlowPoll {
        let Ok(mut flows) = self.flows.lock() else {
            return FlowPoll::Expired;
        };
        Self::prune(&mut flows, now);
        let Some(flow) = flows.remove(flow_id) else {
            return FlowPoll::Expired;
        };
        match flow.outcome {
            Outcome::Pending | Outcome::Exchanging => {
                flows.insert(flow_id.to_owned(), flow);
                FlowPoll::Pending
            }
            Outcome::Ready {
                session,
                email,
                display_name,
                avatar_url,
            } => FlowPoll::Ready {
                session,
                email,
                display_name,
                avatar_url,
            },
            Outcome::Denied => FlowPoll::Denied,
            Outcome::Failed => FlowPoll::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_urls_must_be_google_picture_hosts() {
        for url in [
            "https://lh3.googleusercontent.com/a/ACg8ocJx=s96-c",
            "https://lh6.googleusercontent.com/a-/ABC123",
        ] {
            assert!(google_avatar_url(url), "{url}");
        }
        for url in [
            "http://lh3.googleusercontent.com/a/x",
            "https://lh3.googleusercontent.com.evil.example/a/x",
            "https://evil.example/lh3.googleusercontent.com/a/x",
            "https://lh.googleusercontent.com/a/x",
            "https://lh3x.googleusercontent.com/a/x",
            "https://lh3.googleusercontent.com:444/a/x",
            "https://user@lh3.googleusercontent.com/a/x",
            "https://lh3.googleusercontent.com/",
            "https://lh3.googleusercontent.com",
            "data:image/png;base64,x",
        ] {
            assert!(!google_avatar_url(url), "{url}");
        }
    }

    #[test]
    fn a_flow_delivers_its_session_once_and_expires() {
        let flows = GoogleFlows::default();
        let started = flows.start(1_000).expect("start");
        assert_eq!(started.challenge.len(), 43);
        assert!(matches!(
            flows.poll(&started.flow_id, 2_000),
            FlowPoll::Pending
        ));
        // The callback claims the verifier once by state; the challenge the app
        // opened Google with must be exactly its S256 hash (PKCE).
        let verifier = flows
            .claim_verifier(&started.state, 2_000)
            .expect("verifier");
        assert!(
            flows.claim_verifier(&started.state, 2_000).is_none(),
            "a duplicate callback cannot claim the same PKCE verifier twice"
        );
        assert!(matches!(
            flows.poll(&started.flow_id, 2_000),
            FlowPoll::Pending
        ));
        assert_eq!(
            started.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
        );
        assert!(flows.complete(
            &started.state,
            Outcome::Ready {
                session: Secret::from_header("tls_test"),
                email: "user@example.com".to_owned(),
                display_name: None,
                avatar_url: None,
            },
            2_000,
        ));
        assert!(!flows.complete(&started.state, Outcome::Failed, 2_000));
        let FlowPoll::Ready { session, .. } = flows.poll(&started.flow_id, 3_000) else {
            panic!("first poll must deliver the session");
        };
        assert_eq!(session.expose(), "tls_test");
        assert!(matches!(
            flows.poll(&started.flow_id, 3_000),
            FlowPoll::Expired
        ));

        let expired = flows.start(10_000).expect("start");
        assert!(matches!(
            flows.poll(&expired.flow_id, 10_000 + FLOW_TTL_MS + 1),
            FlowPoll::Expired
        ));
        assert!(
            flows
                .claim_verifier(&expired.state, 10_000 + FLOW_TTL_MS + 1)
                .is_none()
        );
    }

    #[test]
    fn flows_cap_per_client_without_blocking_other_clients() {
        let flows = GoogleFlows::default();
        for _ in 0..MAX_FLOWS_PER_CLIENT {
            assert!(flows.start_for("198.51.100.1", 1_000).is_some());
        }
        assert!(
            flows.start_for("198.51.100.1", 1_000).is_none(),
            "one client cannot start unbounded concurrent Google flows"
        );
        assert!(
            flows.start_for("203.0.113.2", 1_000).is_some(),
            "another client keeps its own per-client allowance"
        );
        // After expiry, capacity opens cleanly.
        assert!(
            flows
                .start_for("198.51.100.1", 1_000 + FLOW_TTL_MS + 1)
                .is_some()
        );
    }

    #[test]
    fn the_auth_url_carries_pkce_and_the_configured_redirect() {
        let google = Google::with_bases(
            "client-1",
            "secret-1",
            "https://tokenlens.online/api/auth/google/callback",
            "https://accounts.google.com",
            "https://oauth2.googleapis.com",
            "https://openidconnect.googleapis.com",
        );
        let url = google.auth_url("state-1", "challenge-1");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=client-1"));
        assert!(url.contains("state=state-1"));
        assert!(url.contains("code_challenge=challenge-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("prompt=select_account"));
        assert!(url.contains("scope=openid+email+profile"));
        assert!(url.contains(
            "redirect_uri=https%3A%2F%2Ftokenlens.online%2Fapi%2Fauth%2Fgoogle%2Fcallback"
        ));
        assert!(!url.contains("secret-1"));
    }
}
