//! Per-account admission of outgoing mutations. The ledger is written before admission;
//! a missing, unreadable or unwritable ledger never grants an approval.
// test: thresholds_latch_the_whole_pending_batch
// test: approval_is_for_an_immutable_generation
// test: restart_keeps_holds_and_counters
// test: concurrent_admission_cannot_spend_the_same_budget
// test: approved_rewrites_survive_their_sent_move_but_not_a_later_edit
// test: dont_upload_survives_an_ignored_rename_and_back_even_when_disabled

mod batches;
mod ledger;
mod rows;
mod recovery;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use konedrive_tree::{
    ConfirmedItem, Item, ItemKind, ItemStore, ItemVersions, LocalItemId,
    OneDriveItemReference, OneDriveStep, StepCommitOutcome, TreeError,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub(crate) use ledger::{Change, Kind};
use ledger::{Disposition, Ledger};

/// Limits in ten minutes, with an additional daily removal limit. Zero holds the first
/// matching operation and is useful for a staged trial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardConfig {
    pub enabled: bool,
    pub removals: u64,
    pub removal_bytes: u64,
    pub daily_removals: u64,
    pub rewrites: u64,
    pub rewrite_bytes: u64,
    pub moves: u64,
    pub uploads: u64,
    pub upload_bytes: u64,
    pub window_seconds: u64,
    pub daily_window_seconds: u64,
}
impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            removals: 20,
            removal_bytes: 1 << 30,
            daily_removals: 100,
            rewrites: 50,
            rewrite_bytes: 1 << 30,
            moves: 200,
            uploads: 500,
            upload_bytes: 5 << 30,
            window_seconds: 600,
            daily_window_seconds: 86400,
        }
    }
}
impl GuardConfig {
    pub(crate) fn thresholds(&self) -> BTreeMap<String, u64> {
        [
            ("removals", self.removals),
            ("removal-bytes", self.removal_bytes),
            ("daily-removals", self.daily_removals),
            ("rewrites", self.rewrites),
            ("rewrite-bytes", self.rewrite_bytes),
            ("moves", self.moves),
            ("uploads", self.uploads),
            ("upload-bytes", self.upload_bytes),
            ("window-seconds", self.window_seconds),
            ("daily-window-seconds", self.daily_window_seconds),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect()
    }
    pub(crate) fn set_thresholds(&mut self, values: BTreeMap<String, u64>) -> Result<(), String> {
        for (key, value) in values {
            match key.as_str() {
                "removals" => self.removals = value,
                "removal-bytes" => self.removal_bytes = value,
                "daily-removals" => self.daily_removals = value,
                "rewrites" => self.rewrites = value,
                "rewrite-bytes" => self.rewrite_bytes = value,
                "moves" => self.moves = value,
                "uploads" => self.uploads = value,
                "upload-bytes" => self.upload_bytes = value,
                "window-seconds" if value > 0 => self.window_seconds = value,
                "daily-window-seconds" if value > 0 => self.daily_window_seconds = value,
                "window-seconds" | "daily-window-seconds" => {
                    return Err("windows must be greater than zero".into())
                }
                _ => return Err(format!("unknown threshold {key:?}")),
            }
        }
        Ok(())
    }
}

struct Inner {
    config: GuardConfig,
    ledger: Ledger,
    saved_revision: Option<u64>,
    failed: Option<String>,
    admitted: BTreeMap<String, Change>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}
/// One mutex covers counting, the hold, durable writes and destructive local work.
/// Network requests run outside it; concurrent admissions cannot spend one budget twice.
pub(crate) struct ChangeGuard {
    file: PathBuf,
    inner: Mutex<Inner>,
    news: broadcast::Sender<()>,
    progress: broadcast::Sender<(String, Vec<String>, u64, u64)>,
}
impl ChangeGuard {
    pub(crate) fn open(file: PathBuf, config: GuardConfig) -> Self {
        let (mut ledger, failed) = match std::fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(ledger) => (ledger, None),
                Err(e) => (
                    Ledger::default(),
                    Some(format!("cannot read change guard {}: {e}", file.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Ledger::default(), None),
            Err(e) => (
                Ledger::default(),
                Some(format!("cannot read change guard {}: {e}", file.display())),
            ),
        };
        let epoch_saved = !ledger.epoch.is_empty();
        if !epoch_saved {
            ledger.epoch = ledger::new_epoch();
        }
        // Restores interrupted by a process exit become retryable holds.
        for change in ledger.current.values_mut() {
            if change.disposition == Disposition::Restoring {
                change.disposition = Disposition::Held;
            }
        }
        let guard = Self {
            file,
            inner: Mutex::new(Inner {
                config: config.clone(),
                saved_revision: epoch_saved.then_some(ledger.revision),
                ledger,
                failed,
                admitted: BTreeMap::new(),
                wake: None,
            }),
            news: broadcast::channel(64).0,
            progress: broadcast::channel(64).0,
        };
        guard.configure(config);
        guard
    }
    pub(crate) fn configure(&self, config: GuardConfig) {
        let mut inner = crate::panic::lock(&self.inner);
        inner.config = config;
        // Entries observed while disabled can still be pending under an older latch.
        // Sweep once on configuration/open, never once per held row in a plan.
        let latched = inner.ledger.latched.clone();
        if inner.config.enabled && inner.ledger.current.values().any(|c| c.disposition == Disposition::Pending && latched.contains(&c.kind)) {
            inner.ledger.revision += 1;
            let revision = inner.ledger.revision;
            for c in inner.ledger.current.values_mut().filter(|c| c.disposition == Disposition::Pending && latched.contains(&c.kind)) {
                c.disposition = Disposition::Held;
                c.revision = revision;
                c.first_held_revision = revision;
            }
            if let Err(error) = self.save(&mut inner) { tracing::error!("{error}"); }
        }
        self.notify(&inner);
    }
    #[cfg(test)]
    pub(crate) fn config(&self) -> GuardConfig {
        crate::panic::lock(&self.inner).config.clone()
    }
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<()> {
        self.news.subscribe()
    }
    pub(crate) fn attach(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        crate::panic::lock(&self.inner).wake = Some(wake);
    }
    fn notify(&self, inner: &Inner) {
        let _ = self.news.send(());
        if let Some(wake) = &inner.wake {
            wake();
        }
    }
    fn save(&self, inner: &mut Inner) -> Result<(), String> {
        if let Some(failed) = &inner.failed {
            return Err(failed.clone());
        }
        let result = ledger::write(&self.file, &inner.ledger);
        if result.is_ok() {
            inner.saved_revision = Some(inner.ledger.revision);
        }
        if let Err(error) = &result {
            inner.failed = Some(error.clone());
        }
        result
    }
    pub(crate) fn error(&self) -> Option<String> {
        crate::panic::lock(&self.inner).failed.clone()
    }
    /// Observe all pending rows before any step of a plan starts. Mount events call this
    /// as well, so a long unlink burst is counted while the first deletion is waiting.
    #[cfg(test)]
    pub(crate) fn observe(&self, rows: &[(Item, String)], now: i64) -> Result<(), String> {
        self.observe_details(rows, &BTreeMap::new(), now)
    }
    /// Read display metadata from the same store snapshot as the observed generations.
    pub(crate) fn observe_items(
        &self,
        items: &konedrive_tree::ItemReader<'_>,
        rows: &[(Item, String)],
        now: i64,
    ) -> Result<(), konedrive_tree::TreeError> {
        let mut details = BTreeMap::new();
        let mut eligible = Vec::new();
        for (row, path) in rows {
            if items.is_ignored(row.local_item_id)? {
                self.retire_upload_observation(row.local_item_id, true)
                    .map_err(|e| TreeError::Io(std::io::Error::other(e)))?;
                continue;
            }
            eligible.push((row.clone(), path.clone()));
            let old_path = if kinds_of(row).contains(&Kind::Move) {
                rows::old_path(items, row)?
            } else {
                String::new()
            };
            let descendants =
                if row.kind == ItemKind::Directory && kinds_of(row).contains(&Kind::Move) {
                    items.list_below_in_onedrive(row.local_item_id)?.len() as u64
                } else {
                    0
                };
            details.insert(row.local_item_id, (old_path, descendants));
        }
        // A persistence failure is handled by the caller's fail-closed plan.
        self.observe_details(&eligible, &details, now)
            .map_err(|e| konedrive_tree::TreeError::from(std::io::Error::other(e)))
    }
    fn observe_details(
        &self,
        rows: &[(Item, String)],
        details: &BTreeMap<LocalItemId, (String, u64)>,
        now: i64,
    ) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        if let Some(failed) = &inner.failed {
            return Err(failed.clone());
        }
        let config = inner.config.clone();
        let mut changed = false;
        // Only conservative local-only rescues can follow a path after startup.
        // Index them once for this snapshot rather than scanning every held entry.
        let local_paths: BTreeMap<_, _> = inner.ledger.current.values()
            .filter(|c| c.kind == Kind::Upload && c.disposition == Disposition::LocalOnly)
            .map(|c| (c.path.clone(), c.clone())).collect();
        for (row, path) in rows {
            if row.local.is_some() && row.content_before_write.is_some() {
                // Invalidate an earlier approval before the first writer gets its
                // descriptor. The final generation is counted only after close.
                for kind in Kind::ALL {
                    let key = format!("{}:{}", row.local_item_id, kind.key());
                    if inner.ledger.current.get(&key).is_some_and(|c| !c.provisional) {
                        inner.ledger.revision += 1;
                        let revision = inner.ledger.revision;
                        let c = inner.ledger.current.get_mut(&key).unwrap();
                        c.provisional = true;
                        c.revision = revision;
                        // Opening invalidates an explicit approval, not an ordinary
                        // below-limit pending generation.
                        if c.disposition == Disposition::Allowed {
                            c.disposition = Disposition::Held;
                            c.first_held_revision = revision;
                        }
                        changed = true;
                    }
                }
                continue;
            }
            let kinds = kinds_of(row);
            // Startup can adopt a made entry from a place-change record under a new
            // local id. Carry only a conservative local-only hold to that path;
            // an Allow never follows a path to a replacement item.
            if kinds.contains(&Kind::Upload) {
                let key = format!("{}:{}", row.local_item_id, Kind::Upload.key());
                if !inner.ledger.current.contains_key(&key) {
                    let previous = local_paths.get(path).cloned();
                    if let Some(previous) = previous {
                        inner.ledger.current.remove(&previous.key());
                        let mut rescued = change(row, Kind::Upload, path.clone(), previous.at);
                        rescued.disposition = Disposition::LocalOnly;
                        inner.ledger.current.insert(key, rescued);
                        changed = true;
                    }
                }
            }
            for kind in Kind::ALL {
                if !kinds.contains(&kind) {
                    changed |= inner.ledger.current.remove(&format!("{}:{}", row.local_item_id, kind.key())).is_some();
                }
            }
            for kind in kinds {
                let mut change = change(row, kind, path.clone(), now);
                if let Some((old_path, descendants)) = details.get(&row.local_item_id) {
                    if kind == Kind::Move {
                        change.old_path = old_path.clone();
                    }
                    change.descendants = *descendants;
                }
                changed |= inner.ledger.observe(change, &config, now);
            }
        }
        if changed {
            self.save(&mut inner)?;
            self.notify(&inner);
        }
        Ok(())
    }
    #[cfg(feature = "fault-injection")]
    pub(crate) fn unsettled(&self) -> usize {
        crate::panic::lock(&self.inner).ledger.current.values().filter(|c| c.provisional).count()
    }
    pub(crate) fn held(&self) -> BTreeSet<LocalItemId> {
        let inner = crate::panic::lock(&self.inner);
        inner
            .ledger
            .current
            .values()
            .filter(|c| c.disposition.blocks() || inner.failed.is_some())
            .map(|c| LocalItemId::new(c.item))
            .collect()
    }
    /// Linearize a destructive local operation against a new hold. This callback is
    /// local disk/SQL work only; it must never make a network request or re-enter us.
    pub(crate) fn while_unheld<T>(
        &self,
        items: &[LocalItemId],
        action: impl FnOnce() -> T,
    ) -> Option<T> {
        let inner = crate::panic::lock(&self.inner);
        if inner.failed.is_some()
            || items.iter().any(|id| inner.ledger.blocks(id.as_i64()))
        {
            return None;
        }
        Some(action())
    }

