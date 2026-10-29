//! Token-scoped decisions over durable generation and hold membership sequences.
// test: a_snapshot_never_decides_later_generations_or_later_holds
// test: snapshot_tokens_survive_restart_and_reject_other_ledgers
use super::{rows, Change, ChangeGuard, Disposition, Kind};
use konedrive_dbus::rows::{GuardResult, HeldChange};
use konedrive_tree::{GuardRestoreFailure as Failure, LocalItemId};
use std::collections::BTreeSet;

impl ChangeGuard {
    pub(crate) fn subscribe_progress(
        &self,
    ) -> tokio::sync::broadcast::Receiver<(String, Vec<String>, u64, u64)> {
        self.progress.subscribe()
    }
    pub(crate) fn progress(&self, token: &str, kinds: &[String], done: u64, total: u64) {
        let _ = self
            .progress
            .send((token.into(), kinds.to_vec(), done, total));
    }

    /// One lock covers both the token and every row it describes, including sent history.
    /// Persist the epoch before publishing its first token, even for an empty ledger.
    pub(crate) fn snapshot(&self) -> Result<(String, Vec<HeldChange>), String> {
        let mut inner = crate::panic::lock(&self.inner);
        if let Some(error) = &inner.failed {
            return Err(error.clone());
        }
        if inner.saved_revision != Some(inner.ledger.revision) {
            self.save(&mut inner)?;
        }
        let held: Vec<_> = inner
            .ledger
            .current
            .values()
            .filter(|c| matches!(c.disposition, Disposition::Held | Disposition::Restoring))
            .collect();
        let kinds: BTreeSet<_> = held.iter().map(|c| c.kind).collect();
        let sent = inner.ledger.sent.iter().filter(|c| {
            let since = inner
                .ledger
                .hold_started
                .get(&c.kind)
                .copied()
                .unwrap_or_else(|| {
                    held.iter()
                        .filter(|h| h.kind == c.kind)
                        .map(|h| h.at)
                        .min()
                        .unwrap_or(i64::MAX)
                });
            kinds.contains(&c.kind)
                && c.at <= since
                && c.at
                    >= since.saturating_sub(inner.config.window_seconds.min(i64::MAX as u64) as i64)
        });
        let rows = rows::describe(held.iter().copied().chain(sent).collect());
        Ok((
            format!("{}:{}", inner.ledger.epoch, inner.ledger.revision),
            rows,
        ))
    }

    pub(crate) fn decide_batch(
        &self,
        token: &str,
        kinds: &[String],
        restore: bool,
    ) -> Result<(Vec<Change>, GuardResult), String> {
        let selected = Kind::parse_set(kinds)?;
        let mut inner = crate::panic::lock(&self.inner);
        let (epoch, revision) = token
            .rsplit_once(':')
            .ok_or("invalid change batch token; review the held changes again")?;
        let revision = revision
            .parse::<u64>()
            .map_err(|_| "invalid change batch token")?;
        if epoch != inner.ledger.epoch || revision > inner.ledger.revision {
            return Err("unknown change batch token; review the held changes again".into());
        }
        if let Some(error) = &inner.failed {
            return Err(error.clone());
        }
        let mut batch = Vec::new();
        let mut result = GuardResult::default();
        for c in inner.ledger.current.values_mut().filter(|c| {
            selected.contains(&c.kind)
                && c.disposition == Disposition::Held
                && c.first_held_revision <= revision
        }) {
            if c.revision > revision {
                result.failed += 1;
                result.failures.push((
                    c.item,
                    c.path.clone(),
                    Failure::Changed.to_string(),
                ));
                continue;
            }
            if c.provisional {
                result.failed += 1;
                result.failures.push((
                    c.item,
                    c.path.clone(),
                    Failure::Open.to_string(),
                ));
                continue;
            }
            batch.push(c.clone());
            c.disposition = if restore {
                Disposition::Restoring
            } else {
                Disposition::Allowed
            };
        }
        for kind in selected {
            if !inner.ledger.current.values().any(|c| {
                c.kind == kind
                    && matches!(c.disposition, Disposition::Held | Disposition::Restoring)
            }) {
                inner.ledger.latched.remove(&kind);
                inner.ledger.hold_started.remove(&kind);
            }
        }
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok((batch, result))
    }

    pub(crate) fn local_only(&self) -> BTreeSet<LocalItemId> {
        crate::panic::lock(&self.inner)
            .ledger
            .current
            .values()
            .filter(|c| c.disposition == Disposition::LocalOnly)
            .map(|c| LocalItemId::new(c.item))
            .collect()
    }

    /// The Issues Upload action re-enters normal admission, including the thresholds.
    /// A folder selects its local-only descendants; ancestors must also be creatable.
    pub(crate) fn upload_local(&self, path: &str, now: i64) -> Result<GuardResult, String> {
        let mut inner = crate::panic::lock(&self.inner);
        let root = inner
            .ledger
            .current
            .values()
            .find(|c| {
                c.path == path && c.kind == Kind::Upload && c.disposition == Disposition::LocalOnly
            })
            .cloned()
            .ok_or("no local-only upload at this path")?;
        let mut changes: Vec<_> = inner
            .ledger
            .current
            .values()
            .filter(|c| {
                c.kind == Kind::Upload
                    && c.disposition == Disposition::LocalOnly
                    && (c.path == path
                        || (root.directory && c.path.starts_with(&format!("{path}/")))
                        || (c.directory && path.starts_with(&format!("{}/", c.path))))
            })
            .cloned()
            .collect();
        let config = inner.config.clone();
        let mut result = GuardResult::default();
        for c in &mut changes {
            inner.ledger.current.remove(&c.key());
            c.disposition = Disposition::Pending;
            inner.ledger.observe(c.clone(), &config, now);
            result.done += 1;
        }
        self.save(&mut inner)?;
        self.notify(&inner);
        Ok(result)
    }
}
