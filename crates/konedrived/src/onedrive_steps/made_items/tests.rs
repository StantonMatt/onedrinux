use crate::onedrive_steps::tests::StepsFolder;
use crate::mount::item_fixture::ItemFixture;

use super::*;
use std::sync::Arc;

#[test]
fn the_fate_of_a_made_item_is_by_its_row() {
    let fixture = ItemFixture::new();
    let row = fixture.make_own_file("a.txt", b"a");
    assert_eq!(decide_made_item_fate(Some(&row)), MadeItemFate::LeaveForListing);
    assert_eq!(decide_made_item_fate(None), MadeItemFate::Remove);
}

/// What a step made and its commit did not write: the item, as the fake has it.
fn confirmed(folder: &StepsFolder, path: &str) -> ConfirmedItem {
    let answered = folder.graph.with(|cloud| cloud.at(path).cloned()).expect("the made item");
    ConfirmedItem {
        item: ItemFixture::onedrive_reference_of(&answered.id),
        parent: ItemFixture::onedrive_reference_of(""),
        name: answered.name.clone().into(),
        content: (!answered.folder).then(|| ItemFixture::onedrive_content_of(&answered.content)),
        etag: answered.etag,
    }
}

#[tokio::test]
async fn a_new_item_whose_row_is_gone_is_removed_in_onedrive() {
    let fixture = ItemFixture::new();
    let (file, directory) = (fixture.make_own_file("a.txt", b"a"), fixture.make_own_directory("new"));
    let (file_versions, directory_versions) = (fixture.versions_of(file.local_item_id), fixture.versions_of(directory.local_item_id));
    let folder = StepsFolder::start(fixture).await;
    folder.graph.with(|cloud| {
        cloud.add_file("MADE", "R", "a.txt", b"a");
        cloud.add(crate::fake_onedrive::FakeItem { id: "MADEDIR".into(), parent: Some("R".into()), name: "new".into(), folder: true, content: Vec::new(), hash: None, size: 0, etag: "e-d".into(), ctag: "c-d".into(), mtime: 0 });
    });
    let (made_file, made_directory) = (confirmed(&folder, "a.txt"), confirmed(&folder, "new"));
    folder.fixture.remove_here(file.local_item_id);
    folder.fixture.remove_here(directory.local_item_id);

    assert_eq!(folder.made_items.follow_up_new_item(&file_versions, &made_file).await, RowsChangedFollowUp::MadeItemRemoved(made_file.item.clone()));
    assert_eq!(folder.made_items.follow_up_new_item(&directory_versions, &made_directory).await, RowsChangedFollowUp::MadeItemLeft(made_directory.item.clone()));
    assert_eq!(folder.graph.with(|cloud| cloud.paths()), ["new"], "a directory made is kept: nothing proves what is in it is ours");
    // The file's removal is guarded by the eTag of the answer; a directory's has no guard.
    assert_eq!(folder.graph.with(|cloud| cloud.guards.clone()), [("me/drive/items/MADE".to_owned(), made_file.etag.clone())]);
}

#[tokio::test]
async fn a_new_item_whose_row_is_still_there_asks_for_a_cycle() {
    let fixture = ItemFixture::new();
    let file = fixture.make_own_file("a.txt", b"a");
    let versions = fixture.versions_of(file.local_item_id);
    let folder = StepsFolder::start(fixture).await;
    folder.graph.with(|cloud| cloud.add_file("MADE", "R", "a.txt", b"a"));
    let made = confirmed(&folder, "a.txt");

    assert_eq!(folder.made_items.follow_up_new_item(&versions, &made).await, RowsChangedFollowUp::CycleWanted);
    assert_eq!(folder.graph.with(|cloud| cloud.paths()), ["a.txt"]);
}

#[tokio::test]
async fn a_made_item_that_cannot_be_removed_is_left_and_said() {
    let fixture = ItemFixture::new();
    let file = fixture.make_own_file("a.txt", b"a");
    let versions = fixture.versions_of(file.local_item_id);
    let folder = StepsFolder::start(fixture).await;
    folder.graph.with(|cloud| cloud.add_file("MADE", "R", "a.txt", b"a"));
    let made = confirmed(&folder, "a.txt");
    folder.fixture.remove_here(file.local_item_id);
    // Another changed it since it was made: the removal is answered 412.
    folder.graph.with(|cloud| cloud.edit("MADE", b"another's"));

    assert_eq!(folder.made_items.follow_up_new_item(&versions, &made).await, RowsChangedFollowUp::MadeItemLeft(made.item.clone()));
    assert_eq!(folder.graph.with(|cloud| cloud.paths()), ["a.txt"]);
}

