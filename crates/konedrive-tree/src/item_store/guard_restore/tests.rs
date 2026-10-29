use super::*;
use crate::item_store::tests::listed_drive::{
    listed_file, reference, store_with_listed_root, write_listing_of_changes,
};
use crate::{ItemVersions, LocalVersionChange};

fn file(store: &mut crate::TreeStore) -> LocalItemId {
    write_listing_of_changes(store, &[listed_file("A", "R", "a", (4, "hash"), "etag")]);
    let row = store
        .item_store()
        .find_bound_item(&reference("A"))
        .unwrap()
        .unwrap();
    store
        .item_store()
        .commit_show_item(&ItemVersions::of(&row), false)
        .unwrap();
    row.local_item_id
}
#[test]
fn restoring_a_removed_generation_refuses_newer_remote_work() {
    let mut store = store_with_listed_root();
    let id = file(&mut store);
    store
        .item_store()
        .apply(&LocalVersionChange::Removed { item: id })
        .unwrap();
    let row = store.item_store().find_item(id).unwrap().unwrap();
    let planned = ItemVersions::of(&row);
    let outdated = ItemVersions {
        onedrive_etag: Some("old".into()),
        ..planned.clone()
    };
    assert_eq!(
        store
            .item_store()
            .commit_guard_restore_removal(&outdated, None, None)
            .unwrap_err().to_string(),
        GuardRestoreFailure::RemoteChanged.to_string()
    );
    // A newer local replacement is never overwritten.
    store
        .item_store()
        .apply(&LocalVersionChange::Made {
            kind: ItemKind::File,
            place: ItemPlace {
                parent_local_item_id: LocalItemId::ROOT,
                name: "a".into(),
            },
            content: Some(ContentVersion::ChangedHere {
                size: 1,
                mtime: 1,
                mtime_nsec: 0,
                change_count: 1,
            }),
            is_name_ignored: false,
            replaced: None,
        })
        .unwrap();
    assert_eq!(
        store
            .item_store()
            .commit_guard_restore_removal(&planned, None, None)
            .unwrap_err().to_string(),
        GuardRestoreFailure::Occupied.to_string()
    );
    assert!(store
        .item_store()
        .find_item(id)
        .unwrap()
        .unwrap()
        .local
        .is_none());
}
#[test]
fn restoring_a_rewrite_keeps_a_local_conflict_copy() {
    let mut store = store_with_listed_root();
    let id = file(&mut store);
    store
        .item_store()
        .apply(&LocalVersionChange::OpenedForWriting { item: id })
        .unwrap();
    store
        .item_store()
        .apply(&LocalVersionChange::ClosedChanged {
            item: id,
            size: 8,
            mtime: 2,
            mtime_nsec: 3,
        })
        .unwrap();
    let row = store.item_store().find_item(id).unwrap().unwrap();
    let local = row.local.as_ref().unwrap().content.clone();
    let planned = ItemVersions::of(&row);
    let copy = ItemPlace {
        parent_local_item_id: LocalItemId::ROOT,
        name: "conflict".into(),
    };
    let conflict = ConflictRow {
        at: 10,
        original: "/a".into(),
        kept_copy: "/conflict".into(),
    };
    let made = store
        .item_store()
        .commit_guard_restore_rewrite(&planned, &copy, &conflict, None)
        .unwrap()
        .unwrap();
    let original = store.item_store().find_item(id).unwrap().unwrap();
    let kept = store.item_store().find_item(made).unwrap().unwrap();
    assert_eq!(
        original.local.as_ref().unwrap().content,
        original.synced_content
    );
    assert_eq!(original.content_presence, ContentPresence::NotOnDisk);
    assert_eq!(kept.local.as_ref().unwrap().content, local);
    assert!(kept.onedrive_item.is_none());
    assert_eq!(store.conflict_count().unwrap(), 1);
    assert!(store
        .item_store()
        .commit_guard_restore_rewrite(&planned, &copy, &conflict, None)
        .is_err());
}

#[test]
fn restoring_a_rewrite_and_its_move_is_one_conditional_transaction() {
    let mut store = store_with_listed_root();
    let id = file(&mut store);
    store
        .item_store()
        .apply(&LocalVersionChange::Moved {
            item: id,
            place: ItemPlace {
                parent_local_item_id: LocalItemId::ROOT,
                name: "moved".into(),
            },
            is_name_ignored: false,
        })
        .unwrap();
    store
        .item_store()
        .apply(&LocalVersionChange::OpenedForWriting { item: id })
        .unwrap();
    store
        .item_store()
        .apply(&LocalVersionChange::ClosedChanged {
            item: id,
            size: 8,
            mtime: 2,
            mtime_nsec: 3,
        })
        .unwrap();
    let planned = ItemVersions::of(&store.item_store().find_item(id).unwrap().unwrap());
    let copy = ItemPlace {
        parent_local_item_id: LocalItemId::ROOT,
        name: "kept".into(),
    };
    let conflict = ConflictRow {
        at: 10,
        original: "/moved".into(),
        kept_copy: "/kept".into(),
    };
    assert!(store
        .item_store()
        .commit_guard_restore_rewrite_and_move(&planned, &copy, &conflict, None)
        .unwrap()
        .is_some());
    let row = store.item_store().find_item(id).unwrap().unwrap();
    assert_eq!(row.local.as_ref().unwrap().place, row.synced_place);
    assert_eq!(row.local.as_ref().unwrap().content, row.synced_content);
    assert!(row.unsent_since.is_none());
}

