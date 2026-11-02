//! Store admission: use this schema, migrate a supported predecessor, quarantine an
//! unsupported or damaged store, and refuse a newer build's schema without writing it.
//!
//! A quarantine retains the database and WAL, restores readable pins before listing,
//! and exports unsent namespace changes for manual recovery. Migrations are sequential
//! transactions that also commit the version, after a durable VACUUM INTO snapshot.
// test: a_store_of_todays_version_is_used
// test: an_unsupported_older_store_is_quarantined_and_never_deleted
// test: a_store_of_the_version_before_is_migrated_in_place_and_keeps_what_it_held
// test: a_file_that_is_no_database_is_preserved_in_quarantine
// test: a_store_that_cannot_be_read_for_another_reason_is_left_alone
// test: where_there_is_no_store_one_is_made

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use super::{at_version, thumbnail_type_index, SCHEMA_VERSION};
use crate::item_store::IS_WRITTEN_COLUMN;
use crate::{meta, TreeError};

/// The oldest retained migration predecessor ([`migrate`]).
const MIGRATED_VERSION: &str = "30";

/// What the file at the store's path says of itself, before anything is changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredVersion {
    /// No file, or a database with no table: a store is made.
    NoStore,
    /// A store that says its version.
    Version(String),
    /// Tables and no version: a build that died while it made them.
    NoVersion,
    /// SQLite says the file is no database, or is corrupt.
    NotADatabase,
}

/// What a rebuild loses, for the journal and for what the folder's start tells the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildLoss {
    pub pins: u64,
    pub activity_events: u64,
    pub conflicts: u64,
    /// Unsent changes the check at start cannot find again: removals, and moves of files
    /// with nothing on disk. Counted in the table `outbox` of a store that has one, and in
    /// the rows of `items`.
    pub unsent_changes_not_on_disk: u64,
    /// The upload sessions that were opened and not ended, in the old tables and in what
    /// the steps remember.
    pub open_upload_sessions: u64,
}

/// What `TreeStore::open` does with the store it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OldStoreDecision {
    /// This build's version: nothing to do.
    Use,
    /// No store: the schema is made.
    Make,
    /// A supported predecessor: snapshot, then migrate in sequential transactions.
    Migrate,
    /// Preserve the file and sidecars, restore readable pins, export pending moves and
    /// removals, and build a fresh store from a full listing and backing bytes.
    Quarantine,
    /// A newer build owns this schema; no write to its files is permitted.
    RefuseNewer,
}

/// How the store the folder's start was given came to be: what the start does next depends
/// on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOrigin {
    /// It was there, of this build's version.
    Kept,
    /// It was made empty: there was none (`loss` is `None`), or the one there was rebuilt,
    /// with what that lost — `None` too where nothing could be counted in it, as in a
    /// file that is no database. The check at start binds the marked entries, and the
    /// first listing is a full one.
    MadeEmpty { loss: Option<RebuildLoss> },
}

/// What to do with a store that says `stored` of itself. Pure: the cases are in the
/// module's doc. Called by `TreeStore::open_with_origin` before anything is written.
pub fn decide_old_store(stored: &StoredVersion) -> OldStoreDecision {
    match stored {
        StoredVersion::NoStore => OldStoreDecision::Make,
        StoredVersion::Version(version) if version == SCHEMA_VERSION => OldStoreDecision::Use,
        StoredVersion::Version(version) if matches!(version.as_str(), "30" | "31" | "32") => {
            OldStoreDecision::Migrate
        }
        StoredVersion::Version(version)
            if version
                .parse::<u64>()
                .is_ok_and(|v| v > SCHEMA_VERSION.parse::<u64>().unwrap())
                || (!version.is_empty()
                    && version.bytes().all(|c| c.is_ascii_digit())
                    && version.parse::<u64>().is_err()) =>
        {
            OldStoreDecision::RefuseNewer
        }
        StoredVersion::Version(_) | StoredVersion::NoVersion | StoredVersion::NotADatabase => {
            OldStoreDecision::Quarantine
        }
    }
}

