//! Device credentials (system keychain only) and pairing.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use usage_event::envelope::{FormFactor, PairClaimRequest, PairClaimResponse, Platform};

use crate::transport::{self, Transport, UrlError};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials {
    pub server_url: String,
    pub device_id: String,
    pub ingest_token: String,
    pub read_token: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("server_url", &self.server_url)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("the system keychain could not be used")]
    Store,
    #[error("the saved pairing in the system keychain is unreadable; pair again")]
    Corrupt,
}

pub trait CredentialStore {
    fn load(&self) -> Result<Option<Credentials>, CredentialError>;
    fn save(&self, credentials: &Credentials) -> Result<(), CredentialError>;
    /// `true` if something was deleted.
    fn delete(&self) -> Result<bool, CredentialError>;
}

/// One generic-password entry (macOS Keychain / Windows Credential Manager)
/// whose secret is the credentials JSON.
pub struct KeychainStore {
    service: String,
}

pub const DEFAULT_KEYCHAIN_SERVICE: &str = "tokenlens";
const KEYCHAIN_ACCOUNT: &str = "device";

impl KeychainStore {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_owned(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(&self.service, KEYCHAIN_ACCOUNT).map_err(|_| CredentialError::Store)
    }
}

fn parse(secret: &str) -> Result<Credentials, CredentialError> {
    let credentials: Credentials =
        serde_json::from_str(secret).map_err(|_| CredentialError::Corrupt)?;
    transport::validate_server_url(&credentials.server_url)
        .map_err(|_| CredentialError::Corrupt)?;
    Ok(credentials)
}

impl CredentialStore for KeychainStore {
    fn load(&self) -> Result<Option<Credentials>, CredentialError> {
        match self.entry()?.get_password() {
            Ok(secret) => parse(&secret).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredentialError::Store),
        }
    }

    fn save(&self, credentials: &Credentials) -> Result<(), CredentialError> {
        let secret = serde_json::to_string(credentials).map_err(|_| CredentialError::Store)?;
        self.entry()?
            .set_password(&secret)
            .map_err(|_| CredentialError::Store)
    }

    fn delete(&self) -> Result<bool, CredentialError> {
        match self.entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(_) => Err(CredentialError::Store),
        }
    }
}

#[derive(Default)]
pub struct MemoryStore(Mutex<Option<String>>);

impl CredentialStore for MemoryStore {
    fn load(&self) -> Result<Option<Credentials>, CredentialError> {
        let secret = self.0.lock().map_err(|_| CredentialError::Store)?.clone();
        secret.as_deref().map(parse).transpose()
    }

    fn save(&self, credentials: &Credentials) -> Result<(), CredentialError> {
        let secret = serde_json::to_string(credentials).map_err(|_| CredentialError::Store)?;
        *self.0.lock().map_err(|_| CredentialError::Store)? = Some(secret);
        Ok(())
    }

    fn delete(&self) -> Result<bool, CredentialError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| CredentialError::Store)?
            .take()
            .is_some())
    }
}

pub const fn this_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// A generic name, so the computer's own (often personal) name is not sent.
pub fn default_display_name(platform: Platform, form_factor: FormFactor) -> String {
    let platform = match platform {
        Platform::Macos => "Mac",
        Platform::Windows => "Windows",
        Platform::Linux => "Linux",
    };
    format!("{platform} {}", form_factor.as_str())
}

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error(transparent)]
    Url(#[from] UrlError),
    #[error("the server could not be reached")]
    Network,
    #[error("the pairing code was not accepted (expired, already used or mistyped)")]
    InvalidCode,
    #[error("too many pairing attempts; wait a few minutes and try again")]
    TooManyAttempts,
    #[error("the server refused the pairing request (HTTP {status}{})", code.as_deref().map(|code| format!(", {code}")).unwrap_or_default())]
    Refused { status: u16, code: Option<String> },
    #[error("the server reply was not understood")]
    BadResponse,
}

/// Exchanges a one-time pairing code for this device's tokens.
pub fn claim(
    transport: &dyn Transport,
    server_url: &str,
    code: &str,
    form_factor: FormFactor,
    display_name: &str,
) -> Result<Credentials, PairError> {
    let server_url = transport::validate_server_url(server_url)?;
    claim_validated(transport, server_url, code, form_factor, display_name)
}

