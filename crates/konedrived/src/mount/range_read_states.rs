//! The state attribute while a program waits for ranges: only a fetch that stays in
//! flight for 500 ms shows hydrating. Once shown, it stays through the range reader's
//! linger. Quick reads never accumulate towards the delay, even on the same item.
//!
//! Argv launch intent can also hold an immediate emblem, independently of fetches.
//! These are presentation states alone. `range_readers` owns the pool slots and
//! `item_download_states` owns whole downloads. A guard surrounds only a source asking
//! identified in `item_kernel_requests` and begun by `range_readers` once its pool slot
//! is held; disk reads never enter here and an in-memory source answer
//! leaves before the delay. Dropping that asking on an error, refusal or interrupt ends
//! the guard too. One timer per item announces each change after releasing the lock.
// test: a_slow_range_is_shown_once_and_stays_through_its_last_reads_linger
// test: fifty_quick_files_and_repeated_quick_reads_of_one_file_are_never_shown
// test: overlapping_quick_reads_do_not_add_their_times
// test: dropping_an_interrupted_fetch_ends_its_shown_state
// test: waiting_for_a_pool_slot_and_an_early_refusal_never_show_hydrating

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use konedrive_tree::LocalItemId;
use tokio::sync::Notify;
use tokio::time::Instant;

use super::range_readers::READ_LINGERS;
use crate::sync_parts::state_changes::StateChanges;

/// Above the 0–300 ms reads a viewer makes for each image; a slower fetch gives feedback
/// without making every thumbnail flash.
pub(super) const SHOW_AFTER: Duration = Duration::from_millis(500);

struct Reader {
    generation: u64,
    reads: HashMap<u64, Instant>,
    lingers_until: Instant,
    shown: bool,
    changed: Arc<Notify>,
}

pub(super) struct RangeReadStates {
    readers: Mutex<HashMap<LocalItemId, Reader>>,
    launches: Mutex<HashMap<LocalItemId, usize>>,
    /// The local items of askings that have reached the range fetcher. Kept only for
    /// those futures' lifetime, so no binding lookup or unbounded id cache is needed.
    pending: Mutex<HashMap<String, HashMap<LocalItemId, usize>>>,
    next_read: AtomicU64,
    changes: Arc<StateChanges>,
}

/// The local item is known before leaving the kernel request; the pool reader knows
/// when its slot is held and the source is about to be asked. This joins those facts
/// without changing the range fetcher's interface.
pub(super) struct PendingRead {
    states: Arc<RangeReadStates>,
    onedrive_id: String,
    item: LocalItemId,
}

impl Drop for PendingRead {
    fn drop(&mut self) {
        let mut pending = crate::panic::lock(&self.states.pending);
        if let Some(items) = pending.get_mut(&self.onedrive_id) {
            if let Some(count) = items.get_mut(&self.item) {
                *count -= 1;
                if *count == 0 {
                    items.remove(&self.item);
                }
            }
            if items.is_empty() {
                pending.remove(&self.onedrive_id);
            }
        }
    }
}

/// An argv launch shows immediately; its owner drops this at the no-read timeout,
/// process exit, intent expiry, mount stop, or after handing over to a whole download.
pub(super) struct LaunchEmblem {
    states: Arc<RangeReadStates>,
    item: LocalItemId,
}

impl Drop for LaunchEmblem {
    fn drop(&mut self) {
        let mut launches = crate::panic::lock(&self.states.launches);
        if let Some(count) = launches.get_mut(&self.item) {
            *count -= 1;
            if *count == 0 { launches.remove(&self.item); }
        }
        drop(launches);
        self.states.changes.changed(vec![self.item]);
    }
}

pub(super) struct ActiveRead {
    states: Arc<RangeReadStates>,
    item: LocalItemId,
    number: u64,
}

impl Drop for ActiveRead {
    fn drop(&mut self) {
        let mut readers = crate::panic::lock(&self.states.readers);
        if let Some(reader) = readers.get_mut(&self.item) {
            reader.reads.remove(&self.number);
            reader.lingers_until = Instant::now() + READ_LINGERS;
            reader.changed.notify_one();
        }
    }
}

