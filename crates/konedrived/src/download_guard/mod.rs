//! Machine-wide admission of remote content requested by programs. Explicit Files calls
//! and pins never enter here. No tree, sync state, file paths or content are changed.

// test: ordinary_desktop_apps_read_without_counters_blocks_or_signals
// test: program_hosts_and_executables_keep_the_default_thirty_file_limit
// test: a_block_is_forwarded_to_the_private_bus_with_daemon_classification

mod downloads;
mod identity;
mod space;
mod read_ahead;
pub(crate) use read_ahead::ReadAheadCaller;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use identity::{Caller, BACKGROUND_READERS, PROGRAM_HOSTS, runs_programs};
pub(crate) use identity::Callers;
pub(crate) use identity::IdentifiedProcess;
#[cfg(test)]
pub(crate) use identity::{ProcessInfo, ProcessReader};
use konedrive_dbus::rows::GuardApp;
use serde::{Deserialize, Serialize};
pub(crate) use downloads::{AccountDownloads, AppDownloads, DownloadTicket};
pub(crate) use space::{DiskReservations, SpaceReservation};
use tokio::sync::broadcast;

/// `[download_guard_limits]`. Missing fields retain their defaults. Reader names replace
/// the built-in lists, so a deliberate config edit can override reader or host classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardConfig {
    pub max_files: u32,
    pub max_bytes: u64,
    pub window_seconds: u32,
    pub allowed_apps: Vec<String>,
    pub blocked_apps: Vec<String>,
    pub background_readers: Vec<String>,
    pub program_hosts: Vec<String>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            max_files: 30,
            max_bytes: 2 << 30,
            window_seconds: 600,
            allowed_apps: Vec::new(),
            blocked_apps: Vec::new(),
            background_readers: BACKGROUND_READERS.iter().map(|s| (*s).into()).collect(),
            program_hosts: PROGRAM_HOSTS.iter().map(|s| (*s).into()).collect(),
        }
    }
}

#[derive(Default)]
struct Window {
    files: HashMap<String, Instant>,
    bytes: VecDeque<(Instant, Instant, u64)>,
    total: u64,
}

impl Window {
    fn expire(&mut self, now: Instant, duration: Duration) {
        self.files.retain(|_, at| now.saturating_duration_since(*at) < duration);
        while self.bytes.front().is_some_and(|(_, at, _)| now.saturating_duration_since(*at) >= duration) {
            self.total -= self.bytes.pop_front().unwrap().2;
        }
    }
    /// Admission counts the requested bytes before network I/O, so parallel reads cannot
    /// race through the limit. Failures remain charged (a retry must not evade the guard).
    fn admit(&mut self, file: &str, bytes: u64, now: Instant, limits: &GuardConfig) -> bool {
        self.expire(now, Duration::from_secs(u64::from(limits.window_seconds)));
        let files = self.files.len() as u64 + u64::from(!self.files.contains_key(file));
        if files > u64::from(limits.max_files) || self.total.saturating_add(bytes) > limits.max_bytes {
            return false;
        }
        self.files.entry(file.into()).and_modify(|at| *at = (*at).max(now)).or_insert(now);
        self.total = self.total.saturating_add(bytes);
        // Coalesce within a second to bound the byte ledger even for tiny reads.
        if let Some((from, at, amount)) = self.bytes.back_mut() {
            if now.saturating_duration_since(*from) < Duration::from_secs(1) {
                *at = (*at).max(now);
                *amount += bytes;
                return true;
            }
        }
        self.bytes.push_back((now, now, bytes));
        true
    }
}

#[derive(Default)]
struct App {
    name: String,
    session_allowed: bool,
    blocked: Option<String>,
    block_id: u64,
    window: Window,
}

struct Inner {
    enabled: bool,
    limits: GuardConfig,
    apps: BTreeMap<String, App>,
}

pub struct DownloadGuard {
    read_ahead_changes: tokio::sync::watch::Sender<()>,
    pub(crate) changing: Mutex<()>,
    callers: Callers,
    inner: Mutex<Inner>,
    news: broadcast::Sender<GuardApp>,
    next_block: AtomicU64,
    pub(crate) downloads: Arc<AppDownloads>,
    pub(crate) disks: std::sync::Arc<DiskReservations>,
}

