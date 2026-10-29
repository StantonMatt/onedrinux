//! Restore runs on the mount's write thread, with the same no-replace disk operations
//! and conditional row commits as normal sync. A collision leaves the batch held.
// test: held_removals_restore_offline_and_refuse_a_replacement
// test: held_rewrites_keep_local_bytes_without_uploading_the_copy
// test: a_restore_with_an_unsynced_record_keeps_the_retained_copy_and_hold
use super::{FolderStepParts, ItemMount};
use crate::change_guard::{generation, Change, Kind};
use crate::mount::item_announcements::{describe_move, ItemAnnouncement};
use crate::mount::mount_take_down::TakeDownCause;
use konedrive_tree::{
    ConflictRow, ContentVersion, Item, ItemKind, ItemPlace, ItemVersions, LocalItemId,
    PlaceChangeRecord, StepCommitOutcome,
};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

impl ItemMount {
    #[cfg(test)]
    pub(crate) async fn restore_guard_change(&self, change: Change) -> Result<bool, String> {
        self.restore_guard_change_taking_move(change, false).await
    }
    pub(crate) async fn restore_guard_change_taking_move(
        &self,
        change: Change,
        take_move: bool,
    ) -> Result<bool, String> {
        self.restore_guard_change_preparing(change, take_move, prepare_retained).await
    }
    async fn restore_guard_change_preparing(
        &self, change: Change, take_move: bool,
        prepare: impl FnOnce(&crate::mount::backing_directory::BackingDirectory, Option<&crate::change_guard::ChangeGuard>, LocalItemId) -> Result<Option<VerifiedRetained>, String> + Send + 'static,
    ) -> Result<bool, String> {
        let parts = self.folder_step_parts.clone();
        let backing = Arc::clone(&self.backing_directory);
        let thread = Arc::clone(&parts.write_operation_thread);
        // Full-file verification must not occupy the serialized mutation thread or
        // tree lock. The final job rechecks inode, size and nanosecond timestamps.
        let verification_backing = Arc::clone(&backing);
        let verification_guard = parts.place_changes.change_guard.clone();
        let id = LocalItemId::new(change.item);
        let retained = if change.kind == Kind::Removal {
            tokio::task::spawn_blocking(move || prepare(&verification_backing, verification_guard.as_deref(), id))
                .await.map_err(|e| e.to_string())??
        } else { None };
        thread
            .run_and_wait(move || restore(&parts, &backing, &change, take_move, retained))
            .await
            .map_err(|e| e.to_string())?
    }
}
fn restore(
    parts: &FolderStepParts,
    backing: &crate::mount::backing_directory::BackingDirectory,
    change: &Change,
    take_move: bool,
    verified: Option<VerifiedRetained>,
) -> Result<bool, String> {
    let fail = |e: &dyn std::fmt::Display| e.to_string();
    let id = LocalItemId::new(change.item);
    let Some(guard) = &parts.place_changes.change_guard else {
        return Err("change guard is not attached".into());
    };
    if change.kind == Kind::Upload || change.item < 0 {
        return Ok(true);
    }
    if parts.item_handles.is_open_for_writing(id) {
        return Ok(false);
    }
    let read = parts
        .store
        .call_blocking(move |store| {
            let items = store.item_store();
            let reader = items.reader();
            let Some(row) = reader.find_item(id)? else {
                return Ok(None);
            };
            let path = reader.find_path(id)?;
            let remote_place = row.onedrive.as_ref().and_then(|v| v.place.clone());
            let destination = match &remote_place {
                Some(place) => reader
                    .find_path(place.parent_local_item_id)?
                    .map(|p| p.join(&place.name)),
                None => None,
            };
            Ok(Some((row, path, destination, remote_place)))
        })
        .map_err(|e| fail(&e))?;
    let Some((row, path, destination, remote_place)) = read else {
        return restore_retained_local(parts, backing, guard, change);
    };
    let _readers = matches!(change.kind, Kind::Rewrite | Kind::Removal).then(|| crate::panic::lock(&parts.item_handles.reader_gate));
    if _readers.is_some() && parts.item_handles.is_open_for_reading(id) { return Ok(false); }
    let planned = ItemVersions::of(&row);
    if generation(&planned) != change.generation
        || match planned.local.as_ref().and_then(|l| l.content.as_ref()) {
            Some(ContentVersion::ChangedHere { change_count, .. }) => {
                change.write_count != Some(*change_count)
            }
            _ => false,
        }
    {
        return Ok(false);
    }
    match change.kind {
        Kind::Removal => {
            let (Some(destination), Some(place)) = (destination, remote_place) else {
                return Ok(false);
            };
            // Never use the displayed path to choose a replacement or create missing
            // parents: a removed parent is restored explicitly before its children.
            if let Some(v) = &verified {
                if !backing.read_metadata(&v.path).is_some_and(|m| identity(&m) == v.identity) { return Ok(false); }
            }
            let retained = verified.as_ref().map(|v| v.path.clone());
            let content = verified.as_ref().map(|v| v.content.clone());
            // Retained older/unknown bytes never become a rewrite of the newer cloud
            // baseline. Keep them local-only and restore the bound row cloud-only.
            let conflict = content.as_ref().is_some_and(|content| row.onedrive.as_ref().and_then(|r| r.content.as_ref()) != Some(content));
            let copy = if conflict {
                let copy_place = free_copy_place(parts, backing, &destination, &place)?;
                Some((destination.with_file_name(&copy_place.name), copy_place))
            } else { None };
            let disk_destination = copy.as_ref().map_or(&destination, |(path, _)| path).clone();
            let mut record = None;
            if retained.is_some() {
                let place_for_record = copy.as_ref().map_or(&place, |(_, place)| place).clone();
                let owner = if conflict { konedrive_tree::MadeEntryOwner::NewItem { replaced: None } } else { konedrive_tree::MadeEntryOwner::Item(id) };
                record = Some(
                    parts
                        .store
                        .call_blocking(move |s| {
                            let record = s.item_store()
                                .record_place_change(&PlaceChangeRecord::MakeEntry {
                                    owner,
                                    place: place_for_record,
                                    kind: ItemKind::File,
                                })?;
                            s.sync_write_ahead_log()?;
                            Ok(record)
                        })
                        .map_err(|e| fail(&e))?,
                );
            }
            let disk = parts.place_changes.begin();
            let undo = match &retained {
                Some(from) => Some(move_on_record(parts, &disk, from, &disk_destination, record)?),
                None => None,
            };
            if let Some(from) = &retained {
                if let Err(error) = sync_parents(backing, from, &disk_destination) {
                    if let Some(undo) = undo {
                        undo_restore(parts, &disk, undo)?;
                    }
                    forget_record(parts, record)?;
                    return Err(error);
                }
            }
            let copy_data = copy.as_ref().map(|(path, place)| (place.clone(), content.clone().unwrap(), ConflictRow {
                at: crate::clock::unix_now(), original: parts.folder.join(&destination).display().to_string(), kept_copy: parts.folder.join(path).display().to_string(),
            }));
            let notice_guard = Arc::clone(guard);
            let notice_change = change.clone();
            let held_path = disk_destination.display().to_string();
            let committed = parts
                .store
                .call_blocking(move |s| {
                    s.item_store().commit_guard_restore_removal_noticed(&planned, if conflict { None } else { content }, copy_data, record, move |rows| {
                        if let Some(copy) = rows.get(1) {
                            notice_guard.keep_local(copy, held_path, crate::clock::unix_now()).map_err(guard_error)?;
                        }
                        notice_guard.record_restore(&[notice_change], rows).map_err(guard_error)
                    })
                })
                .map_err(|e| fail(&e));
            if !matches!(committed, Ok(StepCommitOutcome::Written)) {
                if let Some(undo) = undo {
                    undo_restore(parts, &disk, undo)?;
                }
                disk.end();
                forget_record(parts, record)?;
                return committed.map(|_| false);
            }
            disk.end();
            sync_restored_row(parts, &disk_destination)?;
            guard.forget_retained(id)?;
            let mut announcements = vec![ItemAnnouncement::Appeared { item: id, place }];
            if let Some((copy_path, copy_place)) = copy {
                if let Some(row) = parts.store.call_blocking(move |s| s.item_store().reader().find_item_by_path(&copy_path)).map_err(|e| e.to_string())? {
                    announcements.push(ItemAnnouncement::Appeared { item: row.local_item_id, place: copy_place });
                }
            }
            parts.inotify_bridge.announce(announcements);
            Ok(true)
        }
        Kind::Move => {
            let (Some(from), Some(to), Some(place), Some(former)) = (
                path,
                destination,
                remote_place,
                row.local.as_ref().and_then(|v| v.place.clone()),
            ) else {
                return Ok(false);
            };
            let on_disk = backing.read_metadata(&from).is_some()
                && (row.kind == ItemKind::Directory
                    || row.content_presence == konedrive_tree::ContentPresence::OnDisk);
            let record = if on_disk {
                let (from, to) = (former.clone(), place.clone());
                Some(
                    parts
                        .store
                        .call_blocking(move |s| {
                            let record = s.item_store()
                                .record_place_change(&PlaceChangeRecord::MoveEntry {
                                    item: id,
                                    from,
                                    to,
                                })?;
                            s.sync_write_ahead_log()?;
                            Ok(record)
                        })
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            let disk = parts.place_changes.begin();
            let undo = if on_disk {
                Some(move_on_record(parts, &disk, &from, &to, record)?)
            } else {
                None
            };
            if on_disk {
                if let Err(error) = sync_parents(backing, &from, &to) {
                    if let Some(undo) = undo {
                        undo_restore(parts, &disk, undo)?;
                    }
                    forget_record(parts, record)?;
                    return Err(error);
                }
            }
            let notice_guard = Arc::clone(guard);
            let notice_change = change.clone();
            let outcome = parts
                .store
                .call_blocking(move |s| {
                    s.item_store().commit_guard_restore_move_noticed(&planned, record, move |row| {
                        notice_guard.record_restore(&[notice_change], std::slice::from_ref(row)).map_err(guard_error)
                    })
                })
                .map_err(|e| fail(&e));
            if !matches!(outcome, Ok(StepCommitOutcome::Written)) {
                if let Some(undo) = undo {
                    undo_restore(parts, &disk, undo)?;
                }
                disk.end();
                forget_record(parts, record)?;
                return outcome.map(|_| false);
            }
            disk.end();
            sync_restored_row(parts, &to)?;
            parts
                .inotify_bridge
                .announce(describe_move(id, row.kind, former, place));
            Ok(true)
        }
        Kind::Rewrite => {
            restore_rewrite(parts, backing, guard, change, row, planned, path, take_move)
        }
        Kind::Upload => Ok(true),
    }
}
fn restore_rewrite(
    parts: &FolderStepParts,
    backing: &crate::mount::backing_directory::BackingDirectory,
    _guard: &crate::change_guard::ChangeGuard,
    change: &Change,
    row: Item,
    planned: ItemVersions,
    path: Option<PathBuf>,
    take_move: bool,
) -> Result<bool, String> {
    let Some(path) = path else { return Ok(false) };
    let Some(place) = row.local.as_ref().and_then(|v| v.place.as_ref()) else {
        return Ok(false);
    };
    let id = row.local_item_id;
    // A private conflict name is never overwritten. The tree transaction independently
    // refuses a row at this name, even if a remote listing arrived after the disk check.
    let copy_place = free_copy_place(parts, backing, &path, place)?;
    let copy_path = path.with_file_name(&copy_place.name);
    let for_record = (place.clone(), copy_place.clone());
    let record = parts
        .store
        .call_blocking(move |s| {
            let record = s.item_store()
                .record_place_change(&PlaceChangeRecord::MoveEntry {
                    item: id,
                    from: for_record.0,
                    to: for_record.1,
                })?;
            s.sync_write_ahead_log()?;
            Ok(record)
        })
        .map_err(|e| e.to_string())?;
    let disk = parts.place_changes.begin();
    let undo = move_on_record(parts, &disk, &path, &copy_path, Some(record))?;
    if let Err(error) = sync_parents(backing, &path, &copy_path) {
        undo_restore(parts, &disk, undo)?;
        forget_record(parts, Some(record))?;
        return Err(error);
    }
    let conflict = ConflictRow {
        at: crate::clock::unix_now(),
        original: parts.folder.join(&path).display().to_string(),
        kept_copy: parts.folder.join(&copy_path).display().to_string(),
    };
    let for_commit = copy_place.clone();
    let copy_guard = parts
        .place_changes
        .change_guard
        .clone()
        .ok_or("change guard is not attached")?;
    let held_path = copy_path.display().to_string();
    let notice_changes = copy_guard.restore_changes(change, take_move);
    let made = parts
        .store
        .call_blocking(move |s| {
            s.item_store().commit_guard_restore_rewrite_noticed(
                &planned,
                &for_commit,
                &conflict,
                Some(record),
                take_move,
                move |copy, original| {
                    copy_guard.keep_local(copy, held_path, crate::clock::unix_now()).map_err(guard_error)?;
                    copy_guard.record_restore(&notice_changes, &[copy.clone(), original.clone()]).map_err(guard_error)
                },
            )
        })
        .map_err(|e| e.to_string());
    let made = match made {
        Ok(Some(made)) => made,
        result => {
            undo_restore(parts, &disk, undo)?;
            disk.end();
            forget_record(parts, Some(record))?;
            return result.map(|_| false);
        }
    };
    disk.end();
    sync_restored_row(parts, &copy_path)?;
    parts.inotify_bridge.announce(vec![
        ItemAnnouncement::Appeared {
            item: made,
            place: copy_place,
        },
        ItemAnnouncement::ContentChanged {
            item: id,
            place: place.clone(),
        },
    ]);
    let _ = change;
    Ok(true)
}
/// A failed rollback leaves its place record for startup and stops the mount.
fn undo_restore(parts: &FolderStepParts, disk: &crate::mount::PlaceChange<'_>, undo: crate::mount::PlaceUndo) -> Result<(), String> {
    use crate::mount::PlaceUndo;
    let path = match &undo {
        PlaceUndo::Nothing => PathBuf::new(),
        PlaceUndo::RemoveMade { path, .. } | PlaceUndo::UnmarkTaken { path } | PlaceUndo::Restore { path, .. } | PlaceUndo::RestoreMark { path, .. } => path.clone(),
        PlaceUndo::MoveBack { to, .. } | PlaceUndo::MoveBackAndRestore { to, .. } => to.clone(),
    };
    disk.undo(undo).map_err(|error| {
        parts.mount_take_down.ask(TakeDownCause::UndoFailed { path });
        error.to_string()
    })
}

/// A restored row must survive before its hold or retained-copy record can go.
/// A committed row is never undone on a barrier failure: leave its bytes and stop.
fn sync_restored_row(parts: &FolderStepParts, path: &Path) -> Result<(), String> {
    parts.store.sync_write_ahead_log().map_err(|error| {
        parts.mount_take_down.ask(TakeDownCause::BackingFileBehindRow { path: path.to_owned() });
        error.to_string()
    })
}

fn sync_parents(
    backing: &crate::mount::backing_directory::BackingDirectory,
    from: &Path,
    to: &Path,
) -> Result<(), String> {
    for path in [from, to] {
        let parent = path.parent().unwrap_or(Path::new(""));
        std::fs::File::open(backing.resolve_path(parent))
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn forget_record(
    parts: &FolderStepParts,
    record: Option<konedrive_tree::PlaceChangeRecordId>,
) -> Result<(), String> {
    if let Some(record) = record {
        parts
            .store
            .call_blocking(move |s| s.item_store().forget_place_change_record(record))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn move_on_record(
    parts: &FolderStepParts,
    disk: &crate::mount::PlaceChange<'_>,
    from: &Path,
    to: &Path,
    record: Option<konedrive_tree::PlaceChangeRecordId>,
) -> Result<crate::mount::PlaceUndo, String> {
    match disk.move_entry(from, to, &vec![]) {
        Ok(undo) => Ok(undo),
        Err(error) => {
            match &error {
                crate::mount::PlaceRefusal::NotPutBack { path, .. } => parts.mount_take_down.ask(TakeDownCause::UndoFailed { path: path.clone() }),
                _ => forget_record(parts, record)?,
            }
            Err(format!("restore place is held: {error:?}"))
        }
    }
}

/// An unsent file's row can disappear on unlink. Its retained bytes still have an
/// original path in the ledger; rescue them as held local-only work, including any
/// unbound parent directories that the same recursive removal took away.
fn restore_retained_local(
    parts: &FolderStepParts,
    backing: &crate::mount::backing_directory::BackingDirectory,
    guard: &crate::change_guard::ChangeGuard,
    change: &Change,
) -> Result<bool, String> {
    let old_id = LocalItemId::new(change.item);
    let Some(retained) = guard.retained(old_id) else {
        return Ok(false);
    };
    let Some(metadata) = backing.read_metadata(&retained).filter(|m| m.is_file()) else {
        return Ok(false);
    };
    let destination = PathBuf::from(&change.path);
    if destination.as_os_str().is_empty()
        || destination
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Ok(false);
    }
    let mut directory = LocalItemId::ROOT;
    let mut parent_path = PathBuf::new();
    for name in destination.parent().unwrap_or(Path::new("")).components() {
        parent_path.push(name.as_os_str());
        let look = parent_path.clone();
        let existing = parts
            .store
            .call_blocking(move |s| s.item_store().reader().find_item_by_path(&look))
            .map_err(|e| e.to_string())?;
        if let Some(existing) = existing {
            if existing.kind != ItemKind::Directory {
                return Ok(false);
            }
            directory = existing.local_item_id;
            continue;
        }
        let place = ItemPlace {
            parent_local_item_id: directory,
            name: name.as_os_str().to_owned(),
        };
        directory = rescue_entry(parts, backing, guard, &parent_path, place, None, None, None)?;
    }
    let place = ItemPlace {
        parent_local_item_id: directory,
        name: destination
            .file_name()
            .ok_or("retained copy has no filename")?
            .to_owned(),
    };
    let content = ContentVersion::ChangedHere {
        size: metadata.len(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec() as u32,
        change_count: 0,
    };
    rescue_entry(
        parts,
        backing,
        guard,
        &destination,
        place,
        Some(content),
        Some(&retained),
        Some(change.clone()),
    )?;
    guard.forget_retained(old_id)?;
    Ok(true)
}
fn rescue_entry(
    parts: &FolderStepParts,
    backing: &crate::mount::backing_directory::BackingDirectory,
    _guard: &crate::change_guard::ChangeGuard,
    path: &Path,
    place: ItemPlace,
    content: Option<ContentVersion>,
    retained: Option<&Path>,
    restored: Option<Change>,
) -> Result<LocalItemId, String> {
    if backing.read_metadata(path).is_some() {
        return Err("a newer local entry holds the restore path".into());
    }
    let kind = if content.is_some() {
        ItemKind::File
    } else {
        ItemKind::Directory
    };
    let for_record = place.clone();
    let record = parts
        .store
        .call_blocking(move |s| {
            let record = s.item_store()
                .record_place_change(&PlaceChangeRecord::MakeEntry {
                    owner: konedrive_tree::MadeEntryOwner::NewItem { replaced: None },
                    place: for_record,
                    kind,
                })?;
            s.sync_write_ahead_log()?;
            Ok(record)
        })
        .map_err(|e| e.to_string())?;
    let disk = parts.place_changes.begin();
    let undo = match retained {
        Some(from) => move_on_record(parts, &disk, from, path, Some(record))?,
        None => match disk.make_entry(path, crate::mount::EntryKind::Directory, &vec![]) {
            Ok((_, undo)) => undo,
            Err(error) => {
                forget_record(parts, Some(record))?;
                return Err(format!("cannot restore parent: {error:?}"));
            }
        },
    };
    let from = retained.unwrap_or(path);
    if let Err(error) = sync_parents(backing, from, path) {
        undo_restore(parts, &disk, undo)?;
        forget_record(parts, Some(record))?;
        return Err(error);
    }
    let for_commit = place.clone();
    let copy_guard = parts
        .place_changes
        .change_guard
        .clone()
        .ok_or("change guard is not attached")?;
    let held_path = path.display().to_string();
    let committed = parts.store.call_blocking(move |s| {
        s.item_store().commit_guard_rescue_noticed(
            kind,
            &for_commit,
            content.as_ref(),
            record,
            move |row| {
                copy_guard.keep_local(row, held_path, crate::clock::unix_now()).map_err(guard_error)?;
                if let Some(change) = restored { copy_guard.record_restore(&[change], std::slice::from_ref(row)).map_err(guard_error)?; }
                Ok(())
            },
        )
    });
    let made = match committed {
        Ok(made) => made,
        Err(error) => {
            undo_restore(parts, &disk, undo)?;
            forget_record(parts, Some(record))?;
            return Err(error.to_string());
        }
    };
    disk.end();
    sync_restored_row(parts, path)?;
    parts
        .inotify_bridge
        .announce(vec![ItemAnnouncement::Appeared { item: made, place }]);
    Ok(made)
}

fn verified_retained_content(
    backing: &crate::mount::backing_directory::BackingDirectory,
    path: &Path,
    snapshot: &ContentVersion,
) -> Result<bool, String> {
    use std::io::Read;
    let ContentVersion::Hashed {
        size,
        mtime,
        mtime_nsec,
        hash: Some(expected),
        ..
    } = snapshot
    else {
        return Ok(false);
    };
    let mut file = backing.open_to_read(path).map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if before.len() != *size
        || before.mtime() != *mtime
        || (*mtime_nsec != 0 && before.mtime_nsec() as u32 != *mtime_nsec)
    {
        return Ok(false);
    }
    let mut hash = konedrive_graph::quickxor::QuickXor::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    Ok(hash.finish_base64() == *expected
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec())
}

fn guard_error(error: String) -> konedrive_tree::TreeError { konedrive_tree::TreeError::Io(std::io::Error::other(error)) }

fn free_copy_place(parts: &FolderStepParts, backing: &crate::mount::backing_directory::BackingDirectory, path: &Path, place: &ItemPlace) -> Result<ItemPlace, String> {
    for attempt in 1..=100 {
        let name = konedrive_tree::make_conflict_copy_name(&place.name, "restored-local", attempt);
        let candidate = path.with_file_name(&name);
        if backing.read_metadata(&candidate).is_some() { continue; }
        if parts.store.call_blocking(move |s| s.item_store().reader().find_item_by_path(&candidate)).map_err(|e| e.to_string())?.is_none() {
            return Ok(ItemPlace { parent_local_item_id: place.parent_local_item_id, name });
        }
    }
    Err("no free conflict copy name for Restore".into())
}

type FileIdentity = (u64, u64, u64, i64, i64, i64, i64);
fn identity(m: &std::fs::Metadata) -> FileIdentity { (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()) }
struct VerifiedRetained { path: PathBuf, content: ContentVersion, identity: FileIdentity }
fn prepare_retained(backing: &crate::mount::backing_directory::BackingDirectory, guard: Option<&crate::change_guard::ChangeGuard>, id: LocalItemId) -> Result<Option<VerifiedRetained>, String> {
    let Some(guard) = guard else { return Ok(None) };
    let Some(path) = guard.retained(id) else { return Ok(None) };
    let Some(metadata) = backing.read_metadata(&path).filter(|m| m.is_file()) else { return Ok(None) };
    let mut content = ContentVersion::ChangedHere { size: metadata.len(), mtime: metadata.mtime(), mtime_nsec: metadata.mtime_nsec() as u32, change_count: 0 };
    if let Some(snapshot) = guard.retained_content(id) {
        if verified_retained_content(backing, &path, &snapshot)? { content = snapshot; }
    }
    if !backing.read_metadata(&path).is_some_and(|m| identity(&m) == identity(&metadata)) { return Err("retained copy changed during verification".into()); }
    Ok(Some(VerifiedRetained { path, content, identity: identity(&metadata) }))
}

#[cfg(test)]
mod tests;