impl RangeReadStates {
    pub(super) fn new(changes: Arc<StateChanges>) -> Self {
        Self {
            readers: Mutex::default(),
            launches: Mutex::default(),
            pending: Mutex::default(),
            next_read: AtomicU64::new(0),
            changes,
        }
    }

    /// A shown whole download overlaps a range: keep the item continuously shown,
    /// including the range linger, even if that range was quick on its own.
    pub(super) fn keep_shown(&self, item: LocalItemId) {
        if let Some(reader) = crate::panic::lock(&self.readers).get_mut(&item) {
            if !reader.reads.is_empty() {
                reader.shown = true;
                reader.changed.notify_one();
            }
        }
    }

    pub(super) fn is_hydrating(&self, item: LocalItemId) -> bool {
        crate::panic::lock(&self.launches).contains_key(&item)
            || crate::panic::lock(&self.readers).get(&item).is_some_and(|reader| reader.shown)
    }

    pub(super) fn show_launch(self: &Arc<Self>, item: LocalItemId) -> LaunchEmblem {
        *crate::panic::lock(&self.launches).entry(item).or_default() += 1;
        self.changes.changed(vec![item]);
        LaunchEmblem { states: Arc::clone(self), item }
    }

    pub(super) fn prepare(self: &Arc<Self>, onedrive_id: &str, item: LocalItemId) -> PendingRead {
        *crate::panic::lock(&self.pending)
            .entry(onedrive_id.to_owned())
            .or_default()
            .entry(item)
            .or_default() += 1;
        PendingRead {
            states: Arc::clone(self),
            onedrive_id: onedrive_id.to_owned(),
            item,
        }
    }

    /// The pool slot is held: time spent waiting for one never ages into hydrating.
    pub(super) fn begin_fetch(self: &Arc<Self>, onedrive_id: &str) -> Vec<ActiveRead> {
        let items: Vec<_> = crate::panic::lock(&self.pending)
            .get(onedrive_id)
            .map(|items| items.keys().copied().collect())
            .unwrap_or_default();
        items
            .into_iter()
            .map(|item| self.begin_read(item))
            .collect()
    }

    /// Called on the runtime, around a fetch of bytes that are not on disk. Each asking
    /// has its own start: finishing one fast read cannot age the next into being shown.
    pub(super) fn begin_read(self: &Arc<Self>, item: LocalItemId) -> ActiveRead {
        let number = self.next_read.fetch_add(1, Ordering::Relaxed);
        let mut readers = crate::panic::lock(&self.readers);
        let is_new = !readers.contains_key(&item);
        let reader = readers.entry(item).or_insert_with(|| Reader {
            generation: number,
            reads: HashMap::new(),
            lingers_until: Instant::now(),
            shown: false,
            changed: Arc::new(Notify::new()),
        });
        reader.reads.insert(number, Instant::now());
        reader.changed.notify_one();
        if is_new {
            tokio::spawn(Arc::clone(self).watch(item, number, Arc::clone(&reader.changed)));
        }
        ActiveRead {
            states: Arc::clone(self),
            item,
            number,
        }
    }

    async fn watch(self: Arc<Self>, item: LocalItemId, generation: u64, changed: Arc<Notify>) {
        loop {
            let notified = changed.notified();
            let (next, announce, ended) = {
                let mut readers = crate::panic::lock(&self.readers);
                let Some(reader) = readers
                    .get_mut(&item)
                    .filter(|reader| reader.generation == generation)
                else {
                    return;
                };
                let now = Instant::now();
                let first_due = reader.reads.values().min().map(|start| *start + SHOW_AFTER);
                let show = !reader.shown && first_due.is_some_and(|due| due <= now);
                reader.shown |= show;
                let ended =
                    reader.reads.is_empty() && (!reader.shown || reader.lingers_until <= now);
                let next = if reader.reads.is_empty() {
                    Some(reader.lingers_until)
                } else if !reader.shown {
                    first_due
                } else {
                    None
                };
                let announce = show || (ended && reader.shown);
                if ended {
                    readers.remove(&item);
                }
                (next, announce, ended)
            };
            if announce {
                self.changes.changed(vec![item]);
            }
            if ended {
                return;
            }
            match next {
                Some(next) => {
                    tokio::select! { () = tokio::time::sleep_until(next) => {}, () = notified => {} }
                }
                None => notified.await,
            }
        }
    }
}

#[cfg(test)]
mod tests;
