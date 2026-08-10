//! Live round-trip of the media object store against the configured
//! `MEDIA_S3_*` bucket. Ignored by default; run with the media environment set.

use std::time::Duration;

use meridian_platform::MediaStore;

#[tokio::test]
#[ignore = "requires MEDIA_S3_* environment and a reachable bucket"]
async fn media_store_round_trips_an_object() {
    let store = MediaStore::from_env()
        .await
        .expect("media store from environment");
    assert!(store.is_ready().await, "media bucket should be reachable");

    let key = format!("probes/{}.txt", uuid::Uuid::now_v7());
    let payload = b"meridian-media-round-trip-probe".to_vec();

    store
        .upload(&key, "text/plain", payload.clone())
        .await
        .expect("upload probe object");

    let listed = store.list_objects("probes/").await.expect("list objects");
    assert!(listed.contains(&key), "uploaded object appears in the listing");

    let url = store
        .presign_get(&key, Duration::from_mins(1))
        .await
        .expect("presign probe object");
    assert!(url.contains("X-Amz-Signature="), "presigned URL is signed");

    // Fetch the presigned URL and confirm the bytes match.
    let fetched = reqwest_get(&url).await;
    assert_eq!(fetched, payload, "downloaded bytes match uploaded bytes");

    store.delete(&key).await.expect("delete probe object");
}

/// Minimal GET using the process's TLS-less HTTP: the presigned URL points at a
/// local, plaintext `RustFS` endpoint, so a raw TCP request keeps the test free of
/// an HTTP-client dependency.
async fn reqwest_get(url: &str) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let without_scheme = url.strip_prefix("http://").expect("plaintext presign URL");
    let (authority, path_and_query) = without_scheme.split_once('/').expect("URL has a path");
    let host = authority.split(':').next().expect("host");
    let port: u16 = authority
        .split(':')
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(80);
    let mut stream = tokio::net::TcpStream::connect((host, port))
        .await
        .expect("connect to media endpoint");
    let request = format!(
        "GET /{path_and_query} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send GET");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    // Split headers from body on the blank line.
    let separator = b"\r\n\r\n";
    let body_start = response
        .windows(separator.len())
        .position(|window| window == separator)
        .map_or(response.len(), |index| index + separator.len());
    response[body_start..].to_vec()
}
