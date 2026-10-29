//! Offline restore, collisions and preservation of unique local bytes.
use super::*;
use crate::change_guard::{ChangeGuard, GuardConfig};

fn guarded() -> (tempfile::TempDir, Arc<ChangeGuard>, OperationsFixture) {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(
        dir.path().join("guard.json"),
        GuardConfig {
            removals: 0,
            rewrites: 0,
            moves: 0,
            uploads: 0,
            ..Default::default()
        },
    ));
    let operations = OperationsFixture::with_report_and_guard(
        |_| crate::status::report::Report::nowhere(),
        Some(Arc::clone(&guard)),
    );
    (dir, guard, operations)
}
#[test]
fn held_removals_restore_offline_and_refuse_a_replacement() {
    let (_dir, guard, operations) = guarded();
    let file = operations
        .fixture
        .show_onedrive_file("doc", b"offline bytes")
        .local_item_id;
    operations.fixture.land_copy(file, b"offline bytes");
    operations.remove("doc", ItemKind::File).unwrap();
    let retained = guard.retained(file).unwrap();
    assert_eq!(
        std::fs::read(operations.fixture.disk_path(retained.to_str().unwrap())).unwrap(),
        b"offline bytes"
    );
    assert!(guard.blocks(file));
    // The retained directory is deliberately skipped by the real start-up scan.
    let mount = operations.fixture.build_mount();
    let batch = guard.decide(&[], true).unwrap();
    assert_eq!(batch.len(), 1);
    let restored = operations
        .fixture
        .runtime()
        .block_on(mount.restore_guard_change(batch[0].clone()))
        .unwrap();
    assert!(restored);
    guard.restored(&batch[0], restored).unwrap();
    assert_eq!(
        operations.fixture.read_disk_file("doc").unwrap(),
        b"offline bytes"
    );
    let restored_row = operations.fixture.find_item(file).unwrap();
    assert_eq!(
        restored_row.local.as_ref().unwrap().content,
        restored_row.synced_content,
        "verified retained bytes return clean"
    );
    assert!(guard.retained(file).is_none());
    mount.stop();

    let (_dir, guard, operations) = guarded();
    let file = operations
        .fixture
        .show_onedrive_file("doc", b"keep me")
        .local_item_id;
    operations.fixture.land_copy(file, b"keep me");
    operations.remove("doc", ItemKind::File).unwrap();
    operations.fixture.make_own_file("doc", b"newer local work");
    let mount = operations.fixture.build_mount();
    let batch = guard.decide(&["removal".into()], true).unwrap();
    let result = operations
        .fixture
        .runtime()
        .block_on(mount.restore_guard_change(batch[0].clone()));
    assert_eq!(result.unwrap_err(), "a file of the same name is in its place");
    guard.restored(&batch[0], false).unwrap();
    assert_eq!(
        operations.fixture.read_disk_file("doc").unwrap(),
        b"newer local work"
    );
    assert!(operations
        .fixture
        .disk_path(guard.retained(file).unwrap().to_str().unwrap())
        .exists());
    assert!(guard.blocks(file));
    mount.stop();
}
#[test]
fn held_rewrites_keep_local_bytes_without_uploading_the_copy() {
    let (_dir, guard, operations) = guarded();
    let file = operations
        .fixture
        .show_onedrive_file("doc", b"cloud bytes")
        .local_item_id;
    operations.fixture.land_copy(file, b"cloud bytes");
    let changed = operations.fixture.change_file_here(file, b"local rewrite");
    guard.observe(&[(changed, "doc".into())], 100).unwrap();
    let mount = operations.fixture.build_mount();
    // Start-up can fill hashes but cannot approve this generation.
    let batch = guard.decide(&["rewrite".into()], true).unwrap();
    let result = operations
        .fixture
        .runtime()
        .block_on(mount.restore_guard_change(batch[0].clone()))
        .unwrap();
    assert!(result);
    guard.restored(&batch[0], true).unwrap();
    let copies: Vec<_> = operations
        .fixture
        .list_items()
        .into_iter()
        .filter(|r| r.onedrive_item.is_none())
        .collect();
    assert_eq!(copies.len(), 1);
    let copy = &copies[0];
    assert!(guard.blocks(copy.local_item_id));
    let path = copy
        .local
        .as_ref()
        .unwrap()
        .place
        .as_ref()
        .unwrap()
        .name
        .to_str()
        .unwrap();
    assert_eq!(
        operations.fixture.read_disk_file(path).unwrap(),
        b"local rewrite"
    );
    let restored = operations.fixture.find_item(file).unwrap();
    assert_eq!(
        restored.local.as_ref().unwrap().content,
        restored.synced_content
    );
    assert_eq!(restored.content_presence, ContentPresence::NotOnDisk);
    mount.stop();
}
#[test]
fn restore_of_a_newer_rewrite_generation_leaves_its_bytes_and_the_hold() {
    let (_dir, guard, operations) = guarded();
    let file = operations
        .fixture
        .show_onedrive_file("doc", b"cloud bytes")
        .local_item_id;
    operations.fixture.land_copy(file, b"cloud bytes");
    let changed = operations.fixture.change_file_here(file, b"first rewrite");
    guard.observe(&[(changed, "doc".into())], 100).unwrap();
    let batch = guard.decide(&["rewrite".into()], true).unwrap();
    operations.fixture.change_file_here(file, b"newer rewrite");
    let mount = operations.fixture.build_mount();
    let result = operations
        .fixture
        .runtime()
        .block_on(mount.restore_guard_change(batch[0].clone()));
    assert_eq!(result.unwrap_err(), "it was changed on this computer after it was held");
    guard.restored(&batch[0], false).unwrap();
    assert_eq!(
        operations.fixture.read_disk_file("doc").unwrap(),
        b"newer rewrite"
    );
    assert!(guard.blocks(file));
    mount.stop();
}

