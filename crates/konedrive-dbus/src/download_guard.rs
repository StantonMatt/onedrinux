//! Machine-wide download admission, at `/org/konedrive/Accounts`.
use crate::rows::GuardApp;

#[zbus::proxy(
    interface = "org.konedrive.DownloadGuard",
    default_service = "org.konedrive.Daemon",
    default_path = "/org/konedrive/Accounts",
    gen_blocking = false
)]
pub trait DownloadGuard {
    fn list_apps(&self) -> zbus::Result<Vec<GuardApp>>;
    fn allow_app(&self, identity: &str, always: bool) -> zbus::Result<()>;
    fn block_app(&self, identity: &str) -> zbus::Result<()>;
    fn free_up_app_downloads(&self, identity: &str) -> zbus::Result<(u64, u64)>;
    fn reset_app(&self, identity: &str) -> zbus::Result<()>;
    fn set_enabled(&self, enabled: bool) -> zbus::Result<()>;
    fn set_thresholds(&self, max_files: u32, max_bytes: u64, window_seconds: u32) -> zbus::Result<()>;
    #[zbus(property)]
    fn enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn max_files(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn max_bytes(&self) -> zbus::Result<u64>;
    #[zbus(property)]
    fn window_seconds(&self) -> zbus::Result<u32>;
    #[zbus(signal)]
    fn downloads_blocked(&self, identity: String, display_name: String, files: u64, bytes: u64, reason: String, runs_programs: bool, block_id: u64) -> zbus::Result<()>;
    #[zbus(signal)]
    fn apps_changed(&self) -> zbus::Result<()>;
}