/// One whole-file transfer. Its known size was reserved at admission. Retries that
/// consume more are charged as they arrive; the already-started transfer may finish.
pub(crate) struct DownloadCharge {
    guard: Arc<DownloadGuard>,
    identity: String,
    included: u64,
    received: AtomicU64,
}
impl DownloadCharge {
    pub(crate) fn identity(&self) -> &str { &self.identity }
    pub(crate) fn received(&self, bytes: u64) {
        let before = self.received.fetch_add(bytes, Ordering::Relaxed);
        let extra = before.saturating_add(bytes).saturating_sub(self.included).saturating_sub(before.saturating_sub(self.included));
        if extra == 0 {
            return;
        }
        let mut inner = crate::panic::lock(&self.guard.inner);
        if !inner.enabled {
            return;
        }
        let limits = inner.limits.clone();
        let Some(app) = inner.apps.get_mut(&self.identity) else {
            return;
        };
        if !inner_enabled(&limits, app, &self.identity) {
            return;
        }
        let now = Instant::now();
        app.window.expire(now, Duration::from_secs(u64::from(limits.window_seconds)));
        app.window.total = app.window.total.saturating_add(extra);
        app.window.bytes.push_back((now, now, extra));
        if app.window.total > limits.max_bytes && app.blocked.as_deref() != Some("bulk") {
            app.blocked = Some("bulk".into());
            app.block_id = self.guard.next_block.fetch_add(1, Ordering::Relaxed);
            let _ = self.guard.news.send(row(&self.identity, app, &limits));
        }
    }
}
fn inner_enabled(limits: &GuardConfig, app: &App, identity: &str) -> bool {
    guarded(identity, limits) && !app.session_allowed && !limits.allowed_apps.iter().any(|id| id == identity)
}

fn guarded(identity: &str, limits: &GuardConfig) -> bool {
    !identity.starts_with("app:") || runs_programs(identity, &limits.program_hosts)
}

impl Default for DownloadGuard {
    fn default() -> Self {
        Self::new(true, GuardConfig::default())
    }
}

impl DownloadGuard {
    pub fn new(enabled: bool, limits: GuardConfig) -> Self {
        Self {
            read_ahead_changes: tokio::sync::watch::channel(()).0,
            changing: Mutex::new(()),
            callers: Callers::default(),
            inner: Mutex::new(Inner { enabled, limits, apps: BTreeMap::new() }),
            news: broadcast::channel(128).0,
            next_block: AtomicU64::new(1),
            disks: Default::default(),
            downloads: Default::default(),
        }
    }
    #[cfg(test)]
    pub(crate) fn set_process_reader(&mut self, reader: Arc<dyn identity::ProcessReader>) {
        self.callers = Callers::with_reader(reader);
    }

    pub(crate) fn configure(&self, enabled: bool, limits: GuardConfig) {
        let mut inner = crate::panic::lock(&self.inner);
        inner.enabled = enabled;
        // A host removed from the configured list becomes an ordinary app immediately;
        // its former automatic block and budget no longer have meaning.
        inner.apps.retain(|id, app| {
            if guarded(id, &limits) { return true; }
            app.window = Window::default();
            if app.blocked.as_deref() == Some("bulk") { app.blocked = None; }
            app.session_allowed || app.blocked.is_some() || limits.allowed_apps.contains(id) || limits.blocked_apps.contains(id)
        });
        inner.limits = limits;
        self.read_ahead_changes.send_replace(());
    }
    pub fn enabled(&self) -> bool {
        crate::panic::lock(&self.inner).enabled
    }
    pub fn limits(&self) -> GuardConfig {
        crate::panic::lock(&self.inner).limits.clone()
    }
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<GuardApp> {
        self.news.subscribe()
    }

