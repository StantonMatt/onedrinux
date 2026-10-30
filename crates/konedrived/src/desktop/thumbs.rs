//! Thumbnails from OneDrive: Graph's own thumbnail of every file of a type a thumbnail is
//! made for — images, videos and documents (the store's `HAS_THUMBNAIL_TYPE`) — written
//! into the freedesktop thumbnail cache under the names KIO and Gwenview look for
//! (`docs/kio-behavior.md`), so that previews draw without opening — and so
//! downloading — the file. In the background, a few requests at once and none while
//! anything else has work for OneDrive ([`request_start`]): a thumbnail is a request to the
//! Graph API, and takes no slot of the account's transfer pool.
//!
//! Only `normal`, `large` and `x-large` (up to 512 px) are filled: one Graph
//! request per file (`c512x512`), scaled down locally to the smaller sizes.
//! `xx-large` (1024 px) is not filled (`docs/design/desktop.md` §8).
//!
//! Every answer that settles whether an item has a usable thumbnail is
//! recorded (the store's thumbnail marks), so the item is asked for again only once it
//! changes; only a passing trouble — no answer, `401`, `408`, `429`, 5xx — is
//! tried again at the next drain. A `406` at `c512x512` is asked
//! once more at Graph's named size `large` before it counts as a refusal. A thumbnail
//! that cannot be written into the cache records nothing and ends the drain: the next
//! one asks again.
//!
//! The cache itself is looked at in the first drain after the folder's sync started and
//! in the one the ten-minute timer starts ([`DrainCause`]): a written thumbnail of which
//! the cache lacks a size under either name is repaired from the other name locally. If
//! neither name has that size, its mark is forgotten ([`decide_cached_thumbnail`]), and
//! the same drain makes it again. The scan and repairs use batches of 200 marks.
//!
//! How many thumbnails are being downloaded now is `Transfers.ThumbnailDownloads`
//! (`TransferStatus::thumbnail_downloads`), written here as each request starts and ends.

// test: a_gwenview_uri_matches_qt_pretty_decoded
// test: spaces_and_unicode_get_both_cache_names_and_plain_names_get_one
// test: the_first_drain_upgrades_old_names_in_batches_without_graph_or_forgetting_marks
// test: either_names_missing_size_is_repaired_but_a_size_missing_from_both_is_fetched
// test: current_gwenview_pngs_are_left_alone_and_stale_ones_are_replaced_locally
// test: a_current_gwenview_png_is_kept_when_both_viewers_share_the_name
// test: a_failed_or_cancelled_local_repair_keeps_its_mark_without_graph
// test: renaming_a_file_writes_both_new_names_and_removal_does_not_fetch_it
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use md5::Digest;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub(crate) mod request_start;

use crate::folder::root::SyncRoot;
use crate::conditions::running::Running;
use crate::status::snapshot::SyncStateHandle;
use konedrive_graph::pool::Class;
use request_start::{decide_request_start, ListingProgress, RequestFacts, RequestStart, RequestWait, ASKED_AGAIN_AFTER};
use konedrive_graph::drive::{DriveClient, Thumbnail};
use konedrive_tree::{LocalItemId, Store, ThumbnailCandidate as Wanted};

/// How often the timer starts a drain, however often the nudges come: in case one was
/// missed, and to look into the cache.
const TIMER_PERIOD: Duration = Duration::from_secs(600);

/// The cache directories KIO consults, and the longest edge of each
/// (`docs/kio-behavior.md` §A). `xx-large` is deliberately absent: it would be a second
/// request per file (`docs/design/desktop.md` §8).
pub(crate) const SIZES: &[(&str, u32)] = &[("normal", 128), ("large", 256), ("x-large", 512)];
/// The one size asked of Graph; the smaller ones are scaled from it.
const GRAPH_SIZE: &str = "c512x512";
/// Asked instead when Graph answers [`GRAPH_SIZE`] with `406 Not Acceptable`,
/// as it does for some items: Graph's named size, up to 800 px, scaled down
/// the same way.
const FALLBACK_SIZE: &str = "large";
/// The decode limits a thumbnail's bytes are read under:
/// `DriveClient::thumbnail` already caps the body at 8 MiB, but a small body
/// can still decompress into a huge image (a decompression bomb), so the
/// decoder itself is capped too — comfortably above any real `c512x512`
/// thumbnail, far below what would hurt.
const MAX_DECODE_EDGE: u32 = 4096;
const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;

