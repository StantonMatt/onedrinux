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
    eprintln!(
        "read-ahead benchmark: off {off:?} / {off_requests} requests; on {on:?} / {on_requests} requests; speedup {:.2}x",
        off.as_secs_f64() / on.as_secs_f64()
    );
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
    assert!(
        scan.content_requests()
            .iter()
            .all(|route| excluded.iter().all(|id| !route.ends_with(id)))
    );
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
            cloud.delay(
                "GET",
                &format!("dl/Shots!{n:02}.png"),
                Duration::from_millis(500),
                1,
            );
        }
    });
    scan.read(0);
    scan.read(1);
    scan.wait_until("prefetches started", || scan.content_requests().len() >= 8);
    // The guard notifies on every refusal of another reader, not only on policy changes.
    let limits = scan.requests.download_guard.limits();
    scan.requests.download_guard.configure(true, limits);
    scan.wait_until("pending prefetches publish", || {
        (2..8).all(|n| scan.is_cached(n))
    });
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
    // The first tiny file's grace period sees scan activation too.
    std::thread::sleep(crate::mount::read_ahead::TINY_DOWNLOAD_DELAY + Duration::from_millis(100));
    for n in 0..sizes.len() {
        assert_eq!(
            scan.fixture
                .find_item(scan.files[n])
                .unwrap()
                .content_presence,
            ContentPresence::NotOnDisk
        );
        assert!(!scan.requests.download_states.is_downloading(scan.files[n]));
        assert!(
            scan.fixture
                .read_disk_file(&format!("Shots/{n:02}.png"))
                .is_none()
        );
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
    std::thread::sleep(crate::mount::read_ahead::TINY_DOWNLOAD_DELAY + Duration::from_millis(100));
    assert!(scan.files.iter().all(
        |item| scan.fixture.find_item(*item).unwrap().content_presence
            == ContentPresence::NotOnDisk
    ));
    assert_eq!(
        scan.content_requests().len(),
        80,
        "80 ranges; the first tiny promotion is cancelled too"
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
fn read_ahead_non_head_ranges_use_handle_coverage_even_for_a_pre_scan_item() {
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
    scan.wait_until("the pre-scan file lands from non-head coverage", || {
        scan.fixture
            .find_item(scan.files[0])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
    assert_eq!(
        scan.fixture
            .find_item(scan.files[1])
            .unwrap()
            .content_presence,
        ContentPresence::NotOnDisk
    );
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
            let skipped = read.in_scan;
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
            completed(&second),
            "a guard notification that leaves the caller permitted keeps the scan"
        );
        let mut blocked = guard.limits();
        blocked
            .blocked_apps
            .push(ScanFixture::caller().identity().into());
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

/// Read several ranges on one real handle, as a decoder does.
fn read_on_handle(scan: &ScanFixture, fh: Fh, from: u64, size: u32) -> Vec<u8> {
    match scan.requests.decide_read(fh, from, size).unwrap() {
        ReadAnswer::Range(range) => scan
            .fixture
            .runtime()
            .block_on(scan.requests.read_range_as(
                range,
                Some(std::process::id()),
                Some(ScanFixture::caller()),
            ))
            .unwrap(),
        ReadAnswer::Bytes(bytes) => bytes,
        _ => panic!("range or resident bytes"),
    }
}

#[test]
fn read_ahead_trace_decode_lands_only_the_handle_that_reads_the_image_through() {
    let sizes = [130, 9712, 32 * 1024, 46642, 9588, 80 * 1024];
    let scan = ScanFixture::with_sizes(&sizes, 16, Duration::ZERO);
    for n in 0..3 {
        scan.read(n);
    }
    scan.read(3); // h1 is a peek, released before h2 is opened.
    let h2 = scan.requests.answer_open_to_read(scan.files[3]).unwrap().fh;
    assert_eq!(
        read_on_handle(&scan, h2, 0, HEAD),
        scan.bytes[3][..HEAD as usize]
    );
    assert_eq!(
        read_on_handle(&scan, h2, HEAD as u64, 30258),
        scan.bytes[3][HEAD as usize..]
    );
    scan.requests.answer_release(h2);
    for _ in 0..8 {
        scan.read(3);
    }
    for n in [0, 1, 2, 4, 5] {
        scan.read(n);
    }
    scan.wait_until("the opened image lands", || {
        scan.fixture
            .find_item(scan.files[3])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
    std::thread::sleep(crate::mount::read_ahead::TINY_DOWNLOAD_DELAY + Duration::from_millis(100));
    for n in [0, 1, 2, 4, 5] {
        assert_eq!(
            scan.fixture
                .find_item(scan.files[n])
                .unwrap()
                .content_presence,
            ContentPresence::NotOnDisk
        );
        assert_eq!(
            scan.content_requests()
                .iter()
                .filter(|route| route.ends_with(&format!("!{n:02}.png")))
                .count(),
            1,
            "peek has no whole-download request"
        );
    }
    assert_eq!(
        scan.content_requests()
            .iter()
            .filter(|route| route.ends_with("!03.png"))
            .count(),
        2
    );
}

#[test]
fn read_ahead_does_not_combine_handles_or_exiv2_probes() {
    let scan = ScanFixture::with_sizes(&[32768, 32768, 46642, 2 * 1024 * 1024], 16, Duration::ZERO);
    scan.read(0);
    scan.read(1);
    let first = scan.requests.answer_open_to_read(scan.files[2]).unwrap().fh;
    let second = scan.requests.answer_open_to_read(scan.files[2]).unwrap().fh;
    read_on_handle(&scan, first, 0, 23321);
    read_on_handle(&scan, second, 23321, 23321);
    scan.requests.answer_release(first);
    scan.requests.answer_release(second);
    for same_handle in [false, true] {
        let fh = scan.requests.answer_open_to_read(scan.files[3]).unwrap().fh;
        let ranges = [(0, 4096), (0, 33), (33, 4096)].into_iter().chain(
            (65536..2 * 1024 * 1024)
                .step_by(65536)
                .map(|from| (from, 4096)),
        );
        for (from, size) in ranges {
            if same_handle {
                read_on_handle(&scan, fh, from, size);
            } else {
                scan.read_part(scan.files[3], from, size, Some(ScanFixture::caller()));
            }
        }
        scan.requests.answer_release(fh);
    }
    for n in [2, 3] {
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

#[test]
fn read_ahead_tiny_desktop_download_waits_but_guarded_reads_start_immediately() {
    for caller in [Some(ScanFixture::caller()), None] {
        let scan = ScanFixture::with_sizes(&[130], 16, Duration::ZERO);
        scan.read_part(scan.files[0], 0, HEAD, caller.clone());
        if caller.is_some() {
            assert_eq!(
                scan.content_requests().len(),
                1,
                "desktop promotion waits for scan detection"
            );
            assert!(!scan.requests.download_states.is_downloading(scan.files[0]));
        } else {
            assert!(
                scan.requests.download_states.is_downloading(scan.files[0])
                    || scan
                        .fixture
                        .find_item(scan.files[0])
                        .unwrap()
                        .content_presence
                        == ContentPresence::OnDisk,
                "guarded reads have no grace period"
            );
        }
        scan.wait_until("a complete tiny read lands", || {
            scan.fixture
                .find_item(scan.files[0])
                .unwrap()
                .content_presence
                == ContentPresence::OnDisk
        });
        assert_eq!(scan.content_requests().len(), 2);
    }
}

#[test]
fn read_ahead_release_retains_only_the_outstanding_ranges_handle_coverage() {
    let scan = ScanFixture::with_sizes(&[32768; 3], 16, Duration::ZERO);
    scan.read(0);
    scan.read(1);
    let fh = scan.requests.answer_open_to_read(scan.files[2]).unwrap().fh;
    read_on_handle(&scan, fh, 0, HEAD);
    let ReadAnswer::Range(range) = scan.requests.decide_read(fh, HEAD as u64, HEAD).unwrap() else {
        panic!("range");
    };
    scan.requests.answer_release(fh);
    assert!(scan.requests.item_handles.range_count(fh).is_none());
    scan.fixture
        .runtime()
        .block_on(scan.requests.read_range_as(
            range,
            Some(std::process::id()),
            Some(ScanFixture::caller()),
        ))
        .unwrap();
    scan.wait_until("released handle's pending read completes coverage", || {
        scan.fixture
            .find_item(scan.files[2])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
}

#[test]
fn launch_intent_absolute_paths_and_file_urls_show_immediately_and_land_during_scans() {
    use crate::download_guard::{ProcessInfo, ProcessReader};
    struct FakeProcess(ProcessInfo);
    impl ProcessReader for FakeProcess {
        fn read(&self, _: u32) -> Option<ProcessInfo> {
            Some(self.0.clone())
        }
    }
    for window in [0, 16] {
        for url in [false, true] {
            let mut scan = ScanFixture::with_sizes(&[32768, 32768, 130], window, Duration::ZERO);
            let root = scan.fixture.folder();
            scan.fixture
                .move_here(scan.files[2], "Shots/with space.png");
            let argv = if url {
                format!("file://{}/Shots/with%20space.png", root.display())
            } else {
                root.join("Shots/with space.png").display().to_string()
            };
            let mut guard = DownloadGuard::default();
            guard.set_process_reader(Arc::new(FakeProcess(ProcessInfo {
                start: 42,
                executable: "/usr/bin/gwenview".into(),
                cgroup: "0::/app-org.kde.gwenview-123.scope".into(),
                cmdline: Arc::new(format!("gwenview\0{argv}\0").into_bytes()),
            })));
            scan.requests.download_guard = Arc::new(guard);
            scan.requests.launch_intents = Some(crate::mount::launch_intent::LaunchIntents::new(
                root,
                Arc::clone(&scan.requests.read_connections),
                Arc::clone(&scan.requests.download_guard),
                Arc::clone(&scan.requests.download_states),
                scan.fixture.runtime(),
            ));
            scan.requests.observe_process(42);
            assert!(
                scan.requests.download_states.is_hydrating(scan.files[2]),
                "emblem before any read; url={url}"
            );
            assert!(!scan.requests.download_states.is_downloading(scan.files[2]));
            scan.read(0);
            scan.read(1);
            let fh = scan
                .requests
                .open_to_read(scan.files[2], Some(42))
                .unwrap()
                .fh;
            let ReadAnswer::Range(range) = scan.requests.decide_read(fh, 0, 1).unwrap() else {
                panic!("range");
            };
            let began = Instant::now();
            assert_eq!(
                scan.fixture
                    .runtime()
                    .block_on(scan.requests.read_range(range, Some(42)))
                    .unwrap(),
                scan.bytes[2][..1]
            );
            scan.requests.answer_release(fh);
            scan.wait_until(
                "launch intent keeps a tiny file after only one byte",
                || {
                    scan.fixture
                        .find_item(scan.files[2])
                        .unwrap()
                        .content_presence
                        == ContentPresence::OnDisk
                },
            );
            assert!(
                began.elapsed() < crate::mount::read_ahead::TINY_DOWNLOAD_DELAY,
                "launch bypasses tiny grace period"
            );
            scan.requests.launch_intents.as_ref().unwrap().stop();
        }
    }
}

#[test]
fn read_ahead_outside_a_scan_still_combines_distinct_handles() {
    let scan = ScanFixture::with_sizes(&[46642], 16, Duration::ZERO);
    scan.read_part(scan.files[0], 0, 23321, Some(ScanFixture::caller()));
    assert!(!scan.requests.download_states.is_downloading(scan.files[0]));
    scan.read_part(scan.files[0], 23321, 23321, Some(ScanFixture::caller()));
    scan.wait_until("outside-scan item coverage combines two handles", || {
        scan.fixture
            .find_item(scan.files[0])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
}

#[test]
fn read_ahead_handle_keeps_successful_coverage_across_scan_activation() {
    // No argv intent: both handles begin with a MIME peek before detection.
    // Only the decoder that reads the remaining bytes on its own handle lands.
    for decode in [false, true] {
        let scan = ScanFixture::with_sizes(&[46642, 32768], 16, Duration::ZERO);
        let fh = scan.requests.answer_open_to_read(scan.files[0]).unwrap().fh;
        assert_eq!(
            read_on_handle(&scan, fh, 0, HEAD),
            scan.bytes[0][..HEAD as usize]
        );
        scan.read(1); // Second distinct head activates the folder scan.
        let mut activated = scan.begin_head(0).unwrap();
        assert!(activated.in_scan);
        activated.completed = true;
        drop(activated);
        if decode {
            assert_eq!(
                read_on_handle(&scan, fh, HEAD as u64, 30258),
                scan.bytes[0][HEAD as usize..]
            );
            scan.wait_until("one handle's pre-scan peek and scan decode land", || {
                scan.fixture
                    .find_item(scan.files[0])
                    .unwrap()
                    .content_presence
                    == ContentPresence::OnDisk
            });
        } else {
            for _ in 0..4 {
                read_on_handle(&scan, fh, 0, HEAD);
            }
            assert!(!scan.requests.download_states.is_downloading(scan.files[0]));
            assert_eq!(
                scan.fixture
                    .find_item(scan.files[0])
                    .unwrap()
                    .content_presence,
                ContentPresence::NotOnDisk
            );
            assert_eq!(
                scan.content_requests()
                    .iter()
                    .filter(|route| route.ends_with("!00.png"))
                    .count(),
                1
            );
        }
        scan.requests.answer_release(fh);
    }
}

#[test]
fn read_ahead_tiny_promotion_rechecks_policy_after_awaiting_the_store() {
    use crate::download_guard::{ProcessInfo, ProcessReader};
    struct Fake;
    impl ProcessReader for Fake {
        fn read(&self, _: u32) -> Option<ProcessInfo> {
            Some(ProcessInfo {
                start: 1,
                executable: "/usr/bin/gwenview".into(),
                cgroup: "0::/app-org.kde.gwenview-123.scope".into(),
                cmdline: Arc::new(b"gwenview\0".to_vec()),
            })
        }
    }
    let mut scan = ScanFixture::with_sizes(&[130], 0, Duration::ZERO);
    let mut guard = DownloadGuard::default();
    guard.set_process_reader(Arc::new(Fake));
    scan.requests.download_guard = Arc::new(guard);
    let scheduler = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fh = scan.requests.answer_open_to_read(scan.files[0]).unwrap().fh;
    let ReadAnswer::Range(range) = scan.requests.decide_read(fh, 0, HEAD).unwrap() else {
        panic!("range");
    };
    let key = DownloadKey {
        item: range.item,
        content: range.source.content.clone(),
    };
    scheduler.block_on(async {
        assert_eq!(
            scan.requests
                .read_range_as(range, Some(42), Some(ScanFixture::caller()))
                .await
                .unwrap(),
            scan.bytes[0]
        );
        tokio::time::pause();
        tokio::task::yield_now().await; // Arm the promotion's grace timer.
    });
    // Hold the read-only queue so the promotion must suspend at its row lookup.
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let store = scan.fixture.store.clone();
    let blocker = std::thread::spawn(move || {
        store
            .read_items_blocking(move |_| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            })
            .unwrap()
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    scheduler.block_on(async {
        tokio::time::advance(
            crate::mount::read_ahead::TINY_DOWNLOAD_DELAY + Duration::from_millis(1),
        )
        .await;
        tokio::task::yield_now().await; // Run the promotion up to the blocked store await.
        let mut limits = scan.requests.download_guard.limits();
        limits
            .blocked_apps
            .push(ScanFixture::caller().identity().into());
        scan.requests.download_guard.configure(true, limits);
        release.send(()).unwrap();
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if scan
                    .requests
                    .download_states
                    .delay_read(key.clone(), ScanFixture::caller().identity())
                    .is_some()
                {
                    break; // Its pending lease is dropped only after the task finishes.
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    blocker.join().unwrap();
    assert!(
        !scan.requests.download_states.is_downloading(scan.files[0]),
        "a block during the store await must prevent promotion"
    );
    assert_eq!(
        scan.fixture
            .find_item(scan.files[0])
            .unwrap()
            .content_presence,
        ContentPresence::NotOnDisk
    );
    assert_eq!(
        scan.content_requests().len(),
        1,
        "only the admitted range, no whole download"
    );
    assert!(
        scan.requests
            .download_guard
            .apps()
            .iter()
            .all(|app| app.files == 0 && app.bytes == 0),
        "a cancelled promotion charges nothing"
    );
    scan.requests.answer_release(fh);
}

#[test]
fn read_ahead_tiny_promotion_survives_reader_exit_without_charging_a_new_pid() {
    use crate::download_guard::{ProcessInfo, ProcessReader};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Fake(AtomicBool);
    impl ProcessReader for Fake {
        fn read(&self, _: u32) -> Option<ProcessInfo> {
            self.0.load(Ordering::Relaxed).then(|| ProcessInfo {
                start: 1,
                executable: "/usr/bin/gwenview".into(),
                cgroup: "0::/app-org.kde.gwenview-123.scope".into(),
                cmdline: Arc::new(b"gwenview\0".to_vec()),
            })
        }
    }
    let mut scan = ScanFixture::with_sizes(&[130], 16, Duration::ZERO);
    let fake = Arc::new(Fake(AtomicBool::new(true)));
    let mut guard = DownloadGuard::default();
    guard.set_process_reader(fake.clone());
    scan.requests.download_guard = Arc::new(guard);
    scan.read(0);
    fake.0.store(false, Ordering::Relaxed);
    scan.wait_until("an already charged promotion outlives the reader", || {
        scan.fixture
            .find_item(scan.files[0])
            .unwrap()
            .content_presence
            == ContentPresence::OnDisk
    });
    assert_eq!(scan.content_requests().len(), 2);
}
