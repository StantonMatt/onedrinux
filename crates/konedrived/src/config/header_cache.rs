//! A private, disposable disk budget for small ranges actually read, owned per account.
//!
//! A writer and an independent read-only worker run SQLite and file operations,
//! never on a FUSE/runtime thread.
//! Writes are best effort with at most 1024 queued records and 4 MiB of content; reads wait
//! at most 20 ms for a known covering row before falling through. An in-memory
//! catalog skips absent intervals and coalesces covered publications. An epoch fences
//! queued work against authentication invalidation. The source owns the meaning of the
//! opaque version and facts, and checks both on use.

// test: header_cache_rebuild_rejects_reader_reopen_from_the_replaced_generation
// test: header_cache_reversed_forgets_refresh_the_catalog_after_each_deletion
// test: header_cache_repeated_memory_hits_do_not_crowd_out_a_new_head
// test: header_cache_known_misses_and_pending_duplicates_never_wait_for_the_writer
// test: header_cache_keeps_ranges_across_restart_and_preserves_facts
// test: header_cache_bounds_ranges_items_and_evicts_lru_rows
// test: header_cache_expires_unused_rows_and_removes_other_versions
// test: header_cache_disabled_corrupt_and_new_schema_files_are_disposable
// test: header_cache_invalidation_fences_queued_writes_and_deletes_the_file
// test: header_cache_failed_deletion_blocks_reuse_and_retries_after_restart
// test: header_cache_failed_unlink_is_remembered_without_directory_write_permission
// test: account_header_cleanup_failures_are_reported_after_sign_out
// test: header_cache_publication_never_waits_for_a_paused_writer_and_has_a_bounded_queue
// test: header_cache_lookup_bypasses_queued_writes
// test: header_cache_delayed_lookup_maintenance_preserves_newer_keeps_and_forgets
// test: sign_out_and_account_retirement_delete_headers_without_a_mounted_folder

mod catalog;
mod database;
mod reader;

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

use super::{AccountId, ConfigStore};
use database::Database;

pub(crate) const MAX_RANGE: u64 = 64 * 1024;
const WRITE_RECORDS: usize = 1024;
const WRITE_BYTES: usize = 4 * 1024 * 1024;
const LOOKUP_WAIT: std::time::Duration = std::time::Duration::from_millis(20);
pub(super) const MAX_ITEM: u64 = 256 * 1024;
pub(super) const LIFETIME: i64 = 30 * 24 * 60 * 60 * 1000;

/// A range and its source's serialized version facts. No paths or download URLs.
#[derive(Debug)]
pub(crate) struct Record {
    pub(crate) from: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) facts: String,
}

/// Usage metadata; misses queue separately, while hits coalesce on stored rows.
struct LookupUse {
    epoch: u64,
    item: String,
    version: String,
    from: u64,
    end: u64,
    at: i64,
}

