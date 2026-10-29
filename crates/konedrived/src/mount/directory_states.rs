//! Recursive xattrs without a recursive getxattr. The index owns a parent/children
//! graph and seven descendant counters per node. A leaf change costs O(depth); moving
//! a directory transfers its counters, without visiting its children unless inherited
//! pins change. Store TEMP triggers feed committed rows before writer jobs answer.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use konedrive_fs::placeholder::State;
use konedrive_tree::{ContentPresence, Item, ItemChanges, ItemKind, ItemReader, LocalItemId, Store, TreeError};

use super::item_download_states::{DownloadKey, ItemDownloadStates};
use super::item_inotify_bridge::ItemInotifyBridge;
use super::item_lookup::{decide_state_attribute, decide_sync_mark, SyncMark};
use crate::sync_parts::{published_plan::PublishedPlan, running_steps::RunningSteps};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts([i64; 7]);
impl Counts {
    fn of(state: State, sync: Option<SyncMark>) -> Self {
        let mut counts = Self::default();
        counts.0[match state {
            State::Failed => 0,
            State::Hydrating => 1,
            State::OnlineOnly => 2,
            State::Hydrated => 3,
        }] = 1;
        if let Some(sync) = sync {
            counts.0[match sync {
                SyncMark::Blocked => 4,
                SyncMark::Uploading => 5,
                SyncMark::Pending => 6,
            }] = 1;
        }
        counts
    }
    fn values(self) -> (State, Option<SyncMark>) {
        let state = if self.0[0] > 0 {
            State::Failed
        } else if self.0[1] > 0 {
            State::Hydrating
        } else if self.0[2] > 0 {
            State::OnlineOnly
        } else {
            State::Hydrated
        };
        let sync = if self.0[4] > 0 {
            Some(SyncMark::Blocked)
        } else if self.0[5] > 0 {
            Some(SyncMark::Uploading)
        } else if self.0[6] > 0 {
            Some(SyncMark::Pending)
        } else {
            None
        };
        (state, sync)
    }
    fn minus(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] - other.0[i]))
    }
}

#[derive(Default)]
struct Node {
    parent: Option<LocalItemId>,
    children: HashSet<LocalItemId>,
    row: Option<Row>,
    own: Counts,
    /// The file's shown marks differ from its aggregate contribution: a pinned
    /// online-only file already contributes hydrating to its ancestors.
    shown: (Option<State>, Option<SyncMark>),
    total: Counts,
}
struct Row {
    item: Item,
    pinned: bool,
    ignored: bool,
    refused: bool,
}
#[derive(Default)]
struct Index {
    nodes: HashMap<LocalItemId, Node>,
}
impl Index {
    fn propagate(&mut self, mut at: Option<LocalItemId>, delta: Counts, changed: &mut BTreeSet<LocalItemId>) {
        if delta == Counts::default() {
            return;
        }
        // The store bounds depth; the bound also makes a corrupt graph harmless here.
        for _ in 0..konedrive_fs::MAX_DEPTH + 2 {
            let Some(id) = at else { break };
            let node = self.nodes.entry(id).or_default();
            let before = node.total.values();
            for i in 0..7 {
                node.total.0[i] += delta.0[i];
            }
            if before != node.total.values() && node.row.as_ref().is_some_and(|row| row.item.kind == ItemKind::Directory) {
                changed.insert(id);
            }
            at = node.parent;
        }
    }
    fn reparent(&mut self, id: LocalItemId, parent: Option<LocalItemId>, changed: &mut BTreeSet<LocalItemId>) {
        let node = self.nodes.entry(id).or_default();
        if node.parent == parent {
            return;
        }
        let (old, total) = (node.parent, node.total);
        if let Some(old) = old {
            self.nodes.entry(old).or_default().children.remove(&id);
        }
        self.propagate(old, Counts::default().minus(total), changed);
        self.nodes.get_mut(&id).unwrap().parent = parent;
        if let Some(parent) = parent {
            self.nodes.entry(parent).or_default().children.insert(id);
        }
        self.propagate(parent, total, changed);
    }
    fn set_own(&mut self, id: LocalItemId, own: Counts, changed: &mut BTreeSet<LocalItemId>) {
        let node = self.nodes.entry(id).or_default();
        let delta = own.minus(node.own);
        node.own = own;
        self.propagate(Some(id), delta, changed);
    }
}

