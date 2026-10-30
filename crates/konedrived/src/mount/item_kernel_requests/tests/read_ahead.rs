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
        Self::with_sizes(
            &(0..count)
                .map(|n| {
                    if n % 2 == 0 {
                        24 * 1024
                    } else {
                        1024 * 1024 + 23456
                    }
                })
                .collect::<Vec<_>>(),
            window,
            delay,
        )
    }

    fn with_sizes(sizes: &[usize], window: usize, delay: Duration) -> Self {
        let fixture = ItemFixture::new();
        fixture.show_onedrive_directory("Shots");
        let bytes: Vec<Vec<u8>> = sizes
            .iter()
            .enumerate()
            .map(|(n, size)| (0..*size).map(|at| (at * 7 + n * 13) as u8).collect())
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
        let row = self.fixture.find_item(self.files[n]).unwrap();
        self.requests.read_ahead.as_ref().unwrap().head(
            Self::caller(),
            self.files[n],
            row.local.as_ref().unwrap().content.as_ref().unwrap(),
            &crate::mount::read_ahead::ScanFile::of(&row).unwrap(),
            0,
        )
    }

    fn caller() -> ReadAheadCaller {
        ReadAheadCaller::testing("app:org.kde.gwenview", "kioworker")
    }

    fn read_part(
        &self,
        item: LocalItemId,
        from: u64,
        size: u32,
        caller: Option<ReadAheadCaller>,
    ) -> Vec<u8> {
        let opened = self
            .requests
            .open_to_read(item, Some(std::process::id()))
            .unwrap();
        let answer = match self.requests.decide_read(opened.fh, from, size).unwrap() {
            ReadAnswer::Range(range) => self
                .fixture
                .runtime()
                .block_on(
                    self.requests
                        .read_range_as(range, Some(std::process::id()), caller),
                )
                .unwrap(),
            ReadAnswer::Bytes(bytes) => bytes,
            _ => panic!("a range or landed bytes"),
        };
        self.requests.answer_release(opened.fh);
        answer
    }

    fn read(&self, n: usize) -> Vec<u8> {
        let answer = self.read_part(self.files[n], 0, HEAD, Some(Self::caller()));
        assert_eq!(
            answer,
            self.bytes[n][..(HEAD as usize).min(self.bytes[n].len())]
        );
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
    // Admission still charges the real read, but active scan coverage is skipped.
    // Only the first file's head was counted before the scan existed.
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
        assert_eq!(
            scan.requests.download_states.count_bytes_read(
                scan.files[n],
                &content,
                u64::from(HEAD),
                content.size() - u64::from(HEAD)
            ),
            n == 0,
            "only the pre-scan head contributes coverage"
        );
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
fn a_guard_notification_for_another_reader_keeps_the_scans_prefetches() {
    let scan = ScanFixture::new(40, 16, Duration::ZERO);
    scan.graph.with(|cloud| {
        for n in 2..8 {
            cloud.delay("GET", &format!("dl/Shots!{n:02}.png"), Duration::from_millis(500), 1);
        }
    });
    scan.read(0);
    scan.read(1);
    scan.wait_until("prefetches started", || scan.content_requests().len() >= 8);
    // The guard notifies on every refusal of another reader, not only on policy changes.
    let limits = scan.requests.download_guard.limits();
    scan.requests.download_guard.configure(true, limits);
    scan.wait_until("pending prefetches publish", || (2..8).all(|n| scan.is_cached(n)));
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

#[test]
fn read_ahead_scan_reads_small_files_in_full_without_keeping_them() {
    let sizes = [512, 1024, 16 * 1024, 20 * 1024, 24 * 1024, 32 * 1024];
    let scan = ScanFixture::with_sizes(&sizes, 16, Duration::ZERO);
    for (n, size) in sizes.into_iter().enumerate() {
        scan.read(n);
        // Exiv2-like 4 KiB probes, including the tail: cover every remaining byte.
        for from in (0..size).step_by(4096) {
            let answer = scan.read_part(
                scan.files[n],
                from as u64,
                4096,
                Some(ScanFixture::caller()),
            );
            assert_eq!(answer, scan.bytes[n][from..(from + 4096).min(size)]);
        }
    }
    scan.wait_until("the first file lands", || {
        scan.fixture
            .find_item(scan.files[0])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
    for n in 1..sizes.len() {
        assert_eq!(
            scan.fixture
                .find_item(scan.files[n])
                .unwrap()
                .content_presence,
            ContentPresence::NotOnDisk
        );
        assert!(!scan.requests.download_states.is_downloading(scan.files[n]));
        assert!(scan
            .fixture
            .read_disk_file(&format!("Shots/{n:02}.png"))
            .is_none());
        assert_eq!(
            scan.content_requests()
                .iter()
                .filter(|route| route.ends_with(&format!("!{n:02}.png")))
                .count(),
            1,
            "no whole content request"
        );
    }
}

#[test]
fn read_ahead_activating_tiny_read_skips_counting_without_polling_the_actor() {
    let mut scan = ScanFixture::with_sizes(&[512; 80], 16, Duration::ZERO);
    // Never poll this runtime: the scheduling actor cannot process even its first event.
    // More than 64 reads also fill its queue, without losing synchronous detection.
    let scheduler = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    scan.requests.read_ahead.as_ref().unwrap().stop();
    scan.requests.read_ahead = Some(Arc::new(crate::mount::read_ahead::ReadAhead::new(
        scan.fixture.store.clone(),
        Arc::clone(&scan.source) as Arc<dyn ContentSource>,
        Arc::clone(&scan.requests.download_guard),
        16,
        scheduler.handle(),
    )));
    for n in 0..80 {
        scan.read(n);
        if n > 0 {
            assert!(!scan.requests.download_states.is_downloading(scan.files[n]));
            assert_eq!(
                scan.fixture
                    .find_item(scan.files[n])
                    .unwrap()
                    .content_presence,
                ContentPresence::NotOnDisk
            );
        }
    }
    scan.wait_until("the first tiny file lands", || {
        scan.fixture
            .find_item(scan.files[0])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
    assert_eq!(
        scan.content_requests().len(),
        81,
        "80 range requests and only the first whole download"
    );
}

#[test]
fn read_ahead_skipped_coverage_is_not_replayed_after_idle() {
    let scan = ScanFixture::with_sizes(&[512, 24 * 1024, 512], 16, Duration::ZERO);
    scan.read(0);
    scan.read(1);
    std::thread::sleep(Duration::from_millis(3100));
    // The old 16 KiB head was skipped: this 8 KiB tail cannot reach the threshold.
    assert_eq!(
        scan.read_part(
            scan.files[1],
            16 * 1024,
            8 * 1024,
            Some(ScanFixture::caller())
        ),
        scan.bytes[1][16 * 1024..]
    );
    assert!(!scan.requests.download_states.is_downloading(scan.files[1]));
    assert_eq!(
        scan.fixture
            .find_item(scan.files[1])
            .unwrap()
            .content_presence,
        ContentPresence::NotOnDisk
    );
    // The tail starts below 1 MiB, so it is a new candidate head. Expire that
    // candidate before the next tiny read to keep this read outside a new scan.
    std::thread::sleep(Duration::from_millis(3100));
    scan.read(2);
    scan.wait_until("the tiny read after idle lands", || {
        scan.fixture
            .find_item(scan.files[2])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
}

#[test]
fn read_ahead_only_skips_the_scanning_identity_and_folder() {
    for other in [
        Some(ReadAheadCaller::testing("app:other-viewer", "viewer")),
        None,
    ] {
        let scan = ScanFixture::with_sizes(&[512; 4], 16, Duration::ZERO);
        scan.read(0);
        scan.read(1);
        assert_eq!(scan.read_part(scan.files[2], 0, HEAD, other), scan.bytes[2]);
        scan.wait_until("another reader's tiny file lands", || {
            scan.fixture
                .find_item(scan.files[2])
                .unwrap()
                .content_presence
                == ContentPresence::OnDisk
        });
    }
    // A different folder is the first candidate of a new scan and counts normally.
    let scan = ScanFixture::with_sizes(&[512; 3], 16, Duration::ZERO);
    let elsewhere = scan
        .fixture
        .show_onedrive_file("elsewhere.png", b"elsewhere");
    scan.graph.with(|cloud| {
        let ContentVersion::Hashed {
            hash, ctag, mtime, ..
        } = elsewhere
            .onedrive
            .as_ref()
            .unwrap()
            .content
            .as_ref()
            .unwrap()
        else {
            panic!("cloud content");
        };
        cloud.add(crate::fake_onedrive::FakeItem {
            id: elsewhere
                .onedrive_item
                .as_ref()
                .unwrap()
                .item_id
                .as_str()
                .into(),
            parent: Some(crate::fake_onedrive::ROOT.into()),
            name: "elsewhere.png".into(),
            folder: false,
            content: b"elsewhere".to_vec(),
            hash: hash.clone(),
            size: 9,
            etag: elsewhere.onedrive_etag.clone().unwrap(),
            ctag: ctag.clone().unwrap(),
            mtime: *mtime,
        });
    });
    scan.read(0);
    scan.read(1);
    assert_eq!(
        scan.read_part(
            elsewhere.local_item_id,
            0,
            HEAD,
            Some(ScanFixture::caller())
        ),
        b"elsewhere"
    );
    scan.wait_until("a tiny read in another folder lands", || {
        scan.fixture
            .find_item(elsewhere.local_item_id)
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
}

#[test]
fn read_ahead_zero_keeps_tiny_reads_as_before() {
    let scan = ScanFixture::with_sizes(&[512; 4], 0, Duration::ZERO);
    for n in 0..4 {
        scan.read(n);
    }
    scan.wait_until("all tiny files land with detection disabled", || {
        scan.files.iter().all(|file| {
            scan.fixture.find_item(*file).unwrap().content_presence == ContentPresence::OnDisk
        })
    });
    assert_eq!(
        scan.content_requests().len(),
        8,
        "one range and one whole download each"
    );
}

#[test]
fn read_ahead_skips_non_head_ranges_and_preserves_pre_scan_coverage() {
    let scan = ScanFixture::with_sizes(&[2 * 1024 * 1024, 512], 16, Duration::ZERO);
    scan.read(0); // Ordinary coverage before activation.
    scan.read(1); // Activates the scan.
    let from = 1024 * 1024;
    assert_eq!(
        scan.read_part(
            scan.files[0],
            from,
            1024 * 1024,
            Some(ScanFixture::caller())
        ),
        scan.bytes[0][from as usize..]
    );
    assert!(
        !scan.requests.download_states.is_downloading(scan.files[0]),
        "a non-head MiB during the scan never reaches the threshold"
    );
    // With detection stopped, only the old 16 KiB counts. The skipped MiB cannot
    // combine with this tail, but the pre-scan head still contributes later.
    scan.requests.read_ahead.as_ref().unwrap().stop();
    let content = scan
        .fixture
        .find_item(scan.files[0])
        .unwrap()
        .local
        .unwrap()
        .content
        .unwrap();
    assert!(!scan.requests.download_states.count_bytes_read(
        scan.files[0],
        &content,
        2 * 1024 * 1024 - 4096,
        4096
    ));
    assert!(scan.requests.download_states.count_bytes_read(
        scan.files[0],
        &content,
        HEAD as u64,
        1024 * 1024 - HEAD as u64 - 4096
    ));
}

#[test]
fn read_ahead_lifecycle_changes_end_synchronous_suppression() {
    let fixture = ItemFixture::new();
    let first = fixture.show_onedrive_file("one.png", b"first");
    let second = fixture.show_onedrive_file("two.png", b"second");
    fixture.show_onedrive_directory("Other");
    let other_first = fixture.show_onedrive_file("Other/one.png", b"other first");
    let other_second = fixture.show_onedrive_file("Other/two.png", b"other second");
    let scheduler = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    scheduler.block_on(async {
        tokio::time::pause();
        let guard = Arc::new(DownloadGuard::default());
        let ahead = crate::mount::read_ahead::ReadAhead::new(
            fixture.store.clone(),
            fixture.content_source(),
            Arc::clone(&guard),
            16,
            scheduler.handle(),
        );
        let observe = |row: &Item| {
            ahead.head(
                ScanFixture::caller(),
                row.local_item_id,
                row.local.as_ref().unwrap().content.as_ref().unwrap(),
                &crate::mount::read_ahead::ScanFile::of(row).unwrap(),
                0,
            )
        };
        let completed = |row: &Item| {
            let mut read = observe(row).unwrap();
            let skipped = read.skip_count;
            read.completed = true;
            skipped
        };
        assert!(!completed(&first));
        assert!(completed(&second));
        tokio::time::advance(Duration::from_millis(3001)).await;
        assert!(!completed(&first), "idle ends suppression synchronously");
        assert!(completed(&second));
        // Keep the sliding idle window alive while crossing the five-minute lifetime.
        for _ in 0..149 {
            tokio::time::advance(Duration::from_secs(2)).await;
            assert!(completed(&second));
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!completed(&first), "lifetime expires despite recent heads");
        assert!(completed(&second));
        drop(observe(&second));
        assert!(
            !completed(&first),
            "cancellation ends suppression before the actor reacts"
        );
        assert!(completed(&second));
        guard.configure(true, guard.limits());
        assert!(
            completed(&first),
            "a guard notification that leaves the caller permitted keeps the scan"
        );
        let mut blocked = guard.limits();
        blocked.blocked_apps.push(ScanFixture::caller().identity().into());
        guard.configure(true, blocked);
        assert!(observe(&second).is_none(), "a blocked caller gets no scan");
        let mut unblocked = guard.limits();
        unblocked.blocked_apps.clear();
        guard.configure(true, unblocked);
        assert!(!completed(&first), "blocking the caller ended the old scan");
        assert!(completed(&second));
        let old_read = observe(&second).unwrap();
        assert!(!completed(&other_first), "a new folder replaces the scan");
        assert!(completed(&other_second));
        drop(old_read);
        assert!(
            completed(&other_second),
            "a late cancellation cannot kill the newer scan"
        );
        ahead.stop();
        assert!(
            observe(&other_second).is_none(),
            "stop disables suppression immediately"
        );
    });
}
