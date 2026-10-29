//! The order the parts of a mount over the rows are built in: the one place where a part
//! of the mount is made, each by its `new` with its final list of parameters.
//!
//! 1. a stale mount taken off the folder (`stale_mount`); `BackingDirectory::open`; the
//!    write bits of everything in it given back (`BackingDirectory::restore_write_bits`):
//!    the check at start renames entries and writes marks, and the parts make and remove
//!    what is in it (a build that fails after this takes them off again);
//!    `UploadRules::new(ignore).with_write_scope(write_scope)`; the time the mount starts at, asked of the setup's
//!    clock;
//! 2. `AccessModeSwitch::new(ReadOnly)`; `MountTakeDown::new(switch, report)`;
//!    `WriteOperationThread::start(tree_lock)`; `PlaceChanges::new` with that thread's
//!    id (`WriteOperationThread::thread_id`);
//! 3. THE CHECK AT START (`start_check::check_backing_directory_at_start`), over the
//!    writer, before anything else reads the rows: it undoes or finishes, from the
//!    record the store keeps, a change of a name the daemon died in; then the flags of
//!    ignored names gone over by the list as it is now (`rewrite_ignored_names`: defaults
//!    an upgrade changed), and the change guard's restore receipts settled
//!    (`reconcile_restores`) and its held uploads of rows now left here retired
//!    (`retire_left_here_uploads`); and only AFTER the check
//!    `PlaceChanges::remove_aside_entries`: what is still set aside then goes (before
//!    it, the entry the check is to put back would be deleted);
//! 4. `ReadConnections::new(store_path)`; `ItemHandles::new`;
//!    `ItemDownloadStates::new(download_after)`;
//! 5. `ItemInotifyBridge::new` (its queue; its thread starts when the mount is served);
//!    `RangeReaders` and `ItemRangeFetcher::new`;
//!    `ItemDownloadLanding::new`; `ItemDownloader::new`;
//! 6. `ItemFreeUp::new`; `ItemContentFiles::new`; `ItemLookup::new`;
//!    `ItemWriteOperations::new`; the parts of the folder steps (`FolderStepParts`);
//!    `ItemFilesInterface::new`;
//! 7. `ItemKernelRequests::new`, which the `Filesystem` of `kernel_requests` is made over.
//!
//! Nothing here mounts: what is built answers a step, the `Files` interface and a write
//! operation without a mount over the folder.
//!
//! **State owned:** none.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use super::super::access_mode_switch::{AccessMode, AccessModeSwitch};
use super::super::backing_directory::{remove_write_bits, BackingDirectory};
use super::super::item_content_files::ItemContentFiles;
use super::super::item_download_landing::ItemDownloadLanding;
use super::super::item_download_states::ItemDownloadStates;
use super::super::item_downloader::ItemDownloader;
use super::super::item_files_interface::ItemFilesInterface;
use super::super::item_free_up::ItemFreeUp;
use super::super::item_handles::ItemHandles;
use super::super::item_inotify_bridge::ItemInotifyBridge;
use super::super::item_kernel_requests::ItemKernelRequests;
use super::super::item_lookup::ItemLookup;
use super::super::item_range_fetcher::ItemRangeFetcher;
use super::super::item_write_operations::ItemWriteOperations;
use super::super::mount_take_down::MountTakeDown;
use super::super::place_changes::PlaceChanges;
use super::super::range_readers::RangeReaders;
use super::super::read_connections::ReadConnections;
use super::super::stale_mount::unmount_stale_mount;
use super::super::start_check::check_backing_directory_at_start;
use super::super::upload_rules::UploadRules;
use super::super::write_operation_thread::WriteOperationThread;
use super::{FolderStepParts, ItemMount, ItemMountError, ItemMountSetup};

/// The parts a stop of the mount reaches, in the order it stops them.
pub(super) struct PartsToStop {
    pub(super) write_operation_thread: Arc<WriteOperationThread>,
    pub(super) inotify_bridge: Arc<ItemInotifyBridge>,
    pub(super) downloader: Arc<ItemDownloader>,
    /// What a test of the stop looks at: the downloader aborts what it holds.
    #[cfg(test)]
    pub(super) download_states: Arc<ItemDownloadStates>,
    pub(super) read_connections: Arc<ReadConnections>,
}

