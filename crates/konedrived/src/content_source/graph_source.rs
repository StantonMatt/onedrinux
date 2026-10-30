//! The content source of a folder that shows OneDrive. A whole download asks Graph for
//! the item's metadata afresh — the cTag and quickXorHash the download checks the bytes
//! against, and a download URL that has not expired — and streams from the offset asked
//! for, to the end of the file or to the end of the piece asked for.
//!
//! A range a program reads is asked one of two ways (`docs/design/mount.md` §5.1):
//!
//! - **By the listed version**, in one request: where the store says what OneDrive listed
//!   of the opened content (`ListedContent`: its eTag and size) and a download URL of the
//!   file is at hand ([`links`]: one listing of its folder gives the URLs of all its
//!   files), the range is asked of that URL, and taken only when the answer's `ETag` is the
//!   listed eTag and the whole size it says is the listed size. A file up to
//!   [`read_blocks::SMALL_FILE`] is asked whole; larger files fetch aligned 1 MiB blocks.
//!   Both paths keep proven bytes in [`read_blocks`], shared under a 64 MiB bound.
//! - **By the metadata**, in three: the item's metadata, the range, and the metadata
//!   again, which must not have changed. Whenever the first way cannot be taken or its
//!   answer is not the listed version: no URL, an expired one, another `ETag` or size, or
//!   neither said.
//!
//! Either way, every range of one opened content must be of the version the first was.
//! Cached URLs and bytes require a fresh authenticated token, checked in memory; token
//! invalidation clears both caches and aborts pending folder listings.

// test: a_hash_is_kept_without_a_ctag_and_hashless_graph_is_still_a_remote_version
// test: ranges_refuse_a_changed_metadata_version_or_download_etag
// test: a_range_that_changes_during_its_body_is_never_answered
// test: a_new_content_generation_does_not_inherit_an_earlier_ranges_version
// test: a_range_with_a_listed_link_asks_onedrive_once
// test: reads_of_a_folders_files_cost_one_listing_and_one_request_each
// test: a_file_changed_behind_its_listed_link_is_never_served_as_the_opened_version
// test: an_expired_link_falls_back_to_the_metadata_and_is_refreshed
// test: a_failed_listing_falls_back_to_the_metadata
// test: concurrent_reads_of_one_folder_share_one_listing
// test: a_changed_answer_through_a_listed_link_is_dropped_and_the_metadata_asked
// test: signed_out_or_revoked_accounts_never_serve_cached_links_or_small_files
// test: a_first_page_read_does_not_wait_for_later_pages
// test: incompatible_download_etags_do_not_recache_the_metadata_url
// test: failed_cached_downloads_drop_the_link_and_take_the_metadata_path
// test: a_refused_listed_binding_sends_no_request
// test: a_five_mib_file_in_128_kib_reads_costs_five_blocks
// test: an_uncached_crossing_read_fetches_both_blocks_and_clips_the_last
// test: the_careful_path_fetches_and_remembers_whole_blocks
// test: a_later_block_cannot_switch_the_version_even_after_cache_eviction
// test: an_incomplete_block_is_never_cached_or_partly_answered
// test: read_ahead_uses_only_listed_proof_and_never_falls_back_to_metadata
// test: read_ahead_and_a_real_read_share_one_request_and_keep_the_single_version_binding
// test: signing_out_cancels_speculative_bodies
// test: a_real_read_keeps_its_joined_request_when_speculation_is_cancelled

mod links;
mod read_blocks;

use std::time::{Duration, SystemTime};
use std::sync::Arc;

use tokio::io::AsyncReadExt;

use async_trait::async_trait;
use futures_util::{future::{BoxFuture, Shared}, FutureExt};

use super::source::{ContentSource, Fetched, ListedContent, SourceError, Version};
use konedrive_graph::drive::{DriveClient, DriveError, DriveItem};
use konedrive_graph::quickxor::decode_base64;
use konedrive_tree::ContentVersion;
use links::{DownloadLinks, Link};
use read_blocks::{ReadBlocks, Piece, covering};

/// What the first range of an opened content asked by the metadata was bound to: the
/// metadata version, the metadata eTag where there is no cTag, and the download
/// response's `ETag`.
type CheckedIdentity = (Option<Version>, Option<String>, Option<String>);

/// The version every range of one opened content is held to: set by its first range,
/// by whichever way it was asked.
#[derive(Default)]
struct Bound {
    /// The first range asked by the metadata.
    checked: Option<CheckedIdentity>,
    /// The `ETag` of the first range taken by the listed version.
    listed: Option<String>,
}