/// Runs the retained 30 to 31, 31 to 32 and 32 to 33 steps in order. Each step and its version
/// commit in one transaction; a crash leaves the last committed version intact.
fn migrate(conn: &Connection) -> Result<(), TreeError> {
    // Every shipped step stays in this chain. Each step and its version commit together.
    if meta::get(conn, meta::SCHEMA_VERSION)?.as_deref() == Some(MIGRATED_VERSION) {
        let step = format!("DROP INDEX items_image_or_video; {} ALTER TABLE thumbnail_marks ADD COLUMN {IS_WRITTEN_COLUMN};", thumbnail_type_index());
        at_version(conn, "31", |tx| Ok(tx.execute_batch(&step)?))?;
    }
    if meta::get(conn, meta::SCHEMA_VERSION)?.as_deref() == Some("31") {
        at_version(conn, "32", |tx| {
            // Earlier opening windows used the machine clock. They cannot prove
            // ownership against Graph's timestamps; leave any unknown holder alone.
            // `onedrive_folder_ctag` is unused and always null (OneDrive personal gives a
            // folder no cTag); the additive media migration retains it.
            tx.execute_batch("ALTER TABLE items ADD COLUMN onedrive_folder_ctag TEXT;
                              DELETE FROM upload_session_openings;")?;
            #[cfg(feature = "fault-injection")]
            if let Some(marker) = std::env::var_os("KONEDRIVE_TEST_MIGRATION_PAUSE") {
                super::quarantine::write_durable(Path::new(&marker), b"migration-before-commit")?;
                // The VM cuts power here. This feature is absent from shipped builds.
                loop {
                    std::thread::park();
                }
            }
            Ok(())
        })?;
    }
    if meta::get(conn, meta::SCHEMA_VERSION)?.as_deref() == Some("32") {
        at_version(conn, "33", |tx| {
            tx.execute_batch(&crate::item_store::media_facts_schema())?;
            // Keep an earlier outstanding cause. Beginning the full listing consumes it.
            if meta::get(tx, meta::FULL_LISTING_REQUESTED)?.is_none() {
                meta::set(tx, meta::FULL_LISTING_REQUESTED, Some("media facts backfill after schema 33 upgrade"))?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// quick_check failures are findings; disk/permission/lock errors remain errors.
fn checked_version(conn: &Connection) -> Result<StoredVersion, TreeError> {
    let stored = read_stored_version(conn)?;
    if matches!(stored, StoredVersion::NotADatabase) {
        return Ok(stored);
    }
    match conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)) {
        Ok(result) if result == "ok" => Ok(stored),
        Ok(_) => Ok(StoredVersion::NotADatabase),
        Err(error) if is_no_database(&error) => Ok(StoredVersion::NotADatabase),
        Err(error) => Err(error.into()),
    }
}

/// Whether SQLite says of the file that it is no database, or is corrupt.
fn is_no_database(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(failure, _) if matches!(failure.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt))
}

fn has_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

/// What the file at the store's path says of itself. Reads `sqlite_master` and `meta`;
/// changes nothing. An error is one that is no finding: permission denied, the disk — the
/// store is then left alone.
pub(super) fn read_stored_version(conn: &Connection) -> Result<StoredVersion, TreeError> {
    let read = || -> rusqlite::Result<StoredVersion> {
        let tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )?;
        if tables == 0 {
            return Ok(StoredVersion::NoStore);
        }
        if !has_table(conn, "meta")? {
            return Ok(StoredVersion::NoVersion);
        }
        let version: Option<Option<String>> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [meta::SCHEMA_VERSION],
                |row| row.get(0),
            )
            .optional()?;
        Ok(version
            .flatten()
            .map_or(StoredVersion::NoVersion, StoredVersion::Version))
    };
    match read() {
        Ok(stored) => Ok(stored),
        Err(error) if is_no_database(&error) => Ok(StoredVersion::NotADatabase),
        Err(error) => Err(error.into()),
    }
}

/// What a rebuild of the store behind `conn` would lose, counted before the file goes.
/// Called by `TreeStore::open_with_origin` when the decision is `Quarantine`, for
/// [`StoreOrigin::MadeEmpty`]. A table the store does not have holds nothing; a store it
/// cannot count in — no database, or tables of another shape — answers nothing.
pub(super) fn count_rebuild_loss(conn: &Connection) -> Option<RebuildLoss> {
    let count = |table: &str, rows: &str| -> rusqlite::Result<u64> {
        if !has_table(conn, table)? {
            return Ok(0);
        }
        conn.query_row(&format!("SELECT count(*) FROM {table} {rows}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count.max(0) as u64)
    };
    let count_all = || -> rusqlite::Result<RebuildLoss> {
        // A removal has nothing on disk; a move of what has no local object neither.
        let unsent_in_outbox = count(
            "outbox",
            "WHERE kind = 'delete' OR (kind = 'move' AND handle IS NULL)",
        )?;
        let unsent_in_items = count(
            "items",
            "WHERE unsent_since IS NOT NULL AND onedrive_item_id IS NOT NULL
               AND (local_parent_id IS NULL
                    OR (kind = 'file' AND content_presence != 'on-disk' AND synced_parent_id IS NOT NULL
                        AND (local_parent_id IS NOT synced_parent_id OR local_name IS NOT synced_name)))",
        )?;
        Ok(RebuildLoss {
            pins: count("pins", "")?,
            activity_events: count("activity", "")?,
            conflicts: count("conflicts", "")?,
            unsent_changes_not_on_disk: unsent_in_outbox + unsent_in_items,
            open_upload_sessions: count("upload_sessions", "")?
                + count("step_memory", "WHERE session_address IS NOT NULL")?,
        })
    };
    count_all().ok()
}

/// What the file at `path` says of itself, read through a connection opened read-only for
/// the question and closed before the answer: the file is not changed. `NoStore` where
/// there is no file. An error is one that is no finding — permission denied, the disk —
/// as [`read_stored_version`] answers it. Blocking.
///
/// For whoever must tell a store of this build's version from one a start would make or
/// rebuild ([`decide_old_store`] of the answer) without opening it as a store: the count
/// of what is unsent in a folder that keeps no open store
/// (`sync_tasks::stop_order::count_unsent_of_closed_store`).
pub fn find_stored_version(path: &Path) -> Result<StoredVersion, TreeError> {
    if !path.try_exists()? {
        return Ok(StoredVersion::NoStore);
    }
    let inspection = super::quarantine::Inspection::open(path)?;
    read_stored_version(&inspection.conn)
}

/// The addresses of the upload sessions that the store at `path` holds, when it is a
/// store of another version than this build's: the `url` of each row of its `upload_sessions`
/// table and the `session_address` of each row of its `step_memory`, whichever of the
/// two tables it has. Read through a connection opened read-only for the question and
/// closed before the answer: the file is not changed, and no journal is left beside it.
/// Empty for no file, for a store of this build's version (its sessions are gone on with, not
/// cancelled), for a file that is no database and for tables of a shape that cannot be
/// read: what cannot be read cannot be cancelled, and expires in OneDrive.
///
/// Blocking. Called by the folder's start
/// (`sync_tasks::start_order::cancel_upload_sessions_of_old_store`) BEFORE
/// [`TreeStore::open_with_origin`](crate::TreeStore::open_with_origin) quarantines such a
/// store, which only counts them (`docs/design/sync.md` §9.2, stage 0).
pub fn list_old_store_upload_sessions(path: &Path) -> Result<Vec<String>, TreeError> {
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    let inspection = super::quarantine::Inspection::open(path)?;
    let conn = &inspection.conn;
    if decide_old_store(&read_stored_version(conn)?) != OldStoreDecision::Quarantine {
        return Ok(Vec::new());
    }
    let list = |table: &str, column: &str, rows: &str| -> rusqlite::Result<Vec<String>> {
        if !has_table(&conn, table)? {
            return Ok(Vec::new());
        }
        let mut statement = conn.prepare(&format!("SELECT {column} FROM {table} {rows}"))?;
        let addresses = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect();
        addresses
    };
    let list_all = || -> rusqlite::Result<Vec<String>> {
        let mut addresses = list("upload_sessions", "url", "WHERE url IS NOT NULL")?;
        addresses.extend(list(
            "step_memory",
            "session_address",
            "WHERE session_address IS NOT NULL",
        )?);
        Ok(addresses)
    };
    // Tables of a shape that cannot be read hold nothing that can be cancelled.
    Ok(list_all().unwrap_or_default())
}

impl crate::TreeStore {
    /// Opens a store without deleting its predecessor. Non-corruption errors leave it
    /// alone; a newer schema is refused and its files are never opened for writing.
    pub fn open_with_origin(path: &Path) -> Result<(Self, StoreOrigin), TreeError> {
        use super::quarantine::{self, Inspection};
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let lock = quarantine::wait_for_store(path)?;
        let pending = quarantine::finish_renames(path)?;
        let (mut store, origin) = if let Some(old) = pending {
            let mut store = Self::open_file(path)?;
            let loss = quarantine::carry_over(path, &old, &mut store)?;
            quarantine::complete(path)?;
            (store, StoreOrigin::MadeEmpty { loss })
        } else if !path.try_exists()? {
            let store = Self::open_file(path)?;
            quarantine::sync_parent(path)?;
            (store, StoreOrigin::MadeEmpty { loss: None })
        } else {
            let inspection = Inspection::open(path)?;
            // Refuse newer versions even if quick_check would find damage. A downgrade
            // must not reinterpret corruption in a layout it does not understand.
            let version = read_stored_version(&inspection.conn)?;
            if decide_old_store(&version) == OldStoreDecision::RefuseNewer {
                if let StoredVersion::Version(version) = version {
                    return Err(TreeError::NewerSchema(version));
                }
            }
            let stored = checked_version(&inspection.conn)?;
            match decide_old_store(&stored) {
                OldStoreDecision::Use => {
                    drop(inspection);
                    (Self::open_file(path)?, StoreOrigin::Kept)
                }
                OldStoreDecision::Make => {
                    drop(inspection);
                    (
                        Self::open_file(path)?,
                        StoreOrigin::MadeEmpty { loss: None },
                    )
                }
                OldStoreDecision::Migrate => {
                    make_snapshot(path, &inspection.conn, &stored)?;
                    drop(inspection);
                    let conn = Connection::open(path)?;
                    conn.busy_timeout(std::time::Duration::from_secs(5))?;
                    conn.pragma_update(None, "synchronous", "FULL")?;
                    migrate(&conn)?;
                    drop(conn);
                    (Self::open_file(path)?, StoreOrigin::Kept)
                }
                OldStoreDecision::Quarantine => {
                    drop(inspection);
                    let old = quarantine::begin(path, &stored)?;
                    let mut store = Self::open_file(path)?;
                    let loss = quarantine::carry_over(path, &old, &mut store)?;
                    quarantine::complete(path)?;
                    tracing::warn!("{} quarantined at {}; readable pins restored; pending namespace changes exported", path.display(), old.display());
                    (store, StoreOrigin::MadeEmpty { loss })
                }
                OldStoreDecision::RefuseNewer => unreachable!(),
            }
        };
        store._file_lock = Some(lock);
        Ok((store, origin))
    }

    /// Called after a successful folder start. Snapshots survive that start plus seven
    /// days. The first successful start stamps them; later starts may prune them.
    pub fn successful_start(&self) -> Result<(), TreeError> {
        use super::quarantine::{beside, sync_parent, write_durable};
        use std::time::{SystemTime, UNIX_EPOCH};
        let Some(path) = &self.path else {
            return Ok(());
        };
        let prefix = format!(
            "{}.pre-",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        for entry in std::fs::read_dir(super::quarantine::parent(path))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(version) = name.strip_prefix(&prefix) else {
                continue;
            };
            let numeric =
                |value: &str| !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit());
            if !numeric(version)
                && !version
                    .split_once('-')
                    .is_some_and(|(v, stamp)| numeric(v) && super::quarantine::is_timestamp(stamp))
            {
                continue;
            }
            let stamp = beside(&entry.path(), ".successful-start");
            match std::fs::read_to_string(&stamp) {
                Ok(stamp_text)
                    if stamp_text
                        .trim()
                        .parse::<u64>()
                        .is_ok_and(|then| now.saturating_sub(then) >= 7 * 86400) =>
                {
                    std::fs::remove_file(entry.path())?;
                    std::fs::remove_file(stamp)?;
                    sync_parent(path)?;
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    write_durable(&stamp, now.to_string().as_bytes())?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn make_snapshot(path: &Path, conn: &Connection, stored: &StoredVersion) -> Result<(), TreeError> {
    use super::quarantine::{beside, sync_parent};
    let StoredVersion::Version(version) = stored else {
        unreachable!()
    };
    let first = beside(path, &format!(".pre-{version}"));
    // A retry may follow real work by the older build. Preserve the previous snapshot
    // and snapshot the current WAL too, instead of silently using stale recovery bytes.
    let snapshot = if first.try_exists()? {
        beside(
            path,
            &format!(".pre-{version}-{}", super::quarantine::timestamp()),
        )
    } else {
        first
    };
    // VACUUM reads the committed WAL too; a file copy of the main database would not.
    let temporary = tempfile::Builder::new()
        .prefix(".store-snapshot-")
        .tempdir_in(super::quarantine::parent(path))?;
    let output = temporary.path().join("snapshot");
    conn.execute("VACUUM INTO ?1", [output.to_string_lossy().as_ref()])?;
    // VACUUM INTO makes the file with the umask's mode; the store's own files are 0600.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::File::open(&output)?.sync_all()?;
    std::fs::rename(output, snapshot)?;
    sync_parent(path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
