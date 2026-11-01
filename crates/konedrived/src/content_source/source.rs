//! Where a download gets its bytes from (`ContentSource`), and the download of a whole file
//! from one of those sources, in one stream or in parts.

use std::time::SystemTime;

use async_trait::async_trait;
use tokio::io::AsyncRead;

mod download;
mod guards;
pub(crate) mod parts;
mod target;

pub(crate) use download::download_staged;
pub(crate) use parts::{Share, Split};

#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum SourceError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Transient(String),
    /// The account is signed out (401): no try again helps until the sign-in.
    #[error("{0}")]
    SignedOut(String),
    /// OneDrive holds the account back (429, 503), with the time it named when the source
    /// was told one.
    #[error("{detail}")]
    Throttled { detail: String, retry_after: Option<std::time::Duration> },
}

/// An assembled stream needs its next network interval opened through `fetch`.
/// Carried by an `io::Error`, distinct from a short response or broken connection.
/// Download writers continue without spending a break only after making progress.
#[derive(Debug, thiserror::Error)]
#[error("open the next download interval")]
pub(crate) struct FetchNext;

impl FetchNext {
    pub(crate) fn requested(error: &std::io::Error) -> bool {
        error.get_ref().is_some_and(|inner| inner.is::<Self>())
    }
}

pub(crate) struct Fetched {
    /// The offset this stream actually starts at.
    ///
    /// A download asks for `from` and writes what comes back at `from`; a source
    /// that silently serves something else corrupts the file and there is no
    /// way to notice after the fact. This is not a theoretical
    /// implementor: the Graph source issues HTTP `Range` requests, and a
    /// server that answers `200` instead of `206` — which is always allowed —
    /// restarts the body at 0. Reporting the served offset is the only thing
    /// that makes that case detectable, so every implementation must set it
    /// to the offset of the first byte of `stream`, not to the offset it was
    /// asked for.
    pub(crate) served_from: u64,
    /// The item's size by now, when the source said one. `None` is "not said", never an
    /// empty file: OneDrive's metadata may leave the size out, and the answer to a range a
    /// program reads tells nothing of the item (`read_range`). Whoever reads it decides
    /// what an answer without a size is to them; a download refuses it
    /// (`guards::check_answer`).
    pub(crate) size: Option<u64>,
    pub(crate) mtime: SystemTime,
    /// `None` from a source that cannot say (`LocalDir`): nothing is then
    /// verified or checkpointed.
    pub(crate) version: Option<Version>,
    /// A whole-download assembly can yield [`FetchNext`] inside an I/O error after
    /// its current bytes, so the next interval retains `fetch`'s refusal handling.
    pub(crate) stream: Box<dyn AsyncRead + Send + Unpin>,
}

/// Which version of a file a source's bytes belong to, when it can say: the
/// cTag, and the quickXorHash the whole file must match.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Version {
    /// Empty when Graph supplied a hash without a cTag.
    pub(crate) ctag: String,
    pub(crate) quick_xor: Option<[u8; konedrive_graph::quickxor::LEN]>,
}

/// What the store's row says OneDrive listed of the content a program opened: the eTag
/// OneDrive gave with that very content, its size, and the folder OneDrive lists the file
/// in. Given with a range read only where the row's OneDrive version has the opened
/// content (`mount::item_read_origin`); a source may then take an answer that says this
/// eTag and this size for the opened content, without asking the item's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedContent {
    /// `Item::onedrive_etag`: it moves at any change of the content, and at a rename or
    /// a move too.
    pub(crate) etag: String,
    pub(crate) size: u64,
    /// The OneDrive id of the folder the file is in; `None` while the store cannot say.
    pub(crate) folder_id: Option<String>,
}