/// Whether a range taken by the listed version may be answered, by what its content is bound to.
enum ListedBinding {
    Bound,
    /// Its content is bound to another version: the range is refused.
    Refused,
    /// Its content was bound by the metadata, to a response `ETag` that is not this one or
    /// to none: only the metadata can say.
    AskMetadata,
}

/// A listed piece stays in flight while either speculation or a real read needs it.
struct Prepared { read: Shared<BoxFuture<'static, Result<bool, SourceError>>> }
type PreparedKey = (String, ContentVersion, u64, u64);

#[derive(Clone)]
pub(crate) struct GraphSource {
    drive: DriveClient,
    /// Listed pieces in flight, shared while speculation or real readers need them.
    pending: Arc<std::sync::Mutex<std::collections::HashMap<PreparedKey, std::sync::Weak<Prepared>>>>,
    invalidated: Arc<tokio::sync::Notify>,
    /// By item and opened content, the version its ranges are bound to. An expired URL
    /// never lets a later range of the same opened content silently switch versions.
    ranges: Arc<std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<ContentVersion, Bound>>>>,
    /// Serializes cache publication with token invalidation; never held across await.
    cache_use: Arc<std::sync::Mutex<()>>,
    links: Arc<DownloadLinks>,
    read_blocks: Arc<ReadBlocks>,
    /// Keeps the token source's weak invalidation callback registered until this source drops.
    _on_invalidate: Arc<konedrive_graph::token::InvalidateHook>,
}

impl GraphSource {
    pub fn new(drive: DriveClient) -> Self {
        let links = Arc::new(DownloadLinks::default());
        let read_blocks = Arc::new(ReadBlocks::default());
        let cache_use = Arc::new(std::sync::Mutex::new(()));
        let clear_use = Arc::clone(&cache_use);
        let (clear_links, clear_files) = (Arc::clone(&links), Arc::clone(&read_blocks));
        let invalidated = Arc::new(tokio::sync::Notify::new());
        let cancel = Arc::clone(&invalidated);
        let on_invalidate: Arc<konedrive_graph::token::InvalidateHook> = Arc::new(move || {
            let _cache_use = crate::panic::lock(&clear_use);
            cancel.notify_waiters();
            clear_links.clear();
            clear_files.clear();
        });
        drive.on_token_invalidate(&on_invalidate);
        Self { drive, pending: Default::default(), invalidated, ranges: Default::default(), cache_use, links, read_blocks, _on_invalidate: on_invalidate }
    }

    fn prepared_key(item_id: &str, opened_as: &ContentVersion, from: u64, end: u64) -> PreparedKey {
        let (from, end) = covering(from, end, opened_as.size());
        (item_id.to_owned(), opened_as.clone(), from, end)
    }

    fn listed_read(&self, item_id: &str, opened_as: &ContentVersion, listed: &ListedContent, from: u64, end: u64) -> Arc<Prepared> {
        let mut pending = crate::panic::lock(&self.pending);
        pending.retain(|_, weak| weak.strong_count() > 0);
        let entry = pending.entry(Self::prepared_key(item_id, opened_as, from, end)).or_default();
        if let Some(read) = entry.upgrade() { return read; }
        let (source, item_id, opened_as, listed) = (self.clone(), item_id.to_owned(), opened_as.clone(), listed.clone());
        // The map owns only a weak reference, and this future owns no Prepared. The
        // last interested reader dropping it cancels the HTTP request without a task.
        let read = async move {
            let cancelled = source.invalidated.notified();
            tokio::pin!(cancelled);
            cancelled.as_mut().enable();
            if !source.drive.cached_content_allowed() { return Ok(false); }
            if source.cached_range(&item_id, &opened_as, from, end).is_some() { return Ok(true); }
            tokio::select! {
                biased;
                _ = cancelled => Err(SourceError::SignedOut("the listed read's authentication was invalidated".into())),
                answer = source.read_listed(&item_id, &opened_as, &listed, from, end) => answer.map(|answer| answer.is_some()),
            }
        }.boxed().shared();
        let prepared = Arc::new(Prepared { read });
        *entry = Arc::downgrade(&prepared);
        prepared
    }

