use super::*;
use crate::mount::item_download_states::{DownloadAsker, ItemDownloadStart};
use crate::mount::item_fixture::ItemFixture;

const ROOT: LocalItemId = LocalItemId::ROOT;
fn states(f: &ItemFixture) -> Arc<DirectoryStates> {
    DirectoryStates::build(&f.store, f.download_states.clone(), f.running_steps.clone(), f.published_plan.clone(), f.inotify_bridge.clone()).unwrap()
}
fn pin(f: &ItemFixture, item: LocalItemId, pinned: bool) {
    f.write_rows(move |store| store.item_store().set_item_pin(item, pinned));
}
fn download(f: &ItemFixture, item: &Item) -> super::super::item_download_states::DownloadRun {
    let key = DownloadKey { item: item.local_item_id, content: item.local.as_ref().unwrap().content.clone().unwrap() };
    match f.download_states.start_or_join(&key, &DownloadAsker::Read, None) {
        ItemDownloadStart::Started { run, .. } => run,
        _ => panic!("a new download"),
    }
}
fn recount(f: &ItemFixture, states: &DirectoryStates) {
    let index = crate::panic::lock(&states.index);
    // Independent full recount, using the STORE's parent links and pin facts, never
    // the cached graph or counters. Catch stale rows as well as propagation mistakes.
    let rows = f.read_rows(|reader| {
        let mut result = Vec::new();
        let mut pending = vec![ROOT];
        while let Some(id) = pending.pop() {
            let item = reader.find_item(id)?.unwrap();
            if item.kind == ItemKind::Directory {
                pending.extend(reader.list_children(id)?.iter().map(|item| item.local_item_id));
            }
            result.push(Row {
                pinned: reader.is_pinned(id)?,
                ignored: reader.is_ignored(id)?,
                refused: item.unsent_since.is_some() && reader.find_item_memory(id)?.refusal.is_some(),
                item,
            });
        }
        Ok(result)
    });
    let mut expected: HashMap<LocalItemId, Counts> = HashMap::new();
    let by_id: HashMap<_, _> = rows.iter().map(|row| (row.item.local_item_id, row)).collect();
    for row in &rows {
        assert!(index
            .nodes
            .get(&row.item.local_item_id)
            .is_some_and(|node| node.row.as_ref().is_some_and(|cached| cached.item == row.item && cached.pinned == row.pinned)));
        let counts = states.contribution(row);
        let mut at = Some(row.item.local_item_id);
        while let Some(id) = at {
            let total = expected.entry(id).or_default();
            for i in 0..7 {
                total.0[i] += counts.0[i];
            }
            at = by_id[&id].item.local.as_ref().and_then(|local| local.place.as_ref()).map(|place| place.parent_local_item_id);
        }
    }
    for (id, total) in expected {
        assert_eq!(index.nodes[&id].total, total, "node {id}");
    }
}

#[test]
fn regression_fs2_subtree_row_reads_allow_xattr_readers_and_runtime_updates() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    let f = ItemFixture::new();
    let dir = f.show_onedrive_directory("docs").local_item_id;
    let files: Vec<_> = (0..128).map(|n| f.show_onedrive_file(&format!("docs/f{n}"), b"hello")).collect();
    let s = states(&f);
    let blocked = files[64].local_item_id;
    let file = files[0].clone();
    let pause = Arc::new(AtomicBool::new(false));
    let (entered, waiting) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let resumed = Mutex::new(resumed);
    let observer = s.clone();
    let should_pause = pause.clone();
    f.store.observe_items(move |reader, changes| {
        observer.refresh_rows_with(changes, |id| {
            if id == blocked && should_pause.swap(false, Ordering::SeqCst) {
                entered.send(()).unwrap();
                resumed.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
            }
            let Some(item) = reader.find_item(id)?.filter(|item| item.local.is_some()) else { return Ok(None) };
            Ok(Some(Row {
                pinned: reader.is_pinned(id)?,
                ignored: reader.is_ignored(id)?,
                refused: item.unsent_since.is_some() && reader.find_item_memory(id)?.refusal.is_some(),
                item,
            }))
        })
    }).unwrap();
    pause.store(true, Ordering::SeqCst);
    std::thread::scope(|scope| {
        let refresh = scope.spawn(|| pin(&f, dir, true));
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let (read, result) = mpsc::channel();
        let s = &s;
        let f = &f;
        scope.spawn(move || {
            let values = (s.directory_values(ROOT).0, s.file_state(file.local_item_id));
            // This update may affect a row already staged. Its new contribution must
            // survive the refresh's final apply, rather than restoring stale counters.
            let run = download(f, &file);
            f.download_states.finish(&run, Err(libc::EIO));
            read.send(values).unwrap();
        });
        let values = result.recv_timeout(Duration::from_secs(1));
        resume.send(()).unwrap();
        refresh.join().unwrap();
        assert!(values.is_ok(), "xattr readers and runtime updates must finish while subtree row I/O is paused");
        assert_eq!(values.unwrap(), (State::OnlineOnly, Some(State::OnlineOnly)));
    });
    assert_eq!(s.directory_values(ROOT).0, State::Failed);
    assert_eq!(s.file_state(files[1].local_item_id), Some(State::OnlineOnly));
    recount(&f, &s);
}

