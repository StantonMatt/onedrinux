use std::sync::Arc;

use konedrive_graph::token::StaticToken;
use serde_json::json;
use tokio::io::AsyncReadExt;
use wiremock::matchers::{method, path, path_regex};
use konedrive_graph::test_server::MockServer;
use wiremock::{Mock, ResponseTemplate};

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
    assert_eq!(source.read_range("A1", &opened("c1"), None, 0, 5).await.ok().expect("the range is answered").size, Some(5));
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
    assert!(source.read_range("A1", &opened("c1"), None, 0, 2).await.is_err(), "no version to check ranges against");
    let (_server, source) = serve(json!({"file": {}, "size": 5, "cTag":"", "eTag":""}), b"bytes").await;
    assert!(source.read_range("A1", &opened("c1"), None, 0, 2).await.is_err(), "empty tags are no version either");

}

#[tokio::test]
async fn ranges_refuse_a_changed_metadata_version_or_download_etag() {
    let (server, source) = serve(json!({"file": {}, "size": 5, "cTag": "c1"}), b"bytes").await;
    assert_eq!(read_whole(source.read_range("A1", &opened("c1"), None, 0, 2).await.unwrap()).await, b"by");
    source.read_blocks.clear();
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"A1", "file":{}, "size":5, "cTag":"c1", "eTag":"renamed", "@microsoft.graph.downloadUrl":format!("{}/bytes-of-a1", server.uri())})))
        .with_priority(2).mount(&server).await;
    assert_eq!(read_whole(source.read_range("A1", &opened("c1"), None, 2, 5).await.unwrap()).await, b"tes", "a metadata-only eTag change preserves the cTag's content");
    source.read_blocks.clear();
    Mock::given(method("GET")).and(path("/bytes-of-a1"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "another-endpoint-version").set_body_bytes(b"other"))
        .with_priority(1).mount(&server).await;
    assert!(matches!(source.read_range("A1", &opened("c1"), None, 2, 5).await, Err(SourceError::NotFound(_))), "same metadata, another endpoint version");
    source.forget("A1");
    source.read_blocks.clear();
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "A1", "file": {}, "size": 5, "cTag": "c2", "@microsoft.graph.downloadUrl": format!("{}/bytes-of-a1", server.uri())})))
        .with_priority(1).mount(&server).await;
    assert!(source.read_range("A1", &opened("c1"), None, 0, 2).await.is_ok(), "a listing can forget the old version before a new handle reads");
}

#[tokio::test]
async fn a_range_that_changes_during_its_body_is_never_answered() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (server, source) = serve(json!({"file": {}, "size": 5, "cTag": "unused"}), b"bytes").await;
    let count = Arc::new(AtomicUsize::new(0));
    let url = format!("{}/bytes-of-a1", server.uri());
    source.read_blocks.clear();
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(move |_: &wiremock::Request| {
            let ctag = if count.fetch_add(1, Ordering::SeqCst) == 0 { "c1" } else { "c2" };
            ResponseTemplate::new(200).set_body_json(json!({"id":"A1", "file":{}, "size":5, "cTag":ctag, "@microsoft.graph.downloadUrl":url}))
        }).with_priority(1).mount(&server).await;
    assert!(matches!(source.read_range("A1", &opened("c1"), None, 0, 2).await, Err(SourceError::NotFound(_))));
}