    /// A range of `opened_as` by the listed version, in one request: `None` when it cannot
    /// be taken that way, and the metadata is to be asked. See the module's documentation.
    async fn read_listed(&self, item_id: &str, opened_as: &ContentVersion, listed: &ListedContent, from: u64, end: u64) -> Result<Option<Fetched>, SourceError> {
        let ContentVersion::Hashed { hash, ctag, .. } = opened_as else { return Ok(None) };
        if listed.etag.is_empty() || listed.size == 0 {
            return Ok(None);
        }
        match self.bind_listed(item_id, opened_as, &listed.etag, false) {
            ListedBinding::Refused => return Err(SourceError::NotFound("the range's version changed".into())),
            ListedBinding::AskMetadata => return Ok(None),
            ListedBinding::Bound => {}
        }
        let link = match self.links.find(item_id) {
            Some(link) => link,
            None => {
                let Some(folder_id) = &listed.folder_id else { return Ok(None) };
                self.links.list_folder(&self.drive, folder_id, item_id).await;
                let Some(link) = self.links.find(item_id) else { return Ok(None) };
                link
            }
        };
        // Listed with another eTag: the file changed or moved since one of the two
        // listings, and the answer could not be taken.
        if link.etag != listed.etag { return Ok(None); }
        if !self.drive.cached_content_allowed() { return Ok(None); }
        let (ask_from, ask_end) = covering(from, end, listed.size);
        let download = match self.drive.download(&link.url, ask_from, Some(ask_end)).await {
            Ok(download) => download,
            Err(DriveError::UrlExpired | DriveError::Failed(_)) => {
                self.links.drop_link(item_id);
                return Ok(None);
            }
            Err(error) => return Err(source_error(error)),
        };
        if download.etag.as_deref() != Some(listed.etag.as_str()) || download.size != Some(listed.size) || download.served_from != ask_from {
            tracing::info!("a range of {item_id} through its listed link was not of the listed version (ETag {:?}, size {:?}): asked by its metadata", download.etag, download.size);
            self.links.drop_link(item_id);
            return Ok(None);
        }
        match self.bind_listed(item_id, opened_as, &listed.etag, true) {
            ListedBinding::Bound => {}
            ListedBinding::Refused => return Err(SourceError::NotFound("the range's version changed".into())),
            ListedBinding::AskMetadata => return Ok(None),
        }
        let wanted = ask_end - ask_from;
        let mut bytes = Vec::new();
        download.stream.take(wanted).read_to_end(&mut bytes).await.map_err(|e| SourceError::Transient(e.to_string()))?;
        if bytes.len() as u64 != wanted {
            return Err(SourceError::Transient(format!("the answer ended after {} of {wanted} bytes", bytes.len())));
        }
        // The answer is of the listed version, which the row holds with the opened content:
        // it is that content, its cTag and hash.
        let version = Some(Version { ctag: ctag.clone().unwrap_or_default(), quick_xor: hash.as_deref().and_then(decode_base64) });
        let _cache_use = crate::panic::lock(&self.cache_use);
        if !self.drive.cached_content_allowed() { return Ok(None); }
        let piece = Piece { from: ask_from, size: Some(listed.size), mtime: SystemTime::UNIX_EPOCH, version, bytes };
        self.read_blocks.keep(item_id, opened_as, &piece);
        Ok(Some(piece.range(from, end)))
    }

    /// Whether a range answered with the `ETag` `etag` by the listed version may be
    /// answered, by what `opened_as` is bound to; `bind` binds an unbound content to it.
    fn bind_listed(&self, item_id: &str, opened_as: &ContentVersion, etag: &str, bind: bool) -> ListedBinding {
        let mut ranges = crate::panic::lock(&self.ranges);
        let bound = ranges.entry(item_id.to_owned()).or_default().entry(opened_as.clone()).or_default();
        if let Some(first) = &bound.listed {
            return if first == etag { ListedBinding::Bound } else { ListedBinding::Refused };
        }
        if bound.checked.as_ref().is_some_and(|(_, _, download_etag)| download_etag.as_deref() != Some(etag)) {
            return ListedBinding::AskMetadata;
        }
        if bind {
            bound.listed = Some(etag.to_owned());
        }
        ListedBinding::Bound
    }

