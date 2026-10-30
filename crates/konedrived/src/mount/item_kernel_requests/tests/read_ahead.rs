//! Desktop scan scheduling over the real Graph source and isolated fake Graph.
use super::*;
use crate::content_source::{graph_source::GraphSource, source::ContentSource};
use crate::download_guard::{DownloadGuard, GuardConfig, ReadAheadCaller};
use crate::mount::read_ahead::HeadRead;

const HEAD: u32 = 16384;

struct ScanFixture {
    fixture: ItemFixture,
    graph: crate::fake_onedrive::FakeGraph,
    source: Arc<GraphSource>,
    mount: crate::mount::item_mount::ItemMount,
    requests: ItemKernelRequests,
    files: Vec<LocalItemId>,
    bytes: Vec<Vec<u8>>,
}

impl Drop for ScanFixture {
    fn drop(&mut self) {
        self.mount.stop();
    }
}

impl ScanFixture {
    fn new(count: usize, window: usize, delay: Duration) -> Self {
        let fixture = ItemFixture::new();
        fixture.show_onedrive_directory("Shots");
        let bytes: Vec<Vec<u8>> = (0..count)
            .map(|n| {
                let size = if n % 2 == 0 {
                    24 * 1024
                } else {
                    1024 * 1024 + 23456
                };
                (0..size).map(|at| (at * 7 + n * 13) as u8).collect()
            })
            .collect();
        let files = bytes
            .iter()
            .enumerate()
            .map(|(n, bytes)| {
                fixture
                    .show_onedrive_file(&format!("Shots/{n:02}.png"), bytes)
                    .local_item_id
            })
            .collect();
        let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
        let source = Arc::new(GraphSource::new(graph.client()));
        let mut setup = fixture.mount_setup();
        setup.source = Arc::clone(&source) as Arc<dyn ContentSource>;
        setup.read_ahead = window;
        setup.download_guard = Arc::new(DownloadGuard::new(
            true,
            GuardConfig {
                max_files: 100,
                ..GuardConfig::default()
            },
        ));
        // Keep the normal pool: cache hits must not use lingering open slots.
        let (mount, requests) =
            crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
        graph.with(|cloud| {
            cloud.log.clear();
            if !delay.is_zero() {
                cloud.delay("GET", "dl/", delay, 200);
            }
        });
        Self {
            fixture,
            graph,
            source,
            mount,
            requests,
            files,
            bytes,
        }
    }

    fn begin_head(&self, n: usize) -> Option<HeadRead> {
        let content = self
            .fixture
            .find_item(self.files[n])
            .unwrap()
            .local
            .unwrap()
            .content
            .unwrap();
        self.requests.read_ahead.as_ref().unwrap().head(
            ReadAheadCaller::testing("app:org.kde.gwenview", "kioworker"),
            self.files[n],
            &content,
            0,
        )
    }

    fn read(&self, n: usize) -> Vec<u8> {
        let mut head = self.begin_head(n);
        let opened = self
            .requests
            .open_to_read(self.files[n], Some(std::process::id()))
            .unwrap();
        let ReadAnswer::Range(range) = self.requests.decide_read(opened.fh, 0, HEAD).unwrap()
        else {
            panic!("a range");
        };
        let answer = self
            .fixture
            .runtime()
            .block_on(self.requests.read_range(range, Some(std::process::id())))
            .unwrap();
        if let Some(head) = &mut head {
            head.completed = true;
        }
        self.requests.answer_release(opened.fh);
        assert_eq!(answer, self.bytes[n][..HEAD as usize]);
        answer
    }

    fn content_requests(&self) -> Vec<String> {
        self.graph.with(|cloud| {
            cloud
                .log
                .iter()
                .filter(|(_, route)| route.starts_with("dl/"))
                .map(|(_, route)| route.clone())
                .collect()
        })
    }