    pub(crate) fn blocks(&self, item: LocalItemId) -> bool {
        let inner = crate::panic::lock(&self.inner);
        inner.failed.is_some() || inner.ledger.blocks(item.as_i64())
    }
    pub(crate) fn change_quiet_until(&self, now: i64) -> BTreeMap<LocalItemId, i64> {
        let inner = crate::panic::lock(&self.inner);
        if !inner.config.enabled {
            return BTreeMap::new();
        }
        let mut latest = BTreeMap::<Kind, i64>::new();
        for c in inner
            .ledger
            .current
            .values()
            .filter(|c| c.disposition == Disposition::Pending)
        {
            latest
                .entry(c.kind)
                .and_modify(|at| *at = (*at).max(c.at))
                .or_insert(c.at);
        }
        inner
            .ledger
            .current
            .values()
            .filter_map(|c| {
                if c.disposition != Disposition::Pending {
                    return None;
                }
                let until =
                    latest[&c.kind].saturating_add(if c.kind == Kind::Removal { 30 } else { 2 });
                (until > now).then_some((LocalItemId::new(c.item), until))
            })
            .fold(
                BTreeMap::<LocalItemId, i64>::new(),
                |mut quiet, (id, until)| {
                    quiet
                        .entry(id)
                        .and_modify(|at| *at = (*at).max(until))
                        .or_insert(until);
                    quiet
                },
            )
    }
    #[cfg(any(test, feature = "fault-injection"))]
    pub(crate) fn list(&self, sent: bool) -> Vec<konedrive_dbus::rows::HeldChange> {
        let inner = crate::panic::lock(&self.inner);
        let changes: Vec<_> = if sent {
            inner.ledger.sent.iter().collect()
        } else {
            inner
                .ledger
                .current
                .values()
                .filter(|c| c.disposition.blocks())
                .collect()
        };
        rows::describe(changes)
    }
    #[cfg(any(test, feature = "fault-injection"))]
    pub(crate) fn decide(&self, kinds: &[String], restore: bool) -> Result<Vec<Change>, String> {
        let token = self.snapshot()?.0;
        self.decide_batch(&token, kinds, restore)
            .and_then(|(batch, result)| {
                if result.failed > 0 {
                    Err(result.failures[0].2.clone())
                } else {
                    Ok(batch)
                }
            })
    }
    /// Failed/colliding restores remain held. New files remain local until explicitly
    /// allowed. The caller checks the generation again on the mount's write thread.
    pub(crate) fn restored(&self, c: &Change, success: bool) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let key = c.key();
        inner.ledger.restored.remove(&key);
        if let Some(current) = inner.ledger.current.get_mut(&key).filter(|v| {
            v.generation == c.generation
                && v.write_count == c.write_count
                && v.revision == c.revision
        }) {
            if success && c.kind != Kind::Upload && c.item >= 0 {
                inner.ledger.current.remove(&key);
            } else {
                current.disposition = if success {
                    Disposition::LocalOnly
                } else {
                    Disposition::Held
                };
            }
        }
        if !inner.ledger.current.values().any(|v| {
            v.kind == c.kind && matches!(v.disposition, Disposition::Held | Disposition::Restoring)
        }) {
            inner.ledger.latched.remove(&c.kind);
            inner.ledger.hold_started.remove(&c.kind);
        }
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok(())
    }
    /// Rows do not describe every outgoing case: recreating an item removed remotely
    /// or completing a temporary move can be planned from a row with an old binding.
    pub(crate) fn lacks_step(&self, step: &OneDriveStep) -> bool {
        let inner = crate::panic::lock(&self.inner);
        step_versions(step).any(|v| {
            !inner.ledger.current.contains_key(&format!(
                "{}:{}",
                v.local_item_id,
                step_kind(step).key()
            ))
        })
    }
    pub(crate) fn observe_step(
        &self,
        step: &OneDriveStep,
        paths: &BTreeMap<LocalItemId, String>,
        now: i64,
    ) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let kind = step_kind(step);
        let config = inner.config.clone();
        let mut changed = false;
        for v in step_versions(step) {
            let key = format!("{}:{}", v.local_item_id, kind.key());
            if inner.ledger.current.contains_key(&key) {
                continue;
            }
            let content = if kind == Kind::Upload {
                v.local.as_ref().and_then(|l| l.content.as_ref())
            } else {
                v.synced_content
                    .as_ref()
                    .or_else(|| v.onedrive.as_ref().and_then(|r| r.content.as_ref()))
            };
            let c = Change {
                item: v.local_item_id.as_i64(),
                kind,
                path: paths
                    .get(&v.local_item_id)
                    .cloned()
                    .unwrap_or_else(|| v.local_item_id.to_string()),
                bytes: content.map_or(0, |c| c.size()),
                after_bytes: v
                    .local
                    .as_ref()
                    .and_then(|l| l.content.as_ref())
                    .map_or(0, |c| c.size()),
                old_path: String::new(),
                directory: matches!(
                    step,
                    OneDriveStep::MakeDirectory { .. } | OneDriveStep::SendDirectoryRemoval { .. }
                ),
                descendants: 0,
                revision: 0,
                first_held_revision: 0,
                at: now,
                generation: generation(v),
                write_count: write_count(v),
                provisional: false,
                disposition: Disposition::Pending,
            };
            changed |= inner.ledger.observe(c, &config, now);
        }
        if changed {
            self.save(&mut inner)?;
            self.notify(&inner);
        }
        Ok(())
    }

    pub(crate) fn admit(&self, step: &OneDriveStep, now: i64) -> Result<bool, String> {
        let mut inner = crate::panic::lock(&self.inner);
        if let Some(failed) = &inner.failed {
            return Err(failed.clone());
        }
        let mut versions = vec![step.item()];
        if let OneDriveStep::SendDirectoryRemoval { below, .. } = step {
            versions.extend(below);
        }
        let kind = match step {
            OneDriveStep::SendFileRemoval { .. } | OneDriveStep::SendDirectoryRemoval { .. } => {
                Kind::Removal
            }
            OneDriveStep::SendContent { .. } => Kind::Rewrite,
            OneDriveStep::SendMove { .. } => Kind::Move,
            OneDriveStep::MakeDirectory { .. } | OneDriveStep::SendNewFile { .. } => Kind::Upload,
        };
        let latest = inner
            .ledger
            .current
            .values()
            .filter(|c| c.kind == kind && c.disposition == Disposition::Pending)
            .map(|c| c.at)
            .max()
            .unwrap_or(0);
        for v in versions {
            if inner.ledger.blocks(v.local_item_id.as_i64())
            {
                return Ok(false);
            }
            let Some(c) = inner
                .ledger
                .current
                .get(&format!("{}:{}", v.local_item_id, kind.key()))
            else {
                return Ok(false);
            };
            if c.generation != generation(v)
                || write_count(v).is_some_and(|n| c.write_count != Some(n))
                || c.disposition.blocks()
                || (inner.config.enabled
                    && c.disposition != Disposition::Allowed
                    && now < latest.saturating_add(if kind == Kind::Removal { 30 } else { 2 }))
            {
                return Ok(false);
            }
        }
        for v in std::iter::once(step.item()).chain(
            match step {
                OneDriveStep::SendDirectoryRemoval { below, .. } => below.as_slice(),
                _ => &[],
            }
            .iter(),
        ) {
            let key = format!("{}:{}", v.local_item_id, kind.key());
            if let Some(mut c) = inner.ledger.current.get(&key).cloned() {
                // Sent history records when this immutable generation first entered the
                // sender, not when the local edit was made. Rechecks keep that time.
                c.at = now;
                inner
                    .admitted
                    .entry(key)
                    .and_modify(|old| {
                        if old.generation != c.generation || old.write_count != c.write_count {
                            *old = c.clone();
                        }
                    })
                    .or_insert(c);
            }
        }
        // Persisting again is intentional: an admission cannot follow a failed write.
        self.save(&mut inner)?;
        Ok(true)
    }
    pub(crate) fn sent(&self, step: &OneDriveStep) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let mut versions = vec![step.item()];
        if let OneDriveStep::SendDirectoryRemoval { below, .. } = step {
            versions.extend(below);
        }
        let kind = match step {
            OneDriveStep::SendFileRemoval { .. } | OneDriveStep::SendDirectoryRemoval { .. } => {
                Kind::Removal
            }
            OneDriveStep::SendContent { .. } => Kind::Rewrite,
            OneDriveStep::SendMove { .. } => Kind::Move,
            OneDriveStep::MakeDirectory { .. } | OneDriveStep::SendNewFile { .. } => Kind::Upload,
        };
        for v in versions {
            let key = format!("{}:{}", v.local_item_id, kind.key());
            let admitted = inner.admitted.remove(&key);
            let current = if inner
                .ledger
                .current
                .get(&key)
                .is_some_and(|c| c.generation == generation(v))
            {
                inner.ledger.current.remove(&key)
            } else {
                None
            };
            if let Some(mut c) = admitted.or(current) {
                c.disposition = Disposition::Sent;
                inner.ledger.sent.push(c);
            }
        }
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok(())
    }
    /// Keep an approved rewrite across this sender's confirmed move. Hold the ledger
    /// lock over the commit so a plan cannot observe its new fingerprint first.
    pub(crate) fn commit_move(
        &self,
        items: &mut ItemStore<'_>,
        planned: &ItemVersions,
        parent: &OneDriveItemReference,
        name: &std::ffi::OsStr,
        moved: &ConfirmedItem,
    ) -> Result<StepCommitOutcome, TreeError> {
        let mut inner = crate::panic::lock(&self.inner);
        let outcome = items.commit_send_move(planned, parent, name, moved)?;
        if outcome != StepCommitOutcome::Written {
            return Ok(outcome);
        }
        let Some(row) = items.reader().find_item(planned.local_item_id)? else { return Ok(outcome) };
        // A concurrent local writer, rename, rebind or remote content change must
        // never inherit approval from the move's planned generation.
        if row.content_before_write.is_some()
            || row.local != planned.local
            || row.onedrive_item != planned.onedrive_item
            || row.synced_content != planned.synced_content
            || row.onedrive.as_ref().and_then(|v| v.content.as_ref())
                != planned.onedrive.as_ref().and_then(|v| v.content.as_ref())
        {
            return Ok(outcome);
        }
        let key = format!("{}:rewrite", planned.local_item_id);
        if let Some(c) = inner.ledger.current.get_mut(&key).filter(|c| {
            c.disposition == Disposition::Allowed
                && !c.provisional
                && c.generation == generation(planned)
                && write_count(planned).is_none_or(|n| c.write_count == Some(n))
        }) {
            c.generation = generation(&ItemVersions::of(&row));
            inner.ledger.revision += 1;
            self.save(&mut inner).map_err(|e| TreeError::Io(std::io::Error::other(e)))?;
        }
        Ok(outcome)
    }

    /// A missing row has no pending upload. Retained removals own their
    /// recovery separately and must survive retirement of this obsolete observation.
    pub(crate) fn retire_upload(&self, item: LocalItemId) -> Result<(), String> {
        self.retire_upload_observation(item, false)
    }
    fn retire_upload_observation(&self, item: LocalItemId, preserve_local_only: bool) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let key = format!("{item}:upload");
        // Ignoring an existing row never withdraws the user's Don't Upload choice.
        if preserve_local_only && inner.ledger.current.get(&key)
            .is_some_and(|c| c.disposition == Disposition::LocalOnly)
        {
            return Ok(());
        }
        if inner.ledger.current.remove(&key).is_some() {
            inner.admitted.remove(&key);
            inner.ledger.revision += 1;
            self.save(&mut inner)?;
            self.notify(&inner);
        }
        Ok(())
    }

    pub(crate) fn ended_without_write(&self, step: &OneDriveStep) {
        let ids: BTreeSet<_> = step_versions(step)
            .map(|v| v.local_item_id.as_i64())
            .collect();
        crate::panic::lock(&self.inner)
            .admitted
            .retain(|_, c| !ids.contains(&c.item));
    }

    /// Same-filesystem retained copies, separate from the temporary aside entries which
    /// start-up removes. Nothing automatically prunes these unique bytes.
    pub(crate) fn admit_cleanup(
        &self,
        reference: &konedrive_tree::OneDriveItemReference,
        bytes: u64,
        version: &str,
        now: i64,
    ) -> Result<bool, String> {
        let identity = format!("{reference:?}");
        let item = cleanup_id(reference);
        let mut inner = crate::panic::lock(&self.inner);
        let c = Change {
            item,
            kind: Kind::Removal,
            path: format!("[upload cleanup] {}", reference.item_id.as_str()),
            bytes,
            after_bytes: 0,
            old_path: String::new(),
            directory: false,
            descendants: 0,
            revision: 0,
            first_held_revision: 0,
            at: now,
            generation: format!("cleanup:{identity}:{version}"),
            write_count: None,
            provisional: false,
            disposition: Disposition::Pending,
        };
        let config = inner.config.clone();
        let key = c.key();
        inner.ledger.cleanup_refs.insert(item, (reference.drive_id.as_str().to_owned(), reference.item_id.as_str().to_owned(), version.to_owned()));
        if inner.ledger.observe(c, &config, now) {
            self.save(&mut inner)?;
            self.notify(&inner);
        }
        if inner.failed.is_some() {
            return Ok(false);
        }
        let c = &inner.ledger.current[&key];
        let latest = inner.ledger.current.values()
            .filter(|c| c.kind == Kind::Removal && c.disposition == Disposition::Pending)
            .map(|c| c.at).max().unwrap_or(c.at);
        Ok(!c.disposition.blocks()
            && (!inner.config.enabled || c.disposition == Disposition::Allowed || now >= latest.saturating_add(30)))
    }
    /// A cleanup is finished, absent or no longer ours. Its synthetic observation
    /// must not turn into a phantom held removal on the next burst.
    pub(crate) fn settle_cleanup(&self, reference: &konedrive_tree::OneDriveItemReference, sent: bool) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let id = cleanup_id(reference);
        inner.ledger.cleanup_refs.remove(&id);
        let key = format!("{}:{}", id, Kind::Removal.key());
        if let Some(mut c) = inner.ledger.current.remove(&key) {
            if sent {
                c.disposition = Disposition::Sent;
                c.at = inner.ledger.clock_now();
                inner.ledger.sent.push(c);
            }
            self.save(&mut inner)?;
            self.notify(&inner);
        }
        Ok(())
    }

    pub(crate) fn keep_local(&self, row: &Item, path: String, now: i64) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let mut c = change(row, Kind::Upload, path, now);
        c.disposition = Disposition::LocalOnly;
        inner.ledger.current.insert(c.key(), c);
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok(())
    }
    pub(crate) fn prepare_removal(
        &self,
        row: &Item,
        path: &str,
        now: i64,
    ) -> Result<PathBuf, String> {
        let mut inner = crate::panic::lock(&self.inner);
        let name = format!(
            ".konedrive-change-guard/{}-{}",
            row.local_item_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut removed = row.clone();
        removed.local = None;
        // The row's unsent stamp is assigned by SQLite. The planner replaces this
        // provisional observation with the committed generation before admission.
        let c = change(&removed, Kind::Removal, path.into(), now);
        let config = inner.config.clone();
        inner.ledger.observe(c, &config, now);
        inner.ledger.retained.insert(
            row.local_item_id.as_i64(),
            ledger::Retained {
                path: name.clone(),
                content: row.local.as_ref().and_then(|l| l.content.clone()),
                bound: row.onedrive_item.is_some(),
                cleanup: false,
            },
        );
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok(name.into())
    }
    pub(crate) fn retained(&self, item: LocalItemId) -> Option<PathBuf> {
        crate::panic::lock(&self.inner)
            .ledger
            .retained
            .get(&item.as_i64())
            .map(|c| PathBuf::from(&c.path))
    }
    pub(crate) fn retained_content(
        &self,
        item: LocalItemId,
    ) -> Option<konedrive_tree::ContentVersion> {
        crate::panic::lock(&self.inner)
            .ledger
            .retained
            .get(&item.as_i64())
            .and_then(|c| c.content.clone())
    }
    pub(crate) fn forget_retained(&self, item: LocalItemId) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        inner.ledger.retained.remove(&item.as_i64());
        self.save(&mut inner)
    }
    pub(crate) fn aside_directory(&self, backing: &Path) -> PathBuf {
        // Keeping this inside the backing descriptor's directory guarantees rename works
        // when the state directory is on a different filesystem. It is outside FUSE's
        // namespace, which consists solely of tree rows.
        backing.join(".konedrive-change-guard")
    }
}