enum Command {
    Touched,
    Use {
        usage: LookupUse,
        _permit: OwnedSemaphorePermit,
    },
    Keep {
        epoch: u64,
        item: String,
        version: String,
        record: Record,
        _permit: OwnedSemaphorePermit,
        _bytes: OwnedSemaphorePermit,
    },
    Forget {
        epoch: u64,
        item: String,
    },
    Clear {
        epoch: u64,
        answer: Option<oneshot::Sender<anyhow::Result<()>>>,
    },
    #[cfg(test)]
    Barrier(oneshot::Sender<()>),
    #[cfg(test)]
    Pause {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
}

/// Shared by the account and all of its sources. Closing it drains the bounded writer.
/// Keeping it in ConfigStore also lets sign-out clear a cache with no mounted folder.
pub(crate) struct HeaderCache {
    sender: Option<mpsc::Sender<Command>>,
    epoch: Arc<AtomicU64>,
    writes: Arc<Semaphore>,
    write_bytes: Arc<Semaphore>,
    touches: Arc<Semaphore>,
    worker: Option<std::thread::JoinHandle<()>>,
    enabled: bool,
    reader: reader::Reader,
    catalog: Arc<std::sync::Mutex<catalog::Catalog>>,
}

impl ConfigStore {
    /// One owner of the account's cache, including while its folder is stopped.
    pub(crate) fn header_cache(&self, id: &AccountId) -> Option<Arc<HeaderCache>> {
        let paths = self.header_paths.account(id)?;
        let limit = self.snapshot().header_cache();
        let mut caches = crate::panic::lock(&self.header_caches);
        Some(Arc::clone(caches.entry(id.clone()).or_insert_with(|| {
            Arc::new(HeaderCache::open(paths.headers, limit))
        })))
    }
}

impl HeaderCache {
    pub(crate) fn open(path: PathBuf, limit: u64) -> Self {
        let (sender, receiver) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&epoch);
        let catalog = Arc::new(std::sync::Mutex::new(catalog::Catalog::default()));
        let inventory = Arc::clone(&catalog);
        let reader = reader::Reader::open(path.clone(), Arc::clone(&epoch));
        let lookup = reader.port();
        let worker = std::thread::Builder::new()
            .name("header-cache".into())
            .spawn(move || {
                let mut store = Worker {
                    path,
                    limit,
                    db: None,
                    disabled: false,
                    warned: false,
                    rebuilt: false,
                    clear_pending: false,
                    catalog: inventory,
                    reader: lookup,
                    epoch: Arc::clone(&current),
                };
                store.start();
                store.publish(current.load(Ordering::SeqCst), None);
                let mut used = Vec::new();
                loop {
                    let idle = if used.is_empty() && crate::panic::lock(&store.catalog).dirty == 0 {
                        std::time::Duration::from_secs(3600)
                    } else {
                        std::time::Duration::from_millis(50)
                    };
                    let command = match receiver.recv_timeout(idle) {
                        Ok(command) => command,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            store.use_lookups(&mut used, current.load(Ordering::SeqCst));
                            store.use_catalog(current.load(Ordering::SeqCst), usize::MAX);
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if !used.is_empty() {
                                store.use_lookups(&mut used, current.load(Ordering::SeqCst));
                            }
                            let (dirty, quiet) = {
                                let catalog = crate::panic::lock(&store.catalog);
                                (catalog.dirty, catalog.quiet())
                            };
                            if dirty > 0 && quiet {
                                store.use_catalog(current.load(Ordering::SeqCst), 64);
                            } else if idle.as_secs() == 3600 && store.db.is_some() {
                                store.run(|db| db.prune(database::now()).map(Some));
                            }
                            store.publish(current.load(Ordering::SeqCst), None);
                            continue;
                        }
                    };
                    match command {
                        Command::Touched => {}
                        Command::Use { usage, .. } => {
                            used.push(usage);
                            if used.len() == 64 {
                                store.use_lookups(&mut used, current.load(Ordering::SeqCst));
                            }
                        }
                        Command::Keep {
                            epoch,
                            item,
                            version,
                            record,
                            ..
                        } => {
                            store.use_lookups(&mut used, current.load(Ordering::SeqCst));
                            if epoch == current.load(Ordering::SeqCst) {
                                store.use_catalog(epoch, usize::MAX);
                                store.run(|db| {
                                    db.keep(&item, &version, &record, database::now()).map(Some)
                                });
                                store.publish(epoch, Some(&item));
                            }
                            crate::panic::lock(&store.catalog).pending.retain(
                                |(queued, key, range)| {
                                    !(*queued == epoch
                                        && key.0 == item
                                        && key.1 == version
                                        && range.from == record.from
                                        && range.end == record.from + record.bytes.len() as u64
                                        && range.facts == record.facts)
                                },
                            );
                        }
                        Command::Forget { epoch, item } => {
                            used.clear();
                            if store.db.is_some() {
                                store.run(|db| db.forget(&item).map(Some));
                            }
                            store.unfence(epoch);
                        }
                        Command::Clear { epoch, answer } => {
                            used.clear();
                            let result = store.clear();
                            store.unfence(epoch);
                            if let Err(error) = &result {
                                store.warn(error);
                            }
                            if let Some(answer) = answer {
                                let _ = answer.send(result);
                            }
                        }
                        #[cfg(test)]
                        Command::Barrier(answer) => {
                            store.use_lookups(&mut used, current.load(Ordering::SeqCst));
                            store.use_catalog(current.load(Ordering::SeqCst), usize::MAX);
                            store.publish(current.load(Ordering::SeqCst), None);
                            let _ = answer.send(());
                        }
                        #[cfg(test)]
                        Command::Pause { entered, release } => {
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                    }
                    store.publish(current.load(Ordering::SeqCst), None);
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                tracing::warn!("header cache unavailable: {error}");
                None
            }
        };
        Self {
            catalog,
            reader,
            enabled: limit > 0 && worker.is_some(),
            sender: worker.as_ref().map(|_| sender),
            epoch,
            writes: Arc::new(Semaphore::new(WRITE_RECORDS)),
            write_bytes: Arc::new(Semaphore::new(WRITE_BYTES)),
            touches: Arc::new(Semaphore::new(64)),
            worker,
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn send(&self, command: Command) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(command);
        }
    }

