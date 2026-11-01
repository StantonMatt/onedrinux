//! Local strace captures replayed against fake Graph, never the installed apps or
//! an account. See docs/app-read-patterns.md for capture and measurement limits.
use super::*;
use crate::content_source::graph_source::GraphSource;
use crate::content_source::source::{ContentSource, Fetched, ListedContent, SourceError};
use crate::download_guard::{DownloadGuard, ProcessInfo, ProcessReader};
use serde::Deserialize;
use std::collections::HashMap;

const PID: u32 = 42;

#[derive(Deserialize)]
struct Capture {
    name: String,
    executable: String,
    desktop: String,
    target: Option<usize>,
    thumbnail: bool,
    read_through: bool,
    files: Vec<(String, usize)>,
    // op (open/read/close), handle, file index or offset, read length.
    events: Vec<(u8, u64, u64, u32)>,
    // Event boundary at the first local-render screenshot, before navigation.
    startup: usize,
}

struct Processes(HashMap<u32, ProcessInfo>);
impl ProcessReader for Processes {
    fn read(&self, pid: u32) -> Option<ProcessInfo> {
        self.0.get(&pid).cloned()
    }
}

fn captures() -> Vec<Capture> {
    serde_json::from_str(include_str!("app_reads/traces.json")).unwrap()
}

/// Deterministic data makes offset/seek mistakes visible, without checking in
/// the generated media. Same sizes, handles and successful ranges as strace.
fn data(n: usize, size: usize) -> Vec<u8> {
    (0..size).map(|at| (at * 7 + at / 4096 + n * 13) as u8).collect()
}

fn replay(capture: &Capture, streaming: bool) {
    replay_with_landing(capture, streaming, None, false);
}

