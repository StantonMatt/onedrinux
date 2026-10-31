//! A short desktop folder scan warms the existing version-checked range store.
//!
//! One scan per mount bounds speculation across programs and folders. Its 16-file
//! window holds at most 16 MiB, with six requests in flight, independently of the
//! foreground transfer pool. Only scan-started contents skip the whole-download
//! threshold; contents already being read keep counting. Speculation touches no disk.

// test: a_desktop_scan_reads_forty_heads_at_least_four_times_faster_with_one_request_each
// test: a_scan_only_fetches_matching_cloud_files_within_its_window
// test: blocking_or_stopping_a_scan_cancels_pending_read_ahead
// test: a_real_read_does_not_wait_for_other_prefetches
// test: read_ahead_activating_tiny_read_skips_counting_without_polling_the_actor
// test: read_ahead_scan_reads_small_files_in_full_without_keeping_them
// test: read_ahead_lifecycle_changes_end_synchronous_suppression
// test: read_ahead_skips_non_head_ranges_and_preserves_pre_scan_coverage
// test: read_ahead_keeps_the_opened_image_after_a_cold_fetch_outlives_the_scan
// test: read_ahead_keeps_the_opened_image_while_its_first_counted_fetch_is_in_flight

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use konedrive_tree::{ContentPresence, ContentVersion, Item, ItemKind, LocalItemId, Store};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::item_download_states::{ItemDownloadStates, ReadCount};
use super::item_read_origin::{decide_listed_content, RangeSource};
use crate::content_source::source::ContentSource;
use crate::download_guard::{DownloadGuard, ReadAheadCaller};
use crate::folder::disk::is_hidden_name;

const IDLE: Duration = Duration::from_secs(3);
const LIFETIME: Duration = Duration::from_secs(5 * 60);
const CONCURRENT: usize = 6;
const HEAD_END: u64 = 1024 * 1024;

enum Event {
    Listed(LocalItemId, Arc<Vec<Item>>),
    Head {
        caller: ReadAheadCaller,
        item: LocalItemId,
        content: ContentVersion,
        at: Instant,
        scan: ScanRecord,
        extension: String,
    },
}

/// Folder facts from the same row that decided the foreground range's origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScanFile {
    folder: LocalItemId,
    extension: Option<String>,
}

impl ScanFile {
    pub(super) fn of(row: &Item) -> Option<Self> {
        (row.kind == ItemKind::File && row.content_presence == ContentPresence::NotOnDisk)
            .then_some(())?;
        Some(Self {
            folder: row.local.as_ref()?.place.as_ref()?.parent_local_item_id,
            extension: extension(row),
        })
    }
}

#[derive(Clone)]
struct ScanRecord {
    id: u64,
    caller: ReadAheadCaller,
    folder: LocalItemId,
    first: LocalItemId,
    active: bool,
    began: Instant,
    last: Instant,
}

/// Detection is synchronous; the actor only schedules speculation. No lock spans an await.
struct ScanState {
    scan: Option<ScanRecord>,
    next: u64,
    stopped: bool,
    counted: Vec<ReadCount>,
}

impl ScanState {
    fn refresh(&mut self, guard: &DownloadGuard) {
        if self.scan.as_ref().is_some_and(|s| {
            s.last.elapsed() >= IDLE
                || s.began.elapsed() >= LIFETIME
                || !guard.permits_read_ahead(&s.caller)
        }) {
            self.scan = None;
            self.counted.clear();
        }
    }
}

/// A cancelled or failed foreground read cancels only the scan it belonged to.
pub(super) struct HeadRead {
    state: Arc<Mutex<ScanState>>,
    cancel: watch::Sender<u64>,
    scan: u64,
    pub(super) counting: Option<ReadCount>,
    pub(super) completed: bool,
}

impl Drop for HeadRead {
    fn drop(&mut self) {
        if !self.completed {
            let mut state = crate::panic::lock(&self.state);
            if state.scan.as_ref().is_some_and(|s| s.id == self.scan) {
                state.scan = None;
                state.counted.clear();
            }
            self.cancel.send_replace(self.scan);
        }
    }
}

