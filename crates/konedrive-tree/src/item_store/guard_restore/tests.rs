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
            .unwrap(),
        StepCommitOutcome::RowsChanged
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
            .unwrap(),
        StepCommitOutcome::RowsChanged
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
        .unwrap()
        .is_none());
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
