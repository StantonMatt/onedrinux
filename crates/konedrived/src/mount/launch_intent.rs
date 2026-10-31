//! A desktop process's bounded argv supplies launch intent without opening paths.
//!
//! Seen process generations are parsed once per mount (up to 1,024 remembered PIDs).
//! Cloud file intents last 30 seconds; the immediate emblem lasts ten without a read.
//! A single timer per process validates its identity and expires its intents. Neither
//! lookup nor expiry touches a mounted path or asks the content source.
// test: launch_intent_absolute_paths_and_file_urls_show_immediately_and_land_during_scans
// test: launch_intent_expires_emblems_and_rejects_dead_or_changed_processes
// test: launch_intent_paths_are_bounded_lexical_and_confined_to_the_mount

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use konedrive_tree::{ContentPresence, ItemKind};
use tokio::time::Instant;

use super::item_download_states::{DownloadKey, ItemDownloadStates};
use super::range_read_states::LaunchEmblem;
use super::read_connections::ReadConnections;
use crate::download_guard::{DownloadGuard, IdentifiedProcess};

const KEEP_FOR: Duration = Duration::from_secs(30);
const SHOW_FOR: Duration = Duration::from_secs(10);
const CHECK_PROCESS: Duration = Duration::from_secs(1);
const PROCESSES_KEPT: usize = 1024;

struct Intent {
    at: Instant,
    emblem: Option<LaunchEmblem>,
}

struct Process {
    generation: u64,
    identity: String,
    intents: HashMap<DownloadKey, Intent>,
    timer: Option<tokio::task::AbortHandle>,
}

#[derive(Default)]
struct State {
    processes: HashMap<u32, Process>,
    stopped: bool,
}

pub(super) struct LaunchIntents {
    root: PathBuf,
    connections: Arc<ReadConnections>,
    guard: Arc<DownloadGuard>,
    downloads: Arc<ItemDownloadStates>,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
}

