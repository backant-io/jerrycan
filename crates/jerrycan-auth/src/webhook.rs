//! Webhook signature verification primitives: HMAC over the EXACT request
//! bytes (pair with the `RawBody` extractor — re-serialized JSON never
//! verifies). Comparisons are constant-time via `hmac`'s `verify_slice`.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;

/// Hex HMAC-SHA256 of `message` — the scheme Stripe (`v1=`), GitHub
/// (`sha256=`), and most modern providers use.
pub fn sign_sha256_hex(secret: &[u8], message: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    hex_encode(&mac.finalize().into_bytes())
}

/// Verifies a hex HMAC-SHA256 signature in constant time. Case-insensitive
/// hex; malformed hex simply fails verification.
pub fn verify_sha256_hex(secret: &[u8], message: &[u8], signature_hex: &str) -> bool {
    let Some(signature) = hex_decode(signature_hex) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    mac.verify_slice(&signature).is_ok()
}

/// Base64 HMAC-SHA1 of `message` — Twilio's `X-Twilio-Signature` scheme
/// (the URL with sorted POST params appended). SHA-1 is what Twilio specifies;
/// HMAC-SHA1 remains secure as a MAC even though SHA-1 is broken for collision
/// resistance.
pub fn sign_sha1_base64(secret: &[u8], message: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Verifies a base64 HMAC-SHA1 signature in constant time. Malformed base64
/// simply fails verification.
pub fn verify_sha1_base64(secret: &[u8], message: &[u8], signature_b64: &str) -> bool {
    let Ok(signature) = base64::engine::general_purpose::STANDARD.decode(signature_b64) else {
        return false;
    };
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    mac.verify_slice(&signature).is_ok()
}

/// Lowercase-hex encode raw MAC bytes.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).expect("nibble < 16"));
        out.push(char::from_digit((b & 0x0f) as u32, 16).expect("nibble < 16"));
    }
    out
}

/// Decode a hex string (upper- or lowercase). Returns `None` for odd length or
/// any non-hex character — never panics, so untrusted signatures are safe.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_roundtrip_and_tamper_detection() {
        let secret = b"whsec_test";
        let message = b"1717000000.{\"id\":\"evt_1\"}";
        let signature = sign_sha256_hex(secret, message);
        assert_eq!(signature.len(), 64);
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(verify_sha256_hex(secret, message, &signature));
        assert!(!verify_sha256_hex(secret, b"tampered", &signature));
        assert!(!verify_sha256_hex(b"wrong-secret", message, &signature));
        // Uppercase hex also accepted (providers vary).
        assert!(verify_sha256_hex(
            secret,
            message,
            &signature.to_uppercase()
        ));
        // Garbage that is not hex never verifies (and never panics).
        assert!(!verify_sha256_hex(secret, message, "zz!"));
        assert!(!verify_sha256_hex(secret, message, ""));
    }

    #[test]
    fn sha1_base64_roundtrip_and_tamper_detection() {
        let secret = b"twilio_auth_token";
        let message = b"https://example.test/hookBody=value";
        let signature = sign_sha1_base64(secret, message);
        assert!(verify_sha1_base64(secret, message, &signature));
        assert!(!verify_sha1_base64(secret, b"tampered", &signature));
        assert!(!verify_sha1_base64(secret, message, "not base64!!"));
    }

    /// Known-answer test: RFC 2202 test case 2 for HMAC-SHA1 ("what do ya want
    /// for nothing?" with key "Jefe") and RFC 4231 test case 2 for HMAC-SHA256 —
    /// proves we compute the real algorithms, not just roundtrip-consistent ones.
    #[test]
    fn known_answer_vectors() {
        assert_eq!(
            sign_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        use base64::Engine as _;
        let mac = sign_sha1_base64(b"Jefe", b"what do ya want for nothing?");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&mac)
            .unwrap();
        assert_eq!(
            raw.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
    }
}
