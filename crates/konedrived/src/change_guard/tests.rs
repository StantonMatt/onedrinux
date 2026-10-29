//! Window decisions and durable admission, independent of any real drive or bus.
use super::ledger::Disposition;
use super::*;
use crate::mount::item_fixture::ItemFixture;
use std::sync::Arc;

fn event(item: i64, kind: Kind, bytes: u64, generation: &str, at: i64) -> Change {
    Change {
        item,
        kind,
        path: format!("f{item}"),
        bytes,
        after_bytes: bytes,
        old_path: String::new(),
        directory: false,
        descendants: 0,
        revision: 0,
        first_held_revision: 0,
        at,
        generation: generation.into(),
        write_count: None,
        provisional: false,
        disposition: Disposition::Pending,
    }
}
fn config() -> GuardConfig {
    GuardConfig {
        removals: 2,
        rewrites: 2,
        moves: 2,
        uploads: 2,
        removal_bytes: 20,
        rewrite_bytes: 20,
        upload_bytes: 20,
        daily_removals: 4,
        ..Default::default()
    }
}
#[test]
fn thresholds_latch_the_whole_pending_batch() {
    for kind in [Kind::Removal, Kind::Rewrite, Kind::Move, Kind::Upload] {
        let mut ledger = Ledger::default();
        for id in 1..=2 {
            assert!(ledger.observe(event(id, kind, 5, "one", 100), &config(), 100));
        }
        assert!(ledger.current.values().all(|c| !c.disposition.blocks()));
        ledger.observe(event(3, kind, 5, "one", 100), &config(), 100);
        assert_eq!(ledger.current.len(), 3);
        assert!(ledger
            .current
            .values()
            .all(|c| c.disposition == Disposition::Held));
        ledger.observe(event(4, kind, 0, "one", 10000), &config(), 10000);
        assert!(
            ledger
                .current
                .values()
                .all(|c| c.disposition == Disposition::Held),
            "window expiry does not clear a latch"
        );
    }
}
#[test]
fn daily_removals_and_original_bytes_are_charged() {
    let mut ledger = Ledger::default();
    let config = GuardConfig {
        removals: 100,
        daily_removals: 2,
        ..config()
    };
    for (id, at) in [(1, 100), (2, 800), (3, 1500)] {
        ledger.observe(event(id, Kind::Removal, 0, "one", at), &config, at);
    }
    assert!(ledger.latched.contains(&Kind::Removal));
    let mut ledger = Ledger::default();
    ledger.observe(
        event(1, Kind::Rewrite, 21, "truncated-to-zero", 100),
        &config,
        100,
    );
    assert!(ledger.latched.contains(&Kind::Rewrite));
}
#[test]
fn counters_expire_at_the_boundary_and_backwards_time_does_not_expire_them() {
    let mut ledger = Ledger::default();
    ledger.observe(event(1, Kind::Move, 0, "one", 1000), &config(), 1000);
    ledger.observe(event(2, Kind::Move, 0, "one", 200), &config(), 200);
    assert_eq!(ledger.events[0].at, 1000);
    ledger.observe(event(3, Kind::Move, 0, "one", 1600), &config(), 1600);
    assert!(!ledger.latched.contains(&Kind::Move));
    assert_eq!(
        ledger.events.iter().map(|c| c.count).sum::<u64>(),
        3,
        "daily history is retained independently of the short window"
    );
}
#[test]
fn repeated_rewrites_of_one_item_trip_the_window_without_double_charging_a_plan() {
    let mut ledger = Ledger::default();
    for generation in ["one", "two", "three"] {
        assert!(ledger.observe(event(1, Kind::Rewrite, 1, generation, 100), &config(), 100));
        assert!(!ledger.observe(event(1, Kind::Rewrite, 1, generation, 100), &config(), 100));
    }
    assert_eq!(ledger.events.iter().map(|c| c.count).sum::<u64>(), 3);
    assert!(ledger.latched.contains(&Kind::Rewrite));
}
#[test]
fn rewrite_counter_distinguishes_equal_size_and_time_and_survives_hashing() {
    let mut ledger = Ledger::default();
    for count in 1..=3 {
        let mut c = event(1, Kind::Rewrite, 1, "same-metadata", 100);
        c.write_count = Some(count);
        assert!(ledger.observe(c, &config(), 100));
    }
    assert!(ledger.latched.contains(&Kind::Rewrite));
    assert!(!ledger.observe(
        event(1, Kind::Rewrite, 1, "same-metadata", 100),
        &config(),
        100
    ));
}
#[test]
fn approval_is_for_an_immutable_generation() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let row = fixture.make_own_file("one", b"one");
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            uploads: 0,
            ..config()
        },
    );
    guard.observe(&[(row.clone(), "one".into())], 100).unwrap();
    assert!(guard.blocks(row.local_item_id));
    let batch = guard.decide(&[], false).unwrap();
    assert_eq!(batch.len(), 1);
    let step = |row: &Item| OneDriveStep::SendNewFile {
        item: ItemVersions::of(row),
        parent: ItemFixture::onedrive_reference_of(""),
        name: "one".into(),
    };
    assert!(guard.admit(&step(&row), 100).unwrap());
    let changed = fixture.change_file_here(row.local_item_id, b"another");
    guard
        .observe(&[(changed.clone(), "one".into())], 101)
        .unwrap();
    assert!(!guard.admit(&step(&changed), 101).unwrap());
    assert!(!guard.admit(&step(&row), 101).unwrap());
    guard.restored(&batch[0], true).unwrap();
    assert!(
        guard.blocks(row.local_item_id),
        "settling the old batch cannot release later work"
    );
}
#[test]
fn removal_quiet_time_cannot_be_bypassed_by_content_times_or_restart() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let row = fixture.show_onedrive_file("one", b"cloud");
    let removed = fixture.remove_here(row.local_item_id).unwrap();
    let file = dir.path().join("ledger.json");
    let guard = ChangeGuard::open(file.clone(), config());
    guard
        .observe(&[(removed.clone(), "one".into())], 100)
        .unwrap();
    let step = OneDriveStep::SendFileRemoval {
        item: ItemVersions::of(&removed),
    };
    assert!(!guard.admit(&step, 129).unwrap());
    drop(guard);
    let guard = ChangeGuard::open(file, config());
    assert!(!guard.admit(&step, 129).unwrap());
    assert!(guard.admit(&step, 130).unwrap());
}
#[test]
fn restart_keeps_holds_and_counters() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let reference = ItemFixture::onedrive_reference_of("leftover");
    {
        let guard = ChangeGuard::open(
            file.clone(),
            GuardConfig {
                removals: 0,
                ..config()
            },
        );
        assert!(!guard
            .admit_cleanup(&reference, 10, "etag-one", 100)
            .unwrap());
        assert_eq!(guard.list(false).len(), 1);
    }
    let guard = ChangeGuard::open(file, config());
    assert!(!guard
        .admit_cleanup(&reference, 10, "etag-one", 10000)
        .unwrap());
    assert_eq!(crate::panic::lock(&guard.inner).ledger.events.len(), 1);
    guard.decide(&[], false).unwrap();
    assert!(guard
        .admit_cleanup(&reference, 10, "etag-one", 10000)
        .unwrap());
    assert!(
        !guard
            .admit_cleanup(&reference, 10, "etag-two", 10001)
            .unwrap(),
        "changed cleanup needs fresh admission"
    );
}
#[test]
fn concurrent_admission_cannot_spend_the_same_budget() {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(dir.path().join("ledger.json"), config()));
    let threads: Vec<_> = (0..20)
        .map(|n| {
            let guard = Arc::clone(&guard);
            std::thread::spawn(move || {
                assert!(!guard
                    .admit_cleanup(
                        &ItemFixture::onedrive_reference_of(&format!("f{n}")),
                        1,
                        "e",
                        100
                    )
                    .unwrap());
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(guard.list(false).len(), 20);
    assert_eq!(
        crate::panic::lock(&guard.inner)
            .ledger
            .events
            .iter()
            .map(|c| c.count)
            .sum::<u64>(),
        20
    );
}
#[test]
fn corrupt_or_unwritable_state_never_grants_admission_or_overwrites_the_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    std::fs::write(&file, b"corrupt").unwrap();
    let guard = ChangeGuard::open(file.clone(), config());
    assert!(guard.decide(&[], false).is_err());
    assert!(
        guard
            .admit_cleanup(&ItemFixture::onedrive_reference_of("x"), 0, "e", 100)
            .is_err()
            || guard.error().is_some()
    );
    assert_eq!(std::fs::read(file).unwrap(), b"corrupt");
    let not_dir = dir.path().join("not-a-directory");
    std::fs::write(&not_dir, b"keep").unwrap();
    let guard = ChangeGuard::open(not_dir.join("ledger.json"), config());
    assert!(guard
        .admit_cleanup(&ItemFixture::onedrive_reference_of("x"), 0, "e", 100)
        .is_err());
}
#[test]
fn a_directory_removal_requires_admission_for_every_descendant() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let parent = fixture.show_onedrive_directory("tree");
    let child = fixture.show_onedrive_file("tree/child", b"cloud");
    let child = fixture.remove_here(child.local_item_id).unwrap();
    let parent = fixture.remove_here(parent.local_item_id).unwrap();
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            removals: 1,
            ..config()
        },
    );
    guard
        .observe(
            &[
                (parent.clone(), "tree".into()),
                (child.clone(), "tree/child".into()),
            ],
            100,
        )
        .unwrap();
    let step = OneDriveStep::SendDirectoryRemoval {
        item: ItemVersions::of(&parent),
        below: vec![ItemVersions::of(&child)],
    };
    assert!(!guard.admit(&step, 200).unwrap());
    assert_eq!(guard.list(false).len(), 1);
    assert_eq!(guard.list(false)[0].descendants, 1);
    guard.decide(&[], false).unwrap();
    assert!(guard.admit(&step, 100).unwrap());
    guard.sent(&step).unwrap();
    assert_eq!(guard.list(true).len(), 1);
    assert_eq!(guard.list(true)[0].descendants, 1);
    assert!(guard.held().is_empty());
}
#[test]
fn restore_of_cleanup_and_new_uploads_keeps_them_flagged_without_deleting() {
    let dir = tempfile::tempdir().unwrap();
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            removals: 0,
            ..config()
        },
    );
    let reference = ItemFixture::onedrive_reference_of("orphan");
    assert!(!guard.admit_cleanup(&reference, 0, "e", 100).unwrap());
    let batch = guard.decide(&[], true).unwrap();
    guard.restored(&batch[0], true).unwrap();
    assert_eq!(guard.list(false)[0].state, "local-only");
    assert!(!guard.admit_cleanup(&reference, 0, "e", 1000).unwrap());
}

