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
        let result = fixture.runtime().block_on(running).unwrap().unwrap();
        assert_eq!(
            result, !replace_during_verification,
            "commit must revalidate the verified inode"
        );
        if replace_during_verification {
            assert_eq!(
                std::fs::read(fixture.disk_path(retained.to_str().unwrap())).unwrap(),
                b"newer retained bytes"
            );
        }
        mount.stop();
    }
}
