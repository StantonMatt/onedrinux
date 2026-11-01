//! Playlist peeks skip launch/coverage landing; §5.5 version revalidation still lands.
use super::*;
use crate::download_guard::{DownloadGuard, ProcessInfo, ProcessReader};
use crate::mount::item_mount::{build_order, ItemMount};
use std::collections::HashMap;
use std::sync::Mutex;

const PID: u32 = 42;
const SIZE: usize = 200 * 1024;

struct Processes(Mutex<HashMap<u32, ProcessInfo>>);
impl ProcessReader for Processes {
    fn read(&self, pid: u32) -> Option<ProcessInfo> {
        self.0.lock().unwrap().get(&pid).cloned()
    }
}

struct Playlist {
    fixture: ItemFixture,
    mount: ItemMount,
    requests: ItemKernelRequests,
    files: [LocalItemId; 2],
    processes: Arc<Processes>,
}

impl Drop for Playlist {
    fn drop(&mut self) {
        self.mount.stop();
    }
}

impl Playlist {
    fn new(mime: Option<&str>, both: bool) -> Self {
        Self::with_options(mime, mime, both, 0)
    }

    fn with_options(a_mime: Option<&str>, b_mime: Option<&str>, both: bool, window: usize) -> Self {
        let fixture = ItemFixture::new();
        fixture.show_onedrive_directory("Media");
        let names = if a_mime == Some("image/jpeg") {
            ["A.jpg", "B.jpg"]
        } else {
            ["A.mp4", "B.mp4"]
        };
        let files = std::array::from_fn(|n| {
            fixture
                .show_onedrive_file_with_mime(
                    &format!("Media/{}", names[n]),
                    &vec![b'x'; SIZE],
                    [a_mime, b_mime][n],
                )
                .local_item_id
        });
        let argv = names
            .into_iter()
            .take(if both { 2 } else { 1 })
            .map(|name| format!("{}\0", fixture.folder().join("Media").join(name).display()))
            .collect::<String>();
        let app = if a_mime == Some("image/jpeg") {
            "gwenview"
        } else {
            "haruna"
        };
        let info = ProcessInfo {
            start: 1,
            parent: 0,
            executable: format!("/usr/bin/{app}"),
            cgroup: format!("0::/app-org.kde.{app}-123.scope"),
            cmdline: Arc::new(format!("{app}\0{argv}").into_bytes()),
        };
        let processes = Arc::new(Processes(Mutex::new(HashMap::from([(PID, info)]))));
        let mut guard = DownloadGuard::default();
        guard.set_process_reader(processes.clone());
        let mut setup = fixture.mount_setup();
        setup.download_guard = Arc::new(guard);
        // Also exercise the documented read_ahead=0 case: playlist intent is independent.
        setup.read_ahead = window;
        setup.download_after = 1024 * 1024;
        let (mount, requests) = build_order::build(setup, fixture.runtime()).unwrap();
        Self {
            fixture,
            mount,
            requests,
            files,
            processes,
        }
    }

    fn read(&self, n: usize, fh: Fh, from: u64, size: u32) {
        self.fixture
            .runtime()
            .block_on(self.read_as(n, fh, from, size, PID));
    }

    async fn read_as(&self, n: usize, fh: Fh, from: u64, size: u32, pid: u32) {
        let bytes = match self.requests.decide_read(fh, from, size).unwrap() {
            ReadAnswer::Range(range) => self.requests.read_range(range, Some(pid)).await.unwrap(),
            ReadAnswer::Bytes(bytes) => bytes,
            _ => panic!("range or landed bytes"),
        };
        assert_eq!(
            bytes,
            vec![b'x'; (SIZE - from as usize).min(size as usize)],
            "file {n}"
        );
    }

    fn assert_cloud(&self, n: usize) {
        assert!(
            !self.requests.download_states.is_downloading(self.files[n]),
            "file {n} must not start a whole download"
        );
        assert_eq!(
            self.fixture
                .find_item(self.files[n])
                .unwrap()
                .content_presence,
            ContentPresence::NotOnDisk
        );
    }