pub(crate) fn generation(v: &ItemVersions) -> String {
    // Content hashes can be filled by a row step without changing the bytes. Bind to the
    // local size/time, place, unsent stamp, cloud binding and original cloud version.
    let local = v
        .local
        .as_ref()
        .map(|l| (&l.place, l.content.as_ref().map(|c| (c.size(), c.mtime()))));
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        v.onedrive_item,
        local,
        v.unsent_since,
        v.onedrive_etag,
        v.onedrive,
        v.synced_place,
        v.synced_content
    )
}
fn change(row: &Item, kind: Kind, path: String, at: i64) -> Change {
    let original = row
        .synced_content
        .as_ref()
        .or_else(|| row.onedrive.as_ref().and_then(|v| v.content.as_ref()))
        .map_or(0, |c| c.size());
    let bytes = if kind == Kind::Upload {
        row.local
            .as_ref()
            .and_then(|v| v.content.as_ref())
            .map_or(0, |c| c.size())
    } else {
        original
    };
    Change {
        item: row.local_item_id.as_i64(),
        kind,
        path,
        bytes,
        after_bytes: row
            .local
            .as_ref()
            .and_then(|v| v.content.as_ref())
            .map_or(0, |c| c.size()),
        old_path: String::new(),
        directory: row.kind == ItemKind::Directory,
        descendants: 0,
        revision: 0,
        first_held_revision: 0,
        at,
        generation: generation(&ItemVersions::of(row)),
        write_count: write_count(&ItemVersions::of(row)),
        provisional: false,
        disposition: Disposition::Pending,
    }
}
fn kinds_of(row: &Item) -> Vec<Kind> {
    if row.local_item_id == LocalItemId::ROOT {
        return vec![];
    }
    match (&row.local, &row.onedrive_item) {
        (None, Some(_)) if row.onedrive.is_some() => vec![Kind::Removal],
        (Some(_), _) if row.onedrive.is_none() => vec![Kind::Upload],
        (Some(_), None) => vec![Kind::Upload],
        (Some(local), Some(_)) => {
            let mut kinds = Vec::new();
            if local.place != row.synced_place {
                kinds.push(Kind::Move);
            }
            if row.kind == ItemKind::File && local.content != row.synced_content {
                kinds.push(Kind::Rewrite);
            }
            kinds
        }
        _ => vec![],
    }
}

