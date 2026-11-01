//! Real disk hits through guard admission, scan accounting and whole-download promotion.
use super::*;
use crate::config::header_cache::HeaderCache;
use crate::content_source::{
    graph_source::GraphSource,
    source::{ContentSource, ListedContent},
};
use crate::download_guard::{DownloadGuard, GuardConfig, ReadAheadCaller};

mod lag;

struct CachedMount {
    fixture: ItemFixture,
    graph: crate::fake_onedrive::FakeGraph,
    source: Arc<GraphSource>,
    mount: crate::mount::item_mount::ItemMount,
    requests: ItemKernelRequests,
    files: Vec<LocalItemId>,
    _cache_dir: tempfile::TempDir,
}

impl Drop for CachedMount {
    fn drop(&mut self) {
        self.mount.stop();
    }
}

impl CachedMount {
    fn new(sizes: &[usize], read_ahead: usize, restart: bool) -> Self {
        let fixture = ItemFixture::new();
        fixture.show_onedrive_directory("Shots");
        let files: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(n, size)| {
                fixture
                    .show_onedrive_file(&format!("Shots/{n:02}.png"), &vec![n as u8; *size])
                    .local_item_id
            })
            .collect();
        let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
        let cache_dir = tempfile::tempdir().unwrap();
        let headers = Arc::new(HeaderCache::open(
            cache_dir.path().join("headers.sqlite"),
            1024 * 1024,
        ));
        let source = GraphSource::with_headers(graph.client(), Some(headers.clone()));
        for item in &files {
            let row = fixture.find_item(*item).unwrap();
            let content = row.local.as_ref().unwrap().content.as_ref().unwrap();
            let listed = ListedContent {
                etag: row.onedrive_etag.clone().unwrap(),
                size: content.size(),
                folder_id: Some("Shots".into()),
            };
            fixture
                .runtime()
                .block_on(source.read_range(
                    row.onedrive_item.as_ref().unwrap().item_id.as_str(),
                    content,
                    Some(&listed),
                    0,
                    content.size().min(16384),
                ))
                .unwrap();
        }
        fixture.runtime().block_on(headers.flush());
        let source = if restart {
            drop(source);
            drop(headers);
            let headers = Arc::new(HeaderCache::open(
                cache_dir.path().join("headers.sqlite"),
                1024 * 1024,
            ));
            fixture.runtime().block_on(headers.flush());
            Arc::new(GraphSource::with_headers(graph.client(), Some(headers)))
        } else {
            Arc::new(source)
        };
        let mut setup = fixture.mount_setup();
        setup.source = source.clone();
        setup.read_ahead = read_ahead;
        setup.download_guard = Arc::new(DownloadGuard::new(
            true,
            GuardConfig {
                max_files: 100,
                ..Default::default()
            },
        ));
        let (mount, requests) =
            crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
        graph.with(|cloud| cloud.log.clear());
        Self {
            fixture,
            graph,
            source,
            mount,
            requests,
            files,
            _cache_dir: cache_dir,
        }
    }

    fn read(&self, n: usize, caller: Option<ReadAheadCaller>) -> Result<Vec<u8>, Errno> {
        self.read_part(n, 0, 16384, caller)
    }

    fn read_part(
        &self,
        n: usize,
        from: u64,
        size: u32,
        caller: Option<ReadAheadCaller>,
    ) -> Result<Vec<u8>, Errno> {
        let opened = self
            .requests
            .open_to_read(self.files[n], Some(std::process::id()))
            .unwrap();
        let ReadAnswer::Range(range) = self.requests.decide_read(opened.fh, from, size).unwrap()
        else {
            panic!("cloud-only range")
        };
        let answer = self.fixture.runtime().block_on(self.requests.read_range_as(
            range,
            Some(std::process::id()),
            caller,
        ));
        self.requests.answer_release(opened.fh);
        answer
    }

    fn caller() -> ReadAheadCaller {
        ReadAheadCaller::testing("app:org.kde.gwenview", "kioworker")
    }

    fn wait_for(&self, description: &str, condition: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < until, "timed out: {description}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn content_requests(&self) -> usize {
        self.graph.with(|cloud| {
            cloud
                .log
                .iter()
                .filter(|(_, route)| route.starts_with("dl/"))
                .count()
        })
    }
}