#[test]
fn restoring_a_file_changed_in_content_and_place_keeps_both_decisions_atomic() {
    let (_dir, guard, operations) = guarded();
    let file = operations
        .fixture
        .show_onedrive_file("doc", b"cloud bytes")
        .local_item_id;
    operations.fixture.land_copy(file, b"cloud bytes");
    operations.fixture.change_file_here(file, b"local rewrite");
    let moved = operations.fixture.move_here(file, "renamed");
    guard.observe(&[(moved, "renamed".into())], 100).unwrap();
    let batch = guard.decide(&[], true).unwrap();
    assert_eq!(batch.len(), 2);
    let rewrite = batch
        .iter()
        .find(|c| c.kind == crate::change_guard::Kind::Rewrite)
        .unwrap();
    let mount = operations.fixture.build_mount();
    assert!(operations
        .fixture
        .runtime()
        .block_on(mount.restore_guard_change_taking_move(rewrite.clone(), true))
        .unwrap());
    for change in &batch {
        guard.restored(change, true).unwrap();
    }
    let row = operations.fixture.find_item(file).unwrap();
    assert_eq!(row.local.as_ref().unwrap().place, row.synced_place);
    assert_eq!(row.local.as_ref().unwrap().content, row.synced_content);
    assert!(!operations.fixture.disk_path("renamed").exists());
    let copy = operations
        .fixture
        .list_items()
        .into_iter()
        .find(|r| r.onedrive_item.is_none())
        .unwrap();
    let path = copy
        .local
        .as_ref()
        .unwrap()
        .place
        .as_ref()
        .unwrap()
        .name
        .to_str()
        .unwrap();
    assert_eq!(
        operations.fixture.read_disk_file(path).unwrap(),
        b"local rewrite"
    );
    assert!(guard.blocks(copy.local_item_id));
    mount.stop();
}

#[test]
fn unique_unsent_bytes_restore_even_after_their_unbound_rows_and_parent_are_gone() {
    let (_dir, guard, operations) = guarded();
    operations.fixture.make_own_directory("scratch");
    let file = operations
        .fixture
        .make_own_file("scratch/unique", b"never uploaded")
        .local_item_id;
    operations.remove("scratch/unique", ItemKind::File).unwrap();
    operations.remove("scratch", ItemKind::Directory).unwrap();
    assert!(operations.fixture.find_item(file).is_none());
    assert!(guard.retained(file).is_some());
    let mount = operations.fixture.build_mount();
    let batch = guard.decide(&["removal".into()], true).unwrap();
    assert_eq!(batch.len(), 1);
    assert!(operations
        .fixture
        .runtime()
        .block_on(mount.restore_guard_change(batch[0].clone()))
        .unwrap());
    guard.restored(&batch[0], true).unwrap();
    assert_eq!(
        operations.fixture.read_disk_file("scratch/unique").unwrap(),
        b"never uploaded"
    );
    let restored = operations.fixture.find_item_at("scratch/unique").unwrap();
    let parent = operations.fixture.find_item_at("scratch").unwrap();
    assert!(guard.blocks(restored.local_item_id));
    assert!(guard.blocks(parent.local_item_id));
    assert!(guard.retained(file).is_none());
    mount.stop();
}

