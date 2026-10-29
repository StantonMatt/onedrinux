//! Admission of program mutations at the dispatch boundary. Kept apart from the
//! operation bodies so it also covers write hydration and unsupported kernel requests.
// test: write_scope_refuses_every_mutating_dispatch_and_checks_both_rename_ends
// test: write_scope_keeps_an_unlinked_writers_admission_but_rechecks_named_files
// test: write_scope_reports_the_refused_endpoint_existing_folder_and_caller

use super::ItemWriteOperations;
use fuser::Errno;
use konedrive_tree::{ItemKind, LocalItemId};
use std::ffi::OsStr;
use std::path::{Component, Path};

impl ItemWriteOperations {
    /// Checks an existing item, or a name under a directory, without touching content.
    pub(in crate::mount) fn check_write_scope(&self, item: LocalItemId, name: Option<&OsStr>, pid: u32) -> Result<(), Errno> {
        self.check_write_places(&[(item, name)], pid)
    }

    /// One scope snapshot and one row transaction for both ends of a rename.
    pub(in crate::mount) fn check_write_places(&self, places: &[(LocalItemId, Option<&OsStr>)], pid: u32) -> Result<(), Errno> {
        self.check_write_places_inner(places, false, pid)
    }

    /// A writer was admitted at open. Once unlinked, its unnamed inode retains that
    /// admission; a file that still has a name must satisfy the current scope.
    pub(in crate::mount) fn check_writer_scope(&self, item: LocalItemId, pid: u32) -> Result<(), Errno> {
        self.check_write_places_inner(&[(item, None)], true, pid)
    }

    fn check_write_places_inner(&self, places: &[(LocalItemId, Option<&OsStr>)], unnamed_writer: bool, pid: u32) -> Result<(), Errno> {
        let scope = self.upload_rules.write_scope.get();
        if scope.unrestricted() {
            return Ok(());
        }
        let allowed = self
            .read_connections
            .read(|reader| {
                for (item, name) in places {
                    let Some(mut path) = reader.find_path(*item)? else {
                        if unnamed_writer && reader.find_item(*item)?.is_none_or(|row| row.local.as_ref().and_then(|local| local.place.as_ref()).is_none()) {
                            continue;
                        }
                        return Ok(false);
                    };
                    // Always report the containing folder. A rename/removal of a
                    // directory also changes its parent's namespace, so allowing the
                    // directory alone would not authorize retrying that operation.
                    let folder = if name.is_some() || reader.find_item(*item)?.is_some_and(|row| row.kind == ItemKind::Directory) {
                        path.clone()
                    } else {
                        path.parent().unwrap_or(Path::new("")).to_path_buf()
                    };
                    if let Some(name) = name {
                        let mut components = Path::new(name).components();
                        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
                            return Ok(false);
                        }
                        path.push(name);
                    }
                    if !scope.allows(&path) {
                        self.upload_rules.write_scope.refusals.record(path, folder, pid);
                        return Ok(false);
                    }
                }
                Ok(true)
            })
            .map_err(|_| Errno::EIO)?;
        if allowed {
            Ok(())
        } else {
            Err(Errno::EROFS)
        }
    }
}
