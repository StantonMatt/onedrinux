//! Version proof, dynamic playback reuse, and fail-closed whole-file verification.
use super::*;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::content_source::source::{download_staged, FetchNext};
use crate::content_source::graph_source::read_blocks::BLOCK;

fn download(source: &GraphSource) -> (Box<dyn ContentSource>, Arc<AtomicU64>) {
    let received = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&received);
    (source.download_source("P0", Arc::new(move |n| { count.fetch_add(n, Ordering::SeqCst); })).unwrap(), received)
}

async fn prime(source: &GraphSource, graph: &FakeGraph, blocks: &[u64]) {
    let (content, listed) = opened_now(graph, "P0");
    for &block in blocks {
        let from = block * BLOCK;
        source.read_range("P0", &content, Some(&listed), from, from + 1).await.unwrap();
    }
}

async fn landed(source: &dyn ContentSource, size: usize) -> Result<Vec<u8>, i32> {
    let file = tempfile::tempfile().unwrap();
    let got = download_staged(&file, "P0", size as u64, source, None).await?;
    assert_eq!(got.size, size as u64);
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0).unwrap();
    Ok(bytes)
}

#[tokio::test]
async fn cached_download_rejects_changed_fresh_metadata() {
    for change in [0, 1, 2] {
        let size = 3 * BLOCK as usize + 57;
        let (graph, source) = folder_of(1, size).await;
        prime(&source, &graph, &[0, 1, 2, 3]).await;
        let current_size = size + if change == 2 { 17 } else { 0 };
        let expected = if change == 0 {
            graph.with(|cloud| cloud.rename("P0", "F", "renamed.mp4"));
            bytes_of(0, size)
        } else {
            let bytes = bytes_of(9, current_size);
            graph.with(|cloud| cloud.edit("P0", &bytes));
            bytes
        };
        let (downloader, received) = download(&source);
        assert_eq!(landed(downloader.as_ref(), current_size).await.unwrap(), expected);
        assert_eq!(received.load(Ordering::SeqCst), current_size as u64, "fresh eTag/content disagreement fetches everything");
    }
}

#[tokio::test]
async fn cached_download_discards_corrupt_reuse_and_retries_entirely_fresh() {
    let size = 3 * BLOCK as usize + 57;
    let (graph, source) = folder_of(1, size).await;
    prime(&source, &graph, &[0, 1, 2, 3]).await;
    let (content, _) = opened_now(&graph, "P0");
    let mut bad = source.read_blocks.find_piece("P0", &content, 0, BLOCK).unwrap();
    bad.bytes[113] ^= 1;
    source.read_blocks.keep("P0", &content, &bad);
    let before = graph.server.received_requests().await.unwrap().len();
    let (downloader, received) = download(&source);
    assert_eq!(landed(downloader.as_ref(), size).await.unwrap(), bytes_of(0, size));
    assert_eq!(received.load(Ordering::SeqCst), size as u64, "the failed memory-only attempt consumes no retry traffic");
    assert!(source.read_blocks.find_piece("P0", &content, 0, BLOCK).is_none());
    let asked = graph.server.received_requests().await.unwrap();
    assert_eq!(asked[before..].iter().filter(|r| r.url.path().starts_with("/dl/")).count(), 1);
}

#[tokio::test]
async fn cached_download_second_hash_failure_is_eio_and_discards_checkpoint() {
    let size = 20 * BLOCK as usize;
    let (graph, source) = folder_of(1, size).await;
    prime(&source, &graph, &(0..20).collect::<Vec<_>>()).await;
    let (content, _) = opened_now(&graph, "P0");
    let mut bad = source.read_blocks.find_piece("P0", &content, 0, BLOCK).unwrap();
    bad.bytes[113] ^= 1;
    source.read_blocks.keep("P0", &content, &bad);
    let mut bad_whole = bytes_of(0, size);
    bad_whole[113] ^= 1;
    graph.with(|cloud| cloud.script("GET", "dl/P0", ResponseTemplate::new(200).set_body_bytes(bad_whole), 1));
    let (downloader, received) = download(&source);
    let file = tempfile::tempfile().unwrap();
    assert_eq!(download_staged(&file, "P0", size as u64, downloader.as_ref(), None).await.err(), Some(libc::EIO));
    assert_eq!(received.load(Ordering::SeqCst), size as u64);
    assert!(konedrive_fs::placeholder::read_progress(&file).unwrap().is_none());
}

#[tokio::test]
async fn cached_download_uses_playback_blocks_published_after_it_started() {
    let size = 3 * BLOCK as usize + 57;
    let (graph, source) = folder_of(1, size).await;
    let (downloader, received) = download(&source);
    // Open a network stream with an empty cache, then let playback overtake it.
    let mut fetched = downloader.fetch("P0", 0, None).await.unwrap();
    let mut bytes = vec![0; BLOCK as usize];
    fetched.stream.read_exact(&mut bytes).await.unwrap();
    prime(&source, &graph, &[1, 2]).await;
    let boundary = fetched.stream.read_to_end(&mut bytes).await.unwrap_err();
    assert!(FetchNext::requested(&boundary));
    assert_eq!(bytes.len(), 3 * BLOCK as usize, "reopen before the uncached tail");
    bytes.extend(read_whole(downloader.fetch("P0", bytes.len() as u64, None).await.unwrap()).await);
    assert_eq!(bytes, bytes_of(0, size));
    assert_eq!(received.load(Ordering::SeqCst), BLOCK + 57, "the running response is stopped before newly cached blocks");
}

