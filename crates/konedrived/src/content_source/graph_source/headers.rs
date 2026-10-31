//! Foreground-only publication and lookup of persistent ranges, with their original facts
//! and range binding. Speculative reads use memory_range and never publish disk records.

// test: a_header_survives_memory_eviction_and_source_restart_without_graph_requests
// test: a_disk_header_of_another_content_misses_and_old_rows_are_deleted
// test: read_ahead_bytes_reach_disk_only_when_a_reader_asks_for_them
// test: disk_headers_restore_the_binding_before_an_uncached_range
// test: read_ahead_prefetches_past_persisted_heads_after_restart
// test: signed_out_or_revoked_accounts_never_serve_disk_headers
// test: a_rename_drops_a_stale_disk_binding_before_uncached_ranges_are_read
// test: disk_headers_compare_metadata_etags_separately_from_endpoint_bindings

use super::*;
use crate::config::header_cache::{Record, MAX_RANGE};

#[derive(serde::Serialize, serde::Deserialize)]
struct Facts {
    size: Option<u64>,
    mtime: SystemTime,
    version: Option<Version>,
    bound: Bound,
}

impl GraphSource {
    /// Forget/invalidation cannot publish an older in-flight fetch back into memory,
    /// where a later reader could otherwise persist it under a new cache epoch.
    pub(super) fn cache_epoch_is(&self, epoch: Option<u64>) -> bool {
        self.headers.as_ref().map(|headers| headers.epoch()) == epoch
    }

    /// Memory inspection for speculative work; no persistent publication.
    pub(super) fn memory_range(
        &self,
        item: &str,
        opened: &ContentVersion,
        from: u64,
        end: u64,
    ) -> Option<Fetched> {
        let _cache_use = crate::panic::lock(&self.cache_use);
        if !self.drive.cached_content_allowed() {
            self.links.clear();
            self.read_blocks.clear();
            return None;
        }
        self.read_blocks
            .find_piece(item, opened, from, end)
            .map(Piece::into_fetched)
    }

    pub(super) fn reader_memory(
        &self,
        item: &str,
        opened: &ContentVersion,
        from: u64,
        end: u64,
        epoch: Option<u64>,
    ) -> Option<Fetched> {
        let _cache_use = crate::panic::lock(&self.cache_use);
        if !self.drive.cached_content_allowed() {
            self.links.clear();
            self.read_blocks.clear();
            return None;
        }
        let piece = self.read_blocks.find_piece(item, opened, from, end)?;
        self.remember_header(item, opened, &piece, from, end, epoch);
        Some(piece.into_fetched())
    }

    /// Called with cache_use held, only for an answer to a real reader. Capturing just
    /// the requested slice prevents whole fetched blocks from becoming persistent.
    pub(super) fn remember_header(
        &self,
        item: &str,
        opened: &ContentVersion,
        piece: &Piece,
        from: u64,
        end: u64,
        epoch: Option<u64>,
    ) {
        let (Some(headers), Some(epoch)) = (&self.headers, epoch) else {
            return;
        };
        if headers.epoch() != epoch
            || end <= from
            || end - from > MAX_RANGE
            || !self.drive.cached_content_allowed()
            || !matches_opened(piece, opened)
            || from < piece.from
            || end > piece.from + piece.bytes.len() as u64
        {
            return;
        }
        let bound = crate::panic::lock(&self.ranges)
            .get(item)
            .and_then(|versions| versions.get(opened))
            .cloned();
        let Some(bound) = bound else { return };
        let facts = Facts {
            size: piece.size,
            mtime: piece.mtime,
            version: piece.version.clone(),
            bound,
        };
        let (Ok(version), Ok(facts)) =
            (serde_json::to_string(opened), serde_json::to_string(&facts))
        else {
            return;
        };
        let bytes = piece.bytes[(from - piece.from) as usize..(end - piece.from) as usize].to_vec();
        headers.keep(epoch, item, version, Record { from, bytes, facts });
    }

    pub(super) async fn disk_range(
        &self,
        item: &str,
        opened: &ContentVersion,
        listed: Option<&ListedContent>,
        from: u64,
        end: u64,
        epoch: Option<u64>,
    ) -> Option<Fetched> {
        let (Some(headers), Some(epoch)) = (&self.headers, epoch) else {
            return None;
        };
        if !self.drive.cached_content_allowed() {
            return None;
        }
        let version = serde_json::to_string(opened).ok()?;
        let record = headers.find(epoch, item, version, from, end).await?;
        let facts: Facts = match serde_json::from_str(&record.facts) {
            Ok(facts) => facts,
            Err(_) => {
                headers.forget(item);
                return None;
            }
        };
        if listed
            .filter(|listed| !listed.etag.is_empty())
            .is_some_and(|listed| {
                facts.bound.metadata_etag.as_deref() != Some(listed.etag.as_str())
            })
        {
            // A rename may preserve ContentVersion but move its eTag. Re-fetch under
            // current listed facts instead of restoring an obsolete endpoint binding
            // that would refuse every later uncached range after a daemon restart.
            headers.forget(item);
            return None;
        }
        let piece = Piece {
            from: record.from,
            bytes: record.bytes,
            size: facts.size,
            mtime: facts.mtime,
            version: facts.version,
        };
        let _cache_use = crate::panic::lock(&self.cache_use);
        if headers.epoch() != epoch
            || !self.drive.cached_content_allowed()
            || !matches_opened(&piece, opened)
        {
            return None;
        }
        let mut ranges = crate::panic::lock(&self.ranges);
        let bound = ranges
            .entry(item.into())
            .or_default()
            .entry(opened.clone())
            .or_default();
        // Restore only compatible identities, including the endpoint ETag. A disk
        // hit must not let a later uncached range bind to a different version.
        if bound
            .checked
            .as_ref()
            .zip(facts.bound.checked.as_ref())
            .is_some_and(|(a, b)| a != b)
            || bound
                .listed
                .as_ref()
                .zip(facts.bound.listed.as_ref())
                .is_some_and(|(a, b)| a != b)
            || bound
                .checked
                .as_ref()
                .zip(facts.bound.listed.as_ref())
                .is_some_and(|(a, b)| a.2.as_ref() != Some(b))
            || facts
                .bound
                .checked
                .as_ref()
                .zip(bound.listed.as_ref())
                .is_some_and(|(a, b)| a.2.as_ref() != Some(b))
        {
            return None;
        }
        if bound.checked.is_none() {
            bound.checked = facts.bound.checked;
        }
        if bound.listed.is_none() {
            bound.listed = facts.bound.listed;
        }
        if bound.metadata_etag.is_none() {
            bound.metadata_etag = facts.bound.metadata_etag;
        }
        drop(ranges);
        // Read-ahead may have published a larger block while SQLite was looking up
        // this head. Keeping the head at the same offset would replace that block.
        if self
            .read_blocks
            .find_piece(
                item,
                opened,
                piece.from,
                piece.from + piece.bytes.len() as u64,
            )
            .is_none()
        {
            self.read_blocks.keep(item, opened, &piece);
        }
        tracing::debug!("header cache hit for {item} at {from}");
        Some(piece.range(from, end))
    }
}