#[test]
fn header_cache_hits_promote_tiny_files_outside_a_scan_and_charge_the_guard() {
    for window in [0, 16] {
        let mut baseline = None;
        for restart in [false, true] {
            let cached = CachedMount::new(&[128], window, restart);
            assert_eq!(cached.read(0, None).unwrap(), vec![0; 128]);
            cached.wait_for("the whole file lands after a counted disk hit", || {
                cached
                    .fixture
                    .find_item(cached.files[0])
                    .unwrap()
                    .content_presence
                    == ContentPresence::OnDisk
            });
            assert_eq!(
                cached.content_requests(),
                0,
                "the verified whole copy reuses the restored bytes without fetching content"
            );
            let counted: Vec<_> = cached
                .requests
                .download_guard
                .apps()
                .into_iter()
                .filter(|app| app.files > 0)
                .map(|app| (app.files, app.bytes))
                .collect();
            assert_eq!(
                counted,
                [(1, 256)],
                "range admission and whole-copy promotion are both charged, as before"
            );
            if let Some(baseline) = &baseline {
                assert_eq!(&counted, baseline);
            } else {
                baseline = Some(counted);
            }
        }
    }
}

#[test]
fn header_cache_scan_hits_stay_cloud_only_while_read_ahead_warms_memory() {
    let mut baseline = None;
    for restart in [false, true] {
        let cached = CachedMount::new(&[512, 1024, 4096, 8192], 16, restart);
        assert_eq!(
            cached.read(0, Some(CachedMount::caller())).unwrap(),
            vec![0; 512]
        );
        assert_eq!(
            cached.read(1, Some(CachedMount::caller())).unwrap(),
            vec![1; 1024]
        );
        let row = cached.fixture.find_item(cached.files[3]).unwrap();
        let content = row.local.as_ref().unwrap().content.as_ref().unwrap();
        cached.wait_for("the scan warms the fourth file in memory", || {
            cached
                .source
                .cached_range(
                    row.onedrive_item.as_ref().unwrap().item_id.as_str(),
                    content,
                    0,
                    content.size(),
                )
                .is_some()
        });
        assert_eq!(
            cached.read(2, Some(CachedMount::caller())).unwrap(),
            vec![2; 4096]
        );
        let first = cached.fixture.find_item(cached.files[0]).unwrap();
        let key = crate::mount::item_download_states::DownloadKey {
            item: cached.files[0],
            content: first.local.unwrap().content.unwrap(),
        };
        cached.wait_for(
            "the first tiny promotion observes the active scan and ends",
            || {
                cached
                    .requests
                    .download_states
                    .delay_read(key.clone(), CachedMount::caller().identity())
                    .is_some()
            },
        );
        assert_eq!(
            cached.content_requests(),
            if restart { 2 } else { 0 },
            "real heads hit disk; read-ahead still warms files beyond them"
        );
        for item in &cached.files {
            assert_eq!(
                cached.fixture.find_item(*item).unwrap().content_presence,
                ContentPresence::NotOnDisk
            );
            assert!(!cached.requests.download_states.is_downloading(*item));
        }
        let counted: Vec<_> = cached
            .requests
            .download_guard
            .apps()
            .into_iter()
            .filter(|app| app.files > 0)
            .map(|app| (app.files, app.bytes))
            .collect();
        assert_eq!(
        counted,
        [(3, 512 + 1024 + 4096 + 512)],
        "real hits and the first tiny promotion retain existing charges; speculative reads are uncharged"
    );
        if let Some(baseline) = &baseline {
            assert_eq!(&counted, baseline);
        } else {
            baseline = Some(counted);
        }
    }
}

