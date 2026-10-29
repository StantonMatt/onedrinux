//! Namespace safety against remote changes and saves crossing requests.

use super::*;
use konedrive_tree::LocalVersionChange;

#[tokio::test]
async fn a_folder_ctag_refuses_an_unlisted_descendant_creation_edit_or_removal() {
    for change in 0..3 {
        let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
        let planned = step.item().onedrive_folder_ctag.clone().expect("the listing's folder cTag");
        folder.graph.with(|cloud| match change {
            0 => cloud.add_file("UNSEEN", "docs", "unseen.txt", b"new"),
            1 => cloud.edit("docs!c.txt", b"changed"),
            _ => cloud.trash("docs!c.txt"),
        });
        assert_eq!(folder.run(step).await, StepOutcome::Refused(StepRefusal::ItemChanged));
        assert!(folder.graph.with(|cloud| cloud.item("docs").is_some()));
        assert!(folder.graph.with(|cloud| cloud.guards.contains(&("me/drive/items/docs".to_owned(), planned))));
    }
}

#[tokio::test]
async fn a_directory_removal_waits_for_a_later_completed_listing() {
    let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
    // A newly started sender has not asked for its own refresh yet.
    crate::panic::lock(&folder.steps.directory_removal_listings).clear();
    assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
    assert_eq!(folder.run(step.clone()).await, StepOutcome::RowsChanged(RowsChangedFollowUp::CycleWanted));
    assert_eq!(folder.count("DELETE", ""), 0);
    folder.fixture.write_listing(vec![]);
    assert!(matches!(folder.run(step).await, StepOutcome::Written { .. }));
}

#[tokio::test]
async fn a_directory_without_a_folder_ctag_never_sends_a_delete() {
    let (folder, step) = stage(StagedStep::SendDirectoryRemoval).await;
    let OneDriveStep::SendDirectoryRemoval { mut item, below } = step else { unreachable!() };
    item.onedrive_folder_ctag = None;
    assert_eq!(folder.run(OneDriveStep::SendDirectoryRemoval { item, below }).await, StepOutcome::Refused(StepRefusal::ItemChanged));
    assert_eq!(folder.count("DELETE", ""), 0);
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
    assert!(folder.graph.with(|cloud| cloud.item("docs").is_some()));
}
