//! The account's Allow/Restore bridge. The guard owns decisions; the mount owns disk.
// test: held_removals_restore_offline_and_refuse_a_replacement
// test: allowing_while_offline_reports_durably_queued_changes
// test: restore_reports_partial_failures_and_final_progress_without_a_mount
use crate::account_folder::SyncService;
use crate::change_guard::Kind;
use konedrive_dbus::rows::GuardResult;
use std::sync::Arc;

impl SyncService {
    pub(crate) fn account_id(&self) -> &str {
        self.wiring.persist.account.as_str()
    }
    pub(crate) fn change_guard(&self) -> Result<Arc<crate::change_guard::ChangeGuard>, String> {
        self.wiring
            .persist
            .store
            .change_guard(&self.wiring.persist.account)
            .map_err(|e| e.to_string())
    }
    /// A successful Allow releases exactly these generations for the ordinary sender.
    /// It does not await Graph: offline work is queued, and later remote conflicts go
    /// through the ordinary conflict/version-history machinery.
    pub(crate) async fn allow_changes(
        &self,
        token: String,
        kinds: Vec<String>,
    ) -> Result<GuardResult, String> {
        let guard = self.change_guard()?;
        let (batch, mut result) =
            tokio::task::spawn_blocking(move || guard.decide_batch(&token, &kinds, false))
                .await
                .map_err(|e| e.to_string())??;
        let offline = self.state().read(|s| {
            s.cycle
                .sync_trouble
                .as_ref()
                .is_some_and(|t| t.kind() == crate::status::sync_trouble::TroubleKind::Unreachable)
        });
        if offline {
            result.pending_offline = batch.len() as u64;
        } else {
            result.done = batch.len() as u64;
        }
        Ok(result)
    }
    pub(crate) async fn restore_changes(
        &self,
        token: String,
        kinds: Vec<String>,
    ) -> Result<GuardResult, String> {
        let mount = self.mount();
        let guard = self.change_guard()?;
        let deciding = Arc::clone(&guard);
        let selected = kinds.clone();
        let batch_token = token.clone();
        let (mut batch, mut result) = tokio::task::spawn_blocking(move || {
            deciding.decide_batch(&batch_token, &selected, true)
        })
        .await
        .map_err(|e| e.to_string())??;
        batch.sort_by_key(|c| c.path.matches('/').count());
        let total = batch.len() as u64 + result.failed;
        let mut processed = result.failed;
        guard.progress(&token, &kinds, processed, total);
        let combined: std::collections::BTreeMap<_, _> = batch
            .iter()
            .filter(|c| {
                c.kind == Kind::Move
                    && batch
                        .iter()
                        .any(|r| r.item == c.item && r.kind == Kind::Rewrite)
            })
            .map(|c| (c.item, c.clone()))
            .collect();
        for change in batch {
            if change.kind == Kind::Move && combined.contains_key(&change.item) {
                continue;
            }
            let take_move = combined.contains_key(&change.item) && change.kind == Kind::Rewrite;
            let outcome = if change.item < 0 {
                if let Some(reference) = guard.cleanup_reference(&change) {
                    let store = mount.as_ref().map(|m| m.folder_step_parts().store);
                    match store {
                        Some(store) => store.call(move |s| s.step_memory().forget_mismatched_item(&reference)).await.map(|_| true).map_err(|e| e.to_string()),
                        None => Err("Mount this account's folder, then restore again.".into()),
                    }
                } else { Ok(true) }
            } else if change.kind == Kind::Upload {
                Ok(true)
            } else if let Some(mount) = &mount {
                mount
                    .restore_guard_change_taking_move(change.clone(), take_move)
                    .await
            } else {
                Err("Mount this account's folder, then restore again.".into())
            };
            let success = matches!(outcome, Ok(true));
            let reason = match outcome {
                Ok(false) => "Newer work, an open file or a file in the destination prevents restore. Close open files or move the conflicting file, then review and restore again.".to_owned(),
                Err(error) => error,
                Ok(true) => String::new(),
            };
            guard.restored(&change, success)?;
            let count = if take_move { 2 } else { 1 };
            if take_move {
                guard.restored(&combined[&change.item], success)?;
            }
            processed += count;
            if success {
                result.done += count;
                if change.kind == Kind::Rewrite {
                    result.kept_both_versions += 1;
                }
            } else {
                result.failed += count;
                result
                    .failures
                    .push((change.item, change.path.clone(), reason.clone()));
                if take_move {
                    result.failures.push((
                        change.item,
                        combined[&change.item].path.clone(),
                        reason,
                    ));
                }
            }
            guard.progress(&token, &kinds, processed, total);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
