//! Random identifiers, credentials and their hashes.
//!
//! Plain-text credentials only ever live in a [`Secret`], whose `Debug`
//! output is redacted, and leave the process once in an HTTP response or on
//! the operator's terminal.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const PAIR_CODE_LEN: usize = 12;
pub const PAIR_CODE_TTL_MS: i64 = 10 * 60 * 1000;
const TOKEN_BYTES: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn from_digits(code: String) -> Self {
        Self(code)
    }

    pub fn from_header(token: &str) -> Self {
        Self(token.to_owned())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ingest,
    Read,
}

impl TokenKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Read => "read",
        }
    }

    /// Distinct prefixes let secret scanners recognise leaked tokens.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Ingest => "tdi_",
            Self::Read => "tdr_",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "ingest" => Some(Self::Ingest),
            "read" => Some(Self::Read),
            _ => None,
        }
    }
}

fn fill_random(bytes: &mut [u8]) {
    // Without an OS random source no credential can be issued safely.
    getrandom::fill(bytes).expect("operating system random source unavailable");
}

fn random_crockford(len: usize) -> String {
    let mut bytes = vec![0_u8; len];
    fill_random(&mut bytes);
    // 256 is a multiple of 32, so masking keeps every symbol equally likely.
    bytes
        .iter()
        .map(|byte| char::from(CROCKFORD[usize::from(byte & 0x1f)]))
        .collect()
}

pub fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// 12 Crockford base32 symbols, 60 random bits, without separators.
pub fn new_pair_code() -> Secret {
    Secret(random_crockford(PAIR_CODE_LEN))
}

/// `ABCDEFGHJKMN` → `ABCD-EFGH-JKMN`.
pub fn format_pair_code(code: &Secret) -> Secret {
    let grouped = code
        .expose()
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("-");
    Secret(grouped)
}

/// Accepts lower case, `-` or space separators and Crockford's aliases
/// (`I`/`L` → `1`, `O` → `0`); anything else is not a code.
pub fn normalize_pair_code(input: &str) -> Option<Secret> {
    if input.len() > 32 {
        return None;
    }
    let mut code = String::with_capacity(PAIR_CODE_LEN);
    for ch in input.chars() {
        let ch = match ch.to_ascii_uppercase() {
            '-' | ' ' => continue,
            'I' | 'L' => '1',
            'O' => '0',
            ch if ch.is_ascii() && CROCKFORD.contains(&(ch as u8)) => ch,
            _ => return None,
        };
        code.push(ch);
    }
    (code.len() == PAIR_CODE_LEN).then_some(Secret(code))
}

pub fn pair_code_hash(normalized: &Secret) -> String {
    sha256_hex(normalized.expose())
}

pub fn new_token(kind: TokenKind) -> Secret {
    let mut bytes = [0_u8; TOKEN_BYTES];
    fill_random(&mut bytes);
    Secret(format!(
        "{}{}",
        kind.prefix(),
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub fn token_hash(token: &str) -> String {
    sha256_hex(token)
}

pub fn new_device_id() -> String {
    format!("dev_{}", random_crockford(16).to_ascii_lowercase())
}

pub fn new_token_id() -> String {
    format!("tok_{}", random_crockford(16).to_ascii_lowercase())
}

/// Opaque account login credential. Unlike device tokens it is never an
/// ingest/read credential and cannot be used to upload or read usage directly.
pub fn new_session() -> Secret {
    let mut bytes = [0_u8; TOKEN_BYTES];
    fill_random(&mut bytes);
    Secret(format!("tls_{}", URL_SAFE_NO_PAD.encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_codes_have_the_documented_shape() {
        let code = new_pair_code();
        assert_eq!(code.expose().len(), PAIR_CODE_LEN);
        assert!(code.expose().bytes().all(|byte| CROCKFORD.contains(&byte)));
        let shown = format_pair_code(&code);
        assert_eq!(shown.expose().len(), 14);
        assert_eq!(shown.expose().matches('-').count(), 2);
        assert_eq!(normalize_pair_code(shown.expose()), Some(code.clone()));
        assert_ne!(new_pair_code(), code);
    }

    #[test]
    fn normalization_accepts_aliases_and_rejects_garbage() {
        assert_eq!(
            normalize_pair_code("abcd efgh ilou").map(|code| code.into_inner()),
            None,
            "U is not a Crockford symbol"
        );
        assert_eq!(
            normalize_pair_code("abcd-efgh-il0o").map(|code| code.into_inner()),
            Some("ABCDEFGH1100".to_owned())
        );
        assert!(normalize_pair_code("ABCD-EFGH-JKM").is_none());
        assert!(normalize_pair_code("ABCD-EFGH-JKMNP").is_none());
        assert!(normalize_pair_code("ÄBCD-EFGH-JKMN").is_none());
    }

    #[test]
    fn tokens_are_prefixed_and_long() {
        let ingest = new_token(TokenKind::Ingest);
        let read = new_token(TokenKind::Read);
        assert!(ingest.expose().starts_with("tdi_"));
        assert!(read.expose().starts_with("tdr_"));
        assert_eq!(ingest.expose().len(), 4 + 43);
        assert!(!ingest.expose().contains('='));
        assert_eq!(token_hash(ingest.expose()).len(), 64);
        assert!(!format!("{ingest:?}").contains("tdi_"));
        let device = new_device_id();
        assert!(device.starts_with("dev_") && device.len() == 20);
    }
}
