//! quick-xml parsing for the ONLY two S3 XML shapes jerrycan reads: error
//! bodies (`<Error><Code>/<Message>`) and `InitiateMultipartUploadResult`
//! (`<UploadId>`). Listing is DB-backed, so object keys never round-trip
//! through XML. Tested against AWS, MinIO, and R2 body variants.

use jerrycan_core::{Error, Result};
use quick_xml::Reader;
use quick_xml::events::Event;

/// Pull the text content of every element named in `wanted`, in document
/// order, tolerating unknown siblings and namespace attributes.
fn texts_of(body: &[u8], wanted: &[&str]) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                current = wanted.contains(&name.as_str()).then_some(name);
            }
            Ok(Event::Text(t)) => {
                // quick-xml 0.37 `BytesText` exposes `unescape()` (not `decode()`);
                // it both decodes and resolves XML entities in `<Message>` text.
                if let (Some(name), Ok(text)) = (current.take(), t.unescape()) {
                    out.push((name, text.into_owned()));
                }
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// `(Code, Message)` from an S3 error body, or `None` when the body is not an
/// S3 error document (the caller reports the raw HTTP status instead).
pub(crate) fn parse_error(body: &[u8]) -> Option<(String, String)> {
    let found = texts_of(body, &["Code", "Message"]);
    let code = found.iter().find(|(k, _)| k == "Code")?.1.clone();
    let message = found
        .iter()
        .find(|(k, _)| k == "Message")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    Some((code, message))
}

/// The `UploadId` from an InitiateMultipartUploadResult body — loud when absent.
pub(crate) fn parse_upload_id(body: &[u8]) -> Result<String> {
    texts_of(body, &["UploadId"])
        .into_iter()
        .next()
        .map(|(_, v)| v)
        .ok_or_else(|| Error::internal("s3: InitiateMultipartUpload response carried no UploadId"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aws_minio_and_r2_error_bodies() {
        // AWS S3 (extra elements after Message).
        let aws = br#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>a.png</Key><RequestId>ABC123</RequestId><HostId>host==</HostId></Error>"#;
        assert_eq!(
            parse_error(aws),
            Some(("NoSuchKey".into(), "The specified key does not exist.".into()))
        );
        // MinIO (adds BucketName/Resource/Region).
        let minio = br#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>AccessDenied</Code><Message>Access Denied.</Message><BucketName>jc</BucketName><Resource>/jc/x</Resource><RequestId>17</RequestId><HostId>h</HostId></Error>"#;
        assert_eq!(parse_error(minio), Some(("AccessDenied".into(), "Access Denied.".into())));
        // R2 (minimal body, no xml declaration).
        let r2 = br#"<Error><Code>InternalError</Code><Message>We encountered an internal error.</Message></Error>"#;
        assert_eq!(parse_error(r2), Some(("InternalError".into(), "We encountered an internal error.".into())));
        // Not an error document at all → None (caller falls back to status text).
        assert_eq!(parse_error(b"not xml"), None);
        assert_eq!(parse_error(b"<Ok/>"), None);
    }

    #[test]
    fn parses_initiate_multipart_upload_id_with_and_without_xmlns() {
        // AWS emits xmlns; MinIO does too; the parser must not care.
        let aws = br#"<?xml version="1.0" encoding="UTF-8"?>
<InitiateMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Bucket>jc</Bucket><Key>app/k.bin</Key><UploadId>VXBsb2FkIElE</UploadId></InitiateMultipartUploadResult>"#;
        assert_eq!(parse_upload_id(aws).unwrap(), "VXBsb2FkIElE");
        let bare = br#"<InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key><UploadId>u-1</UploadId></InitiateMultipartUploadResult>"#;
        assert_eq!(parse_upload_id(bare).unwrap(), "u-1");
        // A body with no UploadId is a loud error, never an empty string.
        assert!(parse_upload_id(b"<InitiateMultipartUploadResult></InitiateMultipartUploadResult>").is_err());
    }
}
