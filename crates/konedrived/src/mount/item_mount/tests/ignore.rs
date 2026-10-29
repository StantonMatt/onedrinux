//! Default build names stay local only while unbound; upgrades preserve cloud rows and bytes.
use crate::local::IgnoreList;
use crate::mount::item_fixture::ItemFixture;
use konedrive_tree::LocalItemId;

const BUILD_NAMES: &[&str] = &["node_modules", ".venv", "venv", "__pycache__", ".gradle"];

fn left_here(fixture: &ItemFixture, id: LocalItemId) -> bool {
    fixture.read_rows(move |r| r.is_ignored(id))
}

fn unsent(fixture: &ItemFixture) -> Vec<LocalItemId> {
    fixture.write_rows(|s| Ok(s.item_store().list_unsent(None, 100)?.iter().map(|r| r.local_item_id).collect()))
}

#[test]
fn default_build_folders_created_locally_stay_visible_with_their_bytes_without_uploads() {
    let fixture = ItemFixture::new();
    *crate::panic::write(&fixture.ignore) = IgnoreList::configured(None);
    for name in BUILD_NAMES {
        let directory = fixture.make_own_directory(name);
        let path = format!("{name}/unique.txt");
        let child = fixture.make_own_file(&path, b"unique local bytes");
        assert!(left_here(&fixture, directory.local_item_id));
        assert!(left_here(&fixture, child.local_item_id));
        assert_eq!(fixture.read_disk_file(&path).unwrap(), b"unique local bytes");
        assert!(fixture.find_item_at(name).is_some());
    }
    assert!(unsent(&fixture).is_empty());
    fixture.assert_rows_and_disk_agree();
}

#[test]
fn default_build_names_already_in_onedrive_remain_visible_and_sync_new_children() {
    let fixture = ItemFixture::new();
    *crate::panic::write(&fixture.ignore) = IgnoreList::configured(None);
    for name in BUILD_NAMES {
        let directory = fixture.show_onedrive_directory(name);
        let path = format!("{name}/cloud.txt");
        let cloud = fixture.show_onedrive_file(&path, b"cloud bytes");
        let child = fixture.make_own_file(&format!("{name}/local.txt"), b"new upload");
        for id in [directory.local_item_id, cloud.local_item_id, child.local_item_id] {
            assert!(!left_here(&fixture, id), "a bound directory is not excluded by its name");
        }
        assert!(unsent(&fixture).contains(&child.local_item_id));
        assert_eq!(fixture.find_item_at(name).unwrap().onedrive_item, directory.onedrive_item);
    }
    fixture.assert_rows_and_disk_agree();
}

