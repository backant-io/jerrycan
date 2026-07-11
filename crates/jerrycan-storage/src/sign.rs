//! App-HMAC signed URLs (the universal default): HMAC-SHA256 over
//! `bucket|object_id|exp`, hex-encoded, verified constant-time. Works on every
//! backend (local included) and keeps the in-app guard + access log; the S3
//! native presign (sigv4.rs) is the opt-in alternative.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Lowercase hex. Shared by the checksum/ETag path (lib.rs) and signatures.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).expect("nibble < 16"));
        out.push(char::from_digit((b & 0x0f) as u32, 16).expect("nibble < 16"));
    }
    out
}

/// Strict lowercase/uppercase hex decode; `None` on odd length or a non-hex char.
pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn mac_for(key: &[u8], bucket: &str, object_id: &str, exp_unix: u64) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(format!("{bucket}|{object_id}|{exp_unix}").as_bytes());
    mac
}

/// The hex signature for `/<bucket>/<object_id>?exp=<exp_unix>&sig=…`.
pub(crate) fn sign(key: &[u8], bucket: &str, object_id: &str, exp_unix: u64) -> String {
    hex(&mac_for(key, bucket, object_id, exp_unix).finalize().into_bytes())
}

/// Verify a presented signature: unexpired (`now < exp`) and a constant-time
/// MAC match (`verify_slice` — never `==` on the hex strings).
pub(crate) fn verify(
    key: &[u8],
    bucket: &str,
    object_id: &str,
    exp_unix: u64,
    sig_hex: &str,
    now_unix: u64,
) -> bool {
    if now_unix >= exp_unix {
        return false;
    }
    let Some(sig) = unhex(sig_hex) else {
        return false;
    };
    mac_for(key, bucket, object_id, exp_unix).verify_slice(&sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"a-very-long-development-secret-string!!";

    #[test]
    fn sign_then_verify_round_trips_and_expires() {
        // WHY: the signed URL is the ONLY credential a download carries — it
        // must verify before expiry and hard-fail after (no grace).
        let sig = sign(KEY, "avatars", "obj-1", 1_000);
        assert!(verify(KEY, "avatars", "obj-1", 1_000, &sig, 999), "valid before expiry");
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, &sig, 1_000), "exp is exclusive");
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, &sig, 2_000), "expired");
    }

    #[test]
    fn any_component_change_breaks_the_signature() {
        // WHY: the signature binds bucket + object id + expiry — reusing a sig
        // across buckets/objects or stretching the expiry must fail.
        let sig = sign(KEY, "avatars", "obj-1", 1_000);
        assert!(!verify(KEY, "invoices", "obj-1", 1_000, &sig, 1));
        assert!(!verify(KEY, "avatars", "obj-2", 1_000, &sig, 1));
        assert!(!verify(KEY, "avatars", "obj-1", 9_000, &sig, 1));
        assert!(!verify(b"other-key", "avatars", "obj-1", 1_000, &sig, 1));
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, "zz-not-hex", 1), "junk sig is false, not a panic");
        let truncated = &sig[..sig.len() - 2];
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, truncated, 1));
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(&[0x00, 0xff, 0x10]), "00ff10");
        assert_eq!(unhex("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert!(unhex("0g").is_none());
        assert!(unhex("0").is_none(), "odd length");
    }
}