/// [`claim`] for an address that needs no URL check: the app's built-in
/// local server (D044), which `transport` reaches without the network.
pub fn claim_validated(
    transport: &dyn Transport,
    server_url: String,
    code: &str,
    form_factor: FormFactor,
    display_name: &str,
) -> Result<Credentials, PairError> {
    let request = PairClaimRequest {
        code: code.trim().to_owned(),
        platform: this_platform(),
        form_factor,
        display_name: display_name.trim().to_owned(),
    };
    let body = serde_json::to_vec(&request).map_err(|_| PairError::BadResponse)?;
    let response = transport
        .post(&format!("{server_url}/api/pair/claim"), None, false, &body)
        .map_err(|_| PairError::Network)?;
    match response.status {
        200 => {
            let claimed: PairClaimResponse =
                serde_json::from_slice(&response.body).map_err(|_| PairError::BadResponse)?;
            let token_ok = |token: &str, prefix: &str| {
                token.starts_with(prefix)
                    && token.len() <= 256
                    && token.bytes().all(|byte| byte.is_ascii_graphic())
            };
            if claimed.device_id.is_empty()
                || !token_ok(&claimed.ingest_token, "tdi_")
                || !token_ok(&claimed.read_token, "tdr_")
            {
                return Err(PairError::BadResponse);
            }
            Ok(Credentials {
                server_url,
                device_id: claimed.device_id,
                ingest_token: claimed.ingest_token,
                read_token: claimed.read_token,
            })
        }
        401 => Err(PairError::InvalidCode),
        429 => Err(PairError::TooManyAttempts),
        status => Err(PairError::Refused {
            status,
            code: transport::error_code(&response.body),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::fake::FakeTransport;

    fn sample() -> Credentials {
        Credentials {
            server_url: "http://127.0.0.1:8787".into(),
            device_id: "dev_synthetic".into(),
            ingest_token: "tdi_synthetic_secret".into(),
            read_token: "tdr_synthetic_secret".into(),
        }
    }

    #[test]
    fn debug_output_has_no_tokens_and_memory_store_round_trips() {
        let text = format!("{:?}", sample());
        assert!(!text.contains("secret"));
        let store = MemoryStore::default();
        assert_eq!(store.load().expect("load"), None);
        store.save(&sample()).expect("save");
        assert_eq!(store.load().expect("load"), Some(sample()));
        assert!(store.delete().expect("delete"));
        assert!(!store.delete().expect("delete again"));
    }

    #[test]
    fn claim_sends_no_token_and_maps_errors() {
        let transport = FakeTransport::default();
        transport.push(
            200,
            serde_json::json!({
                "deviceId": "dev_synthetic",
                "ingestToken": "tdi_synthetic_secret",
                "readToken": "tdr_synthetic_secret"
            }),
        );
        let credentials = claim(
            &transport,
            "http://127.0.0.1:8787/",
            " ABCD-EFGH-JKMN ",
            FormFactor::Laptop,
            "Mac laptop",
        )
        .expect("claim");
        assert_eq!(credentials, sample());
        let sent = transport.sent();
        assert_eq!(sent[0].url, "http://127.0.0.1:8787/api/pair/claim");
        assert_eq!(sent[0].bearer, None);
        let body: serde_json::Value = serde_json::from_slice(&sent[0].body).expect("json");
        assert_eq!(body["code"], "ABCD-EFGH-JKMN");
        assert_eq!(body["formFactor"], "laptop");
        transport.push(401, serde_json::json!({"error": "invalid_pair_code"}));
        assert!(matches!(
            claim(
                &transport,
                "http://127.0.0.1:1",
                "x",
                FormFactor::Desktop,
                "x"
            ),
            Err(PairError::InvalidCode)
        ));
        transport.push(400, serde_json::json!({"error": "bad_request"}));
        assert!(matches!(
            claim(
                &transport,
                "http://127.0.0.1:1",
                "x",
                FormFactor::Desktop,
                "x"
            ),
            Err(PairError::Refused {
                status: 400,
                code: Some(_)
            })
        ));
        transport.push(
            200,
            serde_json::json!({"deviceId": "d", "ingestToken": "x", "readToken": "y"}),
        );
        assert!(matches!(
            claim(
                &transport,
                "http://127.0.0.1:1",
                "x",
                FormFactor::Desktop,
                "x"
            ),
            Err(PairError::BadResponse)
        ));
        assert!(matches!(
            claim(
                &transport,
                "http://example.com",
                "x",
                FormFactor::Desktop,
                "x"
            ),
            Err(PairError::Url(UrlError::Scheme))
        ));
        assert_eq!(
            default_display_name(Platform::Windows, FormFactor::Desktop),
            "Windows desktop"
        );
    }
}