#[test]
fn an_upgrade_refreshes_unbound_build_names_and_preserves_synced_pinned_folders() {
    let fixture = ItemFixture::new();
    *crate::panic::write(&fixture.ignore) = IgnoreList::new(Vec::<String>::new());
    let local_parent = fixture.make_own_directory("local");
    let mut local = Vec::new();
    let mut bound = Vec::new();
    for name in BUILD_NAMES {
        let dir = fixture.show_onedrive_directory(name);
        let reference = dir.onedrive_item.clone().unwrap();
        let pin = reference.item_id.as_str().to_owned();
        fixture.write_rows(move |s| s.set_pin(&pin, true));
        let path = format!("{name}/cloud.txt");
        let file = fixture.show_onedrive_file(&path, b"downloaded cloud bytes");
        fixture.land_copy(file.local_item_id, b"downloaded cloud bytes");
        bound.push((dir, fixture.find_item(file.local_item_id).unwrap(), path));
        let dir = fixture.make_own_directory(&format!("local/{name}"));
        let path = format!("local/{name}/unique.txt");
        let file = fixture.make_own_file(&path, b"never uploaded bytes");
        local.push((dir, file, path));
    }
    assert_eq!(unsent(&fixture).len(), 11);
    *crate::panic::write(&fixture.ignore) = IgnoreList::configured(None);
    let mount = fixture.build_mount(); // Runs the actual upgrade/startup flag refresh.
    for (before, file, path) in bound {
        let after = fixture.find_item(before.local_item_id).unwrap();
        assert!(after.is_name_ignored);
        assert_eq!((&after.local, &after.onedrive, &after.onedrive_item), (&before.local, &before.onedrive, &before.onedrive_item));
        assert!(!left_here(&fixture, file.local_item_id));
        assert!(!left_here(&fixture, before.local_item_id));
        assert_eq!(fixture.find_item(file.local_item_id).unwrap().content_presence, file.content_presence);
        assert_eq!(fixture.read_disk_file(&path).unwrap(), b"downloaded cloud bytes");
        assert!(fixture.write_rows(|s| s.pins()).contains(&after.onedrive_item.unwrap().item_id.as_str().to_owned()));
    }
    for (dir, file, path) in local {
        assert!(left_here(&fixture, dir.local_item_id));
        assert!(left_here(&fixture, file.local_item_id));
        assert_eq!(fixture.read_disk_file(&path).unwrap(), b"never uploaded bytes");
        assert!(fixture.find_item_at(&path).is_some());
    }
    assert_eq!(unsent(&fixture), [local_parent.local_item_id]);
    fixture.assert_rows_and_disk_agree();
    mount.stop();
}

#[test]
fn a_new_onedrive_folder_at_a_left_here_build_folders_name_waits_and_the_local_folder_stays_unsent() {
    use crate::sync_plan_task::local_step_tries::LocalStepTries;
    use crate::sync_plan_task::plan_input::{plan_next_steps, DaemonFacts, PlanSettings};
    use konedrive_tree::{Reason, WaitReason};
    use std::collections::BTreeSet;

    let fixture = ItemFixture::new();
    *crate::panic::write(&fixture.ignore) = IgnoreList::configured(None);
    let local = fixture.make_own_directory("node_modules");
    let dependency = fixture.make_own_file("node_modules/dependency.js", b"never uploaded bytes");
    let cloud = fixture.list_onedrive_directory("node_modules");
    let cloud_file = fixture.list_onedrive_file("node_modules/cloud.js", b"cloud bytes");
    let runners = fixture.step_runners_stand_in();
    let facts = DaemonFacts {
        items_with_running_step: BTreeSet::new(),
        destinations_of_running_steps: Vec::new(),
        is_onedrive_full: false,
        now: fixture.clock()() + 1_000,
        left_by_start_up_check: BTreeSet::new(),
        local_step_tries: LocalStepTries::default(),
        is_pinned_files_reader_asked: true,
        settings: PlanSettings { computer_name: "here".into(), quiet_time: 0, items_read_at_once: 100 },
    };
    let mut plan = None;
    for _ in 0..4 {
        let facts = facts.clone();
        let round = fixture.read_rows(move |items| plan_next_steps(items, &facts)).plan;
        for step in round.steps.clone() {
            fixture.runtime().block_on((runners.run_step)(step));
        }
        plan = Some(round);
    }
    let plan = plan.unwrap();
    assert!(plan.steps.is_empty(), "{plan:?}");
    let wait_of = |item: LocalItemId| plan.waits.iter().find(|wait| wait.item == item).map(|wait| wait.reason.clone());
    assert_eq!(wait_of(cloud.local_item_id), Some(WaitReason::User(Reason::Parent)), "OneDrive's folder waits for the user");
    let kept = fixture.find_item_at("node_modules").unwrap();
    assert_eq!((kept.local_item_id, kept.onedrive_item.as_ref()), (local.local_item_id, None), "neither renamed nor merged");
    assert!(left_here(&fixture, local.local_item_id) && left_here(&fixture, dependency.local_item_id));
    assert_eq!(fixture.read_disk_file("node_modules/dependency.js").unwrap(), b"never uploaded bytes");
    assert!(unsent(&fixture).is_empty(), "nothing in the local folder is sent");
    for item in [cloud, cloud_file] {
        let row = fixture.find_item(item.local_item_id).unwrap();
        assert_eq!(row.onedrive_item, item.onedrive_item, "OneDrive's items stay where they are");
        assert!(row.onedrive.is_some());
    }
    assert!(fixture.list_items().iter().all(|row| row.local.as_ref().and_then(|l| l.place.as_ref()).is_none_or(|place| !place.name.to_string_lossy().starts_with("node_modules-"))));
    fixture.assert_rows_and_disk_agree();
}

