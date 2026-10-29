//! Namespace safety against remote changes and saves crossing requests.

use super::*;
use konedrive_tree::LocalVersionChange;

/// OneDrive personal moves no folder's eTag at a change below it (measured 2026-10-29):
/// what is below is checked by the rows, after the attempt's own listing. A creation, an
/// edit or a removal below the directory that the listing brings holds the removal back,
/// nothing is sent, and the next attempt waits for another listing.
#[tokio::test]
async fn a_change_below_the_directory_that_the_attempts_listing_brings_holds_the_removal_back() {
    for change in 0..3 {
        let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
        let planned = step.item().onedrive_etag.clone().expect("the listing's folder eTag");
        crate::panic::lock(&folder.steps.directory_removal_listings).clear();
        assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
        folder.graph.with(|cloud| match change {
            0 => cloud.add_file("UNSEEN", "docs", "unseen.txt", b"new"),
            1 => cloud.edit("docs!c.txt", b"changed"),
            _ => cloud.trash("docs!c.txt"),
        });
        assert_eq!(folder.graph.with(|cloud| cloud.item("docs").map(|docs| docs.etag.clone())), Some(planned), "the folder's eTag did not move");
        folder.list_cloud();
        assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::Nothing), "change {change}");
        assert_eq!(folder.run(step).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted), "the wait was used up");
        assert_eq!(folder.count("DELETE", ""), 0);
        assert!(folder.graph.with(|cloud| cloud.item("docs").is_some()));
    }
}

/// Each attempt waits for a listing begun after its first look, which it asks for once;
/// one under way at that look began before it and is not the one.
#[tokio::test]
async fn a_directory_removal_waits_for_a_later_completed_listing() {
    let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
    crate::panic::lock(&folder.steps.directory_removal_listings).clear();
    let under_way = folder.fixture.write_rows(|store| store.item_store().begin_delta_listing());
    assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
    assert_eq!(folder.run(step.clone()).await, StepOutcome::NotRun(StepObstacle::ListingAwaited), "the cycle is asked for once");
    folder.fixture.write_rows(move |store| {
        let link = store.delta_link()?.expect("the store's delta link");
        store.item_store().write_listed_changes(under_way.id, &[], under_way.began_at_count, &konedrive_tree::ListingLink::DeltaLink(link))
    });
    folder.write_listing_end(crate::clock::unix_now());
    assert_eq!(folder.run(step.clone()).await, StepOutcome::NotRun(StepObstacle::ListingAwaited), "the listing under way at the first look ended");
    assert_eq!(folder.count("DELETE", ""), 0);
    folder.list_cloud();
    assert!(matches!(folder.run(step).await, StepOutcome::Written { .. }));
    assert_eq!(folder.count("DELETE", ""), 1);
}

/// A hold between the end of the attempt's listing and its look — another removal made
/// here restarts the change guard's quiet time — keeps the DELETE from going out on rows
/// the hold made old: once the hold ends, the attempt waits for a listing of its own again.
#[tokio::test]
async fn a_hold_after_the_attempts_listing_ended_makes_the_removal_wait_for_another_listing() {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(crate::change_guard::ChangeGuard::open(dir.path().join("guard.json"), Default::default()));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let directory = fixture.show_onedrive_directory("Docs");
    let file = fixture.show_onedrive_file("Docs/c.txt", b"c");
    let other = fixture.show_onedrive_file("other.txt", b"other");
    let now = (fixture.clock())();
    let removed = fixture.remove_here(file.local_item_id).unwrap();
    let removed_directory = fixture.remove_here(directory.local_item_id).unwrap();
    guard.observe(&[(removed, "Docs/c.txt".into()), (removed_directory, "Docs".into())], now).unwrap();
    let folder = StepsFolder::start(fixture).await;
    let step = || OneDriveStep::SendDirectoryRemoval { item: folder.fixture.versions_of(directory.local_item_id), below: vec![folder.fixture.versions_of(file.local_item_id)] };
    folder.fixture.now.fetch_add(31, Ordering::SeqCst);
    assert_eq!(folder.run(step()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted), "the first look");
    folder.list_cloud();
    let removed_other = folder.fixture.remove_here(other.local_item_id).unwrap();
    guard.observe(&[(removed_other, "other.txt".into())], (folder.fixture.clock())()).unwrap();
    assert_eq!(folder.run(step()).await, StepOutcome::NotRun(StepObstacle::SendingStopped), "the quiet time of the other removal");
    folder.fixture.now.fetch_add(31, Ordering::SeqCst);
    assert_eq!(folder.run(step()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted), "the hold forgot the wait");
    assert_eq!(folder.count("DELETE", ""), 0);
    folder.list_cloud();
    assert!(matches!(folder.run(step()).await, StepOutcome::Written { .. }));
    assert_eq!(folder.count("DELETE", ""), 1);
}

