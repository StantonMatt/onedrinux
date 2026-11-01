//! A read-only SQLite connection on its own worker: foreground lookups never sit
//! behind publication, LRU or vacuum commands. Closing is acknowledged before deletion.
// test: header_cache_rebuild_rejects_reader_reopen_from_the_replaced_generation
// test: a_ready_prefetch_answers_without_waiting_for_a_paused_disk_lookup
// test: header_cache_lookup_bypasses_queued_writes
// test: header_cache_invalidation_fences_queued_writes_and_deletes_the_file

use super::{database, Record};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use tokio::sync::oneshot;

enum Command {
    Find {
        epoch: u64,
        item: String,
        version: String,
        from: u64,
        end: u64,
        answer: oneshot::Sender<Option<Record>>,
    },
    Close(mpsc::Sender<()>),
    #[cfg(test)]
    Pause {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
}

#[derive(Clone)]
pub(super) struct Port(mpsc::Sender<Command>);

impl Port {
    pub(super) fn find(
        &self,
        epoch: u64,
        item: String,
        version: String,
        from: u64,
        end: u64,
        answer: oneshot::Sender<Option<Record>>,
    ) {
        let _ = self.0.send(Command::Find {
            epoch,
            item,
            version,
            from,
            end,
            answer,
        });
    }

    pub(super) fn close(&self) {
        let (answer, receive) = mpsc::channel();
        if self.0.send(Command::Close(answer)).is_ok() {
            let _ = receive.recv();
        }
    }

    #[cfg(test)]
    pub(super) fn pause_for_test(&self) -> mpsc::Sender<()> {
        let (entered, inside) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        self.0
            .send(Command::Pause {
                entered,
                release: wait,
            })
            .unwrap();
        inside
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        release
    }
}

pub(super) struct Reader {
    port: Option<Port>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Reader {
    pub(super) fn open(path: PathBuf, current: Arc<AtomicU64>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("header-reader".into())
            .spawn(move || {
                let mut connection: Option<(u64, rusqlite::Connection)> = None;
                for command in receiver {
                    match command {
                        #[cfg(test)]
                        Command::Pause { entered, release } => {
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                        Command::Close(answer) => {
                            connection = None;
                            let _ = answer.send(());
                        }
                        Command::Find {
                            epoch,
                            item,
                            version,
                            from,
                            end,
                            answer,
                        } => {
                            if answer.is_closed() || epoch != current.load(Ordering::SeqCst) {
                                continue;
                            }
                            if connection
                                .as_ref()
                                .is_none_or(|(opened, _)| *opened != epoch)
                            {
                                // The catalog publishes only after the writer has opened
                                // and checked this private database; never create a file.
                                connection = rusqlite::Connection::open_with_flags(
                                    &path,
                                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                                )
                                .and_then(|conn| {
                                    conn.busy_timeout(std::time::Duration::from_millis(5))?;
                                    Ok((epoch, conn))
                                })
                                .ok();
                            }
                            let found = connection.as_ref().and_then(|(_, conn)| {
                                database::lookup(conn, &item, &version, from, end, database::now())
                                    .ok()
                                    .flatten()
                            });
                            let _ = answer.send(
                                (epoch == current.load(Ordering::SeqCst))
                                    .then_some(found)
                                    .flatten(),
                            );
                        }
                    }
                }
            });
        Self {
            port: Some(Port(sender)),
            worker: worker.ok(),
        }
    }

    pub(super) fn port(&self) -> Port {
        self.port.as_ref().unwrap().clone()
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.port.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