#[test]
fn editing_a_restored_conflict_copy_does_not_silently_admit_its_upload() {
    let mut ledger = Ledger::default();
    let mut c = event(5, Kind::Upload, 1, "first", 100);
    c.disposition = Disposition::LocalOnly;
    ledger.current.insert(c.key(), c);
    ledger.observe(
        event(5, Kind::Upload, 1, "edited", 101),
        &GuardConfig::default(),
        101,
    );
    assert_eq!(
        ledger.current.values().next().unwrap().disposition,
        Disposition::LocalOnly
    );
}

#[test]
fn opening_a_new_writer_invalidates_approval_before_bytes_can_change() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let file = fixture
        .show_onedrive_file("file", b"cloud bytes")
        .local_item_id;
    fixture.land_copy(file, b"cloud bytes");
    let row = fixture.change_file_here(file, b"first rewrite");
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            rewrites: 0,
            ..GuardConfig::default()
        },
    );
    guard.observe(&[(row.clone(), "file".into())], 100).unwrap();
    guard.decide(&[], false).unwrap();
    let first = OneDriveStep::SendContent {
        item: ItemVersions::of(&row),
    };
    assert!(guard.admit(&first, 100).unwrap());
    fixture.write_rows(move |s| {
        s.item_store()
            .apply(&konedrive_tree::LocalVersionChange::OpenedForWriting { item: file })
    });
    let opened = fixture.find_item(file).unwrap();
    assert!(opened.content_before_write.is_some());
    guard.observe(&[(opened, "file".into())], 101).unwrap();
    assert!(!guard.admit(&first, 101).unwrap());
    assert!(
        guard.decide(&[], false).is_err(),
        "a writer cannot approve mutable bytes"
    );
    fixture.write_rows(move |s| {
        s.item_store()
            .apply(&konedrive_tree::LocalVersionChange::ClosedChanged {
                item: file,
                size: 5,
                mtime: 10,
                mtime_nsec: 11,
            })
    });
    guard
        .observe(&[(fixture.find_item(file).unwrap(), "file".into())], 102)
        .unwrap();
    assert_eq!(
        crate::panic::lock(&guard.inner)
            .ledger
            .events
            .iter()
            .filter(|c| c.kind == Kind::Rewrite)
            .map(|c| c.count)
            .sum::<u64>(),
        2
    );
    guard.decide(&[], false).unwrap();
}

