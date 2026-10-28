//! Session-only attribution of whole copies. Tickets are made before start-or-join;
//! only the starter's ticket can land. Explicit keep requests cancel tickets, including
//! ones still downloading, so their eventual completion cannot restore attribution.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use konedrive_tree::{ContentVersion, LocalItemId};

/// Across all apps/accounts, including transfers in flight. At capacity new downloads
/// remain untracked until space is available; no existing attribution is displaced.
const MAX_DOWNLOADS: usize = 10_000;

#[derive(Default)]
pub(crate) struct AppDownloads {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    copies: BTreeMap<u64, Copy>,
}

struct Copy {
    identity: String,
    store: PathBuf,
    item: LocalItemId,
    content: ContentVersion,
    landed: bool,
}

/// One account's view of the shared bounded ledger. Store identity survives remounts;
/// item ids and content versions avoid attributing a different file at the same name.
#[derive(Clone)]
pub(crate) struct AccountDownloads {
    ledger: Arc<AppDownloads>,
    store: PathBuf,
}

#[derive(Clone)]
pub(crate) struct DownloadTicket(Arc<Ticket>);

struct Ticket {
    ledger: Arc<AppDownloads>,
    id: u64,
}

#[derive(Clone)]
pub(crate) struct AppCopy {
    id: u64,
    pub(crate) item: LocalItemId,
    pub(crate) content: ContentVersion,
}

impl AppDownloads {
    pub(crate) fn account(self: &Arc<Self>, store: &Path) -> AccountDownloads {
        AccountDownloads {
            ledger: Arc::clone(self),
            store: store.to_owned(),
        }
    }
}

impl AccountDownloads {
    pub(crate) fn begin(
        &self,
        identity: &str,
        item: LocalItemId,
        content: ContentVersion,
    ) -> Option<DownloadTicket> {
        let mut inner = crate::panic::lock(&self.ledger.inner);
        if inner.copies.len() >= MAX_DOWNLOADS {
            return None;
        }
        // Never reuse a ticket, even if a theoretically exhausted counter is reached.
        inner.next = inner.next.checked_add(1)?;
        let id = inner.next;
        inner.copies.insert(
            id,
            Copy {
                identity: identity.into(),
                store: self.store.clone(),
                item,
                content,
                landed: false,
            },
        );
        Some(DownloadTicket(Arc::new(Ticket {
            ledger: Arc::clone(&self.ledger),
            id,
        })))
    }

    pub(crate) fn landed(&self, identity: &str) -> Vec<AppCopy> {
        crate::panic::lock(&self.ledger.inner)
            .copies
            .iter()
            .filter_map(|(id, copy)| {
                (copy.store == self.store && copy.identity == identity && copy.landed).then(|| {
                    AppCopy {
                        id: *id,
                        item: copy.item,
                        content: copy.content.clone(),
                    }
                })
            })
            .collect()
    }

    /// Recheck a snapshot on the write-operation thread: a concurrent keep/free or
    /// second invocation may have cancelled it while this invocation was queued.
    pub(crate) fn contains(&self, copy: &AppCopy) -> bool {
        crate::panic::lock(&self.ledger.inner)
            .copies
            .get(&copy.id)
            .is_some_and(|entry| entry.store == self.store && entry.landed)
    }

    pub(crate) fn forget(&self, copy: &AppCopy) {
        crate::panic::lock(&self.ledger.inner)
            .copies
            .remove(&copy.id);
    }

    pub(crate) fn items(&self) -> Vec<LocalItemId> {
        crate::panic::lock(&self.ledger.inner)
            .copies
            .values()
            .filter(|copy| copy.store == self.store)
            .map(|copy| copy.item)
            .collect()
    }

    pub(crate) fn forget_item(&self, item: LocalItemId) {
        crate::panic::lock(&self.ledger.inner)
            .copies
            .retain(|_, copy| copy.store != self.store || copy.item != item);
    }
}

impl DownloadTicket {
    /// Called only after an actual link and successful row commit, on the same write
    /// thread as explicit keep invalidation and free-up. AlreadyOnDisk never calls it.
    pub(crate) fn landed(&self) {
        let mut inner = crate::panic::lock(&self.0.ledger.inner);
        if let Some(copy) = inner.copies.get_mut(&self.0.id) {
            copy.landed = true;
            let (store, item) = (copy.store.clone(), copy.item);
            // A newly landed whole copy supersedes any previous attribution for the
            // same item, even if a missing backing copy was repaired in between.
            inner
                .copies
                .retain(|id, copy| *id == self.0.id || copy.store != store || copy.item != item);
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut inner = crate::panic::lock(&self.ledger.inner);
        if inner.copies.get(&self.id).is_some_and(|copy| !copy.landed) {
            inner.copies.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests;
