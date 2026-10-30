//! A short desktop folder scan warms the existing version-checked range store.
//!
//! One scan per mount bounds speculation across programs and folders. Its 16-file
//! window holds at most 16 MiB, with six requests in flight, independently of the
//! foreground transfer pool. No range reader, threshold counter or disk file is touched.

// test: a_desktop_scan_reads_forty_heads_at_least_four_times_faster_with_one_request_each
// test: a_scan_only_fetches_matching_cloud_files_within_its_window
// test: blocking_or_stopping_a_scan_cancels_pending_read_ahead
// test: a_real_read_does_not_wait_for_other_prefetches

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use konedrive_tree::{ContentPresence, ContentVersion, Item, ItemKind, LocalItemId, Store};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

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
    },
}

/// A cancelled or failed foreground read cancels its scan too.
pub(super) struct HeadRead {
    cancel: watch::Sender<Option<(String, Instant)>>,
    identity: String,
    pub(super) completed: bool,
}

impl Drop for HeadRead {
    fn drop(&mut self) {
        if !self.completed {
            self.cancel
                .send_replace(Some((self.identity.clone(), Instant::now())));
        }
    }
}

pub(super) struct ReadAhead {
    events: mpsc::Sender<Event>,
    cancel: watch::Sender<Option<(String, Instant)>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ReadAhead {
    pub(super) fn new(
        store: Store,
        source: Arc<dyn ContentSource>,
        guard: Arc<DownloadGuard>,
        window: usize,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        let (events, receiver) = mpsc::channel(64);
        let (cancel, cancelled) = watch::channel(None);
        let task = (window > 0).then(|| {
            runtime.spawn(run(
                store,
                source,
                guard,
                window.min(crate::config::READ_AHEAD),
                receiver,
                cancelled,
            ))
        });
        Self {
            events,
            cancel,
            task,
        }
    }

    pub(super) fn listed(&self, folder: LocalItemId, items: Arc<Vec<Item>>) {
        let _ = self.events.try_send(Event::Listed(folder, items));
    }

    pub(super) fn head(
        &self,
        caller: ReadAheadCaller,
        item: LocalItemId,
        content: &ContentVersion,
        from: u64,
    ) -> Option<HeadRead> {
        if self.task.is_none() || from >= HEAD_END || content.size() == 0 {
            return None;
        }
        let identity = caller.identity().to_owned();
        self.events
            .try_send(Event::Head {
                caller,
                item,
                content: content.clone(),
                at: Instant::now(),
            })
            .ok()?;
        Some(HeadRead {
            cancel: self.cancel.clone(),
            identity,
            completed: false,
        })
    }

    pub(super) fn stop(&self) {
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
    caller: ReadAheadCaller,
    folder: LocalItemId,
    first: LocalItemId,
    active: bool,
    extensions: HashSet<String>,
    began: Instant,
    last: Instant,
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
    window: usize,
    mut events: mpsc::Receiver<Event>,
    mut cancelled: watch::Receiver<Option<(String, Instant)>>,
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
                if scan.as_ref().is_some_and(|s| !guard.permits_read_ahead(&s.caller)) {
                    pending.clear(); scan = None; listing = None;
                }
            }
            changed = cancelled.changed() => {
                if changed.is_err() { return; }
                if scan.as_ref().is_some_and(|s| cancelled.borrow().as_ref().is_some_and(|(id, _)| id == s.caller.identity())) {
                    pending.clear(); scan = None; listing = None;
                }
            }
            event = events.recv() => {
                let Some(event) = event else { return; };
                match event {
                    Event::Listed(folder, items) => { listing = Some((folder, items)); if scan.is_none() { idle = Instant::now() + IDLE; } }
                    Event::Head { caller, item, content, at } => {
                        if !guard.permits_read_ahead(&caller) || at.elapsed() >= IDLE
                            || cancelled.borrow().as_ref().is_some_and(|(id, when)| id == caller.identity() && at <= *when) { continue; }
                        let facts = store.read_items(move |reader| {
                            let Some(row) = reader.find_item(item)? else { return Ok(None); };
                            if row.content_presence != ContentPresence::NotOnDisk || row.kind != ItemKind::File
                                || row.local.as_ref().and_then(|local| local.content.as_ref()) != Some(&content) { return Ok(None); }
                            let Some(place) = row.local.as_ref().and_then(|local| local.place.as_ref()) else { return Ok(None); };
                            let Some(ext) = extension(&row) else { return Ok(None); };
                            let folder = place.parent_local_item_id;
                            Ok(Some((folder, ext, reader.list_children(folder)?)))
                        }).await;
                        let Ok(Some((folder, ext, rows))) = facts else { continue; };
                        let same = scan.as_ref().is_some_and(|s| s.folder == folder && s.caller.identity() == caller.identity()
                            && at.saturating_duration_since(s.last) < IDLE && at.saturating_duration_since(s.began) < LIFETIME);
                        if !same {
                            pending.clear();
                            scan = Some(Scan { caller, folder, first: item, active: false, extensions: HashSet::new(), began: at, last: at, window: Vec::new(), attempted: HashSet::new() });
                        }
                        let s = scan.as_mut().unwrap();
                        s.active |= item != s.first;
                        s.last = at;
                        s.extensions.insert(ext);
                        idle = (at + IDLE).min(s.began + LIFETIME);
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
        if let Some(s) = scan.as_mut().filter(|s| {
            s.active
                && s.last.elapsed() < IDLE
                && s.began.elapsed() < LIFETIME
                && guard.permits_read_ahead(&s.caller)
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
                    s.caller.clone(),
                    s.folder,
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
