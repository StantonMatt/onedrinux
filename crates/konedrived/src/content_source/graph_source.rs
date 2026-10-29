//! The content source of a folder that shows OneDrive: each fetch asks Graph
//! for the item's metadata afresh — the cTag
//! and quickXorHash the download checks the bytes against, and a download URL that
//! has not expired — and streams from the offset asked for, to the end of
//! the file or to the end of the piece asked for.

// test: a_hash_is_kept_without_a_ctag_and_hashless_graph_is_still_a_remote_version
// test: ranges_refuse_a_changed_metadata_version_or_download_etag
// test: a_range_that_changes_during_its_body_is_never_answered
// test: a_new_content_generation_does_not_inherit_an_earlier_ranges_version

use std::time::{Duration, SystemTime};

use tokio::io::AsyncReadExt;

use async_trait::async_trait;

use super::source::{ContentSource, Fetched, SourceError, Version};
use konedrive_graph::drive::{DriveClient, DriveError, DriveItem};
use konedrive_graph::quickxor::decode_base64;

pub(crate) struct GraphSource {
    drive: DriveClient,
    /// The first range's metadata and download response identity. An expired URL
    /// never lets a later range of the same opened content silently switch versions.
    ranges: std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<konedrive_tree::ContentVersion, (Option<Version>, Option<String>, Option<String>)>>>,
}

impl GraphSource {
    pub fn new(drive: DriveClient) -> Self {
        Self { drive, ranges: Default::default() }
    }
}

#[async_trait]
impl ContentSource for GraphSource {
    async fn read_range(&self, item_id: &str, opened_as: &konedrive_tree::ContentVersion, from: u64, end: u64) -> Result<Fetched, SourceError> {
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
            let generations = ranges.entry(item_id.to_owned()).or_default();
            match generations.get(opened_as) {
                Some(first) if first != &identity => return Err(SourceError::NotFound("the range's version changed".into())),
                Some(_) => {}
                None => { generations.insert(opened_as.clone(), identity); }
            }
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
        Ok(Fetched { served_from: download.served_from, size: item.size, mtime: mtime_of(&item),
            version, stream: Box::new(std::io::Cursor::new(bytes)) })
    }

    fn forget(&self, item_id: &str) {
        crate::panic::lock(&self.ranges).remove(item_id);
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
