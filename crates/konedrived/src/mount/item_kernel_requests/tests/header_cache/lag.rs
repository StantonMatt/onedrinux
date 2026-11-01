//! Two concurrent desktop readers over a restart, with real scan scheduling and Graph latency.
use super::*;
fn session(
    fixture: &ItemFixture,
    graph: &crate::fake_onedrive::FakeGraph,
    files: &[LocalItemId],
    headers: Option<Arc<HeaderCache>>,
) -> (Duration, usize, usize) {
    let source = Arc::new(GraphSource::with_headers(graph.client(), headers.clone()));
    let mut setup = fixture.mount_setup();
    setup.source = source.clone();
    setup.read_ahead = 16;
    setup.download_guard = Arc::new(DownloadGuard::new(
        true,
        GuardConfig {
            max_files: 1000,
            ..Default::default()
        },
    ));
    let (mount, requests) =
        crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
    graph.with(|cloud| {
        cloud.log.clear();
        cloud.delay("GET", "dl/", Duration::from_millis(300), 10000);
    });
    let read = |n: usize, from, wanted| {
        let opened = requests
            .open_to_read(files[n], Some(std::process::id()))
            .unwrap();
        let answer = match requests.decide_read(opened.fh, from, wanted).unwrap() {
            ReadAnswer::Range(range) => fixture
                .runtime()
                .block_on(requests.read_range_as(
                    range,
                    Some(std::process::id()),
                    Some(CachedMount::caller()),
                ))
                .unwrap(),
            ReadAnswer::Bytes(bytes) => bytes,
            _ => panic!("cloud-only range"),
        };
        requests.answer_release(opened.fh);
        let size = fixture
            .find_item(files[n])
            .unwrap()
            .local
            .unwrap()
            .content
            .unwrap()
            .size();
        assert_eq!(
            answer,
            vec![n as u8; size.saturating_sub(from).min(u64::from(wanted)) as usize]
        );
    };
    let began = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 0..files.len() {
                read(n, 0, 16384);
            }
        });
        scope.spawn(|| {
            for n in 0..files.len() {
                for (from, len) in [
                    (0, 4096),
                    (0, 33),
                    (33, 4096),
                    (65536, 4096),
                    (131072, 4096),
                ] {
                    read(n, from, len);
                }
            }
        });
    });
    let elapsed = began.elapsed();
    for item in files {
        assert_eq!(
            fixture.find_item(*item).unwrap().content_presence,
            ContentPresence::NotOnDisk,
            "range caches and metadata scans never hydrate a file"
        );
    }
    mount.stop();
    if let Some(headers) = headers {
        fixture.runtime().block_on(headers.flush());
    }
    let bytes = graph.with(|cloud| {
        cloud
            .log
            .iter()
            .filter(|(_, route)| route.starts_with("dl/"))
            .count()
    });
    (elapsed, source.foreground_range_requests(), bytes)
}

#[test]
fn header_cache_two_desktop_readers_after_restart_avoid_serial_graph_round_trips() {
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("Shots");
    let files: Vec<_> = (0..120)
        .map(|n| {
            fixture
                .show_onedrive_file(
                    &format!("Shots/{n:03}.png"),
                    &vec![n as u8; if n % 3 == 0 { 15153 } else { 160 * 1024 }],
                )
                .local_item_id
        })
        .collect();
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let first = session(
        &fixture,
        &graph,
        &files,
        Some(Arc::new(HeaderCache::open(path.clone(), 16 * 1024 * 1024))),
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let heads: i64 = db
        .query_row("SELECT count(*) FROM headers WHERE offset = 0", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(db);
    let headers = Arc::new(HeaderCache::open(path.clone(), 16 * 1024 * 1024));
    fixture.runtime().block_on(headers.flush());
    let warm = session(&fixture, &graph, &files, Some(headers));
    // Model old best-effort publications that retained tails but lost heads.
    let db = rusqlite::Connection::open(&path).unwrap();
    for (_, item) in files.iter().enumerate().filter(|(n, _)| n % 4 == 3) {
        let id = fixture
            .find_item(*item)
            .unwrap()
            .onedrive_item
            .unwrap()
            .item_id;
        db.execute(
            "DELETE FROM headers WHERE item = ?1 AND offset = 0",
            [id.as_str()],
        )
        .unwrap();
    }
    drop(db);
    let headers = Arc::new(HeaderCache::open(path, 16 * 1024 * 1024));
    fixture.runtime().block_on(headers.flush());
    let sparse = session(&fixture, &graph, &files, Some(headers));
    let off = session(&fixture, &graph, &files, None);
    eprintln!("two-reader restart benchmark (wall, foreground Graph range requests, total Graph content): first {first:?}, persisted heads {heads}/120, warm {warm:?}, sparse {sparse:?}, header_cache=0 {off:?}");
    assert!(warm.0 * 4 < off.0, "warm cache must beat no disk cache");
    assert!(
        warm.1 <= off.1,
        "the optional tier must not add foreground Graph requests"
    );
    assert!(warm.2 < off.2, "warm disk ranges avoid network requests");
    assert_eq!(heads, 120, "each first-session foreground head persists");
    assert!(
        sparse.1 <= off.1 && sparse.2 <= off.2,
        "missing disk heads must not cost more Graph requests than no disk cache"
    );
}