#[tokio::test]
async fn cached_download_resume_offsets_and_bounded_parts_preserve_exact_bytes() {
    let size = 3 * BLOCK as usize + 57;
    let (graph, source) = folder_of(1, size).await;
    prime(&source, &graph, &[0, 2]).await;
    let (downloader, received) = download(&source);
    for (from, end) in [(BLOCK / 2, 2 * BLOCK + 101), (2 * BLOCK + 11, 3 * BLOCK + 57), (size as u64, size as u64)] {
        let mut bytes = Vec::new();
        loop {
            let at = from + bytes.len() as u64;
            let mut fetched = downloader.fetch("P0", at, Some(end)).await.unwrap();
            assert_eq!(fetched.served_from, at);
            match fetched.stream.read_to_end(&mut bytes).await {
                Ok(_) => break,
                Err(error) => {
                    assert!(FetchNext::requested(&error));
                    assert!(from + bytes.len() as u64 > at);
                }
            }
        }
        assert_eq!(bytes, bytes_of(0, size)[from as usize..end as usize]);
    }
    assert_eq!(received.load(Ordering::SeqCst), BLOCK + 57);
}

#[tokio::test]
async fn cached_download_expired_link_rechecks_changed_content_instead_of_reporting_item_gone() {
    use std::sync::atomic::AtomicUsize;
    let size = 3 * BLOCK as usize + 57;
    let (graph, source) = folder_of(1, size).await;
    prime(&source, &graph, &[2]).await;
    let metadata = |tag: &str, bytes: &[u8]| json!({
        "id": "P0", "file": {"hashes": {"quickXorHash": crate::fake_onedrive::qx(bytes)}},
        "size": size, "cTag": tag, "eTag": tag,
        "@microsoft.graph.downloadUrl": format!("{}/dl/P0", graph.server.uri()),
    });
    let mut old = metadata("c-P0", &bytes_of(0, size));
    old["eTag"] = json!("e-P0");
    let changed = bytes_of(9, size);
    let fresh = metadata("new", &changed);
    graph.with(|cloud| {
        cloud.edit("P0", &changed);
        cloud.script("GET", "dl/P0", ResponseTemplate::new(403), 1);
    });
    let calls = AtomicUsize::new(0);
    Mock::given(method("GET")).and(path_regex("/items/P0$"))
        .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200)
            .set_body_json(if calls.fetch_add(1, Ordering::SeqCst) == 0 { &old } else { &fresh }))
        .with_priority(1).mount(&graph.server).await;
    let (downloader, _) = download(&source);
    assert_eq!(landed(downloader.as_ref(), size).await.unwrap(), changed);
}

#[tokio::test]
async fn cached_download_reuses_listed_blocks_when_only_fresh_metadata_supplies_the_hash() {
    let size = 3 * BLOCK as usize + 57;
    let (graph, source) = folder_of(1, size).await;
    let (mut content, listed) = opened_now(&graph, "P0");
    if let ContentVersion::Hashed { hash, .. } = &mut content { *hash = None; }
    source.read_range("P0", &content, Some(&listed), 0, size as u64).await.unwrap();
    let (downloader, received) = download(&source);
    assert_eq!(landed(downloader.as_ref(), size).await.unwrap(), bytes_of(0, size));
    assert_eq!(received.load(Ordering::SeqCst), 0, "matching eTag/cTag/size proof is reusable with the fresh full-file hash");
}

#[tokio::test]
async fn cached_download_many_missing_intervals_do_not_spend_the_break_budget() {
    use crate::content_source::source::{Share, Split};
    // Six network intervals exceed the three-break limit, in either writer.
    for in_parts in [false, true] {
        let size = 12 * BLOCK as usize;
        let (graph, source) = folder_of(1, size).await;
        prime(&source, &graph, &[0, 2, 4, 6, 8, 10]).await;
        let (downloader, received) = download(&source);
        let file = tempfile::tempfile().unwrap();
        let split = in_parts.then(|| Split::new(Arc::clone(source.drive.pool()), Share::new()));
        assert_eq!(download_staged(&file, "P0", size as u64, downloader.as_ref(), split.as_ref()).await.unwrap().size, size as u64);
        let mut bytes = vec![0; size];
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, bytes_of(0, size));
        assert_eq!(received.load(Ordering::SeqCst), 6 * BLOCK);
        assert!(konedrive_fs::placeholder::read_progress(&file).unwrap().is_none());
    }
}

#[tokio::test]
async fn cached_download_short_successful_bodies_keep_a_checkpoint_and_resume() {
    let size = 40 * BLOCK as usize;
    let half = 20 * BLOCK as usize;
    let (graph, source) = folder_of(1, size).await;
    graph.with(|cloud| cloud.script("GET", "dl/P0", ResponseTemplate::new(200)
        .set_body_bytes(bytes_of(0, size)[..half].to_vec()), 3));
    let (downloader, received) = download(&source);
    let file = tempfile::tempfile().unwrap();
    assert_eq!(download_staged(&file, "P0", size as u64, downloader.as_ref(), None).await.err(), Some(libc::EIO));
    assert_eq!(file.metadata().unwrap().len(), half as u64, "a short 200 retains its usable prefix");
    let checkpoint = konedrive_fs::placeholder::read_progress(&file).unwrap().expect("a durable prefix checkpoint");
    assert_eq!(checkpoint.bytes, 16 * BLOCK);
    assert_eq!(received.load(Ordering::SeqCst), half as u64);
    let (retry, received) = download(&source);
    assert_eq!(download_staged(&file, "P0", size as u64, retry.as_ref(), None).await.unwrap().size, size as u64);
    assert_eq!(received.load(Ordering::SeqCst), size as u64 - checkpoint.bytes);
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(bytes, bytes_of(0, size));
    assert!(konedrive_fs::placeholder::read_progress(&file).unwrap().is_none());
}
