//! The attribute and announcements while a real range-fetch future waits, ends, fails or
//! is aborted. The source is held, never a mount or an account.
use super::*;
use crate::content_source::source::{ContentSource, Fetched, ListedContent, SourceError};
use crate::mount::directory_states::DirectoryStates;
use crate::mount::item_announcements::ItemAnnouncement;
use crate::mount::range_read_states::SHOW_AFTER;
use crate::mount::range_readers::{RangeReaders, READ_LINGERS};
use crate::status::throttle_wait::ThrottleWait;
use konedrive_graph::pool::Throughput;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

struct HeldRanges {
    inner: Arc<dyn ContentSource>,
    gate: tokio::sync::Semaphore,
    entered: AtomicUsize,
    refusal: Mutex<Option<fn() -> SourceError>>,
}
#[async_trait::async_trait]
impl ContentSource for HeldRanges {
    async fn fetch(&self, id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError> {
        self.inner.fetch(id, from, end).await
    }
    async fn read_range(
        &self,
        id: &str,
        content: &ContentVersion,
        listed: Option<&ListedContent>,
        from: u64,
        end: u64,
    ) -> Result<Fetched, SourceError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.gate.acquire().await.unwrap().forget();
        let refusal = *self.refusal.lock().unwrap();
        match refusal {
            Some(refusal) => Err(refusal()),
            None => self.inner.read_range(id, content, listed, from, end).await,
        }
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(6);
    while !condition() {
        assert!(Instant::now() < limit, "the state did not arrive");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn held_mount() -> (Requests, Arc<HeldRanges>, Arc<DirectoryStates>, LocalItemId) {
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("docs");
    let item = fixture
        .show_onedrive_file("docs/photo.png", &[7; 65536])
        .local_item_id;
    let source = Arc::new(HeldRanges {
        inner: fixture.content_source(),
        gate: tokio::sync::Semaphore::new(0),
        entered: AtomicUsize::new(0),
        refusal: Mutex::new(None),
    });
    let mut mount = Requests::over(fixture, Arc::new(ItemInotifyBridge::new(PathBuf::new())));
    let f = &mount.fixture;
    let states = DirectoryStates::build(
        &f.store,
        f.download_states.clone(),
        f.running_steps.clone(),
        f.published_plan.clone(),
        f.inotify_bridge.clone(),
    )
    .unwrap();
    let readers = Arc::new(
        RangeReaders::new(f.pool.clone(), f.runtime())
            .with_read_states(f.download_states.range_reads.clone()),
    );
    let requests = Arc::get_mut(&mut mount.requests).unwrap();
    requests.range_fetcher = Arc::new(ItemRangeFetcher::new(
        readers,
        source.clone(),
        ThrottleWait::new(f.sync_state.clone()),
    ));
    requests.lookup = Arc::new(
        ItemLookup::new(
            f.read_connections.clone(),
            f.place_changes.clone(),
            f.item_handles.clone(),
            f.download_states.clone(),
            f.access_mode_switch.clone(),
            f.running_steps.clone(),
            f.published_plan.clone(),
            f.backing_directory.clone(),
            START_TIME,
        )
        .with_directory_states(states.clone()),
    );
    (mount, source, states, item)
}

fn state(mount: &Requests, item: LocalItemId) -> String {
    String::from_utf8(
        mount
            .requests
            .lookup
            .find_xattr(item, OsStr::new(konedrive_fs::placeholder::XATTR_STATE))
            .unwrap(),
    )
    .unwrap()
}
fn marks(mount: &Requests, item: LocalItemId) -> usize {
    mount.fixture.inotify_bridge.list_queued().iter().flatten().filter(|announcement| matches!(announcement, ItemAnnouncement::MarksChanged { item: changed, .. } if *changed == item)).count()
}
struct CloseReader {
    requests: Arc<ItemKernelRequests>,
    fh: Fh,
}
impl Drop for CloseReader {
    fn drop(&mut self) {
        self.requests.answer_release(self.fh);
    }
}

fn begin(mount: &Requests, item: LocalItemId) -> tokio::task::JoinHandle<Result<Vec<u8>, Errno>> {
    let opened = mount.open_to_read(item).unwrap();
    let ReadAnswer::Range(range) = mount.decide_read(opened.fh, 0, 4096).unwrap() else {
        panic!("a range");
    };
    let requests = mount.requests.clone();
    let reader = CloseReader {
        requests: requests.clone(),
        fh: opened.fh,
    };
    mount.fixture.runtime().spawn(async move {
        let _reader = reader;
        requests.fetch_read_range(range).await
    })
}
fn shown(mount: &Requests, source: &HeldRanges, item: LocalItemId) {
    wait_for(|| source.entered.load(Ordering::SeqCst) > 0);
    assert_eq!(state(mount, item), "online-only");
    assert_eq!(marks(mount, item), 0);
    wait_for(|| state(mount, item) == "hydrating");
    wait_for(|| marks(mount, item) == 1);
}
fn ended(mount: &Requests, item: LocalItemId) {
    wait_for(|| state(mount, item) == "online-only");
    wait_for(|| marks(mount, item) == 2);
    assert!(!mount.fixture.download_states.is_downloading(item));
}

#[test]
fn a_slow_range_changes_the_attribute_and_ancestors_once_at_each_end() {
    let (mount, source, states, item) = held_mount();
    let docs = mount.fixture.find_item_at("docs").unwrap().local_item_id;
    let read = begin(&mount, item);
    shown(&mount, &source, item);
    assert_eq!(state(&mount, docs), "hydrating");
    assert_eq!(state(&mount, ROOT), "hydrating");
    assert_eq!(
        states.directory_values(ROOT).0,
        konedrive_fs::placeholder::State::Hydrating
    );
    source.gate.add_permits(1);
    assert_eq!(
        mount.fixture.runtime().block_on(read).unwrap().unwrap(),
        [7; 4096]
    );
    assert_eq!(
        state(&mount, item),
        "hydrating",
        "shown through the read linger"
    );
    ended(&mount, item);
    assert_eq!(state(&mount, docs), "online-only");
    assert_eq!(state(&mount, ROOT), "online-only");
}

fn failed_range(refusal: fn() -> SourceError, errno: Errno) {
    let (mount, source, _, item) = held_mount();
    *source.refusal.lock().unwrap() = Some(refusal);
    let read = begin(&mount, item);
    shown(&mount, &source, item);
    source.gate.add_permits(1);
    assert_eq!(mount.fixture.runtime().block_on(read).unwrap(), Err(errno));
    ended(&mount, item);
}
#[test]
fn a_failed_range_ends_its_hydrating_state() {
    failed_range(|| SourceError::Transient("offline".into()), Errno::EIO);
}
#[test]
fn a_refused_or_stale_range_ends_its_hydrating_state() {
    failed_range(|| SourceError::NotFound("gone".into()), Errno::ESTALE);
}
#[test]
fn a_range_refused_by_the_sources_throttle_ends_its_hydrating_state() {
    failed_range(
        || SourceError::Throttled {
            detail: "429".into(),
            retry_after: Some(Duration::from_secs(30)),
        },
        Errno::EAGAIN,
    );
}
#[test]
fn an_interrupt_drops_a_range_and_ends_its_hydrating_state() {
    let (mount, source, _, item) = held_mount();
    let read = begin(&mount, item);
    shown(&mount, &source, item);
    read.abort();
    assert!(mount
        .fixture
        .runtime()
        .block_on(read)
        .unwrap_err()
        .is_cancelled());
    ended(&mount, item);
}
#[test]
fn a_throttle_wait_cancels_a_slow_range_and_later_refusals_never_show_it() {
    let (mount, source, _, item) = held_mount();
    let read = begin(&mount, item);
    shown(&mount, &source, item);
    mount.fixture.sync_state.set_throughput(Throughput {
        retry_after: 30,
        ..Default::default()
    });
    assert_eq!(
        mount.fixture.runtime().block_on(read).unwrap(),
        Err(Errno::EAGAIN)
    );
    ended(&mount, item);
    assert!(
        matches!(mount.open_to_read(item), Err(Errno::EAGAIN)),
        "a later open is refused before it can begin fetching"
    );
    std::thread::sleep(SHOW_AFTER + Duration::from_millis(30));
    assert_eq!(marks(&mount, item), 2);
    assert_eq!(source.entered.load(Ordering::SeqCst), 1);
}
#[test]
fn disk_reads_and_immediate_memory_answers_never_show_hydrating() {
    let (mount, source, _, item) = held_mount();
    source.gate.add_permits(100);
    // Stay below the cumulative whole-download threshold: this checks memory alone.
    for _ in 0..5 {
        assert_eq!(
            mount
                .fixture
                .runtime()
                .block_on(begin(&mount, item))
                .unwrap()
                .unwrap(),
            [7; 4096]
        );
    }
    std::thread::sleep(SHOW_AFTER + Duration::from_millis(30));
    assert_eq!(marks(&mount, item), 0);
    mount.fixture.land_copy(item, &[7; 65536]);
    let opened = mount.open_to_read(item).unwrap();
    assert!(matches!(
        mount.decide_read(opened.fh, 0, 4096).unwrap(),
        ReadAnswer::Bytes(_)
    ));
    std::thread::sleep(READ_LINGERS);
    assert_eq!(state(&mount, item), "hydrated");
    assert_eq!(marks(&mount, item), 0);
    assert_eq!(source.entered.load(Ordering::SeqCst), 5);
}

#[test]
fn fifty_quick_cloud_files_and_their_cached_ranges_make_no_state_announcement() {
    use crate::content_source::graph_source::GraphSource;
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("shots");
    let files: Vec<_> = (0..50)
        .map(|n| {
            fixture
                .show_onedrive_file(&format!("shots/{n}.png"), &[n; 20480])
                .local_item_id
        })
        .collect();
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let mut setup = fixture.mount_setup();
    setup.pool = konedrive_graph::pool::TransferPool::new(128);
    setup.source = Arc::new(GraphSource::new(graph.client()));
    let (mount, requests) =
        crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
    let bridge = mount.folder_step_parts().inotify_bridge;
    graph.with(|cloud| cloud.log.clear());
    for (from, size) in [(0, 16384), (16384, 1024)] {
        for (n, &file) in files.iter().enumerate() {
            let opened = requests.open_to_read(file, None).unwrap();
            let ReadAnswer::Range(range) = requests.decide_read(opened.fh, from, size).unwrap()
            else {
                panic!("a range");
            };
            assert_eq!(
                fixture
                    .runtime()
                    .block_on(requests.fetch_read_range(range))
                    .unwrap(),
                vec![n as u8; size as usize]
            );
            requests.answer_release(opened.fh);
        }
    }
    std::thread::sleep(SHOW_AFTER + Duration::from_millis(30));
    assert_eq!(
        graph.with(|cloud| cloud
            .log
            .iter()
            .filter(|(method, route)| method == "GET" && route.starts_with("dl/"))
            .count()),
        50,
        "the second ranges all came from the source's memory cache"
    );
    assert_eq!(bridge.list_queued().iter().flatten().filter(|announcement| matches!(announcement, ItemAnnouncement::MarksChanged { item, .. } if files.contains(item))).count(), 0);
    for item in files {
        assert_eq!(
            requests
                .lookup
                .find_xattr(item, OsStr::new(konedrive_fs::placeholder::XATTR_STATE))
                .unwrap(),
            b"online-only"
        );
    }
}