/// `file://` and the path, as KIO spells the URI it hashes (§A of the
/// measurements): every byte percent-encoded except the unreserved
/// characters, `/`, and the sub-delimiters a path segment may hold.
pub(crate) fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// For absolute Unix paths below the sync root, Gwenview 25.12's
/// `url.adjusted(QUrl::RemovePassword).url()`: PrettyDecoded
/// keeps spaces and valid Unicode, but escapes ASCII outside the path's allowed bytes.
/// Invalid UTF-8 paths have only the KIO name: Qt's replacement characters lose identity.
fn gwenview_uri(path: &Path) -> Option<String> {
    let mut out = String::from("file://");
    for ch in path.to_str()?.chars() {
        if !ch.is_ascii() || ch == ' ' || ch.is_ascii_alphanumeric() || "-._~/!$&'()*+,;=:@".contains(ch) {
            out.push(ch);
        } else {
            out.push_str(&format!("%{:02X}", ch as u32));
        }
    }
    Some(out)
}

/// One name when the spellings agree; otherwise KIO first, Gwenview second.
fn thumbnail_uris(file: &Path) -> Vec<String> {
    let mut uris = vec![file_uri(file)];
    if let Some(uri) = gwenview_uri(file).filter(|uri| uri != &uris[0]) {
        uris.push(uri);
    }
    uris
}

pub(crate) struct ThumbnailFiller {
    drive: DriveClient,
    store: Store,
    root: SyncRoot,
    cache: PathBuf,
    /// Whether thumbnails are asked for now: the account's setting, and its pause
    /// (`conditions::running`).
    running: Arc<Running>,
    /// Where the count of the thumbnails being downloaded is said.
    state: SyncStateHandle,
}

/// What one `run_once` did: `taken` is how many candidates it
/// looked at, `written` how many thumbnails it actually wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RunOutcome {
    pub(crate) taken: usize,
    pub(crate) written: usize,
}

/// What started a drain, which decides whether it looks into the cache before it reads
/// its candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainCause {
    /// The first drain since the folder's sync started, whatever woke it: the cache may
    /// have been emptied while the folder's sync did not run. Looks into the cache.
    SyncStarted,
    /// The ten-minute timer ([`TIMER_PERIOD`]). Looks into the cache.
    TimerFired,
    /// A listing ended or steps changed the folder (the nudge), or thumbnails were turned
    /// on again. These come often while files change: they go by the marks alone.
    Nudged,
}

/// What came of asking for one candidate's thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateOutcome {
    /// The thumbnail was written into the cache, and recorded so.
    Written,
    /// OneDrive has no usable thumbnail of the content: none, a refusal, or bytes that are
    /// no image. Recorded, so that it is not asked again until the content or the path
    /// changes.
    NoThumbnail,
    /// Nothing settled: a passing trouble, or the stop came first. Nothing is recorded,
    /// and the next drain asks again.
    NotSettled,
    /// The cache cannot be written. Nothing is recorded, and the drain stops: no further
    /// request starts in it.
    CacheUnwritable,
}

/// Whether the cache holds a file's thumbnail whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachedThumbnail {
    /// The current PNG of every size ([`SIZES`]) is there under every URI spelling.
    Whole,
    /// A size is missing under both names and could not be repaired locally: the
    /// thumbnail is made again, since a viewer makes a missing size by reading the file.
    Missing,
}

/// What making a thumbnail from the bytes Graph gave back can fail at, once
/// there are bytes to work with at all.
#[derive(Debug)]
enum FillError {
    /// The bytes are not a usable image: corrupt, an unsupported format, or
    /// over the decode limits ([`MAX_DECODE_EDGE`]/[`MAX_DECODE_ALLOC`]).
    /// Recorded like a 404 — asking Graph again would only get the same
    /// bytes back.
    Undecodable(String),
    /// A local problem: disk, permissions, a vanished cache directory. Not recorded: the
    /// drain stops, and the next asks again ([`CandidateOutcome::CacheUnwritable`]).
    Io(String),
}

impl ThumbnailFiller {
    pub fn new(drive: DriveClient, store: Store, root: SyncRoot, cache: PathBuf, running: Arc<Running>, state: SyncStateHandle) -> Self {
        Self { drive, store, root, cache, running, state }
    }

