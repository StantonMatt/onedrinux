//! Range reads of files not on disk through the Graph source, over the fake OneDrive: what
//! they ask of OneDrive, and what the download guard counts of them.

use super::*;
use crate::content_source::graph_source::GraphSource;
use crate::download_guard::{DownloadGuard, GuardConfig};
use konedrive_dbus::rows::GuardApp;

const SHOTS: usize = 32;
const SHOT_SIZE: usize = 20 * 1024;

fn shot(n: usize) -> Vec<u8> {
    (0..SHOT_SIZE).map(|at| (at * 7 + n * 13) as u8).collect()
}

/// A folder of [`SHOTS`] small files not on disk, read as an image viewer reads them: the
/// first 16 KiB of each, one after the other, then a little more of each. Through the Graph
/// source over the fake OneDrive, or through the fixture's own source. What the download
/// guard counted for this process, and the requests OneDrive was asked during the reads
/// (none for the fixture's source).
fn read_the_heads_of_a_folders_files(through_graph: bool) -> (Vec<GuardApp>, Vec<(String, String)>) {
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("Screenshots");
    let files: Vec<LocalItemId> = (0..SHOTS).map(|n| fixture.show_onedrive_file(&format!("Screenshots/{n}.png"), &shot(n)).local_item_id).collect();
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let mut setup = fixture.mount_setup();
    if through_graph {
        setup.source = Arc::new(GraphSource::new(graph.client()));
    }
    // A slot for each file: each lingers after its read (`range_readers::READ_LINGERS`),
    // and the fixture's pool of four would make the reads wait for one another.
    setup.pool = konedrive_graph::pool::TransferPool::new(2 * SHOTS);
    let guard = Arc::new(DownloadGuard::new(true, GuardConfig { max_files: 2 * SHOTS as u32, ..Default::default() }));
    setup.download_guard = Arc::clone(&guard);
    let (_mount, requests) = crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
    graph.with(|cloud| cloud.log.clear());
    let pid = std::process::id();
    let read = |n: usize, from: u64, end: u64| {
        let opened = requests.open_to_read(files[n], Some(pid)).unwrap();
        let ReadAnswer::Range(range) = requests.decide_read(opened.fh, from, (end - from) as u32).unwrap() else { panic!("a range of a file not on disk") };
        let bytes = fixture.runtime().block_on(requests.read_range(range, Some(pid))).unwrap();
        requests.answer_release(opened.fh);
        assert_eq!(bytes, shot(n)[from as usize..end as usize]);
    };
    for n in 0..SHOTS {
        read(n, 0, 16384);
    }
    for n in 0..SHOTS {
        read(n, 16384, 18000);
    }
    let asked = graph.with(|cloud| std::mem::take(&mut cloud.log));
    (guard.apps().into_iter().filter(|app| app.files > 0).collect(), asked)
}

/// One listing of the folder and one request a file, the rest of each file from memory;
/// and the download guard counts every file and every byte the program read exactly as it
/// counts them through a source that asks OneDrive for each range.
#[test]
fn a_folders_small_files_cost_one_listing_and_one_request_each_and_the_guard_counts_every_read() {
    let (counted, asked) = read_the_heads_of_a_folders_files(true);
    let listings = asked.iter().filter(|(_, route)| route.starts_with("me/drive/items/Screenshots/children")).count();
    let downloads = asked.iter().filter(|(_, route)| route.starts_with("dl/")).count();
    assert_eq!((listings, downloads, asked.len()), (1, SHOTS, SHOTS + 1), "no metadata, and nothing for the second reads: {asked:?}");
    assert_eq!(counted.iter().map(|app| (app.files, app.bytes)).collect::<Vec<_>>(), [(SHOTS as u64, (SHOTS * 18000) as u64)], "every byte read, also those from memory");
    let (counted_per_range, _) = read_the_heads_of_a_folders_files(false);
    assert_eq!(counted, counted_per_range, "the guard counts the same");
}
