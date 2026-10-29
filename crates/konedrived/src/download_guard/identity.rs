//! Caller facts from procfs, never from the mounted folder. Arguments are not published.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub identity: String,
    pub display_name: String,
    pub reader: String,
}

/// Executable basenames and the special KIO thumbnail role. Dolphin's own MIME sniffing
/// is automatic (docs/kio-behavior.md C); file.so copies are not thumbnail requests.
pub(super) const BACKGROUND_READERS: &[&str] = &[
    "dolphin",
    "kio-thumbnail",
    "baloo_file",
    "baloo_file_extractor",
    "tracker-extract",
    "tracker-extract-3",
    "tracker-miner-fs",
    "tracker-miner-fs-3",
    "localsearch-extractor",
    "localsearch-3",
    "tumblerd",
    "ffmpegthumbnailer",
];

#[derive(Default)]
pub(crate) struct Callers(Mutex<HashMap<u32, Cached>>);

struct Cached {
    start: u64,
    executable: String,
    cgroup: String,
    reader: String,
    caller: Caller,
}

impl Callers {
    /// Revalidate the start tick, exe, unit and KIO role on EVERY request: an exec or a
    /// move into another scope may keep the same PID. A bounded cache saves normalization,
    /// not procfs reads. A process that vanished or cannot be inspected fails closed.
    pub fn identify(&self, pid: u32) -> Option<Caller> {
        let root = format!("/proc/{pid}");
        let stat = std::fs::read_to_string(format!("{root}/stat")).ok()?;
        let start = start_time(&stat)?;
        let executable = std::fs::read_link(format!("{root}/exe")).ok()?.to_str()?.to_owned();
        let cgroup = std::fs::read_to_string(format!("{root}/cgroup")).ok()?;
        let mut cmdline = Vec::new();
        File::open(format!("{root}/cmdline")).ok()?.take(65537).read_to_end(&mut cmdline).ok()?;
        if cmdline.len() > 65536 {
            return None;
        }
        let reader = reader_role(&executable, &cmdline);
        if start_time(&std::fs::read_to_string(format!("{root}/stat")).ok()?)? != start {
            return None;
        }
        let mut cache = crate::panic::lock(&self.0);
        if let Some(old) = cache.get(&pid) {
            if old.start == start && old.executable == executable && old.cgroup == cgroup && old.reader == reader {
                return Some(old.caller.clone());
            }
        }
        let caller = caller(&executable, &cgroup, reader.clone());
        if cache.len() >= 1024 {
            cache.clear();
        }
        cache.insert(pid, Cached { start, executable, cgroup, reader, caller: caller.clone() });
        Some(caller)
    }
}

fn start_time(stat: &str) -> Option<u64> {
    // comm may contain spaces and ')'; field 22 is index 19 after the last ')'.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
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
