use super::*;
use crate::download_guard::{ProcessInfo, ProcessReader};
use crate::mount::item_fixture::ItemFixture;

struct FakeProcess(Mutex<Option<ProcessInfo>>);
impl ProcessReader for FakeProcess {
    fn read(&self, _pid: u32) -> Option<ProcessInfo> {
        self.0.lock().unwrap().clone()
    }
}

fn process(path: &Path) -> Arc<FakeProcess> {
    Arc::new(FakeProcess(Mutex::new(Some(ProcessInfo {
        parent: 0,
        start: 1,
        executable: "/usr/bin/viewer".into(),
        cgroup: "0::/app-org.example.viewer-123.scope".into(),
        cmdline: Arc::new(format!("viewer\0{}\0", path.display()).into_bytes()),
    }))))
}

#[test]
fn launch_intent_paths_are_bounded_lexical_and_confined_to_the_mount() {
    let args = b"viewer\0/mount/a.png\0file:///mount/b%20c.png\0/elsewhere/secret\0/mount-other/file\0/mount/../outside\0relative.png\0file://remote/mount/a.png\0/mount\0";
    assert_eq!(
        paths(args, Path::new("/mount")),
        [PathBuf::from("a.png"), PathBuf::from("b c.png")]
    );
    assert_eq!(
        paths(b"/mount/program\0", Path::new("/mount")),
        [PathBuf::from("program")],
        "argv[0] is an argument too"
    );
    let mut many = b"viewer\0".to_vec();
    for _ in 0..64 {
        many.extend_from_slice(b"ignored\0");
    }
    many.extend_from_slice(b"/mount/too-late.png\0");
    assert!(paths(&many, Path::new("/mount")).is_empty());
    let oversized = format!("viewer\0/mount/{}\0", "x".repeat(4096));
    assert!(paths(oversized.as_bytes(), Path::new("/mount")).is_empty());
}

#[test]
fn launch_intent_expires_emblems_and_rejects_dead_or_changed_processes() {
    let fixture = ItemFixture::new();
    let row = fixture.show_onedrive_file("a.png", b"tiny");
    let key = DownloadKey {
        item: row.local_item_id,
        content: row.local.unwrap().content.unwrap(),
    };
    let fake = process(&fixture.folder().join("a.png"));
    let mut guard = DownloadGuard::default();
    guard.set_process_reader(fake.clone());
    let guard = Arc::new(guard);
    let clock = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let intents = LaunchIntents::new(
        fixture.folder(),
        Arc::clone(&fixture.read_connections),
        guard,
        Arc::clone(&fixture.download_states),
        clock.handle().clone(),
    );
    clock.block_on(async {
        tokio::time::pause();
        intents.observe(42);
        assert!(fixture.download_states.is_hydrating(key.item));
        assert!(
            !fixture.download_states.is_downloading(key.item),
            "intent alone never downloads"
        );
        tokio::task::yield_now().await;
        tokio::time::advance(SHOW_FOR).await;
        tokio::task::yield_now().await;
        assert!(
            !fixture.download_states.is_hydrating(key.item),
            "no-read emblem clears after ten seconds"
        );
        assert!(
            intents.take(42, &key).is_some(),
            "intent still lasts thirty seconds"
        );
        intents.observe(42);
        assert!(
            intents.take(42, &key).is_none(),
            "same process argv is parsed only once"
        );
        fake.0.lock().unwrap().as_mut().unwrap().start += 1;
        intents.observe(42);
        assert!(fixture.download_states.is_hydrating(key.item));
        tokio::task::yield_now().await;
        tokio::time::advance(KEEP_FOR).await;
        tokio::task::yield_now().await;
        assert!(!fixture.download_states.is_hydrating(key.item));
        assert!(
            intents.take(42, &key).is_none(),
            "thirty-second expiry loses launch intent"
        );
        fake.0.lock().unwrap().as_mut().unwrap().start += 1;
        intents.observe(42);
        tokio::task::yield_now().await;
        *fake.0.lock().unwrap() = None;
        // Tokio's timer wheel rounds up to a millisecond. Cross its tick as well as
        // the one-second revalidation deadline, without a wall-clock sleep.
        tokio::time::advance(CHECK_PROCESS + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            !fixture.download_states.is_hydrating(key.item),
            "process exit clears the emblem"
        );
        assert!(
            intents.take(42, &key).is_none(),
            "a vanished process cannot consume intent"
        );
    });
    intents.stop();
}

