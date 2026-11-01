//! Version-checked bytes of range reads, shared by small whole files and aligned blocks.
//!
//! **State owned:** pieces by item and opened content, at most [`HELD_AT_MOST`] bytes,
//! oldest first and kept for [`HELD_FOR`]. A lookup joins adjacent pieces only of that
//! opened generation. Forget and authentication invalidation discard them all.

// test: read_blocks_prune_expired_bytes_on_find_and_keep
// test: read_blocks_share_a_bound_and_evict_the_oldest_without_refreshing_on_find
// test: adjacent_blocks_answer_a_crossing_range_only_of_the_same_generation
// test: block_ranges_are_aligned_clipped_and_do_not_overflow

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use konedrive_tree::ContentVersion;
use crate::content_source::source::{Fetched, Version};

/// Files up to this size retain their whole-file read policy.
pub(super) const SMALL_FILE: u64 = 256 * 1024;
/// Larger files are read in aligned pieces of this size.
pub(super) const BLOCK: u64 = 1024 * 1024;
const HELD_AT_MOST: usize = 64 * 1024 * 1024;
const HELD_FOR: Duration = Duration::from_secs(5 * 60);

/// The whole aligned interval containing a read, clipped to the known file size.
pub(super) fn covering(from: u64, end: u64, size: u64) -> (u64, u64) {
    if size <= SMALL_FILE { return (0, size); }
    let start = (from / BLOCK * BLOCK).min(size);
    let stop = end.div_ceil(BLOCK).saturating_mul(BLOCK).min(size);
    (start, stop.max(start))
}

/// Bytes whose version has been checked, beginning at `from` in the file.
pub(super) struct Piece {
    pub(super) from: u64,
    pub(super) size: Option<u64>,
    pub(super) mtime: SystemTime,
    pub(super) version: Option<Version>,
    pub(super) bytes: Vec<u8>,
}

impl Piece {
    /// Consume a piece already clipped to exactly the reader's interval.
    pub(super) fn into_fetched(self) -> Fetched {
        Fetched { served_from: self.from, size: self.size, mtime: self.mtime, version: self.version,
            stream: Box::new(std::io::Cursor::new(self.bytes)) }
    }

    /// An answer preserving its version facts, with only the requested bytes.
    pub(super) fn range(&self, from: u64, end: u64) -> Fetched {
        let start = from.saturating_sub(self.from).min(self.bytes.len() as u64) as usize;
        let stop = end.saturating_sub(self.from).min(self.bytes.len() as u64) as usize;
        Fetched { served_from: from, size: self.size, mtime: self.mtime, version: self.version.clone(),
            stream: Box::new(std::io::Cursor::new(self.bytes[start..stop.max(start)].to_vec())) }
    }
}

/// Fresh metadata may add facts omitted by a listing. Every cached fact must
/// agree; the bound metadata eTag and size are checked separately by the caller.
pub(super) fn matches_fresh(cached: &Option<Version>, fresh: &Option<Version>) -> bool {
    cached.as_ref().zip(fresh.as_ref()).is_some_and(|(cached, fresh)| {
        (cached.ctag.is_empty() || cached.ctag == fresh.ctag)
            && cached.quick_xor.is_none_or(|hash| fresh.quick_xor == Some(hash))
    })
}

struct Held {
    item_id: String,
    opened_as: ContentVersion,
    since: Instant,
    piece: Piece,
}

#[derive(Default)]
pub(super) struct ReadBlocks {
    held: Mutex<VecDeque<Held>>,
}

impl ReadBlocks {
    /// An answer only when every requested byte is held for this opened generation.
    pub(super) fn find_piece(&self, item_id: &str, opened_as: &ContentVersion, from: u64, end: u64) -> Option<Piece> {
        let mut held = crate::panic::lock(&self.held);
        held.retain(|held| held.since.elapsed() < HELD_FOR);
        let mut at = from;
        let mut answer: Option<Piece> = None;
        while at < end {
            let piece = &held.iter().find(|held| held.item_id == item_id && &held.opened_as == opened_as
                && held.piece.from <= at && at < held.piece.from + held.piece.bytes.len() as u64)?.piece;
            let stop = end.min(piece.from + piece.bytes.len() as u64);
            let answer = answer.get_or_insert_with(|| Piece { from, size: piece.size, mtime: piece.mtime, version: piece.version.clone(), bytes: Vec::new() });
            // Publication already binds every piece to one version; keep the facts equal too.
            if answer.version != piece.version || answer.size != piece.size { return None; }
            answer.bytes.extend_from_slice(&piece.bytes[(at - piece.from) as usize..(stop - piece.from) as usize]);
            at = stop;
        }
        answer
    }