/// A step that did not run at all while its listing ended long ago — a pause, a wait
/// OneDrive asked for, a sign-out — does not use the wait up with it: it waits for
/// another listing.
#[tokio::test]
async fn a_listing_that_ended_long_before_the_look_does_not_use_the_wait_up() {
    let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
    folder.write_listing_end(crate::clock::unix_now() - DIRECTORY_REMOVAL_LISTING_FRESH_SECONDS - 1);
    assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
    assert_eq!(folder.run(step.clone()).await, StepOutcome::NotRun(StepObstacle::ListingAwaited), "a new wait");
    assert_eq!(folder.count("DELETE", ""), 0);
    folder.list_cloud();
    assert!(matches!(folder.run(step).await, StepOutcome::Written { .. }));
    assert_eq!(folder.count("DELETE", ""), 1);
}

/// A directory with no eTag waits for a listing that describes it, with a reason of its
/// own, and is never answered that it changed in OneDrive.
#[tokio::test]
async fn a_directory_without_an_etag_waits_for_a_listing_and_never_sends_a_delete() {
    let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
    let OneDriveStep::SendDirectoryRemoval { mut item, below } = step else { unreachable!() };
    item.onedrive_etag = None;
    let outcome = folder.run(OneDriveStep::SendDirectoryRemoval { item, below }).await;
    assert_eq!(outcome, StepOutcome::Failed(StepFailure::FolderNotDescribed));
    assert_eq!(folder.count("DELETE", ""), 0);
}

/// A folder renamed or moved in OneDrive has another eTag: the DELETE sent with the one
/// planned from is refused 412, `ItemChanged`. It is never sent again without a guard or
/// with the eTag that failed: the next attempt waits for a listing of its own, which
/// writes the new eTag, and the next plan decides.
#[tokio::test]
async fn a_folder_renamed_or_moved_in_onedrive_refuses_the_removal_and_it_waits_to_be_planned_again() {
    for is_move in [false, true] {
        let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
        let planned = step.item().onedrive_etag.clone().expect("the listing's folder eTag");
        folder.graph.with(|cloud| {
            if is_move {
                cloud.add(crate::fake_onedrive::FakeItem { id: "OTHER".into(), parent: Some(crate::fake_onedrive::ROOT.into()), name: "other".into(), folder: true, content: Vec::new(), hash: None, size: 0, etag: "e-OTHER".into(), ctag: String::new(), mtime: 0 });
                cloud.rename("docs", "OTHER", "docs");
            } else {
                cloud.rename("docs", crate::fake_onedrive::ROOT, "renamed");
            }
        });
        let moved = folder.graph.with(|cloud| cloud.item("docs").unwrap().etag.clone());
        assert_ne!(moved, planned, "the folder's own rename or move moves its eTag");
        assert_eq!(folder.run(step.clone()).await, StepOutcome::Refused(StepRefusal::ItemChanged));
        assert_eq!(folder.graph.with(|cloud| cloud.guards.iter().filter(|(path, _)| path == "me/drive/items/docs").cloned().collect::<Vec<_>>()), [("me/drive/items/docs".to_owned(), planned.clone())]);
        assert!(folder.graph.with(|cloud| cloud.item("docs").is_some() && cloud.item("docs!c.txt").is_some()));
        assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
        assert_eq!(folder.count("DELETE", ""), 1, "no unconditional retry");
        folder.list_cloud();
        assert_eq!(folder.fixture.versions_of(step.item().local_item_id).onedrive_etag, Some(moved), "the listing wrote the new eTag");
    }
}

/// The window the design leaves: what OneDrive gets below the directory after the
/// attempt's listing and before its DELETE goes with it, to the recycle bin, inside the
/// folder — where the user's Restore finds it.
#[tokio::test]
async fn an_item_added_in_onedrive_after_the_listing_and_before_the_delete_goes_to_the_recycle_bin_inside_the_folder() {
    let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
    folder.graph.with(|cloud| cloud.add_file("LATE", "docs", "late.txt", b"late"));
    assert!(matches!(folder.run(step).await, StepOutcome::Written { .. }));
    folder.graph.with(|cloud| {
        assert!(cloud.item("LATE").is_none());
        assert_eq!(cloud.bin.get("LATE").and_then(|late| late.parent.clone()).as_deref(), Some("docs"));
        assert!(cloud.bin.contains_key("docs") && cloud.bin.contains_key("docs!c.txt"));
    });
}

