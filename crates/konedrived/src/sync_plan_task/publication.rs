//! Publication of a plan, queue progress and the activity of settled steps.

use super::*;

impl SyncPlanTask {
    /// Publishes what the properties of the bus say of the queue
    /// (`upload_queue::publish_queue_status`), from ONE `Store::read_items` with the
    /// published answers and the running steps taken before it
    /// (`upload_queue::read_queue_totals`, `count_queue`, `read_running_upload_paths`,
    /// `describe_running_uploads`), `read_queue_conditions` taken before the read too.
    /// With `is_counts_read` false only the bytes of the running uploads are published,
    /// with the counts as they were last read: the tick's round, which reads no row but
    /// the paths of the uploads. The task is the ONE writer of these fields. Called by
    /// the loop after every plan it published, after every round in which a step was
    /// settled, and at the tick ([`decide_progress_tick`]); a read that fails is a warning
    /// and publishes nothing.
    pub(super) async fn publish_queue_status(&self, is_counts_read: bool) {
        let parts = &self.parts;
        let conditions = crate::upload_queue::read_queue_conditions(&parts.sending_holds, &parts.onedrive_space, (parts.clock)());
        let answers = parts.published_plan.list_answers();
        let running = parts.running_steps.list_steps();
        let read = {
            let running = running.clone();
            move |items: &konedrive_tree::ItemReader<'_>| {
                let totals = if is_counts_read { Some(crate::upload_queue::read_queue_totals(items, &answers, &running)?) } else { None };
                Ok((totals, crate::upload_queue::read_running_upload_paths(items, &running)?))
            }
        };
        let (totals, upload_paths) = match parts.store.read_items(read).await {
            Ok(read) => read,
            Err(error) => {
                tracing::warn!("cannot read the upload queue of {}: {error}", parts.folder.display());
                return;
            }
        };
        let counts = {
            let mut queue_counts = crate::panic::lock(&self.queue_counts);
            if let Some(totals) = totals {
                *queue_counts = crate::upload_queue::count_queue(&totals, &conditions);
            }
            *queue_counts
        };
        // The bytes are those the steps said before the read: a step that began since is published with the next round.
        let uploads = crate::upload_queue::describe_running_uploads(&running, &upload_paths, &parts.folder);
        crate::upload_queue::publish_queue_status(&parts.state, counts, uploads, conditions.is_onedrive_full);
    }

    /// Announces what a round's settlement wrote and changed: each event written
    /// (`Activity::announce`, in the order written; `Activity::prune` once when one of
    /// them is a conflict, so that `Conflicts.Count` says it); the sync marks of the
    /// items of the steps towards OneDrive that were settled (`marks_announcer`); the
    /// thumbnail filler nudged when a step towards the folder was written
    /// (`after_folder_steps`). Called by [`settle_steps`](Self::settle_steps) after the
    /// job of the store, with the rows that job wrote, and after each of the steps was
    /// ended (`RunningSteps::end`): who reads a mark again on the announcement must not
    /// find the step still running.
    pub(super) async fn announce_settled_steps(&self, settled: &[(SyncStep, StepSettlement)], written: &[konedrive_tree::ActivityRow]) {
        let parts = &self.parts;
        for row in written {
            parts.activity.announce(row.clone());
        }
        // `Activity::prune` reads the store and asks the disk for the kept copies, through the mount for those inside the folder: off the loop's thread, and waited for only while the sync runs. Once the loop is stopping (`SendingHolds::set_stopping`, set before its last settlements) it is not asked: nothing here may wait for the mount to answer when a stop is to end, and `Conflicts.Count` is read again by the next listing and by `Conflicts.List`.
        let is_stopping = parts.sending_holds.find((parts.clock)()) == Some(SendingHold::Stopping);
        if !is_stopping && written.iter().any(|row| row.kind == konedrive_tree::ActivityKind::Conflict) {
            let activity = Arc::clone(&parts.activity);
            if let Err(error) = tokio::task::spawn_blocking(move || activity.prune()).await {
                tracing::warn!("the task that counts the conflicts ended: {error}");
            }
        }
        let sent_items: Vec<LocalItemId> = settled
            .iter()
            .filter(|(step, _)| match step {
                SyncStep::OneDrive(_) => true,
                SyncStep::Folder(_) | SyncStep::Row(_) => false,
            })
            .flat_map(|(step, _)| {
                let running_step = step_choice::describe_running_step(step);
                std::iter::once(running_step.item).chain(running_step.other_items)
            })
            .collect();
        if !sent_items.is_empty() {
            (parts.marks_announcer)(sent_items);
        }
        // A step that was written is the one whose tries are forgotten (`decide_step_settlement`): the settlement keeps no outcome.
        let is_folder_step_written = settled.iter().any(|(step, settlement)| match step {
            SyncStep::Folder(_) => settlement.local_step_tries == step_settlement::LocalStepTriesChange::Forgotten,
            SyncStep::OneDrive(_) | SyncStep::Row(_) => false,
        });
        if let (true, Some(after_folder_steps)) = (is_folder_step_written, &parts.after_folder_steps) {
            after_folder_steps.notify_one();
        }
    }

    /// Publishes what a plan answered: the answers per item (`PublishedPlan::publish`),
    /// the folder's counts (`PlannedRound::item_counts`, of the plan's own reading), the
    /// pinned files still to download (`PinnedFilesReading::find_total`, of the same) and
    /// note on the bus (through `state`), the faults of the plan in the journal, each
    /// once. The task is the one owner of the note (`UploadQueueStatus::note`): it writes it
    /// with every plan, also to none, and nobody else may. For
    /// `PlanFault::FullListingNeeded` the store is asked for a full listing
    /// (`ItemStore::request_full_listing`) and then the poller for a cycle
    /// (`ListingPollHandle::refresh_after_full_listing_request`): once per item until a
    /// listing asked for it ended, and again when the request failed.
    pub(super) async fn publish_round(&self, round: &PlannedRound, hold: Option<&SendingHold>) {
        let parts = &self.parts;
        let plan = &round.plan;
        let mut answers = BTreeMap::new();
        answers.extend(plan.steps.iter().map(|step| (step.local_item_id(), ItemAnswer::Planned(step_choice::decide_step_class(step)))));
        answers.extend(plan.waits.iter().map(|wait| (wait.item, ItemAnswer::Waiting(wait.reason.clone()))));
        answers.extend(plan.exclusions.iter().map(|exclusion| (exclusion.item, ItemAnswer::Excluded(exclusion.reason))));
        answers.extend(plan.faults.iter().map(|fault| (fault.local_item_id(), ItemAnswer::Fault(fault.clone()))));
        // What the mark of an item is answered from changes with the answers: the items whose answer turned to or from "blocked" are said to whoever watches the folder.
        let has_refusal_for_good = |item: &LocalItemId| round.memories.get(item).is_some_and(|memory| memory.refusal.is_some());
        let answers_before = parts.published_plan.list_answers();
        let marks_changed: Vec<LocalItemId> = answers_before
            .keys()
            .chain(answers.keys())
            .copied()
            .collect::<BTreeSet<LocalItemId>>()
            .into_iter()
            .filter(|item| decide_is_shown_blocked(answers_before.get(item), has_refusal_for_good(item)) != decide_is_shown_blocked(answers.get(item), has_refusal_for_good(item)))
            .collect();
        parts.published_plan.publish(answers);
        parts.published_plan.publish_unsent_not_to_send_count(round.unsent_not_to_send_count);
        if !marks_changed.is_empty() {
            (parts.marks_announcer)(marks_changed);
        }
        (parts.unsent_count_report)(round.unsent_to_send_count);

        let note = match hold {
            Some(SendingHold::Throttled { until }) => Some(UploadQueueNote::Throttled(*until)),
            Some(SendingHold::WritesNotAllowed { reason }) => Some(UploadQueueNote::WritesNotAllowed(reason.clone())),
            Some(SendingHold::Stopping | SendingHold::SignedOut | SendingHold::FolderReadOnly | SendingHold::Paused { .. } | SendingHold::FirstListingNotEnded) | None => None,
        };
        let counts = round.item_counts;
        parts.state.update_if_changed(|state| {
            // A reading that did not ask the reader of pinned files knows nothing anew: the count stays.
            state.transfers.pinned_download = round.pinned_files_reading.find_total().unwrap_or(state.transfers.pinned_download);
            state.cycle.items_listed = counts.in_onedrive;
            state.cycle.items_placed = counts.shown;
            state.cycle.skipped_count = counts.cannot_be_shown;
            state.local.pinned_count = u32::try_from(counts.pinned.count).unwrap_or(u32::MAX);
            state.local.pinned_bytes = counts.pinned.bytes;
            state.upload_queue.note = note;
        });

        // Each fault once: not one the plan before answered too.
        let new_faults: Vec<PlanFault> = {
            let mut told_faults = crate::panic::lock(&self.told_faults);
            let new_faults = plan.faults.iter().filter(|fault| !told_faults.contains(fault)).cloned().collect();
            told_faults.clone_from(&plan.faults);
            new_faults
        };
        for fault in new_faults {
            tracing::warn!("the sync plan of {} is at fault: {fault:?}", parts.folder.display());
        }
        let asked = crate::panic::lock(&self.full_listings_asked).clone();
        for item in plan.faults.iter().filter_map(|fault| if let PlanFault::FullListingNeeded(item) = fault { Some(*item) } else { None }).filter(|item| !asked.contains(item)) {
            match parts.store.call(move |store| store.item_store().request_full_listing(FullListingCause::PlanFault(item))).await {
                Ok(()) => {
                    crate::panic::lock(&self.full_listings_asked).insert(item);
                    parts.poll.refresh_after_full_listing_request();
                }
                // Not kept as asked: the next plan that answers the fault asks again.
                Err(error) => tracing::warn!("cannot ask for a full listing for item {item}: {error}"),
            }
        }
    }
}
