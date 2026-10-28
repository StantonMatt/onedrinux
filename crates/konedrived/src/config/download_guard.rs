//! Always rules persist before taking effect; session approvals live only in memory.
use crate::download_guard::{DownloadGuard, GuardConfig};
use crate::config::{ConfigError, ConfigStore};

impl ConfigStore {
    pub fn guard(&self) -> &std::sync::Arc<DownloadGuard> {
        &self.download_guard
    }

    pub fn set_download_guard(&self, enabled: bool) -> Result<(), ConfigError> {
        self.update(|config| {
            config.download_guard = Some(enabled);
            Ok(())
        })
    }
    pub fn set_guard_thresholds(&self, files: u32, bytes: u64, seconds: u32) -> Result<(), ConfigError> {
        if files == 0 || bytes == 0 || seconds == 0 {
            return Err(ConfigError::InvalidGuard("thresholds must be greater than zero".into()));
        }
        self.update(|config| {
            let limits = config.download_guard_limits.get_or_insert_with(GuardConfig::default);
            limits.max_files = files;
            limits.max_bytes = bytes;
            limits.window_seconds = seconds;
            Ok(())
        })
    }
    /// `session` or `always` allows, `block` always blocks, `reset` restores automatic.
    pub fn set_guard_app(&self, identity: &str, policy: &str) -> Result<(), ConfigError> {
        if identity.len() > 4096
            || !(identity.starts_with("app:") || identity.starts_with("exe:/"))
            || identity.chars().any(char::is_control)
        {
            return Err(ConfigError::InvalidGuard("expected an app: or exe:/ identity from ListApps".into()));
        }
        if !["session", "always", "block", "reset"].contains(&policy) {
            return Err(ConfigError::InvalidGuard("unknown app policy".into()));
        }
        // Serialize app policy calls through persistence and the following session change.
        let _changing = crate::panic::lock(&self.download_guard.changing);
        if policy == "session" {
            // A temporary approval does not erase an always rule. It overrides it only
            // in this process and works even when config.toml cannot be written.
            self.download_guard.reset(identity);
            self.download_guard.allow_session(identity);
            return Ok(());
        }
        self.update(|config| {
            let limits = config.download_guard_limits.get_or_insert_with(GuardConfig::default);
            limits.allowed_apps.retain(|id| id != identity);
            limits.blocked_apps.retain(|id| id != identity);
            match policy {
                "always" => limits.allowed_apps.push(identity.into()),
                "block" => limits.blocked_apps.push(identity.into()),
                _ => {}
            }
            Ok::<_, ConfigError>(())
        })?;
        self.download_guard.reset(identity);
        Ok(())
    }
}