    /// What [`decide_request_start`] is asked with, read now: the listing from the store,
    /// the throttle, the opens and what waits from the account's transfer pool. A store
    /// that cannot be read says the listing has not ended: no request starts.
    async fn read_request_facts(&self, requests_under_way: usize) -> RequestFacts {
        let read = self.store.call(|store| Ok((store.delta_link()?.is_some(), store.item_store().find_listing_under_way()?.is_some()))).await;
        let listing = match read {
            Ok((true, false)) => ListingProgress::Ended,
            Ok((true, true)) => ListingProgress::UnderWay,
            Ok((false, _)) => ListingProgress::FullNotEnded,
            Err(e) => {
                tracing::warn!("cannot read how far the listing is, no thumbnail is asked for: {e}");
                ListingProgress::FullNotEnded
            }
        };
        let pool = self.drive.pool();
        RequestFacts {
            listing,
            throttle_left: Duration::from_secs(u64::from(pool.throughput().retry_after)),
            is_file_being_opened: pool.held(Class::Open) > 0,
            is_transfer_waiting: pool.waiting(),
            requests_under_way,
        }
    }

    /// Makes up to `limit` missing thumbnails, a request starting whenever
    /// [`decide_request_start`] lets one.
    #[cfg(test)]
    pub(crate) async fn run_once(&self, cancel: &CancellationToken, limit: usize) -> RunOutcome {
        self.run_from(cancel, limit, None).await.0
    }

