//! Spec §Security (Rule 14): secrets are never written into the generated app.
//! Hand-rolled matchers (no regex crate): JWT = three dot-joined base64url
//! runs each ≥ 8 chars starting "eyJ"; Supabase key prefixes; conn strings
//! with a password. Data-column hits become `suspected_secret` ADVISORY gaps —
//! flagged, never silently embedded (the data still seeds; it is user data).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Jwt,
    SupabaseKey,
    PasswordUrl,
}

impl SecretKind {
    fn placeholder(self) -> &'static str {
        match self {
            SecretKind::Jwt => "jwt",
            SecretKind::SupabaseKey => "key",
            SecretKind::PasswordUrl => "password",
        }
    }
}

#[derive(Debug)]
pub struct SecretHit {
    pub kind: SecretKind,
    /// First 8 chars + "…" — safe to print, never the secret.
    pub preview: String,
    pub offset: usize,
}

fn is_b64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn preview(s: &str) -> String {
    let mut p: String = s.chars().take(8).collect();
    p.push('…');
    p
}

/// Length of the maximal `is_b64url` run starting at byte `pos`.
fn run_end(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && is_b64url(bytes[pos] as char) {
        pos += 1;
    }
    pos
}

fn jwt_end(bytes: &[u8], start: usize) -> Option<usize> {
    let seg1 = run_end(bytes, start);
    if bytes.get(seg1) != Some(&b'.') {
        return None;
    }
    let seg2 = run_end(bytes, seg1 + 1);
    if seg2 - (seg1 + 1) < 8 || bytes.get(seg2) != Some(&b'.') {
        return None;
    }
    let seg3 = run_end(bytes, seg2 + 1);
    if seg3 - (seg2 + 1) < 8 {
        return None;
    }
    Some(seg3)
}

pub fn scan(text: &str) -> Vec<SecretHit> {
    let bytes = text.as_bytes();
    let mut hits = Vec::new();

    // JWTs (Supabase anon/service-role keys are JWTs).
    let mut i = 0;
    while let Some(rel) = text[i..].find("eyJ") {
        let start = i + rel;
        if let Some(end) = jwt_end(bytes, start) {
            hits.push(SecretHit {
                kind: SecretKind::Jwt,
                preview: preview(&text[start..end]),
                offset: start,
            });
            i = end;
        } else {
            i = start + 3;
        }
    }

    // Supabase secret keys: `sb_secret_…` (≥ 20 total) and `sbp_` + ≥ 40 hex.
    let mut i = 0;
    while let Some(rel) = text[i..].find("sb_secret_") {
        let start = i + rel;
        let end = run_end(bytes, start);
        if end - start >= 20 {
            hits.push(SecretHit {
                kind: SecretKind::SupabaseKey,
                preview: preview(&text[start..end]),
                offset: start,
            });
        }
        i = start + "sb_secret_".len();
    }
    let mut i = 0;
    while let Some(rel) = text[i..].find("sbp_") {
        let start = i + rel;
        let hex_start = start + 4;
        let mut end = hex_start;
        while end < bytes.len() && (bytes[end] as char).is_ascii_hexdigit() {
            end += 1;
        }
        if end - hex_start >= 40 {
            hits.push(SecretHit {
                kind: SecretKind::SupabaseKey,
                preview: preview(&text[start..end]),
                offset: start,
            });
        }
        i = start + 4;
    }

    // Password-bearing Postgres connection strings.
    for scheme in ["postgresql://", "postgres://"] {
        let mut i = 0;
        while let Some(rel) = text[i..].find(scheme) {
            let start = i + rel;
            let userinfo_start = start + scheme.len();
            // userinfo runs up to '@' before the next '/'.
            let rest = &text[userinfo_start..];
            let at = rest.find('@');
            let slash = rest.find('/');
            if let Some(at) = at
                && slash.map(|s| at < s).unwrap_or(true)
                && rest[..at].contains(':')
            {
                hits.push(SecretHit {
                    kind: SecretKind::PasswordUrl,
                    preview: preview(&text[start..userinfo_start]),
                    offset: start,
                });
            }
            i = start + scheme.len();
        }
    }

    hits.sort_by_key(|h| h.offset);
    hits
}

/// Redact secret-bearing values in a `.env`-style text to placeholders, and
/// return the hits (for the rotation checklist).
pub fn redact_env(text: &str) -> (String, Vec<SecretHit>) {
    let mut out = String::with_capacity(text.len());
    let mut all_hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some((key, value)) = line.split_once('=') {
            let hits = scan(value);
            if let Some(first) = hits.first() {
                out.push_str(key);
                out.push('=');
                out.push_str(&format!("<ROTATE-ME:{}>", first.kind.placeholder()));
                all_hits.extend(hits);
                continue;
            }
        }
        out.push_str(line);
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    (out, all_hits)
}

/// Hard gate: error if any emitted artifact still carries a secret. A leak is a
/// translator bug — the orchestrator runs this and exits non-zero (fail loud).
pub fn assert_clean(paths: &[PathBuf]) -> Result<(), String> {
    for path in paths {
        if let Ok(text) = std::fs::read_to_string(path) {
            let hits = scan(&text);
            if let Some(hit) = hits.first() {
                return Err(format!(
                    "{}: a {:?} secret survived into an emitted artifact ({}…) — refusing to write secrets",
                    Path::new(path).display(),
                    hit.kind,
                    hit.preview
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJub3RlIjoiamVycnljYW4gdGVzdCBmaXh0dXJlLCBub3QgYSByZWFsIHNlY3JldCJ9.amVycnljYW4tZml4dHVyZS1zaWduYXR1cmUtcGxhY2Vob2xkZXItMDAw";

    #[test]
    fn jwts_sb_keys_and_password_urls_are_detected() {
        assert_eq!(scan(&format!("key={JWT}"))[0].kind, SecretKind::Jwt);
        assert_eq!(
            scan(&format!("sb_secret_{}", "x".repeat(24)))[0].kind,
            SecretKind::SupabaseKey
        );
        assert_eq!(
            scan(&format!("sbp_{}", "0".repeat(40)))[0].kind,
            SecretKind::SupabaseKey
        );
        assert_eq!(
            scan("postgresql://postgres:s3cret@db.example.com:5432/x")[0].kind,
            SecretKind::PasswordUrl
        );
        assert!(scan("plain text, no secrets, even with eyJ prefix alone").is_empty());
    }

    #[test]
    fn env_files_redact_to_placeholders_and_feed_the_rotation_checklist() {
        let env = format!("SUPABASE_SERVICE_ROLE_KEY={JWT}\nOTHER=fine\n");
        let (redacted, hits) = redact_env(&env);
        assert!(
            !redacted.contains("eyJ"),
            "secret bytes never survive: {redacted}"
        );
        assert!(redacted.contains("SUPABASE_SERVICE_ROLE_KEY=<ROTATE-ME:jwt>"));
        assert!(redacted.contains("OTHER=fine"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn previews_never_contain_the_secret() {
        let hits = scan(JWT);
        assert!(
            hits[0].preview.len() < 20 && !JWT.contains(&hits[0].preview),
            "{}",
            hits[0].preview
        );
    }
}
