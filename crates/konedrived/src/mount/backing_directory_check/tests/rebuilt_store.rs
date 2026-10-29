//! A store that was lost and made again: the rows are made from the disk, a marked entry
//! bound to the id of its mark, an unmarked one new.

use konedrive_tree::{ContentPresence, ContentVersion, ItemKind};

use super::{changed_here, content, folder, found, reference, DiskFinding, Folder};

/// A backing directory as a daemon left it, and no row but the root's.
fn tree_with_no_rows() -> Folder {
    let f = folder();
    f.make_directory("docs");
    f.mark_directory("docs", "D");
    f.write("docs/a.txt", b"aaa");
    f.mark_copy("docs/a.txt", "A");
    f.make_directory("docs/sub");
    f.mark_directory("docs/sub", "S");
    f.write("docs/sub/b.txt", b"bbbb");
    f.mark_copy("docs/sub/b.txt", "B");
    f.make_directory("mine");
    f.write("mine/own.txt", b"own");
    f.write("top.txt", b"top");
    f
}

#[test]
fn every_marked_entry_is_bound_to_the_id_of_its_mark_and_an_unmarked_one_is_new() {
    let mut f = tree_with_no_rows();
    let findings = f.check();
    assert_eq!(found(&findings), [(DiskFinding::Made, 3), (DiskFinding::MarkedWithoutRow, 4)]);
    assert_eq!(findings.unresolved(), []);
    for (path, id) in [("docs", "D"), ("docs/a.txt", "A"), ("docs/sub", "S"), ("docs/sub/b.txt", "B")] {
        assert_eq!(f.row(path).unwrap().onedrive_item, Some(reference("d1", id)), "{path}");
        assert_eq!(f.mark_of(path).as_deref(), Some(id), "{path}: the mark stays");
    }
    for path in ["mine", "mine/own.txt", "top.txt"] {
        assert_eq!(f.row(path).unwrap().onedrive_item, None, "{path}");
    }
    // A whole copy is taken with the content its cTag mark tells, with no hash: the plan
    // compares it with what a listing brings. Nothing is agreed on yet.
    let a = f.row("docs/a.txt").unwrap();
    assert_eq!(content(&a), changed_here(3, 1));
    assert_eq!((a.content_presence, a.synced_place.clone(), a.synced_content.clone(), a.onedrive.clone()), (ContentPresence::OnDisk, None, None, None));
    assert_eq!(content(&f.row("top.txt").unwrap()), changed_here(3, 1));
    f.assert_settled();
}

/// A file that carries the id alone holds the person's bytes.
#[test]
fn a_file_marked_with_an_id_alone_is_bound_with_its_bytes_as_content_changed_here() {
    let mut f = folder();
    f.write("saved.txt", b"saved by rename");
    xattr::set(f.disk("saved.txt"), konedrive_fs::placeholder::XATTR_ITEM_ID, b"A").unwrap();
    let findings = f.check();
    assert_eq!((found(&findings), findings.unresolved()), (vec![(DiskFinding::MarkedWithoutRow, 1)], &[][..]));
    let row = f.row("saved.txt").unwrap();
    assert_eq!((row.onedrive_item.clone(), content(&row), row.content_presence), (Some(reference("d1", "A")), changed_here(15, 1), ContentPresence::OnDisk));
    f.assert_settled();
}

#[test]
fn a_marked_entry_no_row_has_is_bound_in_the_drive_of_the_nearest_directory_of_onedrive_above_it() {
    let mut f = folder();
    f.directory_of_drive("shared", "d2", "S");
    f.make_directory("shared/inner");
    f.write("shared/inner/x.bin", b"x");
    f.mark_copy("shared/inner/x.bin", "X");
    f.write("y.bin", b"y");
    f.mark_copy("y.bin", "Y");
    assert_eq!(found(&f.check()), [(DiskFinding::NothingChanged, 1), (DiskFinding::Made, 1), (DiskFinding::MarkedWithoutRow, 2)]);
    assert_eq!(f.row("shared/inner/x.bin").unwrap().onedrive_item, Some(reference("d2", "X")));
    assert_eq!(f.row("y.bin").unwrap().onedrive_item, Some(reference("d1", "Y")));
}

/// A folder a daemon left: `docs` (D) with the clean copy `z.txt` (A), and `zdir` (Z) with
/// the clean copy `in.txt` (I).
fn folder_before_the_rebuild() -> Folder {
    let mut f = folder();
    f.onedrive_directory("docs", "D");
    f.copy("docs/z.txt", b"aaa", "A");
    f.onedrive_directory("zdir", "Z");
    f.copy("zdir/in.txt", b"iii", "I");
    f
}

/// The store is rebuilt, the check runs, and a listing says what OneDrive has: what the
/// check found, with the plan asked that nothing is moved or removed in OneDrive.
fn check_after_rebuild(f: &mut Folder) -> Vec<(DiskFinding, usize)> {
    let onedrive = f.rebuild_store();
    let findings = f.check();
    assert_eq!(findings.unresolved(), []);
    f.write_listing(&onedrive);
    f.assert_plan_sends_no_removal_or_move("after a rebuilt store");
    found(&findings)
}