    /// No counting at open: opening alone still downloads nothing. None identifies an
    /// uninspectable caller and is refused. The bridge is bypassed by its caller.
    pub(crate) fn check_open(&self, pid: u32) -> Result<(), fuser::Errno> {
        self.check(pid, None)
    }
    pub(crate) fn admit(&self, pid: u32, file: &str, bytes: u64) -> Result<(), fuser::Errno> {
        self.check(pid, Some((file, bytes)))
    }
    pub(crate) fn charge(self: &Arc<Self>, pid: u32, file: &str, bytes: u64) -> Result<Option<Arc<DownloadCharge>>, fuser::Errno> {
        let Some(who) = self.callers.identify(pid) else {
            return if self.enabled() { Err(fuser::Errno::EACCES) } else { Ok(None) };
        };
        let identity = who.identity.clone();
        self.decide(who, Some((file, bytes)), Instant::now())?;
        Ok(Some(Arc::new(DownloadCharge { guard: Arc::clone(self), identity, included: bytes, received: AtomicU64::new(0) })))
    }
    fn check(&self, pid: u32, fetch: Option<(&str, u64)>) -> Result<(), fuser::Errno> {
        if !self.enabled() {
            return Ok(());
        }
        let who = self.callers.identify(pid).unwrap_or_else(|| Caller {
            identity: "unidentified".into(),
            display_name: "Unknown process".into(),
            reader: "unidentified".into(),
        });
        self.decide(who, fetch, Instant::now())
    }
    fn decide(&self, who: Caller, fetch: Option<(&str, u64)>, now: Instant) -> Result<(), fuser::Errno> {
        let mut inner = crate::panic::lock(&self.inner);
        if !inner.enabled {
            return Ok(());
        }
        let limits = inner.limits.clone();
        // Ordinary desktop apps neither consume a budget nor create meaningless seen rows.
        // Background readers still take the same refusal path, even in an ordinary app unit.
        if who.reader != "unidentified" && !limits.background_readers.contains(&who.reader)
            && !guarded(&who.identity, &limits)
            && !limits.blocked_apps.contains(&who.identity) && !limits.allowed_apps.contains(&who.identity)
            && !inner.apps.get(&who.identity).is_some_and(|app| app.session_allowed)
        {
            return Ok(());
        }
        let app = inner.apps.entry(who.identity.clone()).or_default();
        app.name = who.display_name;
        let reason = if who.reader == "unidentified" {
            Some("unidentified")
        } else if limits.background_readers.contains(&who.reader) {
            Some("background")
        } else if app.session_allowed {
            return Ok(());
        } else if limits.blocked_apps.contains(&who.identity) {
            Some("user")
        } else if limits.allowed_apps.contains(&who.identity) {
            return Ok(());
        } else if !guarded(&who.identity, &limits) {
            return Ok(());
        } else if app.blocked.as_deref() == Some("bulk") {
            return Err(fuser::Errno::EACCES);
        } else if let Some((file, bytes)) = fetch {
            (!app.window.admit(file, bytes, now, &limits)).then_some("bulk")
        } else {
            None
        };
        if let Some(reason) = reason {
            if app.blocked.as_deref() != Some("bulk") && app.blocked.as_deref() != Some(reason) {
                app.blocked = Some(reason.into());
                app.block_id = self.next_block.fetch_add(1, Ordering::Relaxed);
                let _ = self.news.send(row(&who.identity, app, &limits));
            }
            self.read_ahead_changes.send_replace(());
            return Err(fuser::Errno::EACCES);
        }
        Ok(())
    }
    /// Always rules also appear before an app has run. Blocked entries retain the
    /// counters at refusal, so a later UI can explain the notification.
    pub fn apps(&self) -> Vec<GuardApp> {
        let mut inner = crate::panic::lock(&self.inner);
        let limits = inner.limits.clone();
        for id in limits.allowed_apps.iter().chain(&limits.blocked_apps) {
            inner.apps.entry(id.clone()).or_insert_with(|| App { name: id.clone(), ..App::default() });
        }
        let now = Instant::now();
        inner
            .apps
            .iter_mut()
            .map(|(id, app)| {
                if app.blocked.is_none() {
                    app.window.expire(now, Duration::from_secs(u64::from(limits.window_seconds)));
                }
                row(id, app, &limits)
            })
            .collect()
    }
    pub(crate) fn allow_session(&self, identity: &str) {
        let mut inner = crate::panic::lock(&self.inner);
        let app = inner.apps.entry(identity.into()).or_default();
        if app.name.is_empty() {
            app.name = identity.into();
        }
        app.session_allowed = true;
        app.blocked = None;
        self.read_ahead_changes.send_replace(());
    }
    pub(crate) fn reset(&self, identity: &str) {
        crate::panic::lock(&self.inner).apps.remove(identity);
        self.read_ahead_changes.send_replace(());
    }
}

fn row(identity: &str, app: &App, limits: &GuardConfig) -> GuardApp {
    let policy = if app.session_allowed {
        "allowed-session"
    } else if limits.blocked_apps.iter().any(|id| id == identity) {
        "blocked-always"
    } else if limits.allowed_apps.iter().any(|id| id == identity) {
        "allowed-always"
    } else {
        "automatic"
    };
    GuardApp {
        identity: identity.into(),
        display_name: app.name.clone(),
        policy: policy.into(),
        blocked: app.blocked.is_some() || policy == "blocked-always",
        reason: app.blocked.clone().unwrap_or_else(|| if policy == "blocked-always" { "user".into() } else { String::new() }),
        files: app.window.files.len() as u64,
        bytes: app.window.total,
        runs_programs: runs_programs(identity, &limits.program_hosts),
        block_id: app.block_id,
    }
}

#[cfg(test)]
mod tests;
