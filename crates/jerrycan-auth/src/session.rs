//! Session cookies: server-private state, ChaCha20-Poly1305 AEAD
//! (confidential + tamper-evident). Wire format: `base64url(nonce[12] ‖ ciphertext+tag)`.
//! The cookie is Secure/HttpOnly/SameSite=Lax by default (spec §4.4).

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use jerrycan_core::{Error, Result};
use rand::RngCore;
use serde::{Serialize, de::DeserializeOwned};

const COOKIE_NAME: &str = "jerrycan_session";

/// Encrypts/decrypts session payloads with a per-store AEAD key.
///
/// Supports key rotation: `encode` always uses the primary key, while `decode`
/// tries the primary first, then each retired fallback in order. This lets a
/// deployment rotate `JERRYCAN_SECRET` without invalidating sessions/tokens
/// minted under the previous key — the old key is moved to `fallbacks` until it
/// is fully retired (dropped from the list), at which point its ciphertexts stop
/// decrypting.
#[derive(Clone)]
pub struct SessionStore {
    primary: ChaCha20Poly1305,
    fallbacks: Vec<ChaCha20Poly1305>,
}

impl SessionStore {
    /// Single-key store (no rotation). Equivalent to `with_keys(key, &[])`.
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            primary: ChaCha20Poly1305::new(key.into()),
            fallbacks: Vec::new(),
        }
    }

    /// Rotation-aware store: `encode` uses `primary`; `decode` tries `primary`
    /// then each entry of `fallbacks` in order. The first key that authenticates
    /// the ciphertext wins.
    pub fn with_keys(primary: &[u8; 32], fallbacks: &[[u8; 32]]) -> Self {
        Self {
            primary: ChaCha20Poly1305::new(primary.into()),
            fallbacks: fallbacks
                .iter()
                .map(|k| ChaCha20Poly1305::new(k.into()))
                .collect(),
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
            .primary
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|_| Error::internal("session encrypt failed"))?;
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(combined))
    }

    /// Decrypt + deserialize. Tries the primary key, then each rotation fallback
    /// in order; the first key that authenticates wins. Any failure (bad base64,
    /// short input, AEAD rejection under *every* key, JSON shape) is `JC0401` —
    /// an untrusted client value.
    pub fn decode<T: DeserializeOwned>(&self, token: &str) -> Result<T> {
        let combined = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| Error::unauthorized())?;
        if combined.len() < 12 {
            return Err(Error::unauthorized());
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        // Primary first, then retired keys in order. AEAD authenticates each
        // attempt, so a wrong key simply fails to decrypt (no false positives).
        let plaintext = std::iter::once(&self.primary)
            .chain(self.fallbacks.iter())
            .find_map(|cipher| cipher.decrypt(nonce, ciphertext).ok())
            .ok_or_else(Error::unauthorized)?;
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

    /// Extract the session cookie value from a `Cookie` request header.
    /// Public so sibling crates (and the fuzz-smoke suite) can exercise the parser.
    pub fn read_cookie(&self, cookie_header: &str) -> Option<String> {
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

    // --- rotation (multi-key decrypt) ---

    const KEY_OLD: [u8; 32] = [1u8; 32];
    const KEY_NEW: [u8; 32] = [2u8; 32];
    const KEY_STRANGER: [u8; 32] = [9u8; 32];

    fn sample() -> Sess {
        Sess {
            user_id: 42,
            role: "user".into(),
        }
    }

    #[test]
    fn rotation_keeps_old_ciphertexts_decryptable_so_no_one_is_logged_out() {
        // Encrypt under the OLD key (single-key store, pre-rotation).
        let before = SessionStore::new(&KEY_OLD);
        let token = before.encode(&sample()).unwrap();

        // Rotate: NEW is primary, OLD becomes a retired fallback.
        let after = SessionStore::with_keys(&KEY_NEW, &[KEY_OLD]);
        let back: Sess = after
            .decode(&token)
            .expect("a session minted before rotation must still decrypt via fallback");
        assert_eq!(back, sample());
    }

    #[test]
    fn encode_after_rotation_uses_the_new_primary_not_a_fallback() {
        let after = SessionStore::with_keys(&KEY_NEW, &[KEY_OLD]);
        let token = after.encode(&sample()).unwrap();
        // The NEW key alone (no fallbacks) must decrypt it: encode used primary.
        let new_only = SessionStore::new(&KEY_NEW);
        assert_eq!(new_only.decode::<Sess>(&token).unwrap(), sample());
        // The OLD key alone must NOT decrypt it.
        let old_only = SessionStore::new(&KEY_OLD);
        assert!(old_only.decode::<Sess>(&token).is_err());
    }

    #[test]
    fn a_key_in_neither_primary_nor_fallbacks_is_rejected_401() {
        // A ciphertext from a stranger key (never primary, never retired).
        let stranger = SessionStore::new(&KEY_STRANGER);
        let token = stranger.encode(&sample()).unwrap();

        let store = SessionStore::with_keys(&KEY_NEW, &[KEY_OLD]);
        let err = store.decode::<Sess>(&token).unwrap_err();
        assert_eq!(
            err.code(),
            "JC0401",
            "fully-retired/unknown keys must invalidate (rotation is not forever)"
        );
    }

    #[test]
    fn new_with_no_fallbacks_matches_with_keys_empty() {
        let a = SessionStore::new(&KEY_NEW);
        let token = a.encode(&sample()).unwrap();
        let b = SessionStore::with_keys(&KEY_NEW, &[]);
        assert_eq!(b.decode::<Sess>(&token).unwrap(), sample());
    }
}