    fn used(&self, usage: LookupUse) {
        // Miss/version observations are best effort and separately bounded;
        // they cannot crowd out content publication or lookups.
        let Ok(permit) = Arc::clone(&self.touches).try_acquire_owned() else {
            return;
        };
        self.send(Command::Use {
            usage,
            _permit: permit,
        });
    }

    pub(crate) async fn find(
        &self,
        epoch: u64,
        item: &str,
        version: String,
        from: u64,
        end: u64,
    ) -> Option<Record> {
        if !self.enabled || end <= from || end - from > MAX_ITEM || end > i64::MAX as u64 {
            return None;
        }
        // Opening the optional tier must not delay the normal read path either.
        if !crate::panic::lock(&self.catalog).covers(
            epoch,
            &(item.into(), version.clone()),
            from,
            end,
            None,
        ) {
            self.used(LookupUse {
                epoch,
                item: item.into(),
                version,
                from,
                end,
                at: database::now(),
            });
            return None;
        }
        let (answer, receive) = oneshot::channel();
        self.reader
            .port()
            .find(epoch, item.into(), version.clone(), from, end, answer);
        // The independent reader bypasses writes and maintenance. A slow disk is
        // still optional; SQLite lock waits consume at most 5 ms of this budget.
        let found = tokio::time::timeout(LOOKUP_WAIT, receive)
            .await
            .ok()?
            .ok()
            .flatten();
        if let Some(record) = &found {
            let wake = crate::panic::lock(&self.catalog).touch(
                epoch,
                &(item.into(), version),
                from,
                end,
                None,
                Some(record.from),
            );
            if wake {
                self.send(Command::Touched);
            }
        }
        (epoch == self.epoch()).then_some(found).flatten()
    }

    /// Never waits for SQLite, a writer or queue space. Only the reader's exact bytes.
    pub(crate) fn keep(&self, epoch: u64, item: &str, version: String, record: Record) {
        if !self.enabled
            || record.bytes.is_empty()
            || record.bytes.len() as u64 > MAX_RANGE
            || record.from > i64::MAX as u64 - MAX_RANGE
        {
            return;
        }
        let key = (item.into(), version.clone());
        let mut catalog = crate::panic::lock(&self.catalog);
        if catalog.covers(
            epoch,
            &key,
            record.from,
            record.from + record.bytes.len() as u64,
            Some(&record.facts),
        ) {
            let wake = catalog.touch(
                epoch,
                &key,
                record.from,
                record.from + record.bytes.len() as u64,
                Some(&record.facts),
                None,
            );
            if wake {
                self.send(Command::Touched);
            }
            return;
        }
        if catalog.queued(epoch, &key, &record) {
            return;
        }
        let Ok(permit) = Arc::clone(&self.writes).try_acquire_owned() else {
            return;
        };
        let Ok(bytes) =
            Arc::clone(&self.write_bytes).try_acquire_many_owned(record.bytes.len() as u32)
        else {
            return;
        };
        catalog.pending.push((
            epoch,
            key,
            catalog::Range {
                from: record.from,
                end: record.from + record.bytes.len() as u64,
                facts: record.facts.clone(),
                at: database::now(),
                used: None,
            },
        ));
        self.send(Command::Keep {
            epoch,
            item: item.into(),
            version,
            record,
            _permit: permit,
            _bytes: bytes,
        });
    }

