//! Results use the account bridge, with no mount, Graph, or real session bus.
use crate::account_folder::testing;
use crate::change_guard::GuardConfig;
use crate::mount::item_fixture::ItemFixture;
use crate::status::sync_trouble::{CycleError, SyncTrouble};

#[tokio::test]
async fn allowing_while_offline_reports_durably_queued_changes() {
    let service = testing::wiring().build();
    let guard = service.change_guard().unwrap();
    guard.configure(GuardConfig {
        uploads: 0,
        ..Default::default()
    });
    let fixture = ItemFixture::new();
    let row = fixture.make_own_file("new", b"local bytes");
    guard.observe(&[(row, "new".into())], 100).unwrap();
    let token = guard.snapshot().unwrap().0;
    service.state().update(|s| {
        s.cycle.sync_trouble = Some(SyncTrouble::Cycle(CycleError::Offline(
            "offline test".into(),
        )))
    });
    let result = service.allow_changes(token, vec![]).await.unwrap();
    assert_eq!(
        (result.done, result.pending_offline, result.failed),
        (0, 1, 0)
    );
    assert!(guard.held().is_empty());
}

#[tokio::test]
async fn restore_reports_partial_failures_and_final_progress_without_a_mount() {
    let service = testing::wiring().build();
    let guard = service.change_guard().unwrap();
    guard.configure(GuardConfig {
        uploads: 0,
        removals: 0,
        ..Default::default()
    });
    let fixture = ItemFixture::new();
    let new = fixture.make_own_file("new", b"local bytes");
    let old = fixture.show_onedrive_file("old", b"cloud bytes");
    let old = fixture.remove_here(old.local_item_id).unwrap();
    guard
        .observe(&[(new.clone(), "new".into()), (old, "old".into())], 100)
        .unwrap();
    let token = guard.snapshot().unwrap().0;
    let mut progress = guard.subscribe_progress();
    let result = service
        .restore_changes(token.clone(), vec![])
        .await
        .unwrap();
    assert_eq!(
        (
            result.done,
            result.pending_offline,
            result.failed,
            result.kept_both_versions
        ),
        (1, 0, 1, 0)
    );
    assert_eq!(result.failures[0].1, "old");
    assert_eq!(result.failures[0].2, "this account's folder is not mounted");
    assert!(guard.local_only().contains(&new.local_item_id));
    let (_, rows) = guard.snapshot().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "removal");
    let mut messages = vec![];
    while let Ok(message) = progress.try_recv() {
        messages.push(message);
    }
    assert_eq!(messages.first().unwrap(), &(token.clone(), vec![], 0, 2));
    assert_eq!(messages.last().unwrap(), &(token, vec![], 2, 2));
}
