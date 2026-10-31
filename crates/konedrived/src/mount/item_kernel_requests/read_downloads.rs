//! Admission, scan/handle coverage and launch-triggered whole downloads for reads.
// test: read_ahead_trace_decode_lands_only_the_handle_that_reads_the_image_through
// test: read_ahead_tiny_desktop_download_waits_but_guarded_reads_start_immediately
// test: launch_intent_absolute_paths_and_file_urls_show_immediately_and_land_during_scans
// test: read_ahead_handle_keeps_successful_coverage_across_scan_activation
// test: read_ahead_tiny_promotion_rechecks_policy_after_awaiting_the_store

use super::*;

impl ItemKernelRequests {
    pub(super) async fn read_range_as(
        &self,
        range: RangeToFetch,
        pid: Option<u32>,
        caller: Option<crate::download_guard::ReadAheadCaller>,
    ) -> Result<Vec<u8>, Errno> {
        self.admit_remote(pid, &range, range.end.saturating_sub(range.from))?;
        let key = DownloadKey {
            item: range.item,
            content: range.source.content.clone(),
        };
        let launched = pid.and_then(|pid| self.launch_intents.as_ref()?.take(pid, &key));
        if launched.is_some() {
            self.start_read_download(range.clone(), pid, caller.clone(), true, None);
            self.download_states.show_launched_download(&key);
        }
        drop(launched);
        let tiny_cancel = if range.source.content.size() <= crate::mount::read_ahead::PEEK_BYTES {
            caller
                .as_ref()
                .zip(range.scan_file.as_ref())
                .and_then(|(caller, file)| Some(self.read_ahead.as_ref()?.tiny_read(caller, file)))
        } else {
            None
        };
        let mut head = caller.as_ref().and_then(|caller| {
            self.read_ahead.as_ref()?.head(
                caller.clone(),
                range.item,
                &range.source.content,
                range.scan_file.as_ref()?,
                range.from,
            )
        });
        let in_scan = head.as_ref().is_some_and(|head| head.in_scan);
        let item_count = (!in_scan).then(|| {
            self.download_states
                .begin_read_count(range.item, &range.source.content)
        });
        let bytes = self.fetch_range(&range).await?;
        if let Some(head) = &mut head {
            head.completed = true;
        }
        // Keep the handle's entire successful history, including reads before a
        // scan activates. Outside scans only item coverage decides promotion.
        let handle_reached = self.download_states.finish_read_count(
            &range.handle_count,
            range.from,
            bytes.len() as u64,
        );
        let reached = item_count.as_ref().map_or(handle_reached, |counting| {
            self.download_states
                .finish_read_count(counting, range.from, bytes.len() as u64)
        });
        if reached
            && (!in_scan || range.source.content.size() > crate::mount::read_ahead::PEEK_BYTES)
        {
            if in_scan {
                tracing::debug!(item = %range.item, fh = range.fh.0, "handle coverage qualifies for whole download");
            }
            self.start_read_download(range, pid, caller, false, tiny_cancel);
        }
        Ok(bytes)
    }

    fn start_read_download(
        &self,
        range: RangeToFetch,
        pid: Option<u32>,
        caller: Option<crate::download_guard::ReadAheadCaller>,
        launched: bool,
        tiny_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) {
        // A range fetch may have awaited since this caller was admitted; launch
        // intent uses this same gate before charging and starting synchronously.
        if caller
            .as_ref()
            .is_some_and(|caller| !self.download_guard.permits_read_ahead(caller))
            || self.download_states.is_downloading(range.item)
        {
            return;
        }
        let delay = !launched
            && range.source.content.size() <= crate::mount::read_ahead::PEEK_BYTES
            && caller
                .as_ref()
                .is_some_and(|caller| self.download_guard.permits_read_ahead(caller));
        if delay {
            let key = DownloadKey {
                item: range.item,
                content: range.source.content.clone(),
            };
            let Some(pending) = self
                .download_states
                .delay_read(key, caller.as_ref().unwrap().identity())
            else {
                return;
            };
            // Bind admission and attribution while this PID still identifies the reader.
            // The grace period must also work for short-lived desktop processes.
            let Ok(charge) = self.charge_download(pid, &range) else {
                return;
            };
            let caller = caller.unwrap();
            let store = self.store.clone();
            let mut stopped = self.download_states.stopped();
            let (downloader, states, guard) = (
                Arc::clone(&self.downloader),
                Arc::clone(&self.download_states),
                Arc::clone(&self.download_guard),
            );
            // A promotion is not an outstanding read: keep no fh/coverage snapshot
            // after returning its bytes, only the content needed by the download.
            let (item, content, file) = (
                range.item,
                range.source.content,
                range.download_at_threshold,
            );
            let size = content.size();
            tokio::spawn(async move {
                let _pending = pending;
                if *stopped.borrow() {
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep(crate::mount::read_ahead::TINY_DOWNLOAD_DELAY) => {},
                    _ = stopped.changed() => return,
                }
                if tiny_cancel
                    .as_ref()
                    .is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::Relaxed))
                {
                    tracing::debug!(item = %item, "dropped tiny-file download after folder scan activated");
                    return;
                }
                if !guard.permits_read_ahead(&caller) {
                    return;
                }
                let still_cloud = store
                    .read_items(move |reader| {
                        Ok(reader.find_item(item)?.is_some_and(|row| {
                            row.content_presence == konedrive_tree::ContentPresence::NotOnDisk
                                && row.local.and_then(|local| local.content).as_ref()
                                    == Some(&content)
                        }))
                    })
                    .await
                    .unwrap_or(false);
                // Policy, cancellation and mount lifetime may have changed while
                // the row lookup awaited. No await separates these checks from start.
                if still_cloud
                    && !*stopped.borrow()
                    && guard.permits_read_ahead(&caller)
                    && !tiny_cancel.as_ref().is_some_and(|cancelled| {
                        cancelled.load(std::sync::atomic::Ordering::Relaxed)
                    })
                    && !states.is_downloading(item)
                    && downloader.read_fits(size)
                {
                    downloader.download_charged(file, None, charge);
                }
            });
        } else if !self.download_states.is_downloading(range.item)
            && self.downloader.read_fits(range.source.content.size())
        {
            if let Ok(charge) = self.charge_download(pid, &range) {
                self.downloader
                    .download_charged(range.download_at_threshold, None, charge);
            }
        }
    }
}
