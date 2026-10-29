//! Daemon settings and immutable outgoing batches, independent of the app UI.
// test: change_guard_interface_matches_its_xml
use super::fault::Fault;
use crate::daemon::manager::{Account, AccountManager};
use konedrive_dbus::{
    rows::{GuardResult, HeldChange},
    Refusal, ACCOUNTS_PATH,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use zbus::{interface, object_server::SignalEmitter, Connection};

pub(crate) struct ChangeGuard {
    pub(crate) manager: Arc<AccountManager>,
}
fn failed(error: impl ToString) -> Fault {
    Fault::refused(Refusal::Failed, error.to_string())
}
impl ChangeGuard {
    fn account(&self, id: &str) -> Result<Arc<Account>, Fault> {
        self.manager
            .accounts()
            .into_iter()
            .find(|a| a.id.as_str() == id)
            .ok_or_else(|| failed("no such account"))
    }
    fn guard(&self, id: &str) -> Result<Arc<crate::change_guard::ChangeGuard>, Fault> {
        self.manager
            .config()
            .change_guard(&self.account(id)?.id)
            .map_err(failed)
    }
}
#[interface(name = "org.konedrive.ChangeGuard")]
impl ChangeGuard {
    #[zbus(out_args("token", "changes"))]
    fn list_held(&self, account: &str) -> Result<(String, Vec<HeldChange>), Fault> {
        self.guard(account)?.snapshot().map_err(failed)
    }
    #[zbus(out_args("result"))]
    async fn allow_held(
        &self,
        account: String,
        token: String,
        kinds: Vec<String>,
    ) -> Result<GuardResult, Fault> {
        self.account(&account)?
            .sync
            .allow_changes(token, kinds)
            .await
            .map_err(failed)
    }
    #[zbus(out_args("result"))]
    async fn restore_held(
        &self,
        account: String,
        token: String,
        kinds: Vec<String>,
    ) -> Result<GuardResult, Fault> {
        self.account(&account)?
            .sync
            .restore_changes(token, kinds)
            .await
            .map_err(failed)
    }
    #[zbus(out_args("result"))]
    async fn upload_local(&self, account: String, path: String) -> Result<GuardResult, Fault> {
        let guard = self.guard(&account)?;
        tokio::task::spawn_blocking(move || guard.upload_local(&path, crate::clock::unix_now()))
            .await
            .map_err(failed)?
            .map_err(failed)
    }
    async fn set_enabled(
        &self,
        enabled: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), Fault> {
        let config = Arc::clone(self.manager.config());
        tokio::task::spawn_blocking(move || config.set_changes_enabled(enabled))
            .await
            .map_err(failed)?
            .map_err(failed)?;
        self.enabled_changed(&emitter).await?;
        Ok(())
    }
    async fn set_thresholds(
        &self,
        thresholds: BTreeMap<String, u64>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), Fault> {
        let config = Arc::clone(self.manager.config());
        tokio::task::spawn_blocking(move || config.set_changes_thresholds(thresholds))
            .await
            .map_err(failed)?
            .map_err(failed)?;
        self.thresholds_changed(&emitter).await?;
        Ok(())
    }
    #[zbus(out_args("enabled", "thresholds"))]
    fn get_settings(&self) -> (bool, BTreeMap<String, u64>) {
        let settings = self
            .manager
            .config()
            .snapshot()
            .change_guard
            .unwrap_or_default();
        (settings.enabled, settings.thresholds())
    }
    #[zbus(out_args("error"))]
    fn get_error(&self, account: &str) -> Result<String, Fault> {
        Ok(self.guard(account)?.error().unwrap_or_default())
    }
    #[zbus(property)]
    fn enabled(&self) -> bool {
        self.get_settings().0
    }
    #[zbus(property)]
    fn thresholds(&self) -> HashMap<String, u64> {
        self.get_settings().1.into_iter().collect()
    }
    #[zbus(signal)]
    pub(crate) async fn changes_held(
        emitter: &SignalEmitter<'_>,
        account: &str,
        token: &str,
        kind: &str,
        count: u64,
        bytes: u64,
        since: i64,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    pub(crate) async fn restore_progress(
        emitter: &SignalEmitter<'_>,
        account: &str,
        token: &str,
        kinds: Vec<String>,
        done: u64,
        total: u64,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    pub(crate) async fn held_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Each export owns one task. Subscribe before taking snapshots; lagged clients re-list.
pub(crate) fn start_signals(
    connection: &Connection,
    account: String,
    guard: Arc<crate::change_guard::ChangeGuard>,
) -> tokio::task::JoinHandle<()> {
    let mut news = guard.subscribe();
    let mut progress = guard.subscribe_progress();
    let connection = connection.clone();
    tokio::spawn(async move {
        let Ok(emitter) = SignalEmitter::new(&connection, ACCOUNTS_PATH) else {
            return;
        };
        loop {
            tokio::select! {
                message = progress.recv() => match message {
                    Ok((token, kinds, done, total)) => { let _ = ChangeGuard::restore_progress(&emitter, &account, &token, kinds, done, total).await; },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                message = news.recv() => match message {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Ok((token, rows)) = guard.snapshot() {
                            for kind in ["removal", "rewrite", "move", "upload"] {
                                let selected: Vec<_> = rows.iter().filter(|r| r.kind == kind && !r.sent).collect();
                                if !selected.is_empty() {
                                    let bytes = selected.iter().fold(0u64, |a, r| a.saturating_add(r.bytes));
                                    let count = selected.iter().fold(0u64, |a,r| a.saturating_add(1 + if matches!(kind, "removal" | "upload") { r.descendants } else { 0 }));
                                    let since = selected.iter().map(|r| r.since).min().unwrap_or(0);
                                    let _ = ChangeGuard::changes_held(&emitter, &account, &token, kind, count, bytes, since).await;
                                }
                            }
                        }
                        let _ = ChangeGuard::held_changed(&emitter).await;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    })
}
