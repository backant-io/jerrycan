//! HS256 JWTs: signed, NOT encrypted (interop bearer tokens — never put secrets
//! in a JWT). We hand-roll the `header.payload.signature` envelope over the
//! `hmac` crate; we do NOT implement HMAC ourselves.

use base64::Engine;
use hmac::{Hmac, Mac};
use jerrycan_core::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::unauthorized())
}

fn sign(message: &str, key: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    b64(&mac.finalize().into_bytes())
}

/// Encode claims as a signed HS256 JWT. Claims SHOULD include an `exp`
/// (unix seconds); `decode` enforces it when present.
pub fn encode<T: Serialize>(claims: &T, key: &[u8]) -> Result<String> {
    let header = b64(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_json =
        serde_json::to_vec(claims).map_err(|e| Error::internal(format!("jwt serialize: {e}")))?;
    let payload = b64(&payload_json);
    let message = format!("{header}.{payload}");
    let signature = sign(&message, key);
    Ok(format!("{message}.{signature}"))
}

/// Verify signature (constant-time via hmac's `verify`), enforce `exp` if
/// present, then deserialize. Any failure is `JC0401`.
pub fn decode<T: DeserializeOwned>(token: &str, key: &[u8]) -> Result<T> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::unauthorized());
    }
    let message = format!("{}.{}", parts[0], parts[1]);
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let provided = unb64(parts[2])?;
    mac.verify_slice(&provided)
        .map_err(|_| Error::unauthorized())?;

    let payload = unb64(parts[1])?;
    // Enforce exp if the payload carries one (don't require a fixed claim type).
    if let Ok(map) = serde_json::from_slice::<serde_json::Value>(&payload)
        && let Some(exp) = map.get("exp").and_then(|v| v.as_u64())
        && exp <= now_unix()
    {
        return Err(Error::unauthorized());
    }
    serde_json::from_slice(&payload).map_err(|_| Error::unauthorized())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Claims {
        sub: String,
        role: String,
        exp: u64,
    }

    fn key() -> [u8; 32] {
        crate::derive_key(b"a-very-long-development-secret-string!!", "jwt")
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let token = encode(
            &Claims {
                sub: "u1".into(),
                role: "admin".into(),
                exp: 9999999999,
            },
            &key(),
        )
        .unwrap();
        assert_eq!(token.split('.').count(), 3, "header.payload.signature");
        let claims: Claims = decode(&token, &key()).unwrap();
        assert_eq!(
            claims,
            Claims {
                sub: "u1".into(),
                role: "admin".into(),
                exp: 9999999999
            }
        );
    }

    #[test]
    fn a_tampered_payload_fails_signature_verification() {
        let token = encode(
            &Claims {
                sub: "u1".into(),
                role: "user".into(),
                exp: 9999999999,
            },
            &key(),
        )
        .unwrap();
        let mut parts: Vec<&str> = token.split('.').collect();
        // Swap the payload for a forged "admin" one (re-encoded), keep the old signature.
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"u1","role":"admin","exp":9999999999}"#);
        parts[1] = &forged;
        let tampered = parts.join(".");
        assert!(decode::<Claims>(&tampered, &key()).is_err());
    }

    #[test]
    fn a_wrong_key_is_rejected() {
        let token = encode(
            &Claims {
                sub: "u1".into(),
                role: "user".into(),
                exp: 9999999999,
            },
            &key(),
        )
        .unwrap();
        let other = crate::derive_key(b"different-secret-of-at-least-32-bytes!!", "jwt");
        assert!(decode::<Claims>(&token, &other).is_err());
    }

    #[test]
    fn expired_tokens_are_rejected() {
        let token = encode(
            &Claims {
                sub: "u1".into(),
                role: "user".into(),
                exp: 1,
            },
            &key(),
        )
        .unwrap();
        let err = decode::<Claims>(&token, &key()).unwrap_err();
        assert_eq!(err.code(), "JC0401");
    }
}
