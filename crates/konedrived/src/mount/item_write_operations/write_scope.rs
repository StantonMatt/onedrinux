//! Admission of program mutations at the dispatch boundary. Kept apart from the
//! operation bodies so it also covers write hydration and unsupported kernel requests.
// test: write_scope_refuses_every_mutating_dispatch_and_checks_both_rename_ends
// test: write_scope_keeps_an_unlinked_writers_admission_but_rechecks_named_files

use super::ItemWriteOperations;
use fuser::Errno;
use konedrive_tree::LocalItemId;
use std::ffi::OsStr;
use std::path::{Component, Path};

impl ItemWriteOperations {
    /// Checks an existing item, or a name under a directory, without touching content.
    pub(in crate::mount) fn check_write_scope(&self, item: LocalItemId, name: Option<&OsStr>) -> Result<(), Errno> {
        self.check_write_places(&[(item, name)])
    }

    /// One scope snapshot and one row transaction for both ends of a rename.
    pub(in crate::mount) fn check_write_places(&self, places: &[(LocalItemId, Option<&OsStr>)]) -> Result<(), Errno> {
        self.check_write_places_inner(places, false)
    }

    /// A writer was admitted at open. Once unlinked, its unnamed inode retains that
    /// admission; a file that still has a name must satisfy the current scope.
    pub(in crate::mount) fn check_writer_scope(&self, item: LocalItemId) -> Result<(), Errno> {
        self.check_write_places_inner(&[(item, None)], true)
    }

    fn check_write_places_inner(&self, places: &[(LocalItemId, Option<&OsStr>)], unnamed_writer: bool) -> Result<(), Errno> {
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
                    if let Some(name) = name {
                        let mut components = Path::new(name).components();
                        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
                            return Ok(false);
                        }
                        path.push(name);
                    }
                    if !scope.allows(&path) {
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