    fn wait_until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(
                Instant::now() < deadline,
                "timed out: {what}; requests: {:?}",
                self.content_requests()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn is_cached(&self, n: usize) -> bool {
        let row = self.fixture.find_item(self.files[n]).unwrap();
        self.source
            .cached_range(
                row.onedrive_item.unwrap().item_id.as_str(),
                row.local.unwrap().content.as_ref().unwrap(),
                0,
                HEAD as u64,
            )
            .is_some()
    }
}

#[test]
fn a_desktop_scan_reads_forty_heads_at_least_four_times_faster_with_one_request_each() {
    let run = |window| {
        let scan = ScanFixture::new(40, window, Duration::from_millis(300));
        let began = Instant::now();
        for n in 0..40 {
            scan.read(n);
        }
        let elapsed = began.elapsed();
        let asked = scan.content_requests();
        assert_eq!(asked.len(), 40, "one content request per file: {asked:?}");
        assert_eq!(
            asked.iter().collect::<std::collections::HashSet<_>>().len(),
            40
        );
        for file in &scan.files {
            assert!(!scan.requests.download_states.is_downloading(*file));
            assert_eq!(
                scan.fixture.find_item(*file).unwrap().content_presence,
                ContentPresence::NotOnDisk
            );
        }
        let counted: Vec<_> = scan
            .requests
            .download_guard
            .apps()
            .into_iter()
            .filter(|app| app.files > 0)
            .map(|app| (app.files, app.bytes))
            .collect();
        assert_eq!(
            counted,
            [(40, 40 * u64::from(HEAD))],
            "only requested bytes, including memory hits"
        );
        let listings = scan.graph.with(|cloud| {
            cloud
                .log
                .iter()
                .filter(|(_, route)| route.starts_with("me/drive/items/Shots/children"))
                .count()
        });
        assert_eq!(listings, 1);
        scan.mount.stop();
        (elapsed, asked.len())
    };
    let (off, off_requests) = run(0);
    let (on, on_requests) = run(16);
    eprintln!("read-ahead benchmark: off {off:?} / {off_requests} requests; on {on:?} / {on_requests} requests; speedup {:.2}x", off.as_secs_f64() / on.as_secs_f64());
    assert!(on * 4 < off, "off {off:?}, on {on:?}");
}

#[test]
fn a_scan_only_fetches_matching_cloud_files_within_its_window() {
    let scan = ScanFixture::new(40, 16, Duration::from_millis(5));
    // Add exclusions before building the scan snapshot. Refresh fake Graph accordingly.
    let jpg = scan
        .fixture
        .show_onedrive_file("Shots/02a.jpg", &vec![0; 24000]);
    let resident = scan
        .fixture
        .show_onedrive_file("Shots/02b.png", &vec![0; 24000]);
    scan.fixture
        .land_copy(resident.local_item_id, &vec![0; 24000]);
    scan.fixture.show_onedrive_directory("Shots/02c.png");
    scan.graph.with(|cloud| {
        // Give both excluded files usable, matching links. Their absence from content
        // requests must come from scan eligibility, not a missing fake-server URL.
        for row in [&jpg, &resident] {
            cloud.add(crate::fake_onedrive::FakeItem {
                id: row.onedrive_item.as_ref().unwrap().item_id.as_str().into(),
                parent: Some("Shots".into()),
                name: row
                    .local
                    .as_ref()
                    .unwrap()
                    .place
                    .as_ref()
                    .unwrap()
                    .name
                    .to_string_lossy()
                    .into_owned(),
                folder: false,
                content: vec![0; 24000],
                hash: None,
                size: 24000,
                etag: row.onedrive_etag.clone().unwrap(),
                ctag: String::new(),
                mtime: 0,
            });
        }
    });
    scan.read(0);
    assert_eq!(
        scan.content_requests().len(),
        1,
        "one file alone never triggers"
    );
    scan.read(0);
    assert_eq!(
        scan.content_requests().len(),
        1,
        "repeated heads of one file never trigger"
    );
    scan.read(1);
    scan.wait_until("the bounded window is warm", || scan.is_cached(17));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        scan.content_requests().len(),
        18,
        "two real files and sixteen ahead"
    );
    for n in 2..18 {
        assert!(scan.is_cached(n));
        assert!(
            !scan.requests.download_states.is_hydrating(scan.files[n]),
            "no speculative emblem"
        );
        assert!(
            !scan.requests.download_states.is_downloading(scan.files[n]),
            "no speculative whole download"
        );
    }
    assert!(!scan.is_cached(18));
    let excluded = [
        jpg.onedrive_item.unwrap().item_id.as_str().to_owned(),
        resident.onedrive_item.unwrap().item_id.as_str().to_owned(),
    ];
    assert!(scan
        .content_requests()
        .iter()
        .all(|route| excluded.iter().all(|id| !route.ends_with(id))));
    let counted = scan.requests.download_guard.apps();
    assert_eq!(
        counted.iter().map(|app| app.bytes).sum::<u64>(),
        3 * u64::from(HEAD)
    );
    // The actual read of a prefetched piece has the same coverage as the first,
    // ordinary read: only its returned head counts, never the whole retained piece.
    scan.read(2);
    for n in [0, 2] {
        let content = scan
            .fixture
            .find_item(scan.files[n])
            .unwrap()
            .local
            .unwrap()
            .content
            .unwrap();
        assert!(!scan.requests.download_states.is_downloading(scan.files[n]));
        assert!(scan.requests.download_states.count_bytes_read(
            scan.files[n],
            &content,
            u64::from(HEAD),
            content.size() - u64::from(HEAD)
        ));
    }
    assert_eq!(
        scan.requests
            .download_guard
            .apps()
            .iter()
            .map(|app| app.bytes)
            .sum::<u64>(),
        4 * u64::from(HEAD)
    );
    scan.mount.stop();
}