#[test]
fn a_sent_generation_is_reported_separately_from_a_newer_held_edit() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let first = fixture.make_own_file("one", b"first");
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            uploads: 0,
            ..Default::default()
        },
    );
    guard
        .observe(&[(first.clone(), "one".into())], 100)
        .unwrap();
    guard.decide(&[], false).unwrap();
    let sent = OneDriveStep::SendNewFile {
        item: ItemVersions::of(&first),
        parent: ItemFixture::onedrive_reference_of(""),
        name: "one".into(),
    };
    assert!(guard.admit(&sent, 100).unwrap());
    let newer = fixture.change_file_here(first.local_item_id, b"newer held work");
    guard.observe(&[(newer, "one".into())], 101).unwrap();
    guard.sent(&sent).unwrap();
    assert_eq!(guard.list(true).len(), 1);
    assert_eq!(guard.list(true)[0].bytes, 5);
    assert_eq!(guard.list(false).len(), 1);
    assert_eq!(guard.list(false)[0].bytes, 15);
}

#[test]
fn a_hold_of_one_kind_blocks_other_outgoing_steps_of_the_same_item() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let file = fixture
        .show_onedrive_file("doc", b"cloud bytes")
        .local_item_id;
    fixture.land_copy(file, b"cloud bytes");
    fixture.change_file_here(file, b"rewrite");
    let moved = fixture.move_here(file, "moved");
    let guard = ChangeGuard::open(
        dir.path().join("guard.json"),
        GuardConfig {
            moves: 0,
            ..Default::default()
        },
    );
    guard
        .observe(&[(moved.clone(), "moved".into())], 100)
        .unwrap();
    assert!(!guard
        .admit(
            &OneDriveStep::SendContent {
                item: ItemVersions::of(&moved)
            },
            200
        )
        .unwrap());
}
#[test]
fn an_outgoing_step_with_an_old_binding_still_gets_new_upload_admission() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let mut row = fixture.show_onedrive_file("doc", b"cloud bytes");
    row.onedrive = None;
    let step = OneDriveStep::SendNewFile {
        item: ItemVersions::of(&row),
        parent: ItemFixture::onedrive_reference_of(""),
        name: "doc".into(),
    };
    let guard = ChangeGuard::open(
        dir.path().join("guard.json"),
        GuardConfig {
            uploads: 0,
            ..Default::default()
        },
    );
    assert!(guard.lacks_step(&step));
    guard
        .observe_step(&step, &[(row.local_item_id, "doc".into())].into(), 100)
        .unwrap();
    assert!(!guard.lacks_step(&step));
    assert!(!guard.admit(&step, 200).unwrap());
    assert_eq!(guard.list(false)[0].kind, "upload");
    guard.decide(&[], false).unwrap();
    assert!(guard.admit(&step, 200).unwrap());
}

