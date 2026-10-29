//! The store shared by the tasks of one folder: one thread owns each connection.

use std::path::PathBuf;

use super::{ReadStore, TreeError, TreeStore};

/// A job for a store's owner thread: a closure over the store, which sends
/// its own answer.
type Job = Box<dyn FnOnce(&mut TreeStore) + Send>;

/// Jobs a store's channel holds before a sender waits.
pub const QUEUE: usize = 1024;

/// Hands out the ids of the owner threads.
static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

thread_local! {
    /// The id of the store this thread owns; 0 on any other thread.
    static OWNING: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One thread that owns a connection and runs the jobs sent to it, one at a
/// time, in the order they arrive.
struct Owner {
    jobs: tokio::sync::mpsc::Sender<Job>,
    id: u64,
    observer: std::sync::Arc<std::sync::Mutex<Option<crate::item_changes::ItemObserver>>>,
    /// True once the thread has dropped its connection ([`Store::close`]).
    closed: tokio::sync::watch::Receiver<bool>,
}

impl Owner {
    /// Starts the thread. It ends, dropping the connection, once every
    /// sender is gone and the jobs already queued have run.
    fn spawn(mut store: TreeStore, name: &str) -> Self {
        let id = NEXT_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (jobs, mut queue) = tokio::sync::mpsc::channel::<Job>(QUEUE);
        let (closing, closed) = tokio::sync::watch::channel(false);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                OWNING.with(|owning| owning.set(id));
                while let Some(job) = queue.blocking_recv() {
                    // A job that panics answers nobody (its caller gets an error);
                    // its transaction, if any, is rolled back as it is dropped.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut store))).is_err() {
                        tracing::error!("a job of the tree store panicked; the store goes on");
                    }
                }
                // The connection is closed before anyone is told so.
                drop(store);
                let _ = closing.send(true);
            })
            .expect("the tree store's thread starts");
        Owner { jobs, id, closed, observer: Default::default() }
    }

    /// A call from inside one of this owner's jobs would wait for itself:
    /// a bug, which panics in debug and test builds and is an error otherwise.
    fn refuse_reentry(&self) -> Result<(), TreeError> {
        if OWNING.with(|owning| owning.get()) != self.id {
            return Ok(());
        }
        debug_assert!(false, "a job of the tree store called the store: it would wait for itself");
        Err(TreeError::Io(std::io::Error::other("a job of the tree store called the store")))
    }

    fn job<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static,
    ) -> (Job, tokio::sync::oneshot::Receiver<Result<T, TreeError>>) {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let observer = self.observer.clone();
        let job: Job = Box::new(move |store| {
            let result = f(store);
            if let Some(observer) = &*crate::lock(&observer) {
                if let Err(error) = observer(store) {
                    tracing::error!("mount derived state could not be updated: {error}");
                }
            }
            let _ = answer.send(result);
        });
        (job, answered)
    }

    async fn call<T: Send + 'static>(&self, f: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static) -> Result<T, TreeError> {
        self.refuse_reentry()?;
        let (job, answered) = self.job(f);
        self.jobs.send(job).await.map_err(|_| stopped())?;
        answered.await.map_err(|_| failed())?
    }

    fn call_blocking<T: Send + 'static>(&self, f: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static) -> Result<T, TreeError> {
        self.refuse_reentry()?;
        let (job, answered) = self.job(f);
        self.jobs.blocking_send(job).map_err(|_| stopped())?;
        answered.blocking_recv().map_err(|_| failed())?
    }
}

fn stopped() -> TreeError {
    TreeError::Io(std::io::Error::other("the tree store's thread has stopped"))
}

fn failed() -> TreeError {
    TreeError::Io(std::io::Error::other("a job of the tree store failed"))
}

/// The store, shared by the tasks of one folder: the listing, the
/// mount, the plan's task and the D-Bus queries. One thread owns
/// its connection, and everyone else sends it jobs: `call` from
/// async code, `call_blocking` from plain threads. Only that thread holds a
/// read-write connection to the store; the bus's reads go to a second,
/// read-only connection with a thread of its own.
#[derive(Clone)]
pub struct Store {
    owner: std::sync::Arc<Owner>,
    /// The read-only connection's owner ([`Store::read`]), started when
    /// first used; `None` inside when it cannot be opened.
    reader: std::sync::Arc<std::sync::OnceLock<Option<Owner>>>,
    path: Option<PathBuf>,
}

impl Store {
    pub fn new(store: TreeStore) -> Self {
        let path = store.path.clone();
        let owner = Owner::spawn(store, "konedrive-store");
        Self {
            owner: std::sync::Arc::new(owner),
            reader: Default::default(),
            path,
        }
    }

