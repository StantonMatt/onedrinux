//! Service assumptions for remote namespace safety. Only the guarded run folder is written.

use super::*;

impl Run {
    /// The focused service check, retaining all the harness's account and wire guards.
    pub async fn remote(&mut self) -> Vec<(&'static str, Outcome)> {
        let mut results = Vec::new();
        for check in 0..4 {
            if self.guard.refused().is_some() { break; }
            let (name, outcome) = match check {
                0 => ("folder eTags and conditional folder deletes", self.remote_safety().await),
                1 => ("createdBy.application identifies this client", self.remote_application().await),
                2 => ("zero-byte create refuses an existing name", self.empty_file_name_taken().await),
                _ => ("zero-byte PUT honours If-Match", self.empty_file_if_match().await),
            };
            show(name, &outcome);
            results.push((name, outcome));
        }
        results
    }

    pub(super) async fn remote_application(&mut self) -> Outcome {
        let made = step!("creating a file", self.put("application-id.txt", Vec::new(), T0).await);
        let found = step!("GET createdBy.application", self.drive.item(&made.id).await);
        let application = found.created_by.as_ref().and_then(|by| by.pointer("/application/id")).and_then(Value::as_str);
        if application.is_some_and(|id| id.eq_ignore_ascii_case(&self.client_id)) {
            Pass("createdBy.application.id matches this client".into())
        } else {
            Fail("createdBy.application.id absent or different; untracked empty holders must be kept".into())
        }
    }

    /// The contract a folder's removal is built on (`docs/design/writes.md` §4.2), as
    /// measured on OneDrive personal on 2026-10-29: a folder has no cTag in GET or in delta;
    /// its eTag stays through a descendant's creation, edit and removal, and changes at its
    /// own rename and at its move; a DELETE with a stale eTag is refused 412, and one with
    /// the current eTag is carried out, 204.
    pub(super) async fn remote_safety(&mut self) -> Outcome {
        let outer = step!("making the guarded parent", self.drive.create_folder(&self.folder, "etag-parent").await);
        let inner = step!("making its child folder", self.drive.create_folder(&outer.id, "child").await);
        let first = step!("GET of the folder", self.drive.item(&outer.id).await);
        if first.c_tag.as_deref().is_some_and(|tag| !tag.is_empty()) {
            return Fail("folder GET carries a cTag; the sender guards a folder's removal by its eTag alone".into());
        }
        let Some(etag) = first.e_tag.clone().filter(|tag| !tag.is_empty()) else {
            return Fail("folder GET has no eTag; a folder's removal would wait for one forever".into());
        };
        for operation in 0..3 {
            let file = if operation == 0 {
                step!("creating a descendant", self.drive.upload_empty(UploadTarget::New { parent_id: &inner.id, name: "descendant.txt" }, T0).await)
            } else {
                step!("reading the descendant", self.drive.child(&inner.id, "descendant.txt").await)
            };
            if operation == 1 {
                step!("editing a descendant", self.replace(&file.id, file.e_tag.as_deref().unwrap_or_default(), b"changed".to_vec(), T0 + 60).await);
            } else if operation == 2 {
                step!("removing a descendant", self.drive.delete_item(&file.id, file.e_tag.as_deref().unwrap_or_default()).await);
            }
            let after = step!("GET after descendant change", self.drive.item(&outer.id).await);
            if after.e_tag.as_deref() != Some(etag.as_str()) {
                return Fail(format!("folder eTag changed after descendant operation {operation} (create/edit/remove); the rows' check after a listing assumes it does not"));
            }
        }
        // Delta is eventually consistent; keep the existing bounded wait and wire budget.
        let mut matched = false;
        for attempt in 0..=5 {
            let listed = step!("folder delta", follow(&self.drive, &self.delta_link).await);
            if let Some(row) = listed.get(&outer.id).filter(|row| row.folder.is_some()) {
                if row.c_tag.as_deref().is_some_and(|tag| !tag.is_empty()) {
                    return Fail("delta carries a folder cTag; the sender guards a folder's removal by its eTag alone".into());
                }
                if row.e_tag.as_deref() == Some(etag.as_str()) {
                    matched = true;
                    break;
                }
            }
            if attempt < 5 { tokio::time::sleep(self.delta_wait / 5).await; }
        }
        if !matched { return Fail("delta did not report the folder with the eTag GET gives".into()); }
        let renamed = step!("renaming the folder", self.rename(&first, "etag-parent-renamed").await);
        if renamed.e_tag.as_deref().is_none_or(|tag| tag == etag) {
            return Fail("folder eTag did not change at its own rename; a stale removal would not be refused".into());
        }
        let sibling = step!("making a sibling folder", self.drive.create_folder(&self.folder, "etag-sibling").await);
        let change = ItemChange { parent_id: Some(&sibling.id), ..ItemChange::default() };
        let moved = step!("moving the folder", self.drive.update_item(&outer.id, renamed.e_tag.as_deref().unwrap_or_default(), &change).await);
        if moved.e_tag.is_none() || moved.e_tag == renamed.e_tag {
            return Fail("folder eTag did not change at its move; a stale removal would not be refused".into());
        }
        match self.drive.delete_folder(&outer.id, &etag).await {
            Err(WriteError::Changed) => {}
            Ok(()) => return Fail("DELETE with a stale folder eTag succeeded; conditional folder deletes are unsafe".into()),
            Err(error) => return Fail(format!("stale folder DELETE: {error}")),
        }
        step!("DELETE with the current folder eTag", self.drive.delete_folder(&outer.id, moved.e_tag.as_deref().unwrap_or_default()).await);
        Pass("no folder cTag in GET or delta; the folder eTag stays through descendant create/edit/remove and changes at its rename and move; stale DELETE is 412, current is 204".into())
    }
}

#[cfg(test)]
mod tests;
