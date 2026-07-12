//! The S3-compatible blob store (AWS S3, Cloudflare R2, MinIO, Supabase's S3
//! endpoint), built on jerrycan's own outbound stack: hyper_util's legacy
//! client + hyper-rustls (rustls/ring, bundled webpki roots) — the identical
//! shape to jerrycan-auth's OAuth transport. Always path-style addressing:
//! `/{s3_bucket}/{app_bucket}/{key}`. Plaintext http:// endpoints are refused
//! unless loopback (the MinIO harness), mirroring the OAuth TLS-downgrade guard.

use crate::sigv4::{self, Credentials};
use crate::store::{BlobFuture, BlobStore};
use crate::xml;
use bytes::Bytes;
use http_body_util::BodyExt;
use jerrycan_core::{Error, Result};
use std::time::Duration;

/// Multipart threshold AND part size: bodies above this upload in 8 MiB parts.
const PART_SIZE: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct S3Config {
    pub bucket: String,
    pub region: String,
    pub endpoint: String, // scheme://host[:port], no trailing slash
    pub access_key: String,
    pub secret_key: String,
}

// Manual, key-material-safe Debug (mirrors `Storage`'s manual Debug in
// lib.rs): the AWS credentials must never reach logs — only the non-secret
// routing fields print.
impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl S3Config {
    /// Parse `s3://bucket?region=…&endpoint=…`; credentials are passed in
    /// (from_env reads AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY).
    pub(crate) fn from_url(
        url: &str,
        access_key: Option<String>,
        secret_key: Option<String>,
    ) -> Result<Self> {
        let rest = url.strip_prefix("s3://").ok_or_else(|| {
            Error::internal(format!("s3 config: `{url}` does not start with s3://"))
        })?;
        let (bucket, query) = rest.split_once('?').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(Error::internal(
                "s3 config: missing bucket — use s3://<bucket>?region=…",
            ));
        }
        let mut region = "us-east-1".to_string();
        let mut endpoint = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some(("region", v)) => region = v.to_string(),
                Some(("endpoint", v)) => endpoint = Some(v.trim_end_matches('/').to_string()),
                _ => {
                    return Err(Error::internal(format!(
                        "s3 config: unknown parameter `{pair}` — supported: region, endpoint"
                    )));
                }
            }
        }
        let endpoint = endpoint.unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        if !plaintext_endpoint_ok(&endpoint) {
            return Err(Error::internal(
                "s3 config: refusing a plaintext http:// endpoint to a non-loopback host — use https:// (http is allowed only for a local MinIO)",
            ));
        }
        let access_key =
            access_key.ok_or_else(|| Error::internal("s3 config: AWS_ACCESS_KEY_ID is not set"))?;
        let secret_key = secret_key
            .ok_or_else(|| Error::internal("s3 config: AWS_SECRET_ACCESS_KEY is not set"))?;
        Ok(Self {
            bucket: bucket.to_string(),
            region,
            endpoint,
            access_key,
            secret_key,
        })
    }

    /// Path-style object path, key encoded per segment (slashes kept).
    pub(crate) fn object_path(&self, app_bucket: &str, key: &str) -> String {
        format!(
            "/{}/{}/{}",
            self.bucket,
            app_bucket,
            sigv4::uri_encode(key, true)
        )
    }

    fn host(&self) -> &str {
        self.endpoint
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.endpoint)
    }

    fn credentials(&self) -> Credentials {
        Credentials {
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            region: self.region.clone(),
        }
    }
}

