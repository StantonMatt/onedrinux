use super::*;

#[test]
fn write_scope_keeps_an_unlinked_writers_admission_but_rechecks_named_files() {
    use crate::config::{WriteScope, WriteScopeSource};
    use crate::mount::upload_rules::UploadRules;
    let mut fixture = ItemFixture::new();
    fixture.make_own_directory("Trial");
    fixture.make_own_directory("Outside");
    let unlinked = fixture.make_own_file("Trial/unlinked", b"old bytes").local_item_id;
    let named = fixture.make_own_file("Trial/named", b"unchanged").local_item_id;
    let shared = Arc::new(std::sync::RwLock::new(WriteScope::parse(Some(&["Trial".into()])).unwrap()));
    let source = {
        let shared = Arc::clone(&shared);
        WriteScopeSource::new(move || shared.read().unwrap().clone())
    };
    fixture.upload_rules = Arc::new(UploadRules::new(Arc::clone(&fixture.ignore)).with_write_scope(source));
    let bridge = Arc::clone(&fixture.inotify_bridge);
    let mount = Requests::over(fixture, bridge);
    let opened = mount.open_to_write(unlinked).unwrap();
    let still_named = mount.open_to_write(named).unwrap();
    mount.remove("Trial/unlinked", ItemKind::File).unwrap();
    assert_eq!(mount.write(opened.fh, 0, b"new bytes"), Ok(9));
    let (_, backing_file) = mount.fixture.item_handles.find_file_to_write(opened.fh).unwrap();
    let mut bytes = [0; 9];
    assert_eq!(backing_file.read_at(&mut bytes, 0).unwrap(), 9);
    assert_eq!(&bytes, b"new bytes");
    assert!(mount.fixture.find_item_at("Trial/unlinked").is_none());
    *shared.write().unwrap() = WriteScope::parse(Some(&["Outside".into()])).unwrap();
    assert_eq!(mount.write(still_named.fh, 0, b"bad"), Err(Errno::EROFS));
    assert_eq!(mount.fixture.read_disk_file("Trial/named").unwrap(), b"unchanged");
    mount.release(opened.fh);
    mount.release(still_named.fh);
}

/// All supported mutations use the same dispatch admission. Restricting an already
/// open writer also refuses subsequent writes, before bytes or rows change.
#[test]
fn write_scope_refuses_every_mutating_dispatch_and_checks_both_rename_ends() {
    use crate::config::{WriteScope, WriteScopeSource};
    use crate::mount::upload_rules::UploadRules;
    let mut fixture = ItemFixture::new();
    fixture.make_own_directory("Trial");
    fixture.show_onedrive_directory("Outside");
    fixture.make_own_directory("Outside/empty");
    let outside = fixture.make_own_file("Outside/a", b"unchanged").local_item_id;
    let online = fixture.show_onedrive_file("Outside/cloud", b"do not download").local_item_id;
    let shared = Arc::new(std::sync::RwLock::new(WriteScope::default()));
    let source = {
        let shared = Arc::clone(&shared);
        WriteScopeSource::new(move || shared.read().unwrap().clone())
    };
    fixture.upload_rules = Arc::new(UploadRules::new(Arc::clone(&fixture.ignore)).with_write_scope(source));
    let bridge = Arc::clone(&fixture.inotify_bridge);
    let mount = Requests::over(fixture, bridge);
    let opened = mount.open_to_write(outside).unwrap();
    *shared.write().unwrap() = WriteScope::parse(Some(&["Trial".into()])).unwrap();
    let denied = Some(Errno::EROFS);
    for kind in [MadeKind::Directory, MadeKind::File, MadeKind::FileOpenedForWriting] {
        assert_eq!(mount.make("Outside/new", kind).err(), denied);
        assert_eq!(mount.make("new", kind).err(), denied);
    }
    assert_eq!(mount.remove("Outside/a", ItemKind::File).err(), denied);
    assert_eq!(mount.remove("Outside/empty", ItemKind::Directory).err(), denied);
    assert_eq!(mount.rename("Outside/a", "Trial/a").err(), denied, "source outside");
    mount.make("Trial/a", MadeKind::File).unwrap();
    let inside = mount.id("Trial/a");
    assert_eq!(mount.rename("Trial/a", "Outside/new").err(), denied, "destination outside");
    assert_eq!(mount.open_to_write(online).err(), denied, "no hydration outside");
    assert_eq!(mount.open_to_write(outside).err(), denied);
    assert_eq!(
        mount.queue(move |requests, reply| requests.queue_open_to_overwrite(outside, A_PROGRAM, reply)).err(),
        denied
    );
    assert_eq!(mount.truncate(outside, Some(opened.fh), 0).err(), denied);
    assert_eq!(mount.set_time_as(outside, None, 4, A_PROGRAM).err(), denied);
    assert_eq!(mount.set_executable_bit(outside, true).err(), denied);
    assert_eq!(mount.write(opened.fh, 0, b"bad").err(), denied);
    mount.release(opened.fh);
    assert_eq!(mount.fixture.read_disk_file("Outside/a").unwrap(), b"unchanged");
    assert!(mount.list_fetched_ranges().is_empty());
    let opened = mount.open_to_write(inside).unwrap();
    assert_eq!(mount.write(opened.fh, 0, b"allowed"), Ok(7));
    mount.release(opened.fh);
    mount.truncate(inside, None, 3).unwrap();
    mount.set_time_as(inside, None, 42, A_PROGRAM).unwrap();
    mount.set_executable_bit(inside, true).unwrap();
    mount.make("Trial/dir", MadeKind::Directory).unwrap();
    mount.rename("Trial/a", "Trial/dir/moved").unwrap();
    mount.remove("Trial/dir/moved", ItemKind::File).unwrap();
    mount.remove("Trial/dir", ItemKind::Directory).unwrap();
    // A request queued under the old scope must observe a restriction set before it runs.
    let held = mount.fixture.tree_lock.blocking_lock();
    let (tell, told) = mpsc::channel();
    let trial = mount.id("Trial");
    mount.ask(move |requests| {
        requests.queue_make(
            trial,
            OsStr::new("queued"),
            MadeKind::File,
            false,
            A_PROGRAM,
            Box::new(move |answer| drop(tell.send(answer))),
        )
    });
    *shared.write().unwrap() = WriteScope::denied();
    drop(held);
    assert_eq!(told.recv_timeout(Duration::from_secs(20)).unwrap().err(), denied);
    assert!(mount.fixture.find_item_at("Trial/queued").is_none());
}

