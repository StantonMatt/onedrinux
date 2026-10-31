//! SQLite operations for the disposable header cache; only the account's worker calls these.
//!
//! Schema 1 stores an item id, serialized content version, offset, bytes, opaque source
//! facts and last-use time. A covering row answers a request; partial coverage misses.
//! DELETE journals and full auto-vacuum keep the actual database under its byte budget.

// test: header_cache_keeps_ranges_across_restart_and_preserves_facts
// test: header_cache_bounds_ranges_items_and_evicts_lru_rows
// test: header_cache_expires_unused_rows_and_removes_other_versions
// test: header_cache_disabled_corrupt_and_new_schema_files_are_disposable
// test: header_cache_failed_deletion_blocks_reuse_and_retries_after_restart
// test: header_cache_failed_unlink_is_remembered_without_directory_write_permission

use rusqlite::{params, Connection, OptionalExtension};
use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Record, LIFETIME, MAX_ITEM, MAX_RANGE};

const SCHEMA: i64 = 1;

pub(super) struct Database {
    pub(super) conn: Connection,
    limit: u64,
}

pub(super) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn directory_exists(path: &Path) -> anyhow::Result<bool> {
    if let Some(parent) = path.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(meta) => anyhow::ensure!(meta.is_dir(), "header cache directory is not a directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn clear_marker(path: &Path) -> PathBuf {
    let mut marker = path.as_os_str().to_owned();
    marker.push("-clear");
    marker.into()
}

pub(super) fn needs_clear(path: &Path) -> anyhow::Result<bool> {
    if !directory_exists(path)? {
        return Ok(false);
    }
    match std::fs::symlink_metadata(clear_marker(path)) {
        Ok(meta) if meta.is_file() && meta.len() == 1 => {
            let mut marker = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(clear_marker(path))?;
            let mut state = [0];
            marker.read_exact(&mut state)?;
            Ok(state != *b"0")
        }
        Ok(_) => Ok(true),
        // An older database without an invalidation marker is disposable too.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(path) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn prepare_clear_marker(path: &Path) -> anyhow::Result<()> {
    // Reserve a writable marker before using any database. Even if the directory
    // later refuses unlink/create, invalidation can truncate this existing 0600 file.
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(clear_marker(path))
    {
        Ok(mut marker) => {
            marker.write_all(b"0")?;
            marker.sync_all()?;
            sync_directory(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::ensure!(!needs_clear(path)?, "header cache deletion is pending");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn mark_clear(path: &Path) -> anyhow::Result<()> {
    if !directory_exists(path)? {
        return Ok(());
    }
    let marker = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(clear_marker(path))?;
    marker.sync_all()?;
    sync_directory(path)
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn finish_clear(path: &Path) -> anyhow::Result<()> {
    if !directory_exists(path)? {
        return Ok(());
    }
    // Make deletion durable before removing the instruction to retry it at startup.
    sync_directory(path)?;
    match std::fs::remove_file(clear_marker(path)) {
        Ok(()) => sync_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn remove(path: &Path) -> anyhow::Result<()> {
    if !directory_exists(path)? {
        return Ok(());
    }
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        match std::fs::remove_file(file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn is_busy(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _))
        if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

impl Database {
    pub(super) fn open(path: &Path, limit: u64) -> anyhow::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("header cache has no directory"))?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        anyhow::ensure!(
            std::fs::symlink_metadata(parent)?.is_dir(),
            "header cache directory is not a directory"
        );
        std::fs::set_permissions(parent, Permissions::from_mode(0o700))?;
        prepare_clear_marker(path)?;
        // Create privately before SQLite opens it, so neither the database nor its
        // journal is briefly world-readable under the daemon's ordinary umask.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "header cache is not a regular file"
        );
        file.set_permissions(Permissions::from_mode(0o600))?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(1))?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        anyhow::ensure!(check == "ok", "header cache integrity check failed");
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(
            version == 0 || version == SCHEMA,
            "unsupported header cache schema {version}"
        );
        conn.query_row("PRAGMA journal_mode = DELETE", [], |row| {
            row.get::<_, String>(0)
        })?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "auto_vacuum", "FULL")?;
        if version == 0 {
            conn.execute_batch(
                "BEGIN;
                CREATE TABLE headers (
                    item TEXT NOT NULL, version TEXT NOT NULL, offset INTEGER NOT NULL,
                    bytes BLOB NOT NULL, facts TEXT NOT NULL, last_used INTEGER NOT NULL,
                    PRIMARY KEY (item, version, offset),
                    CHECK (offset >= 0 AND length(bytes) BETWEEN 1 AND 65536)
                );
                CREATE INDEX headers_lru ON headers(last_used);
                PRAGMA user_version = 1;
                COMMIT;",
            )?;
        }
        let db = Self { conn, limit };
        db.expire(now())?;
        db.evict()?;
        Ok(db)
    }

    fn expire(&self, at: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM headers WHERE last_used <= ?1",
            [at.saturating_sub(LIFETIME)],
        )?;
        Ok(())
    }

    pub(super) fn prune(&self, at: i64) -> anyhow::Result<()> {
        self.expire(at)?;
        self.evict()
    }

    fn current(&self, item: &str, version: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM headers WHERE item = ?1 AND version <> ?2",
            params![item, version],
        )?;
        Ok(())
    }

    pub(super) fn find(
        &mut self,
        item: &str,
        version: &str,
        from: u64,
        end: u64,
        at: i64,
    ) -> anyhow::Result<Option<Record>> {
        self.expire(at)?;
        self.current(item, version)?;
        let found = self
            .conn
            .query_row(
                "SELECT rowid, offset, bytes, facts FROM headers
             WHERE item = ?1 AND version = ?2 AND offset <= ?3 AND offset + length(bytes) >= ?4
             ORDER BY last_used DESC LIMIT 1",
                params![item, version, from as i64, end as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        Record {
                            from: row.get::<_, i64>(1)? as u64,
                            bytes: row.get(2)?,
                            facts: row.get(3)?,
                        },
                    ))
                },
            )
            .optional()?;
        if let Some((rowid, record)) = found {
            self.conn.execute(
                "UPDATE headers SET last_used = ?1 WHERE rowid = ?2",
                params![at, rowid],
            )?;
            return Ok(Some(record));
        }
        Ok(None)
    }

    pub(super) fn keep(
        &mut self,
        item: &str,
        version: &str,
        record: &Record,
        at: i64,
    ) -> anyhow::Result<()> {
        let bytes = record.bytes.len() as u64;
        if bytes == 0 || bytes > MAX_RANGE {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM headers WHERE last_used <= ?1 OR (item = ?2 AND version <> ?3)",
            params![at.saturating_sub(LIFETIME), item, version],
        )?;
        let (total, old): (i64, i64) = tx.query_row(
            "SELECT coalesce(sum(length(bytes)), 0), coalesce(max(CASE WHEN offset = ?3 THEN length(bytes) END), 0)
             FROM headers WHERE item = ?1 AND version = ?2", params![item, version, record.from as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if old >= bytes as i64 {
            tx.execute("UPDATE headers SET last_used = ?1 WHERE item = ?2 AND version = ?3 AND offset = ?4", params![at, item, version, record.from as i64])?;
        } else if total - old + bytes as i64 <= MAX_ITEM as i64 {
            tx.execute("INSERT INTO headers (item, version, offset, bytes, facts, last_used) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(item, version, offset) DO UPDATE SET bytes = excluded.bytes, facts = excluded.facts, last_used = excluded.last_used",
                params![item, version, record.from as i64, record.bytes, record.facts, at])?;
        }
        tx.commit()?;
        self.evict()
    }

    pub(super) fn forget(&mut self, item: &str) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM headers WHERE item = ?1", [item])?;
        Ok(())
    }

    fn bytes_on_disk(&self) -> anyhow::Result<u64> {
        let pages: i64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok((pages * page_size) as u64)
    }

    pub(super) fn stats(&self) -> anyhow::Result<(u64, u64)> {
        let rows: i64 = self
            .conn
            .query_row("SELECT count(*) FROM headers", [], |row| row.get(0))?;
        Ok((self.bytes_on_disk()?, rows as u64))
    }

    fn evict(&self) -> anyhow::Result<()> {
        loop {
            let bytes = self.bytes_on_disk()?;
            if bytes <= self.limit {
                return Ok(());
            }
            // Evict oldest rows in batches; auto-vacuum reclaims their pages at commit.
            // A small overrun takes one row, larger ones up to 64 rows at a time.
            let batch = (bytes - self.limit).div_ceil(MAX_RANGE).clamp(1, 64);
            let removed = self.conn.execute("DELETE FROM headers WHERE rowid IN (SELECT rowid FROM headers ORDER BY last_used, rowid LIMIT ?1)", [batch as i64])?;
            if removed == 0 {
                return Ok(());
            }
        }
    }
}