    fn wait_landed(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .fixture
            .find_item(self.files[n])
            .unwrap()
            .content_presence
            != ContentPresence::OnDisk
        {
            assert!(Instant::now() < deadline, "file {n} should land");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn playlist_peeks_complete_small_media_neighbour_stays_cloud_only() {
    for window in [0, 16] {
        let playlist = Playlist::with_options(Some("video/mp4"), Some("video/mp4"), false, window);
        let a = playlist
            .requests
            .open_to_read(playlist.files[0], Some(PID))
            .unwrap()
            .fh;
        playlist.read(0, a, 0, 1); // Consume A's launch intent first, as Haruna does.
        playlist.wait_landed(0);
        let b = playlist
            .requests
            .open_to_read(playlist.files[1], Some(PID))
            .unwrap()
            .fh;
        // Haruna's sequential 32 KiB neighbour reads cover this small file completely.
        for from in (0..SIZE).step_by(32768) {
            playlist.read(1, b, from as u64, 32768);
        }
        if playlist
            .requests
            .download_states
            .is_downloading(playlist.files[1])
        {
            playlist.wait_landed(1);
        }
        assert_eq!(
            playlist
                .fixture
                .find_item(playlist.files[1])
                .unwrap()
                .content_presence,
            ContentPresence::NotOnDisk
        );
        playlist.requests.answer_release(a);
        playlist.requests.answer_release(b);
    }
}

#[test]
fn playlist_peeks_after_window_uses_fresh_coverage_on_the_same_handle() {
    let playlist = Playlist::new(Some("video/mp4"), false);
    let clock = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    clock.block_on(async {
        tokio::time::pause();
        let b = playlist
            .requests
            .open_to_read(playlist.files[1], Some(PID))
            .unwrap()
            .fh;
        playlist.read_as(1, b, 0, SIZE as u32, PID).await;
        playlist.assert_cloud(1); // Intent has not yet been consumed by A.
        tokio::time::advance(Duration::from_secs(30)).await;
        playlist.read_as(1, b, 0, (SIZE / 2) as u32, PID).await;
        playlist.assert_cloud(1); // Neither item nor handle may retain the old peek bytes.
        playlist
            .read_as(1, b, (SIZE / 2) as u64, (SIZE / 2) as u32, PID)
            .await;
        playlist.requests.answer_release(b);
    });
    playlist.wait_landed(1);
}

#[test]
fn playlist_peeks_non_media_and_unknown_mime_keep_normal_landing() {
    for (a, b) in [
        (Some("image/jpeg"), Some("image/jpeg")),
        (Some("video/mp4"), None),
        (None, Some("video/mp4")),
        (Some("image/jpeg"), Some("video/mp4")),
        (Some("video/mp4"), Some("image/jpeg")),
    ] {
        let playlist = Playlist::with_options(a, b, false, 0);
        let b = playlist
            .requests
            .open_to_read(playlist.files[1], Some(PID))
            .unwrap()
            .fh;
        playlist.read(1, b, 0, SIZE as u32);
        playlist.wait_landed(1);
        playlist.requests.answer_release(b);
    }
}

#[test]
fn playlist_peeks_both_argv_files_land_at_the_first_read() {
    let playlist = Playlist::new(Some("video/mp4"), true);
    for n in 0..2 {
        let fh = playlist
            .requests
            .open_to_read(playlist.files[n], Some(PID))
            .unwrap()
            .fh;
        playlist.read(n, fh, 0, 1);
        playlist.wait_landed(n);
        playlist.requests.answer_release(fh);
    }
}

#[test]
fn playlist_peeks_other_identity_and_other_folder_keep_normal_landing() {
    for other_app in [false, true] {
        let playlist = Playlist::new(Some("video/mp4"), false);
        playlist.requests.observe_process(PID);
        let reader = if other_app {
            let mut processes = playlist.processes.0.lock().unwrap();
            let mut other = processes[&PID].clone();
            other.cgroup = "0::/app-org.example.player-456.scope".into();
            other.cmdline = Arc::new(b"player\0".to_vec());
            processes.insert(PID + 1, other);
            PID + 1
        } else {
            playlist.fixture.show_onedrive_directory("Elsewhere");
            playlist
                .fixture
                .move_here(playlist.files[1], "Elsewhere/B.mp4");
            PID
        };
        let b = playlist
            .requests
            .open_to_read(playlist.files[1], Some(reader))
            .unwrap()
            .fh;
        playlist
            .fixture
            .runtime()
            .block_on(playlist.read_as(1, b, 0, SIZE as u32, reader));
        playlist.wait_landed(1);
        playlist.requests.answer_release(b);
    }
}

#[test]
fn playlist_peeks_audio_and_same_identity_processes_share_the_window() {
    let playlist = Playlist::with_options(Some("audio/ogg"), Some("video/mp4"), false, 0);
    playlist.requests.observe_process(PID);
    {
        let mut processes = playlist.processes.0.lock().unwrap();
        let mut sibling = processes[&PID].clone();
        sibling.cmdline = Arc::new(b"haruna\0".to_vec());
        processes.insert(PID + 1, sibling);
    }
    let b = playlist
        .requests
        .open_to_read(playlist.files[1], Some(PID + 1))
        .unwrap()
        .fh;
    playlist
        .fixture
        .runtime()
        .block_on(playlist.read_as(1, b, 0, SIZE as u32, PID + 1));
    playlist.assert_cloud(1);
    playlist.requests.answer_release(b);
}

/// Playlist suppression does not bypass §5.5: a previously refused content version
/// must be established by a whole download, which lands before answering the read.
#[test]
fn playlist_peeks_version_revalidation_preserves_the_whole_landing_exception() {
    use crate::mount::item_download_states::{DownloadAsker, ItemDownloadStart};
    let playlist = Playlist::new(Some("video/mp4"), false);
    let b = playlist
        .requests
        .open_to_read(playlist.files[1], Some(PID))
        .unwrap()
        .fh;
    let row = playlist.fixture.find_item(playlist.files[1]).unwrap();
    let key = DownloadKey {
        item: row.local_item_id,
        content: local_content(&row),
    };
    assert!(playlist
        .requests
        .launch_intents
        .as_ref()
        .unwrap()
        .is_playlist_peek(PID, &key, crate::mount::launch_intent::media_folder(&row),));
    // Model the retained fact left by a previous download's version refusal.
    // The existing a_read_of_a_content_onedrive_answered_with_another_once_asks_anew_and_succeeds
    // test covers the actual source refusal and subsequent accepted answer.
    let ItemDownloadStart::Started { run, .. } =
        playlist
            .requests
            .download_states
            .start_or_join(&key, &DownloadAsker::Read, None)
    else {
        panic!("no download is running");
    };
    playlist
        .requests
        .download_states
        .finish_as_another_content(&run);
    let ReadAnswer::WholeDownload(range) = playlist.requests.decide_read(b, 0, 1).unwrap() else {
        panic!("documented version revalidation requires a whole download");
    };
    assert_eq!(
        playlist
            .fixture
            .runtime()
            .block_on(playlist.requests.read_after_download(range, Some(PID)))
            .unwrap(),
        b"x"
    );
    assert_eq!(
        playlist
            .fixture
            .find_item(playlist.files[1])
            .unwrap()
            .content_presence,
        ContentPresence::OnDisk,
        "§5.5 version revalidation must still land a playlist neighbour before returning its range"
    );
    playlist.requests.answer_release(b);
}
