use std::sync::Arc;

use konedrive_graph::token::StaticToken;
use serde_json::json;
use tokio::io::AsyncReadExt;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

/// A Graph that answers the metadata of the item `A1` with `item` and its bytes with
/// `bytes`, and the source over it.
async fn serve(mut item: serde_json::Value, bytes: &[u8]) -> (MockServer, GraphSource) {
    let server = MockServer::start().await;
    item["id"] = json!("A1");
    item["@microsoft.graph.downloadUrl"] = json!(format!("{}/bytes-of-a1", server.uri()));
    Mock::given(method("GET")).and(path_regex("/items/A1$")).respond_with(ResponseTemplate::new(200).set_body_json(item)).mount(&server).await;
    Mock::given(method("GET")).and(path("/bytes-of-a1")).respond_with(ResponseTemplate::new(200).set_body_bytes(bytes)).mount(&server).await;
    let base = url::Url::parse(&format!("{}/", server.uri())).unwrap();
    let drive = DriveClient::new(base, Arc::new(StaticToken::new("T"))).unwrap();
    (server, GraphSource::new(drive))
}

async fn read_whole(fetched: Fetched) -> Vec<u8> {
    let (mut stream, mut bytes) = (fetched.stream, Vec::new());
    stream.read_to_end(&mut bytes).await.unwrap();
    bytes
}

fn opened(ctag: &str) -> konedrive_tree::ContentVersion {
    konedrive_tree::ContentVersion::Hashed { size: 5, mtime: 0, mtime_nsec: 0, hash: None, ctag: Some(ctag.into()) }
}

#[tokio::test]
async fn the_size_of_an_answer_is_the_one_onedrive_said_and_none_when_it_said_none() {
    // The metadata says a size: that one.
    let (_server, source) = serve(json!({"file": {}, "size": 5, "cTag": "c1"}), b"bytes").await;
    let fetched = source.fetch("A1", 0, None).await.ok().expect("the item is answered");
    assert_eq!(fetched.size, Some(5));
    assert_eq!(read_whole(fetched).await, b"bytes");

    // An empty file: OneDrive says 0, which is a size.
    let (_server, source) = serve(json!({"file": {}, "size": 0, "cTag": "c2"}), b"").await;
    assert_eq!(source.fetch("A1", 0, None).await.ok().expect("the item is answered").size, Some(0));

    // The metadata says none: not said, which is not an empty file.
    let (_server, source) = serve(json!({"file": {}, "cTag": "c1"}), b"bytes").await;
    assert_eq!(source.fetch("A1", 0, None).await.ok().expect("the item is answered").size, None, "a size OneDrive did not say");

    // A range carries the metadata version against which its bytes were checked.
    let (_server, source) = serve(json!({"file": {}, "size": 5, "cTag": "c1"}), b"bytes").await;
    assert_eq!(source.read_range("A1", &opened("c1"), 0, 5).await.ok().expect("the range is answered").size, Some(5));
}

#[tokio::test]
async fn a_hash_is_kept_without_a_ctag_and_hashless_graph_is_still_a_remote_version() {
    let hash = crate::fake_onedrive::qx(b"bytes");
    let (_server, source) = serve(json!({"file": {"hashes": {"quickXorHash": hash}}, "size": 5}), b"bytes").await;
    let version = source.fetch("A1", 0, None).await.unwrap().version.unwrap();
    assert!(version.ctag.is_empty());
    assert_eq!(version.quick_xor, decode_base64(&hash));
    let (_server, source) = serve(json!({"file": {}, "size": 5}), b"bytes").await;
    assert!(source.fetch("A1", 0, None).await.unwrap().version.is_some(), "no hash must reach the fail-closed remote verifier");
    assert!(source.read_range("A1", &opened("c1"), 0, 2).await.is_err(), "no version to check ranges against");
    let (_server, source) = serve(json!({"file": {}, "size": 5, "cTag":"", "eTag":""}), b"bytes").await;
    assert!(source.read_range("A1", &opened("c1"), 0, 2).await.is_err(), "empty tags are no version either");

}

#[tokio::test]
async fn ranges_refuse_a_changed_metadata_version_or_download_etag() {
    let (server, source) = serve(json!({"file": {}, "size": 5, "cTag": "c1"}), b"bytes").await;
    assert_eq!(read_whole(source.read_range("A1", &opened("c1"), 0, 2).await.unwrap()).await, b"by");
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"A1", "file":{}, "size":5, "cTag":"c1", "eTag":"renamed", "@microsoft.graph.downloadUrl":format!("{}/bytes-of-a1", server.uri())})))
        .with_priority(2).mount(&server).await;
    assert_eq!(read_whole(source.read_range("A1", &opened("c1"), 2, 5).await.unwrap()).await, b"tes", "a metadata-only eTag change preserves the cTag's content");
    Mock::given(method("GET")).and(path("/bytes-of-a1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "another-endpoint-version").set_body_bytes(b"other"))
        .with_priority(1).mount(&server).await;
    assert!(matches!(source.read_range("A1", &opened("c1"), 2, 5).await, Err(SourceError::NotFound(_))), "same metadata, another endpoint version");
    source.forget("A1");
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "A1", "file": {}, "size": 5, "cTag": "c2", "@microsoft.graph.downloadUrl": format!("{}/bytes-of-a1", server.uri())})))
        .with_priority(1).mount(&server).await;
    assert!(source.read_range("A1", &opened("c1"), 0, 2).await.is_ok(), "a listing can forget the old version before a new handle reads");
}

#[tokio::test]
async fn a_range_that_changes_during_its_body_is_never_answered() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (server, source) = serve(json!({"file": {}, "size": 5, "cTag": "unused"}), b"bytes").await;
    let count = Arc::new(AtomicUsize::new(0));
    let url = format!("{}/bytes-of-a1", server.uri());
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(move |_: &wiremock::Request| {
            let ctag = if count.fetch_add(1, Ordering::SeqCst) == 0 { "c1" } else { "c2" };
            ResponseTemplate::new(200).set_body_json(json!({"id":"A1", "file":{}, "size":5, "cTag":ctag, "@microsoft.graph.downloadUrl":url}))
        }).with_priority(1).mount(&server).await;
    assert!(matches!(source.read_range("A1", &opened("c1"), 0, 2).await, Err(SourceError::NotFound(_))));
}

#[tokio::test]
async fn a_new_content_generation_does_not_inherit_an_earlier_ranges_version() {
    let (server, source) = serve(json!({"file": {}, "size": 5, "cTag": "c1"}), b"bytes").await;
    assert_eq!(read_whole(source.read_range("A1", &opened("c1"), 0, 2).await.unwrap()).await, b"by");
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"A1", "file":{}, "size":5, "cTag":"c2", "@microsoft.graph.downloadUrl":format!("{}/bytes-of-a1", server.uri())})))
        .with_priority(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/bytes-of-a1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"other"))
        .with_priority(1).mount(&server).await;
    assert!(matches!(source.read_range("A1", &opened("c1"), 2, 5).await, Err(SourceError::NotFound(_))), "an old reader cannot switch versions");
    assert_eq!(read_whole(source.read_range("A1", &opened("c2"), 0, 2).await.unwrap()).await, b"ot", "the reconciled content is readable without an explicit forget of the former binding");
    assert!(matches!(source.read_range("A1", &opened("c1"), 2, 5).await, Err(SourceError::NotFound(_))), "the new generation never resets an old reader's identity");
    assert_eq!(read_whole(source.read_range("A1", &opened("c2"), 2, 5).await.unwrap()).await, b"her");
}
