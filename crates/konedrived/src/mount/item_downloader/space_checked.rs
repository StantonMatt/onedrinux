//! Recheck space while a read-triggered or pin stream is consumed: other programs may have
//! allocated on this filesystem since admission. The check is outside every tree lock.
use crate::content_source::source::{ContentSource, Fetched, SourceError};
use crate::mount::backing_directory::BackingDirectory;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

pub(super) struct SpaceChecked<'a> {
    pub(super) charge: Option<Arc<crate::download_guard::DownloadCharge>>,
    pub(super) inner: &'a dyn ContentSource,
    pub(super) backing: Arc<BackingDirectory>,
    /// None is an explicit hydrate, whose disk behavior is unchanged.
    pub(super) reserve: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

struct Stream {
    inner: Box<dyn AsyncRead + Send + Unpin>,
    charge: Option<Arc<crate::download_guard::DownloadCharge>>,
    left: u64,
    backing: Arc<BackingDirectory>,
    reserve: Arc<dyn Fn() -> u64 + Send + Sync>,
}
impl AsyncRead for Stream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let reserve = (self.reserve)();
        let room = self.backing.measure_free_space().ok().and_then(|free| free.checked_sub(reserve)).unwrap_or(0);
        // Checking the buffer's whole capacity is conservative at EOF. Never let a
        // network buffer through if writing it could consume the reserve.
        if room < (buf.remaining() as u64).min(self.left) {
            return Poll::Ready(Err(io::Error::from_raw_os_error(libc::ENOSPC)));
        }
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled {
            let bytes = (buf.filled().len() - before) as u64;
            self.left = self.left.saturating_sub(bytes);
            if let Some(charge) = &self.charge {
                charge.received(bytes);
            }
        }
        polled
    }
}
#[async_trait::async_trait]
impl ContentSource for SpaceChecked<'_> {
    async fn fetch(&self, item_id: &str, from: u64, end: Option<u64>) -> Result<Fetched, SourceError> {
        let mut fetched = self.inner.fetch(item_id, from, end).await?;
        if self.reserve.is_some() || self.charge.is_some() {
            let reserve = self.reserve.clone().unwrap_or_else(|| Arc::new(|| 0));
            fetched.stream = Box::new(Stream {
                inner: fetched.stream,
                charge: self.charge.clone(),
                left: fetched.size.unwrap_or(u64::MAX).saturating_sub(from),
                backing: Arc::clone(&self.backing),
                reserve,
            });
        }
        Ok(fetched)
    }
    fn progress(&self, done: u64, size: u64) {
        self.inner.progress(done, size);
    }
    fn reject_written(&self) {
        self.inner.reject_written();
    }
}
