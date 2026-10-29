//! Preservation of a store before it is replaced. A durable intent makes the three
//! renames resumable; a restart finishes them before opening a fresh database.
//!
//! Inspections use a private copy, including the WAL. Even SQLite's shared-memory
//! bookkeeping must not write to a newer build's store.
// test: a_newer_store_is_refused_without_changing_any_member
// test: a_quarantine_interrupted_between_members_is_finished_before_open
// test: quarantines_keep_the_newest_three_and_keep_their_sidecars
// test: quarantined_pins_and_namespace_intentions_are_kept_without_replay

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::old_store::{count_rebuild_loss, read_stored_version, RebuildLoss, StoredVersion};
use crate::{TreeError, TreeStore};

pub(super) fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

pub(super) fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

pub(super) fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(parent(path))?.sync_all()
}

pub(super) fn lock_store(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(beside(path, ".owner-lock"))?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "the store is in use; stop its daemon before resetting it",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(file)
}

/// A dropped shared store finishes queued jobs before releasing ownership. Startup
/// waits briefly for that close, as SQLite's busy timeout does; an explicit reset does
/// not wait and cannot move the files of a live owner.
pub(super) fn wait_for_store(path: &Path) -> io::Result<File> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match lock_store(path) {
            Ok(file) => return Ok(file),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(error) => return Err(error),
        }
    }
}

/// The connection must be dropped before the directory that holds its copy.
pub(super) struct Inspection {
    pub(super) conn: Connection,
    _directory: tempfile::TempDir,
}

impl Inspection {
    pub(super) fn open(path: &Path) -> Result<Self, TreeError> {
        let directory = tempfile::Builder::new()
            .prefix(".store-inspect-")
            .tempdir_in(parent(path))?;
        let copy = directory.path().join("items.db");
        for suffix in ["", "-wal", "-journal"] {
            match std::fs::copy(beside(path, suffix), beside(&copy, suffix)) {
                Ok(_) => (),
                Err(error) if suffix != "" && error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let conn = Connection::open(copy)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            conn,
            _directory: directory,
        })
    }
}

/// Write privately and durably before making the name visible.
pub(super) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(parent(path))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    sync_parent(path)
}

/// UTC calendar timestamp without a timezone or locale dependency.
pub(super) fn timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let days = (elapsed.as_secs() / 86400) as i64;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    let seconds = elapsed.as_secs() % 86400;
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}.{:09}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        elapsed.subsec_nanos()
    )
}

pub(super) fn is_timestamp(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    bytes.len() == 26
        && bytes[8] == b'T'
        && bytes[15] == b'.'
        && bytes[25] == b'Z'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 8 | 15 | 25) || c.is_ascii_digit())
}

pub(super) fn begin(path: &Path, stored: &StoredVersion) -> Result<PathBuf, TreeError> {
    let version = match stored {
        StoredVersion::Version(version)
            if !version.is_empty() && version.bytes().all(|c| c.is_ascii_digit()) =>
        {
            version.as_str()
        }
        _ => "corrupt",
    };
    let destination = beside(path, &format!(".{version}-{}", timestamp()));
    for suffix in [
        "",
        "-wal",
        "-shm",
        "-journal",
        ".members-moved",
        ".unsent-changes.txt",
    ] {
        if beside(&destination, suffix).try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "quarantine destination exists; retry with a new timestamp",
            )
            .into());
        }
    }
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::other("store has no filename"))?
        .to_str()
        .ok_or_else(|| io::Error::other("store filename is not UTF-8"))?;
    write_durable(&beside(path, ".quarantine-intent"), name.as_bytes())?;
    finish_renames(path)?.ok_or_else(|| io::Error::other("quarantine intent disappeared").into())
}

