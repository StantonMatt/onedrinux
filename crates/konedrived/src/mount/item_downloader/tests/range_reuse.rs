//! Playback bytes can feed a verified whole copy; unfinished staging never feeds reads.
use super::*;
use crate::content_source::graph_source::GraphSource;
use crate::content_source::source::ListedContent;
use tokio::io::AsyncReadExt;

const BLOCK: usize = 1024 * 1024;

#[test]
fn a_throttle_after_a_reused_prefix_promptly_refuses_the_waiting_reader() {
    // The plain download is the control: reuse must retain the same refusal path.
    for reuse in [false, true] {
        let fixture = ItemFixture::new();
        let bytes = vec![17; 2 * BLOCK];
        let row = fixture.show_onedrive_file("video.mp4", &bytes);
        let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
        let state = fixture.sync_state.clone();
        fixture.pool.set_observer(Arc::new(move |throughput| state.set_throughput(throughput)));
        let source = Arc::new(GraphSource::new(graph.client()
            .with_pool(Arc::clone(&fixture.pool))
            .with_retry(konedrive_graph::drive::RetryPolicy::default())));
        let item = row.onedrive_item.as_ref().unwrap().item_id.as_str();
        let content = row.local.as_ref().unwrap().content.as_ref().unwrap();
        if reuse {
            let listed = ListedContent { etag: row.onedrive_etag.clone().unwrap(), size: bytes.len() as u64, folder_id: None };
            fixture.runtime().block_on(source.read_range(item, content, Some(&listed), 0, 1)).unwrap();
        }
        graph.with(|cloud| cloud.throttle_429("GET", "dl/video.mp4", 0, 300, 1));
        let (downloader, _) = make_downloader_reporting_to(&fixture, source, fixture.report());
        assert_eq!(hear_soon(&fixture, ask(&downloader, &row, DownloadAsker::Read)),
            Some(Err(THROTTLE_WAIT_ERRNO)), "reuse={reuse}: a 300-second retry must not hold the reader");
        assert!(fixture.read_disk_file("video.mp4").is_none());
        let requests = fixture.runtime().block_on(graph.server.received_requests()).unwrap();
        let last = requests.iter().rev().find(|r| r.url.path() == "/dl/video.mp4").unwrap();
        assert_eq!(last.headers.get("range").map(|range| range.to_str().unwrap()),
            reuse.then_some("bytes=1048576-"));
    }
}

fn land_after_reads(cached: &[usize]) -> (u64, Vec<u8>, u64) {
    land_with_reads(cached, false)
}

fn land_with_reads(cached: &[usize], corrupt: bool) -> (u64, Vec<u8>, u64) {
    let fixture = ItemFixture::new();
    let bytes: Vec<_> = (0..3 * BLOCK + 57).map(|n| (n * 17 + n / 1009) as u8).collect();
    let row = fixture.show_onedrive_file("video.mp4", &bytes);
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let source = Arc::new(GraphSource::new(graph.client()));
    let item = row.onedrive_item.as_ref().unwrap().item_id.as_str();
    let content = row.local.as_ref().unwrap().content.as_ref().unwrap();
    let listed = ListedContent { etag: row.onedrive_etag.clone().unwrap(), size: bytes.len() as u64, folder_id: None };
    if corrupt {
        let mut bad = bytes[..BLOCK].to_vec();
        bad[113] ^= 1;
        graph.with(|cloud| cloud.script("GET", "dl/video.mp4", wiremock::ResponseTemplate::new(206)
            .insert_header("ETag", listed.etag.as_str())
            .insert_header("Content-Range", format!("bytes 0-{}/{}", BLOCK - 1, bytes.len()))
            .set_body_bytes(bad), 1));
    }
    fixture.runtime().block_on(async {
        for &block in cached {
            let from = (block * BLOCK) as u64;
            let mut read = source.read_range(item, content, Some(&listed), from, from + 1).await.unwrap();
            let mut got = Vec::new();
            read.stream.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, bytes[from as usize..from as usize + 1]);
        }
    });
    let before = fixture.runtime().block_on(graph.server.received_requests()).unwrap().len();
    let (downloader, _) = make_downloader_from(&fixture, source);
    let guard = Arc::new(crate::download_guard::DownloadGuard::default());
    let charge = guard.charge(std::process::id(), "video.mp4", bytes.len() as u64).unwrap().unwrap();
    let (waiter, heard) = oneshot::channel();
    downloader.download_charged(to_download(&row), Some(waiter), Some(charge));
    assert_eq!(hear(heard), Ok(()));
    let requests = fixture.runtime().block_on(graph.server.received_requests()).unwrap();
    let fetched = requests[before..].iter().filter(|r| r.url.path().starts_with("/dl/")).map(|r| {
        let range = r.headers.get("range").map(|v| v.to_str().unwrap()).unwrap_or("bytes=0-");
        let (from, end) = range.strip_prefix("bytes=").unwrap().split_once('-').unwrap();
        let end = if end.is_empty() { bytes.len() as u64 } else { end.parse::<u64>().unwrap() + 1 };
        end - from.parse::<u64>().unwrap()
    }).sum();
    let landed = fixture.read_disk_file("video.mp4").unwrap();
    assert_eq!(landed, bytes);
    (fetched, landed, guard.apps().iter().map(|app| app.bytes).sum())
}