#[test]
fn regression_fs4_removing_effective_pins_discards_pin_only_failures_before_repinning() {
    use konedrive_tree::ItemVersions;

    let f = ItemFixture::new();
    let dir = f.show_onedrive_directory("docs").local_item_id;
    let elsewhere = f.show_onedrive_directory("elsewhere").local_item_id;
    let file = f.show_onedrive_file("docs/file", b"hello");
    let id = file.local_item_id;
    let s = states(&f);
    let key = DownloadKey { item: id, content: file.local.as_ref().unwrap().content.clone().unwrap() };
    for removal in 0..4 {
        let pinned = if removal == 0 { id } else { dir };
        pin(&f, pinned, true);
        let asker = DownloadAsker::PlanPin { planned_from: ItemVersions::of(&f.find_item(id).unwrap()) };
        let ItemDownloadStart::Started { run, .. } = f.download_states.start_or_join(&key, &asker, None) else { panic!("a new download") };
        f.download_states.finish(&run, Err(libc::EIO));
        assert_eq!(s.file_state(id), Some(State::Failed));
        match removal {
            0 | 1 => pin(&f, pinned, false),
            2 => pin(&f, id, false), // An exception to an inherited pin.
            _ => { f.move_here(id, "elsewhere/file"); }
        }
        assert_eq!(s.directory_values(ROOT).0, State::OnlineOnly);
        // No failure query of the unpinned file: the row refresh must clear it even
        // when no client happens to ask for the file's state between pin changes.
        assert!(!f.download_states.has_failed_download(&key, true));
        if removal == 3 {
            f.move_here(id, "docs/file");
        } else {
            pin(&f, pinned, true);
        }
        assert_eq!(s.file_state(id), Some(State::OnlineOnly), "repinning starts afresh");
        assert_eq!(s.directory_values(ROOT).0, State::Hydrating);
        pin(&f, pinned, false);
        recount(&f, &s);
    }
    assert_eq!(s.directory_values(elsewhere).0, State::Hydrated);
    pin(&f, dir, true);
    let run = download(&f, &file);
    f.download_states.finish(&run, Err(libc::EIO));
    pin(&f, dir, false);
    assert_eq!(s.file_state(id), Some(State::Failed), "unpin must preserve read failures");
    recount(&f, &s);
}

#[test]
fn precedence_and_empty_directory() {
    assert_eq!(Counts::default().values(), (State::Hydrated, None));
    let mut sum = Counts::default();
    for (state, sync) in [
        (State::Hydrated, None),
        (State::OnlineOnly, Some(SyncMark::Pending)),
        (State::Hydrating, Some(SyncMark::Uploading)),
        (State::Failed, Some(SyncMark::Blocked)),
    ] {
        let counts = Counts::of(state, sync);
        for i in 0..7 {
            sum.0[i] += counts.0[i];
        }
        assert_eq!(sum.values(), (state, sync));
    }
}

#[test]
fn incremental_pin_move_delete_failure_and_recovery() {
    let f = ItemFixture::new();
    let a = f.show_onedrive_directory("a").local_item_id;
    let b = f.show_onedrive_directory("b").local_item_id;
    let file = f.show_onedrive_file("a/file", b"hello");
    let s = states(&f);
    assert_eq!(s.directory_values(a), (State::OnlineOnly, None));
    assert_eq!(s.directory_values(b), (State::Hydrated, None));
    pin(&f, a, true);
    assert_eq!(s.directory_values(ROOT).0, State::Hydrating);
    pin(&f, a, false);
    assert_eq!(s.directory_values(ROOT).0, State::OnlineOnly);
    for _ in 0..1 {
        let run = download(&f, &file);
        assert_eq!(s.directory_values(a).0, State::Hydrating);
        f.download_states.finish(&run, Err(libc::EIO));
    }
    assert_eq!(s.file_state(file.local_item_id), Some(State::Failed));
    assert_eq!(s.directory_values(ROOT).0, State::Failed);
    let run = download(&f, &file);
    assert_eq!(s.directory_values(a).0, State::Hydrating);
    f.download_states.finish(&run, Ok(()));
    assert_eq!(s.file_state(file.local_item_id), Some(State::OnlineOnly));
    pin(&f, a, true);
    f.published_plan.set_downloads_held([file.local_item_id].into());
    assert_eq!(s.directory_values(a).0, State::Failed);
    pin(&f, a, false);
    assert_eq!(s.directory_values(a).0, State::OnlineOnly);
    f.published_plan.set_downloads_held(Default::default());
    f.move_here(file.local_item_id, "b/file");
    assert_eq!(s.directory_values(a), (State::Hydrated, None));
    assert_eq!(s.directory_values(b), (State::OnlineOnly, Some(SyncMark::Pending)));
    recount(&f, &s);
    f.remove_here(file.local_item_id);
    assert_eq!(s.directory_values(ROOT), (State::Hydrated, None));
    recount(&f, &s);
}

