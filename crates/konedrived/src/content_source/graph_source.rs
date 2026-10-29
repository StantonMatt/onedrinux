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
//!   [`small_files::SMALL_FILE`] is asked whole, and its other ranges come from memory.
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

mod links;
mod small_files;

use std::time::{Duration, SystemTime};
use std::sync::Arc;

use tokio::io::AsyncReadExt;

use async_trait::async_trait;

use super::source::{ContentSource, Fetched, ListedContent, SourceError, Version};
use konedrive_graph::drive::{DriveClient, DriveError, DriveItem};
use konedrive_graph::quickxor::decode_base64;
use konedrive_tree::ContentVersion;
use links::{DownloadLinks, Link};
use small_files::{SmallFiles, WholeFile, SMALL_FILE};

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

pub(crate) struct GraphSource {
    drive: DriveClient,
    /// By item and opened content, the version its ranges are bound to. An expired URL
    /// never lets a later range of the same opened content silently switch versions.
    ranges: std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<ContentVersion, Bound>>>,
    /// Serializes cache publication with token invalidation; never held across await.
    cache_use: Arc<std::sync::Mutex<()>>,
    links: Arc<DownloadLinks>,
    small_files: Arc<SmallFiles>,
    /// Keeps the token source's weak invalidation callback registered until this source drops.
    _on_invalidate: Arc<konedrive_graph::token::InvalidateHook>,
}

impl GraphSource {
    pub fn new(drive: DriveClient) -> Self {
        let links = Arc::new(DownloadLinks::default());
        let small_files = Arc::new(SmallFiles::default());
        let cache_use = Arc::new(std::sync::Mutex::new(()));
        let clear_use = Arc::clone(&cache_use);
        let (clear_links, clear_files) = (Arc::clone(&links), Arc::clone(&small_files));
        let on_invalidate: Arc<konedrive_graph::token::InvalidateHook> = Arc::new(move || {
            let _cache_use = crate::panic::lock(&clear_use);
            clear_links.clear();
            clear_files.clear();
        });
        drive.on_token_invalidate(&on_invalidate);
        Self { drive, ranges: Default::default(), cache_use, links, small_files, _on_invalidate: on_invalidate }
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
        let is_whole = listed.size <= SMALL_FILE;
        let (ask_from, ask_end) = if is_whole { (0, listed.size) } else { (from, end) };
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
        if is_whole {
            let file = self.small_files.keep(item_id, opened_as, WholeFile { size: Some(listed.size), mtime: SystemTime::UNIX_EPOCH, version, bytes });
            return Ok(Some(file.range(from, end)));
        }
        Ok(Some(Fetched { served_from: from, size: Some(listed.size), mtime: SystemTime::UNIX_EPOCH, version, stream: Box::new(std::io::Cursor::new(bytes)) }))
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
        let download = self.drive.download(&address, from, Some(end)).await.map_err(source_error)?;
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
        // Keep only this requested range in memory, then check metadata again before
        // any of it reaches FUSE. No staging file or whole-file allocation is needed.
        let mut bytes = Vec::new();
        download.stream.take(end.saturating_sub(from)).read_to_end(&mut bytes).await
            .map_err(|e| SourceError::Transient(e.to_string()))?;
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
        Ok(Fetched { served_from: download.served_from, size: item.size, mtime: mtime_of(&item),
            version, stream: Box::new(std::io::Cursor::new(bytes)) })
    }
}

#[async_trait]
impl ContentSource for GraphSource {
    async fn read_range(&self, item_id: &str, opened_as: &ContentVersion, listed: Option<&ListedContent>, from: u64, end: u64) -> Result<Fetched, SourceError> {
        // No secret lookup or refresh on the fast path. Without a fresh token the
        // authenticated path reports SignedOut (or refreshes), as before.
        let allowed = {
            let _cache_use = crate::panic::lock(&self.cache_use);
            let allowed = self.drive.cached_content_allowed();
            if !allowed {
                self.links.clear();
                self.small_files.clear();
            } else if let Some(file) = self.small_files.find(item_id, opened_as) {
                return Ok(file.range(from, end));
            }
            allowed
        };
        if !allowed { return self.read_checked(item_id, opened_as, from, end).await; }
        if let Some(listed) = listed {
            if let Some(fetched) = self.read_listed(item_id, opened_as, listed, from, end).await? {
                return Ok(fetched);
            }
        }
        self.read_checked(item_id, opened_as, from, end).await
    }

    fn forget(&self, item_id: &str) {
        crate::panic::lock(&self.ranges).remove(item_id);
        self.small_files.forget(item_id);
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
