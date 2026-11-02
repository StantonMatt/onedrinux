//! Caller facts from procfs, never from the mounted folder. Arguments are not published.

// test: t3_agents_konsole_children_and_flatpak_scopes_keep_their_identities
// test: host_matching_keeps_launch_prefixes_case_and_jetbrains_rules
// test: thumbnail_helpers_keep_the_background_role_without_blocking_ordinary_ghostscript
// test: dolphins_metadata_helper_is_a_background_reader_in_dolphins_unit

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub identity: String,
    pub display_name: String,
    pub reader: String,
}

/// Executable basenames and the special KIO thumbnail role. Dolphin's own MIME sniffing
/// is automatic (docs/kio-behavior.md C); file.so copies are not thumbnail requests.
/// baloo-widgets starts its metadata extractor per hovered or inspected file.
pub(super) const BACKGROUND_READERS: &[&str] = &[
    "dolphin",
    "kio-thumbnail",
    "baloo_file",
    "baloo_file_extractor",
    "baloo_filemetadata_temp_extractor",
    "tracker-extract",
    "tracker-extract-3",
    "tracker-miner-fs",
    "tracker-miner-fs-3",
    "localsearch-extractor",
    "localsearch-3",
    "tumblerd",
    "ffmpegthumbnailer",
];

/// Desktop ids whose app units are shared with programs they run. A final `*` matches
/// a prefix; Flatpak/GNOME launcher prefixes and case are ignored when matching.
pub(super) const PROGRAM_HOSTS: &[&str] = &[
    "org.kde.konsole",
    "org.kde.yakuake",
    "org.kde.kate",
    "kitty",
    "alacritty",
    "org.alacritty.alacritty",
    "org.gnome.terminal",
    "org.gnome.ptyxis",
    "com.mitchellh.ghostty",
    "org.wezfurlong.wezterm",
    "foot",
    "footclient",
    "com.gexperts.tilix",
    "terminator",
    "xterm",
    "debian-xterm",
    "debian-uxterm",
    "code",
    "code-oss",
    "code-insiders",
    "codium",
    "com.visualstudio.code",
    "com.vscodium.codium",
    "org.kde.kdevelop",
    "com.t3tools.t3code",
    "t3code",
    "t3code-url-handler",
    "t3-code-nightly",
    "codex-shared-client",
    "com.anthropic.claude",
    "chatgpt",
    "cursor",
    "com.todesktop.230313mzl4w4u92",
    "zed",
    "dev.zed.zed",
    "jetbrains-*",
    "com.jetbrains.*",
];

pub(super) fn runs_programs(identity: &str, hosts: &[String]) -> bool {
    let Some(id) = identity.strip_prefix("app:") else { return false };
    let id = id.to_lowercase();
    let id = id.strip_prefix("flatpak-").or_else(|| id.strip_prefix("gnome-")).unwrap_or(&id);
    hosts.iter().any(|host| {
        let host = host.to_lowercase();
        host.strip_suffix('*').map_or(id == host, |prefix| id.starts_with(prefix))
    })
}

/// The procfs seam also lets launch-intent tests supply argv without inspecting a
/// real process. The production reader checks the start tick around its snapshot.
pub(crate) trait ProcessReader: Send + Sync {
    fn read(&self, pid: u32) -> Option<ProcessInfo>;
}

#[derive(Clone)]
pub(crate) struct ProcessInfo {
    pub(crate) start: u64,
    pub(crate) parent: u32,
    pub(crate) executable: String,
    pub(crate) cgroup: String,
    pub(crate) cmdline: Arc<Vec<u8>>,
}

struct ProcReader;
impl ProcessReader for ProcReader {
    fn read(&self, pid: u32) -> Option<ProcessInfo> {
        let root = format!("/proc/{pid}");
        let stat = std::fs::read_to_string(format!("{root}/stat")).ok()?;
        let start = start_time(&stat)?;
        let parent = parent_pid(&stat)?;
        let executable = std::fs::read_link(format!("{root}/exe")).ok()?.to_str()?.to_owned();
        let cgroup = std::fs::read_to_string(format!("{root}/cgroup")).ok()?;
        let mut cmdline = Vec::new();
        File::open(format!("{root}/cmdline")).ok()?.take(65537).read_to_end(&mut cmdline).ok()?;
        let after = std::fs::read_to_string(format!("{root}/stat")).ok()?;
        if cmdline.len() > 65536 || start_time(&after)? != start || parent_pid(&after)? != parent {
            return None;
        }
        Some(ProcessInfo { start, parent, executable, cgroup, cmdline: Arc::new(cmdline) })
    }
}

pub(crate) struct Callers {
    cache: Mutex<CallerCache>,
    reader: Arc<dyn ProcessReader>,
}

#[derive(Default)]
struct CallerCache {
    by_pid: HashMap<u32, Cached>,
    next: u64,
}

struct Cached {
    process: ProcessInfo,
    reader: String,
    caller: Caller,
    generation: u64,
}

/// An identity generation, rather than a bare pid, survives neither exec, PID reuse,
/// a scope/role change nor a failed inspection. Arguments stay private to the daemon.
pub(crate) struct IdentifiedProcess {
    pub(crate) caller: Caller,
    pub(crate) generation: u64,
    pub(crate) cmdline: Arc<Vec<u8>>,
}