#[test]
fn launch_intent_ignores_unknown_resident_directory_and_guarded_arguments() {
    let fixture = ItemFixture::new();
    let cloud = fixture.show_onedrive_file("cloud.png", b"cloud");
    let resident = fixture.show_onedrive_file("resident.png", b"resident");
    fixture.land_copy(resident.local_item_id, b"resident");
    let directory = fixture.show_onedrive_directory("directory");
    let fake = process(&fixture.folder().join("cloud.png"));
    fake.0.lock().unwrap().as_mut().unwrap().cmdline = Arc::new(
        format!(
            "viewer\0{}\0{}\0{}\0{}\0/never/open/this/path\0",
            fixture.folder().join("cloud.png").display(),
            fixture.folder().join("resident.png").display(),
            fixture.folder().join("directory").display(),
            fixture.folder().join("unknown.png").display()
        )
        .into_bytes(),
    );
    let mut guard = DownloadGuard::default();
    guard.set_process_reader(fake.clone());
    let intents = LaunchIntents::new(
        fixture.folder(),
        fixture.read_connections.clone(),
        Arc::new(guard),
        fixture.download_states.clone(),
        fixture.runtime(),
    );
    intents.observe(42);
    assert!(fixture.download_states.is_hydrating(cloud.local_item_id));
    assert!(!fixture.download_states.is_hydrating(resident.local_item_id));
    assert!(!fixture
        .download_states
        .is_hydrating(directory.local_item_id));
    let state = intents.state.lock().unwrap();
    assert_eq!(state.processes[&42].intents.len(), 1);
    drop(state);
    // An exec into a host identity cannot consume the ordinary desktop's intent.
    fake.0.lock().unwrap().as_mut().unwrap().cgroup = "0::/app-org.kde.konsole-123.scope".into();
    intents.observe(42);
    assert!(!fixture.download_states.is_hydrating(cloud.local_item_id));
    fake.0.lock().unwrap().as_mut().unwrap().cgroup = "0::/app-org.example.viewer-123.scope".into();
    intents.observe(42);
    assert!(fixture.download_states.is_hydrating(cloud.local_item_id));
    intents.stop();
    assert!(!fixture.download_states.is_hydrating(cloud.local_item_id));
    intents.observe(42);
    assert!(
        !fixture.download_states.is_hydrating(cloud.local_item_id),
        "stop cannot recreate intent"
    );
}

#[test]
fn launch_intent_playlist_peeks_revalidate_consumed_owners() {
    for exited in [false, true] {
        let fixture = ItemFixture::new();
        let a = fixture.show_onedrive_file_with_mime("A.mp4", b"a", Some("video/mp4"));
        let b = fixture.show_onedrive_file_with_mime("B.mp4", b"b", Some("video/mp4"));
        let a_key = DownloadKey {
            item: a.local_item_id,
            content: a.local.as_ref().unwrap().content.clone().unwrap(),
        };
        let b_key = DownloadKey {
            item: b.local_item_id,
            content: b.local.as_ref().unwrap().content.clone().unwrap(),
        };
        let fake = process(&fixture.folder().join("A.mp4"));
        let mut guard = DownloadGuard::default();
        guard.set_process_reader(fake.clone());
        let intents = LaunchIntents::new(
            fixture.folder(),
            fixture.read_connections.clone(),
            Arc::new(guard),
            fixture.download_states.clone(),
            fixture.runtime(),
        );
        intents.observe(42);
        assert!(intents.is_playlist_peek(42, &b_key, media_folder(&b)));
        assert!(intents.take(42, &a_key).is_some());
        assert!(intents.take(42, &a_key).is_none());
        assert!(
            intents.is_playlist_peek(42, &b_key, media_folder(&b)),
            "consumption retains media evidence"
        );
        assert!(
            !intents.is_playlist_peek(42, &a_key, media_folder(&a)),
            "consumed argv target stays exempt"
        );
        if exited {
            *fake.0.lock().unwrap() = None;
        } else {
            fake.0.lock().unwrap().as_mut().unwrap().start += 1;
        }
        assert!(
            !intents.is_playlist_peek(42, &b_key, media_folder(&b)),
            "exit or PID reuse loses consumed evidence immediately"
        );
        intents.stop();
    }
}