#[tokio::test]
async fn write_scope_leaves_unresolvable_cleanup_items_in_onedrive() {
    use crate::config::{WriteScope, WriteScopeSource};
    let fixture = ItemFixture::new();
    let file = fixture.make_own_file("a.txt", b"a");
    let versions = fixture.versions_of(file.local_item_id);
    let mut folder = StepsFolder::start(fixture).await;
    folder.graph.with(|cloud| cloud.add_file("MADE", "R", "a.txt", b"a"));
    let made = confirmed(&folder, "a.txt");
    folder.fixture.remove_here(file.local_item_id);
    folder.made_items = folder.made_items.with_write_scope(WriteScopeSource::new(WriteScope::denied));
    assert_eq!(folder.made_items.follow_up_new_item(&versions, &made).await, RowsChangedFollowUp::MadeItemLeft(made.item.clone()));
    assert_eq!(folder.count("DELETE", "items/"), 0);
    assert_eq!(folder.graph.with(|cloud| cloud.paths()), ["a.txt"]);
}

#[tokio::test]
async fn a_cleanup_waiting_for_metadata_cannot_cross_a_new_guard_hold() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(crate::change_guard::ChangeGuard::open(dir.path().join("guard.json"), crate::change_guard::GuardConfig { enabled: false, ..Default::default() }));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let file = fixture.make_own_file("a.txt", b"a");
    let versions = fixture.versions_of(file.local_item_id);
    let folder = Arc::new(StepsFolder::start(fixture).await);
    folder.graph.with(|cloud| cloud.add_file("MADE", "R", "a.txt", b"a"));
    let made = confirmed(&folder, "a.txt");
    folder.fixture.remove_here(file.local_item_id);
    folder.pool.set_paused(true);
    let waiting = tokio::spawn({
        let folder = Arc::clone(&folder);
        async move { folder.made_items.follow_up_new_item(&versions, &made).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !folder.pool.waiting() { tokio::task::yield_now().await; }
    }).await.expect("the cleanup waits for its metadata slot");
    guard.configure(crate::change_guard::GuardConfig { removals: 0, ..Default::default() });
    folder.pool.set_paused(false);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), waiting).await.unwrap().unwrap(), RowsChangedFollowUp::CycleWanted);
    assert_eq!(folder.count("DELETE", "items/"), 0);
    assert_eq!(folder.graph.with(|cloud| cloud.paths()), ["a.txt"]);
}

#[tokio::test]
async fn a_deferred_one_shot_cleanup_is_durable_and_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(crate::change_guard::ChangeGuard::open(dir.path().join("guard.json"), Default::default()));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let file = fixture.make_own_file("a.txt", b"a");
    let versions = fixture.versions_of(file.local_item_id);
    let folder = StepsFolder::start(fixture).await;
    folder.graph.with(|cloud| cloud.add_file("MADE", "R", "a.txt", b"a"));
    let made = confirmed(&folder, "a.txt");
    folder.fixture.remove_here(file.local_item_id);
    assert_eq!(folder.made_items.follow_up_new_item(&versions, &made).await, RowsChangedFollowUp::CycleWanted);
    assert_eq!(folder.fixture.read_rows(|items| items.list_mismatched_item_removals(10)).len(), 1);
    assert_eq!(folder.count("DELETE", ""), 0);
    // A new cancellation runner reads its work from the durable queue, not a closure
    // or the vanished local row. Advancing time completes the removal quiet period.
    folder.fixture.now.fetch_add(31, std::sync::atomic::Ordering::SeqCst);
    let runner = crate::onedrive_steps::session_cancellations::SessionCancellations::new(folder.fixture.graph_access(&folder.graph), folder.lost_answers.clone(), Arc::clone(&folder.fixture.running_steps));
    let round = runner.remove_mismatched_items(10).await;
    assert_eq!(round.done_count, 1);
    assert!(folder.cloud_paths().is_empty());
    assert!(guard.list(false).iter().all(|r| !r.path.starts_with("[upload cleanup]")));
}