impl LaunchIntents {
    pub(super) fn new(
        root: PathBuf,
        connections: Arc<ReadConnections>,
        guard: Arc<DownloadGuard>,
        downloads: Arc<ItemDownloadStates>,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            root,
            connections,
            guard,
            downloads,
            runtime,
            state: Mutex::default(),
        })
    }

    /// Called synchronously on PID-bearing metadata, directory, open and read requests.
    /// Resolve only lexical mount-relative names, through the local store's index.
    pub(super) fn observe(self: &Arc<Self>, pid: u32) {
        let Some(process) = self.guard.launch_process(pid) else {
            self.forget(pid);
            return;
        };
        let mut state = crate::panic::lock(&self.state);
        if state.stopped
            || state
                .processes
                .get(&pid)
                .is_some_and(|old| old.generation == process.generation)
        {
            return;
        }
        if let Some(old) = state.processes.remove(&pid) {
            if let Some(timer) = old.timer {
                timer.abort();
            }
        }
        if state.processes.len() >= PROCESSES_KEPT {
            // Bound both timers and argv snapshots; eviction only loses intent.
            if let Some(old) = state.processes.keys().next().copied() {
                if let Some(old) = state.processes.remove(&old) {
                    if let Some(timer) = old.timer {
                        timer.abort();
                    }
                }
            }
        }
        let paths = paths(&process.cmdline, &self.root);
        let keys = self
            .connections
            .read(|reader| {
                let mut keys = Vec::new();
                for path in paths {
                    let Some(row) = reader.find_item_by_path(&path)? else {
                        continue;
                    };
                    if row.kind != ItemKind::File
                        || row.content_presence != ContentPresence::NotOnDisk
                        || row.onedrive_item.is_none()
                    {
                        continue;
                    }
                    if let Some(content) = row.local.and_then(|local| local.content) {
                        keys.push(DownloadKey {
                            item: row.local_item_id,
                            content,
                        });
                    }
                }
                Ok(keys)
            })
            .unwrap_or_default();
        let mut intents = HashMap::new();
        for key in keys {
            tracing::debug!(item = %key.item, identity = process.caller.identity, "launch intent found");
            intents.entry(key.clone()).or_insert_with(|| Intent {
                at: Instant::now(),
                emblem: Some(self.downloads.range_reads.show_launch(key.item)),
            });
        }
        let has_intents = !intents.is_empty();
        let generation = process.generation;
        state.processes.insert(
            pid,
            Process {
                generation,
                identity: process.caller.identity,
                intents,
                timer: None,
            },
        );
        if has_intents {
            let weak = Arc::downgrade(self);
            let task = self.runtime.spawn(async move {
                loop {
                    tokio::time::sleep(CHECK_PROCESS).await;
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    if !this.refresh(pid, generation) {
                        return;
                    }
                }
            });
            state.processes.get_mut(&pid).unwrap().timer = Some(task.abort_handle());
        }
    }

    fn refresh(&self, pid: u32, generation: u64) -> bool {
        let current = self.guard.launch_process(pid);
        let mut state = crate::panic::lock(&self.state);
        let Some(process) = state
            .processes
            .get_mut(&pid)
            .filter(|p| p.generation == generation)
        else {
            return false;
        };
        if !matches!(current, Some(IdentifiedProcess { generation: current, .. }) if current == generation)
        {
            state.processes.remove(&pid);
            return false;
        }
        process.intents.retain(|_, intent| {
            if intent.at.elapsed() >= SHOW_FOR {
                intent.emblem.take();
            }
            intent.at.elapsed() < KEEP_FOR
        });
        !process.intents.is_empty()
    }

    /// Identity and generation must still match. Keep the emblem alive while starting
    /// the download: start_or_join inherits it, so the handover cannot flash cloud-only.
    pub(super) fn take(&self, pid: u32, key: &DownloadKey) -> Option<Vec<LaunchEmblem>> {
        let Some(current) = self.guard.launch_process(pid) else {
            self.forget(pid);
            return None;
        };
        let candidates: Vec<_> = crate::panic::lock(&self.state)
            .processes
            .iter()
            .filter(|(_, process)| {
                process.identity == current.caller.identity && process.intents.contains_key(key)
            })
            .map(|(owner, process)| (*owner, process.generation))
            .collect();
        let mut consumed = false;
        let mut emblems = Vec::new();
        for (owner, generation) in candidates {
            if self
                .guard
                .launch_process(owner)
                .is_none_or(|process| process.generation != generation)
            {
                self.forget(owner);
                continue;
            }
            let mut state = crate::panic::lock(&self.state);
            let Some(process) = state
                .processes
                .get_mut(&owner)
                .filter(|process| process.generation == generation)
            else {
                continue;
            };
            if let Some(intent) = process.intents.remove(key) {
                if intent.at.elapsed() < KEEP_FOR {
                    consumed = true;
                    emblems.extend(intent.emblem);
                }
            }
        }
        consumed.then_some(emblems)
    }

    fn forget(&self, pid: u32) {
        if let Some(process) = crate::panic::lock(&self.state).processes.remove(&pid) {
            if let Some(timer) = process.timer {
                timer.abort();
            }
        }
    }

    pub(super) fn stop(&self) {
        let mut state = crate::panic::lock(&self.state);
        state.stopped = true;
        for (_, process) in state.processes.drain() {
            if let Some(timer) = process.timer {
                timer.abort();
            }
        }
    }
}

impl Drop for LaunchIntents {
    fn drop(&mut self) {
        self.stop();
    }
}

/// URL decoding and confinement are lexical. In particular, never canonicalize, stat,
/// or open an argv path, inside or outside the mount. Reject parent traversal outright.
fn paths(cmdline: &[u8], root: &Path) -> Vec<PathBuf> {
    cmdline
        .split(|byte| *byte == 0)
        .take(64)
        .filter(|arg| arg.len() <= 4096)
        .filter_map(|arg| {
            let arg = std::str::from_utf8(arg).ok()?;
            let path = if arg.starts_with("file://") {
                url::Url::parse(arg).ok()?.to_file_path().ok()?
            } else {
                PathBuf::from(arg)
            };
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir))
            {
                return None;
            }
            let relative = path.strip_prefix(root).ok()?;
            (!relative.as_os_str().is_empty()).then(|| relative.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests;
