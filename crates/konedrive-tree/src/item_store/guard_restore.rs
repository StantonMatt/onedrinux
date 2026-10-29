//! Conditional row writes for a user's Restore of a held outgoing generation.
//! The caller preserves bytes and serializes local operations; these transactions
//! still compare every planned version so a newer listing cannot be overwritten.
// test: restoring_a_removed_generation_refuses_newer_remote_work
// test: restoring_a_rewrite_keeps_a_local_conflict_copy
use super::item_row::{find_item, write_item};
use super::place_change_records::forget_place_change_record_when_written;
use super::step_commit::{commit_show_item, StepCommitOutcome};
use super::step_record::add_conflicts_in;
use super::{make, ItemStore};
use crate::{
    ConflictRow, ContentPresence, ContentVersion, ItemKind, ItemPlace, ItemVersions, LocalItemId,
    PlaceChangeRecordId, TreeError,
};
use rusqlite::Transaction;

/// Short reasons for a failed held-change decision, suitable for a row's second line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GuardRestoreFailure {
    #[error("it is open in another program")]
    Open,
    #[error("it was changed on this computer after it was held")]
    LocalChanged,
    #[error("it was changed in OneDrive after it was held")]
    RemoteChanged,
    #[error("it changed after it was held")]
    Changed,
    #[error("a file of the same name is in its place")]
    Occupied,
    #[error("the folder it belongs in is missing or has moved")]
    ParentMissing,
    #[error("its local copy is no longer available")]
    LocalMissing,
    #[error("there is no free name for a copy of the local file")]
    NoCopyName,
    #[error("this account's folder is not mounted")]
    Unmounted,
    #[error("this computer couldn't save the restored file")]
    Storage,
    #[error("this account's folder stopped before it could be restored")]
    Stopped,
}

/// A listing can run while the backing operation runs. Diagnose its refusal in the
/// same transaction that would have restored the row, before writing anything.
fn restore_row(tx: &Transaction<'_>, planned: &ItemVersions) -> Result<crate::Item, TreeError> {
    let row = find_item(tx, planned.local_item_id)?.ok_or(GuardRestoreFailure::LocalMissing)?;
    if !planned.are_those_of(&row) {
        return Err(if planned.local != row.local || planned.unsent_since != row.unsent_since {
            GuardRestoreFailure::LocalChanged
        } else if !planned.are_onedrive_and_synced_those_of(&row) {
            GuardRestoreFailure::RemoteChanged
        } else { GuardRestoreFailure::LocalChanged }.into());
    }
    Ok(row)
}

fn restore_place(tx: &Transaction<'_>, item: LocalItemId, place: &ItemPlace) -> Result<(), TreeError> {
    let parent = find_item(tx, place.parent_local_item_id)?;
    if !parent.is_some_and(|p| p.kind == ItemKind::Directory && p.local.as_ref().is_some_and(|l| l.place.is_some() || p.local_item_id == LocalItemId::ROOT)) {
        return Err(GuardRestoreFailure::ParentMissing.into());
    }
    if super::find_item_at(tx, place.parent_local_item_id, &place.name)?.is_some_and(|r| r.local_item_id != item) {
        return Err(GuardRestoreFailure::Occupied.into());
    }
    // The old parent may have moved inside the item since: no longer a place for it.
    match super::check_can_hold(tx, place.parent_local_item_id, item) {
        Err(TreeError::Refused(_)) => Err(GuardRestoreFailure::ParentMissing.into()),
        result => result,
    }
}

fn removal_row(tx: &Transaction<'_>, planned: &ItemVersions) -> Result<(), TreeError> {
    let row = restore_row(tx, planned)?;
    let place = row.onedrive.as_ref().and_then(|v| v.place.as_ref()).ok_or(GuardRestoreFailure::RemoteChanged)?;
    restore_place(tx, row.local_item_id, place)
}