/// Builds the parts in the order of the module's doc and answers the mount, not over the
/// folder, with what answers the kernel. Blocking (the check at start walks the backing
/// directory): called by `ItemMount::build` off the runtime, and by the fixture. A build
/// that fails leaves the folder on disk without its write bits, as it was.
pub(in crate::mount) fn build(setup: ItemMountSetup, runtime: tokio::runtime::Handle) -> Result<(ItemMount, ItemKernelRequests), ItemMountError> {
    // A mount a daemon that died left on the folder goes first: nothing opens under it.
    unmount_stale_mount(&setup.root.path);
    let backing_directory = Arc::new(BackingDirectory::open(&setup.root.path)?);
    backing_directory.restore_write_bits();
    build_over(Arc::clone(&backing_directory), setup, runtime).inspect_err(|_| remove_write_bits(&backing_directory.resolve_root_directory_path()))
}

/// Steps 1 to 7 over a backing directory that is open and has its write bits.
fn build_over(backing_directory: Arc<BackingDirectory>, setup: ItemMountSetup, runtime: tokio::runtime::Handle) -> Result<(ItemMount, ItemKernelRequests), ItemMountError> {
    let ItemMountSetup { root, store_path, store, drive_id, source, report, pool, split_download_share, download_after, download_guard, change_guard, free_space_reserve, tree_lock, ignore, write_scope, sync_plan_wake, running_steps, published_plan, take_down_report, clock } = setup;
    let folder = root.path;
    let mount_started_at = clock();
    let upload_rules = Arc::new(UploadRules::new(Arc::clone(&ignore)).with_write_scope(write_scope));

    let access_mode_switch = Arc::new(AccessModeSwitch::new(AccessMode::ReadOnly));
    let mount_take_down = Arc::new(MountTakeDown::new(Arc::clone(&access_mode_switch), take_down_report));
    let write_operation_thread = Arc::new(WriteOperationThread::start(tree_lock)?);
    let mut place_changes = PlaceChanges::new(Arc::clone(&backing_directory), write_operation_thread.thread_id());
    place_changes.change_guard = change_guard.clone();
    if let Some(guard) = &change_guard {
        let wake = Arc::clone(&sync_plan_wake);
        guard.attach(Arc::new(move || wake.wake(crate::sync_parts::sync_plan_wake::WakeCause::LocalVersionWritten)));
    }
    let place_changes = Arc::new(place_changes);

    // Before anything else reads the rows: afterwards they say what the disk has.
    let start_check_findings = check_backing_directory_at_start(Arc::clone(&backing_directory), Arc::clone(&upload_rules), drive_id, &store)?;
    // Defaults can change on upgrade even when config still says `ignore: None`.
    // Refresh only row flags: bindings, pins, paths and local bytes stay as they are.
    // One read of the shared list for the whole scan, not one per row.
    let current_list = crate::panic::read(&ignore).clone();
    store.call_blocking(move |s| s.item_store().rewrite_ignored_names(&|name| current_list.matches(name)))?;
    if let Some(guard) = &change_guard {
        let guard = Arc::clone(guard);
        store.call_blocking(move |s| {
            let item_store = s.item_store();
            let items = item_store.reader();
            guard.reconcile_restores(&items)?;
            guard.retire_left_here_uploads(&items)
        })?;
    }
    // Only now: before the check, the entry a record puts back would be deleted.
    let removed = place_changes.remove_aside_entries()?;
    if removed > 0 {
        tracing::info!("{removed} entries of {} that were still set aside are removed", folder.display());
    }

    let read_connections = Arc::new(ReadConnections::new(store_path.clone()));
    let item_handles = Arc::new(ItemHandles::new());
    let download_states = Arc::new(ItemDownloadStates::new(download_after));

    let inotify_bridge = Arc::new(ItemInotifyBridge::new(folder.clone()));
    let range_readers = Arc::new(RangeReaders::new(Arc::clone(&pool), runtime.clone()));
    let range_fetcher = Arc::new(ItemRangeFetcher::new(range_readers, Arc::clone(&source), report.throttle_wait.clone()));
    let download_landing = Arc::new(ItemDownloadLanding::new(Arc::clone(&place_changes), store.clone(), Arc::clone(&running_steps), Arc::clone(&mount_take_down), Arc::clone(&item_handles))?);
    let downloader = Arc::new(ItemDownloader::new(
        Arc::clone(&backing_directory),
        Arc::clone(&download_states),
        download_landing,
        Arc::clone(&inotify_bridge),
        Arc::clone(&write_operation_thread),
        source,
        pool,
        split_download_share,
        report.clone(),
        folder.clone(),
        runtime.clone(),
    ).with_read_reserve(Arc::clone(&download_guard.disks), free_space_reserve)
        .with_app_downloads(download_guard.downloads.account(&store_path)));

    // An unread part can contain a unique copy. Preserve all copies until a clean
    // restart; published policy and recovery holds also protect otherwise clean rows.
    let uncertain = start_check_findings.has_unread_part || start_check_findings.unresolved_path_count > 0;
    let free_up_plan = Arc::clone(&published_plan);
    let free_up = Arc::new(ItemFreeUp::new(Arc::clone(&place_changes), store.clone(), Arc::clone(&item_handles), Arc::clone(&download_states), Arc::clone(&running_steps), Arc::clone(&inotify_bridge))
        .with_protection(Arc::new(move |item| uncertain || matches!(free_up_plan.find_answer(item),
            Some(crate::sync_parts::published_plan::ItemAnswer::Waiting(_)
                | crate::sync_parts::published_plan::ItemAnswer::Fault(_))))));
    let item_content_files = Arc::new(ItemContentFiles::new(Arc::clone(&place_changes), store_path, Arc::clone(&item_handles)));
    item_content_files.retry_guard_removals()?;
    let directory_states = super::super::directory_states::DirectoryStates::build(
        &store, Arc::clone(&download_states), Arc::clone(&running_steps), Arc::clone(&published_plan), Arc::clone(&inotify_bridge),
    )?;
    let lookup = Arc::new(ItemLookup::new(
        Arc::clone(&read_connections),
        Arc::clone(&place_changes),
        Arc::clone(&item_handles),
        Arc::clone(&download_states),
        Arc::clone(&access_mode_switch),
        Arc::clone(&running_steps),
        published_plan,
        Arc::clone(&backing_directory),
        mount_started_at,
    ).with_directory_states(directory_states));
    let write_operations = Arc::new(ItemWriteOperations::new(
        Arc::clone(&place_changes),
        Arc::clone(&backing_directory),
        store.clone(),
        Arc::clone(&read_connections),
        Arc::clone(&item_handles),
        Arc::clone(&download_states),
        Arc::clone(&upload_rules),
        Arc::clone(&access_mode_switch),
        Arc::clone(&running_steps),
        Arc::clone(&sync_plan_wake),
        Arc::clone(&mount_take_down),
        report.clone(),
        folder.clone(),
    ));
    let folder_step_parts = FolderStepParts {
        place_changes: Arc::clone(&place_changes),
        store: store.clone(),
        write_operation_thread: Arc::clone(&write_operation_thread),
        upload_rules: Arc::clone(&upload_rules),
        item_handles: Arc::clone(&item_handles),
        inotify_bridge: Arc::clone(&inotify_bridge),
        downloader: Arc::clone(&downloader),
        download_states: Arc::clone(&download_states),
        range_fetcher: Arc::clone(&range_fetcher),
        mount_take_down,
        folder: folder.clone(),
    };
    let files_interface = Arc::new(ItemFilesInterface::new(
        folder,
        store.clone(),
        Arc::clone(&download_states),
        Arc::clone(&downloader),
        Arc::clone(&free_up),
        Arc::clone(&item_handles),
        Arc::clone(&write_operation_thread),
        sync_plan_wake,
        Arc::clone(&inotify_bridge),
        running_steps,
    ));

    let mut kernel_requests = ItemKernelRequests::new(
        lookup,
        Arc::clone(&read_connections),
        place_changes,
        Arc::clone(&item_handles),
        write_operations,
        Arc::clone(&write_operation_thread),
        Arc::clone(&inotify_bridge),
        Arc::clone(&downloader),
        Arc::clone(&download_states),
        range_fetcher,
        free_up,
        Arc::clone(&access_mode_switch),
        store.clone(),
    );
    kernel_requests.download_guard = download_guard;
    let mount = ItemMount {
        backing_directory,
        access_mode_switch,
        folder_step_parts,
        item_content_files,
        files_interface,
        start_check_findings,
        store,
        upload_rules,
        parts_to_stop: PartsToStop {
            write_operation_thread,
            inotify_bridge,
            downloader,
            #[cfg(test)]
            download_states,
            read_connections,
        },
        session: Mutex::new(None),
        is_stopped: AtomicBool::new(false),
    };
    Ok((mount, kernel_requests))
}