/// `cp -a docs/z.txt docs/a.txt`, and the store rebuilt before the next start.
/// The copy sorts first, was bound, and the plan sent a move of the item to the copy's name.
#[test]
fn an_id_a_file_and_its_copy_carry_binds_neither_and_both_are_new_without_their_marks() {
    let mut f = folder_before_the_rebuild();
    f.copy_with_marks("docs/z.txt", "docs/a.txt");
    assert_eq!(check_after_rebuild(&mut f), [(DiskFinding::Made, 2), (DiskFinding::MarkedWithoutRow, 3)]);
    for copy in ["docs/a.txt", "docs/z.txt"] {
        assert_eq!((f.row(copy).unwrap().onedrive_item, f.mark_of(copy), f.copy_marks_of(copy)), (None, None, (false, None)), "{copy}");
    }
    assert_eq!(f.row("zdir/in.txt").unwrap().onedrive_item, Some(reference("d1", "I")), "an id one entry carries binds it");
    f.assert_settled();
    // Rebuilt again: the marks are gone, and nothing is bound by them.
    assert_eq!(check_after_rebuild(&mut f), [(DiskFinding::Made, 2), (DiskFinding::MarkedWithoutRow, 3)]);
}

/// `ln docs/z.txt docs/a.txt` with the rows intact, and the store rebuilt
/// later. The file has one set of marks for both names: they are left as they are, and
/// while both names carry the id it binds neither, at this start and at every later one.
#[test]
fn an_id_two_names_of_one_file_carry_binds_neither_and_the_marks_of_the_file_are_left() {
    let mut f = folder_before_the_rebuild();
    f.link("docs/z.txt", "docs/a.txt");
    assert_eq!(found(&f.check()), [(DiskFinding::NothingChanged, 4), (DiskFinding::Made, 1)]);
    for _ in 0..2 {
        assert_eq!(check_after_rebuild(&mut f), [(DiskFinding::Made, 2), (DiskFinding::MarkedWithoutRow, 3)]);
        for name in ["docs/a.txt", "docs/z.txt"] {
            assert_eq!((f.row(name).unwrap().onedrive_item, f.mark_of(name).as_deref()), (None, Some("A")), "{name}");
        }
        f.assert_settled();
    }
}

/// `cp -a zdir adir`, rebuilt store. The whole directory of OneDrive was
/// renamed to the copy's name.
#[test]
fn an_id_a_directory_and_its_copy_carry_binds_neither_nor_what_is_in_them() {
    let mut f = folder_before_the_rebuild();
    f.make_directory("adir");
    f.mark_directory("adir", "Z");
    f.copy_with_marks("zdir/in.txt", "adir/in.txt");
    assert_eq!(check_after_rebuild(&mut f), [(DiskFinding::Made, 4), (DiskFinding::MarkedWithoutRow, 2)]);
    for copy in ["adir", "zdir", "adir/in.txt", "zdir/in.txt"] {
        assert_eq!((f.row(copy).unwrap().onedrive_item, f.mark_of(copy)), (None, None), "{copy}");
    }
    f.assert_settled();
}

/// An entry bound by its mark alone has nothing agreed on: where OneDrive has the item is
/// the listing's to say and the plan's to compare.
#[test]
fn a_row_bound_by_a_mark_is_bound_with_nothing_agreed_on() {
    let mut f = folder_before_the_rebuild();
    f.rename("docs/z.txt", "moved.txt");
    f.rebuild_store();
    assert_eq!(found(&f.check()), [(DiskFinding::MarkedWithoutRow, 4)]);
    let row = f.row("moved.txt").unwrap();
    assert_eq!((row.onedrive_item.clone(), row.synced_place.clone(), row.synced_content.clone(), row.onedrive.clone()), (Some(reference("d1", "A")), None, None, None));
}

/// The id is one a row has that the folder does not show: no row is bound to it again.
#[test]
fn an_entry_marked_with_an_id_a_row_the_folder_does_not_show_has_is_new() {
    let mut f = folder();
    let listed = f.store.item_store().make_onedrive_item(&reference("d1", "A"), ItemKind::File).unwrap();
    f.write("a.txt", b"aaa");
    f.mark_copy("a.txt", "A");
    let before = f.item(listed);
    assert_eq!(found(&f.check()), [(DiskFinding::Made, 1)]);
    assert_eq!((f.row("a.txt").unwrap().onedrive_item, f.mark_of("a.txt")), (None, None));
    assert_eq!(f.item(listed), before);
}

#[test]
fn a_same_size_same_second_edit_with_a_stale_mark_is_hashed_after_store_recovery() {
    let mut f = folder();
    f.copy("a.txt", b"aaa", "A");
    let listed = f.rebuild_store();
    // The size, whole second and cTag all still match the cloud. The bytes do not.
    f.write("a.txt", b"bbb");
    f.mark_copy("a.txt", "A");
    f.check();
    f.write_listing(&listed);
    let row = f.row("a.txt").unwrap();
    assert!(matches!(content(&row), Some(ContentVersion::ChangedHere { .. })));
    // The planner agrees on a recovered item's place before comparing its content.
    // Advance only that namespace agreement; content must remain changed-here.
    for _ in 0..3 {
        let plan = f.plan();
        for step in plan.steps {
            if let konedrive_tree::SyncStep::Row(konedrive_tree::RowStep::AgreeOnPlace { item }) = step {
                f.store.item_store().commit_agree_on_place(&item).unwrap();
            }
        }
    }
    let plan = f.plan();
    assert!(plan.steps.iter().any(|step| matches!(step, konedrive_tree::SyncStep::Row(konedrive_tree::RowStep::HashLocalContent { .. }))), "recovered bytes must be hashed before content agreement: {plan:?}");
    f.assert_plan_sends_no_removal_or_move("stale cTag after store recovery");
}

#[test]
fn missing_backing_content_does_not_plan_a_cloud_removal() {
    let mut f = folder();
    f.copy("a.txt", b"aaa", "A");
    std::fs::remove_file(f.disk("a.txt")).unwrap();
    f.check();
    f.assert_plan_sends_no_removal_or_move("missing backing bytes");
    let rows = f.rows();
    assert!(rows.iter().any(|row| row.onedrive_item.as_ref().is_some_and(|binding| binding.item_id.as_str() == "A")), "the cloud binding stays");
}
