//! Live integration against a MinIO container. Gated on JERRYCAN_TEST_S3 so
//! `cargo test` stays hermetic by default. To run locally:
//!   docker run --rm -d -p 9000:9000 --name jc-minio minio/minio server /data
//!   AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin \
//!   JERRYCAN_TEST_S3='s3://jerrycan-test?region=us-east-1&endpoint=http://127.0.0.1:9000' \
//!   cargo test -p jerrycan-storage --features storage-s3 --test s3_minio
#![cfg(feature = "storage-s3")]

use bytes::Bytes;
use jerrycan_storage::{BlobStore, S3Store};
use std::time::Duration;

fn store() -> Option<S3Store> {
    let Ok(url) = std::env::var("JERRYCAN_TEST_S3") else {
        eprintln!("SKIP s3_minio: JERRYCAN_TEST_S3 not set (see file header to run against MinIO)");
        return None;
    };
    Some(S3Store::from_url(&url).expect("JERRYCAN_TEST_S3 must parse"))
}

#[tokio::test]
async fn single_shot_round_trip_and_idempotent_delete() {
    let Some(s) = store() else { return };
    s.ensure_bucket().await.expect("bucket exists");
    s.put("it", "small/a.txt", Bytes::from_static(b"hello minio"), "text/plain").await.unwrap();
    assert_eq!(s.get("it", "small/a.txt").await.unwrap(), Bytes::from_static(b"hello minio"));
    s.delete("it", "small/a.txt").await.unwrap();
    assert_eq!(s.get("it", "small/a.txt").await.unwrap_err().code(), "JC0404");
    s.delete("it", "small/a.txt").await.unwrap(); // idempotent
}

#[tokio::test]
async fn multipart_round_trips_a_20mib_body() {
    let Some(s) = store() else { return };
    s.ensure_bucket().await.expect("bucket exists");
    // > PART_SIZE forces initiate/parts/complete; a byte pattern (not zeros)
    // catches part reordering.
    let body: Vec<u8> = (0..20 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    s.put("it", "big/blob.bin", Bytes::from(body.clone()), "application/octet-stream").await.unwrap();
    let got = s.get("it", "big/blob.bin").await.unwrap();
    assert_eq!(got.len(), body.len());
    assert_eq!(&got[..], &body[..], "multipart reassembly is byte-exact");
    s.delete("it", "big/blob.bin").await.unwrap();
}

#[tokio::test]
async fn presigned_get_url_is_fetchable_without_credentials() {
    let Some(s) = store() else { return };
    s.ensure_bucket().await.expect("bucket exists");
    s.put("it", "signed/x.txt", Bytes::from_static(b"presigned"), "text/plain").await.unwrap();
    let url = s
        .presign_get("it", "signed/x.txt", Duration::from_secs(120))
        .await
        .unwrap()
        .expect("s3 backend presigns natively");
    let bytes = s.fetch_unauthenticated(&url).await.expect("presigned fetch");
    assert_eq!(bytes, Bytes::from_static(b"presigned"));
    // A tampered signature is refused by the SERVER (403), proving the
    // signature is load-bearing, not decorative.
    assert!(s.fetch_unauthenticated(&format!("{url}0")).await.is_err());
    s.delete("it", "signed/x.txt").await.unwrap();
}