#[test]
fn the_start_retires_held_uploads_of_rows_it_leaves_here_and_keeps_dont_upload() {
    use crate::change_guard::{ChangeGuard, GuardConfig};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(dir.path().join("ledger.json"), GuardConfig { uploads: 0, ..Default::default() }));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    *crate::panic::write(&fixture.ignore) = IgnoreList::new(Vec::<String>::new());
    let directory = fixture.make_own_directory("node_modules");
    let file = fixture.make_own_file("node_modules/dependency.js", b"never uploaded bytes");
    let kept_local = fixture.make_own_directory(".venv");
    let other = fixture.make_own_file("notes.txt", b"notes");
    let observed: Vec<_> = [(&directory, "node_modules"), (&file, "node_modules/dependency.js"), (&other, "notes.txt")].into_iter().map(|(row, path)| (row.clone(), path.to_owned())).collect();
    guard.observe(&observed, 100).unwrap();
    guard.keep_local(&kept_local, ".venv".into(), 100).unwrap();
    assert!([directory.local_item_id, file.local_item_id, other.local_item_id].into_iter().all(|id| guard.blocks(id)));
    let paths = || guard.list(false).into_iter().map(|held| held.path).collect::<std::collections::BTreeSet<_>>();
    assert!(paths().contains("node_modules"));

    *crate::panic::write(&fixture.ignore) = IgnoreList::configured(None);
    let mount = fixture.build_mount();
    assert!(left_here(&fixture, directory.local_item_id) && left_here(&fixture, file.local_item_id));
    assert!(!guard.blocks(directory.local_item_id) && !guard.blocks(file.local_item_id), "no phantom upload is left held");
    assert!(guard.blocks(other.local_item_id), "what is still to upload stays held");
    assert!(guard.local_only().contains(&kept_local.local_item_id), "Don't Upload stays the user's choice");
    assert!(paths().iter().all(|path| !path.starts_with("node_modules")), "On Hold lists neither: {:?}", paths());
    mount.stop();
}

#[test]
fn a_held_upload_whose_row_the_start_removes_is_retired_and_the_mount_builds() {
    use crate::change_guard::{ChangeGuard, GuardConfig};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let guard = Arc::new(ChangeGuard::open(dir.path().join("ledger.json"), GuardConfig { uploads: 0, ..Default::default() }));
    let fixture = ItemFixture::with_change_guard(Some(Arc::clone(&guard)));
    let gone = fixture.make_own_file("gone.txt", b"lost in a power cut");
    let other = fixture.make_own_file("notes.txt", b"notes");
    guard.observe(&[(gone.clone(), "gone.txt".to_owned()), (other.clone(), "notes.txt".to_owned())], 100).unwrap();
    assert!(guard.blocks(gone.local_item_id) && guard.blocks(other.local_item_id));
    std::fs::remove_file(fixture.disk_path("gone.txt")).unwrap();

    let mount = fixture.build_mount();
    assert!(fixture.find_item(gone.local_item_id).is_none(), "the start check removes the unbound row");
    assert!(!guard.blocks(gone.local_item_id), "the stale entry is retired");
    assert!(guard.blocks(other.local_item_id), "what is still to upload stays held");
    assert!(guard.list(false).iter().all(|held| held.path != "gone.txt"));
    mount.stop();
}
