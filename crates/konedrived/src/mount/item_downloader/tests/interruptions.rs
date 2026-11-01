//! Whole transfers through the fake Graph and real loopback socket failures, with
//! the same state publisher Dolphin's xattrs use. No mount or live account.

use super::*;
use crate::content_source::graph_source::GraphSource;
use crate::fake_onedrive::connections::DropAt;
use konedrive_fs::placeholder::{read_progress, State};

fn download_for_reader(fixture: &ItemFixture, downloader: &Arc<ItemDownloader>, row: &Item) -> Result<(), i32> {
    fixture.runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(30), ask(downloader, row, DownloadAsker::Read)).await
            .expect("a finite fake transfer finishes within 30 seconds").expect("the reader is answered")
    })
}

#[test]
fn a_long_connection_outage_keeps_a_checkpoint_and_clears_the_error_on_recovery() {
    let fixture = ItemFixture::new();
    let content: Vec<u8> = (0..40 * 1024 * 1024u64).map(|at| ((at * 131 + at / 257) % 251) as u8).collect();
    let row = fixture.show_onedrive_file("big.bin", &content);
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let proxy = fixture.runtime().block_on(graph.connection_faults());
    let (downloader, _landing) = make_downloader_from(&fixture, Arc::new(GraphSource::new(graph.client())));
    // Match mount/build_order: landing installs the reader wake observer first,
    // then DirectoryStates becomes the store's derived-state observer.
    let states = super::super::super::directory_states::DirectoryStates::build(
        &fixture.store, fixture.download_states.clone(), fixture.running_steps.clone(), fixture.published_plan.clone(), fixture.inotify_bridge.clone(),
    ).unwrap();
    // Drop the real body after a durable (16 MiB) checkpoint, then refuse the
    // two remaining tries at the socket. The waiting reader must hear EIO.
    proxy.drop_next("GET", "/dl/", 0, 1, DropAt::Body(20 * 1024 * 1024));
    proxy.drop_next("GET", "/dl/", 0, 2, DropAt::Request(0));
    assert_eq!(download_for_reader(&fixture, &downloader, &row), Err(libc::EIO));
    assert_eq!(proxy.asked().len(), 3);
    assert_eq!(states.file_state(row.local_item_id), Some(State::Failed));
    assert_eq!(states.directory_values(LocalItemId::ROOT).0, State::Failed);
    let left = fixture.download_states.list_unfinished_files();
    assert_eq!(left.len(), 1);
    let checkpoint = read_progress(&left[0].1).unwrap().unwrap();
    assert!((16 * 1024 * 1024..=20 * 1024 * 1024).contains(&checkpoint.bytes));
    assert!(fixture.read_disk_file("big.bin").is_none(), "the partial file never lands");

    // A later open while the outage lasts minutes still fails promptly and
    // keeps the checkpoint and the error emblem; it does not loop forever.
    proxy.offline(true);
    fixture.now.fetch_add(10 * 60, Ordering::SeqCst);
    assert_eq!(download_for_reader(&fixture, &downloader, &row), Err(libc::EIO));
    assert_eq!(proxy.asked().len(), 6);
    assert_eq!(states.file_state(row.local_item_id), Some(State::Failed));
    assert_eq!(read_progress(&fixture.download_states.list_unfinished_files()[0].1).unwrap(), Some(checkpoint.clone()));

    proxy.offline(false);
    // Simulate sleep past the link lifetime: an expired download link gets fresh
    // metadata once; the checkpoint's version and hash still have to match.
    fixture.now.fetch_add(8 * 3600, Ordering::SeqCst);
    graph.with(|cloud| cloud.script("GET", "dl/", wiremock::ResponseTemplate::new(403), 1));
    assert_eq!(download_for_reader(&fixture, &downloader, &row), Ok(()));
    let requests = proxy.asked();
    assert_eq!(requests.len(), 8);
    assert!(requests[6..].iter().all(|asked| asked.range == Some(format!("bytes={}-", checkpoint.bytes))));
    assert_eq!(fixture.read_disk_file("big.bin"), Some(content));
    assert!(fixture.download_states.list_unfinished_files().is_empty());
    assert!(!fixture.download_states.has_failed_download(&key_of(&row), false));
    assert_eq!(states.file_state(row.local_item_id), Some(State::Hydrated));
    assert_eq!(states.directory_values(LocalItemId::ROOT).0, State::Hydrated);
}

#[test]
fn a_forbidden_graph_download_shows_an_error_without_landing_bytes() {
    let fixture = ItemFixture::new();
    let row = fixture.show_onedrive_file("denied.bin", b"cloud content");
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    graph.with(|cloud| cloud.script("GET", "items/denied.bin", wiremock::ResponseTemplate::new(403).set_body_json(serde_json::json!({"error": {"code": "accessDenied", "message": "denied"}})), u32::MAX));
    let (downloader, _landing) = make_downloader_from(&fixture, Arc::new(GraphSource::new(graph.client())));
    let states = super::super::super::directory_states::DirectoryStates::build(
        &fixture.store, fixture.download_states.clone(), fixture.running_steps.clone(), fixture.published_plan.clone(), fixture.inotify_bridge.clone(),
    ).unwrap();
    assert_eq!(download_for_reader(&fixture, &downloader, &row), Err(libc::EIO));
    assert_eq!(graph.with(|cloud| cloud.count("GET", "items/denied.bin")), 3);
    assert_eq!(graph.with(|cloud| cloud.count("GET", "dl/")), 0);
    assert_eq!(states.file_state(row.local_item_id), Some(State::Failed));
    assert_eq!(states.directory_values(LocalItemId::ROOT).0, State::Failed);
    assert!(fixture.read_disk_file("denied.bin").is_none());
}

#[test]
fn a_single_dropped_download_body_resumes_in_place_without_leaving_an_error() {
    let fixture = ItemFixture::new();
    let content: Vec<u8> = (0..32 * 1024 * 1024u64).map(|at| ((at * 131 + at / 257) % 251) as u8).collect();
    let row = fixture.show_onedrive_file("big.bin", &content);
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let proxy = fixture.runtime().block_on(graph.connection_faults());
    let (downloader, _landing) = make_downloader_from(&fixture, Arc::new(GraphSource::new(graph.client())));
    proxy.drop_next("GET", "/dl/", 0, 1, DropAt::Body(20 * 1024 * 1024));
    assert_eq!(download_for_reader(&fixture, &downloader, &row), Ok(()));
    let requests = proxy.asked();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].range, None);
    assert_eq!(requests[1].range.as_deref(), Some("bytes=20971520-"));
    assert_eq!(fixture.read_disk_file("big.bin"), Some(content));
    assert!(!fixture.download_states.has_failed_download(&key_of(&row), false));
    assert!(fixture.download_states.list_unfinished_files().is_empty());
}
