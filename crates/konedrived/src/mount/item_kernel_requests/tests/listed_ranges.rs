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

/// The supplied Gwenview header/stride pattern, including repeated probes of its header.
/// Extra header probes bring this to the 22 reads observed in the journal.
fn screenshot_reads() -> Vec<(u64, u32)> {
    let mut reads = vec![(0, 4096); 3];
    reads.extend([(0, 33), (33, 4096)]);
    reads.extend((65536..491408).step_by(65536).map(|from| (from, 4096)));
    reads.push((487424, 3984));
    reads.extend(std::iter::repeat_n((0, 4096), 22 - reads.len()));
    reads
}

fn read_a_larger_screenshot(through_graph: bool, listed: bool) -> (Vec<GuardApp>, Vec<(String, String)>) {
    let size = 491408;
    let bytes: Vec<_> = (0..size).map(|at| (at * 7 + at / 65536) as u8).collect();
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("Screenshots");
    let file = fixture.show_onedrive_file("Screenshots/large.png", &bytes);
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let mut setup = fixture.mount_setup();
    if through_graph { setup.source = Arc::new(GraphSource::new(graph.client())); }
    let guard = Arc::new(DownloadGuard::new(true, GuardConfig::default()));
    setup.download_guard = Arc::clone(&guard);
    let (_mount, requests) = crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
    let pid = std::process::id();
    let opened = requests.open_to_read(file.local_item_id, Some(pid)).unwrap();
    graph.with(|cloud| cloud.log.clear());
    for (from, size) in screenshot_reads() {
        let ReadAnswer::Range(mut range) = requests.decide_read(opened.fh, from, size).unwrap() else { panic!("a range") };
        if !listed { range.source.listed = None; }
        let answer = fixture.runtime().block_on(requests.read_range(range, Some(pid))).unwrap();
        assert_eq!(answer, bytes[from as usize..from as usize + size as usize]);
    }
    assert!(!requests.download_states.is_downloading(file.local_item_id), "the whole block fetched does not count toward download_after");
    requests.answer_release(opened.fh);
    let asked = graph.with(|cloud| std::mem::take(&mut cloud.log));
    (guard.apps().into_iter().filter(|app| app.files > 0).collect(), asked)
}

#[test]
fn a_491408_byte_screenshot_costs_one_range_for_22_reads_and_the_guard_counts_only_requested_bytes() {
    let (counted, asked) = read_a_larger_screenshot(true, true);
    assert_eq!(screenshot_reads().len(), 22);
    let program_bytes = screenshot_reads().iter().map(|(_, size)| *size as u64).sum::<u64>();
    assert_eq!(asked.iter().filter(|(_, route)| route.starts_with("me/drive/items/Screenshots/children")).count(), 1);
    assert_eq!(asked.iter().filter(|(_, route)| route.starts_with("dl/")).count(), 1);
    assert_eq!(asked.len(), 2, "one listing and one range, every later read from memory: {asked:?}");
    assert_eq!(counted.iter().map(|app| (app.files, app.bytes)).collect::<Vec<_>>(), [(1, program_bytes)]);
    assert_eq!(counted, read_a_larger_screenshot(false, true).0, "aligned fetches preserve guard decisions and counters for the same reads");
    let (careful, asked) = read_a_larger_screenshot(true, false);
    assert_eq!(counted, careful);
    assert_eq!(asked.iter().filter(|(_, route)| route.starts_with("dl/")).count(), 1);
    assert_eq!(asked.len(), 3, "without listed metadata, one metadata/body/metadata exchange for all 22 reads: {asked:?}");
}

/// Repeated reads of a cached small file still cost every requested byte to the guard,
/// but cannot manufacture the distinct coverage needed to hydrate it.
#[test]
fn gwenview_repeated_small_screenshot_reads_stay_online_only_and_keep_guard_accounting() {
    let size = 20187;
    let bytes = vec![7; size];
    let fixture = ItemFixture::new();
    fixture.show_onedrive_directory("Screenshots");
    let file = fixture.show_onedrive_file("Screenshots/small.png", &bytes).local_item_id;
    let graph = fixture.runtime().block_on(fixture.start_fake_onedrive());
    let mut setup = fixture.mount_setup();
    setup.source = Arc::new(GraphSource::new(graph.client()));
    let guard = Arc::new(DownloadGuard::new(true, GuardConfig::default()));
    setup.download_guard = Arc::clone(&guard);
    let (_mount, requests) = crate::mount::item_mount::build_order::build(setup, fixture.runtime()).unwrap();
    let pid = std::process::id();
    let opened = requests.open_to_read(file, Some(pid)).unwrap();
    graph.with(|cloud| cloud.log.clear());
    let mut reads = vec![(0, 16384)];
    reads.extend([(0, 4096); 4]);
    reads.extend([(0, 33), (33, 4096), (0, 16384)]);
    let mut returned = 0;
    for (from, size) in reads {
        let ReadAnswer::Range(range) = requests.decide_read(opened.fh, from, size).unwrap() else { panic!("a range") };
        let answer = fixture.runtime().block_on(requests.read_range(range, Some(pid))).unwrap();
        assert_eq!(answer, bytes[from as usize..from as usize + size as usize]);
        returned += answer.len() as u64;
        assert!(!requests.download_states.is_downloading(file), "repeated header reads start no hydration");
        assert_eq!(fixture.find_item(file).unwrap().content_presence, ContentPresence::NotOnDisk);
    }
    requests.answer_release(opened.fh);
    let counted: Vec<_> = guard.apps().into_iter().filter(|app| app.files > 0).map(|app| (app.files, app.bytes)).collect();
    assert_eq!(counted, [(1, returned)]);
    assert!(returned > size as u64);
    let asked = graph.with(|cloud| std::mem::take(&mut cloud.log));
    assert_eq!(asked.len(), 2, "one listing and one small-file fetch: {asked:?}");
}
