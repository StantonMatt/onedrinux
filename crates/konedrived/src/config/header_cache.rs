//! A private, disposable disk budget for small ranges actually read, one worker per account.
//!
//! SQLite and all file operations run on the worker, never on a FUSE/runtime thread.
//! Writes are best effort with at most 64 queued records (4 MiB of content); reads wait
//! only for a lookup. An epoch fences queued work against authentication invalidation.
//! The source owns the meaning of the opaque version and facts, and checks both on use.

// test: header_cache_keeps_ranges_across_restart_and_preserves_facts
// test: header_cache_bounds_ranges_items_and_evicts_lru_rows
// test: header_cache_expires_unused_rows_and_removes_other_versions
// test: header_cache_disabled_corrupt_and_new_schema_files_are_disposable
// test: header_cache_invalidation_fences_queued_writes_and_deletes_the_file
// test: header_cache_failed_deletion_blocks_reuse_and_retries_after_restart
// test: header_cache_failed_unlink_is_remembered_without_directory_write_permission
// test: account_header_cleanup_failures_are_reported_after_sign_out
// test: header_cache_publication_never_waits_for_a_paused_writer_and_has_a_bounded_queue
// test: sign_out_and_account_retirement_delete_headers_without_a_mounted_folder

mod database;

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

use super::{AccountId, ConfigStore};
use database::Database;

pub(crate) const MAX_RANGE: u64 = 64 * 1024;
pub(super) const MAX_ITEM: u64 = 256 * 1024;
pub(super) const LIFETIME: i64 = 30 * 24 * 60 * 60 * 1000;

/// A range and its source's serialized version facts. No paths or download URLs.
#[derive(Debug)]
pub(crate) struct Record {
    pub(crate) from: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) facts: String,
}

enum Command {
    Find {
        epoch: u64,
        item: String,
        version: String,
        from: u64,
        end: u64,
        answer: oneshot::Sender<Option<Record>>,
    },
    Keep {
        epoch: u64,
        item: String,
        version: String,
        record: Record,
        _permit: OwnedSemaphorePermit,
    },
    Forget {
        item: String,
    },
    Clear {
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
    worker: Option<std::thread::JoinHandle<()>>,
    enabled: bool,
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
                };
                store.start();
                loop {
                    let command = match receiver.recv_timeout(std::time::Duration::from_secs(3600))
                    {
                        Ok(command) => command,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if store.db.is_some() {
                                store.run(|db| db.prune(database::now()).map(Some));
                            }
                            continue;
                        }
                    };
                    match command {
                        Command::Find {
                            epoch,
                            item,
                            version,
                            from,
                            end,
                            answer,
                        } => {
                            let found = if epoch == current.load(Ordering::SeqCst) {
                                store.run(|db| db.find(&item, &version, from, end, database::now()))
                            } else {
                                None
                            };
                            let _ = answer.send(found);
                        }
                        Command::Keep {
                            epoch,
                            item,
                            version,
                            record,
                            ..
                        } => {
                            if epoch == current.load(Ordering::SeqCst) {
                                store.run(|db| {
                                    db.keep(&item, &version, &record, database::now()).map(Some)
                                });
                            }
                        }
                        Command::Forget { item } => {
                            if store.db.is_some() {
                                store.run(|db| db.forget(&item).map(Some));
                            }
                        }
                        Command::Clear { answer } => {
                            let result = store.clear();
                            if let Err(error) = &result {
                                store.warn(error);
                            }
                            if let Some(answer) = answer {
                                let _ = answer.send(result);
                            }
                        }
                        #[cfg(test)]
                        Command::Barrier(answer) => {
                            let _ = answer.send(());
                        }
                        #[cfg(test)]
                        Command::Pause { entered, release } => {
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                    }
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
            enabled: limit > 0 && worker.is_some(),
            sender: worker.as_ref().map(|_| sender),
            epoch,
            writes: Arc::new(Semaphore::new(64)),
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
        let (answer, receive) = oneshot::channel();
        self.send(Command::Find {
            epoch,
            item: item.into(),
            version,
            from,
            end,
            answer,
        });
        let found = receive.await.ok().flatten();
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
        let Ok(permit) = Arc::clone(&self.writes).try_acquire_owned() else {
            return;
        };
        self.send(Command::Keep {
            epoch,
            item: item.into(),
            version,
            record,
            _permit: permit,
        });
    }

    pub(crate) fn forget(&self, item: &str) {
        // In-flight older reads cannot republish the item after this deletion.
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.send(Command::Forget { item: item.into() });
    }

    /// Reject old queued reads/writes immediately; deletion runs in worker order.
    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.send(Command::Clear { answer: None });
    }

    /// Sign-out waits until the worker closes SQLite and deletes the database/sidecars.
    pub(crate) async fn clear(&self) -> anyhow::Result<()> {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let (answer, receive) = oneshot::channel();
        self.send(Command::Clear {
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
}

impl Worker {
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
}

#[cfg(test)]
mod tests;
