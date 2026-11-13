//! Reversible-at-rest protection for persistent tenant pairing codes. The
//! server still authenticates claims by hash; plaintext is only returned to a
//! logged-in member. The encryption key is deployment configuration, not SQL.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

#[derive(Clone)]
pub struct PairingCipher(aes_gcm::Aes256Gcm);

impl PairingCipher {
    pub fn from_base64(value: &str) -> Result<Self, &'static str> {
        use aes_gcm::KeyInit;
        let bytes = URL_SAFE_NO_PAD
            .decode(value.trim())
            .or_else(|_| STANDARD.decode(value.trim()))
            .map_err(|_| "pairing key must be 32 random bytes in base64")?;
        if bytes.len() != 32 {
            return Err("pairing key must be 32 random bytes in base64");
        }
        Ok(Self(
            aes_gcm::Aes256Gcm::new_from_slice(&bytes).map_err(|_| "invalid pairing key")?,
        ))
    }

    pub fn generate() -> String {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).expect("operating system random source unavailable");
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn encrypt(&self, plaintext: &str) -> String {
        use aes_gcm::aead::{Aead, AeadCore, OsRng};
        let nonce = aes_gcm::Aes256Gcm::generate_nonce(&mut OsRng);
        let encrypted = self
            .0
            .encrypt(&nonce, plaintext.as_bytes())
            .expect("AES-GCM encryption cannot fail");
        let mut bytes = nonce.to_vec();
        bytes.extend(encrypted);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn decrypt(&self, encoded: &str) -> Option<String> {
        use aes_gcm::aead::Aead;
        let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
        let (nonce, encrypted) = bytes.split_at_checked(12)?;
        let nonce = aes_gcm::Nonce::from_slice(nonce);
        let plaintext = self.0.decrypt(nonce, encrypted).ok()?;
        String::from_utf8(plaintext).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_codes_round_trip_and_reject_wrong_keys() {
        let first = PairingCipher::from_base64(&PairingCipher::generate()).unwrap();
        let second = PairingCipher::from_base64(&PairingCipher::generate()).unwrap();
        let encrypted = first.encrypt("ABCD-EFGH-JKMN");
        assert_ne!(encrypted, "ABCD-EFGH-JKMN");
        assert_eq!(first.decrypt(&encrypted).as_deref(), Some("ABCD-EFGH-JKMN"));
        assert!(second.decrypt(&encrypted).is_none());
    }
}
