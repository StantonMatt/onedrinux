//! In-memory range metadata: absent intervals never wait for a disk lookup, and covered
//! foreground reads only touch LRU metadata instead of publishing duplicate bytes.
// test: header_cache_clean_catalog_does_not_scan_for_uses
// test: header_cache_catalog_dirty_accounting_only_examines_replaced_rows
// test: header_cache_memory_hits_coalesce_lru_on_rows_until_the_writer_flushes
// test: header_cache_catalog_tracks_eviction_versions_and_forget
// test: header_cache_repeated_memory_hits_do_not_crowd_out_a_new_head
// test: header_cache_known_misses_and_pending_duplicates_never_wait_for_the_writer

use super::{LookupUse, Record, LIFETIME};
use std::collections::HashMap;

pub(super) type Key = (String, String);
pub(super) type Entries = HashMap<Key, Vec<Range>>;

pub(super) struct Range {
    pub(super) from: u64,
    pub(super) end: u64,
    pub(super) facts: String,
    pub(super) at: i64,
    pub(super) used: Option<i64>,
}

#[derive(Default)]
pub(super) struct Catalog {
    pub(super) epoch: u64,
    pub(super) ready: bool,
    pub(super) blocked: bool,
    pub(super) stored: Entries,
    pub(super) pending: Vec<(u64, Key, Range)>,
    pub(super) dirty: usize,
    last_use: Option<std::time::Instant>,
    #[cfg(test)]
    pub(super) rows_examined: usize,
}

impl Catalog {
    /// One coalesced timestamp on the existing row, never one queued write per hit.
    pub(super) fn touch(
        &mut self,
        epoch: u64,
        key: &Key,
        from: u64,
        end: u64,
        facts: Option<&str>,
        exact: Option<u64>,
    ) -> bool {
        if !self.ready || self.blocked || self.epoch != epoch {
            return false;
        }
        let Some(range) = self.stored.get_mut(key).and_then(|ranges| {
            ranges.iter_mut().find(|range| {
                range.from <= from
                    && range.end >= end
                    && exact.is_none_or(|offset| offset == range.from)
                    && facts.is_none_or(|facts| range.facts == facts)
            })
        }) else {
            return false;
        };
        self.last_use = Some(std::time::Instant::now());
        let at = super::database::now();
        if at <= range.at {
            return false;
        }
        let wake = self.dirty == 0;
        if range.used.is_none() {
            self.dirty += 1;
        }
        range.used = Some(at);
        wake
    }

    pub(super) fn quiet(&self) -> bool {
        self.last_use
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(50))
    }

    pub(super) fn take_uses(&mut self, epoch: u64, limit: usize) -> Vec<LookupUse> {
        let mut used = Vec::new();
        if self.blocked || self.epoch != epoch || self.dirty == 0 {
            return used;
        }
        for (key, ranges) in &mut self.stored {
            for range in ranges {
                if used.len() == limit {
                    break;
                }
                #[cfg(test)]
                {
                    self.rows_examined += 1;
                }
                if let Some(at) = range.used.take() {
                    used.push(LookupUse {
                        epoch,
                        item: key.0.clone(),
                        version: key.1.clone(),
                        from: range.from,
                        end: range.end,
                        at,
                    });
                }
            }
            if used.len() == limit {
                break;
            }
        }
        self.dirty -= used.len();
        used
    }

    pub(super) fn replace(&mut self, mut entries: Entries, partial: bool) {
        let mut dirty = if partial { self.dirty } else { 0 };
        for (key, ranges) in &mut entries {
            if let Some(old) = self.stored.get(key) {
                if partial {
                    dirty -= old
                        .iter()
                        .filter(|range| {
                            #[cfg(test)]
                            {
                                self.rows_examined += 1;
                            }
                            range.used.is_some()
                        })
                        .count();
                }
                for range in ranges.iter_mut() {
                    range.used = old
                        .iter()
                        .find(|other| other.from == range.from && other.facts == range.facts)
                        .and_then(|other| other.used)
                        .filter(|at| *at > range.at);
                }
            }
            dirty += ranges
                .iter()
                .filter(|range| {
                    #[cfg(test)]
                    {
                        self.rows_examined += 1;
                    }
                    range.used.is_some()
                })
                .count();
        }
        if partial {
            self.stored.extend(entries);
        } else {
            self.stored = entries;
        }
        self.dirty = dirty;
    }

    pub(super) fn covers(
        &self,
        epoch: u64,
        key: &Key,
        from: u64,
        end: u64,
        facts: Option<&str>,
    ) -> bool {
        self.ready
            && self.epoch == epoch
            && self.stored.get(key).is_some_and(|ranges| {
                ranges.iter().any(|range| {
                    range.from <= from
                        && range.end >= end
                        && range.at > super::database::now() - LIFETIME
                        && facts.is_none_or(|facts| facts == range.facts)
                })
            })
    }

    pub(super) fn queued(&self, epoch: u64, key: &Key, record: &Record) -> bool {
        self.pending.iter().any(|(queued, item, range)| {
            *queued == epoch
                && item == key
                && range.from <= record.from
                && range.end >= record.from + record.bytes.len() as u64
                && range.facts == record.facts
        })
    }
}