#[tokio::test]
async fn a_new_content_generation_does_not_inherit_an_earlier_ranges_version() {
    let (server, source) = serve(json!({"file": {}, "size": 5, "cTag": "c1"}), b"bytes").await;
    assert_eq!(read_whole(source.read_range("A1", &opened("c1"), None, 0, 2).await.unwrap()).await, b"by");
    source.read_blocks.clear();
    Mock::given(method("GET")).and(path_regex("/items/A1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"A1", "file":{}, "size":5, "cTag":"c2", "@microsoft.graph.downloadUrl":format!("{}/bytes-of-a1", server.uri())})))
        .with_priority(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/bytes-of-a1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"other"))
        .with_priority(1).mount(&server).await;
    assert!(matches!(source.read_range("A1", &opened("c1"), None, 2, 5).await, Err(SourceError::NotFound(_))), "an old reader cannot switch versions");
    assert_eq!(read_whole(source.read_range("A1", &opened("c2"), None, 0, 2).await.unwrap()).await, b"ot", "the reconciled content is readable without an explicit forget of the former binding");
    assert!(matches!(source.read_range("A1", &opened("c1"), None, 2, 5).await, Err(SourceError::NotFound(_))), "the new generation never resets an old reader's identity");
    assert_eq!(read_whole(source.read_range("A1", &opened("c2"), None, 2, 5).await.unwrap()).await, b"her");
}

// Ranges by the listed version, over the fake OneDrive: its download URLs are the item's,
// not one version's, and answer with the item's eTag and `Content-Range`, as OneDrive's
// were measured to (2026-10-29).

use crate::fake_onedrive::{FakeGraph, FakeItem, ROOT};

/// Bytes `size` long, of a file `n` of a folder: no two files the same.
fn bytes_of(n: usize, size: usize) -> Vec<u8> {
    (0..size).map(|at| (at * 7 + n * 13 + at / 65536) as u8).collect()
}

/// A fake OneDrive with the folder `F` holding `files` files `P0`, `P1`… of `size` bytes,
/// and a source over it.
async fn folder_of(files: usize, size: usize) -> (FakeGraph, GraphSource) {
    let graph = FakeGraph::start().await;
    graph.with(|cloud| {
        cloud.add(FakeItem { id: "F".into(), parent: Some(ROOT.into()), name: "Screenshots".into(), folder: true, content: Vec::new(), hash: None, size: 0, etag: "e-F".into(), ctag: String::new(), mtime: 0 });
        for n in 0..files {
            cloud.add_file(&format!("P{n}"), "F", &format!("{n}.png"), &bytes_of(n, size));
        }
    });
    let source = GraphSource::new(graph.client());
    (graph, source)
}

/// The content of `id` as a program opens it now, and what the store says OneDrive listed
/// of it.
fn opened_now(graph: &FakeGraph, id: &str) -> (konedrive_tree::ContentVersion, ListedContent) {
    graph.with(|cloud| {
        let item = cloud.item(id).expect("the file");
        let content = konedrive_tree::ContentVersion::Hashed { size: item.size, mtime: 0, mtime_nsec: 0, hash: item.hash.clone(), ctag: Some(item.ctag.clone()) };
        (content, ListedContent { etag: item.etag.clone(), size: item.size, folder_id: item.parent.clone() })
    })
}

/// The requests since the last call, as `(kind, item)`: `children` of a folder, `item`
/// metadata, `bytes` through a download URL.
fn requests_since(graph: &FakeGraph) -> Vec<(&'static str, String)> {
    graph.with(|cloud| {
        let asked = std::mem::take(&mut cloud.log);
        asked.into_iter().map(|(_, route)| {
            let path = route.split('?').next().unwrap_or_default().to_owned();
            match path.split('/').collect::<Vec<_>>().as_slice() {
                ["me", "drive", "items", id, "children"] => ("children", (*id).to_owned()),
                ["me", "drive", "items", id] => ("item", (*id).to_owned()),
                ["dl", id] => ("bytes", (*id).to_owned()),
                _ => ("other", route),
            }
        }).collect()
    })
}

fn of(kind: &'static str, id: &str) -> (&'static str, String) {
    (kind, id.to_owned())
}

/// A file over the small-file limit, contained in one aligned block.
const LARGE: usize = 300 * 1024;

#[tokio::test]
async fn a_range_with_a_listed_link_asks_onedrive_once() {
    let (graph, source) = folder_of(1, LARGE).await;
    let (content, listed) = opened_now(&graph, "P0");
    let bytes = bytes_of(0, LARGE);
    let first = source.read_range("P0", &content, Some(&listed), 0, 16384).await.unwrap();
    assert_eq!((first.size, first.version.as_ref().map(|v| v.ctag.clone())), (Some(LARGE as u64), Some("c-P0".into())), "the listed size and the opened content's version");
    assert_eq!(read_whole(first).await, bytes[..16384]);
    assert_eq!(requests_since(&graph), [of("children", "F"), of("bytes", "P0")], "no URL yet: the folder is listed once");
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 16384, 32768).await.unwrap()).await, bytes[16384..32768]);
    assert!(requests_since(&graph).is_empty(), "the next range is already in the aligned piece");
    source.read_blocks.clear();
    // Without what was listed, the metadata is asked before and after, as ever.
    assert_eq!(read_whole(source.read_range("P0", &content, None, 0, 10).await.unwrap()).await, bytes[..10]);
    assert_eq!(requests_since(&graph), [of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
}

#[tokio::test]
async fn reads_of_a_folders_files_cost_one_listing_and_one_request_each() {
    // An image viewer opens one screenshot of a folder and reads the head of every other.
    let (graph, source) = folder_of(32, 20 * 1024).await;
    for n in 0..32 {
        let id = format!("P{n}");
        let (content, listed) = opened_now(&graph, &id);
        assert_eq!(read_whole(source.read_range(&id, &content, Some(&listed), 0, 16384).await.unwrap()).await, bytes_of(n, 20 * 1024)[..16384]);
    }
    let asked = requests_since(&graph);
    assert_eq!(asked.iter().filter(|(kind, _)| *kind == "children").count(), 1, "{asked:?}");
    assert_eq!(asked.iter().filter(|(kind, _)| *kind == "bytes").count(), 32, "{asked:?}");
    assert_eq!(asked.len(), 33, "no metadata: {asked:?}");
    // Each small file came whole with its first range: the rest of it is in memory.
    for n in 0..32 {
        let id = format!("P{n}");
        let (content, listed) = opened_now(&graph, &id);
        assert_eq!(read_whole(source.read_range(&id, &content, Some(&listed), 16384, 20 * 1024).await.unwrap()).await, bytes_of(n, 20 * 1024)[16384..]);
    }
    assert!(requests_since(&graph).is_empty(), "the rest of a small file asks nothing");
    // What the source forgets of an item, its bytes and its link, it asks again: by the
    // metadata, as its folder was listed a moment ago, which gives the link back.
    let (content, listed) = opened_now(&graph, "P3");
    source.forget("P3");
    assert_eq!(read_whole(source.read_range("P3", &content, Some(&listed), 0, 4).await.unwrap()).await, bytes_of(3, 20 * 1024)[..4]);
    assert_eq!(requests_since(&graph), [of("item", "P3"), of("bytes", "P3"), of("item", "P3")]);
    let (content, listed) = opened_now(&graph, "P4");
    assert_eq!(read_whole(source.read_range("P4", &content, Some(&listed), 0, 4).await.unwrap()).await, bytes_of(4, 20 * 1024)[..4]);
    assert!(requests_since(&graph).is_empty(), "the folder's other files are not forgotten");
}

#[tokio::test]
async fn concurrent_reads_of_one_folder_share_one_listing() {
    let (graph, source) = folder_of(8, 1024).await;
    graph.with(|cloud| cloud.delay("GET", "/children", Duration::from_millis(200), 1));
    let opened: Vec<_> = (0..8).map(|n| opened_now(&graph, &format!("P{n}"))).collect();
    let reads = opened.iter().enumerate().map(|(n, (content, listed))| {
        let (source, id) = (&source, format!("P{n}"));
        async move { read_whole(source.read_range(&id, content, Some(listed), 0, 8).await.unwrap()).await }
    });
    let answers = futures_util::future::join_all(reads).await;
    assert!(answers.iter().enumerate().all(|(n, bytes)| bytes[..] == bytes_of(n, 1024)[..8]));
    let asked = requests_since(&graph);
    assert_eq!(asked.iter().filter(|(kind, _)| *kind == "children").count(), 1, "the reads waited for one listing: {asked:?}");
    assert_eq!(asked.len(), 9, "{asked:?}");
}

#[tokio::test]
async fn a_file_changed_behind_its_listed_link_is_never_served_as_the_opened_version() {
    for size in [LARGE, 20 * 1024] {
        let (graph, source) = folder_of(1, size).await;
        let (content, listed) = opened_now(&graph, "P0");
        assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap()).await, bytes_of(0, size)[..10]);
        // Edited elsewhere to as many bytes: the link the folder's listing gave serves the new ones.
        graph.with(|cloud| cloud.edit("P0", &bytes_of(9, size)));
        requests_since(&graph);
        if size == LARGE {
            source.read_blocks.clear();
            // A program that read the old version goes on reading the same opened content.
            let later = source.read_range("P0", &content, Some(&listed), 10, 20).await;
            assert!(matches!(later, Err(SourceError::NotFound(_))), "another eTag through the link, and the metadata's version is not the one its first range was");
            assert_eq!(requests_since(&graph), [of("bytes", "P0"), of("item", "P0"), of("bytes", "P0")]);
        }
        // A source that never read it: the listing already has the new eTag, and the
        // metadata says the new version, which the reader refuses as another content.
        let (_, fresh) = (requests_since(&graph), GraphSource::new(graph.client()));
        let answer = fresh.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
        assert_eq!(answer.version.as_ref().map(|v| v.ctag.as_str()), graph.with(|cloud| Some(cloud.item("P0").unwrap().ctag.clone())).as_deref(), "never the opened cTag over the new bytes");
        assert_ne!(answer.version.as_ref().map(|v| v.ctag.as_str()), Some("c-P0"));
        assert_eq!(requests_since(&graph), [of("children", "F"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")], "no request through a link listed with another eTag");
    }
    // Renamed elsewhere: the content is the same, the eTag is not, and the link's answer
    // is not taken.
    let (graph, source) = folder_of(1, LARGE).await;
    let (content, listed) = opened_now(&graph, "P0");
    graph.with(|cloud| cloud.rename("P0", "F", "renamed.png"));
    let answer = source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
    assert_eq!(answer.version.as_ref().map(|v| v.ctag.as_str()), Some("c-P0"), "the metadata says the same content");
    assert_eq!(requests_since(&graph), [of("children", "F"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
}

#[tokio::test]
async fn a_changed_answer_through_a_listed_link_is_dropped_and_the_metadata_asked() {
    // The link was listed with the opened eTag, and the file changes between the listing
    // and the request: its answer says the new eTag, and is not taken.
    let (graph, source) = folder_of(1, 20 * 1024).await;
    let (content, listed) = opened_now(&graph, "P0");
    source.links.list_folder(&source.drive, "F", "P0").await;
    graph.with(|cloud| cloud.edit("P0", &bytes_of(9, 20 * 1024)));
    requests_since(&graph);
    let answer = source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
    assert_ne!(answer.version.as_ref().map(|v| v.ctag.as_str()), Some("c-P0"), "the new bytes carry the new version");
    assert_eq!(requests_since(&graph), [of("bytes", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
    assert!(source.read_blocks.find("P0", &content, 0, 10).is_none(), "nothing of the new bytes is held as the opened content");
    // An answer that says no `ETag`, or another size, is not taken either.
    let (graph, source) = folder_of(1, LARGE).await;
    let (content, listed) = opened_now(&graph, "P0");
    let url = format!("{}/dl/P0", graph.server.uri());
    for answer in [ResponseTemplate::new(206).insert_header("Content-Range", format!("bytes 0-9/{LARGE}").as_str()).set_body_bytes(vec![0; 10]),
                   ResponseTemplate::new(206).insert_header("ETag", listed.etag.as_str()).insert_header("Content-Range", "bytes 0-9/11").set_body_bytes(vec![0; 10])] {
        source.read_blocks.clear();
        source.links.keep("P0", Link { url: url.clone(), etag: listed.etag.clone() });
        source.read_blocks.clear();
        graph.with(|cloud| cloud.script("GET", "dl/P0", answer, 1));
        requests_since(&graph);
        assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap()).await, bytes_of(0, LARGE)[..10]);
        assert_eq!(requests_since(&graph), [of("bytes", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
    }
}

#[tokio::test]
async fn an_expired_link_falls_back_to_the_metadata_and_is_refreshed() {
    let (graph, source) = folder_of(1, LARGE).await;
    let (content, listed) = opened_now(&graph, "P0");
    let bytes = bytes_of(0, LARGE);
    source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
    source.read_blocks.clear();
    graph.with(|cloud| cloud.script("GET", "dl/P0", ResponseTemplate::new(410), 1));
    requests_since(&graph);
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 10, 20).await.unwrap()).await, bytes[10..20]);
    assert_eq!(requests_since(&graph), [of("bytes", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")], "an expired link: the metadata, which gives a new one");
    source.read_blocks.clear();
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 20, 30).await.unwrap()).await, bytes[20..30]);
    assert_eq!(requests_since(&graph), [of("bytes", "P0")], "the metadata's link serves the next range");
}

#[tokio::test]
async fn a_failed_listing_falls_back_to_the_metadata() {
    let (graph, source) = folder_of(2, LARGE).await;
    graph.with(|cloud| cloud.script("GET", "/children", ResponseTemplate::new(403), 1));
    for n in 0..2 {
        let id = format!("P{n}");
        let (content, listed) = opened_now(&graph, &id);
        assert_eq!(read_whole(source.read_range(&id, &content, Some(&listed), 0, 10).await.unwrap()).await, bytes_of(n, LARGE)[..10]);
    }
    assert_eq!(requests_since(&graph), [of("children", "F"), of("item", "P0"), of("bytes", "P0"), of("item", "P0"), of("item", "P1"), of("bytes", "P1"), of("item", "P1")], "the folder is not listed again at once");
    // A file whose folder the store cannot say is read by its metadata, and listed nothing.
    let (graph, source) = folder_of(1, LARGE).await;
    let (content, listed) = opened_now(&graph, "P0");
    source.read_range("P0", &content, Some(&ListedContent { folder_id: None, ..listed }), 0, 10).await.unwrap();
    assert_eq!(requests_since(&graph), [of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
}

use konedrive_graph::token::{AuthError, RefreshReport, TokenManager};
use konedrive_graph::secret::MemoryStore;
use konedrive_graph::oauth::{Endpoints, OAuthClient, TokenResponse};

struct Report;
impl RefreshReport for Report {
    fn failed(&self, _: &str) {}
    fn signed_out(&self, _: &str) {}
}

#[tokio::test]
async fn signed_out_or_revoked_accounts_never_serve_cached_links_or_small_files() {
    for size in [1024, LARGE] {
        for revoke in [false, true] {
            let (graph, _) = folder_of(1, size).await;
            let secrets = Arc::new(MemoryStore::with_token("RT"));
            let tokens = Arc::new(TokenManager::new(secrets, Report));
            let base = url::Url::parse(&format!("{}/", graph.server.uri())).unwrap();
            tokens.set_oauth(Some(OAuthClient::new(reqwest::Client::new(), Endpoints { authority: base.clone(), graph: base.clone() }, "client".into())));
            let response = |expires_in| TokenResponse { access_token: "AT".into(), expires_in, refresh_token: None, scope: None };
            tokens.seed(&response(3600)).await;
            let source = GraphSource::new(DriveClient::new(base, tokens.clone()).unwrap());
            let (content, listed) = opened_now(&graph, "P0");
            source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
            assert!(source.links.find("P0").is_some());
            assert!(source.read_blocks.find("P0", &content, 0, 10).is_some());
            if revoke {
                Mock::given(method("POST")).and(path("/token"))
                    .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":"invalid_grant"})))
                    .mount(&graph.server).await;
                tokens.seed(&response(60)).await;
                assert_eq!(tokens.access_token().await, Err(AuthError::SignedOut));
            } else {
                tokens.forget().await.unwrap();
            }
            assert!(source.links.find("P0").is_none(), "invalidation clears URLs immediately");
            assert!(source.read_blocks.find("P0", &content, 0, 10).is_none(), "invalidation clears bytes immediately");
            requests_since(&graph);
            assert!(matches!(source.read_range("P0", &content, Some(&listed), 10, 20).await, Err(SourceError::SignedOut(_))));
            assert!(requests_since(&graph).is_empty(), "no URL or metadata request after sign-out");
        }
    }
}

#[tokio::test]
async fn a_first_page_read_does_not_wait_for_later_pages() {
    let (graph, source) = folder_of(1000, 1024).await;
    let base = graph.server.uri();
    // Page one contains P0..P199. Later pages take two seconds each; completion of
    // either first-page read within one second proves neither waited on the rest.
    Mock::given(method("GET")).and(path("/me/drive/items/F/children"))
        .respond_with(move |request: &wiremock::Request| {
            let from = request.url.query_pairs().find(|(k, _)| k == "$skiptoken").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
            let value: Vec<_> = (from..from + 200).map(|n| json!({"id":format!("P{n}"), "eTag":format!("e-P{n}"), "size":1024, "@microsoft.graph.downloadUrl":format!("{base}/dl/P{n}")})).collect();
            let mut body = json!({"value":value});
            if from < 800 { body["@odata.nextLink"] = json!(format!("{base}/me/drive/items/F/children?$skiptoken={}", from + 200)); }
            ResponseTemplate::new(200).set_body_json(body).set_delay(if from == 0 { Duration::from_millis(50) } else { Duration::from_secs(2) })
        }).with_priority(1).mount(&graph.server).await;
    let (content0, listed0) = opened_now(&graph, "P0");
    let (content1, listed1) = opened_now(&graph, "P1");
    let answers = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(source.read_range("P0", &content0, Some(&listed0), 0, 10), source.read_range("P1", &content1, Some(&listed1), 0, 10))
    }).await.expect("page-one readers must not wait for the next page");
    assert_eq!(read_whole(answers.0.unwrap()).await, bytes_of(0, 1024)[..10]);
    assert_eq!(read_whole(answers.1.unwrap()).await, bytes_of(1, 1024)[..10]);
    // The listing continues in the background, with its other pages still coalesced.
    let (content, listed) = opened_now(&graph, "P999");
    tokio::time::timeout(Duration::from_secs(12), source.read_range("P999", &content, Some(&listed), 0, 10)).await.unwrap().unwrap();
    let asked = graph.server.received_requests().await.unwrap();
    assert_eq!(asked.iter().filter(|r| r.url.path().ends_with("/children")).count(), 5);
    assert!(asked.iter().all(|r| !r.url.path().ends_with("/items/P999")), "the later read used the same background listing");
    // A requested id beyond the bound falls back, without restarting the listing.
    graph.with(|cloud| cloud.add_file("OUTSIDE", "F", "outside.png", b"outside"));
    let (content, listed) = opened_now(&graph, "OUTSIDE");
    requests_since(&graph);
    assert_eq!(read_whole(source.read_range("OUTSIDE", &content, Some(&listed), 0, 7).await.unwrap()).await, b"outside");
    assert_eq!(requests_since(&graph), [of("item", "OUTSIDE"), of("bytes", "OUTSIDE"), of("item", "OUTSIDE")]);
    assert!(asked.iter().any(|r| r.url.query_pairs().any(|(k, v)| k == "$top" && v == "200")));
}

#[tokio::test]
async fn incompatible_download_etags_do_not_recache_the_metadata_url() {
    for etag in [Some("endpoint-only"), None] {
        let (graph, source) = folder_of(1, LARGE).await;
        let (content, listed) = opened_now(&graph, "P0");
        let mut answer = ResponseTemplate::new(206).insert_header("Content-Range", format!("bytes 0-{}/{LARGE}", LARGE - 1).as_str()).set_body_bytes(bytes_of(0, LARGE));
        if let Some(etag) = etag { answer = answer.insert_header("ETag", etag); }
        graph.with(|cloud| cloud.script("GET", "dl/P0", answer, 4));
        source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
        assert!(source.links.find("P0").is_none(), "an incompatible endpoint cannot prove listed reads");
        assert_eq!(requests_since(&graph), [of("children", "F"), of("bytes", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
        source.read_blocks.clear();
        source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
        assert_eq!(requests_since(&graph), [of("item", "P0"), of("bytes", "P0"), of("item", "P0")], "later reads cost only the careful three requests");
    }
}

#[tokio::test]
async fn failed_cached_downloads_drop_the_link_and_take_the_metadata_path() {
    for answer in [ResponseTemplate::new(400), ResponseTemplate::new(206), ResponseTemplate::new(206).insert_header("Content-Range", "unreadable")] {
        let (graph, source) = folder_of(1, LARGE).await;
        let (content, listed) = opened_now(&graph, "P0");
        source.read_range("P0", &content, Some(&listed), 0, 10).await.unwrap();
        source.read_blocks.clear();
        graph.with(|cloud| cloud.script("GET", "dl/P0", answer, 1));
        requests_since(&graph);
        assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 10, 20).await.unwrap()).await, bytes_of(0, LARGE)[10..20]);
        assert_eq!(requests_since(&graph), [of("bytes", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
        source.read_blocks.clear();
        source.links.keep("P0", Link { url: ":not-a-url".into(), etag: listed.etag.clone() });
        source.read_range("P0", &content, Some(&listed), 20, 30).await.unwrap();
        assert_eq!(requests_since(&graph), [of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
    }
}

#[tokio::test]
async fn a_refused_listed_binding_sends_no_request() {
    let (graph, source) = folder_of(1, LARGE).await;
    let (content, listed) = opened_now(&graph, "P0");
    assert!(matches!(source.bind_listed("P0", &content, "old-etag", true), ListedBinding::Bound));
    assert!(matches!(source.read_range("P0", &content, Some(&listed), 0, 10).await, Err(SourceError::NotFound(_))));
    assert!(requests_since(&graph).is_empty(), "the refusal precedes even a folder listing");
}

mod blocks;

mod read_ahead;

mod headers;