#[test]
fn a_restore_copy_is_not_committed_when_its_durable_hold_cannot_be_written() {
    let mut store = store_with_listed_root();
    let id = file(&mut store);
    store
        .item_store()
        .apply(&LocalVersionChange::OpenedForWriting { item: id })
        .unwrap();
    store
        .item_store()
        .apply(&LocalVersionChange::ClosedChanged {
            item: id,
            size: 8,
            mtime: 2,
            mtime_nsec: 3,
        })
        .unwrap();
    let before = store.item_store().list_items().unwrap();
    let planned = ItemVersions::of(&store.item_store().find_item(id).unwrap().unwrap());
    let copy = ItemPlace {
        parent_local_item_id: LocalItemId::ROOT,
        name: "kept".into(),
    };
    let conflict = ConflictRow {
        at: 10,
        original: "/a".into(),
        kept_copy: "/kept".into(),
    };
    let refused = store.item_store().commit_guard_restore_rewrite_noticed(
        &planned,
        &copy,
        &conflict,
        None,
        false,
        |_, _| Err(TreeError::Io(std::io::Error::other("guard fsync failed"))),
    );
    assert!(refused.is_err());
    assert_eq!(store.item_store().list_items().unwrap(), before);
    assert_eq!(store.conflict_count().unwrap(), 0);
}

#[test]
fn restore_transactions_distinguish_local_changes_missing_rows_and_missing_parents() {
    let mut store = store_with_listed_root();
    let id = file(&mut store);
    let planned = ItemVersions::of(&store.item_store().find_item(id).unwrap().unwrap());
    store.item_store().apply(&LocalVersionChange::Removed { item: id }).unwrap();
    assert!(matches!(store.item_store().commit_guard_restore_move_noticed(&planned, None, |_| Ok(())), Err(TreeError::GuardRestore(GuardRestoreFailure::LocalChanged))));
    let planned = ItemVersions::of(&store.item_store().find_item(id).unwrap().unwrap());
    let missing = ItemVersions { local_item_id: LocalItemId::new(9999), ..planned.clone() };
    assert!(matches!(store.item_store().commit_guard_restore_removal(&missing, None, None), Err(TreeError::GuardRestore(GuardRestoreFailure::LocalMissing))));
    // The planned row is intact, but the directory it belongs in has disappeared.
    let place = ItemPlace { parent_local_item_id: LocalItemId::new(9999), name: "copy".into() };
    let tx = store.conn.transaction().unwrap();
    assert!(matches!(restore_place(&tx, id, &place), Err(TreeError::GuardRestore(GuardRestoreFailure::ParentMissing))));
}

#[test]
fn restore_failure_fragments_are_documented_and_ready_for_a_rows_second_line() {
    let xml = include_str!("../../../../../dbus/org.konedrive.ChangeGuard.xml");
    let design = include_str!("../../../../../docs/design/change-guard.md");
    for reason in [GuardRestoreFailure::Open, GuardRestoreFailure::LocalChanged, GuardRestoreFailure::RemoteChanged, GuardRestoreFailure::Changed, GuardRestoreFailure::Occupied, GuardRestoreFailure::ParentMissing, GuardRestoreFailure::LocalMissing, GuardRestoreFailure::NoCopyName, GuardRestoreFailure::Unmounted, GuardRestoreFailure::Storage, GuardRestoreFailure::Stopped] {
        let text = reason.to_string();
        assert!(text.starts_with(char::is_lowercase));
        assert!(!text.ends_with('.') && !text.contains('\n'));
        assert!(xml.contains(&text), "XML is missing {text}");
        assert!(design.contains(&text), "design is missing {text}");
    }
}

#[test]
fn a_restore_says_why_when_its_parent_moved_inside_it_or_its_bytes_are_not_the_baseline() {
    use crate::item_store::tests::listed_drive::listed_directory;
    let mut store = store_with_listed_root();
    write_listing_of_changes(&mut store, &[listed_directory("P", "R", "p", "e1"), listed_directory("D", "P", "d", "e2")]);
    let show = |store: &mut crate::TreeStore, id: &str| {
        let row = store.item_store().find_bound_item(&reference(id)).unwrap().unwrap();
        store.item_store().commit_show_item(&ItemVersions::of(&row), false).unwrap();
        row.local_item_id
    };
    let (parent, directory) = (show(&mut store, "P"), show(&mut store, "D"));
    let moved = |item, parent_local_item_id, name: &str| LocalVersionChange::Moved { item, place: ItemPlace { parent_local_item_id, name: name.into() }, is_name_ignored: false };
    // The held move took `d` out of `p`; then `p` was moved into `d`.
    store.item_store().apply(&moved(directory, LocalItemId::ROOT, "d")).unwrap();
    store.item_store().apply(&moved(parent, directory, "p")).unwrap();
    let planned = ItemVersions::of(&store.item_store().find_item(directory).unwrap().unwrap());
    assert_eq!(
        store.item_store().commit_guard_restore_move_noticed(&planned, None, |_| Ok(())).unwrap_err().to_string(),
        GuardRestoreFailure::ParentMissing.to_string()
    );

    let id = file(&mut store);
    store.item_store().apply(&LocalVersionChange::Removed { item: id }).unwrap();
    let planned = ItemVersions::of(&store.item_store().find_item(id).unwrap().unwrap());
    let other = ContentVersion::Hashed { size: 4, mtime: 1, mtime_nsec: 0, ctag: None, hash: Some("other".into()) };
    assert_eq!(
        store.item_store().commit_guard_restore_removal(&planned, Some(other), None).unwrap_err().to_string(),
        GuardRestoreFailure::RemoteChanged.to_string()
    );
    assert!(store.item_store().find_item(id).unwrap().unwrap().local.is_none(), "nothing was written");
}
