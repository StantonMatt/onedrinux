//! Per-download assembly from version-bound memory pieces and missing Graph ranges.
//! Rejected bytes disable reuse for the rest of this download, including its retry.
use super::*;
use crate::content_source::source::FetchNext;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

pub(super) struct Download {
    pub(super) source: GraphSource,
    pub(super) item_id: String,
    pub(super) received: Arc<dyn Fn(u64) + Send + Sync>,
    pub(super) fresh: AtomicBool,
}

#[async_trait]
impl ContentSource for Download {
    async fn fetch(&self, item_id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError> {
        if self.fresh.load(Ordering::SeqCst) {
            return self.fresh_fetch(item_id, from, end).await;
        }
        let item = self.source.drive.item(item_id).await.map_err(source_error)?;
        if item.deleted.is_some() || item.file.is_none() {
            return Err(SourceError::NotFound(format!("{item_id} is not a file in OneDrive any more")));
        }
        // Reuse is only an input to a hash-verified remote download.
        let Some(size) = item.size.filter(|_| version_of(&item).and_then(|v| v.quick_xor).is_some()) else {
            return self.fresh_fetch(item_id, from, end).await;
        };
        let stop = end.unwrap_or(size).min(size).max(from);
        let url = match &item.download_url {
            Some(url) => url.clone(),
            None => self.source.drive.content_url(item_id).await.map_err(source_error)?,
        };
        let mut state = Assembly { source: self.source.clone(), item_id: item_id.to_owned(), item: item.clone(),
            at: from, stop, url, received: Arc::clone(&self.received), bounded: end.is_some(), active: None };
        // Every missing range opens here, inside the caller's throttle race and typed
        // refusal handling. The stream yields FetchNext before another network open.
        state.active = state.next().await?;
        let stream = futures_util::stream::try_unfold(state, |mut state| async move {
            loop {
                // Playback may have filled this block while the response was in flight.
                // Stop consuming that response before its newly cached block; the next
                // missing range starts after the reused bytes, never from staging.
                if state.active.as_ref().is_some_and(|(_, _, network)| *network) && state.at % read_blocks::BLOCK == 0 {
                    if let (Some(piece), _) = state.cached().map_err(io::Error::other)? {
                        let len = piece.bytes.len() as u64;
                        state.active = Some((piece.into_fetched().stream, len, false));
                    }
                }
                let Some((stream, left, _)) = &mut state.active else { return Ok::<_, io::Error>(None) };
                let until_block = read_blocks::BLOCK - state.at % read_blocks::BLOCK;
                let mut bytes = vec![0; (64 * 1024).min((*left).min(until_block) as usize)];
                if bytes.is_empty() {
                    if state.at >= state.stop { return Ok(None); }
                    let (piece, _) = state.cached().map_err(io::Error::other)?;
                    let piece = piece.ok_or_else(|| io::Error::other(FetchNext))?;
                    let len = piece.bytes.len() as u64;
                    state.active = Some((piece.into_fetched().stream, len, false));
                    continue;
                }
                let n = stream.read(&mut bytes).await?;
                if n == 0 { return Err(io::ErrorKind::UnexpectedEof.into()); }
                bytes.truncate(n);
                *left -= n as u64;
                state.at += n as u64;
                return Ok(Some((std::io::Cursor::new(bytes), state)));
            }
        });
        Ok(Fetched { served_from: from, size: Some(size), mtime: mtime_of(&item), version: version_of(&item),
            stream: Box::new(tokio_util::io::StreamReader::new(Box::pin(stream))) })
    }

    fn reject_written(&self) {
        self.fresh.store(true, Ordering::SeqCst);
        self.source.read_blocks.forget(&self.item_id);
    }
}

impl Download {
    async fn fresh_fetch(&self, item_id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError> {
        let mut fetched = self.source.fetch(item_id, from, end).await?;
        fetched.stream = Box::new(Network { inner: fetched.stream, received: Arc::clone(&self.received) });
        Ok(fetched)
    }
}

/// Recheck the live cache at each block boundary to pick up concurrent playback.
/// With no playback cache, retain the original single full-response download.
struct Assembly {
    source: GraphSource,
    item_id: String,
    item: DriveItem,
    at: u64,
    stop: u64,
    url: String,
    received: Arc<dyn Fn(u64) + Send + Sync>,
    bounded: bool,
    active: Option<(Box<dyn AsyncRead + Send + Unpin>, u64, bool)>,
}

impl Assembly {
    fn cached(&self) -> Result<(Option<Piece>, u64), SourceError> {
        let answer = {
            let _cache_use = crate::panic::lock(&self.source.cache_use);
            if !self.source.drive.cached_content_allowed() {
                return Err(SourceError::SignedOut("cached download invalidated".into()));
            }
            let ranges = crate::panic::lock(&self.source.ranges);
            let generations: Vec<_> = ranges.get(&self.item_id).into_iter().flat_map(|g| g.iter())
                .filter(|(opened, bound)| {
                    opened.size() == self.item.size.unwrap_or(0)
                        && self.item.e_tag.as_deref().is_some_and(|tag| !tag.is_empty() && bound.metadata_etag.as_deref() == Some(tag))
                        && match opened {
                            ContentVersion::Hashed { ctag, hash, .. } => {
                                ctag.as_deref().is_none_or(|tag| self.item.c_tag.as_deref() == Some(tag))
                                    && hash.as_deref().is_none_or(|hash| self.item.quick_xor_hash() == Some(hash))
                            }
                            _ => false,
                        }
                        && bound.checked.as_ref().is_none_or(|(version, _, _)| read_blocks::matches_fresh(version, &version_of(&self.item)))
                }).map(|(opened, _)| opened.clone()).collect();
            self.source.read_blocks.download_piece(&self.item_id, &generations, self.at, self.stop, &version_of(&self.item))
        };
        Ok(answer)
    }

