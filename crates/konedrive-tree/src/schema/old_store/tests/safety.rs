use super::*;
use crate::schema::quarantine::{self, beside};

fn quarantines(path: &Path) -> Vec<PathBuf> {
    let prefix = format!("{}.", path.file_name().unwrap().to_string_lossy());
    let mut found: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with(&prefix) && name.ends_with('Z')
        })
        .collect();
    found.sort();
    found
}

#[test]
fn a_newer_store_is_refused_without_changing_any_member() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    conn.execute("UPDATE meta SET value='99' WHERE key='schema_version'", [])
        .unwrap();
    let before: Vec<_> = ["", "-wal", "-shm"]
        .map(|suffix| std::fs::read(beside(&path, suffix)).unwrap())
        .into();
    assert!(
        matches!(TreeStore::open(&path), Err(TreeError::NewerSchema(version)) if version == "99")
    );
    for (suffix, bytes) in ["", "-wal", "-shm"].into_iter().zip(before) {
        assert_eq!(
            std::fs::read(beside(&path, suffix)).unwrap(),
            bytes,
            "{suffix} untouched, including the WAL-only version"
        );
    }
    assert!(quarantines(&path).is_empty());
    assert_eq!(
        decide_old_store(&StoredVersion::Version("99999999999999999999999999".into())),
        OldStoreDecision::RefuseNewer
    );
}

#[test]
fn quarantines_keep_the_newest_three_and_keep_their_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    for _ in 0..5 {
        std::fs::write(&path, b"unreadable original").unwrap();
        std::fs::write(beside(&path, "-wal"), b"original wal").unwrap();
        std::fs::write(beside(&path, "-shm"), b"original shm").unwrap();
        drop(TreeStore::open(&path).unwrap());
    }
    let kept = quarantines(&path);
    assert_eq!(kept.len(), 3);
    for old in kept {
        assert_eq!(std::fs::read(&old).unwrap(), b"unreadable original");
        assert_eq!(
            std::fs::read(beside(&old, "-wal")).unwrap(),
            b"original wal"
        );
        assert_eq!(
            std::fs::read(beside(&old, "-shm")).unwrap(),
            b"original shm"
        );
    }
}

#[test]
fn a_quarantine_interrupted_between_members_is_finished_before_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    let old = beside(&path, ".19-20261028T000000.000000000Z");
    drop(TreeStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    conn.execute(
        "INSERT INTO pins(id,is_kept) VALUES('KEPT',1),('EXCEPTION',0)",
        [],
    )
    .unwrap();
    conn.execute("UPDATE meta SET value='19' WHERE key='schema_version'", [])
        .unwrap();
    // Simulate death just after the main file's rename: the original WAL is still there.
    quarantine::write_durable(
        &beside(&path, ".quarantine-intent"),
        old.file_name().unwrap().as_encoded_bytes(),
    )
    .unwrap();
    std::fs::rename(&path, &old).unwrap();
    // Keep SQLite from checkpointing on close: its files now have different names.
    let (store, _) = TreeStore::open_with_origin(&path).unwrap();
    assert_eq!(store.pins().unwrap(), ["KEPT"]);
    assert_eq!(
        store
            .conn
            .query_row("SELECT is_kept FROM pins WHERE id='EXCEPTION'", [], |row| {
                row.get::<_, bool>(0)
            })
            .unwrap(),
        false
    );
    drop(store);
    // Closing the simulated dead connection now would unlink the replacement's WAL by
    // pathname; do it only after the replacement has closed and checkpointed.
    drop(conn);
    assert!(old.exists());
    assert!(!beside(&path, ".quarantine-intent").exists());
}

#[test]
fn an_explicit_reset_preserves_newer_stores_and_refuses_an_open_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    let store = TreeStore::open(&path).unwrap();
    assert!(TreeStore::quarantine_for_reset(&path).is_err());
    store.set_pin("F", true).unwrap();
    drop(store);
    set_stored_version(&path, "99");
    let old = TreeStore::quarantine_for_reset(&path).unwrap();
    assert!(old.exists());
    assert!(!path.exists());
    assert_eq!(TreeStore::open(&path).unwrap().pins().unwrap(), ["F"]);
}