    pub(crate) fn forget(&self, item: &str) {
        // In-flight older reads cannot republish the item after this deletion.
        let epoch = self.fence();
        self.send(Command::Forget {
            epoch,
            item: item.into(),
        });
    }

    fn fence(&self) -> u64 {
        let mut catalog = crate::panic::lock(&self.catalog);
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        catalog.epoch = epoch;
        catalog.ready = false;
        catalog.blocked = true;
        catalog.stored.clear();
        catalog.dirty = 0;
        epoch
    }

    /// Reject old queued reads/writes immediately; deletion runs in worker order.
    pub(crate) fn invalidate(&self) {
        let epoch = self.fence();
        self.send(Command::Clear {
            epoch,
            answer: None,
        });
    }

    /// Sign-out waits until the worker closes SQLite and deletes the database/sidecars.
    pub(crate) async fn clear(&self) -> anyhow::Result<()> {
        let epoch = self.fence();
        let (answer, receive) = oneshot::channel();
        self.send(Command::Clear {
            epoch,
            answer: Some(answer),
        });
        receive
            .await
            .map_err(|_| anyhow::anyhow!("header cache worker unavailable"))?
    }

    /// A barrier for deterministic tests, not a read-path flush.
    #[cfg(test)]
    pub(crate) async fn flush(&self) {
        let (answer, receive) = oneshot::channel();
        self.send(Command::Barrier(answer));
        let _ = receive.await;
    }

    #[cfg(test)]
    pub(crate) fn pause_for_test(&self) -> mpsc::Sender<()> {
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        self.send(Command::Pause {
            entered,
            release: wait,
        });
        inside
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        release
    }

    #[cfg(test)]
    pub(crate) fn pause_reader_for_test(&self) -> mpsc::Sender<()> {
        self.reader.port().pause_for_test()
    }
}

impl Drop for HeaderCache {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Worker {
    path: PathBuf,
    limit: u64,
    db: Option<Database>,
    disabled: bool,
    warned: bool,
    rebuilt: bool,
    clear_pending: bool,
    catalog: Arc<std::sync::Mutex<catalog::Catalog>>,
    reader: reader::Port,
    epoch: Arc<AtomicU64>,
}

impl Worker {
    fn unfence(&self, epoch: u64) {
        let mut catalog = crate::panic::lock(&self.catalog);
        if catalog.epoch == epoch {
            catalog.blocked = false;
        }
    }

    fn publish(&mut self, epoch: u64, item: Option<&str>) {
        if crate::panic::lock(&self.catalog).blocked {
            return;
        }
        let Some(db) = &self.db else {
            let mut catalog = crate::panic::lock(&self.catalog);
            catalog.stored.clear();
            catalog.dirty = 0;
            catalog.ready = false;
            return;
        };
        let full = {
            let catalog = crate::panic::lock(&self.catalog);
            !catalog.ready || catalog.epoch != epoch || db.removed.get()
        };
        if !full && item.is_none() {
            return;
        }
        let item = item.filter(|_| !full);
        match db.inventory(item) {
            Ok(entries) => {
                let mut catalog = crate::panic::lock(&self.catalog);
                if catalog.blocked || catalog.epoch > epoch {
                    return;
                }
                catalog.replace(entries, item.is_some());
                catalog.epoch = epoch;
                catalog.ready = true;
                db.removed.set(false);
            }
            Err(_) => {
                crate::panic::lock(&self.catalog).ready = false;
            }
        }
    }

    fn use_catalog(&mut self, epoch: u64, limit: usize) {
        let mut used = crate::panic::lock(&self.catalog).take_uses(epoch, limit);
        self.use_lookups(&mut used, epoch);
    }

