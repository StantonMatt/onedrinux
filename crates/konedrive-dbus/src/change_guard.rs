//! Daemon-wide guard settings and account-scoped immutable outgoing batches.
use crate::rows::{GuardResult, HeldChange};
use std::collections::{BTreeMap, HashMap};
#[zbus::proxy(
    interface = "org.konedrive.ChangeGuard",
    default_service = "org.konedrive.Daemon",
    default_path = "/org/konedrive/Accounts",
    gen_blocking = false
)]
pub trait ChangeGuard {
    fn list_held(&self, account: &str) -> zbus::Result<(String, Vec<HeldChange>)>;
    fn allow_held(&self, account: &str, token: &str, kinds: &[String])
        -> zbus::Result<GuardResult>;
    fn restore_held(
        &self,
        account: &str,
        token: &str,
        kinds: &[String],
    ) -> zbus::Result<GuardResult>;
    fn upload_local(&self, account: &str, path: &str) -> zbus::Result<GuardResult>;
    fn set_enabled(&self, enabled: bool) -> zbus::Result<()>;
    fn set_thresholds(&self, thresholds: BTreeMap<String, u64>) -> zbus::Result<()>;
    fn get_settings(&self) -> zbus::Result<(bool, BTreeMap<String, u64>)>;
    fn get_error(&self, account: &str) -> zbus::Result<String>;
    #[zbus(property)]
    fn enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn thresholds(&self) -> zbus::Result<HashMap<String, u64>>;
    #[zbus(signal)]
    fn changes_held(
        &self,
        account: String,
        token: String,
        kind: String,
        count: u64,
        bytes: u64,
        since: i64,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    fn restore_progress(
        &self,
        account: String,
        token: String,
        kinds: Vec<String>,
        done: u64,
        total: u64,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    fn held_changed(&self) -> zbus::Result<()>;
}