/// Test control retaining the original fresh whole-download path while forwarding
/// every playback/cache operation to the same Graph source.
struct WithoutReuse(GraphSource);
#[async_trait::async_trait]
impl ContentSource for WithoutReuse {
    async fn fetch(&self, id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError> {
        self.0.fetch(id, from, end).await
    }
    async fn read_range(&self, id: &str, opened: &konedrive_tree::ContentVersion, listed: Option<&ListedContent>, from: u64, end: u64) -> Result<Fetched, SourceError> {
        self.0.read_range(id, opened, listed, from, end).await
    }
    fn cached_range(&self, id: &str, opened: &konedrive_tree::ContentVersion, from: u64, end: u64) -> Option<Fetched> {
        self.0.cached_range(id, opened, from, end)
    }
    async fn prepared_range(&self, id: &str, opened: &konedrive_tree::ContentVersion, listed: Option<&ListedContent>, from: u64, end: u64) -> Option<Fetched> {
        self.0.prepared_range(id, opened, listed, from, end).await
    }
    async fn read_ahead(&self, id: &str, opened: &konedrive_tree::ContentVersion, listed: &ListedContent) {
        self.0.read_ahead(id, opened, listed).await;
    }
    fn forget(&self, id: &str) { self.0.forget(id); }
}

fn replay_with_landing(capture: &Capture, streaming: bool, slow: Option<bool>, baseline: bool) {
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("Samples");
    let bytes: Vec<_> = capture.files.iter().enumerate().map(|(n, (_, size))| data(n, *size)).collect();
    let files: Vec<_> = capture.files.iter().zip(&bytes).map(|((name, _), bytes)| {
        let mime = if name.ends_with(".mp4") { Some("video/mp4") }
            else if name.ends_with(".mkv") { Some("video/x-matroska") } else { None };
        fixture.show_onedrive_file_with_mime(&format!("Samples/{name}"), bytes, mime).local_item_id
    }).collect();
    // This untouched sibling proves that opening a target does not itself
    // launch a folder scan or hydrate a file that the application never read.
    let untouched = fixture.show_onedrive_file("Samples/untouched.bin", b"not requested").local_item_id;
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let proxy = slow.map(|_| fixture.runtime().block_on(graph.connection_faults()));
    let mut setup = fixture.mount_setup();
    let source = GraphSource::new(graph.client());
    setup.source = if baseline { Arc::new(WithoutReuse(source)) } else { Arc::new(source) };
    if streaming && slow.is_none() { setup.download_after = u64::MAX; }
    let cmdline = if capture.executable.ends_with("kioworker") {
        Some(b"kioworker\0/usr/lib/qt6/plugins/kf6/kio/thumbnail.so\0".to_vec())
    } else if !streaming {
        capture.target.map(|n| format!("{}\0{}\0", capture.executable, fixture.folder().join("Samples").join(&capture.files[n].0).display()).into_bytes())
    } else { None }.unwrap_or_else(|| format!("{}\0", capture.executable).into_bytes());
    let child = ProcessInfo {
        start: 2, parent: if capture.thumbnail { 41 } else { 0 },
        executable: capture.executable.clone(), cgroup: format!("0::/app-{}-123.scope", capture.desktop),
        cmdline: Arc::new(cmdline),
    };
    let parent = ProcessInfo {
        start: 1, parent: 0, executable: "/usr/lib/kf6/kioworker".into(),
        cgroup: child.cgroup.clone(),
        cmdline: Arc::new(b"kioworker\0/usr/lib/qt6/plugins/kf6/kio/thumbnail.so\0".to_vec()),
    };
    let mut guard = DownloadGuard::default();
    guard.set_process_reader(Arc::new(Processes(HashMap::from([(PID, child), (41, parent)]))));
    let guard = Arc::new(guard);
    setup.download_guard = guard.clone();
    let (mount, requests) = crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
    graph.with(|cloud| {
        cloud.log.clear();
        if slow.is_none() { cloud.delay("GET", "dl/", Duration::from_millis(300), 10000); }
    });
    if let Some(slow) = slow {
        // Only the whole-copy response is delayed: playback ranges remain fast.
        // All complete response bodies are counted below, after verified landing.
        let n = capture.target.unwrap();
        let etag = fixture.find_item(files[n]).unwrap().onedrive_etag.unwrap();
        let name = capture.files[n].0.clone();
        let body = bytes[n].clone();
        fixture.runtime().block_on(async {
            let wanted = name.clone();
            let size = body.len() as u64;
            wiremock::Mock::given(move |request: &wiremock::Request| {
                if !request.url.path().starts_with("/dl/") || !request.url.path().ends_with(&wanted) { return false; }
                let range = request.headers.get("range").map(|r| r.to_str().unwrap()).unwrap_or("bytes=0-");
                let (from, last) = range.strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                let from: u64 = from.parse().unwrap();
                let end = if last.is_empty() { size } else { last.parse::<u64>().unwrap() + 1 };
                // Captured playback reads span at most two aligned blocks. Delay
                // the whole copy's larger missing intervals, honoring its Range.
                end.saturating_sub(from) > 2 * 1024 * 1024
            }).respond_with(move |request: &wiremock::Request| {
                let range = request.headers.get("range").map(|r| r.to_str().unwrap()).unwrap_or("bytes=0-");
                let (from, last) = range.strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                let from: usize = from.parse().unwrap();
                let end = if last.is_empty() { body.len() } else { last.parse::<usize>().unwrap() + 1 };
                wiremock::ResponseTemplate::new(206).insert_header("ETag", etag.as_str())
                    .insert_header("Content-Range", format!("bytes {from}-{}/{}", end - 1, body.len()))
                    .set_body_bytes(body[from..end].to_vec())
                    .set_delay(if slow { Duration::from_secs(10) } else { Duration::ZERO })
            }).with_priority(1).mount(&graph.server).await;
        });
    }

    let began = Instant::now();
    if capture.events.is_empty() {
        let directory = fixture.find_item_at("Samples").unwrap().local_item_id;
        let fh = requests.answer_opendir(directory).unwrap();
        let listed = requests.answer_readdir(directory, fh, 0).unwrap();
        assert_eq!(listed.len(), files.len() + 1);
        for item in listed.iter() { assert!(requests.describe_listed_item(item.clone()).is_some()); }
        requests.answer_releasedir(fh);
        assert!(graph.with(|cloud| cloud.log.is_empty()), "browsing names/attributes fetches no content");
    }
    let mut startup = Duration::ZERO;
    let mut first = None;
    let mut handles = HashMap::new();
    let mut returned = 0;
    let mut fast_copy_landed = false;
    for (i, &(op, handle, at, size)) in capture.events.iter().enumerate() {
        if i == capture.startup { startup = began.elapsed(); }
        match op {
            0 => {
                let opened = requests.open_to_read(files[at as usize], Some(PID));
                if capture.thumbnail {
                    assert!(matches!(opened, Err(Errno::EACCES)), "{}: thumbnail source open must be refused", capture.name);
                } else {
                    handles.insert(handle, (opened.unwrap().fh, at as usize));
                }
            }
            1 if !capture.thumbnail => {
                let &(fh, n) = handles.get(&handle).expect("captured live handle");
                let answer = match requests.decide_read(fh, at, size).unwrap() {
                    ReadAnswer::Bytes(bytes) => bytes,
                    ReadAnswer::Range(range) => fixture.runtime().block_on(requests.read_range(range, Some(PID))).unwrap(),
                    _ => panic!("unchanged fake content"),
                };
                assert_eq!(answer, bytes[n][at as usize..at as usize + size as usize], "{}: read {i}", capture.name);
                returned += answer.len();
                first.get_or_insert_with(|| began.elapsed());
                // Model a copy completing before subsequent playback, without a
                // scheduler-dependent sleep. Slow mode instead runs playback ahead.
                if slow == Some(false) && capture.target == Some(n) && !fast_copy_landed
                    && (requests.download_states.is_downloading(files[n])
                        || fixture.find_item(files[n]).unwrap().content_presence == ContentPresence::OnDisk) {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while fixture.find_item(files[n]).unwrap().content_presence != ContentPresence::OnDisk {
                        assert!(Instant::now() < deadline, "fast whole copy completes before playback resumes");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    fast_copy_landed = true;
                }
            }
            2 if !capture.thumbnail => {
                let (fh, _) = handles.remove(&handle).expect("captured handle closes once");
                requests.answer_release(fh);
            }
            _ => {}
        }
    }
    if capture.startup >= capture.events.len() { startup = began.elapsed(); }
    assert!(handles.is_empty());
    let replay_elapsed = began.elapsed();
    let landed_during_replay = capture.target.is_some_and(|n| fixture.find_item(files[n]).unwrap().content_presence == ContentPresence::OnDisk);
    if slow == Some(false) { assert!(landed_during_replay, "fast copy must land before playback ends"); }
    if slow == Some(true) { assert!(!landed_during_replay, "slow copy must outlast playback"); }
    if (slow.is_some() || !streaming || capture.read_through) && !capture.thumbnail {
        if let Some(n) = capture.target {
            let deadline = Instant::now() + Duration::from_secs(30);
            while fixture.find_item(files[n]).unwrap().content_presence != ContentPresence::OnDisk || requests.download_states.is_downloading(files[n]) {
                assert!(Instant::now() < deadline, "{}: launched or completely read target must land", capture.name);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    assert_eq!(fixture.find_item(untouched).unwrap().content_presence, ContentPresence::NotOnDisk);
    for (n, file) in files.iter().enumerate() {
        if capture.thumbnail || (capture.desktop == "org.kde.haruna" && capture.target != Some(n)) {
            assert_eq!(fixture.find_item(*file).unwrap().content_presence, ContentPresence::NotOnDisk, "{}: neighbour or thumbnail stays cloud-only", capture.name);
            assert!(!requests.download_states.is_downloading(*file));
        }
    }
    if capture.thumbnail {
        assert!(graph.with(|cloud| cloud.log.is_empty()), "thumbnail helpers make no Graph request");
        assert!(guard.launch_process(PID).is_none());
    } else {
        assert!(guard.apps().is_empty(), "ordinary desktop apps consume no bulk budget");
    }
    let asked = fixture.runtime().block_on(graph.server.received_requests()).unwrap();
    let mut content = 0;
    let mut fetched = 0;
    for request in &asked {
        if !request.url.path().starts_with("/dl/") { continue; }
        content += 1;
        let range = request.headers.get("range").map(|value| value.to_str().unwrap()).unwrap_or("bytes=0-").strip_prefix("bytes=").unwrap();
        let (from, end) = range.split_once('-').unwrap();
        let from: u64 = from.parse().unwrap();
        let size = capture.files.iter().find(|(name, _)| request.url.path().ends_with(name)).map(|(_, size)| *size as u64).unwrap();
        let end = if end.is_empty() { size } else { end.parse::<u64>().unwrap() + 1 };
        fetched += end.saturating_sub(from);
    }
    if let Some(proxy) = &proxy {
        let wire: Vec<_> = capture.files.iter().map(|(name, _)| proxy.asked().iter()
            .filter(|r| r.path.starts_with("/dl/") && r.path.ends_with(name)).map(|r| r.response_bytes).sum::<u64>()).collect();
        eprintln!("video-wire {} baseline={baseline} landed_during_replay={landed_during_replay} argv={} slow={} sizes={:?} body_bytes={wire:?}", capture.name, !streaming, slow.unwrap(), capture.files.iter().map(|(_, s)| s).collect::<Vec<_>>());
    }
    eprintln!("app-replay {} streaming={streaming} slow={slow:?} replay={replay_elapsed:?}: first={first:?} startup={startup:?} total={:?} Graph={} content={content} requested={returned} fetched={fetched}", capture.name, began.elapsed(), asked.len());
    mount.stop();
}

#[test]
fn captured_desktop_reads_stream_seek_and_land_without_bulk_limits() {
    for capture in captures().iter().filter(|c| !c.thumbnail) {
        replay(capture, false);
    }
}

#[test]
fn captured_desktop_reads_without_launch_intent_reuse_ranges_under_graph_latency() {
    for capture in captures().iter().filter(|c| !c.thumbnail) {
        replay(capture, true);
    }
}

#[test]
fn captured_thumbnail_workers_and_ghostscript_helpers_never_fetch_cloud_content() {
    for capture in captures().iter().filter(|c| c.thumbnail) {
        replay(capture, false);
    }
}

#[test]
fn captured_video_default_landing_wire_bytes() {
    for capture in captures().iter().filter(|c| matches!(c.name.as_str(), "vlc-watch" | "haruna-x-start")) {
        for streaming in [false, true] {
            for slow in [false, true] { replay_with_landing(capture, streaming, Some(slow), false); }
        }
    }
}

#[test]
fn captured_video_default_landing_wire_bytes_without_reuse() {
    for capture in captures().iter().filter(|c| matches!(c.name.as_str(), "vlc-watch" | "haruna-x-start")) {
        for streaming in [false, true] {
            for slow in [false, true] { replay_with_landing(capture, streaming, Some(slow), true); }
        }
    }
}