#[test]
fn migration_31_to_32_keeps_a_wal_inclusive_snapshot_and_a_nullable_folder_ctag() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; DROP TRIGGER media_facts_item_gone; DROP TRIGGER media_facts_binding_gone; DROP TABLE media_facts; ALTER TABLE items DROP COLUMN onedrive_folder_ctag;").unwrap();
    conn.execute("UPDATE meta SET value='31' WHERE key='schema_version'", [])
        .unwrap();
    conn.execute("INSERT INTO pins VALUES('WAL-ONLY',0)", [])
        .unwrap();
    let store = TreeStore::open(&path).unwrap();
    assert_eq!(store.meta("schema_version").unwrap().as_deref(), Some(SCHEMA_VERSION));
    let snapshot = beside(&path, ".pre-31");
    let snap =
        Connection::open_with_flags(&snapshot, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        read_stored_version(&snap).unwrap(),
        StoredVersion::Version("31".into())
    );
    assert_eq!(
        snap.query_row("SELECT is_kept FROM pins WHERE id='WAL-ONLY'", [], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap(),
        false
    );
    let nullable: (String, bool) = store.conn.query_row("SELECT type, \"notnull\" FROM pragma_table_info('items') WHERE name='onedrive_folder_ctag'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(nullable, ("TEXT".into(), false));
    let columns = |conn: &Connection| -> Vec<(String, String, String, bool, Option<String>, i64)> {
        let mut query = conn.prepare(
            "SELECT m.name, p.name, p.type, p.\"notnull\", p.dflt_value, p.pk
             FROM sqlite_master m JOIN pragma_table_info(m.name) p
             WHERE m.type='table' ORDER BY m.name, p.name",
        ).unwrap();
        query.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
            .unwrap().collect::<Result<_, _>>().unwrap()
    };
    let made = TreeStore::in_memory().unwrap();
    assert_eq!(columns(&store.conn), columns(&made.conn), "fresh and migrated stores have the same columns, regardless of ALTER column order");
    assert!(!beside(&snapshot, ".successful-start").exists());
    store.successful_start().unwrap();
    assert!(snapshot.exists());
    store.successful_start().unwrap();
    assert!(
        snapshot.exists(),
        "not removed at the next successful start"
    );
    std::fs::write(beside(&snapshot, ".successful-start"), b"1").unwrap();
    store.successful_start().unwrap();
    assert!(
        !snapshot.exists(),
        "seven days after a successful start it may go"
    );
    drop(store);
    drop(conn);
}

#[test]
fn a_migration_snapshot_is_readable_by_its_owner_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TRIGGER media_facts_item_gone; DROP TRIGGER media_facts_binding_gone; DROP TABLE media_facts; ALTER TABLE items DROP COLUMN onedrive_folder_ctag; UPDATE meta SET value='31' WHERE key='schema_version';").unwrap();
    drop(conn);
    // A desktop session's umask lets others read what VACUUM INTO makes.
    drop(TreeStore::open(&path).unwrap());
    let mode = std::fs::metadata(beside(&path, ".pre-31")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn migration_31_to_32_discards_machine_clock_upload_openings() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TRIGGER media_facts_item_gone; DROP TRIGGER media_facts_binding_gone; DROP TABLE media_facts; ALTER TABLE items DROP COLUMN onedrive_folder_ctag;
        UPDATE meta SET value='31' WHERE key='schema_version';
        INSERT INTO upload_session_openings VALUES(1, 'holder', 'holder', NULL, 100, 200);").unwrap();
    drop(conn);
    let store = TreeStore::open(&path).unwrap();
    assert_eq!(store.conn.query_row("SELECT COUNT(*) FROM upload_session_openings", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    let snapshot = Connection::open(beside(&path, ".pre-31")).unwrap();
    assert_eq!(snapshot.query_row("SELECT COUNT(*) FROM upload_session_openings", [], |row| row.get::<_, i64>(0)).unwrap(), 1, "the legacy evidence is preserved only in the snapshot");
}

#[test]
fn a_failed_migration_keeps_its_version_and_its_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    // Malformed predecessor: ALTER would add a column that already exists. Its version
    // must not be advanced on a failed transaction, nor may it be quarantined as old.
    set_stored_version(&path, "31");
    assert!(TreeStore::open(&path).is_err());
    assert_eq!(
        find_stored_version(&path).unwrap(),
        StoredVersion::Version("31".into())
    );
    assert!(beside(&path, ".pre-31").exists());
    assert!(quarantines(&path).is_empty());
}

#[test]
fn quick_check_damage_quarantines_even_a_store_of_todays_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE items SET kind='damaged' WHERE local_item_id=1;").unwrap();
    drop(conn);
    let (_, origin) = TreeStore::open_with_origin(&path).unwrap();
    assert!(matches!(origin, StoreOrigin::MadeEmpty { .. }));
    assert_eq!(quarantines(&path).len(), 1);
}

#[test]
fn quarantined_pins_and_namespace_intentions_are_kept_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    let mut store = store_in_file_with_listed_root(&path);
    use crate::item_store::tests::shown_items::show_listed_directory;
    show_listed_directory(&mut store, "OLD-DIR", "R", "before");
    show_listed_directory(&mut store, "OLD-DEEP", "OLD-DIR", "deep");
    show_listed_directory(&mut store, "NEW-DIR", "R", "after");
    let destination = show_listed_directory(&mut store, "NEW-DEEP", "NEW-DIR", "deep");
    let removed = show_listed_file(&mut store, "REMOVED", "OLD-DEEP", "gone", "h1");
    let moved = show_listed_file(&mut store, "MOVED", "OLD-DEEP", "old", "h2");
    // Cloud items with no local baseline are ordinary listing/skipping state,
    // not user removals. Include both beside real namespace intentions.
    let never_shown = show_listed_file(&mut store, "NEVER-SHOWN", "R", "never", "h3");
    let skipped = show_listed_file(&mut store, "SKIPPED", "R", "skipped", "h4");
    for id in [never_shown, skipped] {
        store.conn.execute("UPDATE items SET local_parent_id=NULL, local_name=NULL, local_folded_name=NULL,
            local_size=NULL, local_mtime=NULL, local_mtime_nsec=NULL, local_hash=NULL, local_ctag=NULL, local_change_count=NULL,
            synced_parent_id=NULL, synced_name=NULL, synced_size=NULL, synced_mtime=NULL, synced_mtime_nsec=NULL, synced_hash=NULL, synced_ctag=NULL, synced_change_count=NULL
            WHERE local_item_id=?1", [id.as_i64()]).unwrap();
    }
    store.conn.execute("UPDATE items SET placement=?1 WHERE local_item_id=?2", rusqlite::params![crate::Placement::Skipped(crate::SkipReason::Unsupported).encode(), skipped.as_i64()]).unwrap();
    store.set_pin("REMOVED", true).unwrap();
    store
        .conn
        .execute("INSERT INTO pins VALUES('EXCEPTION',0)", [])
        .unwrap();
    store
        .item_store()
        .apply(&LocalVersionChange::Removed { item: removed })
        .unwrap();
    store
        .item_store()
        .apply(&LocalVersionChange::Moved {
            item: moved,
            place: ItemPlace {
                parent_local_item_id: destination,
                name: "new".into(),
            },
            is_name_ignored: false,
        })
        .unwrap();
    drop(store);
    set_stored_version(&path, "19");
    let (mut store, origin) = TreeStore::open_with_origin(&path).unwrap();
    assert_eq!(store.pins().unwrap(), ["REMOVED"]);
    assert_eq!(
        store
            .conn
            .query_row("SELECT is_kept FROM pins WHERE id='EXCEPTION'", [], |row| {
                row.get::<_, bool>(0)
            })
            .unwrap(),
        false
    );
    assert!(matches!(
        origin,
        StoreOrigin::MadeEmpty {
            loss: Some(RebuildLoss {
                pins: 0,
                unsent_changes_not_on_disk: 2,
                ..
            })
        }
    ));
    assert_eq!(
        store.item_store().list_items().unwrap().len(),
        1,
        "only root: nothing replayed"
    );
    // The new listing allocates different local ids. Pins follow cloud ids, and an
    // explicit exception remains an exception after the first listing too.
    crate::item_store::tests::shown_items::write_listed_root(&mut store);
    show_listed_file(&mut store, "FILLER", "R", "first", "h0");
    let restored = show_listed_file(&mut store, "REMOVED", "R", "gone", "h1");
    let excepted = show_listed_file(&mut store, "EXCEPTION", "R", "excepted", "h3");
    assert_ne!(restored, removed);
    assert!(store.item_store().is_pinned(restored).unwrap());
    assert!(!store.item_store().is_pinned(excepted).unwrap());
    let notice = store.recovery_notice().unwrap().unwrap();
    assert!(notice.contains("2 unsent"));
    let export = beside(&quarantines(&path)[0], ".unsent-changes.txt");
    let text = std::fs::read_to_string(&export).unwrap();
    for word in ["REMOVED", "MOVED", "/before/deep/gone", "/before/deep/old", "/after/deep/new", "NOT replayed"] {
        assert!(text.contains(word), "{text}");
    }
    assert!(text.lines().any(|line| line == "\"REMOVED\"\t\"file\"\t\"/before/deep/gone\"\tNull"), "{text}");
    assert!(!text.contains("NEVER-SHOWN") && !text.contains("SKIPPED"), "{text}");
    drop(store);
    assert_eq!(
        TreeStore::open(&path).unwrap().recovery_notice().unwrap(),
        Some(notice)
    );
    let original = quarantines(&path)[0].clone();
    for _ in 0..3 {
        set_stored_version(&path, "19");
        drop(TreeStore::open(&path).unwrap());
    }
    assert!(!original.exists(), "retention pruned the originating database");
    assert_eq!(std::fs::read_to_string(export).unwrap(), text, "resolved paths survive pruning");
}

#[test]
fn a_migration_retry_snapshots_new_work_without_overwriting_the_first_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    drop(TreeStore::open(&path).unwrap());
    set_stored_version(&path, "31");
    assert!(TreeStore::open(&path).is_err());
    let first = std::fs::read(beside(&path, ".pre-31")).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TRIGGER media_facts_item_gone; DROP TRIGGER media_facts_binding_gone; DROP TABLE media_facts; ALTER TABLE items DROP COLUMN onedrive_folder_ctag; INSERT INTO pins VALUES('NEW-WORK',1);").unwrap();
    drop(conn);
    let store = TreeStore::open(&path).unwrap();
    assert_eq!(store.pins().unwrap(), ["NEW-WORK"]);
    assert_eq!(std::fs::read(beside(&path, ".pre-31")).unwrap(), first);
    let retry = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tree.sqlite.pre-31-")
        })
        .unwrap();
    let snapshot = Connection::open(&retry).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT id FROM pins", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "NEW-WORK"
    );
    store.successful_start().unwrap();
    assert!(
        beside(&retry, ".successful-start").exists(),
        "retry snapshot gets the same retention clock"
    );
}

