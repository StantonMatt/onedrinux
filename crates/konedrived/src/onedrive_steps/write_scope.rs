//! Defence in depth at sender admission. Both the local place and the remote namespace
//! are checked; a pending move cannot launder an out-of-scope remote source.
// test: write_scope_sender_checks_every_step_and_remote_rename_sources
// test: write_scope_sender_allows_in_scope_steps_and_checks_the_actual_destination
// test: write_scope_rechecks_current_locations_after_a_step_was_planned

use super::OneDriveSteps;
use crate::config::{WriteScope, WriteScopeSource};
use crate::sync_parts::step_outcome::{StepOutcome, StepRefusal};
use konedrive_tree::{ItemPlace, ItemReader, ItemVersions, LocalItemId, OneDriveStep, TreeError};
use std::path::PathBuf;

/// A cloud path walks cloud parents only. Using local parents here would let a pending
/// directory move pull a cloud item outside the allowlist into it.
fn remote_place(reader: &ItemReader<'_>, place: &ItemPlace) -> Result<Option<PathBuf>, TreeError> {
    let mut names = vec![place.name.clone()];
    let mut parent = place.parent_local_item_id;
    for _ in 0..1024 {
        if parent == LocalItemId::ROOT {
            return Ok(Some(names.iter().rev().collect()));
        }
        let Some(place) = reader.find_item(parent)?.and_then(|row| row.onedrive).and_then(|version| version.place) else {
            return Ok(None);
        };
        names.push(place.name);
        parent = place.parent_local_item_id;
    }
    Ok(None)
}

fn allowed_remote(reader: &ItemReader<'_>, scope: &WriteScope, place: Option<&ItemPlace>) -> Result<bool, TreeError> {
    let Some(place) = place else { return Ok(false) };
    Ok(remote_place(reader, place)?.is_some_and(|path| scope.allows(&path)))
}

fn allowed_local(reader: &ItemReader<'_>, scope: &WriteScope, item: &ItemVersions) -> Result<bool, TreeError> {
    let Some(place) = item.local.as_ref().and_then(|version| version.place.as_ref()) else {
        return Ok(false);
    };
    Ok(reader
        .find_path(place.parent_local_item_id)?
        .is_some_and(|parent| scope.allows(&parent.join(&place.name))))
}

fn allowed_item(reader: &ItemReader<'_>, scope: &WriteScope, item: &ItemVersions) -> Result<bool, TreeError> {
    if item.local.is_some() && !allowed_local(reader, scope, item)? {
        return Ok(false);
    }
    if item.onedrive_item.is_some() && !allowed_remote(reader, scope, item.onedrive.as_ref().and_then(|version| version.place.as_ref()))? {
        return Ok(false);
    }
    Ok(true)
}

fn allows_step(reader: &ItemReader<'_>, scope: &WriteScope, step: &OneDriveStep) -> Result<bool, TreeError> {
    let (item, target) = match step {
        OneDriveStep::MakeDirectory { item, parent, name }
        | OneDriveStep::SendNewFile { item, parent, name }
        | OneDriveStep::SendMove { item, parent, name } => (item, Some((parent, name))),
        OneDriveStep::SendContent { item } | OneDriveStep::SendFileRemoval { item } | OneDriveStep::SendDirectoryRemoval { item, .. } => (item, None),
    };
    if !allowed_item(reader, scope, item)? {
        return Ok(false);
    }
    // A listing can move the item after planning without changing its content cTag.
    // That cTag could still authorize a content write at the new, out-of-scope location.
    let Some(current) = reader.find_item(item.local_item_id)? else {
        return Ok(false);
    };
    if !allowed_item(reader, scope, &ItemVersions::of(&current))? {
        return Ok(false);
    }
    // A resumed upload can still target the name it was opened at before a local move.
    if let Some(place) = reader
        .find_item_memory(item.local_item_id)?
        .upload_session
        .and_then(|session| session.empty_file_place)
    {
        if !allowed_remote(reader, scope, Some(&place))? {
            return Ok(false);
        }
    }
    if let Some((parent, name)) = target {
        let Some(parent) = reader.find_bound_item(parent)? else { return Ok(false) };
        if !allowed_remote(
            reader,
            scope,
            Some(&ItemPlace {
                parent_local_item_id: parent.local_item_id,
                name: name.clone(),
            }),
        )? {
            return Ok(false);
        }
    }
    if let OneDriveStep::SendDirectoryRemoval { below, .. } = step {
        for child in below {
            if !allowed_item(reader, scope, child)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Leftovers without a resolvable cloud path stay in OneDrive while restricted.
pub(super) async fn allows_reference(graph: &super::GraphAccess, source: &WriteScopeSource, reference: &konedrive_tree::OneDriveItemReference) -> bool {
    let scope = source.get();
    if scope.unrestricted() {
        return true;
    }
    let reference = reference.clone();
    graph
        .store
        .read_items(move |reader| {
            let Some(row) = reader.find_bound_item(&reference)? else { return Ok(false) };
            allowed_remote(reader, &scope, row.onedrive.as_ref().and_then(|version| version.place.as_ref()))
        })
        .await
        .unwrap_or(false)
}

/// Authorizes an upload answer or a name holder by its returned cloud parent and name.
/// Unlike a leftover reference, a fresh answer need not have a bound row yet.
pub(super) async fn allows_confirmed(graph: &super::GraphAccess, source: &WriteScopeSource, item: &konedrive_tree::ConfirmedItem) -> bool {
    let scope = source.get();
    if scope.unrestricted() {
        return true;
    }
    if item.parent.drive_id != graph.drive_id || item.item.drive_id != graph.drive_id {
        return false;
    }
    let (parent, name) = (item.parent.clone(), item.name.clone());
    graph
        .store
        .read_items(move |reader| {
            let Some(parent) = reader.find_bound_item(&parent)? else { return Ok(false) };
            allowed_remote(
                reader,
                &scope,
                Some(&ItemPlace {
                    parent_local_item_id: parent.local_item_id,
                    name,
                }),
            )
        })
        .await
        .unwrap_or(false)
}

impl OneDriveSteps {
    pub(crate) fn with_write_scope(mut self, source: WriteScopeSource) -> Self {
        self.content_upload = self.content_upload.with_write_scope(source.clone());
        self.name_holders = self.name_holders.with_write_scope(source.clone());
        self.made_items = self.made_items.with_write_scope(source.clone());
        self.write_scope = source;
        self
    }

    pub(super) async fn check_write_scope(&self, step: &OneDriveStep) -> Result<(), StepOutcome> {
        let scope = self.write_scope.get();
        if scope.unrestricted() {
            return Ok(());
        }
        let step = step.clone();
        match self.graph.store.read_items(move |reader| allows_step(reader, &scope, &step)).await {
            Ok(true) => Ok(()),
            // A retryable refusal keeps the local version, is visible in Issues and
            // automatically tries again after the scope is widened or cleared.
            Ok(false) => Err(StepOutcome::Refused(StepRefusal::Transient {
                detail: "outside the account's write scope; change account write-scope to send it".into(),
            })),
            Err(error) => Err(super::fail_store(error)),
        }
    }
}
