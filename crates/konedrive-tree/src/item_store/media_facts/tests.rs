use super::*;
use crate::item_store::tests::listed_drive::{listed_file, listed_gone, listed_root, reference, store_with_listed_root, write_listing_of_changes};
use crate::item_store::tests::shown_items::show_listed_file;
use crate::{Change, ContentVersion, LocalVersionChange, MediaFacts, MediaKind, ListingLink};

fn media_change(make: &str) -> Change {
    let Change::Upsert(mut row) = listed_file("F", "R", "f", (10, "h1"), "e1") else { panic!() };
    let mut facts = MediaFacts::empty(MediaKind::Photo);
    facts.camera_make = Some(make.into());
    row.media = Some(facts);
    Change::Upsert(row)
}

fn facts(store: &mut crate::TreeStore, id: LocalItemId) -> Option<String> {
    store.item_store().reader().find_media_facts(id).unwrap()
}

#[test]
fn media_facts_replace_clear_delete_and_reject_stale_content() {
    let mut store = store_with_listed_root();
    write_listing_of_changes(&mut store, &[media_change("Camera A")]);
    let file = store.item_store().find_bound_item(&reference("F")).unwrap().unwrap();
    let id = file.local_item_id;
    assert_eq!(facts(&mut store, id).as_deref(), Some(r#"{"v":1,"kind":"photo","cameraMake":"Camera A"}"#));
    write_listing_of_changes(&mut store, &[media_change("Camera B")]);
    assert!(facts(&mut store, id).unwrap().contains("Camera B"));
    // A step can change the OneDrive content before the next listing supplies facets.
    store.conn.execute("UPDATE items SET onedrive_ctag = 'new-content' WHERE local_item_id = ?1", [id.as_i64()]).unwrap();
    assert_eq!(facts(&mut store, id), None);
    write_listing_of_changes(&mut store, &[media_change("Camera C")]);
    store.conn.execute("UPDATE items SET onedrive_etag = 'new-etag' WHERE local_item_id = ?1", [id.as_i64()]).unwrap();
    assert_eq!(facts(&mut store, id), None);
    write_listing_of_changes(&mut store, &[media_change("Camera D")]);
    write_listing_of_changes(&mut store, &[listed_file("F", "R", "f", (10, "h1"), "e1")]);
    assert_eq!(facts(&mut store, id), None, "missing facets clear even an unchanged version");
    write_listing_of_changes(&mut store, &[media_change("Camera E")]);
    write_listing_of_changes(&mut store, &[listed_gone("F")]);
    assert_eq!(facts(&mut store, id), None);
    assert_eq!(store.conn.query_row("SELECT count(*) FROM media_facts", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    write_listing_of_changes(&mut store, &[media_change("Camera F")]);
    let id = store.item_store().find_bound_item(&reference("F")).unwrap().unwrap().local_item_id;
    store.conn.execute("DELETE FROM items WHERE local_item_id = ?1", [id.as_i64()]).unwrap();
    assert_eq!(store.conn.query_row("SELECT count(*) FROM media_facts", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "delete trigger");
}

#[test]
fn unchanged_full_listing_backfills_media_without_changing_rows_or_pins() {
    let mut store = store_with_listed_root();
    let cloud = show_listed_file(&mut store, "F", "R", "f", "h1");
    let kept = show_listed_file(&mut store, "K", "R", "kept", "hk");
    store.set_pin("K", true).unwrap();
    // Keep one real on-disk version and one pending local edit unchanged by backfill.
    store.item_store().set_content_presence(kept, crate::ContentPresence::OnDisk).unwrap();
    store.item_store().apply(&LocalVersionChange::ClosedChanged { item: kept, size: 12, mtime: 2000, mtime_nsec: 0 }).unwrap();
    let pending = store.item_store().find_item(kept).unwrap().unwrap();
    let session = crate::UploadSession {
        address: crate::UploadSessionAddress::new("https://upload.invalid/synthetic-session"),
        expires_at: Some(5000), next_offset: 4,
        content: pending.local.as_ref().unwrap().content.clone().unwrap(), empty_file_place: None,
    };
    assert_eq!(store.step_memory().remember_upload_session(kept, pending.unsent_since, &session).unwrap(), crate::UploadSessionStanding::Continued);
    let memory_before = store.step_memory().find_item_memory(kept).unwrap();
    let before = store.item_store().list_items().unwrap();
    let Change::Upsert(mut media) = media_change("Backfill Camera") else { panic!() };
    // Use exactly the existing listed content, not the helper's arbitrary size/eTag.
    let old = store.item_store().find_item(cloud).unwrap().unwrap();
    let ContentVersion::Hashed { size, mtime, ctag, hash, .. } = old.onedrive.as_ref().unwrap().content.as_ref().unwrap() else { panic!() };
    media.size = *size; media.mtime = *mtime; media.ctag = ctag.clone(); media.quickxor = hash.clone(); media.etag = old.onedrive_etag.clone();
    let kept_listing = listed_file("K", "R", "kept", (3, "hk"), "e-K");
    let full = store.item_store().begin_full_listing().unwrap();
    store.item_store().write_listed_changes(full.id, &[listed_root(), Change::Upsert(media), kept_listing], full.began_at_count, &ListingLink::DeltaLink("backfilled".into())).unwrap();
    assert_eq!(store.item_store().list_items().unwrap(), before, "no removal, download request, or local edit");
    assert_eq!(store.pins().unwrap(), ["K"]);
    assert_eq!(store.step_memory().find_item_memory(kept).unwrap(), memory_before, "pending upload progress and session preserved");
    assert!(facts(&mut store, cloud).unwrap().contains("Backfill Camera"));
}

#[test]
fn media_facts_are_deleted_when_the_binding_is_replaced() {
    let mut store = store_with_listed_root();
    write_listing_of_changes(&mut store, &[media_change("Original Camera")]);
    let id = store.item_store().find_bound_item(&reference("F")).unwrap().unwrap().local_item_id;
    assert!(facts(&mut store, id).is_some());
    store.conn.execute("UPDATE items SET onedrive_item_id = 'replacement' WHERE local_item_id = ?1", [id.as_i64()]).unwrap();
    assert_eq!(facts(&mut store, id), None);
    assert_eq!(store.conn.query_row("SELECT count(*) FROM media_facts", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "binding trigger removes facts, rather than merely hiding them");
}

#[test]
fn media_facts_follow_the_conflict_binding_to_its_cloud_only_successor() {
    let mut store = store_with_listed_root();
    write_listing_of_changes(&mut store, &[media_change("Conflict Camera")]);
    let id = crate::item_store::tests::shown_items::show(&mut store, "F");
    let expected = facts(&mut store, id).unwrap();
    store.item_store().set_content_presence(id, crate::ContentPresence::OnDisk).unwrap();
    store.item_store().apply(&LocalVersionChange::ClosedChanged { item: id, size: 12, mtime: 2000, mtime_nsec: 0 }).unwrap();
    let successor = store.item_store().give_up_binding(id, false).unwrap();
    assert_eq!(facts(&mut store, id), None, "the local conflict copy has no cloud facts");
    assert_eq!(facts(&mut store, successor), Some(expected.clone()), "the unchanged remote content keeps its facts without another delta");
    store.item_store().apply(&LocalVersionChange::Moved { item: id, place: crate::item_store::tests::place(LocalItemId::ROOT, "f-conflict"), is_name_ignored: false }).unwrap();
    crate::item_store::tests::shown_items::show(&mut store, "F");
    assert_eq!(facts(&mut store, successor), Some(expected));
    assert_eq!(store.item_store().find_item(successor).unwrap().unwrap().content_presence, crate::ContentPresence::NotOnDisk);
}

#[test]
fn media_facts_follow_merge_and_take_over_without_refreshing_stale_fences() {
    use crate::item_store::tests::make_file;
    for take_over in [false, true] {
        for stale_column in [None, Some("onedrive_etag"), Some("onedrive_ctag"), Some("onedrive_mtime")] {
            let mut store = store_with_listed_root();
            write_listing_of_changes(&mut store, &[media_change("Handoff Camera")]);
            let from = store.item_store().find_bound_item(&reference("F")).unwrap().unwrap().local_item_id;
            let expected = facts(&mut store, from).unwrap();
            if take_over {
                crate::item_store::tests::shown_items::show(&mut store, "F");
                store.item_store().apply(&LocalVersionChange::Removed { item: from }).unwrap();
            }
            if let Some(column) = stale_column {
                store.conn.execute(&format!("UPDATE items SET {column} = '9999' WHERE local_item_id = ?1"), [from.as_i64()]).unwrap();
                assert_eq!(facts(&mut store, from), None);
            }
            let to = make_file(&mut store, LocalItemId::ROOT, "new-local");
            if take_over {
                store.item_store().write(|tx| crate::item_store::bindings::take_over_binding(tx, to, from)).unwrap();
            } else {
                store.item_store().merge_items(to, from).unwrap();
            }
            assert_eq!(store.item_store().find_item(from).unwrap(), None);
            assert_eq!(facts(&mut store, to), stale_column.is_none().then_some(expected), "take_over={take_over}, stale={stale_column:?}");
        }
    }
}

#[test]
fn media_facts_follow_both_bindings_in_an_atomic_save() {
    let mut store = store_with_listed_root();
    let Change::Upsert(mut second) = media_change("Temporary Camera") else { panic!() };
    second.id = "T".into(); second.name = "temporary".into();
    write_listing_of_changes(&mut store, &[media_change("Original Camera"), Change::Upsert(second)]);
    let original = crate::item_store::tests::shown_items::show(&mut store, "F");
    let temp = crate::item_store::tests::shown_items::show(&mut store, "T");
    store.item_store().set_content_presence(temp, crate::ContentPresence::OnDisk).unwrap();
    let expected = [facts(&mut store, original), facts(&mut store, temp)];
    store.item_store().apply(&LocalVersionChange::SavedOver { replacing: temp, replaced: original }).unwrap();
    assert_eq!([facts(&mut store, temp), facts(&mut store, original)], expected);
}

#[test]
fn media_facts_binding_transfer_rolls_back_with_its_rows() {
    let mut store = store_with_listed_root();
    write_listing_of_changes(&mut store, &[media_change("Rollback Camera")]);
    let from = crate::item_store::tests::shown_items::show(&mut store, "F");
    let before = store.item_store().list_items().unwrap();
    let expected = facts(&mut store, from);
    store.conn.execute_batch("CREATE TRIGGER fail_transfer BEFORE INSERT ON media_facts BEGIN SELECT RAISE(ABORT, 'failed facts transfer'); END").unwrap();
    assert!(store.item_store().give_up_binding(from, false).is_err());
    assert_eq!(store.item_store().list_items().unwrap(), before);
    assert_eq!(facts(&mut store, from), expected);
}
