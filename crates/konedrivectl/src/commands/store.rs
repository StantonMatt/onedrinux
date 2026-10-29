//! Offline store recovery. Take the same configuration lock as the daemon, including
//! older builds that do not yet take the per-store lock. No bus or account is contacted.
// test: store_reset_is_an_offline_command_with_an_explicit_path

use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{anyhow, Context};

use crate::cli::StoreCmd;

pub(crate) fn store(command: StoreCmd) -> anyhow::Result<()> {
    let StoreCmd::Reset { path, config } = command;
    let config = config
        .or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })?;
            Some(base.join("konedrive/config.toml"))
        })
        .ok_or_else(|| anyhow!("pass --config with the daemon's config.toml"))?;
    let mut lock_path = config.as_os_str().to_owned();
    lock_path.push(".lock");
    // Never create a guessed configuration directory. A mistyped path is an error.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .with_context(|| format!("cannot open the daemon lock beside {}", config.display()))?;
    lock.try_lock()
        .context("the daemon is running; stop it before resetting its store")?;
    let quarantine = konedrive_tree::TreeStore::quarantine_for_reset(&path)?;
    println!("Store preserved at {}. Start the daemon to recover readable pins and export pending removals and moves.", quarantine.display());
    Ok(())
}
