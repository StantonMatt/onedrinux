//! Service assumptions for remote namespace safety. Only the guarded run folder is written.

use super::*;

impl Run {
    /// The focused service check, retaining all the harness's account and wire guards.
    pub async fn remote(&mut self) -> Vec<(&'static str, Outcome)> {
        let mut results = Vec::new();
        for check in 0..4 {
            if self.guard.refused().is_some() { break; }
            let (name, outcome) = match check {
                0 => ("folder cTags and stale conditional deletes", self.remote_safety().await),
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

    pub(super) async fn remote_safety(&mut self) -> Outcome {
        let outer = step!("making the guarded parent", self.drive.create_folder(&self.folder, "ctag-parent").await);
        let inner = step!("making its child folder", self.drive.create_folder(&outer.id, "child").await);
        let mut before = step!("GET of the folder", self.drive.item(&outer.id).await);
        for operation in 0..3 {
            let Some(old_tag) = before.c_tag.as_deref().filter(|tag| !tag.is_empty()) else {
                return Fail("folder GET has no cTag; default folder deletion must stay blocked".into());
            };
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
            if after.c_tag.as_deref().filter(|tag| !tag.is_empty()).is_none() || after.c_tag == before.c_tag {
                return Fail(format!("folder cTag did not change after descendant operation {operation} (create/edit/remove)"));
            }
            match self.drive.delete_folder(&outer.id, old_tag).await {
                Err(WriteError::Changed) => {},
                Ok(()) => return Fail("DELETE with a stale folder cTag succeeded; conditional folder deletes are unsafe".into()),
                Err(error) => return Fail(format!("stale folder DELETE: {error}")),
            }
            // Delta is eventually consistent; keep the existing bounded wait and wire budget.
            let mut matched = false;
            for attempt in 0..=5 {
                let listed = step!("folder delta", follow(&self.drive, &self.delta_link).await);
                if listed.get(&outer.id).is_some_and(|row| row.folder.is_some() && row.c_tag == after.c_tag) {
                    matched = true;
                    break;
                }
                if attempt < 5 { tokio::time::sleep(self.delta_wait / 5).await; }
            }
            if !matched { return Fail("delta did not report the folder with its current cTag".into()); }
            before = after;
        }
        Pass("GET and delta carry folder cTags; descendant create/edit/remove changes them; stale DELETE is 412".into())
    }
}

#[cfg(test)]
mod tests;