#[test]
fn mixed_changes_keep_the_latest_quiet_deadline_for_each_item() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let first = fixture.show_onedrive_file("one", b"cloud").local_item_id;
    fixture.land_copy(first, b"cloud");
    fixture.change_file_here(first, b"rewritten");
    let first = fixture.move_here(first, "one-moved");
    let second = fixture.show_onedrive_file("two", b"cloud").local_item_id;
    let second = fixture.move_here(second, "two-moved");
    let guard = ChangeGuard::open(dir.path().join("guard.json"), GuardConfig::default());
    guard
        .observe(&[(first.clone(), "one-moved".into())], 100)
        .unwrap();
    guard.observe(&[(second, "two-moved".into())], 110).unwrap();
    assert_eq!(guard.change_quiet_until(110)[&first.local_item_id], 112);
}

#[test]
fn approval_does_not_cover_a_new_cloud_content_when_the_etag_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let mut first = fixture.show_onedrive_file("doc", b"cloud");
    first.local = None;
    first.onedrive_etag = None;
    let guard = ChangeGuard::open(
        dir.path().join("guard.json"),
        GuardConfig {
            removals: 0,
            ..Default::default()
        },
    );
    guard
        .observe(&[(first.clone(), "doc".into())], 100)
        .unwrap();
    guard.decide(&[], false).unwrap();
    let mut newer = first.clone();
    newer.onedrive.as_mut().unwrap().content =
        Some(ItemFixture::onedrive_content_of(b"new remote work"));
    guard
        .observe(&[(newer.clone(), "doc".into())], 101)
        .unwrap();
    assert!(!guard
        .admit(
            &OneDriveStep::SendFileRemoval {
                item: ItemVersions::of(&newer)
            },
            200
        )
        .unwrap());
}

#[test]
fn startup_recovery_carries_a_local_only_hold_to_a_new_id_but_never_an_approval() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let original = fixture.make_own_file("kept", b"local bytes");
    let file = dir.path().join("guard.json");
    let config = GuardConfig {
        enabled: false,
        ..Default::default()
    };
    {
        let guard = ChangeGuard::open(file.clone(), config.clone());
        guard.keep_local(&original, "kept".into(), 100).unwrap();
    }
    let guard = ChangeGuard::open(file, config);
    let mut recovered = original.clone();
    recovered.local_item_id = LocalItemId::new(999);
    guard
        .observe(&[(recovered.clone(), "kept".into())], 101)
        .unwrap();
    assert!(guard.blocks(recovered.local_item_id));
    assert!(!guard.blocks(original.local_item_id));
    assert!(!guard
        .admit(
            &OneDriveStep::SendNewFile {
                item: ItemVersions::of(&recovered),
                parent: ItemFixture::onedrive_reference_of(""),
                name: "kept".into()
            },
            200
        )
        .unwrap());
}

#[test]
fn configured_short_and_daily_windows_are_honored() {
    let mut ledger = Ledger::default();
    let config = GuardConfig {
        removals: 100,
        daily_removals: 1,
        window_seconds: 3,
        daily_window_seconds: 5,
        ..Default::default()
    };
    ledger.observe(event(1, Kind::Removal, 0, "one", 100), &config, 100);
    ledger.observe(event(2, Kind::Removal, 0, "two", 106), &config, 106);
    assert!(!ledger.latched.contains(&Kind::Removal));
    ledger.observe(event(3, Kind::Removal, 0, "three", 107), &config, 107);
    assert!(ledger.latched.contains(&Kind::Removal));
    let mut config = GuardConfig::default();
    assert!(config
        .set_thresholds([("window-seconds".into(), 0)].into())
        .is_err());
}

#[test]
fn a_snapshot_never_decides_later_generations_or_later_holds() {
    for restore in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let guard = ChangeGuard::open(
            dir.path().join("ledger.json"),
            GuardConfig {
                uploads: 0,
                ..Default::default()
            },
        );
        let fixture = ItemFixture::new();
        let first = fixture.make_own_file("first", b"one");
        let second = fixture.make_own_file("second", b"two");
        guard
            .observe(&[(first.clone(), "first".into())], 100)
            .unwrap();
        let (token, _) = guard.snapshot().unwrap();
        guard
            .observe(&[(second.clone(), "second".into())], 100)
            .unwrap();
        let (batch, result) = guard.decide_batch(&token, &[], restore).unwrap();
        assert_eq!(result.failed, 0);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].item, first.local_item_id.as_i64());
        assert!(guard.blocks(second.local_item_id));
        // The latch must survive a partial, token-scoped decision.
        assert!(crate::panic::lock(&guard.inner)
            .ledger
            .latched
            .contains(&Kind::Upload));
        if restore {
            guard.restored(&batch[0], false).unwrap();
        }
        let (token, _) = guard.snapshot().unwrap();
        let edited = fixture.change_file_here(second.local_item_id, b"edited after review");
        guard.observe(&[(edited, "second".into())], 101).unwrap();
        let (batch, result) = guard
            .decide_batch(&token, &["upload".into()], restore)
            .unwrap();
        assert!(batch
            .iter()
            .all(|c| c.item != second.local_item_id.as_i64()));
        assert_eq!(result.failed, 1);
        assert!(result.failures[0].2.contains("Review"));
        assert!(guard.blocks(second.local_item_id));
    }
}