#[test]
fn blocking_or_stopping_a_scan_cancels_pending_read_ahead() {
    for how in ["block", "stop", "cancel", "idle"] {
        let scan = ScanFixture::new(40, 16, Duration::ZERO);
        scan.graph.with(|cloud| {
            for n in 2..18 {
                cloud.delay(
                    "GET",
                    &format!("dl/Shots!{n:02}.png"),
                    Duration::from_secs(2),
                    1,
                );
            }
        });
        scan.read(0);
        scan.read(1);
        scan.wait_until("prefetches started", || scan.content_requests().len() >= 8);
        let started = scan.content_requests().len();
        match how {
            "block" => {
                let mut limits = scan.requests.download_guard.limits();
                limits.blocked_apps.push("app:org.kde.gwenview".into());
                scan.requests.download_guard.configure(true, limits);
            }
            "stop" => scan.mount.stop(),
            "cancel" => {
                drop(scan.begin_head(2));
            }
            "idle" => {
                std::thread::sleep(Duration::from_secs(4));
            }
            _ => unreachable!(),
        }
        if how != "idle" {
            std::thread::sleep(Duration::from_millis(2200));
            assert_eq!(
                scan.content_requests().len(),
                started,
                "{how}: launch no more requests"
            );
            assert!(
                !(2..8).any(|n| scan.is_cached(n)),
                "{how}: cancelled bodies never publish"
            );
        } else {
            let before = scan.content_requests().len();
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(scan.content_requests().len(), before);
            // A single new file after expiry must not trigger a new window.
            scan.read(20);
            std::thread::sleep(Duration::from_millis(650));
            assert_eq!(scan.content_requests().len(), before + 1);
        }
        scan.mount.stop();
    }
}

#[test]
fn a_real_read_does_not_wait_for_other_prefetches() {
    let scan = ScanFixture::new(40, 16, Duration::ZERO);
    scan.graph.with(|cloud| {
        for n in 2..8 {
            cloud.delay(
                "GET",
                &format!("dl/Shots!{n:02}.png"),
                Duration::from_secs(2),
                1,
            );
        }
    });
    scan.read(0);
    scan.read(1);
    scan.wait_until("all six speculative requests are in flight", || {
        scan.content_requests().len() >= 8
    });
    let began = Instant::now();
    scan.read(35);
    let elapsed = began.elapsed();
    eprintln!("foreground read with six 2-second prefetches in flight: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(400),
        "a foreground read waits behind speculation: {elapsed:?}"
    );
    scan.mount.stop();
}
