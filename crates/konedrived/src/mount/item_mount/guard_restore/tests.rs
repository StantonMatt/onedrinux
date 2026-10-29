//! Verification is outside the serialized writer, with identity checked again inside.
use super::*;
use crate::change_guard::{ChangeGuard, GuardConfig};
use crate::mount::item_fixture::ItemFixture;
use std::time::Duration;

#[test]
fn retained_verification_leaves_the_write_thread_available_and_revalidates_identity() {
    for replace_during_verification in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let guard = Arc::new(ChangeGuard::open(
            dir.path().join("guard.json"),
            GuardConfig {
                removals: 0,
                ..Default::default()
            },
        ));
        let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
        let row = fixture.show_onedrive_file("doc", b"cloud bytes");
        fixture.land_copy(row.local_item_id, b"cloud bytes");
        let retained = guard.prepare_removal(&row, "doc", 100).unwrap();
        std::fs::create_dir_all(fixture.disk_path(retained.parent().unwrap().to_str().unwrap()))
            .unwrap();
        std::fs::rename(
            fixture.disk_path("doc"),
            fixture.disk_path(retained.to_str().unwrap()),
        )
        .unwrap();
        let removed = fixture.remove_here(row.local_item_id).unwrap();
        guard.observe(&[(removed, "doc".into())], 100).unwrap();
        let mount = Arc::new(fixture.build_mount());
        let batch = guard.decide(&["removal".into()], true).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let running = fixture.runtime().spawn({
            let mount = Arc::clone(&mount);
            let change = batch[0].clone();
            async move {
                mount
                    .restore_guard_change_preparing(change, false, move |backing, guard, id| {
                        let verified = prepare_retained(backing, guard, id)?;
                        entered_tx.send(()).unwrap();
                        resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                        Ok(verified)
                    })
                    .await
            }
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let thread = Arc::clone(&mount.folder_step_parts.write_operation_thread);
        let available = fixture.runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(2), thread.run_and_wait(|| true)).await
        });
        if replace_during_verification {
            let path = fixture.disk_path(retained.to_str().unwrap());
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"newer retained bytes").unwrap();
        }
        resume_tx.send(()).unwrap();
        assert!(
            available.unwrap().unwrap(),
            "verification must leave the mutation thread free"
        );
        let result = fixture.runtime().block_on(running).unwrap();
        if replace_during_verification {
            assert_eq!(result.unwrap_err(), "it was changed on this computer after it was held");
        } else { assert_eq!(result, Ok(true)); }
        if replace_during_verification {
            assert_eq!(
                std::fs::read(fixture.disk_path(retained.to_str().unwrap())).unwrap(),
                b"newer retained bytes"
            );
        }
        mount.stop();
    }
}

fn held_rewrite(fixture: &ItemFixture, guard: &ChangeGuard) -> Change {
    let row = fixture.show_onedrive_file("doc", b"cloud bytes");
    fixture.land_copy(row.local_item_id, b"cloud bytes");
    let row = fixture.change_file_here(row.local_item_id, b"local bytes");
    guard.observe(&[(row, "doc".into())], 100).unwrap();
    guard.decide(&["rewrite".into()], true).unwrap().remove(0)
}

#[test]
fn restore_distinguishes_later_cloud_changes_from_later_local_changes_and_legacy_holds() {
    for cause in [Failure::RemoteChanged, Failure::LocalChanged, Failure::Changed] {
        let dir = tempfile::tempdir().unwrap();
        let guard = Arc::new(ChangeGuard::open(dir.path().join("guard.json"), GuardConfig { rewrites: 0, ..Default::default() }));
        let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
        let mut change = held_rewrite(&fixture, &guard);
        let mount = fixture.build_mount();
        if cause == Failure::RemoteChanged {
            fixture.list_onedrive_file("doc", b"changed in cloud");
        } else {
            fixture.change_file_here(LocalItemId::new(change.item), b"changed locally");
            if cause == Failure::Changed { change.remote_generation = None; }
        }
        let result = fixture.runtime().block_on(mount.restore_guard_change(change.clone()));
        assert_eq!(result.unwrap_err(), cause.to_string());
        assert_eq!(fixture.read_disk_file("doc").unwrap(), if cause == Failure::RemoteChanged { &b"local bytes"[..] } else { &b"changed locally"[..] });
        guard.restored(&change, false).unwrap();
        assert!(guard.blocks(LocalItemId::new(change.item)));
        mount.stop();
    }
}