#[test]
fn snapshot_tokens_survive_restart_and_reject_other_ledgers() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let fixture = ItemFixture::new();
    let row = fixture.make_own_file("one", b"one");
    let config = GuardConfig {
        uploads: 0,
        ..Default::default()
    };
    let token = {
        let guard = ChangeGuard::open(file.clone(), config.clone());
        guard.observe(&[(row, "one".into())], 100).unwrap();
        guard.snapshot().unwrap().0
    };
    let guard = ChangeGuard::open(file, config.clone());
    let other = ChangeGuard::open(dir.path().join("other.json"), config);
    assert!(other.decide_batch(&token, &[], false).is_err());
    assert_eq!(guard.decide_batch(&token, &[], false).unwrap().0.len(), 1);
    assert_eq!(
        guard.decide_batch(&token, &[], false).unwrap().0.len(),
        0,
        "replaying Allow cannot decide other work"
    );
}

#[test]
fn held_rows_fold_trees_and_preserve_rewrite_and_move_details() {
    let mut removed = event(1, Kind::Removal, 0, "folder", 100);
    removed.path = "2019 Lisbon trip".into();
    removed.directory = true;
    let mut child = event(2, Kind::Removal, 14, "child", 101);
    child.path = "2019 Lisbon trip/photo.jpg".into();
    let mut new_folder = event(3, Kind::Upload, 0, "newfolder", 100);
    new_folder.path = "new".into();
    new_folder.directory = true;
    let mut new_file = event(4, Kind::Upload, 25, "newfile", 100);
    new_file.path = "new/file".into();
    let mut rewrite = event(5, Kind::Rewrite, 48, "rewrite", 101);
    rewrite.after_bytes = 0;
    let mut moved = event(6, Kind::Move, 0, "move", 101);
    moved.path = "Archive/file".into();
    moved.old_path = "Inbox/file".into();
    moved.directory = true;
    moved.descendants = 1204;
    let mut sent = child.clone();
    sent.disposition = Disposition::Sent;
    let described = super::rows::describe(vec![
        &removed,
        &child,
        &new_folder,
        &new_file,
        &rewrite,
        &moved,
        &sent,
    ]);
    assert_eq!(described.len(), 5);
    let deleted = described.iter().find(|r| r.item == 1).unwrap();
    assert_eq!((deleted.descendants, deleted.bytes), (1, 14));
    let new = described.iter().find(|r| r.item == 3).unwrap();
    assert_eq!((new.descendants, new.bytes, new.after_bytes), (1, 25, 25));
    let rewrite = described.iter().find(|r| r.item == 5).unwrap();
    assert_eq!((rewrite.bytes, rewrite.after_bytes), (48, 0));
    let moved = described.iter().find(|r| r.item == 6).unwrap();
    assert_eq!(
        (
            moved.old_path.as_str(),
            moved.path.as_str(),
            moved.descendants
        ),
        ("Inbox/file", "Archive/file", 1204)
    );
    assert_eq!(described.iter().filter(|r| r.sent).count(), 1);
}

#[test]
fn dont_upload_is_durable_and_upload_readmits_instead_of_bypassing_limits() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let fixture = ItemFixture::new();
    let row = fixture.make_own_file("one", b"one");
    let config = GuardConfig {
        uploads: 0,
        ..Default::default()
    };
    {
        let guard = ChangeGuard::open(file.clone(), config.clone());
        guard.observe(&[(row.clone(), "one".into())], 100).unwrap();
        let token = guard.snapshot().unwrap().0;
        let batch = guard.decide_batch(&token, &[], true).unwrap().0;
        guard.restored(&batch[0], true).unwrap();
        assert!(guard.snapshot().unwrap().1.is_empty());
        assert_eq!(guard.local_only().len(), 1);
        assert!(guard.decide_batch(&token, &[], false).unwrap().0.is_empty());
    }
    let guard = ChangeGuard::open(file, config);
    assert!(guard.blocks(row.local_item_id));
    assert_eq!(guard.upload_local("one", 101).unwrap().done, 1);
    assert!(guard.local_only().is_empty());
    assert_eq!(
        guard.snapshot().unwrap().1.len(),
        1,
        "the normal threshold can hold it again"
    );
}