#[test]
fn a_restore_with_an_unsynced_record_keeps_the_retained_copy_and_hold() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, guard, operations) = guarded();
    let file = operations.fixture.show_onedrive_file("doc", b"retained bytes").local_item_id;
    operations.fixture.land_copy(file, b"retained bytes");
    operations.remove("doc", ItemKind::File).unwrap();
    let retained = guard.retained(file).unwrap();
    let mount = operations.fixture.build_mount();
    let batch = guard.decide(&["removal".into()], true).unwrap();
    let wal = operations.fixture.store_path.with_file_name("tree.db-wal");
    std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone()));
    std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(result.unwrap_err(), "this computer couldn't save the restored file");
    guard.restored(&batch[0], false).unwrap();
    assert!(guard.blocks(file));
    assert_eq!(operations.fixture.read_disk_file("doc"), None);
    assert_eq!(std::fs::read(operations.fixture.disk_path(retained.to_str().unwrap())).unwrap(), b"retained bytes");
    mount.stop();
}

#[test]
fn retained_old_bytes_restore_as_a_local_only_copy_of_the_new_remote_baseline() {
    let (_dir, guard, operations) = guarded();
    let file = operations.fixture.show_onedrive_file("doc", b"version A").local_item_id;
    operations.fixture.land_copy(file, b"version A");
    operations.remove("doc", ItemKind::File).unwrap();
    let remote = operations.fixture.list_onedrive_file("doc", b"version B");
    guard.observe(&[(remote, "doc".into())], 200).unwrap();
    let mount = operations.fixture.build_mount();
    let batch = guard.decide(&["removal".into()], true).unwrap();
    assert!(operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone())).unwrap());
    guard.restored(&batch[0], true).unwrap();
    let row = operations.fixture.find_item(file).unwrap();
    assert_eq!(row.local.as_ref().unwrap().content, row.onedrive.as_ref().unwrap().content);
    assert_eq!(row.synced_content, row.onedrive.as_ref().unwrap().content);
    assert_eq!(row.content_presence, ContentPresence::NotOnDisk);
    let copy = operations.fixture.list_items().into_iter().find(|r| r.onedrive_item.is_none()).unwrap();
    let name = copy.local.as_ref().unwrap().place.as_ref().unwrap().name.to_str().unwrap();
    assert_eq!(operations.fixture.read_disk_file(name).unwrap(), b"version A");
    assert!(guard.local_only().contains(&copy.local_item_id));
    mount.stop();
}

#[test]
fn rewrite_restore_waits_for_resident_readers() {
    use crate::mount::item_handles::OpenItemFile;
    // Restoring a removed file against a newer remote baseline switches that same
    // FUSE inode too, while a pre-unlink reader can still hold its retained inode.
    for removal in [false, true] {
        let (_dir, guard, operations) = guarded();
        let file = operations.fixture.show_onedrive_file("doc", b"cloud bytes").local_item_id;
        operations.fixture.land_copy(file, b"cloud bytes");
        let open = Arc::new(std::fs::File::open(operations.fixture.disk_path("doc")).unwrap());
        let kind = if removal {
            operations.remove("doc", ItemKind::File).unwrap();
            let row = operations.fixture.list_onedrive_file("doc", b"newer remote");
            guard.observe(&[(row, "doc".into())], 100).unwrap();
            "removal"
        } else {
            let changed = operations.fixture.change_file_here(file, b"local rewrite");
            guard.observe(&[(changed, "doc".into())], 100).unwrap();
            "rewrite"
        };
        let mount = operations.fixture.build_mount();
        let handles = mount.folder_step_parts().item_handles;
        let reader = handles.add_file(OpenItemFile::ReadingBackingFile { item: file, backing_file: open });
        let batch = guard.decide(&[kind.into()], true).unwrap();
        assert_eq!(operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone())).unwrap_err(), "it is open in another program");
        handles.close_reader(reader);
        assert!(operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone())).unwrap());
        guard.restored(&batch[0], true).unwrap();
        mount.stop();
    }
}