    async fn next(&mut self) -> Result<Option<(Box<dyn AsyncRead + Send + Unpin>, u64, bool)>, SourceError> {
        if self.at >= self.stop { return Ok(None); }
        let (piece, next) = self.cached()?;
        if let Some(piece) = piece {
            let len = piece.bytes.len() as u64;
            return Ok(Some((piece.into_fetched().stream, len, false)));
        }
        let end = next;
        let asked_end = (self.bounded || end < self.stop).then_some(end);
        let mut refreshed = false;
        let mut url = self.url.clone();
        let download = loop {
            match self.source.drive.download(&url, self.at, asked_end).await {
                Ok(download) => break download,
                Err(DriveError::UrlExpired) if !refreshed => {
                    refreshed = true;
                    let fresh = self.source.drive.item(&self.item_id).await.map_err(source_error)?;
                    if fresh.deleted.is_some() || fresh.file.is_none() || fresh.size != self.item.size
                        || fresh.e_tag != self.item.e_tag || version_of(&fresh) != version_of(&self.item) {
                        return Err(SourceError::Transient("the file changed while its download URL expired; reopen its metadata".into()));
                    }
                    url = match fresh.download_url {
                        Some(url) => url,
                        None => self.source.drive.content_url(&self.item_id).await.map_err(source_error)?,
                    };
                }
                Err(error) => return Err(source_error(error)),
            }
        };
        // Metadata supplies the whole size. A short 200's Content-Length can
        // describe only its usable prefix; let the usual short-stream path keep
        // and checkpoint that prefix instead of rejecting it before any write.
        if download.served_from != self.at {
            return Err(SourceError::Transient("the missing range answered another offset".into()));
        }
        Ok(Some((Box::new(Network { inner: download.stream, received: Arc::clone(&self.received) }), end - self.at, true)))
    }
}

/// Only network input consumes retry traffic; cached bytes still pass through the
/// downloader's space check, disk writer, progress/checkpoints and complete hash.
struct Network {
    inner: Box<dyn AsyncRead + Send + Unpin>,
    received: Arc<dyn Fn(u64) + Send + Sync>,
}
impl AsyncRead for Network {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = result { (self.received)((buf.filled().len() - before) as u64); }
        result
    }
}