#[async_trait]
pub(crate) trait ContentSource: Send + Sync {
    /// A private whole-download source, when this source can assemble checked range
    /// bytes with network input. `received` charges only newly fetched network bytes;
    /// the caller still reserves the complete size and checks space for every write.
    /// Rejection must disable reuse for this download's subsequent fresh retry.
    fn download_source(&self, _item_id: &str, _received: std::sync::Arc<dyn Fn(u64) + Send + Sync>) -> Option<Box<dyn ContentSource>> { None }

    /// Bytes of `item_id` starting at `from`, plus the item's current size and mtime.
    ///
    /// `end` is where the bytes wanted stop — the offset of the first byte not
    /// wanted — or `None` for the rest of the file: one piece of a download
    /// in parts asks for its own range only. A source may still
    /// serve more than that; the download reads no further than `end`.
    ///
    /// A `from` at the end of the item or past it is answered, not refused: an empty stream
    /// with the item's size and version. A download that continues from a checkpoint learns
    /// the item's size by now only from this answer, and drops a checkpoint the item has
    /// become too short for (`guards::resume`); a source that failed such a fetch would
    /// have it broken off three times with the checkpoint kept. `DriveClient::download`
    /// answers a `416` this way, and a file read past its end is empty.
    async fn fetch(&self, item_id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError>;

    /// Bytes `from`..`end` of `item_id` for a program that reads the file, not for a download
    /// of it: a folder served through FUSE answers a read of a file that is not downloaded
    /// with them (`crate::mount`). A remote source must bind every range to one metadata version and check that
    /// version again after consuming its body, before returning any bytes. The answer
    /// carries the version for comparison with `opened_as`. Remembered range identities
    /// belong to that opened content generation, not to the item id for its lifetime. A source that
    /// asks for an address before it asks for the bytes may remember the address between
    /// reads ([`forget`](Self::forget)). `listed` is what the store says OneDrive listed of
    /// the opened content, when it says it ([`ListedContent`]): a source may prove an
    /// answer's version by it instead.
    async fn read_range(&self, item_id: &str, _opened_as: &konedrive_tree::ContentVersion, _listed: Option<&ListedContent>, from: u64, end: u64) -> Result<Fetched, SourceError> {
        self.fetch(item_id, from, Some(end)).await
    }

    /// A proven memory hit, without taking a transfer slot. Authentication and opened
    /// generation checks are the same as for `read_range`.
    fn cached_range(&self, _item_id: &str, _opened_as: &konedrive_tree::ContentVersion, _from: u64, _end: u64) -> Option<Fetched> { None }

    /// A held memory/disk piece, or one an existing request is preparing. Joining that
    /// work must start no network request, so a foreground caller needs no second
    /// transfer slot. `listed` carries current eTag facts when the mount has them.
    async fn prepared_range(&self, item_id: &str, opened_as: &konedrive_tree::ContentVersion, _listed: Option<&ListedContent>, from: u64, end: u64) -> Option<Fetched> {
        self.cached_range(item_id, opened_as, from, end)
    }

    /// Speculatively retain the first piece, only by listed version proof. Unsupported
    /// sources do nothing; this must never fall back to metadata or write to disk.
    async fn read_ahead(&self, _item_id: &str, _opened_as: &konedrive_tree::ContentVersion, _listed: &ListedContent) {}

    /// The item changed or is gone: whatever the source remembered of it is dropped.
    fn forget(&self, _item_id: &str) {}

    /// How far a download in parts has come as a whole: `done` bytes of the
    /// file's `size` are on disk. No one of its streams can tell, so the
    /// download says it here; a source that shows progress
    /// (`tracked::Tracked`) shows this one.
    fn progress(&self, _done: u64, _size: u64) {}

    /// The download does not trust what is on disk of the file any more — the hash of
    /// the bytes it wrote does not match, the file changed in OneDrive between two
    /// answers, or the checkpoint it was to go on from cannot be continued: nothing of
    /// it counts from this moment. Said before the download starts over or gives up
    /// (`guards::StartOver`); a source that tells readers how far the file is written
    /// takes that back here.
    fn reject_written(&self) {}
}
