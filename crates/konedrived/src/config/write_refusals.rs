//! Ephemeral, per-account reports of refused program writes. Admission only tries a
//! bounded channel; procfs lookup and D-Bus delivery run away from the FUSE request.
// test: write_refusals_emit_first_then_coalesce_and_count_overflow
// test: write_refusals_keep_accounts_separate_and_forget_dead_accounts

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

use super::{AccountId, ConfigStore};

pub(crate) const WINDOW: Duration = Duration::from_secs(10);
const CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub(crate) struct RefusedWrite {
    pub path: PathBuf,
    /// Empty means the drive root: never offer Allow This Folder for it.
    pub folder: PathBuf,
    pub pid: u32,
}

pub(crate) struct WriteRefusals {
    sender: mpsc::Sender<RefusedWrite>,
    receiver: Mutex<Option<mpsc::Receiver<RefusedWrite>>>,
    overflow: Arc<AtomicU64>,
    overflow_ready: Arc<Notify>,
}

impl Default for WriteRefusals {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            overflow: Arc::default(),
            overflow_ready: Arc::default(),
        }
    }
}

impl WriteRefusals {
    pub(crate) fn record(&self, path: PathBuf, folder: PathBuf, pid: u32) {
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.sender.try_send(RefusedWrite { path, folder, pid })
        {
            let _ = self
                .overflow
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    Some(count.saturating_add(1))
                });
            self.overflow_ready.notify_one();
        }
    }

    /// One consumer per account export. The exporter keeps this owner alive until the
    /// account is removed; an unused weak entry in ConfigStore is then swept away.
    pub(crate) fn take_receiver(&self) -> Option<RefusalReceiver> {
        Some(RefusalReceiver {
            receiver: crate::panic::lock(&self.receiver).take()?,
            overflow: self.overflow.clone(),
            overflow_ready: self.overflow_ready.clone(),
            deadline: None,
            last: None,
        })
    }
}

pub(crate) struct RefusalReceiver {
    receiver: mpsc::Receiver<RefusedWrite>,
    overflow: Arc<AtomicU64>,
    deadline: Option<Instant>,
    last: Option<RefusedWrite>,
    overflow_ready: Arc<Notify>,
}

impl RefusalReceiver {
    /// The first refusal is ready immediately. Later details collapse to the first
    /// queued refusal and a count, at most once per window, even under sustained load.
    pub(crate) async fn next(&mut self) -> Option<(RefusedWrite, u64)> {
        if let Some(deadline) = self.deadline {
            tokio::time::sleep_until(deadline).await;
        }
        loop {
            let (event, mut count) = if let Ok(event) = self.receiver.try_recv() {
                (event, 1u64)
            } else if self.overflow.load(Ordering::Relaxed) != 0 {
                // A Full result can race with the drain: its count arrives after
                // the previous swap. Wake and flush it even if the burst ended.
                (self.last.clone()?, 0)
            } else {
                tokio::select! {
                    event = self.receiver.recv() => (event?, 1),
                    _ = self.overflow_ready.notified() => continue,
                }
            };
            if self.deadline.is_some() {
                // Bounded work: a producer cannot keep this drain running forever.
                for _ in count..CAPACITY as u64 {
                    if self.receiver.try_recv().is_err() {
                        break;
                    }
                    count = count.saturating_add(1);
                }
                count = count.saturating_add(self.overflow.swap(0, Ordering::Relaxed));
            }
            self.last = Some(event.clone());
            self.deadline = Some(Instant::now() + WINDOW);
            return Some((event, count));
        }
    }
}

impl ConfigStore {
    pub(crate) fn write_refusals(&self, id: &AccountId) -> Arc<WriteRefusals> {
        let mut accounts = crate::panic::lock(&self.write_refusals);
        accounts.retain(|_, reports| reports.strong_count() != 0);
        if let Some(reports) = accounts.get(id).and_then(std::sync::Weak::upgrade) {
            return reports;
        }
        let reports = Arc::new(WriteRefusals::default());
        accounts.insert(id.clone(), Arc::downgrade(&reports));
        reports
    }
}

#[cfg(test)]
mod tests;