    /// `run_once` over the candidates after the item `after`
    /// (`ItemReader::list_thumbnail_candidates`, by local item id);
    /// with the item the next batch goes on from, `None` once every candidate
    /// has been looked at, and once the cache could not be written: what was started then
    /// ends, and no further request starts.
    async fn run_from(&self, cancel: &CancellationToken, limit: usize, after: Option<LocalItemId>) -> (RunOutcome, Option<LocalItemId>) {
        // Asked before the candidates are read: while a listing goes on the plan's task
        // nudges the filler after every round, and each drain would read them for nothing.
        if matches!(decide_request_start(&self.read_request_facts(0).await), RequestStart::Wait(RequestWait::FullListingNotEnded | RequestWait::ListingUnderWay)) {
            return (RunOutcome::default(), None);
        }
        let candidates = match self.store.read_items(move |reader| reader.list_thumbnail_candidates(after, limit)).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!("cannot list the thumbnails to make: {e}");
                return (RunOutcome::default(), None);
            }
        };
        // A full batch may have more after it; a short one was the last.
        let mut next = candidates.last().map(|last| last.item.local_item_id).filter(|_| limit > 0 && candidates.len() >= limit);
        let taken = candidates.len();
        let mut running = tokio::task::JoinSet::new();
        let (mut written, mut is_cache_unwritable) = (0, false);
        let mut count = |done: Result<CandidateOutcome, tokio::task::JoinError>| {
            let outcome = done.unwrap_or(CandidateOutcome::NotSettled);
            written += usize::from(outcome == CandidateOutcome::Written);
            is_cache_unwritable |= outcome == CandidateOutcome::CacheUnwritable;
            is_cache_unwritable
        };
        for wanted in candidates {
            // Turned off or paused meanwhile: no more requests; what was not asked waits
            // for the next drain.
            if !self.running.thumbnails_go() {
                break;
            }
            // The wait for its turn, and the request — which can wait out Graph's
            // `Retry-After`, up to 300 s, four times — give way to a stop: the stop of the
            // folder's sync waits for this task.
            let is_started = loop {
                // What ended meanwhile is counted first: once the cache could not be
                // written, no further request starts in this drain.
                let mut is_stopped_by_cache = false;
                while let Some(done) = running.try_join_next() {
                    is_stopped_by_cache = count(done);
                    self.state.set_thumbnail_downloads(running.len());
                }
                if is_stopped_by_cache {
                    break false;
                }
                let wait = match decide_request_start(&self.read_request_facts(running.len()).await) {
                    RequestStart::Start => break true,
                    // The listing's end nudges the filler: the drain ends here, and what
                    // was not asked waits for the one the nudge starts.
                    RequestStart::Wait(RequestWait::FullListingNotEnded | RequestWait::ListingUnderWay) => break false,
                    RequestStart::Wait(RequestWait::Throttled { left }) => left,
                    RequestStart::Wait(RequestWait::FileBeingOpened | RequestWait::TransferWaiting | RequestWait::RequestsAtLimit) => ASKED_AGAIN_AFTER,
                };
                tokio::select! {
                    () = cancel.cancelled() => break false,
                    () = tokio::time::sleep(wait) => {}
                    Some(done) = running.join_next(), if !running.is_empty() => {
                        let is_stopped_by_cache = count(done);
                        self.state.set_thumbnail_downloads(running.len());
                        if is_stopped_by_cache {
                            break false;
                        }
                    }
                }
            };
            if !is_started {
                next = None;
                break;
            }
            let one = One { drive: self.drive.clone(), store: self.store.clone(), cache: self.cache.clone(), folder: self.root.path.clone() };
            let cancel = cancel.clone();
            running.spawn(async move {
                tokio::select! {
                    () = cancel.cancelled() => CandidateOutcome::NotSettled,
                    outcome = one.make(wanted) => outcome,
                }
            });
            self.state.set_thumbnail_downloads(running.len());
        }
        while let Some(done) = running.join_next().await {
            count(done);
            self.state.set_thumbnail_downloads(running.len());
        }
        if is_cache_unwritable {
            next = None;
        }
        (RunOutcome { taken, written }, next)
    }

    /// Batches of up to `limit`, each going on where the last stopped,
    /// until every candidate has been looked at once — a
    /// candidate that failed this time waits for the next drain — or
    /// cancellation, or the cache could not be written. Started by `cause`: the first drain
    /// after the folder's sync started and the timer's look into the cache first
    /// ([`forget_marks_of_missing_thumbnails`](Self::forget_marks_of_missing_thumbnails)),
    /// so that the thumbnails it lacks are among the candidates.
    async fn drain(&self, cancel: &CancellationToken, limit: usize, cause: DrainCause) -> RunOutcome {
        if matches!(cause, DrainCause::SyncStarted | DrainCause::TimerFired) {
            self.forget_marks_of_missing_thumbnails(cancel, limit).await;
        }
        let mut total = RunOutcome::default();
        let mut after = None;
        loop {
            let (outcome, next) = self.run_from(cancel, limit, after).await;
            total.taken += outcome.taken;
            total.written += outcome.written;
            if next.is_none() || cancel.is_cancelled() || !self.running.thumbnails_go() {
                break;
            }
            after = next;
        }
        total
    }

    /// Forgets the marks of the thumbnails the cache does not hold whole, so that the drain
    /// makes them again: the marks that stand of a thumbnail written
    /// (`ItemReader::list_thumbnail_marks`), `limit` at a time, each looked up in the cache
    /// ([`find_cached_sizes`], [`decide_cached_thumbnail`]), and those `Missing` forgotten
    /// in one store write per batch (`ItemStore::forget_thumbnail_marks`). A missing or
    /// stale name is first repaired from its current counterpart at the same size, keeping
    /// the mark without a Graph request. A cache that cannot be read or written ends the
    /// scan; cancellation is checked between marks as well as between bounded batches.
    async fn forget_marks_of_missing_thumbnails(&self, cancel: &CancellationToken, limit: usize) {
        let mut forgotten = 0;
        let mut after = None;
        while !cancel.is_cancelled() {
            let marks = match self.store.read_items(move |reader| {
                reader.list_thumbnail_marks(after, limit)?.into_iter().map(|mark| {
                    let content = reader.find_item(mark.item)?.and_then(|item| item.onedrive).and_then(|version| version.content);
                    Ok((mark, content))
                }).collect::<Result<Vec<_>, konedrive_tree::TreeError>>()
            }).await {
                Ok(marks) => marks,
                Err(e) => {
                    tracing::warn!("cannot list the thumbnails made, to look for them in the cache: {e}");
                    break;
                }
            };
            // A full batch may have more after it; a short one was the last.
            let next = marks.last().map(|(last, _)| last.item).filter(|_| limit > 0 && marks.len() >= limit);
            let (cache, folder) = (self.cache.clone(), self.root.path.clone());
            let stop = cancel.clone();
            let looked = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<LocalItemId>> {
                let mut missing = Vec::new();
                for (mark, content) in marks {
                    if stop.is_cancelled() {
                        break;
                    }
                    let Some(content) = content else { continue };
                    if decide_cached_thumbnail(&find_cached_sizes(&cache, &folder.join(&mark.path), content.mtime().0, content.size())?) == CachedThumbnail::Missing {
                        missing.push(mark.item);
                    }
                }
                Ok(missing)
            });
            let missing = match looked.await {
                Ok(Ok(missing)) => missing,
                Ok(Err(e)) => {
                    tracing::warn!("cannot look into the thumbnail cache: {e}");
                    break;
                }
                Err(e) => {
                    tracing::warn!("the look into the thumbnail cache failed: {e}");
                    break;
                }
            };
            if !missing.is_empty() {
                let count = missing.len();
                if let Err(e) = self.store.call(move |store| store.item_store().forget_thumbnail_marks(&missing)).await {
                    tracing::warn!("cannot forget the marks of the thumbnails missing from the cache: {e}");
                    break;
                }
                forgotten += count;
            }
            if next.is_none() {
                break;
            }
            after = next;
        }
        if forgotten > 0 {
            tracing::info!("the cache lacks {forgotten} thumbnails made before; they are made again");
        }
    }

    /// Runs in the background: after every cycle (`kick`), when thumbnails are
    /// turned on again, and every ten minutes ([`TIMER_PERIOD`]) in case a kick was missed,
    /// draining 200 thumbnails at a time until every candidate has been looked at. The
    /// first drain, and the timer's, look into the cache first ([`DrainCause`]).
    pub(crate) fn spawn(self, kick: Arc<Notify>, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // A timer of its own, which a kick does not start over: kicks come every
            // minute while the folder is listed.
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + TIMER_PERIOD, TIMER_PERIOD);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut first = Some(DrainCause::SyncStarted);
            loop {
                let woke = tokio::select! {
                    () = kick.notified() => DrainCause::Nudged,
                    () = self.running.thumbnails_turned_on() => DrainCause::Nudged,
                    _ = timer.tick() => DrainCause::TimerFired,
                    () = cancel.cancelled() => return,
                };
                // Turned off, or paused (`docs/design/writes.md` §6.3): no request; the
                // next kick after the pause ends, or turning them on, drains what waits.
                if !self.running.thumbnails_go() {
                    continue;
                }
                self.drain(&cancel, 200, first.take().unwrap_or(woke)).await;
            }
        })
    }
}