#[test]
fn an_earlier_namespace_export_remains_an_issue_after_another_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    let mut store = store_in_file_with_listed_root(&path);
    let removed = show_listed_file(&mut store, "REMOVED", "R", "gone", "h1");
    store
        .item_store()
        .apply(&LocalVersionChange::Removed { item: removed })
        .unwrap();
    drop(store);
    for _ in 0..5 {
        set_stored_version(&path, "19");
        let store = TreeStore::open(&path).unwrap();
        assert!(store
            .recovery_notice()
            .unwrap()
            .unwrap()
            .contains("1 unsent"));
        drop(store);
    }
    assert_eq!(quarantines(&path).len(), 3);
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".unsent-changes.txt"))
            .count(),
        1,
        "export survives pruning of its quarantine"
    );
}

#[test]
fn quarantine_pruning_leaves_unowned_backup_names_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    // A timestamp-shaped manual backup without the daemon's completed-quarantine mark.
    let manual = beside(&path, ".19-20000101T000000.000000000Z");
    std::fs::write(&manual, b"manual backup").unwrap();
    let other = beside(&path, ".19-00000000X000000.000000000Z");
    std::fs::write(&other, b"unrelated named file").unwrap();
    for _ in 0..5 {
        std::fs::write(&path, b"not a database").unwrap();
        drop(TreeStore::open(&path).unwrap());
    }
    assert_eq!(std::fs::read(manual).unwrap(), b"manual backup");
    assert_eq!(std::fs::read(other).unwrap(), b"unrelated named file");
}