/// `https://` always; `http://` only to 127.0.0.1 / ::1 / localhost (the local
/// MinIO harness). Same policy as `jerrycan-auth::oauth::is_loopback_http_ok`.
fn plaintext_endpoint_ok(endpoint: &str) -> bool {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .expect("split yields one element");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(after) = authority.strip_prefix('[') {
        match after.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        }
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

type Client = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<Bytes>,
>;

/// The S3-compatible [`BlobStore`]. Construct via [`S3Store::from_url`].
pub struct S3Store {
    config: S3Config,
    client: Client,
}

impl S3Store {
    /// `s3://bucket?region=…&endpoint=…`; credentials from the standard
    /// AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY env vars.
    pub fn from_url(url: &str) -> Result<Self> {
        let config = S3Config::from_url(
            url,
            std::env::var("AWS_ACCESS_KEY_ID").ok(),
            std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
        )?;
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())
            .expect("ring provider supports rustls' safe default protocol versions")
            .https_or_http()
            .enable_http1()
            .build();
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);
        Ok(Self { config, client })
    }

    /// `YYYYMMDDTHHMMSSZ` for now — SystemTime-derived, no chrono (mirrors the
    /// epoch-millis philosophy in jerrycan-jobs).
    fn amz_datetime() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::amz_datetime_at(secs)
    }

    /// The pure core of [`Self::amz_datetime`], testable at fixed instants —
    /// a regression here breaks EVERY SigV4 signature.
    fn amz_datetime_at(secs: u64) -> String {
        // Civil-from-days (Howard Hinnant's algorithm) — correct for all dates
        // the process will ever see; leap seconds are not S3's concern.
        let days = (secs / 86_400) as i64;
        let (h, m, s) = ((secs % 86_400) / 3_600, (secs % 3_600) / 60, secs % 60);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if mo <= 2 { y + 1 } else { y };
        format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}Z")
    }

    /// One signed request. Non-2xx maps NoSuchKey/404 → not_found, everything
    /// else → an internal error carrying the parsed `<Code>: <Message>` (never
    /// the signature or credentials).
    async fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Bytes,
        content_type: Option<&str>,
    ) -> Result<(http::StatusCode, http::HeaderMap, Bytes)> {
        let datetime = Self::amz_datetime();
        let payload_hash = sigv4::sha256_hex(&body);
        let mut headers: Vec<(String, String)> = vec![
            ("host".into(), self.config.host().to_string()),
            ("x-amz-content-sha256".into(), payload_hash.clone()),
            ("x-amz-date".into(), datetime.clone()),
        ];
        if let Some(ct) = content_type {
            headers.push(("content-type".into(), ct.to_string()));
        }
        let (auth, _sig) = sigv4::authorization(
            &self.config.credentials(),
            "s3",
            method,
            path,
            query,
            &headers,
            &payload_hash,
            &datetime,
        );
        let qs = if query.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = query
                .iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        sigv4::uri_encode(k, false)
                    } else {
                        format!(
                            "{}={}",
                            sigv4::uri_encode(k, false),
                            sigv4::uri_encode(v, false)
                        )
                    }
                })
                .collect();
            format!("?{}", pairs.join("&"))
        };
        let uri = format!("{}{}{}", self.config.endpoint, path, qs);
        let mut builder = hyper::Request::builder()
            .method(method)
            .uri(&uri)
            .header("authorization", &auth);
        for (k, v) in &headers {
            if k != "host" {
                builder = builder.header(k, v);
            }
        }
        let request = builder
            .body(http_body_util::Full::new(body))
            .map_err(|e| Error::internal(format!("s3: building request failed: {e}")))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| Error::internal("s3: request to the storage endpoint failed"))?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| Error::internal("s3: reading the response body failed"))?
            .to_bytes();
        Ok((status, headers, bytes))
    }

    /// Map a non-2xx S3 response to a jerrycan error. The parsed `<Code>` /
    /// `<Message>` go to STDERR for the operator; the CLIENT gets a generic
    /// "storage error" — provider error taxonomy must not leak in 5xx bodies.
    fn s3_error(status: http::StatusCode, body: &[u8]) -> Error {
        match xml::parse_error(body) {
            // A missing BUCKET is an operator misconfiguration, not a missing
            // object: fail LOUD (500) — it must never read as an empty
            // bucket / plausible 404 while every object silently vanishes.
            Some((code, message)) if code == "NoSuchBucket" => {
                crate::store::internal_storage_error(format!(
                    "s3: {code}: {message} — the configured S3 bucket does not exist"
                ))
            }
            Some((code, _)) if code == "NoSuchKey" || status == http::StatusCode::NOT_FOUND => {
                Error::not_found()
            }
            Some((code, message)) => {
                crate::store::internal_storage_error(format!("s3: {code}: {message}"))
            }
            None if status == http::StatusCode::NOT_FOUND => Error::not_found(),
            None => crate::store::internal_storage_error(format!("s3: unexpected status {status}")),
        }
    }

    /// Multipart upload: initiate (XML UploadId) → PUT each 8 MiB part
    /// (collecting ETag headers) → complete (XML part manifest). An error at
    /// any stage aborts the upload so the bucket carries no dangling parts.
    async fn put_multipart(&self, bucket: &str, key: &str, body: Bytes, mime: &str) -> Result<()> {
        let path = self.config.object_path(bucket, key);
        let (status, _h, resp) = self
            .request(
                "POST",
                &path,
                &[("uploads".into(), String::new())],
                Bytes::new(),
                Some(mime),
            )
            .await?;
        if !status.is_success() {
            return Err(Self::s3_error(status, &resp));
        }
        let upload_id = xml::parse_upload_id(&resp)?;

        let mut parts: Vec<(usize, String)> = Vec::new();
        for (i, (start, end)) in part_ranges(body.len()).into_iter().enumerate() {
            let n = i + 1;
            let query = vec![
                ("partNumber".into(), n.to_string()),
                ("uploadId".into(), upload_id.clone()),
            ];
            let (status, headers, resp) = self
                .request("PUT", &path, &query, body.slice(start..end), None)
                .await?;
            if !status.is_success() {
                self.abort_multipart(&path, &upload_id).await;
                return Err(Self::s3_error(status, &resp));
            }
            let etag = headers
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| Error::internal("s3: UploadPart response carried no ETag"))?;
            parts.push((n, etag));
        }

        let manifest = complete_multipart_body(&parts);
        let (status, _h, resp) = self
            .request(
                "POST",
                &path,
                &[("uploadId".into(), upload_id.clone())],
                Bytes::from(manifest),
                Some("application/xml"),
            )
            .await?;
        // CompleteMultipartUpload can return 200 with an <Error> body.
        if !status.is_success() || xml::parse_error(&resp).is_some() {
            self.abort_multipart(&path, &upload_id).await;
            return Err(Self::s3_error(status, &resp));
        }
        Ok(())
    }

    /// Best-effort abort — failure here is logged, not surfaced (the original
    /// error is what the caller needs).
    async fn abort_multipart(&self, path: &str, upload_id: &str) {
        let query = vec![("uploadId".into(), upload_id.to_string())];
        if let Err(e) = self
            .request("DELETE", path, &query, Bytes::new(), None)
            .await
        {
            eprintln!("jerrycan-storage: abort multipart upload failed: {e}");
        }
    }
}

