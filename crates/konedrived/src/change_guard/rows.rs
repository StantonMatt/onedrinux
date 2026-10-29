//! Rows for the app, folded on the daemon so thousands of descendants stay cheap.
// test: held_rows_fold_trees_and_preserve_rewrite_and_move_details
// test: moved_rows_keep_the_agreed_parent_path_and_count_unchanged_descendants
use super::{Change, Disposition, Kind};
use konedrive_dbus::rows::HeldChange;
use std::collections::BTreeMap;

pub(super) fn describe(changes: Vec<&Change>) -> Vec<HeldChange> {
    let mut changes = changes;
    changes.sort_by(|a, b| (&a.path, a.kind, a.at).cmp(&(&b.path, b.kind, b.at)));
    let mut rows: Vec<HeldChange> = Vec::new();
    let mut folders = BTreeMap::new();
    for c in changes {
        let sent = c.disposition == Disposition::Sent;
        // Paths are display-only here. Decisions still select individual ids/generations.
        let parent = if matches!(c.kind, Kind::Removal | Kind::Upload) {
            let mut path = c.path.as_str();
            let mut found = None;
            while let Some((above, _)) = path.rsplit_once('/') {
                if let Some(index) = folders.get(&(above.to_owned(), c.kind, sent)) {
                    found = Some(*index);
                }
                path = above;
            }
            found
        } else {
            None
        };
        if let Some(index) = parent {
            let row: &mut HeldChange = &mut rows[index];
            row.descendants = row.descendants.saturating_add(1);
            row.bytes = row.bytes.saturating_add(c.bytes);
            row.after_bytes = row.after_bytes.saturating_add(c.after_bytes);
            row.since = row.since.min(c.at);
            continue;
        }
        if c.directory && matches!(c.kind, Kind::Removal | Kind::Upload) {
            folders.insert((c.path.clone(), c.kind, sent), rows.len());
        }
        rows.push(HeldChange {
            item: c.item,
            path: c.path.clone(),
            kind: c.kind.key().into(),
            bytes: c.bytes,
            since: c.at,
            generation: c.generation.clone(),
            state: c.disposition.key().into(),
            directory: c.directory,
            descendants: c.descendants,
            after_bytes: c.after_bytes,
            old_path: c.old_path.clone(),
            sent,
        });
    }
    rows
}

/// Follow only agreed places: a locally moved parent must not rewrite a child's old
/// path. ItemReader::find_synced_path deliberately switches to the current local path.
pub(super) fn old_path(
    items: &konedrive_tree::ItemReader<'_>,
    row: &konedrive_tree::Item,
) -> Result<String, konedrive_tree::TreeError> {
    let mut names = Vec::new();
    let mut at = row.clone();
    let mut seen = std::collections::BTreeSet::new();
    while at.local_item_id != konedrive_tree::LocalItemId::ROOT {
        if !seen.insert(at.local_item_id) {
            return Err(konedrive_tree::TreeError::Io(std::io::Error::other(
                "cycle in agreed change-guard path",
            )));
        }
        let Some(place) = at.synced_place else {
            return Ok(String::new());
        };
        names.push(place.name);
        let Some(parent) = items.find_item(place.parent_local_item_id)? else {
            return Ok(String::new());
        };
        at = parent;
    }
    Ok(names
        .into_iter()
        .rev()
        .collect::<std::path::PathBuf>()
        .display()
        .to_string())
}