/// A store migrated from schema 31 had no folder cTag in any row: a folder's removal goes
/// through on its eTag.
#[tokio::test]
async fn a_folder_removal_goes_through_on_a_store_migrated_from_schema_31() {
    let fixture = ItemFixture::migrated_from_schema_31(None);
    let directory = fixture.show_onedrive_directory("docs");
    let file = fixture.show_onedrive_file("docs/c.txt", b"c");
    fixture.remove_here(file.local_item_id);
    fixture.remove_here(directory.local_item_id);
    let folder = StepsFolder::start(fixture).await;
    let step = || OneDriveStep::SendDirectoryRemoval { item: folder.fixture.versions_of(directory.local_item_id), below: vec![folder.fixture.versions_of(file.local_item_id)] };
    assert_eq!(folder.run(step()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
    folder.list_cloud();
    let planned = folder.fixture.versions_of(directory.local_item_id).onedrive_etag.expect("the eTag of a folder listed");
    assert!(matches!(folder.run(step()).await, StepOutcome::Written { .. }));
    assert!(folder.graph.with(|cloud| cloud.guards.contains(&("me/drive/items/docs".to_owned(), planned)) && cloud.bin.contains_key("docs")));
}

fn save(folder: &StepsFolder, replacing: LocalItemId, replaced: LocalItemId) {
    let path = folder.fixture.folder();
    let store = folder.fixture.store.clone();
    folder.fixture.run_on_write_operation_thread(move || {
        std::fs::rename(path.join("temporary.txt"), path.join("original.txt")).unwrap();
        store.call_blocking(move |store| store.item_store().apply(&LocalVersionChange::SavedOver { replacing, replaced })).unwrap();
    });
    let mut marks = folder.fixture.read_disk_marks("original.txt");
    marks.onedrive_item_id = Some("original.txt".to_owned());
    folder.fixture.write_disk_marks("original.txt", &marks);
}

async fn two_cloud_files() -> (StepsFolder, LocalItemId, LocalItemId) {
    let fixture = ItemFixture::new();
    let destination = fixture.show_onedrive_file("original.txt", b"original").local_item_id;
    let temporary = fixture.show_onedrive_file("temporary.txt", b"temporary").local_item_id;
    fixture.land_copy(destination, b"original");
    fixture.land_copy(temporary, b"temporary");
    (StepsFolder::start(fixture).await, temporary, destination)
}

#[tokio::test]
async fn an_uploaded_temporary_saved_over_a_cloud_file_updates_the_original_id() {
    let (folder, temporary, destination) = two_cloud_files().await;
    save(&folder, temporary, destination);
    let upload = folder.fixture.versions_of(temporary);
    assert_eq!(upload.onedrive_item.as_ref().unwrap().item_id.as_str(), "original.txt");
    assert!(matches!(folder.run(OneDriveStep::SendContent { item: upload }).await, StepOutcome::Written { .. }));
    let removal = folder.fixture.versions_of(destination);
    assert_eq!(removal.onedrive_item.as_ref().unwrap().item_id.as_str(), "temporary.txt");
    assert!(matches!(folder.run(OneDriveStep::SendFileRemoval { item: removal }).await, StepOutcome::Written { .. }));
    assert_eq!(folder.graph.with(|cloud| cloud.item("original.txt").unwrap().content.clone()), b"temporary");
    assert_eq!(folder.cloud_paths(), ["original.txt"]);
    assert_eq!(folder.fixture.read_disk_marks("original.txt").onedrive_item_id.as_deref(), Some("original.txt"));
}

#[tokio::test]
async fn a_cloud_edit_of_the_destination_refuses_an_atomic_save_upload() {
    let (folder, temporary, destination) = two_cloud_files().await;
    save(&folder, temporary, destination);
    folder.graph.with(|cloud| cloud.edit("original.txt", b"another device"));
    assert_eq!(folder.run(OneDriveStep::SendContent { item: folder.fixture.versions_of(temporary) }).await, StepOutcome::Refused(StepRefusal::ItemChanged));
    assert_eq!(folder.graph.with(|cloud| cloud.item("original.txt").unwrap().content.clone()), b"another device");
}

#[tokio::test]
async fn a_cloud_edit_of_the_temporary_refuses_its_cleanup_delete() {
    let (folder, temporary, destination) = two_cloud_files().await;
    save(&folder, temporary, destination);
    folder.graph.with(|cloud| cloud.edit("temporary.txt", b"another device"));
    assert_eq!(folder.run(OneDriveStep::SendFileRemoval { item: folder.fixture.versions_of(destination) }).await, StepOutcome::Refused(StepRefusal::ItemChanged));
    assert!(folder.graph.with(|cloud| cloud.item("temporary.txt").is_some()));
}

#[tokio::test]
async fn an_upload_in_flight_follows_the_temporary_binding_after_an_atomic_save() {
    let (folder, temporary, destination) = two_cloud_files().await;
    folder.fixture.change_file_here(temporary, b"new bytes");
    let step = OneDriveStep::SendContent { item: folder.fixture.versions_of(temporary) };
    folder.graph.with(|cloud| cloud.delay("POST", "items/temporary.txt/createUploadSession", std::time::Duration::from_millis(250), 1));
    let folder = Arc::new(folder);
    let running = tokio::spawn({ let folder = folder.clone(); async move { folder.run(step).await } });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while folder.count("POST", "items/temporary.txt/createUploadSession") == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    save(&folder, temporary, destination);
    assert_eq!(running.await.unwrap(), StepOutcome::NotRun(StepObstacle::ContentChanged));
    assert_eq!(folder.fixture.versions_of(temporary).onedrive_item.as_ref().unwrap().item_id.as_str(), "original.txt");
    assert_eq!(folder.graph.with(|cloud| cloud.item("original.txt").unwrap().content.clone()), b"original", "the in-flight request cannot replace the original");
    assert!(matches!(folder.run(OneDriveStep::SendContent { item: folder.fixture.versions_of(temporary) }).await, StepOutcome::Written { .. }));
    assert_eq!(folder.fixture.read_disk_marks("original.txt").onedrive_item_id.as_deref(), Some("original.txt"));
}

#[tokio::test]
async fn a_new_temporarys_in_flight_opening_cannot_attach_to_the_original_binding() {
    let fixture = ItemFixture::new();
    let destination = fixture.show_onedrive_file("original.txt", b"original").local_item_id;
    fixture.land_copy(destination, b"original");
    let temporary = fixture.make_own_file("temporary.txt", b"new bytes").local_item_id;
    let folder = Arc::new(StepsFolder::start(fixture).await);
    let step = OneDriveStep::SendNewFile { item: folder.fixture.versions_of(temporary), parent: StepsFolder::root(), name: "temporary.txt".into() };
    folder.graph.with(|cloud| cloud.delay("POST", "createUploadSession", std::time::Duration::from_millis(250), 1));
    let running = tokio::spawn({ let folder = folder.clone(); async move { folder.run(step).await } });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while folder.count("POST", "createUploadSession") == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    save(&folder, temporary, destination);
    assert_eq!(running.await.unwrap(), StepOutcome::NotRun(StepObstacle::ContentChanged));
    assert!(folder.memory(temporary).upload_session.is_none());
    assert!(matches!(folder.run(OneDriveStep::SendContent { item: folder.fixture.versions_of(temporary) }).await, StepOutcome::Written { .. }));
    assert_eq!(folder.graph.with(|cloud| cloud.item("original.txt").unwrap().content.clone()), b"new bytes");
}

#[tokio::test]
async fn a_temporary_upload_committing_after_the_save_cannot_restore_the_temporary_xattr_id() {
    let (folder, temporary, destination) = two_cloud_files().await;
    folder.fixture.change_file_here(temporary, b"new bytes");
    let step = OneDriveStep::SendContent { item: folder.fixture.versions_of(temporary) };
    folder.graph.with(|cloud| cloud.delay("PUT", "upload/", std::time::Duration::from_millis(250), 1));
    let folder = Arc::new(folder);
    let running = tokio::spawn({ let folder = folder.clone(); async move { folder.run(step).await } });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while folder.count("PUT", "upload/") == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    save(&folder, temporary, destination);
    assert!(matches!(running.await.unwrap(), StepOutcome::Written { .. }));
    assert_eq!(folder.fixture.read_disk_marks("original.txt").onedrive_item_id.as_deref(), Some("original.txt"));
    assert_eq!(folder.fixture.versions_of(temporary).onedrive_item.as_ref().unwrap().item_id.as_str(), "original.txt");
    assert!(matches!(folder.run(OneDriveStep::SendContent { item: folder.fixture.versions_of(temporary) }).await, StepOutcome::Written { .. }));
    assert_eq!(folder.fixture.read_disk_marks("original.txt").onedrive_item_id.as_deref(), Some("original.txt"));
}

#[tokio::test]
async fn the_fake_refuses_a_folder_delete_without_if_match() {
    let (folder, _) = stage(StagedStep::SendDirectoryRemoval).await;
    let answer = reqwest::Client::new().delete(format!("{}/me/drive/items/docs", folder.graph.server.uri())).send().await.unwrap();
    assert_eq!(answer.status().as_u16(), 412);
    let file_ctag = folder.graph.with(|cloud| cloud.item("docs!c.txt").unwrap().ctag.clone());
    for tag in ["", "*", file_ctag.as_str(), "e-docs-stale"] {
        let answer = reqwest::Client::new().delete(format!("{}/me/drive/items/docs", folder.graph.server.uri())).header("If-Match", tag).send().await.unwrap();
        assert_eq!(answer.status().as_u16(), 412, "{tag:?}");
    }
    assert!(folder.graph.with(|cloud| cloud.item("docs").is_some() && cloud.item("docs").unwrap().ctag.is_empty()));
}

/// The removal the user made of `Documents/Recipes`, end to end: one folder, one resident
/// file the change guard set aside. The DELETE waits for the removal's quiet time and for
/// the attempt's listing, goes out once with the folder's eTag, takes both to the recycle
/// bin, and the set-aside bytes stay.
#[tokio::test]
async fn a_folder_with_a_resident_file_set_aside_is_removed_on_its_etag_after_its_quiet_time_and_its_listing() {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(crate::change_guard::ChangeGuard::open(dir.path().join("guard.json"), Default::default()));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let directory = fixture.show_onedrive_directory("Recipes");
    let file = fixture.show_onedrive_file("Recipes/Pie.docx", b"13 KiB of recipe");
    fixture.land_copy(file.local_item_id, b"13 KiB of recipe");
    let now = (fixture.clock())();
    let retained = guard.prepare_removal(&fixture.find_item(file.local_item_id).unwrap(), "Recipes/Pie.docx", now, false).unwrap().unwrap();
    let retained = fixture.disk_path(retained.to_str().unwrap());
    std::fs::create_dir_all(retained.parent().unwrap()).unwrap();
    std::fs::rename(fixture.disk_path("Recipes/Pie.docx"), &retained).unwrap();
    let removed = fixture.remove_here(file.local_item_id).unwrap();
    // As the plan does: the committed removal replaces the one observed when it was set aside.
    guard.observe(&[(removed, "Recipes/Pie.docx".into())], now).unwrap();
    fixture.remove_here(directory.local_item_id);
    let folder = StepsFolder::start(fixture).await;
    let step = || OneDriveStep::SendDirectoryRemoval { item: folder.fixture.versions_of(directory.local_item_id), below: vec![folder.fixture.versions_of(file.local_item_id)] };
    assert_eq!(folder.run(step()).await, StepOutcome::NotRun(StepObstacle::SendingStopped), "the removal's quiet time");
    assert_eq!(folder.count("DELETE", ""), 0);
    folder.fixture.now.fetch_add(31, Ordering::SeqCst);
    assert_eq!(folder.run(step()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
    assert_eq!(folder.count("DELETE", ""), 0);
    folder.list_cloud();
    let tag = folder.fixture.versions_of(directory.local_item_id).onedrive_etag.expect("the folder's eTag");
    assert!(matches!(folder.run(step()).await, StepOutcome::Written { .. }));
    assert_eq!(folder.count("DELETE", ""), 1);
    folder.graph.with(|cloud| {
        assert_eq!(cloud.guards.iter().filter(|(path, _)| path == "me/drive/items/Recipes").cloned().collect::<Vec<_>>(), [("me/drive/items/Recipes".to_owned(), tag)]);
        assert!(cloud.bin.contains_key("Recipes") && cloud.bin.contains_key("Recipes!Pie.docx"));
    });
    assert_eq!(std::fs::read(&retained).unwrap(), b"13 KiB of recipe", "the set-aside bytes stay");
}