    fn use_lookups(&mut self, used: &mut Vec<LookupUse>, epoch: u64) {
        used.retain(|usage| usage.epoch == epoch);
        if !used.is_empty() {
            let touched = self
                .run(|db| db.use_lookups(used).map(Some))
                .unwrap_or_default();
            if self.db.is_some() {
                let mut catalog = crate::panic::lock(&self.catalog);
                for usage in &touched {
                    if catalog.epoch != usage.epoch {
                        continue;
                    }
                    if let Some(ranges) = catalog
                        .stored
                        .get_mut(&(usage.item.clone(), usage.version.clone()))
                    {
                        for range in ranges.iter_mut().filter(|range| range.from == usage.from) {
                            range.at = usage.at;
                        }
                    }
                }
            }
        }
        used.clear();
    }

    fn warn(&mut self, error: &dyn std::fmt::Display) {
        if !self.warned {
            tracing::warn!("header cache unavailable or rebuilt: {error}");
            self.warned = true;
        }
    }

    fn start(&mut self) {
        match database::needs_clear(&self.path) {
            Ok(pending) => self.clear_pending = pending,
            Err(error) => {
                self.warn(&error);
                self.disabled = true;
            }
        }
        if self.limit == 0 {
            if let Err(error) = self.clear() {
                self.warn(&error);
            }
            self.disabled = true;
        } else {
            self.ensure_open();
        }
        if let Some(db) = &self.db {
            if let Ok((bytes, rows)) = db.stats() {
                tracing::info!("header cache: {bytes} bytes on disk, {rows} ranges");
            }
        } else {
            tracing::info!("header cache: disabled or unavailable");
        }
    }

    fn ensure_open(&mut self) {
        if self.disabled || self.db.is_some() {
            return;
        }
        if self.clear_pending {
            if let Err(error) = self.clear() {
                self.warn(&error);
                return;
            }
        }
        match Database::open(&self.path, self.limit) {
            Ok(db) => self.db = Some(db),
            Err(error) => {
                self.warn(&error);
                // Lock contention is not corruption, and never a reason to unlink a
                // database another source/process might still be using.
                if database::is_busy(&error) || self.rebuilt {
                    self.disabled = true;
                    return;
                }
                self.rebuilt = true;
                self.close_for_rebuild();
                match database::remove(&self.path)
                    .and_then(|()| Database::open(&self.path, self.limit))
                {
                    Ok(db) => self.db = Some(db),
                    Err(_) => self.disabled = true,
                }
            }
        }
    }

    /// Never reopen an invalidated database until all its files have been removed.
    /// The marker carries a failed deletion across worker/daemon restarts too.
    fn clear(&mut self) -> anyhow::Result<()> {
        self.db = None;
        self.reader.close();
        self.clear_pending = true;
        let marked = database::mark_clear(&self.path);
        let result = database::remove(&self.path).and_then(|()| database::finish_clear(&self.path));
        match result {
            Ok(()) => {
                self.clear_pending = false;
                Ok(())
            }
            Err(error) => Err(match marked {
                Ok(()) => error,
                Err(marker) => error.context(format!(
                    "could not persist header cache invalidation: {marker}"
                )),
            }),
        }
    }

    fn run<T>(
        &mut self,
        action: impl FnOnce(&mut Database) -> anyhow::Result<Option<T>>,
    ) -> Option<T> {
        self.ensure_open();
        let result = action(self.db.as_mut()?);
        match result {
            Ok(answer) => answer,
            Err(error) => {
                self.warn(&error);
                self.db = None;
                if !database::is_busy(&error) && !self.rebuilt {
                    self.rebuilt = true;
                    self.close_for_rebuild();
                    if database::remove(&self.path).is_ok() {
                        self.ensure_open();
                    } else {
                        self.disabled = true;
                    }
                } else {
                    self.disabled = true;
                }
                None
            }
        }
    }

    fn close_for_rebuild(&self) {
        {
            let mut catalog = crate::panic::lock(&self.catalog);
            // Reject lookups already admitted by the old catalog, even if they
            // reach the reader after its close acknowledgment. New lookups miss
            // until the replacement database's inventory is published.
            // Startup has not admitted any reader work and can retain its queued
            // publications; only a previously published inventory needs fencing.
            if catalog.ready || !catalog.stored.is_empty() {
                catalog.epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
            }
            catalog.ready = false;
            catalog.stored.clear();
            catalog.dirty = 0;
        }
        self.reader.close();
    }
}

#[cfg(test)]
mod tests;