/// `(start, end)` byte ranges of PART_SIZE chunks covering `len`.
fn part_ranges(len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < len {
        out.push((start, (start + PART_SIZE).min(len)));
        start += PART_SIZE;
    }
    out
}

/// The CompleteMultipartUpload request body. ETags pass through verbatim
/// (S3 returns them quoted). Building XML is trivial string work — quick-xml
/// is for PARSING only.
fn complete_multipart_body(parts: &[(usize, String)]) -> String {
    let mut body = String::from("<CompleteMultipartUpload>");
    for (n, etag) in parts {
        body.push_str(&format!(
            "<Part><PartNumber>{n}</PartNumber><ETag>{etag}</ETag></Part>"
        ));
    }
    body.push_str("</CompleteMultipartUpload>");
    body
}

impl S3Store {
    /// Create the configured bucket if it does not exist (idempotent:
    /// BucketAlreadyOwnedByYou / BucketAlreadyExists are success). Used by the
    /// MinIO harness and first-boot provisioning.
    pub async fn ensure_bucket(&self) -> Result<()> {
        let path = format!("/{}", self.config.bucket);
        let (status, _h, resp) = self.request("PUT", &path, &[], Bytes::new(), None).await?;
        if status.is_success() {
            return Ok(());
        }
        match xml::parse_error(&resp) {
            Some((code, _))
                if code == "BucketAlreadyOwnedByYou" || code == "BucketAlreadyExists" =>
            {
                Ok(())
            }
            _ => Err(Self::s3_error(status, &resp)),
        }
    }