/// What one thumbnail needs, for a task of its own.
struct One {
    drive: DriveClient,
    store: Store,
    cache: PathBuf,
    folder: PathBuf,
}

impl One {
    /// Asks Graph for the thumbnail of `wanted`, caches it and records what settled.
    async fn make(self, wanted: Wanted) -> CandidateOutcome {
        // A candidate is a file OneDrive has: its binding is what Graph is asked by, and the content OneDrive has
        // is what the thumbnail is of.
        let (Some(reference), Some(content)) = (wanted.item.onedrive_item.clone(), wanted.item.onedrive.as_ref().and_then(|version| version.content.clone())) else {
            return CandidateOutcome::NotSettled;
        };
        let (id, rel) = (reference.item_id.as_str(), &wanted.path);
        let mut fetched = self.drive.thumbnail(id, GRAPH_SIZE).await;
        if matches!(fetched, Ok(Thumbnail::Refused(konedrive_graph::drive::Status::NOT_ACCEPTABLE))) {
            fetched = self.drive.thumbnail(id, FALLBACK_SIZE).await;
        }
        // What settles the question of whether the item has a usable thumbnail is
        // recorded: a write, a 404, a refusal, an oversized body, bytes that will not
        // decode. A passing trouble (`DriveClient::thumbnail`'s `Err`) and a cache that
        // cannot be written are not, and are asked again at the next drain.
        let outcome = match fetched {
            Ok(Thumbnail::Image(bytes)) => {
                let (cache, file, mtime, source_size) = (self.cache.clone(), self.folder.join(rel), content.mtime().0, content.size());
                match tokio::task::spawn_blocking(move || write_thumbnail(&cache, &file, mtime, source_size, &bytes)).await {
                    Ok(Ok(())) => CandidateOutcome::Written,
                    Ok(Err(FillError::Undecodable(reason))) => {
                        tracing::warn!("no usable thumbnail for {}: {reason}", rel.display());
                        CandidateOutcome::NoThumbnail
                    }
                    Ok(Err(FillError::Io(reason))) => {
                        tracing::warn!("cannot cache the thumbnail of {}, no more are asked for until the next drain: {reason}", rel.display());
                        CandidateOutcome::CacheUnwritable
                    }
                    Err(e) => {
                        tracing::warn!("the thumbnail task for {} failed: {e}", rel.display());
                        CandidateOutcome::NotSettled
                    }
                }
            }
            Ok(Thumbnail::None) => CandidateOutcome::NoThumbnail,
            Ok(Thumbnail::Refused(status)) => {
                // Once per version of the item: recorded, so not asked again
                // until it changes.
                tracing::info!("OneDrive refuses a thumbnail of {}: {status}", rel.display());
                CandidateOutcome::NoThumbnail
            }
            Err(e) => {
                tracing::info!("no thumbnail for {} this time: {e}", rel.display());
                CandidateOutcome::NotSettled
            }
        };
        let is_written = match outcome {
            CandidateOutcome::Written => true,
            CandidateOutcome::NoThumbnail => false,
            CandidateOutcome::NotSettled | CandidateOutcome::CacheUnwritable => return outcome,
        };
        let (item, path) = (wanted.item.local_item_id, wanted.path.clone());
        if let Err(e) = self.store.call(move |store| store.item_store().set_thumbnail_made(item, &content, &path, is_written)).await {
            tracing::warn!("cannot record the thumbnail of {}: {e}", wanted.path.display());
        }
        outcome
    }
}

