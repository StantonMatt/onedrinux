//! The download URLs of the files a program may read next, from one listing of their
//! folder: a range read that finds its file's URL here asks OneDrive once, where one
//! without asks three times (`GraphSource::read_range`).
//!
//! **State owned:** the URLs by item id, each with the eTag it was listed with and when;
//! and by folder, when it may be listed again. In memory, one per source, so one per
//! account's folder; nothing of it is written anywhere.
//!
//! A listing moves no content: the download guard, which counts the bytes programs read,
//! sees nothing of it. A failed listing is only no URL: the read asks the item's metadata,
//! as it would have.

// test: a_first_page_read_does_not_wait_for_later_pages
// test: concurrent_reads_of_one_folder_share_one_listing
// test: download_links_prune_expired_entries_and_cap_their_count
// test: clearing_download_links_cancels_a_background_listing

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use konedrive_graph::drive::DriveClient;

/// How long a URL is used after its folder was listed. OneDrive's live about an hour; one
/// that ends sooner costs a read the metadata it would have asked anyway
/// (`DriveError::UrlExpired`).
pub(super) const LINK_LIVES: Duration = Duration::from_secs(40 * 60);

/// How long a folder whose listing failed is not listed again: its reads ask the metadata
/// meanwhile.
const FAILED_LISTING_WAITS: Duration = Duration::from_secs(60);

/// The children one listing of a folder reads at most: normally five pages of 200,
/// and never more than six pages even if Graph returns short or empty ones. A file
/// beyond the bounded listing has no URL here and its reads ask the metadata.
pub(super) const FOLDER_LISTING_BOUND: usize = 1_000;

/// A file's download URL and the eTag OneDrive listed it with. The URL is not bound to
/// that version (measured on OneDrive personal, 2026-10-29: a URL listed before an edit
/// serves the new content, with the new eTag in its `ETag`), so the eTag says only what
/// the file was when listed; the answer's own `ETag` says what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Link {
    pub(super) url: String,
    pub(super) etag: String,
}

struct Held {
    link: Link,
    since: Instant,
}

/// How many URLs are held, even when all are still fresh. The oldest goes first.
const LINKS_AT_MOST: usize = 10_000;

struct Listing {
    /// `None` while pages are arriving; completion says when the folder may be listed again.
    progress: tokio::sync::watch::Receiver<Option<Instant>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listing {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The download URLs of one source. Dropping or clearing it aborts its background listings.
#[derive(Default)]
pub(super) struct DownloadLinks {
    links: Mutex<HashMap<String, Held>>,
    folders: Mutex<HashMap<String, Arc<Listing>>>,
    /// Prevent a page already arriving when sign-out clears the cache from repopulating it.
    generation: std::sync::atomic::AtomicU64,
}

impl DownloadLinks {
    /// The URL of `item_id`, while it lives.
    pub(super) fn find(&self, item_id: &str) -> Option<Link> {
        let mut links = crate::panic::lock(&self.links);
        links.retain(|_, held| held.since.elapsed() < LINK_LIVES);
        links.get(item_id).map(|held| held.link.clone())
    }

    /// `link` is the URL of `item_id` from now: one the item's metadata gave.
    pub(super) fn keep(&self, item_id: &str, link: Link) {
        let mut links = crate::panic::lock(&self.links);
        Self::keep_in(&mut links, item_id.to_owned(), link, Instant::now());
    }

    fn keep_in(links: &mut HashMap<String, Held>, item_id: String, link: Link, since: Instant) {
        links.retain(|_, held| held.since.elapsed() < LINK_LIVES);
        links.insert(item_id, Held { link, since });
        while links.len() > LINKS_AT_MOST {
            let oldest = links.iter().min_by_key(|(_, held)| held.since).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest { links.remove(&oldest); }
        }
    }

    /// The URL of `item_id` is not used again: it expired, or its answer was not of the
    /// version it was asked for.
    pub(super) fn drop_link(&self, item_id: &str) {
        crate::panic::lock(&self.links).remove(item_id);
    }

    /// Forget every URL and abort pending listings when authentication is invalidated.
    pub(super) fn clear(&self) {
        let mut folders = crate::panic::lock(&self.folders);
        for listing in folders.values() { listing.task.abort(); }
        folders.clear();
        let mut links = crate::panic::lock(&self.links);
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        links.clear();
    }

    /// List at most [`FOLDER_LISTING_BOUND`] children, sharing one background listing per
    /// folder. Wait only until `item_id` arrives or that listing ends. Other pages continue
    /// after this read returns; failures back off for [`FAILED_LISTING_WAITS`].
    pub(super) async fn list_folder(self: &Arc<Self>, drive: &DriveClient, folder_id: &str, item_id: &str) {
        let listing = {
            let mut folders = crate::panic::lock(&self.folders);
            let now = Instant::now();
            folders.retain(|_, listing| listing.progress.borrow().is_none_or(|again| now < again));
            Arc::clone(folders.entry(folder_id.to_owned()).or_insert_with(|| {
                let (progress, receiver) = tokio::sync::watch::channel(None);
                let (source, drive, folder_id) = (Arc::downgrade(self), drive.clone(), folder_id.to_owned());
                let generation = self.generation.load(std::sync::atomic::Ordering::SeqCst);
                let task = tokio::spawn(async move {
                    let asked = Instant::now();
                    let result = drive.visit_children_with_download_urls(&folder_id, FOLDER_LISTING_BOUND, |children| {
                        if let Some(source) = source.upgrade() {
                            let mut links = crate::panic::lock(&source.links);
                            if source.generation.load(std::sync::atomic::Ordering::SeqCst) != generation || !drive.cached_content_allowed() {
                                return;
                            }
                            for child in children {
                                if let (Some(url), Some(etag)) = (child.download_url, child.e_tag.filter(|etag| !etag.is_empty())) {
                                    Self::keep_in(&mut links, child.id, Link { url, etag }, asked);
                                }
                            }
                        }
                        progress.send_replace(None);
                    }).await;
                    let again = match result {
                        Ok(()) => asked + LINK_LIVES,
                        Err(error) => {
                            tracing::info!("the download links of folder {folder_id} were not listed; its files are read by their metadata: {error}");
                            Instant::now() + FAILED_LISTING_WAITS
                        }
                    };
                    progress.send_replace(Some(again));
                });
                Arc::new(Listing { progress: receiver, task })
            }))
        };
        let mut progress = listing.progress.clone();
        loop {
            // Mark the observed progress before looking for the URL, so no page notification
            // can be lost between the lookup and waiting for the next page.
            let finished = progress.borrow_and_update().is_some();
            if self.find(item_id).is_some() || finished { return; }
            if progress.changed().await.is_err() { return; }
        }
    }
}

#[cfg(test)]
mod tests;