#[test]
fn write_scope_reports_the_refused_endpoint_existing_folder_and_caller() {
    use crate::config::{WriteScope, WriteScopeSource};
    use crate::mount::upload_rules::UploadRules;
    for (operation, path, folder) in [
        ("create", "Outside/new", "Outside"),
        ("top", "new", ""),
        ("file", "Outside/a", "Outside"),
        ("directory", "Outside/empty", "Outside/empty"),
        ("remove-directory", "Outside/empty", "Outside"),
        ("rename-source", "Outside/a", "Outside"),
        ("rename-target", "Outside/new", "Outside"),
    ] {
        let mut fixture = ItemFixture::new();
        fixture.make_own_directory("Trial");
        fixture.make_own_directory("Outside");
        fixture.make_own_directory("Outside/empty");
        fixture.make_own_file("Outside/a", b"unchanged");
        fixture.make_own_file("Trial/a", b"inside");
        let source = WriteScopeSource::new(|| WriteScope::parse(Some(&["Trial".into()])).unwrap());
        let mut reports = source.refusals.take_receiver().unwrap();
        fixture.upload_rules = Arc::new(UploadRules::new(Arc::clone(&fixture.ignore)).with_write_scope(source));
        let bridge = Arc::clone(&fixture.inotify_bridge);
        let mount = Requests::over(fixture, bridge);
        let pid = std::process::id();
        let answer = match operation {
            "create" | "top" => mount.make_as(path, MadeKind::FileOpenedForWriting, pid).map(drop),
            "file" => {
                let item = mount.id(path);
                mount.ask(move |requests| requests.decide_open_to_write(item, pid)).map(drop)
            },
            "directory" => mount.set_time_as(mount.id(path), None, 4, pid).map(drop),
            "remove-directory" => mount.remove_as(path, ItemKind::Directory, pid),
            "rename-source" | "rename-target" => {
                let (from, to) = if operation == "rename-source" { ("Outside/a", "Trial/new") } else { ("Trial/a", "Outside/new") };
                let ((directory, name), (new_directory, new_name)) = (split(from), split(to));
                let (directory, name, new_directory, new_name) = (mount.id(directory), OsString::from(name), mount.id(new_directory), OsString::from(new_name));
                mount.queue(move |requests, reply| requests.queue_rename(directory, &name, new_directory, &new_name, RenameMode::Replace, pid, reply))
            }
            _ => unreachable!(),
        };
        assert_eq!(answer, Err(Errno::EROFS), "{operation}");
        let (report, count) = mount.fixture.runtime().block_on(reports.next()).unwrap();
        assert_eq!(report.path, std::path::PathBuf::from(path), "{operation}");
        assert_eq!(report.folder, std::path::PathBuf::from(folder), "{operation}");
        assert_eq!(report.pid, pid, "{operation}");
        assert_eq!(count, 1);
    }
}
