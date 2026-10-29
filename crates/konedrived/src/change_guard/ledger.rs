//! Durable values and pure window accounting, independent of the mount and the bus.
// test: thresholds_latch_the_whole_pending_batch
// test: daily_removals_and_original_bytes_are_charged
// test: local_only_edits_update_generations_without_charging_until_upload_local

use super::GuardConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Kind {
    Removal,
    Rewrite,
    Move,
    Upload,
}
impl Kind {
    pub(super) const ALL: [Self; 4] = [Self::Removal, Self::Rewrite, Self::Move, Self::Upload];
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Removal => "removal",
            Self::Rewrite => "rewrite",
            Self::Move => "move",
            Self::Upload => "upload",
        }
    }
    pub(crate) fn parse_set(kinds: &[String]) -> Result<BTreeSet<Self>, String> {
        if kinds.is_empty() {
            return Ok([Self::Removal, Self::Rewrite, Self::Move, Self::Upload].into());
        }
        kinds
            .iter()
            .map(|k| match k.as_str() {
                "removal" => Ok(Self::Removal),
                "rewrite" => Ok(Self::Rewrite),
                "move" => Ok(Self::Move),
                "upload" => Ok(Self::Upload),
                _ => Err(format!(
                    "unknown change kind {k:?}; expected removal, rewrite, move or upload"
                )),
            })
            .collect()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Disposition {
    Pending,
    Held,
    Allowed,
    Restoring,
    LocalOnly,
    Sent,
}
impl Disposition {
    pub(super) fn blocks(self) -> bool {
        matches!(self, Self::Held | Self::Restoring | Self::LocalOnly)
    }
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Held => "held",
            Self::Allowed => "allowed",
            Self::Restoring => "restoring",
            Self::LocalOnly => "local-only",
            Self::Sent => "sent",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Change {
    pub(crate) item: i64,
    pub(crate) kind: Kind,
    pub(crate) path: String,
    pub(crate) bytes: u64,
    #[serde(default)]
    pub(crate) after_bytes: u64,
    #[serde(default)]
    pub(crate) old_path: String,
    #[serde(default)]
    pub(crate) directory: bool,
    /// Number of descendants of a moved directory, including unchanged descendants.
    #[serde(default)]
    pub(crate) descendants: u64,
    /// Last membership/generation change, in the ledger's persistent sequence.
    #[serde(default)]
    pub(crate) revision: u64,
    #[serde(default)]
    pub(crate) first_held_revision: u64,
    pub(crate) at: i64,
    pub(crate) generation: String,
    /// Separate remote identity for explaining a refusal; older ledgers lack it.
    #[serde(default)]
    pub(crate) remote_generation: Option<String>,
    #[serde(default)]
    pub(crate) write_count: Option<u64>,
    #[serde(default)]
    pub(crate) provisional: bool,
    pub(super) disposition: Disposition,
}
impl Change {
    pub(super) fn key(&self) -> String {
        format!("{}:{}", self.item, self.kind.key())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Counter {
    pub(super) at: i64,
    pub(super) kind: Kind,
    pub(super) count: u64,
    pub(super) bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Retained {
    pub(super) path: String,
    pub(super) content: Option<konedrive_tree::ContentVersion>,
    #[serde(default)]
    pub(super) bound: bool,
    #[serde(default)]
    pub(super) cleanup: bool,
}
/// Written before the SQL commit. Only these exact resulting rows prove completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RestoreReceipt {
    pub(super) revision: u64,
    pub(super) rows: Vec<(i64, String, Option<u64>)>,
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct Ledger {
    #[serde(default)]
    pub(super) epoch: String,
    #[serde(default)]
    pub(super) revision: u64,
    pub(super) current: BTreeMap<String, Change>,
    #[serde(default)]
    pub(super) retained: BTreeMap<i64, Retained>,
    #[serde(default)]
    pub(super) restored: BTreeMap<String, RestoreReceipt>,
    #[serde(default)]
    pub(super) cleanup_refs: BTreeMap<i64, (String, String, String)>,
    pub(super) events: Vec<Counter>,
    pub(super) latched: BTreeSet<Kind>,
    #[serde(default)]
    pub(super) hold_started: BTreeMap<Kind, i64>,
    pub(super) sent: Vec<Change>,
    /// Wall time is clamped to the latest observation. A backwards clock cannot expire
    /// a counter or finish a deletion quiet period early.
    clock: i64,
}
pub(super) fn new_epoch() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("the OS random number generator failed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
impl Ledger {
    pub(super) fn clock_now(&self) -> i64 { self.clock }
    pub(super) fn blocks(&self, item: i64) -> bool {
        Kind::ALL.into_iter().any(|kind| self.current.get(&format!("{item}:{}", kind.key())).is_some_and(|c| c.disposition.blocks()))
    }

    pub(super) fn observe(&mut self, mut change: Change, config: &GuardConfig, now: i64) -> bool {
        let now = now.max(self.clock);
        self.clock = now;
        if let Some(previous) = self.current.get_mut(&change.key()).filter(|c| {
            !c.provisional
                && c.generation == change.generation
                && c.path == change.path
                && (change.write_count.is_none() || c.write_count == change.write_count)
        }) {
            // Enrich an observation (including a legacy ledger) without charging the
            // same generation twice. Mount-only readings do not erase an old path.
            let mut changed = previous.directory != change.directory
                || previous.after_bytes != change.after_bytes;
            previous.directory = change.directory;
            previous.after_bytes = change.after_bytes;
            if !change.old_path.is_empty()
                && (previous.old_path != change.old_path
                    || previous.descendants != change.descendants)
            {
                previous.old_path = change.old_path;
                previous.descendants = change.descendants;
                changed = true;
            }
            return changed;
        }

        self.revision = self
            .revision
            .checked_add(1)
            .expect("change guard sequence exhausted");
        change.revision = self.revision;
        change.first_held_revision = self
            .current
            .get(&change.key())
            .filter(|c| matches!(c.disposition, Disposition::Held | Disposition::Restoring))
            .map_or(self.revision, |c| c.first_held_revision);
        if change.kind == Kind::Removal {
            if let Some(previous) = self
                .current
                .get_mut(&change.key())
                .filter(|c| matches!(c.disposition, Disposition::Pending | Disposition::Held))
            {
                // prepare_removal precedes SQLite's unsent stamp. Finishing that same
                // unlink is not a second removal and does not restart its quiet time.
                previous.generation = change.generation;
                previous.remote_generation = change.remote_generation;
                previous.revision = change.revision;
                previous.provisional = false;
                previous.directory = change.directory;
                previous.old_path = change.old_path;
                return true;
            }
        }
        change.at = now;
        if self.current.get(&change.key()).is_some_and(|c| c.disposition == Disposition::LocalOnly) {
            // Local-only bytes are not outgoing work. Track their latest generation
            // without charging admission until UploadLocal removes this disposition.
            change.disposition = Disposition::LocalOnly;
            self.current.insert(change.key(), change);
            return true;
        }
        let retained_for = config.window_seconds.max(config.daily_window_seconds);
        self.events
            .retain(|c| (now.saturating_sub(c.at) as u64) < retained_for);
        self.sent.retain(|c| now.saturating_sub(c.at) < 86400);
        let kind = change.kind;
        // One bucket per kind and second bounds the counter ledger, including a
        // script that repeatedly rewrites one item while it is already held.
        if let Some(bucket) = self
            .events
            .iter_mut()
            .rev()
            .find(|c| c.at == now && c.kind == kind)
        {
            bucket.count = bucket.count.saturating_add(1);
            bucket.bytes = bucket.bytes.saturating_add(change.bytes);
        } else {
            self.events.push(Counter {
                at: now,
                kind,
                count: 1,
                bytes: change.bytes,
            });
        }
        let recent = self.events.iter().filter(|c| {
            c.kind == kind && (now.saturating_sub(c.at) as u64) < config.window_seconds
        });
        // Each new generation counts, including repeated edits of the same file.
        let (count, bytes) = recent.fold((0u64, 0u64), |(n, b), c| {
            (n.saturating_add(c.count), b.saturating_add(c.bytes))
        });
        let over = match kind {
            Kind::Removal => {
                count > config.removals
                    || bytes > config.removal_bytes
                    || self
                        .events
                        .iter()
                        .filter(|c| {
                            c.kind == kind
                                && (now.saturating_sub(c.at) as u64) < config.daily_window_seconds
                        })
                        .fold(0u64, |n, c| n.saturating_add(c.count))
                        > config.daily_removals
            }
            Kind::Rewrite => count > config.rewrites || bytes > config.rewrite_bytes,
            Kind::Move => count > config.moves,
            Kind::Upload => count > config.uploads || bytes > config.upload_bytes,
        };
        let newly_latched = config.enabled && over && self.latched.insert(kind);
        if config.enabled && over {
            self.hold_started.entry(kind).or_insert(now);
        }
        if config.enabled && self.latched.contains(&kind) {
            change.disposition = Disposition::Held;
        }
        if self
            .current
            .get(&change.key())
            .is_some_and(|c| matches!(c.disposition, Disposition::Held | Disposition::Restoring))
        {
            change.disposition = Disposition::Held;
        }
        self.current.insert(change.key(), change);
        if newly_latched {
            for c in self
                .current
                .values_mut()
                .filter(|c| c.kind == kind && c.disposition == Disposition::Pending)
            {
                c.disposition = Disposition::Held;
                c.revision = self.revision;
                c.first_held_revision = self.revision;
            }
        }
        true
    }

    /// The removal of a file OneDrive never had sends nothing, so it is never charged. It
    /// waits with the removals around it and joins their hold, or `held` when its own
    /// upload was held, so a restore can put it back with them; otherwise the start or a
    /// plan retires it once the removals' quiet time is over (`retire_unsent_removals`).
    pub(super) fn observe_unsent_removal(&mut self, mut change: Change, config: &GuardConfig, now: i64, held: bool) {
        let now = now.max(self.clock);
        self.clock = now;
        self.revision = self
            .revision
            .checked_add(1)
            .expect("change guard sequence exhausted");
        change.revision = self.revision;
        change.first_held_revision = self.revision;
        change.at = now;
        if held || (config.enabled && self.latched.contains(&Kind::Removal)) {
            change.disposition = Disposition::Held;
        }
        self.current.insert(change.key(), change);
    }
}

/// Atomic replace with both file and parent-directory sync. Admission fails closed if
/// any step fails; a corrupt existing ledger is never overwritten by a fresh one.
pub(super) fn write(file: &Path, ledger: &Ledger) -> Result<(), String> {
    let write = || -> anyhow::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = file
            .parent()
            .ok_or_else(|| anyhow::anyhow!("change guard path has no parent"))?;
        std::fs::create_dir_all(dir)?;
        let temporary = file.with_extension("json.tmp");
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        out.write_all(&serde_json::to_vec(ledger)?)?;
        out.sync_all()?;
        std::fs::rename(temporary, file)?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    };
    write().map_err(|e| format!("cannot persist change guard {}: {e}", file.display()))
}