#[test]
fn a_fully_streamed_video_lands_without_fetching_its_cached_bytes_again() {
    assert_eq!(land_after_reads(&[0, 1, 2, 3]).0, 0);
}

#[test]
fn a_partly_streamed_video_fetches_only_the_missing_blocks_to_land() {
    assert_eq!(land_after_reads(&[0, 2]).0, BLOCK as u64 + 57);
}


#[test]
fn a_corrupt_playback_block_lands_via_fresh_retry_without_charging_reused_bytes() {
    let size = 3 * BLOCK as u64 + 57;
    let (network, _, budget) = land_with_reads(&[0], true);
    assert_eq!(network, 2 * size - BLOCK as u64);
    assert_eq!(budget, network, "only consumed network bytes above the whole-size reservation add retry charges");
}

#[test]
fn an_interrupted_download_with_reused_bytes_checkpoints_and_resumes_without_exposing_staging() {
    let fixture = ItemFixture::new();
    let bytes: Vec<_> = (0..40 * BLOCK).map(|n| (n * 17 + n / 1009) as u8).collect();
    let row = fixture.show_onedrive_file("video.mp4", &bytes);
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let proxy = fixture.runtime().block_on(graph.connection_faults());
    let source = Arc::new(GraphSource::new(graph.client()));
    let item = row.onedrive_item.as_ref().unwrap().item_id.as_str();
    let content = row.local.as_ref().unwrap().content.as_ref().unwrap();
    let listed = ListedContent { etag: row.onedrive_etag.clone().unwrap(), size: bytes.len() as u64, folder_id: None };
    fixture.runtime().block_on(source.read_range(item, content, Some(&listed), 0, 1)).unwrap();
    let (downloader, _) = make_downloader_from(&fixture, source);
    use crate::fake_onedrive::connections::DropAt;
    proxy.drop_next("GET", "/dl/", 0, 1, DropAt::Body(20 * BLOCK as u64));
    proxy.drop_next("GET", "/dl/", 0, 2, DropAt::Request(0));
    assert_eq!(hear_soon(&fixture, ask(&downloader, &row, DownloadAsker::Read)), Some(Err(libc::EIO)));
    let unfinished = fixture.download_states.list_unfinished_files();
    assert_eq!(unfinished.len(), 1);
    let progress = konedrive_fs::placeholder::read_progress(&unfinished[0].1).unwrap().unwrap();
    assert!((16 * BLOCK as u64..=21 * BLOCK as u64).contains(&progress.bytes));
    assert!(fixture.read_disk_file("video.mp4").is_none());
    assert_eq!(hear_soon(&fixture, ask(&downloader, &row, DownloadAsker::Read)), Some(Ok(())));
    assert_eq!(fixture.read_disk_file("video.mp4"), Some(bytes));
    assert!(fixture.download_states.list_unfinished_files().is_empty());
}
