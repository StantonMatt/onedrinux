//! The guarded file and directory removals sent to OneDrive.

use super::*;

impl OneDriveSteps {
    /// What a removal's request leaves the step: `Ok` when the item is gone in OneDrive —
    /// removed, found gone (a 404 is a confirmation), or read as gone after a lost
    /// answer —, else the outcome. A 412 is `ItemChanged`.
    async fn follow_removal_answer(&self, item: &ItemVersions, answered: Result<(), WriteError>) -> Result<(), StepOutcome> {
        let error = match answered {
            Ok(()) | Err(WriteError::NotFound) => return Ok(()),
            Err(error) => error,
        };
        match decide_refusal(&error) {
            RefusedRequest::AnswerLost { detail } => match self.follow_lost_answer(item, SentRequest::SendRemoval, None, detail).await? {
                None => Ok(()),
                // A removal confirms no item: read as one that did not go through.
                Some(_) => Err(self.refuse_for_certain(item, StepOutcome::Refused(StepRefusal::ItemChanged)).await),
            },
            RefusedRequest::Changed => Err(self.refuse_for_certain(item, StepOutcome::Refused(StepRefusal::ItemChanged)).await),
            _ => Err(self.refuse_plainly(item, &error, StepRefusal::ItemGone).await),
        }
    }