fn write_count(v: &ItemVersions) -> Option<u64> {
    match v.local.as_ref().and_then(|l| l.content.as_ref()) {
        Some(konedrive_tree::ContentVersion::ChangedHere { change_count, .. }) => {
            Some(*change_count)
        }
        _ => None,
    }
}

fn step_kind(step: &OneDriveStep) -> Kind {
    match step {
        OneDriveStep::SendFileRemoval { .. } | OneDriveStep::SendDirectoryRemoval { .. } => {
            Kind::Removal
        }
        OneDriveStep::SendContent { .. } => Kind::Rewrite,
        OneDriveStep::SendMove { .. } => Kind::Move,
        OneDriveStep::SendNewFile { .. } | OneDriveStep::MakeDirectory { .. } => Kind::Upload,
    }
}
fn step_versions(step: &OneDriveStep) -> impl Iterator<Item = &ItemVersions> {
    std::iter::once(step.item()).chain(
        match step {
            OneDriveStep::SendDirectoryRemoval { below, .. } => below.as_slice(),
            _ => &[],
        }
        .iter(),
    )
}

fn cleanup_id(reference: &konedrive_tree::OneDriveItemReference) -> i64 {
    let hash = format!("{reference:?}").bytes().fold(0u64, |n, b| n.wrapping_mul(131).wrapping_add(u64::from(b)));
    -((hash % (i64::MAX as u64 - 2)) as i64) - 2
}