/// Whether the cache holds a file's thumbnail whole, given the sizes whose PNG a look
/// found under every URI spelling (`found`, by directory name). Pure; the lookups are
/// [`find_cached_sizes`].
fn decide_cached_thumbnail(found: &[&str]) -> CachedThumbnail {
    if SIZES.iter().all(|(size, _)| found.contains(size)) {
        CachedThumbnail::Whole
    } else {
        CachedThumbnail::Missing
    }
}

/// The sizes held under every spelling, repairing a missing or stale name from the
/// current PNG under the other name at that size. A current PNG from any writer is left
/// alone. If both names lack a size, the caller forgets the mark and asks Graph as before.
/// Blocking; no source file is opened. I/O errors end the cache scan without forgetting.
fn find_cached_sizes(cache: &Path, file: &Path, mtime: i64, source_size: u64) -> std::io::Result<Vec<&'static str>> {
    let uris = thumbnail_uris(file);
    let names: Vec<_> = uris.iter().map(|uri| name_for_uri(uri)).collect();
    let mut found = Vec::new();
    for (size, edge) in SIZES {
        let images = uris.iter().zip(&names).map(|(uri, name)| cached_png(&cache.join(size).join(name), uri, mtime, source_size)).collect::<std::io::Result<Vec<_>>>()?;
        if let Some(png) = images.iter().flatten().next() {
            if images.iter().any(Option::is_none) {
                // Decoding is needed only for a repair, under the same limits as Graph.
                let Ok(image) = decode_thumbnail(&png.bytes) else { continue };
                for ((uri, name), present) in uris.iter().zip(&names).zip(&images) {
                    if present.is_none() {
                        write_size(cache, name, uri, mtime, &image, size, *edge).map_err(std::io::Error::other)?;
                    }
                }
            }
            found.push(*size);
        }
    }
    Ok(found)
}

/// A current cache image and its writer; repairs reuse the bytes, Graph writes preserve
/// another program's current PNG even when the two URI spellings share one name.
struct CachedPng {
    bytes: Vec<u8>,
    software: Option<String>,
}