pub(super) struct ReadAhead {
    events: mpsc::Sender<Event>,
    cancel: watch::Sender<u64>,
    state: Arc<Mutex<ScanState>>,
    guard: Arc<DownloadGuard>,
    downloads: Arc<ItemDownloadStates>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ReadAhead {
    pub(super) fn new(
        store: Store,
        source: Arc<dyn ContentSource>,
        guard: Arc<DownloadGuard>,
        downloads: Arc<ItemDownloadStates>,
        window: usize,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        let (events, receiver) = mpsc::channel(64);
        let (cancel, cancelled) = watch::channel(0);
        let state = Arc::new(Mutex::new(ScanState {
            scan: None,
            next: 0,
            stopped: window == 0,
            counted: Vec::new(),
        }));
        let task = (window > 0).then(|| {
            runtime.spawn(run(
                store,
                source,
                Arc::clone(&guard),
                Arc::clone(&state),
                window.min(crate::config::READ_AHEAD),
                receiver,
                cancelled,
            ))
        });
        Self {
            events,
            cancel,
            state,
            guard,
            downloads,
            task,
        }
    }

    pub(super) fn listed(&self, folder: LocalItemId, items: Arc<Vec<Item>>) {
        let _ = self.events.try_send(Event::Listed(folder, items));
    }

    /// Observe every admitted desktop range before fetching it. The activating head
    /// already skips counting for new content even if the scheduling task has never
    /// been polled. Counting registration and scan activation share this lock.
    pub(super) fn head(
        &self,
        caller: ReadAheadCaller,
        item: LocalItemId,
        content: &ContentVersion,
        file: &ScanFile,
        from: u64,
    ) -> Option<HeadRead> {
        let mut state = crate::panic::lock(&self.state);
        state.refresh(&self.guard);
        if state.stopped || !self.guard.permits_read_ahead(&caller) {
            return None;
        }
        let at = Instant::now();
        let same = state
            .scan
            .as_ref()
            .is_some_and(|s| s.folder == file.folder && s.caller.identity() == caller.identity());
        if from < HEAD_END && content.size() > 0 {
            if let Some(ext) = &file.extension {
                if !same {
                    state.counted.clear();
                    state.next += 1;
                    state.scan = Some(ScanRecord {
                        id: state.next,
                        caller: caller.clone(),
                        folder: file.folder,
                        first: item,
                        active: false,
                        began: at,
                        last: at,
                    });
                }
                let activating = state
                    .scan
                    .as_ref()
                    .is_some_and(|s| !s.active && item != s.first);
                if activating {
                    state.counted = self.downloads.keep_counted_reads();
                }
                let s = state.scan.as_mut()?;
                if !s.active && item != s.first {
                    s.active = true;
                    tracing::debug!(folder = %s.folder, identity = s.caller.identity(),
                        "scan-started file reads are served in memory and are not kept on disk");
                }
                s.last = at;
                // A full scheduling queue may drop speculation, never scan detection.
                let _ = self.events.try_send(Event::Head {
                    caller: s.caller.clone(),
                    item,
                    content: content.clone(),
                    at,
                    scan: s.clone(),
                    extension: ext.clone(),
                });
            }
        }
        let s = state
            .scan
            .as_ref()
            .filter(|s| s.folder == file.folder && s.caller.identity() == caller.identity())?;
        Some(HeadRead {
            state: Arc::clone(&self.state),
            cancel: self.cancel.clone(),
            scan: s.id,
            counting: self.downloads.begin_read_count(item, content, s.active),
            completed: false,
        })
    }

    pub(super) fn stop(&self) {
        let mut state = crate::panic::lock(&self.state);
        state.stopped = true;
        state.scan = None;
        state.counted.clear();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Scan {
    record: ScanRecord,
    extensions: HashSet<String>,
    window: Vec<Item>,
    attempted: HashSet<(LocalItemId, ContentVersion)>,
}

fn extension(item: &Item) -> Option<String> {
    let name = &item.local.as_ref()?.place.as_ref()?.name;
    if is_hidden_name(name) {
        return None;
    }
    std::path::Path::new(name)
        .extension()?
        .to_str()
        .filter(|ext| !ext.is_empty())
        .map(str::to_ascii_lowercase)
}

/// The same listed proof as a real range. The file must still be cloud-only, in
/// the folder being scanned on both sides, and of its currently shown generation.
fn candidate(item: &Item, folder: LocalItemId, folder_id: &str) -> Option<RangeSource> {
    if item.kind != ItemKind::File || item.content_presence != ContentPresence::NotOnDisk {
        return None;
    }
    let local = item.local.as_ref()?;
    if local.place.as_ref()?.parent_local_item_id != folder
        || item.onedrive.as_ref()?.place.as_ref()?.parent_local_item_id != folder
    {
        return None;
    }
    let content = local.content.as_ref()?;
    let mut listed = decide_listed_content(item, content)?;
    listed.folder_id = Some(folder_id.to_owned());
    Some(RangeSource {
        onedrive_item: item.onedrive_item.clone()?,
        content: content.clone(),
        listed: Some(listed),
    })
}

async fn run(
    store: Store,
    source: Arc<dyn ContentSource>,
    guard: Arc<DownloadGuard>,
    state: Arc<Mutex<ScanState>>,
    window: usize,
    mut events: mpsc::Receiver<Event>,
    mut cancelled: watch::Receiver<u64>,
) {
    let mut policy = guard.read_ahead_changes();
    let mut scan: Option<Scan> = None;
    let mut listing: Option<(LocalItemId, Arc<Vec<Item>>)> = None;
    let mut pending: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();
    let mut idle = Instant::now() + IDLE;
    loop {
        // Policy notifications and cancellation win over launching another request.
        tokio::select! {
            biased;
            changed = policy.changed() => {
                if changed.is_err() { return; }
                // The guard also notifies on every refusal; only the scan's own caller losing
                // permission ends it.
                if scan.as_ref().is_some_and(|s| !guard.permits_read_ahead(&s.record.caller)) {
                    pending.clear(); scan = None; listing = None;
                }
            }
            changed = cancelled.changed() => {
                if changed.is_err() { return; }
                if scan.as_ref().is_some_and(|s| *cancelled.borrow() == s.record.id) {
                    pending.clear(); scan = None; listing = None;
                }
            }
            event = events.recv() => {
                let Some(event) = event else { return; };
                match event {
                    Event::Listed(folder, items) => { listing = Some((folder, items)); if scan.is_none() { idle = Instant::now() + IDLE; } }
                    Event::Head { caller, item, content, at, scan: record, extension: ext } => {
                        let current = {
                            let mut state = crate::panic::lock(&state);
                            state.refresh(&guard);
                            state.scan.as_ref().is_some_and(|s| s.id == record.id)
                        };
                        if !current || !guard.permits_read_ahead(&caller) || at.elapsed() >= IDLE { continue; }
                        let facts = store.read_items(move |reader| {
                            let Some(row) = reader.find_item(item)? else { return Ok(None); };
                            if row.content_presence != ContentPresence::NotOnDisk || row.kind != ItemKind::File
                                || row.local.as_ref().and_then(|local| local.content.as_ref()) != Some(&content) { return Ok(None); }
                            let Some(place) = row.local.as_ref().and_then(|local| local.place.as_ref()) else { return Ok(None); };
                            let folder = place.parent_local_item_id;
                            Ok(Some((folder, reader.list_children(folder)?)))
                        }).await;
                        let Ok(Some((folder, rows))) = facts else { continue; };
                        if folder != record.folder { continue; }
                        if scan.as_ref().is_none_or(|s| s.record.id != record.id) {
                            pending.clear();
                            scan = Some(Scan { record: record.clone(), extensions: HashSet::new(), window: Vec::new(), attempted: HashSet::new() });
                        }
                        let s = scan.as_mut().unwrap();
                        s.record = record;
                        s.extensions.insert(ext);
                        idle = (at + IDLE).min(s.record.began + LIFETIME);
                        let ordered = listing.as_ref().filter(|(id, _)| *id == folder).map(|(_, rows)| rows.as_slice()).unwrap_or(&rows);
                        s.window = ordered.iter().position(|row| row.local_item_id == item).map(|at| ordered[at + 1..].iter()
                            .filter(|row| row.kind == ItemKind::File && row.content_presence == ContentPresence::NotOnDisk
                                && extension(row).is_some_and(|ext| s.extensions.contains(&ext)))
                            .take(window).cloned().collect()).unwrap_or_default();
                    }
                }
            }
            _ = tokio::time::sleep_until(idle) => {
                pending.clear(); scan = None; listing = None;
                // An empty actor holds no rows, URLs or content; it waits for the next head.
                idle = Instant::now() + LIFETIME;
            }
            _ = pending.next(), if !pending.is_empty() => {}
        }
        let current = {
            let mut state = crate::panic::lock(&state);
            state.refresh(&guard);
            state.scan.as_ref().map(|s| s.id)
        };
        if scan.as_ref().is_some_and(|s| Some(s.record.id) != current) {
            pending.clear();
            scan = None;
        }
        if let Some(s) = scan.as_mut().filter(|s| {
            s.record.active
                && Some(s.record.id) == current
                && guard.permits_read_ahead(&s.record.caller)
        }) {
            for row in &s.window {
                if pending.len() >= CONCURRENT {
                    break;
                }
                let Some(content) = row.local.as_ref().and_then(|l| l.content.clone()) else {
                    continue;
                };
                let key = (row.local_item_id, content.clone());
                if !s.attempted.insert(key) {
                    continue;
                }
                let (store, source, guard, caller, folder, item) = (
                    store.clone(),
                    Arc::clone(&source),
                    Arc::clone(&guard),
                    s.record.caller.clone(),
                    s.record.folder,
                    row.local_item_id,
                );
                pending.push(Box::pin(async move {
                    // Revalidate on launch: a listing snapshot never proves residency now.
                    let fresh = store
                        .read_items(move |reader| {
                            let Some(row) = reader.find_item(item)? else {
                                return Ok(None);
                            };
                            if row.local.as_ref().and_then(|l| l.content.as_ref()) != Some(&content)
                            {
                                return Ok(None);
                            }
                            let Some(binding) =
                                reader.find_item(folder)?.and_then(|row| row.onedrive_item)
                            else {
                                return Ok(None);
                            };
                            Ok(candidate(&row, folder, binding.item_id.as_str())
                                .filter(|source| source.onedrive_item.drive_id == binding.drive_id))
                        })
                        .await;
                    if let Ok(Some(range)) = fresh {
                        if guard.permits_read_ahead(&caller) {
                            source
                                .read_ahead(
                                    range.onedrive_item.item_id.as_str(),
                                    &range.content,
                                    range.listed.as_ref().unwrap(),
                                )
                                .await;
                        }
                    }
                }));
            }
        }
    }
}