#[test]
fn moving_a_subtree_transfers_counts_and_recomputes_inherited_pins() {
    let f = ItemFixture::new();
    let a = f.show_onedrive_directory("a").local_item_id;
    let b = f.show_onedrive_directory("b").local_item_id;
    let sub = f.show_onedrive_directory("a/sub").local_item_id;
    f.show_onedrive_file("a/sub/file", b"hello");
    let s = states(&f);
    pin(&f, b, true);
    f.move_here(sub, "b/sub");
    assert_eq!(s.directory_values(a).0, State::Hydrated);
    assert_eq!(s.directory_values(b).0, State::Hydrating);
    f.move_here(sub, "a/sub");
    assert_eq!(s.directory_values(a).0, State::OnlineOnly);
    recount(&f, &s);
}

#[test]
fn randomized_operations_match_a_full_store_recount() {
    let f = ItemFixture::new();
    let mut dirs = vec![ROOT];
    for n in 0..5 {
        dirs.push(f.show_onedrive_directory(&format!("d{n}")).local_item_id);
    }
    let s = states(&f);
    let mut files = Vec::new();
    let mut random = 0x6e9f_510a_44d3_1287u64;
    for n in 0..400 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let dir = (random as usize >> 8) % dirs.len();
        let choice = random as usize % 9;
        if choice == 0 || files.is_empty() {
            let path = if dir == 0 { format!("f{n}") } else { format!("d{}/f{n}", dir - 1) };
            files.push(f.show_onedrive_file(&path, b"test"));
        } else {
            let at = (random as usize >> 16) % files.len();
            let id = files[at].local_item_id;
            match choice {
                1 => {
                    let path = if dir == 0 { format!("m{n}") } else { format!("d{}/m{n}", dir - 1) };
                    files[at] = f.move_here(id, &path);
                }
                2 => {
                    f.remove_here(id);
                    files.swap_remove(at);
                }
                3 => pin(&f, dirs[dir], random & 0x1000 != 0),
                4 => {
                    let run = download(&f, &files[at]);
                    f.download_states.finish(&run, Err(libc::EIO));
                }
                5 => {
                    let run = download(&f, &files[at]);
                    f.download_states.finish(&run, Ok(()));
                }
                6 => {
                    let content = files[at].local.as_ref().unwrap().content.clone().unwrap();
                    f.write_rows(move |store| store.item_store().commit_landed_content(id, &content, None));
                }
                7 => {
                    let content = files[at].local.as_ref().unwrap().content.clone().unwrap();
                    f.write_rows(move |store| store.item_store().commit_freed_content(id, &content));
                }
                _ => {
                    f.published_plan.set_downloads_held([id].into());
                }
            }
        }
        recount(&f, &s);
    }
}