pub(super) fn finish_renames(path: &Path) -> Result<Option<PathBuf>, TreeError> {
    let intent = beside(path, ".quarantine-intent");
    let name = match std::fs::read_to_string(&intent) {
        Ok(name) => name,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if Path::new(&name).components().count() != 1
        || !name.starts_with(&format!(
            "{}.",
            path.file_name().unwrap_or_default().to_string_lossy()
        ))
    {
        return Err(io::Error::other("invalid store quarantine intent").into());
    }
    let destination = path.with_file_name(name);
    if beside(&destination, ".members-moved").try_exists()? {
        return Ok(Some(destination));
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let source = beside(path, suffix);
        let target = beside(&destination, suffix);
        // A member already moved stays untouched, including after fresh-store creation.
        if target.try_exists()? {
            continue;
        }
        match std::fs::rename(&source, &target) {
            Ok(()) => sync_parent(path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    // Renaming and syncing a directory preserves names, not dirty WAL contents left
    // by a killed process. Preserve those bytes too before admitting a replacement.
    for suffix in ["", "-wal", "-shm", "-journal"] {
        match File::open(beside(&destination, suffix)) {
            Ok(file) => file.sync_all()?,
            Err(error) if suffix != "" && error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    write_durable(&beside(&destination, ".members-moved"), b"complete")?;
    Ok(Some(destination))
}

/// Keep only the newest three completed quarantines. Recovery exports are never pruned.
fn prune(path: &Path) -> Result<(), TreeError> {
    let prefix = format!(
        "{}.",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut quarantines = Vec::new();
    for entry in std::fs::read_dir(parent(path))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some((version, stamp)) = rest.split_once('-') else {
            continue;
        };
        if (version == "corrupt"
            || (!version.is_empty() && version.bytes().all(|c| c.is_ascii_digit())))
            && is_timestamp(stamp)
            && entry.file_type()?.is_file()
            && beside(&entry.path(), ".members-moved").try_exists()?
        {
            quarantines.push((stamp.to_owned(), entry.path()));
        }
    }
    quarantines.sort();
    let excess = quarantines.len().saturating_sub(3);
    for (_, old) in quarantines.into_iter().take(excess) {
        for suffix in ["-wal", "-shm", "-journal", ".members-moved", ""] {
            match std::fs::remove_file(beside(&old, suffix)) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    sync_parent(path)?;
    Ok(())
}

pub(super) fn rows(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let columns = statement.column_count();
    let result = statement
        .query_map([], |row| {
            let mut values = Vec::new();
            for column in 0..columns {
                values.push(match row.get_ref(column)? {
                    // A name that is not UTF-8 is escaped byte for byte, so it can be told
                    // from every other name.
                    rusqlite::types::ValueRef::Text(text) => match std::str::from_utf8(text) {
                        Ok(text) => format!("{text:?}"),
                        Err(_) => format!("b\"{}\"", text.escape_ascii()),
                    },
                    value => format!("{value:?}"),
                });
            }
            Ok(values.join("\t"))
        })?
        .collect();
    result
}

/// Import pins before the first listing, and preserve namespace intentions for manual
/// recovery. Neither a removal nor a move is replayed against OneDrive.
pub(super) fn carry_over(
    path: &Path,
    quarantine: &Path,
    store: &mut TreeStore,
) -> Result<Option<RebuildLoss>, TreeError> {
    let inspection = Inspection::open(quarantine)?;
    let conn = &inspection.conn;
    let mut loss = count_rebuild_loss(conn);
    let pins = (|| -> rusqlite::Result<Vec<(String, bool)>> {
        let mut statement = conn.prepare("SELECT id, is_kept FROM pins")?;
        let result = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        Ok(result)
    })()
    .unwrap_or_default();
    let recovered_pin_count = pins.len() as u64;
    let tx = store.conn.unchecked_transaction()?;
    for (id, kept) in pins {
        tx.execute(
            "INSERT OR REPLACE INTO pins (id, is_kept) VALUES (?1, ?2)",
            rusqlite::params![id, kept],
        )?;
    }
    tx.commit()?;
    let mut changes = rows(conn, "WITH RECURSIVE
        old_paths(id, path) AS (
            SELECT local_item_id, '' FROM items WHERE local_item_id = 1
            UNION ALL
            SELECT item.local_item_id, parent.path || '/' || COALESCE(item.synced_name, item.onedrive_name)
            FROM items AS item JOIN old_paths AS parent
              ON item.synced_parent_id = parent.id
                OR (item.synced_parent_id IS NULL AND item.onedrive_parent_id = parent.id)
        ),
        intended_paths(id, path) AS (
            SELECT local_item_id, '' FROM items WHERE local_item_id = 1
            UNION ALL
            SELECT item.local_item_id, parent.path || '/' || item.local_name
            FROM items AS item JOIN intended_paths AS parent ON item.local_parent_id = parent.id
        )
        SELECT onedrive_item_id, kind, old_paths.path, intended_paths.path FROM items
        LEFT JOIN old_paths ON old_paths.id = items.local_item_id
        LEFT JOIN intended_paths ON intended_paths.id = items.local_item_id
        WHERE onedrive_item_id IS NOT NULL AND COALESCE(synced_parent_id, onedrive_parent_id) IS NOT NULL
          AND (synced_parent_id IS NOT NULL OR unsent_since IS NOT NULL)
          AND (local_parent_id IS NOT COALESCE(synced_parent_id, onedrive_parent_id) OR local_name IS NOT COALESCE(synced_name, onedrive_name))").unwrap_or_default();
    changes.extend(
        rows(
            conn,
            "SELECT kind, rel, state FROM outbox WHERE kind IN ('delete', 'move')",
        )
        .unwrap_or_default(),
    );
    if !changes.is_empty() {
        let export = beside(quarantine, ".unsent-changes.txt");
        if !export.try_exists()? {
            write_durable(&export, format!("Unsent removals and moves; NOT replayed. Columns: item id, kind, old path, intended path (Null for removal); legacy rows: kind, path, state.\n{}\n", changes.join("\n")).as_bytes())?;
        }
        if let Some(loss) = &mut loss {
            loss.unsent_changes_not_on_disk = changes.len() as u64;
        }
    }
    // Older exports remain actionable through later rebuilds and quarantine pruning.
    // Escaped fields occupy one line per change, including filenames with newlines.
    let prefix = format!(
        "{}.",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut exported = 0usize;
    let mut exports = Vec::new();
    for entry in std::fs::read_dir(parent(path))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".unsent-changes.txt") {
            exported += std::fs::read_to_string(entry.path())?
                .lines()
                .skip(1)
                .count();
            exports.push(entry.path());
        }
    }
    if exported > 0 {
        let location = if exports.len() == 1 {
            exports[0].clone()
        } else {
            parent(path).to_path_buf()
        };
        store.set_meta(
            "quarantined_changes",
            Some(&format!(
                "{exported} unsent removal(s) or move(s) need manual recovery; see {}",
                location.display()
            )),
        )?;
    }
    // A missing or corrupt pins table is best effort; the original bytes stay quarantined.
    if let Some(loss) = &mut loss {
        loss.pins = loss.pins.saturating_sub(recovered_pin_count);
    }
    drop(inspection);
    Ok(loss)
}

pub(super) fn complete(path: &Path) -> Result<(), TreeError> {
    // The replacement must be durable before the intent can go away.
    let conn = Connection::open(path)?;
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
    if busy != 0 {
        return Err(io::Error::other("replacement store checkpoint is busy").into());
    }
    drop(conn);
    File::open(path)?.sync_all()?;
    sync_parent(path)?;
    std::fs::remove_file(beside(path, ".quarantine-intent"))?;
    sync_parent(path)?;
    prune(path)
}

impl TreeStore {
    /// Pending namespace changes exported by quarantine, for LastError and Issues.
    pub fn recovery_notice(&self) -> Result<Option<String>, TreeError> {
        self.meta("quarantined_changes")
    }

    /// Offline explicit reset. Quarantines even a newer schema. The daemon must be stopped;
    /// exclusive ownership is checked before touching any store member. Pins are restored
    /// on the next open, from the durable intent, before the first listing.
    pub fn quarantine_for_reset(path: &Path) -> Result<PathBuf, TreeError> {
        let _lock = lock_store(path)?;
        if let Some(quarantine) = finish_renames(path)? {
            return Ok(quarantine);
        }
        let inspection = Inspection::open(path)?;
        let stored = read_stored_version(&inspection.conn)?;
        drop(inspection);
        begin(path, &stored)
    }
}
