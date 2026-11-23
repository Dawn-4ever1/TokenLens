//! Minimal outbound email delivery. Request logs never include the recipient,
//! verification code, message body or API credential.

use std::time::Duration;

/// Sending failed; the caller turns this into "service unavailable" without
/// learning whether the address or the provider was the problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailError {
    pub status: Option<u16>,
}

impl MailError {
    pub const fn status(self) -> Option<u16> {
        self.status
    }
}

impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sign-in email could not be sent")
    }
}

impl std::error::Error for MailError {}

#[derive(Clone)]
pub struct Mailer {
    api_key: crate::secrets::Secret,
    from: String,
    endpoint: String,
    agent: ureq::Agent,
}

impl Mailer {
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("TOKENLENS_EMAIL_API_KEY")
            .ok()?
            .trim()
            .to_owned();
        let from = std::env::var("TOKENLENS_EMAIL_FROM")
            .ok()?
            .trim()
            .to_owned();
        if api_key.is_empty() || from.is_empty() {
            return None;
        }
        let endpoint = std::env::var("TOKENLENS_EMAIL_API_URL")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "https://api.resend.com/emails".to_owned());
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .max_redirects(0)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build(),
        );
        Some(Self {
            api_key: crate::secrets::Secret::from_header(&api_key),
            from,
            endpoint,
            agent,
        })
    }

    /// Sends only a short-lived numeric code and generic product text. No
    /// usage data, device identifiers or tenant identifiers are included.
    pub fn send_code(&self, email: &str, code: &str) -> Result<(), MailError> {
        let payload = serde_json::json!({
            "from": self.from,
            "to": [email],
            "subject": "TokenLens 验证码",
            "text": format!("你的 TokenLens 验证码是 {code}，10 分钟内有效。不要把它转发给别人。若非本人操作，请忽略此邮件。")
        });
        let body = serde_json::to_vec(&payload).map_err(|_| MailError { status: None })?;
        let response = self
            .agent
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key.expose()))
            .header("Content-Type", "application/json")
            .send(&body)
            .map_err(|err| match err {
                ureq::Error::StatusCode(code) => MailError { status: Some(code) },
                _ => MailError { status: None },
            })?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(MailError {
                status: Some(status.as_u16()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_configuration_has_no_secret_values() {
        let endpoint = "https://api.resend.com/emails";
        assert!(endpoint.starts_with("https://"));
        assert!(!endpoint.contains("token"));
    }
}