#[test]
fn header_cache_restarted_desktop_scan_prefetches_exiv2_ranges_concurrently() {
    // Gate the actor's calls, retaining the real GraphSource underneath. Six calls
    // must enter before any can finish; no timer decides whether they overlap.
    struct Gated {
        source: Arc<GraphSource>,
        entered: std::sync::mpsc::Sender<String>,
        finished: std::sync::mpsc::Sender<()>,
        resume: tokio::sync::Semaphore,
    }
    #[async_trait::async_trait]
    impl ContentSource for Gated {
        async fn fetch(
            &self,
            _: &str,
            _: u64,
            _: Option<u64>,
        ) -> Result<
            crate::content_source::source::Fetched,
            crate::content_source::source::SourceError,
        > {
            unreachable!("the scan uses only read_ahead")
        }
        async fn read_ahead(&self, item: &str, opened: &ContentVersion, listed: &ListedContent) {
            self.entered.send(item.into()).unwrap();
            self.resume.acquire().await.unwrap().forget();
            self.source.read_ahead(item, opened, listed).await;
            let _ = self.finished.send(());
        }
    }
    let sizes: Vec<_> = (0..24)
        .map(|n| if n % 2 == 0 { 192 * 1024 } else { 1536 * 1024 })
        .collect();
    let mut cached = CachedMount::new(&sizes, 16, true);
    let (entered, starts) = std::sync::mpsc::channel();
    let (finished, finishes) = std::sync::mpsc::channel();
    let gated = Arc::new(Gated {
        source: cached.source.clone(),
        entered,
        finished,
        resume: tokio::sync::Semaphore::new(0),
    });
    cached.requests.read_ahead.as_ref().unwrap().stop();
    cached.requests.read_ahead = Some(Arc::new(crate::mount::read_ahead::ReadAhead::new(
        cached.fixture.store.clone(),
        gated.clone(),
        cached.requests.download_guard.clone(),
        16,
        &cached.fixture.runtime(),
    )));
    let is_warm = |n: usize| {
        let row = cached.fixture.find_item(cached.files[n]).unwrap();
        let content = row.local.as_ref().unwrap().content.as_ref().unwrap();
        cached
            .source
            .cached_range(
                row.onedrive_item.as_ref().unwrap().item_id.as_str(),
                content,
                0,
                content.size().min(1024 * 1024),
            )
            .is_some()
    };
    assert!((0..24).all(|n| !is_warm(n)), "restart has no memory blocks");
    cached.read(0, Some(CachedMount::caller())).unwrap();
    cached.read(1, Some(CachedMount::caller())).unwrap();
    assert_eq!(cached.content_requests(), 0, "initial MIME heads hit disk");
    let mut launched = Vec::new();
    for _ in 0..6 {
        launched.push(starts.recv_timeout(Duration::from_secs(5)).unwrap());
    }
    assert_eq!(
        launched
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
    assert!(
        starts.try_recv().is_err(),
        "no seventh speculative read while six wait"
    );
    gated.resume.add_permits(16);
    for _ in 0..16 {
        finishes.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert!(
        (2..18).all(is_warm),
        "all sixteen files inside the window are prefetched"
    );
    assert!(
        (18..24).all(|n| !is_warm(n)),
        "nothing beyond the window is fetched"
    );
    assert_eq!(cached.content_requests(), 16);
    gated.resume.add_permits(64);
    // Replay the viewer's metadata pattern in folder order. The first two files
    // precede the speculative window; all others must reuse their prefetched block.
    for (n, size) in sizes.iter().enumerate() {
        if n >= 2 {
            cached.wait_for("the next file's Exiv2 range is warm", || is_warm(n));
        }
        assert_eq!(
            cached.read(n, Some(CachedMount::caller())).unwrap(),
            vec![n as u8; 16384]
        );
        for (from, len) in [
            (0, 4096),
            (0, 33),
            (33, 4096),
            (65536, 4096),
            (131072, 4096),
        ] {
            assert_eq!(
                cached
                    .read_part(n, from, len, Some(CachedMount::caller()))
                    .unwrap(),
                vec![n as u8; len as usize]
            );
        }
        if *size > 1024 * 1024 {
            assert_eq!(
                cached
                    .read_part(n, 983040, 4096, Some(CachedMount::caller()))
                    .unwrap(),
                vec![n as u8; 4096]
            );
        }
    }
    let requests = cached.graph.with(|cloud| cloud.log.clone());
    for n in 0..24 {
        assert_eq!(
            requests
                .iter()
                .filter(|(_, route)| route == &format!("dl/Shots!{n:02}.png"))
                .count(),
            1,
            "file {n}: metadata probes share one whole-small-file/first-block request"
        );
        assert_eq!(
            cached
                .fixture
                .find_item(cached.files[n])
                .unwrap()
                .content_presence,
            ContentPresence::NotOnDisk
        );
        assert!(!cached
            .requests
            .download_states
            .is_downloading(cached.files[n]));
    }
    assert_eq!(cached.content_requests(), 24);
}
