//! Session cookies: server-private state, ChaCha20-Poly1305 AEAD
//! (confidential + tamper-evident). Wire format: base64url(nonce[12] ‖ ciphertext+tag).
//! The cookie is Secure/HttpOnly/SameSite=Lax by default (spec §4.4).

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use jerrycan_core::{Error, Result};
use rand::RngCore;
use serde::{Serialize, de::DeserializeOwned};

const COOKIE_NAME: &str = "jerrycan_session";

/// Encrypts/decrypts session payloads with a per-store AEAD key.
#[derive(Clone)]
pub struct SessionStore {
    cipher: ChaCha20Poly1305,
}

impl SessionStore {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
        }
    }

    /// Serialize + encrypt to a base64url token (no padding).
    pub fn encode<T: Serialize>(&self, value: &T) -> Result<String> {
        let plaintext = serde_json::to_vec(value)
            .map_err(|e| Error::internal(format!("session serialize: {e}")))?;
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|_| Error::internal("session encrypt failed"))?;
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(combined))
    }

    /// Decrypt + deserialize. Any failure (bad base64, short input, AEAD
    /// rejection, JSON shape) is `JC0401` — an untrusted client value.
    pub fn decode<T: DeserializeOwned>(&self, token: &str) -> Result<T> {
        let combined = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| Error::unauthorized())?;
        if combined.len() < 12 {
            return Err(Error::unauthorized());
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| Error::unauthorized())?;
        serde_json::from_slice(&plaintext).map_err(|_| Error::unauthorized())
    }

    /// A `Set-Cookie` header value establishing the session (secure defaults).
    pub fn set_cookie<T: Serialize>(&self, value: &T) -> Result<String> {
        let token = self.encode(value)?;
        Ok(format!(
            "{COOKIE_NAME}={token}; HttpOnly; Secure; SameSite=Lax; Path=/"
        ))
    }

    /// A `Set-Cookie` header value clearing the session.
    pub fn clear_cookie(&self) -> String {
        format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
    }

    // Consumed by the `Session` extractor in a later task; reserved here with the store.
    #[allow(dead_code)]
    pub(crate) fn read_cookie(&self, cookie_header: &str) -> Option<String> {
        cookie_header
            .split(';')
            .filter_map(|kv| kv.trim().split_once('='))
            .find(|(k, _)| *k == COOKIE_NAME)
            .map(|(_, v)| v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sess {
        user_id: i64,
        role: String,
    }

    fn store() -> SessionStore {
        SessionStore::new(&crate::derive_key(
            b"a-very-long-development-secret-string!!",
            "session",
        ))
    }

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let s = store();
        let token = s
            .encode(&Sess {
                user_id: 7,
                role: "admin".into(),
            })
            .unwrap();
        let back: Sess = s.decode(&token).unwrap();
        assert_eq!(
            back,
            Sess {
                user_id: 7,
                role: "admin".into()
            }
        );
    }

    #[test]
    fn tokens_are_opaque_and_nonce_randomized() {
        let s = store();
        let a = s
            .encode(&Sess {
                user_id: 1,
                role: "u".into(),
            })
            .unwrap();
        let b = s
            .encode(&Sess {
                user_id: 1,
                role: "u".into(),
            })
            .unwrap();
        assert_ne!(a, b, "fresh nonce per encode");
        assert!(!a.contains("user_id"), "ciphertext is opaque: {a}");
    }

    #[test]
    fn tampering_is_rejected() {
        let s = store();
        let mut token = s
            .encode(&Sess {
                user_id: 1,
                role: "u".into(),
            })
            .unwrap();
        // Flip a character in the middle of the base64 payload.
        let mid = token.len() / 2;
        let bytes = flip_one_char(&token, mid);
        token = bytes;
        assert!(
            s.decode::<Sess>(&token).is_err(),
            "AEAD must reject tampering"
        );
    }

    #[test]
    fn a_wrong_key_cannot_decrypt() {
        let a = store();
        let token = a
            .encode(&Sess {
                user_id: 1,
                role: "u".into(),
            })
            .unwrap();
        let other = SessionStore::new(&crate::derive_key(
            b"a-totally-different-secret-of-length-32+",
            "session",
        ));
        assert!(other.decode::<Sess>(&token).is_err());
    }

    #[test]
    fn set_cookie_and_clear_cookie_have_secure_attributes() {
        let s = store();
        let set = s
            .set_cookie(&Sess {
                user_id: 1,
                role: "u".into(),
            })
            .unwrap();
        assert!(set.starts_with("jerrycan_session="));
        for attr in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
            assert!(set.contains(attr), "missing {attr}: {set}");
        }
        let clear = s.clear_cookie();
        assert!(clear.contains("Max-Age=0"));
    }

    // Flips one base64 char to a different one (corrupts the token).
    fn flip_one_char(s: &str, at: usize) -> String {
        let mut chars: Vec<char> = s.chars().collect();
        chars[at] = if chars[at] == 'A' { 'B' } else { 'A' };
        chars.into_iter().collect()
    }
}