#[test]
fn one_damaged_pin_does_not_prevent_recovery_of_readable_pins() {
    let dir = tempfile::tempdir().unwrap();
    let path = store_path(&dir);
    let store = store_in_file_with_listed_root(&path);
    store
        .conn
        .execute_batch("INSERT INTO pins VALUES('READABLE',1),('DAMAGED','bad flag');")
        .unwrap();
    drop(store);
    set_stored_version(&path, "19");
    let (store, origin) = TreeStore::open_with_origin(&path).unwrap();
    assert_eq!(store.pins().unwrap(), ["READABLE"]);
    assert!(matches!(
        origin,
        StoreOrigin::MadeEmpty {
            loss: Some(RebuildLoss { pins: 1, .. })
        }
    ));
    let preserved = Connection::open(&quarantines(&path)[0]).unwrap();
    assert_eq!(
        preserved
            .query_row("SELECT is_kept FROM pins WHERE id='DAMAGED'", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "bad flag"
    );
}

#[test]
fn an_older_stores_hot_rollback_journal_is_preserved_and_used_for_pin_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.sqlite");
    let path = store_path(&dir);
    let conn = Connection::open(&source).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA cache_size=1;
        CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
        INSERT INTO meta VALUES('schema_version','19');
        CREATE TABLE pins(id TEXT PRIMARY KEY, is_kept INTEGER);
        INSERT INTO pins VALUES('COMMITTED-PIN',1);
        CREATE TABLE payload(id INTEGER PRIMARY KEY, data BLOB);",
    )
    .unwrap();
    for _ in 0..32 {
        conn.execute("INSERT INTO payload(data) VALUES(zeroblob(65536))", [])
            .unwrap();
    }
    conn.execute_batch(
        "BEGIN IMMEDIATE; UPDATE pins SET is_kept=0; UPDATE payload SET data=randomblob(65536);",
    )
    .unwrap();
    // Copy the spilled main file and hot journal to model the state after process death.
    // Dropping the live connection rolls back only source.sqlite, not these copies.
    std::fs::copy(&source, &path).unwrap();
    std::fs::copy(beside(&source, "-journal"), beside(&path, "-journal")).unwrap();
    drop(conn);
    let main_before = std::fs::read(&path).unwrap();
    let journal_before = std::fs::read(beside(&path, "-journal")).unwrap();
    let store = TreeStore::open(&path).unwrap();
    assert_eq!(
        store.pins().unwrap(),
        ["COMMITTED-PIN"],
        "pin recovery must include rollback of an uncommitted older-store write"
    );
    let preserved = &quarantines(&path)[0];
    assert_eq!(std::fs::read(preserved).unwrap(), main_before);
    assert_eq!(
        std::fs::read(beside(preserved, "-journal")).unwrap(),
        journal_before
    );
    assert!(
        !beside(&path, "-journal").exists(),
        "no older journal may be left beside the replacement"
    );
}

#[test]
fn an_exported_name_that_is_not_utf8_keeps_its_bytes() {
    let conn = Connection::open_in_memory().unwrap();
    let rows = crate::schema::quarantine::rows(&conn, "SELECT '/' || CAST(X'6E6577FF' AS TEXT), '/' || CAST(X'6E6577FE' AS TEXT), '/new'").unwrap();
    assert_eq!(rows, [r#"b"/new\xff"	b"/new\xfe"	"/new""#]);
}