pub(super) struct DirectoryStates {
    index: Mutex<Index>,
    downloads: Arc<ItemDownloadStates>,
    running: Arc<RunningSteps>,
    plan: Arc<PublishedPlan>,
    bridge: Arc<ItemInotifyBridge>,
}
impl DirectoryStates {
    pub(super) fn build(
        store: &Store,
        downloads: Arc<ItemDownloadStates>,
        running: Arc<RunningSteps>,
        plan: Arc<PublishedPlan>,
        bridge: Arc<ItemInotifyBridge>,
    ) -> Result<Arc<Self>, TreeError> {
        let states = Arc::new(Self { index: Mutex::default(), downloads, running, plan, bridge });
        let weak = Arc::downgrade(&states);
        let listener: Arc<dyn Fn(Vec<LocalItemId>, bool) + Send + Sync> = Arc::new(move |items, content_changed| {
            if let Some(states) = weak.upgrade() {
                states.refresh_runtime(items, content_changed);
            }
        });
        states.downloads.changes.listen(listener.clone());
        states.running.changes.listen(listener.clone());
        states.plan.changes.listen(listener);
        let weak = Arc::downgrade(&states);
        store.observe_items(move |reader, changes| {
            if let Some(states) = weak.upgrade() {
                states.refresh_rows(reader, changes)?;
            }
            Ok(())
        })?;
        Ok(states)
    }
    fn file_values(&self, row: &Row) -> (Option<State>, Option<SyncMark>) {
        let item = &row.item;
        let downloading = self.downloads.is_hydrating(item.local_item_id);
        let replacement = super::item_lookup::has_pending_replacement(item);
        let failed = (item.content_presence == ContentPresence::NotOnDisk || replacement)
            && ((row.pinned && self.plan.is_download_held(item.local_item_id))
                || item.local.as_ref().and_then(|local| local.content.as_ref()).into_iter()
                    .chain(item.onedrive.as_ref().and_then(|v| v.content.as_ref()).filter(|_| replacement)).any(|content| {
                    self.downloads.has_failed_download(&DownloadKey { item: item.local_item_id, content: content.clone() }, row.pinned)
                }));
        let state = if failed && !downloading && item.onedrive_item.is_some() {
            Some(State::Failed)
        } else {
            decide_state_attribute(item.file_sync_state(), downloading)
        };
        let sync = decide_sync_mark(
            item,
            row.ignored,
            self.running.has_running_step(item.local_item_id),
            self.plan.find_answer(item.local_item_id).as_ref(),
            row.refused,
        );
        (state, sync)
    }
    fn contribution(row: &Row, values: (Option<State>, Option<SyncMark>)) -> Counts {
        if row.item.kind != ItemKind::File || row.item.local.is_none() {
            return Counts::default();
        }
        let (state, sync) = values;
        let state = state.unwrap_or(State::Hydrated);
        let state = if state == State::OnlineOnly && row.pinned { State::Hydrating } else { state };
        Counts::of(state, sync)
    }
    fn refresh_rows(&self, reader: &ItemReader<'_>, changes: ItemChanges) -> Result<(), TreeError> {
        self.refresh_rows_with(changes, |id| {
            reader
                .find_item(id)?
                .filter(|item| item.local.is_some())
                .map(|item| -> Result<_, TreeError> {
                    Ok(Row {
                        pinned: reader.is_pinned(id)?,
                        ignored: reader.is_ignored(id)?,
                        refused: item.unsent_since.is_some() && reader.find_item_memory(id)?.refusal.is_some(),
                        item,
                    })
                })
                .transpose()
        })
    }
    fn refresh_rows_with(&self, changes: ItemChanges, mut read_row: impl FnMut(LocalItemId) -> Result<Option<Row>, TreeError>) -> Result<(), TreeError> {
        let mut pin_roots = changes.pin_roots;
        let mut rows = HashMap::new();
        let parent_of = |row: &Option<Row>| {
            row.as_ref()
                .and_then(|row| row.item.local.as_ref()?.place.as_ref())
                .filter(|place| !crate::folder::disk::is_hidden_name(&place.name))
                .map(|place| place.parent_local_item_id)
        };
        for id in changes.items {
            let row = read_row(id)?;
            let index = crate::panic::lock(&self.index);
            if let Some(old) = index.nodes.get(&id) {
                // The inherited boolean at a directory is sufficient: descendants'
                // own pin/exception marks still override it in exactly the same way.
                // A rename between equally pinned parents transfers totals in O(depth).
                let old_pin = old.row.as_ref().is_some_and(|row| row.pinned);
                let new_pin = row.as_ref().is_some_and(|row| row.pinned);
                if old_pin != new_pin || (old.row.is_none() && !old.children.is_empty()) {
                    pin_roots.insert(id);
                }
            }
            rows.insert(id, row);
        }
        // Read every affected descendant before changing anything, including moved
        // subtrees whose nearest pin may now differ. Rollback/error leaves the index
        // untouched and the store feed retained for retry.
        // Only the store's serialized writer observer changes rows or parent links.
        // Runtime notifications change counters alone, so the graph stays valid while
        // staging. Lock one node at a time; neither the walk nor SQLite holds up xattrs.
        let mut below = BTreeSet::new();
        let mut pending: Vec<_> = pin_roots.into_iter().collect();
        while let Some(id) = pending.pop() {
            if !below.insert(id) {
                continue;
            }
            {
                let index = crate::panic::lock(&self.index);
                if let Some(node) = index.nodes.get(&id) {
                    pending.extend(node.children.iter().copied());
                }
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = rows.entry(id) {
                entry.insert(read_row(id)?);
            }
        }
        let mut index = crate::panic::lock(&self.index);
        let mut changed = BTreeSet::new();
        let mut cleanup: Vec<_> = rows.keys().copied().collect();
        cleanup.extend(rows.keys().filter_map(|id| index.nodes.get(id).and_then(|node| node.parent)));
        // Detach ALL changed parent edges first. Otherwise a valid batch that rotates
        // a/b to b/a could temporarily form a cycle, depending on row-id order.
        for (&id, row) in &rows {
            if index.nodes.get(&id).and_then(|node| node.parent) != parent_of(row) {
                index.reparent(id, None, &mut changed);
            }
        }
        let parents: Vec<_> = rows.iter().map(|(&id, row)| (id, parent_of(row))).collect();
        for (id, row) in rows {
            if row.as_ref().is_some_and(|row| !row.pinned) {
                if let Some(old) = index.nodes.get(&id).and_then(|node| node.row.as_ref()).filter(|row| row.pinned) {
                    if let Some(content) = old.item.local.as_ref().and_then(|local| local.content.as_ref()) {
                        self.downloads.forget_pin_failure(&DownloadKey { item: id, content: content.clone() });
                    }
                }
            }
            // Runtime changes can run during staging. Compute from the current source
            // states under the apply lock, never from a pre-I/O counter snapshot.
            let shown = row.as_ref().map_or((None, None), |row| self.file_values(row));
            let own = row.as_ref().map_or(Counts::default(), |row| Self::contribution(row, shown));
            index.set_own(id, own, &mut changed);
            let node = index.nodes.get_mut(&id).unwrap();
            node.shown = shown;
            node.row = row;
        }
        for (id, parent) in parents {
            index.reparent(id, parent, &mut changed);
        }
        // Only inspect touched nodes; scanning the full map after every single-file
        // store job would turn a large listing or hydration into quadratic work.
        while let Some(id) = cleanup.pop() {
            if index.nodes.get(&id).is_some_and(|node| node.row.is_none() && node.children.is_empty()) {
                index.nodes.remove(&id);
            }
        }
        drop(index);
        self.bridge.announce_directories(changed);
        Ok(())
    }
    fn refresh_runtime(&self, items: Vec<LocalItemId>, content_changed: bool) {
        let mut index = crate::panic::lock(&self.index);
        let mut changed = BTreeSet::new();
        let mut files = Vec::new();
        for id in items {
            let Some(node) = index.nodes.get(&id) else { continue };
            let Some(row) = node.row.as_ref() else { continue };
            let shown = self.file_values(row);
            let own = Self::contribution(row, shown);
            if (content_changed || shown != node.shown) && row.item.kind == ItemKind::File {
                if let Some(place) = row.item.local.as_ref().and_then(|local| local.place.clone()) {
                    files.push(if content_changed {
                        super::item_announcements::ItemAnnouncement::ContentChanged { item: id, place }
                    } else {
                        super::item_announcements::ItemAnnouncement::MarksChanged { item: id, place }
                    });
                }
            }
            index.set_own(id, own, &mut changed);
            index.nodes.get_mut(&id).unwrap().shown = shown;
        }
        drop(index);
        // After the source state changed: a watcher that immediately reads xattrs sees
        // the new value, including a download's final failure, not its former run.
        self.bridge.announce(files);
        self.bridge.announce_directories(changed);
    }
    pub(super) fn directory_values(&self, id: LocalItemId) -> (State, Option<SyncMark>) {
        crate::panic::lock(&self.index).nodes.get(&id).map_or(Counts::default(), |node| node.total).values()
    }
    pub(super) fn file_state(&self, id: LocalItemId) -> Option<State> {
        crate::panic::lock(&self.index).nodes.get(&id).and_then(|node| node.row.as_ref()).and_then(|row| self.file_values(row).0)
    }
}

#[cfg(test)]
mod tests;