#[test]
#[ignore = "100k-item startup and lookup measurement; run explicitly"]
fn a_hundred_thousand_files_are_indexed_once_and_queried_without_tree_walks() {
    let f = ItemFixture::new();
    f.write_rows(|store| {
        store.bench_sql(
            "BEGIN;
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000)
         INSERT INTO items(local_item_id, kind, onedrive_drive_id, onedrive_item_id,
             local_parent_id, local_name, local_folded_name, onedrive_parent_id, onedrive_name, onedrive_folded_name, synced_parent_id, synced_name)
         SELECT x+1, 'directory', 'd1', 'D'||x, 1, 'd'||x, 'd'||x, 1, 'd'||x, 'd'||x, 1, 'd'||x FROM n;
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000)
         INSERT INTO items(local_item_id, kind, onedrive_drive_id, onedrive_item_id,
             local_parent_id, local_name, local_folded_name, local_size, local_mtime, local_mtime_nsec, local_hash, local_ctag,
             onedrive_parent_id, onedrive_name, onedrive_folded_name, onedrive_size, onedrive_mtime, onedrive_mtime_nsec, onedrive_hash, onedrive_ctag,
             synced_parent_id, synced_name, synced_size, synced_mtime, synced_mtime_nsec, synced_hash, synced_ctag)
         SELECT x+1001, 'file', 'd1', 'F'||x,
             2+(x-1)/100, 'f'||x, 'f'||x, 4, 1, 0, 'hash', 'ctag',
             2+(x-1)/100, 'f'||x, 'f'||x, 4, 1, 0, 'hash', 'ctag',
             2+(x-1)/100, 'f'||x, 4, 1, 0, 'hash', 'ctag' FROM n;
         COMMIT;",
        )
    });
    let rss = || std::fs::read_to_string("/proc/self/status").unwrap().lines().find(|line| line.starts_with("VmRSS:")).unwrap().to_owned();
    eprintln!("before startup: {}", rss());
    let began = std::time::Instant::now();
    let s = states(&f);
    eprintln!("101001-row directory-state startup: {:?}; {}", began.elapsed(), rss());
    assert_eq!(s.directory_values(ROOT), (State::OnlineOnly, None));
    assert_eq!(crate::panic::lock(&s.index).nodes[&ROOT].total.0[2], 100_000);
    let began = std::time::Instant::now();
    for _ in 0..100_000 {
        assert_eq!(s.directory_values(ROOT).0, State::OnlineOnly);
    }
    eprintln!("100000 cached root queries: {:?}", began.elapsed());
    let item = f.find_item(LocalItemId::new(1002)).unwrap();
    let began = std::time::Instant::now();
    for _ in 0..1000 {
        let run = download(&f, &item);
        f.download_states.finish(&run, Ok(()));
    }
    eprintln!("1000 download start/end ancestor updates: {:?}", began.elapsed());
}

#[test]
fn upload_runtime_and_plan_changes_propagate_with_independent_precedence() {
    use crate::sync_parts::published_plan::ItemAnswer;
    use crate::sync_parts::running_steps::{RunningStep, StepClass};
    use konedrive_tree::{Reason, WaitReason};
    let f = ItemFixture::new();
    let dir = f.show_onedrive_directory("docs").local_item_id;
    let a = f.make_own_file("docs/a", b"local").local_item_id;
    let b = f.make_own_file("docs/b", b"local").local_item_id;
    let s = states(&f);
    assert_eq!(s.directory_values(dir), (State::Hydrated, Some(SyncMark::Pending)));
    f.running_steps.begin(RunningStep { class: StepClass::Content, item: a, other_items: vec![], destination: None, sent: 0, total: 5 });
    assert_eq!(s.directory_values(ROOT).1, Some(SyncMark::Uploading));
    f.published_plan.publish([(b, ItemAnswer::Waiting(WaitReason::User(Reason::NameCharacters)))].into());
    assert_eq!(s.directory_values(ROOT).1, Some(SyncMark::Blocked));
    f.published_plan.publish(Default::default());
    assert_eq!(s.directory_values(ROOT).1, Some(SyncMark::Uploading));
    f.running_steps.abort_requests();
    assert_eq!(s.directory_values(ROOT).1, Some(SyncMark::Pending));
    f.remove_here(a);
    f.remove_here(b);
    assert_eq!(s.directory_values(ROOT), (State::Hydrated, None));
    recount(&f, &s);
}

#[test]
fn a_batch_can_reverse_directory_ancestry_and_then_remove_a_subtree() {
    let f = ItemFixture::new();
    let a = f.show_onedrive_directory("a").local_item_id;
    let b = f.show_onedrive_directory("a/b").local_item_id;
    let file = f.show_onedrive_file("a/b/file", b"hello").local_item_id;
    let s = states(&f);
    f.write_rows(move |store| {
        // One committed writer job changes a/b to b/a; the feed must apply the
        // final graph without going through an a<->b cycle, in either row-id order.
        store.bench_sql(&format!("BEGIN; UPDATE items SET local_parent_id=1, local_name='b',local_folded_name='b' WHERE local_item_id={b}; UPDATE items SET local_parent_id={b} WHERE local_item_id={a}; COMMIT;"))
    });
    assert_eq!(s.directory_values(a), (State::Hydrated, None));
    recount(&f, &s);
    f.remove_here(file);
    f.remove_here(a);
    f.remove_here(b);
    assert_eq!(s.directory_values(ROOT), (State::Hydrated, None));
    recount(&f, &s);
}