#[test]
fn a_committed_restore_is_reconciled_after_the_ledger_completion_crash_window() {
    for kind in ["removal", "rewrite", "move"] {
        let (dir, guard, operations) = guarded();
        let file = operations.fixture.show_onedrive_file("doc", b"cloud bytes").local_item_id;
        operations.fixture.land_copy(file, b"cloud bytes");
        match kind {
            "removal" => operations.remove("doc", ItemKind::File).unwrap(),
            "rewrite" => { let row = operations.fixture.change_file_here(file, b"local bytes"); guard.observe(&[(row, "doc".into())], 100).unwrap(); },
            _ => { let row = operations.fixture.move_here(file, "renamed"); guard.observe(&[(row, "renamed".into())], 100).unwrap(); },
        }
        let mount = operations.fixture.build_mount();
        let batch = guard.decide(&[kind.into()], true).unwrap();
        assert!(operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone())).unwrap());
        // Deliberately omit guard.restored: this is the crash window under review.
        mount.stop();
        let recovered = Arc::new(ChangeGuard::open(dir.path().join("guard.json"), Default::default()));
        assert!(recovered.blocks(file));
        operations.fixture.store.call_blocking({ let recovered = Arc::clone(&recovered); move |s| recovered.reconcile_restores(&s.item_store().reader()) }).unwrap();
        assert!(!recovered.blocks(file), "completed {kind} must not be held forever");
        assert_eq!(operations.fixture.find_item(file).unwrap().local.as_ref().unwrap().content, operations.fixture.find_item(file).unwrap().synced_content);
    }
}

#[test]
fn allowing_an_unbound_removal_cleans_only_its_approved_retained_bytes() {
    let (_dir, guard, operations) = guarded();
    let file = operations.fixture.make_own_file("temporary", b"unique bytes").local_item_id;
    operations.remove("temporary", ItemKind::File).unwrap();
    let retained = guard.retained(file).unwrap();
    operations.fixture.run_on_write_operation_thread({ let files = operations.fixture.make_content_files(); move || files.retry_guard_removals().unwrap() });
    assert!(operations.fixture.disk_path(retained.to_str().unwrap()).exists(), "unapproved unique bytes stay");
    guard.decide(&["removal".into()], false).unwrap();
    operations.fixture.run_on_write_operation_thread({ let files = operations.fixture.make_content_files(); move || files.retry_guard_removals().unwrap() });
    assert!(!operations.fixture.disk_path(retained.to_str().unwrap()).exists());
    assert!(guard.retained(file).is_none());
}

#[test]
fn a_sent_removals_retained_cleanup_retries_after_failure_and_restart() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, guard, operations) = guarded();
    let file = operations.fixture.show_onedrive_file("doc", b"cloud bytes").local_item_id;
    operations.fixture.land_copy(file, b"cloud bytes");
    operations.remove("doc", ItemKind::File).unwrap();
    let retained = guard.retained(file).unwrap();
    let batch = guard.decide(&["removal".into()], false).unwrap();
    let planned = operations.fixture.versions_of(file);
    guard.confirm_removals(&[file]).unwrap();
    operations.fixture.write_rows(move |s| s.item_store().commit_send_file_removal(&planned));
    // No guard.sent call: a crash after SQL commit must still leave cleanup ownership.
    let retained_dir = operations.fixture.disk_path(retained.parent().unwrap().to_str().unwrap());
    std::fs::set_permissions(&retained_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    operations.fixture.run_on_write_operation_thread({ let files = operations.fixture.make_content_files(); move || files.retry_guard_removals().unwrap() });
    assert!(guard.retained(file).is_some());
    std::fs::set_permissions(&retained_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let recovered = ChangeGuard::open(dir.path().join("guard.json"), Default::default());
    let clean = operations.fixture.store.call_blocking(move |s| recovered.retained_cleanup(&s.item_store().reader())).unwrap();
    assert_eq!(clean, vec![(file, retained.clone())]);
    operations.fixture.run_on_write_operation_thread({ let files = operations.fixture.make_content_files(); move || files.retry_guard_removals().unwrap() });
    assert!(!operations.fixture.disk_path(retained.to_str().unwrap()).exists());
    assert!(guard.retained(file).is_none());
    let _ = batch;
}

#[test]
fn rewrite_restore_bounds_conflict_names_and_tries_collisions() {
    let (_dir, guard, operations) = guarded();
    let name = "a".repeat(255);
    let file = operations.fixture.show_onedrive_file(&name, b"cloud bytes").local_item_id;
    operations.fixture.land_copy(file, b"cloud bytes");
    let changed = operations.fixture.change_file_here(file, b"local bytes");
    let first = konedrive_tree::make_conflict_copy_name(std::ffi::OsStr::new(&name), "restored-local", 1);
    operations.fixture.make_own_file(first.to_str().unwrap(), b"collision");
    guard.observe(&[(changed, name.clone())], 100).unwrap();
    let mount = operations.fixture.build_mount();
    let batch = guard.decide(&["rewrite".into()], true).unwrap();
    assert!(operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone())).unwrap());
    assert_eq!(operations.fixture.read_disk_file(first.to_str().unwrap()).unwrap(), b"collision");
    let second = konedrive_tree::make_conflict_copy_name(std::ffi::OsStr::new(&name), "restored-local", 2);
    assert_eq!(operations.fixture.read_disk_file(second.to_str().unwrap()).unwrap(), b"local bytes");
    mount.stop();
}