#[test]
fn dont_upload_survives_an_ignored_rename_and_back_even_when_disabled() {
    for enabled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let fixture = ItemFixture::new();
        *crate::panic::write(&fixture.ignore) = crate::local::IgnoreList::new(["*.tmp"]);
        let row = fixture.make_own_file("local", b"never upload");
        let id = row.local_item_id;
        let guard = Arc::new(ChangeGuard::open(
            dir.path().join("ledger.json"),
            GuardConfig { uploads: 0, ..Default::default() },
        ));
        guard.observe(&[(row, "local".into())], 100).unwrap();
        let batch = guard.decide(&["upload".into()], true).unwrap();
        guard.restored(&batch[0], true).unwrap();
        guard.configure(GuardConfig { enabled, ..Default::default() });

        // Both windows have expired. Neither rename may spend another upload budget.
        let now = 100 + GuardConfig::default().daily_window_seconds as i64;
        let counters = crate::panic::lock(&guard.inner).ledger.events.clone();
        for name in ["local.tmp", "local"] {
            let renamed = fixture.move_here(id, name);
            let observing = Arc::clone(&guard);
            fixture.read_rows(move |items| {
                observing.observe_items(items, &[(renamed, name.into())], now)
            });
            assert!(guard.local_only().contains(&id), "{name}, enabled={enabled}");
            assert!(guard.blocks(id));
            let inner = crate::panic::lock(&guard.inner);
            assert_eq!(
                serde_json::to_value(&inner.ledger.events).unwrap(),
                serde_json::to_value(&counters).unwrap(),
                "local-only renames spend no upload budget"
            );
        }
        let step = OneDriveStep::SendNewFile {
            item: fixture.versions_of(id),
            parent: fixture.versions_of(LocalItemId::ROOT).onedrive_item.unwrap(),
            name: "local".into(),
        };
        assert!(!guard.admit(&step, now).unwrap(), "Don't Upload still blocks the sender");
        fixture.remove_here(id);
        guard.retire_upload(id).unwrap();
        assert!(!guard.local_only().contains(&id), "disappearance retires LocalOnly");
    }
}

#[test]
fn a_token_from_before_the_trip_does_not_cover_preexisting_pending_entries() {
    let dir = tempfile::tempdir().unwrap();
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            uploads: 1,
            ..Default::default()
        },
    );
    let fixture = ItemFixture::new();
    let one = fixture.make_own_file("one", b"one");
    let two = fixture.make_own_file("two", b"two");
    guard.observe(&[(one, "one".into())], 100).unwrap();
    let (token, rows) = guard.snapshot().unwrap();
    assert!(rows.is_empty());
    guard.observe(&[(two, "two".into())], 100).unwrap();
    let (batch, result) = guard.decide_batch(&token, &[], false).unwrap();
    assert!(batch.is_empty());
    assert_eq!(result.failed, 0);
    assert_eq!(guard.held().len(), 2);
}

#[test]
fn moved_rows_keep_the_agreed_parent_path_and_count_unchanged_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            moves: 0,
            ..Default::default()
        },
    ));
    let fixture = ItemFixture::new();
    let directory = fixture.show_onedrive_directory("Inbox");
    let child = fixture.show_onedrive_file("Inbox/report", b"cloud");
    fixture.show_onedrive_file("Inbox/unchanged", b"cloud");
    let moved_directory = fixture.move_here(directory.local_item_id, "Archive");
    let moved_child = fixture.move_here(child.local_item_id, "Archive/new-name");
    let observing = Arc::clone(&guard);
    fixture.read_rows(move |items| {
        observing.observe_items(
            items,
            &[
                (moved_directory, "Archive".into()),
                (moved_child, "Archive/new-name".into()),
            ],
            100,
        )
    });
    let (_, rows) = guard.snapshot().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0].old_path.as_str(),
            rows[0].path.as_str(),
            rows[0].descendants
        ),
        ("Inbox", "Archive", 2)
    );
    assert_eq!(
        (rows[1].old_path.as_str(), rows[1].path.as_str()),
        ("Inbox/report", "Archive/new-name")
    );
}

#[test]
fn sent_before_the_hold_excludes_work_allowed_after_the_trip() {
    let dir = tempfile::tempdir().unwrap();
    let guard = ChangeGuard::open(
        dir.path().join("ledger.json"),
        GuardConfig {
            uploads: 1,
            ..Default::default()
        },
    );
    let fixture = ItemFixture::new();
    let one = fixture.make_own_file("one", b"one");
    let two = fixture.make_own_file("two", b"two");
    let three = fixture.make_own_file("three", b"three");
    let step = |row: &Item, name: &str| OneDriveStep::SendNewFile {
        item: ItemVersions::of(row),
        parent: ItemFixture::onedrive_reference_of(""),
        name: name.into(),
    };
    guard.observe(&[(one.clone(), "one".into())], 100).unwrap();
    assert!(guard.admit(&step(&one, "one"), 102).unwrap());
    guard.sent(&step(&one, "one")).unwrap();
    guard.observe(&[(two.clone(), "two".into())], 106).unwrap();
    let token = guard.snapshot().unwrap().0;
    guard.observe(&[(three, "three".into())], 107).unwrap();
    assert_eq!(guard.decide_batch(&token, &[], false).unwrap().0.len(), 1);
    assert!(guard.admit(&step(&two, "two"), 108).unwrap());
    guard.sent(&step(&two, "two")).unwrap();
    let (_, rows) = guard.snapshot().unwrap();
    let sent: Vec<_> = rows.iter().filter(|r| r.sent).collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].path, "one");
    assert_eq!(sent[0].since, 102);
}