impl ItemStore<'_> {
    /// Restore a removed cloud row, optionally with retained local content. A changed
    /// local copy is deliberately unverified and will be hashed by the ordinary plan.
    pub fn commit_guard_restore_removal(
        &mut self,
        planned: &ItemVersions,
        retained: Option<ContentVersion>,
        record: Option<PlaceChangeRecordId>,
    ) -> Result<StepCommitOutcome, TreeError> {
        self.write(|tx| {
            removal_row(tx, planned)?;
            let outcome = commit_show_item(tx, planned, false)?;
            if outcome == StepCommitOutcome::Written {
                if let Some(content) = retained {
                    let mut row = find_item(tx, planned.local_item_id)?
                        .ok_or_else(|| super::refuse("restored item went".into()))?;
                    if row.synced_content.as_ref() != Some(&content) { return Err(GuardRestoreFailure::RemoteChanged.into()); }
                    if let Some(local) = &mut row.local {
                        local.content = Some(content);
                    }
                    row.content_presence = ContentPresence::OnDisk;
                    write_item(tx, &row)?;
                }
            }
            forget_place_change_record_when_written(tx, record, outcome)
        })
    }
    /// Restore a removal, optionally keeping retained bytes as an unbound conflict
    /// copy. `notice` persists holds and exact completion evidence before SQL commits.
    pub fn commit_guard_restore_removal_noticed(
        &mut self,
        planned: &ItemVersions,
        retained: Option<ContentVersion>,
        copy: Option<(ItemPlace, ContentVersion, ConflictRow)>,
        record: Option<PlaceChangeRecordId>,
        notice: impl FnOnce(&[crate::Item]) -> Result<(), TreeError>,
    ) -> Result<StepCommitOutcome, TreeError> {
        self.write(|tx| {
            removal_row(tx, planned)?;
            let outcome = commit_show_item(tx, planned, false)?;
            if outcome == StepCommitOutcome::Written {
                let mut row = find_item(tx, planned.local_item_id)?.ok_or_else(|| super::refuse("restored item went".into()))?;
                if let Some(content) = retained {
                    // Only content identical to the current remote baseline may occupy
                    // the bound row. Older/unknown bytes belong in `copy` instead.
                    if row.synced_content.as_ref() != Some(&content) { return Err(GuardRestoreFailure::RemoteChanged.into()); }
                    row.local.as_mut().unwrap().content = Some(content);
                    row.content_presence = ContentPresence::OnDisk;
                    write_item(tx, &row)?;
                }
                let mut rows = vec![row];
                if let Some((place, content, conflict)) = copy {
                    let made = make(tx, ItemKind::File, &place, Some(&content), false, None)?;
                    rows.push(find_item(tx, made)?.unwrap());
                    add_conflicts_in(tx, &[conflict])?;
                }
                notice(&rows)?;
            }
            forget_place_change_record_when_written(tx, record, outcome)
        })
    }
    /// Take back a move and persist completion evidence in the same SQL transaction.
    pub fn commit_guard_restore_move_noticed(
        &mut self, planned: &ItemVersions, record: Option<PlaceChangeRecordId>,
        notice: impl FnOnce(&crate::Item) -> Result<(), TreeError>,
    ) -> Result<StepCommitOutcome, TreeError> {
        self.write(|tx| {
            let row = restore_row(tx, planned)?;
            let place = row.onedrive.as_ref().and_then(|v| v.place.as_ref()).ok_or(GuardRestoreFailure::RemoteChanged)?;
            restore_place(tx, row.local_item_id, place)?;
            let outcome = super::step_commit::commit_move_to_onedrive_place(tx, planned, false)?;
            if outcome == StepCommitOutcome::Written { notice(&find_item(tx, planned.local_item_id)?.unwrap())?; }
            forget_place_change_record_when_written(tx, record, outcome)
        })
    }

    /// Split local bytes into a new, unbound conflict row and let the original row take
    /// the cloud content. The new row must be held by the caller before waking sync.
    pub fn commit_guard_restore_rewrite(
        &mut self,
        planned: &ItemVersions,
        copy: &ItemPlace,
        conflict: &ConflictRow,
        record: Option<PlaceChangeRecordId>,
    ) -> Result<Option<LocalItemId>, TreeError> {
        self.write(|tx| restore_rewrite(tx, planned, copy, conflict, record, false))
    }
    /// Persist the new copy's hold before its row becomes visible. Failure of that
    /// separate state write rolls this SQL transaction back; a restart cannot upload
    /// a copy created by Restore without its local-only hold.
    pub fn commit_guard_restore_rewrite_noticed(
        &mut self,
        planned: &ItemVersions,
        copy: &ItemPlace,
        conflict: &ConflictRow,
        record: Option<PlaceChangeRecordId>,
        take_move: bool,
        notice: impl FnOnce(&crate::Item, &crate::Item) -> Result<(), TreeError>,
    ) -> Result<Option<LocalItemId>, TreeError> {
        self.write(|tx| {
            let made = restore_rewrite(tx, planned, copy, conflict, record, take_move)?;
            if let Some(id) = made {
                let row = find_item(tx, id)?
                    .ok_or_else(|| super::refuse("conflict copy went before its hold".into()))?;
                notice(&row, &find_item(tx, planned.local_item_id)?.unwrap())?;
            }
            Ok(made)
        })
    }

    /// The same ordering for rescuing retained bytes whose unbound row disappeared.
    pub fn commit_guard_rescue_noticed(
        &mut self,
        kind: ItemKind,
        place: &ItemPlace,
        content: Option<&ContentVersion>,
        record: PlaceChangeRecordId,
        notice: impl FnOnce(&crate::Item) -> Result<(), TreeError>,
    ) -> Result<LocalItemId, TreeError> {
        self.write(|tx| {
            let made = make(tx, kind, place, content, false, None)?;
            super::place_change_records::forget_place_change_record(tx, record)?;
            let row = find_item(tx, made)?
                .ok_or_else(|| super::refuse("rescued copy went before its hold".into()))?;
            notice(&row)?;
            Ok(made)
        })
    }

    /// The same Restore when the user selected this file's pending move as well.
    pub fn commit_guard_restore_rewrite_and_move(
        &mut self,
        planned: &ItemVersions,
        copy: &ItemPlace,
        conflict: &ConflictRow,
        record: Option<PlaceChangeRecordId>,
    ) -> Result<Option<LocalItemId>, TreeError> {
        self.write(|tx| restore_rewrite(tx, planned, copy, conflict, record, true))
    }
}
fn restore_rewrite(
    tx: &Transaction<'_>,
    planned: &ItemVersions,
    copy: &ItemPlace,
    conflict: &ConflictRow,
    record: Option<PlaceChangeRecordId>,
    take_move: bool,
) -> Result<Option<LocalItemId>, TreeError> {
    let mut row = restore_row(tx, planned)?;
    restore_place(tx, row.local_item_id, copy)?;
    let Some(local_content) = row.local.as_ref().map(|v| v.content.clone()) else {
        return Ok(None);
    };
    let Some(remote_content) = row.onedrive.as_ref().and_then(|v| v.content.clone()) else {
        return Ok(None);
    };
    if take_move {
        let Some(remote_place) = row.onedrive.as_ref().and_then(|v| v.place.clone()) else {
            return Ok(None);
        };
        restore_place(tx, row.local_item_id, &remote_place)?;
        if let Some(local) = &mut row.local {
            local.place = Some(remote_place.clone());
        }
        row.synced_place = Some(remote_place);
    }
    let made = make(
        tx,
        ItemKind::File,
        copy,
        local_content.as_ref(),
        false,
        None,
    )?;
    if let Some(local) = &mut row.local {
        local.content = Some(remote_content.clone());
    }
    row.synced_content = Some(remote_content);
    row.content_presence = ContentPresence::NotOnDisk;
    row.content_before_write = None;
    write_item(tx, &row)?;
    add_conflicts_in(tx, std::slice::from_ref(conflict))?;
    forget_place_change_record_when_written(tx, record, StepCommitOutcome::Written)?;
    Ok(Some(made))
}
#[cfg(test)]
mod tests;