#[test]
fn ignored_mount_items_do_not_spend_upload_budgets_or_hold_eligible_work() {
    let (_dir, guard, operations) = guarded();
    guard.configure(GuardConfig { uploads: 1, upload_bytes: 4, ..Default::default() });
    *crate::panic::write(&operations.fixture.ignore) = crate::local::IgnoreList::new(["*.tmp", "*.crdownload", "cache"]);
    operations.make("cache", MadeKind::Directory).unwrap();
    for path in ["download.tmp", "download.crdownload", "cache/ordinary.txt"] {
        let made = operations.make(path, MadeKind::FileOpenedForWriting).unwrap();
        operations.write_through(made.fh, b"ignored bytes over the upload limit");
        operations.close(made.fh);
        assert!(!guard.blocks(made.item.local_item_id), "{path} never goes to the sender");
    }
    assert!(guard.snapshot().unwrap().1.is_empty());
    let made = operations.make("eligible.txt", MadeKind::File).unwrap();
    assert!(!guard.blocks(made.item.local_item_id), "ignored work cannot latch uploads");
    // The planner's ignored eligibility also applies in a snapshot observation.
    let rows = operations.fixture.list_items();
    let observed = Arc::clone(&guard);
    operations.fixture.read_rows(move |items| {
        let rows = rows.into_iter().map(|row| {
            let path = items.find_path(row.local_item_id)?.unwrap_or_default().display().to_string();
            Ok((row, path))
        }).collect::<Result<Vec<_>, konedrive_tree::TreeError>>()?;
        observed.observe_items(items, &rows, 100)
    });
    assert!(guard.snapshot().unwrap().1.is_empty());
}

#[test]
fn deleting_a_held_never_uploaded_file_retires_only_its_upload_observation() {
    let (_dir, guard, operations) = guarded();
    let made = operations.make("unique", MadeKind::FileOpenedForWriting).unwrap();
    operations.write_through(made.fh, b"unique bytes");
    operations.close(made.fh);
    let file = made.item.local_item_id;
    assert_eq!(guard.snapshot().unwrap().1[0].kind, "upload");
    operations.remove("unique", ItemKind::File).unwrap();
    assert!(operations.fixture.find_item(file).is_none());
    let held = guard.snapshot().unwrap().1;
    assert_eq!(held.len(), 1, "only retained-removal recovery remains");
    assert_eq!(held[0].kind, "removal");
    assert_eq!(std::fs::read(operations.fixture.disk_path(guard.retained(file).unwrap().to_str().unwrap())).unwrap(), b"unique bytes");
    let batch = guard.decide(&["removal".into()], true).unwrap();
    let mount = operations.fixture.build_mount();
    assert!(operations.fixture.runtime().block_on(mount.restore_guard_change(batch[0].clone())).unwrap());
    guard.restored(&batch[0], true).unwrap();
    assert_eq!(operations.fixture.read_disk_file("unique").unwrap(), b"unique bytes");
    mount.stop();
}