    /// A range of `opened_as` by the metadata: the item's metadata, the range, and the
    /// metadata again. See the module's documentation.
    async fn read_checked(&self, item_id: &str, opened_as: &ContentVersion, from: u64, end: u64) -> Result<Fetched, SourceError> {
        let item = self.drive.item(item_id).await.map_err(source_error)?;
        if item.deleted.is_some() || item.file.is_none() {
            return Err(SourceError::NotFound(format!("{item_id} is not a file in OneDrive any more")));
        }
        let version = version_of(&item);
        if content_tag_of(&item).is_none() && item.e_tag.as_deref().filter(|tag| !tag.is_empty()).is_none() && version.as_ref().and_then(|v| v.quick_xor).is_none() {
            return Err(SourceError::Transient("OneDrive gave no version for a range read".into()));
        }
        let address = match &item.download_url {
            Some(url) => url.clone(),
            None => self.drive.content_url(item_id).await.map_err(source_error)?,
        };
        let (ask_from, ask_end) = covering(from, end, item.size.unwrap_or(opened_as.size()));
        let download = self.drive.download(&address, ask_from, Some(ask_end)).await.map_err(source_error)?;
        if download.served_from != ask_from {
            return Err(SourceError::Transient("the range started at another offset".into()));
        }
        // cTag is the content version; eTag also changes for a rename. Use
        // metadata eTag only when no cTag is available.
        let metadata_etag = item.e_tag.clone().filter(|tag| content_tag_of(&item).is_none() && !tag.is_empty());
        let identity = (version.clone(), metadata_etag, download.etag.clone());
        {
            let mut ranges = crate::panic::lock(&self.ranges);
            let bound = ranges.entry(item_id.to_owned()).or_default().entry(opened_as.clone()).or_default();
            let is_another = match (&bound.checked, &bound.listed) {
                (Some(first), _) => first != &identity,
                // Bound by the listed version: this answer must say the same `ETag`.
                (None, Some(listed)) => identity.2.as_deref() != Some(listed.as_str()),
                (None, None) => false,
            };
            if is_another {
                return Err(SourceError::NotFound("the range's version changed".into()));
            }
            bound.checked.get_or_insert(identity);
        }
        // Read the aligned piece, then check metadata again before publication or FUSE.
        let mut bytes = Vec::new();
        download.stream.take(ask_end.saturating_sub(ask_from)).read_to_end(&mut bytes).await
            .map_err(|e| SourceError::Transient(e.to_string()))?;
        if bytes.len() as u64 != ask_end.saturating_sub(ask_from) {
            return Err(SourceError::Transient("the range ended before the aligned piece was complete".into()));
        }
        let after = self.drive.item(item_id).await.map_err(source_error)?;
        if after.deleted.is_some() || after.file.is_none() || version_of(&after) != version
            || (content_tag_of(&item).is_none() && after.e_tag != item.e_tag) || after.size != item.size {
            return Err(SourceError::NotFound("the file changed while its range was read".into()));
        }
        // Keep a metadata URL only if this endpoint's ETag can prove the listed version.
        // Otherwise every later range would pay for a known-incompatible extra request.
        if let (Some(url), Some(etag)) = (item.download_url.clone(), item.e_tag.clone().filter(|tag| !tag.is_empty())) {
            let _cache_use = crate::panic::lock(&self.cache_use);
            if download.etag.as_deref() == Some(etag.as_str()) && self.drive.cached_content_allowed() {
                self.links.keep(item_id, Link { url, etag });
            }
        }
        let piece = Piece { from: ask_from, size: item.size, mtime: mtime_of(&item), version, bytes };
        let _cache_use = crate::panic::lock(&self.cache_use);
        // The caller compares these facts with the opened row too. Never remember a
        // changed answer as the old generation before that comparison can reject it.
        if self.drive.cached_content_allowed() && matches_opened(&piece, opened_as) {
            self.read_blocks.keep(item_id, opened_as, &piece);
        }
        Ok(piece.range(from, end))
    }
}

#[async_trait]
impl ContentSource for GraphSource {
    async fn read_range(&self, item_id: &str, opened_as: &ContentVersion, listed: Option<&ListedContent>, from: u64, end: u64) -> Result<Fetched, SourceError> {
        if let Some(answer) = self.cached_range(item_id, opened_as, from, end) { return Ok(answer); }
        if !self.drive.cached_content_allowed() { return self.read_checked(item_id, opened_as, from, end).await; }
        if let Some(listed) = listed {
            let prepared = self.listed_read(item_id, opened_as, listed, from, end);
            prepared.read.clone().await?;
            if let Some(answer) = self.cached_range(item_id, opened_as, from, end) { return Ok(answer); }
        }
        self.read_checked(item_id, opened_as, from, end).await
    }

    fn cached_range(&self, item_id: &str, opened_as: &ContentVersion, from: u64, end: u64) -> Option<Fetched> {
        let _cache_use = crate::panic::lock(&self.cache_use);
        if !self.drive.cached_content_allowed() {
            self.links.clear();
            self.read_blocks.clear();
            return None;
        }
        self.read_blocks.find(item_id, opened_as, from, end)
    }

    async fn prepared_range(&self, item_id: &str, opened_as: &ContentVersion, from: u64, end: u64) -> Option<Fetched> {
        if let Some(answer) = self.cached_range(item_id, opened_as, from, end) { return Some(answer); }
        let prepared = crate::panic::lock(&self.pending).get(&Self::prepared_key(item_id, opened_as, from, end)).and_then(std::sync::Weak::upgrade)?;
        // A real read keeps polling the shared request if the scan is cancelled or
        // replaced. Only speculative owners dropping cannot force a foreground retry.
        let _ = prepared.read.clone().await;
        self.cached_range(item_id, opened_as, from, end)
    }

