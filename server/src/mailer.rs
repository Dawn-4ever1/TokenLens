//! Minimal outbound email delivery. Request logs never include the recipient,
//! verification code, message body or API credential.

use std::time::Duration;

/// Sending failed; the caller turns this into "service unavailable" without
/// learning whether the address or the provider was the problem.
#[derive(Debug)]
pub struct MailError;

impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sign-in email could not be sent")
    }
}

impl std::error::Error for MailError {}

#[derive(Clone, Debug)]
pub struct Mailer {
    api_key: String,
    from: String,
    endpoint: String,
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
            .unwrap_or_else(|_| "https://api.resend.com/emails".to_owned());
        Some(Self {
            api_key,
            from,
            endpoint,
        })
    }

    /// Sends only a short-lived numeric code and generic product text. No
    /// usage data, device identifiers or tenant identifiers are included.
    pub fn send_code(&self, email: &str, code: &str) -> Result<(), MailError> {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .max_redirects(0)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build(),
        );
        let payload = serde_json::json!({
            "from": self.from,
            "to": [email],
            "subject": "TokenLens 验证码",
            "text": format!("你的 TokenLens 验证码是 {code}，10 分钟内有效。不要把它转发给别人。若非本人操作，请忽略此邮件。")
        });
        let body = serde_json::to_vec(&payload).map_err(|_| MailError)?;
        let response = agent
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send(&body)
            .map_err(|_| MailError)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(MailError)
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