#[test]
fn legacy_ledger_rows_gain_display_metadata_without_charging_again() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let fixture = ItemFixture::new();
    let folder = fixture.make_own_directory("new");
    let row = fixture.make_own_file("new/file", b"local bytes");
    let config = GuardConfig {
        uploads: 0,
        ..Default::default()
    };
    let rows = vec![(folder, "new".into()), (row, "new/file".into())];
    {
        let guard = ChangeGuard::open(file.clone(), config.clone());
        guard.observe(&rows, 100).unwrap();
    }
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("epoch");
    legacy.as_object_mut().unwrap().remove("revision");
    for c in legacy["current"].as_object_mut().unwrap().values_mut() {
        for key in [
            "directory",
            "after_bytes",
            "old_path",
            "descendants",
            "revision",
            "first_held_revision",
        ] {
            c.as_object_mut().unwrap().remove(key);
        }
    }
    std::fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let guard = ChangeGuard::open(file, config);
    guard.observe(&rows, 101).unwrap();
    let (_, described) = guard.snapshot().unwrap();
    assert_eq!(described.len(), 1);
    assert!(described[0].directory);
    assert_eq!((described[0].descendants, described[0].bytes), (1, 11));
    assert_eq!(
        crate::panic::lock(&guard.inner)
            .ledger
            .events
            .iter()
            .map(|c| c.count)
            .sum::<u64>(),
        2
    );
}


#[test]
fn published_provisional_tokens_cannot_roll_back_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let config = GuardConfig { uploads: 0, ..Default::default() };
    let fixture = ItemFixture::new();
    let mut row = fixture.make_own_file("one", b"one");
    let guard = ChangeGuard::open(file.clone(), config.clone());
    guard.observe(&[(row.clone(), "one".into())], 100).unwrap();
    row.content_before_write = row.local.as_ref().unwrap().content.clone();
    guard.observe(&[(row.clone(), "one".into())], 101).unwrap();
    let first = guard.snapshot().unwrap().0;
    guard.observe(&[(row, "one".into())], 102).unwrap();
    let token = guard.snapshot().unwrap().0;
    assert_eq!(first, token, "unchanged provisional observations do not advance the sequence");
    drop(guard);
    let guard = ChangeGuard::open(file, config);
    assert_eq!(guard.snapshot().unwrap().0, token, "every published revision was durable");
    let later = fixture.make_own_file("two", b"two");
    guard.observe(&[(later.clone(), "two".into())], 103).unwrap();
    let (batch, _) = guard.decide_batch(&token, &[], false).unwrap();
    assert!(batch.iter().all(|c| c.item != later.local_item_id.as_i64()));
    assert!(guard.blocks(later.local_item_id));
}

#[test]
fn reopening_pending_work_stays_below_limits_even_when_disabled() {
    for enabled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let guard = ChangeGuard::open(dir.path().join("ledger.json"), GuardConfig { enabled, ..Default::default() });
        let fixture = ItemFixture::new();
        let mut row = fixture.make_own_file("one", b"one");
        guard.observe(&[(row.clone(), "one".into())], 100).unwrap();
        row.content_before_write = row.local.as_ref().unwrap().content.clone();
        guard.observe(&[(row.clone(), "one".into())], 101).unwrap();
        row.content_before_write = None;
        guard.observe(&[(row.clone(), "one".into())], 102).unwrap();
        assert!(!guard.blocks(row.local_item_id));
        assert!(guard.admit(&OneDriveStep::SendNewFile { item: ItemVersions::of(&row), parent: fixture.versions_of(LocalItemId::ROOT).onedrive_item.unwrap(), name: "one".into() }, 200).unwrap());
    }
}

#[test]
fn cleanup_shares_the_removal_deadline_and_disabled_policy() {
    let dir = tempfile::tempdir().unwrap();
    let guard = ChangeGuard::open(dir.path().join("ledger.json"), Default::default());
    let a = ItemFixture::onedrive_reference_of("a");
    let b = ItemFixture::onedrive_reference_of("b");
    assert!(!guard.admit_cleanup(&a, 1, "e-a", 100).unwrap());
    assert!(!guard.admit_cleanup(&b, 1, "e-b", 129).unwrap());
    assert!(!guard.admit_cleanup(&a, 1, "e-a", 130).unwrap());
    assert!(guard.admit_cleanup(&a, 1, "e-a", 159).unwrap());
    guard.configure(GuardConfig { enabled: false, ..Default::default() });
    assert!(guard.admit_cleanup(&ItemFixture::onedrive_reference_of("c"), 1, "e-c", 160).unwrap());
}