    async fn read_ahead(&self, item_id: &str, opened_as: &ContentVersion, listed: &ListedContent) {
        if !self.drive.cached_content_allowed() || self.drive.pool().throughput().retry_after > 0 { return; }
        let end = opened_as.size().min(read_blocks::BLOCK);
        if end == 0 || self.cached_range(item_id, opened_as, 0, end).is_some() { return; }
        let prepared = self.listed_read(item_id, opened_as, listed, 0, end);
        match prepared.read.clone().await {
            Ok(true) => tracing::debug!("read ahead of {item_id}: first {end} bytes retained"),
            Ok(false) => {},
            Err(error) => tracing::debug!("read ahead of {item_id} stopped: {error}"),
        }
    }

    fn forget(&self, item_id: &str) {
        crate::panic::lock(&self.ranges).remove(item_id);
        self.read_blocks.forget(item_id);
        self.links.drop_link(item_id);
    }

    async fn fetch(&self, item_id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError> {
        let mut fresh_link = false;
        loop {
            let item = self.drive.item(item_id).await.map_err(source_error)?;
            if item.deleted.is_some() || item.file.is_none() {
                return Err(SourceError::NotFound(format!("{item_id} is not a file in OneDrive any more")));
            }
            let version = version_of(&item);
            let url = match &item.download_url {
                Some(url) => url.clone(),
                None => self.drive.content_url(item_id).await.map_err(source_error)?,
            };
            match self.drive.download(&url, from, end).await {
                Ok(download) => {
                    return Ok(Fetched {
                        served_from: download.served_from,
                        // As OneDrive said it: a size it left out is not a size of 0.
                        size: item.size,
                        mtime: mtime_of(&item),
                        version,
                        stream: download.stream,
                    })
                }
                // A download URL lives about an hour: ask for a fresh one, once.
                Err(DriveError::UrlExpired) if !fresh_link => fresh_link = true,
                Err(e) => return Err(source_error(e)),
            }
        }
    }
}

/// A missing item — and a signed-out account, which no retry can fix — ends
/// the download at once; everything else is worth another try. A throttle (429, 503) is told
/// apart from the rest of what passes: the drive has waited it out already as far as it
/// does, and told the pool the time OneDrive named, so none is given here.
fn source_error(error: DriveError) -> SourceError {
    let is_throttled = matches!(error.status().map(|status| status.code()), Some(429 | 503));
    match error {
        DriveError::NotFound => SourceError::NotFound(error.to_string()),
        DriveError::SignedOut => SourceError::SignedOut(error.to_string()),
        other if is_throttled => SourceError::Throttled { detail: other.to_string(), retry_after: None },
        other => SourceError::Transient(other.to_string()),
    }
}

fn mtime_of(item: &DriveItem) -> SystemTime {
    let seconds = item.mtime();
    if seconds <= 0 {
        return SystemTime::UNIX_EPOCH;
    }
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds as u64)
}


#[cfg(test)]
mod tests;

/// Keep a supplied hash even when Graph omits the cTag. An empty cTag means absent;
/// checkpoints then still require the full version and hash to agree.
fn version_of(item: &DriveItem) -> Option<Version> {
    let quick_xor = item.quick_xor_hash().and_then(decode_base64);
    Some(Version {
        ctag: content_tag_of(item).unwrap_or_default().to_owned(), quick_xor,
    })
}

fn content_tag_of(item: &DriveItem) -> Option<&str> {
    item.c_tag.as_deref().filter(|tag| !tag.is_empty())
}

/// Cache only facts compatible with the opened row, using the range fetcher's same
/// cTag-first comparison (and the hash when both sides supply one).
fn matches_opened(piece: &Piece, opened: &ContentVersion) -> bool {
    let ContentVersion::Hashed { ctag, hash, .. } = opened else { return false };
    if let (Some(opened), Some(answered)) = (hash.as_deref().and_then(decode_base64), piece.version.as_ref().and_then(|v| v.quick_xor)) {
        if opened != answered { return false; }
    }
    if let (Some(opened), Some(answered)) = (ctag, piece.version.as_ref().filter(|v| !v.ctag.is_empty())) {
        return opened == &answered.ctag;
    }
    let mtime = piece.mtime.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    !piece.size.is_some_and(|size| size != opened.size()) && (mtime == 0 || mtime == opened.mtime().0)
}