impl Default for Callers {
    fn default() -> Self { Self { cache: Mutex::default(), reader: Arc::new(ProcReader) } }
}

impl Callers {
    #[cfg(test)]
    pub(crate) fn with_reader(reader: Arc<dyn ProcessReader>) -> Self {
        Self { cache: Mutex::default(), reader }
    }

    /// Revalidate start tick, exe, unit and KIO role on every request. The bounded
    /// cache saves normalization and launch parsing, never identity validation.
    pub fn identify(&self, pid: u32) -> Option<Caller> {
        self.process(pid).map(|process| process.caller)
    }

    pub(crate) fn process(&self, pid: u32) -> Option<IdentifiedProcess> {
        let Some(process) = self.reader.read(pid) else {
            crate::panic::lock(&self.cache).by_pid.remove(&pid);
            return None;
        };
        let mut reader = reader_role(&process.executable, &process.cmdline);
        // KIO's PDF thumbnailer delegates to Ghostscript, which opens the file
        // itself. Preserve the thumbnail role for a direct helper, using only
        // procfs facts; the same executable used elsewhere retains its own role.
        if reader == "gs" && process.parent != 0 && self.reader.read(process.parent).is_some_and(|parent| {
            reader_role(&parent.executable, &parent.cmdline) == "kio-thumbnail"
        }) {
            // The parent snapshot must still belong to this child generation.
            let Some(current) = self.reader.read(pid) else {
                crate::panic::lock(&self.cache).by_pid.remove(&pid);
                return None;
            };
            if current.start != process.start || current.parent != process.parent
                || current.executable != process.executable || current.cgroup != process.cgroup
                || current.cmdline != process.cmdline {
                crate::panic::lock(&self.cache).by_pid.remove(&pid);
                return None;
            }
            reader = "kio-thumbnail".into();
        }
        let mut cache = crate::panic::lock(&self.cache);
        if let Some(old) = cache.by_pid.get(&pid) {
            if old.process.start == process.start && old.process.executable == process.executable
                && old.process.cgroup == process.cgroup && old.process.parent == process.parent && old.reader == reader {
                return Some(IdentifiedProcess { caller: old.caller.clone(), generation: old.generation, cmdline: Arc::clone(&old.process.cmdline) });
            }
        }
        let caller = caller(&process.executable, &process.cgroup, reader.clone());
        if cache.by_pid.len() >= 1024 { cache.by_pid.clear(); }
        cache.next += 1;
        let generation = cache.next;
        let cmdline = Arc::clone(&process.cmdline);
        cache.by_pid.insert(pid, Cached { process, reader, caller: caller.clone(), generation });
        Some(IdentifiedProcess { caller, generation, cmdline })
    }
}

fn start_time(stat: &str) -> Option<u64> {
    // comm may contain spaces and ')'; field 22 is index 19 after the last ')'.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

fn parent_pid(stat: &str) -> Option<u32> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

fn basename(path: &str) -> &str {
    Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path)
}

fn reader_role(executable: &str, cmdline: &[u8]) -> String {
    let name = basename(executable).trim_end_matches(" (deleted)");
    if matches!(name, "kioworker" | "kioslave5" | "kioslave") {
        let thumbnail = cmdline.split(|b| *b == 0).skip(1).any(|arg| {
            let arg = String::from_utf8_lossy(arg);
            // KF6 uses a plugin path; older versions use the protocol name.
            arg == "thumbnail" || basename(&arg) == "thumbnail.so"
        });
        if thumbnail {
            return "kio-thumbnail".into();
        }
    }
    name.to_owned()
}

fn caller(executable: &str, cgroup: &str, reader: String) -> Caller {
    let app = cgroup.lines().filter_map(|line| line.splitn(3, ':').nth(2)).flat_map(|path| path.split('/')).rev().find_map(app_unit);
    let (identity, display_name) = match app {
        Some(app) => (format!("app:{app}"), app),
        None => (format!("exe:{executable}"), basename(executable).to_owned()),
    };
    // A shared Dolphin app unit must not turn file.so copies into background sniffing.
    Caller { identity, display_name, reader }
}

fn app_unit(unit: &str) -> Option<String> {
    let unit = unit.strip_prefix("app-")?;
    let app = if let Some(service) = unit.strip_suffix(".service") {
        service.split_once('@').map_or(service, |(name, _)| name)
    } else if let Some(scope) = unit.strip_suffix(".scope") {
        // systemd's final component is the launch instance, not part of the desktop id.
        scope.rsplit_once('-')?.0
    } else {
        return None;
    };
    (!app.is_empty()).then(|| unescape_unit(app))
}

fn unescape_unit(text: &str) -> String {
    let mut bytes = Vec::new();
    let mut rest = text.as_bytes();
    while !rest.is_empty() {
        if rest.starts_with(b"\\x") && rest.len() >= 4 {
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&rest[2..4]).unwrap_or(""), 16) {
                bytes.push(byte);
                rest = &rest[4..];
                continue;
            }
        }
        bytes.push(rest[0]);
        rest = &rest[1..];
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests;
