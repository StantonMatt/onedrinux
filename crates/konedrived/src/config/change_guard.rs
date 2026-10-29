//! Account settings and the single ledger owner for each account.
// test: change_guard_settings_round_trip
// test: change_guard_settings_are_shared_with_open_and_future_accounts
// test: legacy_change_guard_settings_migrate_with_the_strictest_limits
use super::{AccountId, ConfigError, ConfigStore};
use crate::change_guard::{ChangeGuard, GuardConfig};
use std::collections::BTreeMap;
use std::sync::Arc;

impl ConfigStore {
    pub(crate) fn change_guard(
        &self,
        account: &AccountId,
    ) -> Result<Arc<ChangeGuard>, ConfigError> {
        if !account.is_valid() {
            return Err(ConfigError::NoAccount(account.clone()));
        }
        // Read config before the registry lock: update takes these locks in that order.
        let inner = self.lock();
        let snapshot = &inner.config;
        if snapshot.account(account).is_none() {
            return Err(ConfigError::NoAccount(account.clone()));
        }
        let config = snapshot.change_guard.clone().unwrap_or_default();
        let mut guards = crate::panic::lock(&self.change_guards);
        Ok(Arc::clone(guards.entry(account.clone()).or_insert_with(
            || {
                Arc::new(ChangeGuard::open(
                    self.state_dir
                        .join("accounts")
                        .join(account.as_str())
                        .join("change-guard.json"),
                    config,
                ))
            },
        )))
    }
    pub(crate) fn set_changes_enabled(&self, enabled: bool) -> Result<(), ConfigError> {
        self.set_changes_config(move |config| {
            config.enabled = enabled;
            Ok(())
        })
    }
    pub(crate) fn set_changes_thresholds(
        &self,
        values: BTreeMap<String, u64>,
    ) -> Result<(), ConfigError> {
        self.set_changes_config(move |config| config.set_thresholds(values))
    }
    fn set_changes_config(
        &self,
        change: impl FnOnce(&mut GuardConfig) -> Result<(), String>,
    ) -> Result<(), ConfigError> {
        self.update(|config| {
            change(config.change_guard.get_or_insert_with(GuardConfig::default))
                .map_err(ConfigError::InvalidGuard)
        })?;
        Ok(())
    }
}

/// If legacy accounts disagree, keep protection on if any account enabled it and take
/// the strictest limit/window. This cannot silently widen a previous safety setting.
pub(super) fn migrate(config: &mut super::Config) -> bool {
    if !config.accounts.iter().any(|a| a.change_guard.is_some()) {
        return false;
    }
    let legacy: Vec<_> = config
        .accounts
        .iter_mut()
        .map(|a| a.change_guard.take().unwrap_or_default())
        .collect();
    if config.change_guard.is_none() {
        let mut merged = legacy[0].clone();
        for value in &legacy[1..] {
            merged.enabled |= value.enabled;
            merged.removals = merged.removals.min(value.removals);
            merged.removal_bytes = merged.removal_bytes.min(value.removal_bytes);
            merged.daily_removals = merged.daily_removals.min(value.daily_removals);
            merged.rewrites = merged.rewrites.min(value.rewrites);
            merged.rewrite_bytes = merged.rewrite_bytes.min(value.rewrite_bytes);
            merged.moves = merged.moves.min(value.moves);
            merged.uploads = merged.uploads.min(value.uploads);
            merged.upload_bytes = merged.upload_bytes.min(value.upload_bytes);
            merged.window_seconds = merged.window_seconds.max(value.window_seconds);
            merged.daily_window_seconds =
                merged.daily_window_seconds.max(value.daily_window_seconds);
        }
        config.change_guard = Some(merged);
    }
    true
}
#[cfg(test)]
mod tests;
