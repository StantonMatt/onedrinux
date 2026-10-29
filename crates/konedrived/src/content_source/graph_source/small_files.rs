//! The whole bytes of small files a program reads as ranges, held a short while: an image
//! viewer that reads a file's header and then the rest asks OneDrive once
//! (`GraphSource::read_range`).
//!
//! **State owned:** by item id and the content a program opened, the bytes of that content
//! and what was proven of them. In memory only: no download, no download state, no emblem
//! and nothing on disk changes. Bounded by [`HELD_AT_MOST`] bytes, served for [`HELD_FOR`];
//! expired bytes are freed on lookup or insertion. Forget or token invalidation drops them.

// test: small_files_prune_expired_bytes_on_find_and_keep

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use konedrive_tree::ContentVersion;

use crate::content_source::source::{Fetched, Version};

/// A file up to this size is read whole at its first range.
pub(super) const SMALL_FILE: u64 = 256 * 1024;

/// The bytes held at most: 64 files of [`SMALL_FILE`]. The oldest goes first.
const HELD_AT_MOST: usize = 16 * 1024 * 1024;

/// How long a file is held after it was read: a program reads its ranges of a small file
/// within moments.
const HELD_FOR: Duration = Duration::from_secs(5 * 60);

/// A small file's bytes, with the size and version proven of them.
pub(super) struct WholeFile {
    pub(super) size: Option<u64>,
    pub(super) mtime: SystemTime,
    pub(super) version: Option<Version>,
    pub(super) bytes: Vec<u8>,
}

impl WholeFile {
    /// Bytes `from`..`end` of it, as an answer to a range read.
    pub(super) fn range(&self, from: u64, end: u64) -> Fetched {
        let length = self.bytes.len() as u64;
        let (start, stop) = (from.min(length) as usize, end.min(length) as usize);
        Fetched {
            served_from: from,
            size: self.size,
            mtime: self.mtime,
            version: self.version.clone(),
            stream: Box::new(std::io::Cursor::new(self.bytes[start..stop.max(start)].to_vec())),
        }
    }
}

struct Held {
    item_id: String,
    opened_as: ContentVersion,
    since: Instant,
    file: Arc<WholeFile>,
}

/// The small files of one source.
#[derive(Default)]
pub(super) struct SmallFiles {
    held: Mutex<VecDeque<Held>>,
}

impl SmallFiles {
    /// The bytes of `item_id` as opened as `opened_as`, while they are held.
    pub(super) fn find(&self, item_id: &str, opened_as: &ContentVersion) -> Option<Arc<WholeFile>> {
        let mut held = crate::panic::lock(&self.held);
        held.retain(|held| held.since.elapsed() < HELD_FOR);
        held.iter().find(|held| held.item_id == item_id && &held.opened_as == opened_as)
            .map(|held| Arc::clone(&held.file))
    }

    /// `file` is the content `opened_as` of `item_id`: held, and the oldest files let go
    /// while more than [`HELD_AT_MOST`] bytes are held.
    pub(super) fn keep(&self, item_id: &str, opened_as: &ContentVersion, file: WholeFile) -> Arc<WholeFile> {
        let file = Arc::new(file);
        let mut held = crate::panic::lock(&self.held);
        held.retain(|other| other.since.elapsed() < HELD_FOR && !(other.item_id == item_id && &other.opened_as == opened_as));
        held.push_back(Held { item_id: item_id.to_owned(), opened_as: opened_as.clone(), since: Instant::now(), file: Arc::clone(&file) });
        let mut total: usize = held.iter().map(|other| other.file.bytes.len()).sum();
        while total > HELD_AT_MOST {
            let Some(oldest) = held.pop_front() else { break };
            total -= oldest.file.bytes.len();
        }
        file
    }

    /// Discard all cached bytes on authentication invalidation.
    pub(super) fn clear(&self) {
        crate::panic::lock(&self.held).clear();
    }

    /// Nothing of `item_id` is held any more.
    pub(super) fn forget(&self, item_id: &str) {
        crate::panic::lock(&self.held).retain(|held| held.item_id != item_id);
    }
}

#[cfg(test)]
mod tests;