#[test]
fn settled_cleanups_do_not_reappear_in_a_later_hold() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let guard = ChangeGuard::open(file.clone(), Default::default());
    for (name, sent) in [("sent", true), ("absent", true), ("foreign", false)] {
        let reference = ItemFixture::onedrive_reference_of(name);
        guard.admit_cleanup(&reference, 1, "e", 100).unwrap();
        guard.settle_cleanup(&reference, sent).unwrap();
    }
    drop(guard);
    let guard = ChangeGuard::open(file, GuardConfig { removals: 0, ..Default::default() });
    guard.admit_cleanup(&ItemFixture::onedrive_reference_of("later"), 1, "e", 200).unwrap();
    let held = guard.list(false);
    assert_eq!(held.len(), 1);
    assert!(held[0].path.ends_with("later"));
    assert_eq!(guard.list(true).len(), 2);
}

#[test]
fn observing_a_hundred_thousand_held_entries_is_not_quadratic() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ItemFixture::new();
    let base = fixture.make_own_file("base", b"a");
    let rows: Vec<_> = (1..=100_000).map(|id| {
        let mut row = base.clone();
        row.local_item_id = LocalItemId::new(id);
        let path = format!("file-{id}");
        row.local.as_mut().unwrap().place.as_mut().unwrap().name = path.clone().into();
        (row, path)
    }).collect();
    let guard = ChangeGuard::open(dir.path().join("ledger.json"), GuardConfig { uploads: 0, ..Default::default() });
    let start = std::time::Instant::now();
    guard.observe(&rows, 100).unwrap();
    let initial = start.elapsed();
    let start = std::time::Instant::now();
    guard.observe(&rows, 100).unwrap();
    let repeated = start.elapsed();
    eprintln!("100000 held entries: initial {initial:?}, unchanged plan {repeated:?}");
    assert!(initial.as_secs() < 20 && repeated.as_secs() < 20, "observation must use item/kind lookups, not repeated ledger scans: {initial:?}, {repeated:?}");
    guard.configure(GuardConfig { enabled: false, uploads: 0, ..Default::default() });
    let mut later = base;
    later.local_item_id = LocalItemId::new(100_001);
    guard.observe(&[(later, "later".into())], 101).unwrap();
    assert!(!guard.blocks(LocalItemId::new(100_001)));
    guard.configure(GuardConfig { uploads: 0, ..Default::default() });
    assert!(guard.blocks(LocalItemId::new(100_001)), "reenabling joins pending work to its existing latch once");
}

#[test]
fn committed_restore_receipts_survive_restart_without_approving_newer_work() {
    for newer in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ledger.json");
        let fixture = ItemFixture::new();
        let row = fixture.make_own_file("one", b"one");
        let id = row.local_item_id;
        let config = GuardConfig { uploads: 0, ..Default::default() };
        let guard = ChangeGuard::open(file.clone(), config.clone());
        guard.observe(&[(row.clone(), "one".into())], 100).unwrap();
        let c = guard.decide(&[], true).unwrap().remove(0);
        guard.record_restore(&[c], &[row]).unwrap();
        if newer { fixture.change_file_here(id, b"two"); }
        drop(guard);
        let guard = Arc::new(ChangeGuard::open(file, config));
        let recovering = Arc::clone(&guard);
        fixture.store.call_blocking(move |s| recovering.reconcile_restores(&s.item_store().reader())).unwrap();
        assert_eq!(guard.blocks(id), newer, "only exact committed output settles the old hold");
    }
}

#[test]
fn local_only_edits_update_generations_without_charging_until_upload_local() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.json");
    let fixture = ItemFixture::new();
    let local = fixture.make_own_file("local", b"one");
    let config = GuardConfig { uploads: 1, upload_bytes: 4, ..Default::default() };
    let guard = ChangeGuard::open(file.clone(), config.clone());
    guard.keep_local(&local, "local".into(), 100).unwrap();
    for bytes in [b"much larger local edit".as_slice(), b"another local edit".as_slice()] {
        let edited = fixture.change_file_here(local.local_item_id, bytes);
        guard.observe(&[(edited.clone(), "local".into())], 101).unwrap();
        let inner = crate::panic::lock(&guard.inner);
        let current = inner.ledger.current.values().next().unwrap();
        assert_eq!(current.generation, generation(&ItemVersions::of(&edited)));
        assert_eq!(current.disposition, Disposition::LocalOnly);
        assert!(inner.ledger.events.is_empty(), "local-only edits spend no outgoing budget");
        assert!(!inner.ledger.latched.contains(&Kind::Upload));
    }
    drop(guard);
    let guard = ChangeGuard::open(file, config);
    let normal = fixture.make_own_file("normal", b"ok");
    guard.observe(&[(normal.clone(), "normal".into())], 102).unwrap();
    assert!(!guard.blocks(normal.local_item_id));
    assert_eq!(guard.upload_local("local", 103).unwrap().done, 1);
    assert!(guard.local_only().is_empty());
    assert!(guard.blocks(local.local_item_id), "UploadLocal re-enters normal accounting");
    let inner = crate::panic::lock(&guard.inner);
    assert_eq!(inner.ledger.events.iter().map(|c| c.count).sum::<u64>(), 2);
    assert_eq!(inner.ledger.events.iter().map(|c| c.bytes).sum::<u64>(), 20);
}