    #[cfg(test)]
    pub(super) fn find(&self, item_id: &str, opened_as: &ContentVersion, from: u64, end: u64) -> Option<Fetched> {
        self.find_piece(item_id, opened_as, from, end).map(Piece::into_fetched)
    }

    /// Trim complete cached blocks from the edges of an aligned network miss.
    /// Keep one request for any remaining middle, even when it has cached holes.
    pub(super) fn missing_edges(&self, item_id: &str, opened_as: &ContentVersion, mut from: u64, mut end: u64) -> (u64, u64) {
        let mut held = crate::panic::lock(&self.held);
        held.retain(|held| held.since.elapsed() < HELD_FOR);
        let covers = |from, end| held.iter().any(|held| {
            held.item_id == item_id && &held.opened_as == opened_as
                && held.piece.from <= from && held.piece.from + held.piece.bytes.len() as u64 >= end
        });
        while from < end {
            let next = from.saturating_add(BLOCK).min(end);
            if !covers(from, next) { break; }
            from = next;
        }
        while from < end {
            let previous = ((end - 1) / BLOCK * BLOCK).max(from);
            if !covers(previous, end) { break; }
            end = previous;
        }
        (from, end)
    }

    /// Split an aligned answer into separate blocks so overlapping fetches share one
    /// memory budget. A repeated block replaces itself; lookup never extends its life.
    pub(super) fn keep(&self, item_id: &str, opened_as: &ContentVersion, piece: &Piece) {
        let mut held = crate::panic::lock(&self.held);
        held.retain(|other| other.since.elapsed() < HELD_FOR);
        for (n, bytes) in piece.bytes.chunks(BLOCK as usize).enumerate() {
            let from = piece.from + n as u64 * BLOCK;
            held.retain(|other| !(other.item_id == item_id && &other.opened_as == opened_as && other.piece.from == from));
            held.push_back(Held { item_id: item_id.to_owned(), opened_as: opened_as.clone(), since: Instant::now(),
                piece: Piece { from, size: piece.size, mtime: piece.mtime, version: piece.version.clone(), bytes: bytes.to_vec() } });
        }
        let mut total: usize = held.iter().map(|held| held.piece.bytes.len()).sum();
        while total > HELD_AT_MOST {
            let Some(oldest) = held.pop_front() else { break };
            total -= oldest.piece.bytes.len();
        }
    }

    /// One reusable piece at `from`, or the start of the next held piece. Fresh
    /// metadata and the generation's binding have already selected `generations`.
    pub(super) fn download_piece(&self, item_id: &str, generations: &[ContentVersion], from: u64, end: u64, version: &Option<Version>) -> (Option<Piece>, u64) {
        let mut held = crate::panic::lock(&self.held);
        held.retain(|held| held.since.elapsed() < HELD_FOR);
        let matching = || held.iter().filter(|held| held.item_id == item_id && generations.contains(&held.opened_as)
            && matches_fresh(&held.piece.version, version) && held.piece.size == Some(held.opened_as.size()));
        if let Some(held) = matching().find(|held| held.piece.from <= from && from < held.piece.from + held.piece.bytes.len() as u64) {
            let piece = &held.piece;
            let stop = end.min(piece.from + piece.bytes.len() as u64);
            return (Some(Piece { from, size: piece.size, mtime: piece.mtime, version: piece.version.clone(),
                bytes: piece.bytes[(from - piece.from) as usize..(stop - piece.from) as usize].to_vec() }), stop);
        }
        (None, matching().filter(|held| held.piece.from > from).map(|held| held.piece.from).min().unwrap_or(end).min(end))
    }

    pub(super) fn clear(&self) { crate::panic::lock(&self.held).clear(); }
    pub(super) fn forget(&self, item_id: &str) { crate::panic::lock(&self.held).retain(|held| held.item_id != item_id); }
}

#[cfg(test)]
mod tests;
