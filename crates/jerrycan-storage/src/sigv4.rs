//! AWS Signature Version 4 over `hmac` + `sha2` (no new crypto crate): header
//! signing for S3 requests and query presigning for native signed GET URLs.
//! Reference: the AWS SigV4 specification; unit tests pin the AWS-published
//! example vectors so a canonicalization regression cannot slip through.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub(crate) struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    crate::sign::hex(&Sha256::digest(data))
}

fn hmac_raw(key: &[u8], data: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().into()
}

/// The AWS4 signing-key chain: HMAC("AWS4"+secret, date) → region → service →
/// "aws4_request".
pub(crate) fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_raw(format!("AWS4{secret}").as_bytes(), date);
    let k_region = hmac_raw(&k_date, region);
    let k_service = hmac_raw(&k_region, service);
    hmac_raw(&k_service, "aws4_request")
}

/// RFC 3986 percent-encoding over the unreserved set; `keep_slash` leaves `/`
/// intact (S3 canonical URIs encode per path segment).
pub(crate) fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Canonical query string: keys+values uri-encoded, sorted by encoded key.
fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The Authorization header + raw signature for a header-signed request.
/// `headers` must already contain `host` (and, for S3, `x-amz-date` +
/// `x-amz-content-sha256`); names are lowercased and sorted here.
pub(crate) fn authorization(
    creds: &Credentials,
    service: &str,
    method: &str,
    canonical_path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    payload_sha256_hex: &str,
    datetime: &str, // YYYYMMDDTHHMMSSZ
) -> (String, String) {
    let mut hdrs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    hdrs.sort();
    let signed_headers = hdrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical_headers: String = hdrs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let canonical_request = format!(
        "{method}\n{canonical_path}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_sha256_hex}",
        canonical_query(query)
    );
    let date = &datetime[..8];
    let scope = format!("{date}/{}/{service}/aws4_request", creds.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(&creds.secret_key, date, &creds.region, service);
    let signature = crate::sign::hex(&hmac_raw(&key, &string_to_sign));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key
    );
    (auth, signature)
}

/// A query-presigned GET URL (service = s3, UNSIGNED-PAYLOAD, host-only
/// signed headers) — the native signed-URL path (Supabase createSignedUrl parity).
pub(crate) fn presign_url(
    creds: &Credentials,
    endpoint: &str, // scheme://host[:port], no trailing slash
    canonical_path: &str,
    ttl_secs: u64,
    datetime: &str,
) -> String {
    let date = &datetime[..8];
    let scope = format!("{date}/{}/s3/aws4_request", creds.region);
    let host = endpoint.split_once("://").map(|(_, rest)| rest).unwrap_or(endpoint);
    let query: Vec<(String, String)> = vec![
        ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
        ("X-Amz-Credential".into(), format!("{}/{scope}", creds.access_key)),
        ("X-Amz-Date".into(), datetime.into()),
        ("X-Amz-Expires".into(), ttl_secs.to_string()),
        ("X-Amz-SignedHeaders".into(), "host".into()),
    ];
    let canonical_request = format!(
        "GET\n{canonical_path}\n{}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD",
        canonical_query(&query)
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(&creds.secret_key, date, &creds.region, "s3");
    let signature = crate::sign::hex(&hmac_raw(&key, &string_to_sign));
    format!(
        "{endpoint}{canonical_path}?{}&X-Amz-Signature={signature}",
        canonical_query(&query)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    #[test]
    fn signing_key_matches_the_aws_published_vector() {
        let k = signing_key(SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(
            crate::sign::hex(&k),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn full_request_signature_matches_the_aws_published_vector() {
        // GET https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08
        // (the canonical example from the AWS SigV4 documentation).
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SECRET.into(),
            region: "us-east-1".into(),
        };
        let empty_body_sha = sha256_hex(b"");
        let (auth, signature) = authorization(
            &creds,
            "iam",
            "GET",
            "/",
            &[("Action".into(), "ListUsers".into()), ("Version".into(), "2010-05-08".into())],
            &[
                ("content-type".into(), "application/x-www-form-urlencoded; charset=utf-8".into()),
                ("host".into(), "iam.amazonaws.com".into()),
                ("x-amz-date".into(), "20150830T123600Z".into()),
            ],
            &empty_body_sha,
            "20150830T123600Z",
        );
        assert_eq!(signature, "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature="), "{auth}");
    }

    #[test]
    fn uri_encode_is_rfc3986_with_optional_slash_passthrough() {
        assert_eq!(uri_encode("a b/c~d", false), "a%20b%2Fc~d");
        assert_eq!(uri_encode("a b/c~d", true), "a%20b/c~d");
    }

    #[test]
    fn presign_query_carries_the_v4_parameters_and_is_deterministic() {
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SECRET.into(),
            region: "us-east-1".into(),
        };
        let a = presign_url(&creds, "https://s3.us-east-1.amazonaws.com", "/bkt/app/k.png", 300, "20150830T123600Z");
        let b = presign_url(&creds, "https://s3.us-east-1.amazonaws.com", "/bkt/app/k.png", 300, "20150830T123600Z");
        assert_eq!(a, b, "presigning is deterministic for a fixed instant");
        for needle in [
            "X-Amz-Algorithm=AWS4-HMAC-SHA256",
            "X-Amz-Credential=AKIDEXAMPLE%2F20150830%2Fus-east-1%2Fs3%2Faws4_request",
            "X-Amz-Date=20150830T123600Z",
            "X-Amz-Expires=300",
            "X-Amz-SignedHeaders=host",
            "X-Amz-Signature=",
        ] {
            assert!(a.contains(needle), "missing {needle} in {a}");
        }
    }
}
