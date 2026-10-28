//! The UI reads pending blocks with ListApps after subscribing to DownloadsBlocked.
use super::fault::Fault;
use crate::config::{ConfigError, ConfigStore};
use konedrive_dbus::{rows::GuardApp, Refusal, ACCOUNTS_PATH};
use std::sync::Arc;
use zbus::{interface, object_server::SignalEmitter, Connection};

pub(crate) struct DownloadGuard {
    pub(crate) config: Arc<ConfigStore>,
    pub(crate) manager: Arc<crate::daemon::manager::AccountManager>,
}

fn fault(error: ConfigError) -> Fault {
    let refusal = if matches!(error, ConfigError::InvalidGuard(_)) { Refusal::InvalidArgs } else { Refusal::Failed };
    Fault::refused(refusal, error.to_string())
}

impl DownloadGuard {
    async fn write(&self, change: impl FnOnce(&ConfigStore) -> Result<(), ConfigError> + Send + 'static) -> Result<(), Fault> {
        let config = Arc::clone(&self.config);
        tokio::task::spawn_blocking(move || change(&config))
            .await
            .map_err(|e| Fault::refused(Refusal::Failed, e.to_string()))?
            .map_err(fault)
    }
}

#[interface(name = "org.konedrive.DownloadGuard")]
impl DownloadGuard {
    #[zbus(out_args("apps"))]
    fn list_apps(&self) -> Vec<GuardApp> {
        self.config.guard().apps()
    }

    #[zbus(out_args("freed_files", "freed_bytes"))]
    async fn free_up_app_downloads(&self, identity: &str, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<(u64, u64), Fault> {
        let mut total = (0, 0);
        for account in self.manager.accounts() {
            let freed = account.sync.free_app_downloads(identity).await;
            total.0 += freed.0;
            total.1 += freed.1;
        }
        if total.0 > 0 { Self::apps_changed(&emitter).await?; }
        Ok(total)
    }

    async fn allow_app(&self, identity: String, always: bool, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<(), Fault> {
        self.write(move |config| config.set_guard_app(&identity, if always { "always" } else { "session" })).await?;
        Self::apps_changed(&emitter).await?;
        Ok(())
    }
    async fn block_app(&self, identity: String, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<(), Fault> {
        self.write(move |config| config.set_guard_app(&identity, "block")).await?;
        Self::apps_changed(&emitter).await?;
        Ok(())
    }
    async fn reset_app(&self, identity: String, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<(), Fault> {
        self.write(move |config| config.set_guard_app(&identity, "reset")).await?;
        Self::apps_changed(&emitter).await?;
        Ok(())
    }
    async fn set_enabled(&self, enabled: bool, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<(), Fault> {
        self.write(move |config| config.set_download_guard(enabled)).await?;
        self.enabled_changed(&emitter).await?;
        Ok(())
    }
    async fn set_thresholds(
        &self,
        max_files: u32,
        max_bytes: u64,
        window_seconds: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), Fault> {
        self.write(move |config| config.set_guard_thresholds(max_files, max_bytes, window_seconds)).await?;
        self.max_files_changed(&emitter).await?;
        self.max_bytes_changed(&emitter).await?;
        self.window_seconds_changed(&emitter).await?;
        Ok(())
    }
    #[zbus(property)]
    fn enabled(&self) -> bool {
        self.config.guard().enabled()
    }
    #[zbus(property)]
    fn max_files(&self) -> u32 {
        self.config.guard().limits().max_files
    }
    #[zbus(property)]
    fn max_bytes(&self) -> u64 {
        self.config.guard().limits().max_bytes
    }
    #[zbus(property)]
    fn window_seconds(&self) -> u32 {
        self.config.guard().limits().window_seconds
    }
    #[zbus(signal)]
    pub(crate) async fn downloads_blocked(
        emitter: &SignalEmitter<'_>,
        identity: &str,
        display_name: &str,
        files: u64,
        bytes: u64,
        reason: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    pub(crate) async fn apps_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The broadcast is only a wake-up: ListApps is authoritative and survives a late UI
/// or lagged receiver. This task owns no manager or guard; it exits with the config owner.
pub(crate) fn start_signals(connection: &Connection, config: &ConfigStore) {
    let mut news = config.guard().subscribe();
    let connection = connection.clone();
    tokio::spawn(async move {
        let Ok(emitter) = SignalEmitter::new(&connection, ACCOUNTS_PATH) else {
            return;
        };
        loop {
            match news.recv().await {
                Ok(app) => {
                    let _ = DownloadGuard::downloads_blocked(&emitter, &app.identity, &app.display_name, app.files, app.bytes, &app.reason)
                        .await;
                    let _ = DownloadGuard::apps_changed(&emitter).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = DownloadGuard::apps_changed(&emitter).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