    /// Installs the mount's derived-state observer. Invoked after each writer job,
    /// before its caller is answered; it must never reenter the store. A failed
    /// observation keeps the dirty set for the next job. Replacing it is supported
    /// when the same store is mounted again.
    pub fn observe_items(&self, observer: impl Fn(&crate::ItemReader<'_>, crate::ItemChanges) -> Result<(), TreeError> + Send + 'static) -> Result<(), TreeError> {
        let target = self.owner.observer.clone();
        self.call_blocking(move |store| {
            let installed = crate::item_changes::install(store, observer)?;
            *crate::lock(&target) = Some(installed);
            Ok(())
        })
    }

    /// Lets go of this handle and waits until the store's connections are closed: that is,
    /// until every other clone has been dropped too and the owner threads have ended. For
    /// whoever removes the store's files, which must not go while a connection is open:
    /// SQLite's last close removes the journal beside the database by name, and would
    /// take a newer store's with it. The caller bounds the wait.
    pub fn close(self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut waits = vec![self.owner.closed.clone()];
        if let Some(Some(reader)) = self.reader.get() {
            waits.push(reader.closed.clone());
        }
        drop(self);
        async move {
            for mut closed in waits {
                // An owner thread that ended without saying so has closed too.
                let _ = closed.wait_for(|closed| *closed).await;
            }
        }
    }

    /// Runs `f` on the store's thread and waits for its answer: for async
    /// code. Jobs run one at a time, in the order they arrive; `f` may hold a
    /// transaction, and must never await, block on anything but SQLite, or
    /// call the store.
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static,
    ) -> Result<T, TreeError> {
        self.owner.call(f).await
    }

    /// [`call`](Self::call) for plain threads (the mount's write-operation thread, the
    /// body of a `spawn_blocking`): waits for the answer.
    /// On an async runtime's thread it panics (tokio refuses to block there).
    pub fn call_blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static,
    ) -> Result<T, TreeError> {
        self.owner.call_blocking(f)
    }

    /// The read-only connection's owner, started when first asked for; `None`
    /// for a store in memory, or one that cannot be opened for reading alone.
    fn reader(&self) -> Option<&Owner> {
        let path = self.path.as_ref()?;
        self.reader
            .get_or_init(|| match TreeStore::open_read_only(path) {
                Ok(opened) => Some(Owner::spawn(opened, "konedrive-store-read")),
                Err(e) => {
                    tracing::warn!("the tree store cannot be opened for reading alone ({e}); it is read through its own thread");
                    None
                }
            })
            .as_ref()
    }

    /// Runs `job` on the thread of the store's read-only connection, which never waits for
    /// a writer and sees what was last committed; on the store's own thread for a store in
    /// memory, or one whose second connection cannot be opened. What every read of the
    /// shared store goes through, whatever view of the store it hands out.
    pub(crate) async fn call_reading_connection<T: Send + 'static>(&self, job: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static) -> Result<T, TreeError> {
        match self.reader() {
            Some(reader) => reader.call(job).await,
            None => self.call(job).await,
        }
    }

    /// [`call_reading_connection`](Self::call_reading_connection) for plain threads.
    pub(crate) fn call_reading_connection_blocking<T: Send + 'static>(&self, job: impl FnOnce(&mut TreeStore) -> Result<T, TreeError> + Send + 'static) -> Result<T, TreeError> {
        match self.reader() {
            Some(reader) => reader.call_blocking(job),
            None => self.call_blocking(job),
        }
    }

    /// Runs `f` on the store's read-only connection, which never waits for a
    /// writer and sees what was last committed: the bus's lists and
    /// sums. A store in memory, or one whose second connection cannot be
    /// opened, is read through its own thread; `f` is handed the same
    /// [`ReadStore`] either way.
    pub async fn read<T: Send + 'static>(&self, f: impl FnOnce(&ReadStore<'_>) -> Result<T, TreeError> + Send + 'static) -> Result<T, TreeError> {
        self.call_reading_connection(move |store: &mut TreeStore| f(&ReadStore::of(store))).await
    }

    /// [`read`](Self::read) for plain threads.
    pub fn read_blocking<T: Send + 'static>(&self, f: impl FnOnce(&ReadStore<'_>) -> Result<T, TreeError> + Send + 'static) -> Result<T, TreeError> {
        self.call_reading_connection_blocking(move |store: &mut TreeStore| f(&ReadStore::of(store)))
    }

    /// Puts what was last committed on disk, as a commit with `synchronous = FULL` would: the
    /// store's write-ahead log synced. The store commits with `NORMAL`, whose last commits a
    /// power loss may take back; this is for a caller that tells a program its change is on
    /// disk (an fsync of a directory). Nothing for a store in memory, or one with no log on
    /// disk: nothing waits in it.
    pub fn sync_write_ahead_log(&self) -> std::io::Result<()> {
        // Serialize with commits and checkpoints: the WAL cannot disappear between
        // choosing what to sync and the sync itself.
        self.call_blocking(|store| store.sync_write_ahead_log()).map_err(|error| match error {
            TreeError::Io(error) => error,
            error => std::io::Error::other(error),
        })
    }

    /// How many handles of this store there are, this one among them: what a test asks to
    /// see that a part it stopped and dropped holds none any more.
    #[cfg(any(test, feature = "testing"))]
    pub fn count_handles(&self) -> usize {
        std::sync::Arc::strong_count(&self.owner)
    }

}

impl TreeStore {
    /// Sync the committed WAL on its owning thread. If a checkpoint has removed it,
    /// the committed state is in the main database instead. Sync the directory too,
    /// so a newly created WAL name survives a power loss.
    pub fn sync_write_ahead_log(&self) -> Result<(), TreeError> {
        let Some(path) = &self.path else { return Ok(()) };
        let log = crate::schema::wal_path(path);
        match std::fs::File::open(log) {
            Ok(log) => log.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::File::open(path)?.sync_all()?,
            Err(error) => return Err(error.into()),
        }
        crate::schema::sync_store_parent(path)?;
        Ok(())
    }
}
