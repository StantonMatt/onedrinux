//! Durable restore completion evidence and ownership of retained-copy cleanup.
// test: committed_restore_receipts_survive_restart_without_approving_newer_work
use super::{
    generation, ledger::RestoreReceipt, write_count, Change, ChangeGuard, Disposition, Kind,
};
use konedrive_tree::{Item, ItemReader, ItemVersions, LocalItemId, TreeError};
use std::path::PathBuf;

impl ChangeGuard {
    pub(crate) fn restore_changes(&self, change: &Change, take_move: bool) -> Vec<Change> {
        let inner = crate::panic::lock(&self.inner);
        let mut changes = vec![change.clone()];
        if take_move {
            if let Some(c) = inner.ledger.current.get(&format!("{}:move", change.item)) {
                changes.push(c.clone());
            }
        }
        changes
    }

    /// Called inside the SQL transaction, after conditional row writes. A crash before
    /// commit leaves the old rows, which cannot match this completion evidence.
    pub(crate) fn record_restore(&self, changes: &[Change], rows: &[Item]) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let rows: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.local_item_id.as_i64(),
                    generation(&ItemVersions::of(r)),
                    write_count(&ItemVersions::of(r)),
                )
            })
            .collect();
        for c in changes {
            inner.ledger.restored.insert(
                c.key(),
                RestoreReceipt {
                    revision: c.revision,
                    rows: rows.clone(),
                },
            );
        }
        self.save(&mut inner)
    }
    /// Run after place-change recovery and before the first plan. Uncommitted receipts
    /// stay held. A committed receipt only settles its own original decision.
    pub(crate) fn reconcile_restores(&self, items: &ItemReader<'_>) -> Result<(), TreeError> {
        let mut inner = crate::panic::lock(&self.inner);
        let mut completed = Vec::new();
        for (key, receipt) in &inner.ledger.restored {
            if !inner
                .ledger
                .current
                .get(key)
                .is_some_and(|c| c.revision == receipt.revision)
            {
                continue;
            }
            let mut matches = !receipt.rows.is_empty();
            for (id, expected, writes) in &receipt.rows {
                matches &= items.find_item(LocalItemId::new(*id))?.is_some_and(|r| {
                    let versions = ItemVersions::of(&r);
                    generation(&versions) == *expected
                        && write_count(&versions).is_none_or(|n| *writes == Some(n))
                });
            }
            if matches {
                completed.push(key.clone());
            }
        }
        for key in &completed {
            if let Some(c) = inner.ledger.current.remove(key) {
                inner.ledger.retained.remove(&c.item);
            }
            inner.ledger.restored.remove(key);
        }
        if !completed.is_empty() {
            for kind in Kind::ALL {
                if !inner.ledger.current.values().any(|c| {
                    c.kind == kind
                        && matches!(c.disposition, Disposition::Held | Disposition::Restoring)
                }) {
                    inner.ledger.latched.remove(&kind);
                    inner.ledger.hold_started.remove(&kind);
                }
            }
            self.save(&mut inner)
                .map_err(|e| TreeError::Io(std::io::Error::other(e)))?;
            self.notify(&inner);
        }
        Ok(())
    }
    pub(crate) fn cleanup_reference(
        &self,
        change: &Change,
    ) -> Option<konedrive_tree::OneDriveItemReference> {
        let inner = crate::panic::lock(&self.inner);
        let (drive, item, _) = inner.ledger.cleanup_refs.get(&change.item)?;
        Some(konedrive_tree::OneDriveItemReference {
            drive_id: konedrive_tree::DriveId::new(drive)?,
            item_id: konedrive_tree::OneDriveItemId::new(item),
        })
    }
    /// A listing can retire a leftover queue entry before its sender runs. Known
    /// disappearance, adoption or a newer remote eTag also retires its guard entry.
    pub(crate) fn reconcile_cleanups(&self, items: &ItemReader<'_>) -> Result<(), TreeError> {
        let mut inner = crate::panic::lock(&self.inner);
        let mut settled = Vec::new();
        for (id, (drive, item, version)) in &inner.ledger.cleanup_refs {
            let drive_id = konedrive_tree::DriveId::new(drive)
                .ok_or_else(|| TreeError::Io(std::io::Error::other("invalid cleanup drive id")))?;
            let reference = konedrive_tree::OneDriveItemReference {
                drive_id,
                item_id: konedrive_tree::OneDriveItemId::new(item),
            };
            if let Some(row) = items.find_bound_item(&reference)? {
                if row.onedrive.is_none()
                    || row.local.is_some()
                    || row.synced_place.is_some()
                    || row.onedrive_etag.as_ref().is_some_and(|tag| tag != version)
                {
                    settled.push(*id);
                }
            }
        }
        for id in &settled {
            inner.ledger.cleanup_refs.remove(id);
            inner.ledger.current.remove(&format!("{id}:removal"));
        }
        if !settled.is_empty() {
            self.save(&mut inner)
                .map_err(|e| TreeError::Io(std::io::Error::other(e)))?;
            self.notify(&inner);
        }
        Ok(())
    }
    /// A confirmed remote removal owns retained cleanup even if the sender died
    /// before recording Sent. Unbound unique bytes require an explicit Allow.
    pub(crate) fn retained_cleanup(
        &self,
        items: &ItemReader<'_>,
    ) -> Result<Vec<(LocalItemId, PathBuf)>, TreeError> {
        let inner = crate::panic::lock(&self.inner);
        let mut clean = Vec::new();
        if inner.failed.is_some() {
            return Ok(clean);
        }
        for (id, retained) in &inner.ledger.retained {
            let row = items.find_item(LocalItemId::new(*id))?;
            if row
                .as_ref()
                .is_some_and(|r| r.local.is_some() || r.onedrive.is_some())
            {
                continue;
            }
            let allowed = inner
                .ledger
                .current
                .get(&format!("{id}:removal"))
                .is_some_and(|c| c.disposition == Disposition::Allowed);
            if retained.cleanup || (!retained.bound && allowed) {
                clean.push((LocalItemId::new(*id), PathBuf::from(&retained.path)));
            }
        }
        Ok(clean)
    }
    /// A successful DELETE or authoritative 404 owns cleanup before the SQL commit.
    /// If that commit fails, cleanup still waits until the row confirms absence.
    pub(crate) fn confirm_removals(&self, ids: &[LocalItemId]) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        let mut changed = false;
        for id in ids {
            if let Some(retained) = inner.ledger.retained.get_mut(&id.as_i64()) {
                changed |= !retained.cleanup;
                retained.cleanup = true;
            }
        }
        if changed {
            self.save(&mut inner)?;
        }
        Ok(())
    }
    pub(crate) fn cleaned_retained(&self, item: LocalItemId) -> Result<(), String> {
        let mut inner = crate::panic::lock(&self.inner);
        inner.ledger.retained.remove(&item.as_i64());
        inner.ledger.current.remove(&format!("{item}:removal"));
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok(())
    }
}