    /// `SendFileRemoval`: the removal of a file that was removed here is sent, guarded.
    ///
    /// Request: `delete_item`, on record before it leaves. An item with no guard is
    /// `StepFailure::AnswerUnreadable`.
    /// Answer: nothing. 404 is a confirmation: the item is gone. 412: `ItemChanged`; the
    /// next plan has "OneDrive changed its content: the item comes back" or "only moved
    /// it: the removal is sent" as rows, with the eTag a listing wrote.
    /// Commit: `commit_send_file_removal(item)`, which forgets what was kept for the
    /// request. `RowsChanged`: a listing cycle is asked for.
    pub(super) async fn run_send_file_removal(&self, item: &ItemVersions) -> StepOutcome {
        let (onedrive_item, guard) = match self.find_guarded_item(item, decide_guard) {
            Ok(guarded) => guarded,
            Err(outcome) => return outcome,
        };
        if let Err(outcome) = self.remember_before_request(item, None, Some(RecordedRequest::SendFileRemoval)).await {
            return outcome;
        }
        let answered = {
            let _slot = self.graph.drive.pool().acquire(Class::Metadata).await;
            if let Err(outcome) = self.check_sender(&OneDriveStep::SendFileRemoval { item: item.clone() }).await {
                return self.refuse_for_certain(item, outcome).await;
            }
            self.graph.drive.delete_item(onedrive_item.item_id.as_str(), guard.as_str()).await
        };
        if let Err(outcome) = self.follow_removal_answer(item, answered).await {
            return outcome;
        }
        if let Some(guard) = &self.graph.change_guard {
            if let Err(detail) = guard.confirm_removals(&[item.local_item_id]) { return StepOutcome::Failed(StepFailure::Store { detail }); }
        }
        let planned = item.clone();
        let committed = match self.commit_step(item, move |store| store.item_store().commit_send_file_removal(&planned)).await {
            Ok(committed) => committed,
            Err(outcome) => return outcome,
        };
        match committed.outcome {
            StepCommitOutcome::Written => describe_written(ActivityKind::CloudDeleted, committed.path_before, None),
            StepCommitOutcome::RowsChanged => StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted),
        }
    }

    /// `SendDirectoryRemoval`: the removal of a directory that was removed here, with all
    /// that was in it, is sent, guarded.
    ///
    /// Guard: the folder's eTag the step was planned from (`item.onedrive_etag`), as for
    /// a file (`decide_guard`). OneDrive personal gives a folder no cTag, and moves its
    /// eTag at its own rename or move and at nothing below it (measured 2026-10-29): what
    /// is below is checked by the rows, after a listing. A directory with no eTag is
    /// `StepFailure::FolderNotDescribed` (`refuse_without_folder_guard`): the step waits
    /// for a listing that writes one, and nothing is sent.
    /// Before the request every attempt waits for a listing cycle of its own. Its first
    /// look notes the count of listings begun and asks for a cycle
    /// (`RowsChanged(CycleWanted)`); a look while it waits is
    /// `NotRun(StepObstacle::ListingAwaited)`, which the listing's end wakes. A listing
    /// under way at the first look has that count, so it is not the one waited for. Once a
    /// listing begun after the first look has ended, the wait is used up, whatever comes
    /// of the attempt — a hold, rows that changed, a failure, a 412 —, and the next
    /// attempt waits for another. It is used up only by a look within
    /// `DIRECTORY_REMOVAL_LISTING_FRESH_SECONDS` of the listing's end; a later look, after
    /// a hold, begins a new wait. A hold that stops the step before it comes here (`run`)
    /// forgets the wait. Then, under the tree lock, the rows are asked again
    /// (`can_directory_removal_be_sent`): an item the listing brought below the directory,
    /// or changed there, is `RowsChanged(Nothing)` and the next plan decides; the record
    /// of the request is written in the same job.
    /// Request: `delete_folder`, which takes the whole tree to the recycle bin. What
    /// OneDrive gets below the directory between the listing and the request goes with
    /// it, to the recycle bin.
    /// Answer: nothing. 404 is a confirmation: the directory is gone. 412: `ItemChanged`,
    /// the folder was renamed or moved in OneDrive; a cycle writes its new eTag, and the
    /// next plan decides.
    /// Commit: `commit_send_directory_removal(item, below)`. The settlement of a written
    /// removal asks for a listing cycle (`step_settlement`): a 404 says nothing of what was
    /// below, and an item moved out of the directory meanwhile is listed again.
    pub(super) async fn run_send_directory_removal(&self, item: &ItemVersions, below: &[ItemVersions]) -> StepOutcome {
        let id = item.local_item_id;
        let (onedrive_item, guard) = match self.find_guarded_item(item, decide_folder_guard).map_err(|outcome| refuse_without_folder_guard(item, &self.graph.drive_id, outcome)) {
            Ok(guarded) => guarded,
            Err(outcome) => return outcome,
        };
        let listing = self.graph.store.call(|store| Ok((store.listing_count()?, store.item_store().find_listing_under_way()?.is_some(), store.last_checked()?))).await;
        let (count, under_way, ended_at) = match listing { Ok(listing) => listing, Err(error) => return fail_store(error) };
        // The clock `last_checked` is written with.
        let ended_lately = ended_at.is_some_and(|ended_at| (0..=DIRECTORY_REMOVAL_LISTING_FRESH_SECONDS).contains(&(crate::clock::unix_now() - ended_at)));
        let waited = {
            let mut listings = crate::panic::lock(&self.directory_removal_listings);
            listings.retain(|(known, stamp), _| *known != id || *stamp == item.unsent_since);
            let has_ended_since = |first: u64| count > first && !under_way;
            match listings.get(&(id, item.unsent_since)).copied() {
                // The wait is used up by this attempt, whatever comes of it.
                Some(first) if has_ended_since(first) && ended_lately => {
                    listings.remove(&(id, item.unsent_since));
                    None
                }
                Some(first) if !has_ended_since(first) => Some(StepOutcome::NotRun(StepObstacle::ListingAwaited)),
                // A first look, or one long after the listing it waited for ended: a wait begins.
                None | Some(_) => {
                    listings.insert((id, item.unsent_since), count);
                    Some(StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted))
                }
            }
        };
        if let Some(outcome) = waited {
            return outcome;
        }
        let can_be_sent = {
            let _tree = self.tree_lock.lock().await;
            let (mut busy_items, mut destinations) = (self.running_steps.list_items(), self.running_steps.list_destinations());
            let (planned, planned_below) = (item.clone(), below.to_vec());
            let record = RequestRecord { item: id, onedrive_item: Some(onedrive_item.clone()), request: RecordedRequest::SendDirectoryRemoval, sent_at: (self.graph.clock)() };
            self.graph
                .store
                .call(move |store| {
                    let destination_rows = store.read_items(|items| {
                        // The requests on record are not settled either, whether their step runs or not; this removal's own has none yet.
                        for standing in items.list_request_records(usize::MAX)? {
                            destinations.extend(standing.request.destination().cloned());
                            busy_items.insert(standing.item);
                        }
                        let mut destination_rows: BTreeSet<LocalItemId> = BTreeSet::new();
                        for destination in &destinations {
                            destination_rows.extend(items.find_bound_item(destination)?.map(|row| row.local_item_id));
                        }
                        Ok(destination_rows)
                    })?;
                    let can_be_sent = store.item_store().can_directory_removal_be_sent(&planned, &planned_below, &busy_items, &destination_rows)?;
                    if can_be_sent {
                        store.step_memory().remember_request_record(&record)?;
                    }
                    Ok(can_be_sent)
                })
                .await
        };
        match can_be_sent {
            Ok(true) => {}
            Ok(false) => return StepOutcome::RowsChanged(RowsChangedFollowUp::Nothing),
            Err(error) => return fail_store(error),
        }
        let answered = {
            let _slot = self.graph.drive.pool().acquire(Class::Metadata).await;
            if let Err(outcome) = self.check_sender(&OneDriveStep::SendDirectoryRemoval { item: item.clone(), below: below.to_vec() }).await {
                return self.refuse_for_certain(item, outcome).await;
            }
            self.graph.drive.delete_folder(onedrive_item.item_id.as_str(), guard.as_str()).await
        };
        if let Err(outcome) = self.follow_removal_answer(item, answered).await {
            return outcome;
        }
        if let Some(guard) = &self.graph.change_guard {
            let ids: Vec<_> = std::iter::once(item.local_item_id).chain(below.iter().map(|v| v.local_item_id)).collect();
            if let Err(detail) = guard.confirm_removals(&ids) { return StepOutcome::Failed(StepFailure::Store { detail }); }
        }
        let (planned, planned_below) = (item.clone(), below.to_vec());
        let committed = match self.commit_step(item, move |store| store.item_store().commit_send_directory_removal(&planned, &planned_below)).await {
            Ok(committed) => committed,
            Err(outcome) => return outcome,
        };
        match committed.outcome {
            StepCommitOutcome::Written => describe_written(ActivityKind::CloudDeleted, committed.path_before, None),
            StepCommitOutcome::RowsChanged => StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted),
        }
    }
}