#[test]
fn restore_reports_an_open_writer_without_moving_its_file() {
    use crate::mount::item_handles::OpenItemFile;
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(dir.path().join("guard.json"), GuardConfig { rewrites: 0, ..Default::default() }));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let change = held_rewrite(&fixture, &guard);
    let mount = fixture.build_mount();
    let handles = mount.folder_step_parts().item_handles;
    let fh = handles.add_file(OpenItemFile::Writing { item: LocalItemId::new(change.item), backing_file: Arc::new(std::fs::OpenOptions::new().write(true).open(fixture.disk_path("doc")).unwrap()) });
    assert_eq!(fixture.runtime().block_on(mount.restore_guard_change(change)).unwrap_err(), Failure::Open.to_string());
    assert_eq!(fixture.read_disk_file("doc").unwrap(), b"local bytes");
    handles.remove_writer(fh);
    mount.stop();
}

#[test]
fn restore_reports_exhausted_copy_names_without_touching_any_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(dir.path().join("guard.json"), GuardConfig { rewrites: 0, ..Default::default() }));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let change = held_rewrite(&fixture, &guard);
    for attempt in 1..=100 {
        let name = konedrive_tree::make_conflict_copy_name(std::ffi::OsStr::new("doc"), "restored-local", attempt);
        fixture.make_own_file(name.to_str().unwrap(), b"another file");
    }
    let mount = fixture.build_mount();
    assert_eq!(fixture.runtime().block_on(mount.restore_guard_change(change)).unwrap_err(), Failure::NoCopyName.to_string());
    assert_eq!(fixture.read_disk_file("doc").unwrap(), b"local bytes");
    assert_eq!(fixture.list_items().len(), 102);
    mount.stop();
}

#[test]
fn restore_reports_missing_local_bytes_and_a_stopped_mount() {
    for stopped in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let guard = Arc::new(ChangeGuard::open(dir.path().join("guard.json"), GuardConfig { rewrites: 0, ..Default::default() }));
        let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
        let change = held_rewrite(&fixture, &guard);
        let mount = fixture.build_mount();
        if stopped { mount.stop(); } else { std::fs::remove_file(fixture.disk_path("doc")).unwrap(); }
        assert_eq!(fixture.runtime().block_on(mount.restore_guard_change(change)).unwrap_err(), if stopped { Failure::Stopped } else { Failure::LocalMissing }.to_string());
        mount.stop();
    }
}

#[test]
fn disk_refusals_keep_their_specific_cause() {
    use crate::mount::PlaceRefusal;
    let path = PathBuf::from("doc");
    assert_eq!(place_failure(&PlaceRefusal::PlaceHeld { path: path.clone() }), Failure::Occupied.to_string());
    for (errno, reason) in [(libc::EEXIST, Failure::Occupied), (libc::ENOTEMPTY, Failure::Occupied), (libc::ENOTDIR, Failure::ParentMissing), (libc::ENOENT, Failure::ParentMissing), (libc::EINVAL, Failure::ParentMissing), (libc::EBUSY, Failure::Open), (libc::ETXTBSY, Failure::Open), (libc::EACCES, Failure::Storage), (libc::ENOSPC, Failure::Storage)] {
        assert_eq!(place_failure(&PlaceRefusal::Disk { errno, path: path.clone() }), reason.to_string());
    }
    assert_eq!(place_failure(&PlaceRefusal::NotPutBack { errno: libc::EIO, path }), Failure::Storage.to_string());
}