/// Reads only a bounded cache PNG, never the original image. The tags are checked even
/// for another program's PNG, including its optional size (Gwenview's acceptance rule).
/// Missing, corrupt, oversized or stale PNGs are absent; other I/O errors stop the scan.
fn cached_png(path: &Path, uri: &str, mtime: i64, source_size: u64) -> std::io::Result<Option<CachedPng>> {
    use std::io::Read;
    const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    match std::fs::File::open(path) {
        Ok(file) => { file.take(MAX_CACHE_BYTES + 1).read_to_end(&mut bytes)?; }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Ok(None);
    }
    let decoder = png::Decoder::new_with_limits(std::io::Cursor::new(&bytes), png::Limits { bytes: MAX_DECODE_ALLOC as usize });
    let Ok(mut reader) = decoder.read_info() else { return Ok(None) };
    if reader.info().width > MAX_DECODE_EDGE || reader.info().height > MAX_DECODE_EDGE || reader.finish().is_err() {
        return Ok(None);
    }
    let info = reader.info();
    // A compressed tag inflates to at most this; a longer one is no tag of ours.
    const MAX_TEXT_BYTES: usize = 64 * 1024;
    let text = |key: &str| {
        info.uncompressed_latin1_text.iter().find(|t| t.keyword == key).map(|t| t.text.clone())
            .or_else(|| info.compressed_latin1_text.iter().find(|t| t.keyword == key).and_then(|t| {
                let mut t = t.clone();
                t.decompress_text_with_limit(MAX_TEXT_BYTES).ok()?;
                t.get_text().ok()
            }))
            .or_else(|| info.utf8_text.iter().find(|t| t.keyword == key).and_then(|t| {
                let mut t = t.clone();
                t.decompress_text_with_limit(MAX_TEXT_BYTES).ok()?;
                t.get_text().ok()
            }))
    };
    let size = text("Thumb::Size").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let current = text("Thumb::URI").as_deref() == Some(uri)
        && text("Thumb::MTime").and_then(|s| s.parse::<i64>().ok()) == Some(mtime)
        && (size == 0 || size == source_size);
    let software = text("Software");
    drop(reader);
    Ok(current.then_some(CachedPng { bytes, software }))
}

/// The MD5 of a viewer's URI spelling, encoded as UTF-8 (QFile::encodeName on Unix).
fn name_for_uri(uri: &str) -> String {
    format!("{:x}.png", md5::Md5::digest(uri.as_bytes()))
}

/// The KIO name, also used by tests of the cache shared with Dolphin.
#[cfg(test)]
fn thumbnail_name(file: &Path) -> String {
    name_for_uri(&file_uri(file))
}

/// Decodes Graph's image once and writes all sizes under both names atomically.
/// A current PNG from another writer is left alone. The daemon's own entries are
/// refreshed even when a new content version has the same mtime and source size.
fn write_thumbnail(cache: &Path, file: &Path, mtime: i64, source_size: u64, jpeg: &[u8]) -> Result<(), FillError> {
    let image = decode_thumbnail(jpeg)?;
    for uri in &thumbnail_uris(file) {
        let name = name_for_uri(uri);
        for (dir, edge) in SIZES {
            let current = cached_png(&cache.join(dir).join(&name), uri, mtime, source_size).map_err(|e| FillError::Io(e.to_string()))?;
            if current.as_ref().is_some_and(|png| png.software.as_deref() != Some("konedrive")) {
                continue;
            }
            write_size(cache, &name, uri, mtime, &image, dir, *edge).map_err(|e| FillError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

/// The same decode limits for Graph images and PNGs used to repair another cache name.
fn decode_thumbnail(bytes: &[u8]) -> Result<image::DynamicImage, FillError> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| FillError::Undecodable(e.to_string()))?;
    reader.limits(limits);
    reader.decode().map_err(|e| FillError::Undecodable(e.to_string()))
}

/// Scale down, tag and atomically replace one cache PNG. Unicode URIs use iTXt,
/// since PNG's older tEXt chunk can only hold Latin-1.
fn write_size(cache: &Path, name: &str, uri: &str, mtime: i64, image: &image::DynamicImage, dir: &str, edge: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let scaled = if image.width().max(image.height()) > edge { image.thumbnail(edge, edge) } else { image.clone() };
    let rgba = scaled.to_rgba8();
    let dir = cache.join(dir);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let tmp = dir.join(format!(".{name}.konedrive"));
    {
        let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), rgba.width(), rgba.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if uri.is_ascii() {
            encoder.add_text_chunk("Thumb::URI".into(), uri.to_owned())?;
        } else {
            encoder.add_itxt_chunk("Thumb::URI".into(), uri.to_owned())?;
        }
        encoder.add_text_chunk("Thumb::MTime".into(), mtime.to_string())?;
        encoder.add_text_chunk("Software".into(), "konedrive".into())?;
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba.as_raw())?;
        writer.finish()?;
    }
    std::fs::rename(&tmp, dir.join(name))?;
    Ok(())
}

#[cfg(test)]
mod tests;
