//! The account's write allowlist. Paths are relative to the drive, compared by whole
//! components with the exact spelling shown by the mount. Invalid configuration denies
//! every write; absence alone means unrestricted.
// test: write_scope_checks_components_and_fails_closed
// test: write_scope_config_round_trips_and_keeps_existing_modes
// test: write_scope_source_follows_persisted_changes_and_missing_accounts_deny

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::{AccountId, ConfigStore};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriteScope {
    paths: Option<Vec<PathBuf>>,
}

impl WriteScope {
    pub(crate) fn parse(paths: Option<&[String]>) -> Result<Self, String> {
        let Some(paths) = paths else { return Ok(Self::default()) };
        let mut kept = Vec::new();
        for text in paths {
            let path = Path::new(text);
            if text.is_empty() || text.contains('\0') || text.contains('\\') || !path.components().all(|part| matches!(part, Component::Normal(_))) {
                return Err(format!(
                    "{text:?}: write paths must be nonempty relative subtrees without '.', '..' or backslashes"
                ));
            }
            // components() normalizes embedded '.', so reject those explicitly too.
            if text.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
                return Err(format!("{text:?}: write paths must contain only folder names"));
            }
            if !kept.contains(&path.to_path_buf()) {
                kept.push(path.to_path_buf());
            }
        }
        Ok(Self { paths: Some(kept) })
    }

    pub(crate) fn denied() -> Self {
        Self { paths: Some(Vec::new()) }
    }

    pub(crate) fn unrestricted(&self) -> bool {
        self.paths.is_none()
    }

    pub(crate) fn allows(&self, path: &Path) -> bool {
        if path.components().any(|part| !matches!(part, Component::Normal(_))) {
            return false;
        }
        self.paths.as_ref().is_none_or(|paths| paths.iter().any(|allowed| path.starts_with(allowed)))
    }
}

/// Read afresh at admission, including on a mount kept across a stop/start.
#[derive(Clone)]
pub(crate) struct WriteScopeSource {
    read: Arc<dyn Fn() -> WriteScope + Send + Sync>,
    pub(crate) refusals: Arc<super::write_refusals::WriteRefusals>,
}

impl Default for WriteScopeSource {
    fn default() -> Self {
        Self::new(WriteScope::default)
    }
}

impl WriteScopeSource {
    pub(crate) fn new(read: impl Fn() -> WriteScope + Send + Sync + 'static) -> Self {
        Self { read: Arc::new(read), refusals: Arc::new(Default::default()) }
    }
    pub(crate) fn get(&self) -> WriteScope {
        (self.read)()
    }
}

impl ConfigStore {
    pub(crate) fn write_scope(self: &Arc<Self>, id: &AccountId) -> WriteScopeSource {
        let (store, id) = (Arc::clone(self), id.clone());
        let refusals = store.write_refusals(&id);
        let read = Arc::new(move || {
            store
                .account(&id)
                .and_then(|account| WriteScope::parse(account.write_paths.as_deref()).ok())
                .unwrap_or_else(WriteScope::denied)
        });
        WriteScopeSource { read, refusals }
    }
}

#[cfg(test)]
mod tests;