    /// GET an absolute URL with NO SigV4 headers — proves a presigned URL is
    /// self-authorizing. Errors on any non-2xx.
    pub async fn fetch_unauthenticated(&self, url: &str) -> Result<Bytes> {
        let request = hyper::Request::builder()
            .method("GET")
            .uri(url)
            .body(http_body_util::Full::new(Bytes::new()))
            .map_err(|e| Error::internal(format!("s3: building request failed: {e}")))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| Error::internal("s3: presigned fetch failed"))?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| Error::internal("s3: reading the response body failed"))?
            .to_bytes();
        if status.is_success() {
            Ok(bytes)
        } else {
            Err(Self::s3_error(status, &bytes))
        }
    }
}

impl BlobStore for S3Store {
    fn put<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        body: Bytes,
        mime: &'a str,
    ) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            crate::store::validate_key(key)?;
            if body.len() > PART_SIZE {
                return self.put_multipart(bucket, key, body, mime).await; // Task 12
            }
            let path = self.config.object_path(bucket, key);
            let (status, _h, resp) = self.request("PUT", &path, &[], body, Some(mime)).await?;
            if status.is_success() {
                Ok(())
            } else {
                Err(Self::s3_error(status, &resp))
            }
        })
    }

    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, Bytes> {
        Box::pin(async move {
            let path = self.config.object_path(bucket, key);
            let (status, _h, resp) = self.request("GET", &path, &[], Bytes::new(), None).await?;
            if status.is_success() {
                Ok(resp)
            } else {
                Err(Self::s3_error(status, &resp))
            }
        })
    }

    fn delete<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            let path = self.config.object_path(bucket, key);
            let (status, _h, resp) = self
                .request("DELETE", &path, &[], Bytes::new(), None)
                .await?;
            // 204 success; 404 is idempotent-ok (the metadata row is the truth).
            if status.is_success() || status == http::StatusCode::NOT_FOUND {
                Ok(())
            } else {
                Err(Self::s3_error(status, &resp))
            }
        })
    }

    fn presign_get<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        ttl: Duration,
    ) -> BlobFuture<'a, Option<String>> {
        Box::pin(async move {
            let path = self.config.object_path(bucket, key);
            Ok(Some(sigv4::presign_url(
                &self.config.credentials(),
                &self.config.endpoint,
                &path,
                ttl.as_secs().max(1),
                &Self::amz_datetime(),
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str) -> Result<S3Config, jerrycan_core::Error> {
        S3Config::from_url(url, Some("ak".into()), Some("sk".into()))
    }

    #[test]
    fn config_parses_bucket_region_and_endpoint() {
        let c = cfg("s3://my-bucket?region=eu-central-1&endpoint=https://minio.example.com:9000")
            .unwrap();
        assert_eq!(c.bucket, "my-bucket");
        assert_eq!(c.region, "eu-central-1");
        assert_eq!(c.endpoint, "https://minio.example.com:9000");
        // Defaults: us-east-1 + the AWS regional endpoint, derived from region.
        let d = cfg("s3://my-bucket").unwrap();
        assert_eq!(d.region, "us-east-1");
        assert_eq!(d.endpoint, "https://s3.us-east-1.amazonaws.com");
    }

    #[test]
    fn config_requires_credentials_and_a_bucket() {
        let err = S3Config::from_url("s3://b", None, Some("sk".into())).unwrap_err();
        assert!(err.message().contains("AWS_ACCESS_KEY_ID"), "{err}");
        let err = cfg("s3://?region=x").unwrap_err();
        assert!(err.message().contains("bucket"), "{err}");
    }

    #[test]
    fn plaintext_endpoints_are_loopback_only() {
        // WHY: an http:// endpoint ships the SigV4-authorized payload in
        // cleartext — allowed only for the local MinIO harness (same
        // TLS-downgrade guard as jerrycan-auth's OAuth transport).
        assert!(cfg("s3://b?endpoint=http://127.0.0.1:9000").is_ok());
        assert!(cfg("s3://b?endpoint=http://localhost:9000").is_ok());
        let err = cfg("s3://b?endpoint=http://minio.internal:9000").unwrap_err();
        assert!(err.message().contains("plaintext"), "{err}");
        assert!(cfg("s3://b?endpoint=https://minio.internal:9000").is_ok());
    }

    #[test]
    fn object_paths_are_path_style_and_segment_encoded() {
        let c = cfg("s3://my-bucket?endpoint=https://x.example.com").unwrap();
        assert_eq!(
            c.object_path("avatars", "u 1/pic.png"),
            "/my-bucket/avatars/u%201/pic.png"
        );
    }

    #[test]
    fn amz_datetime_formats_known_instants() {
        // WHY: this string is signed into EVERY SigV4 request — a formatting
        // or calendar regression invalidates all S3 auth, and only the
        // env-gated MinIO suite would otherwise notice.
        assert_eq!(S3Store::amz_datetime_at(0), "19700101T000000Z");
        // Leap day (2000 IS a leap year despite the century rule).
        assert_eq!(S3Store::amz_datetime_at(951_782_400), "20000229T000000Z");
        // An arbitrary modern instant with a non-midnight time component.
        assert_eq!(S3Store::amz_datetime_at(1_700_000_000), "20231114T221320Z");
        // 2100 is NOT a leap year: Feb 28 + 1 day must be Mar 1.
        assert_eq!(
            S3Store::amz_datetime_at(4_102_444_800 + 59 * 86_400),
            "21000301T000000Z"
        );
    }

    #[test]
    fn config_debug_redacts_both_credentials() {
        // WHY (security): S3Config reaches error/trace contexts — a derived
        // Debug would print live AWS keys into logs.
        let c = S3Config::from_url(
            "s3://b?region=eu-central-1",
            Some("AKIA-PLAINTEXT-ACCESS".into()),
            Some("PLAINTEXT-SECRET-KEY".into()),
        )
        .unwrap();
        let dbg = format!("{c:?}");
        assert!(
            !dbg.contains("AKIA-PLAINTEXT-ACCESS") && !dbg.contains("PLAINTEXT-SECRET-KEY"),
            "credentials must never print: {dbg}"
        );
        assert!(dbg.contains("<redacted>"), "{dbg}");
        assert!(dbg.contains("eu-central-1"), "routing fields do print: {dbg}");
    }

    #[test]
    fn s3_error_bodies_are_generic_to_the_client() {
        // WHY (security): the message is the client-visible 5xx body — the
        // provider's error taxonomy (<Code>/<Message>, which can carry ARNs,
        // hostnames, request ids) stays on stderr for the operator.
        let body = b"<Error><Code>AccessDenied</Code><Message>arn:aws:s3:::prod-secrets denied</Message></Error>";
        let err = S3Store::s3_error(http::StatusCode::FORBIDDEN, body);
        assert_eq!(err.code(), "JC0500");
        assert_eq!(err.message(), "storage error");
        // NotFound mapping is untouched: a missing KEY is the caller's 404.
        let nf = S3Store::s3_error(
            http::StatusCode::NOT_FOUND,
            b"<Error><Code>NoSuchKey</Code><Message>gone</Message></Error>",
        );
        assert_eq!(nf.code(), "JC0404");
    }

    #[test]
    fn missing_bucket_fails_loud_instead_of_reading_as_404() {
        // WHY: NoSuchBucket arrives with HTTP 404 — mapping it to not_found
        // makes a misconfigured JERRYCAN_STORAGE bucket look like empty
        // results/missing objects forever. It is an operator fault: 500.
        let err = S3Store::s3_error(
            http::StatusCode::NOT_FOUND,
            b"<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>",
        );
        assert_eq!(err.code(), "JC0500", "{err}");
        assert_eq!(err.message(), "storage error", "still generic to clients");
    }

    #[test]
    fn part_split_covers_the_body_exactly() {
        // 20 MiB → 8 + 8 + 4.
        let chunks = part_ranges(20 * 1024 * 1024);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], (0, 8 * 1024 * 1024));
        assert_eq!(chunks[1], (8 * 1024 * 1024, 16 * 1024 * 1024));
        assert_eq!(chunks[2], (16 * 1024 * 1024, 20 * 1024 * 1024));
        // Exactly one part size → a single range (multipart not even entered).
        assert_eq!(part_ranges(8 * 1024 * 1024), vec![(0, 8 * 1024 * 1024)]);
    }

    #[test]
    fn complete_body_lists_parts_in_order_with_their_etags() {
        let body = complete_multipart_body(&[(1, "\"etag-a\"".into()), (2, "\"etag-b\"".into())]);
        assert_eq!(
            body,
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"etag-a\"</ETag></Part><Part><PartNumber>2</PartNumber><ETag>\"etag-b\"</ETag></Part></CompleteMultipartUpload>"
        );
    }
}
