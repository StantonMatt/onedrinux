# Code map

Where everything is: every crate, directory and file with one line each, where its tests are, and
how each suite is run. The rules this structure is kept by are in
[`CONTRIBUTING.md`](../CONTRIBUTING.md), "The structure of the code".

A change that adds, moves or removes a file changes its line here.

## How to read it

- A heading names a directory; the files under it are named from that directory.
- `[tests]` after a Rust file `x.rs` means its unit tests are in `x/tests.rs`, or in `x/tests/`
  by topic. After `mod.rs`, `lib.rs` or `main.rs` it means `tests.rs` or `tests/` in the same
  directory. A `tests.rs` has no line of its own; the files of a `tests/` directory have.
- What a layer is, what state it owns and what of it is tested without the rest is said in the
  module doc at the top of its `mod.rs` (for the store: `item_store.rs`, `step_memory.rs`,
  `sync_plan.rs`). [`docs/design/`](design/README.md) says what the parts are, what holds
  between them, and why.

## The words

| Word | What it is |
|---|---|
| sync folder | the folder an account's OneDrive is shown in: a FUSE mount the daemon serves |
| backing directory | the directory on disk the mount lies over: the downloaded copies and what the user made here |
| item | one file or directory the folder shows or OneDrive has |
| local item id | the number the store gives an item, never reused; it is the kernel's node id too |
| binding | which item a OneDrive item (a drive id and an item id) belongs to |
| version | what an item is on one side: its place and, for a file, its content |
| local, OneDrive, synced version | what the folder shows; what OneDrive had when last asked; what the two sides last agreed on |
| sync plan | the steps that make the local and the OneDrive versions agree again; a pure function of rows |
| step | one thing the plan does: towards OneDrive (a request to Graph), towards the folder, or of the rows alone |
| listing | OneDrive's delta listing, written into the rows page by page |
| step memory | what a step keeps of an item between tries and across a stop: no source of truth |

## The parts

| Part | What it is |
|---|---|
| `crates/konedrived` | The user's daemon: sign-in, the sync folder as a FUSE mount, the sync by the plan, the D-Bus API |
| `crates/konedrivectl` | The command line: everything the window can do |
| `crates/konedrive-graph` | Sign-in with Microsoft and everything that talks to Microsoft Graph |
| `crates/konedrive-tree` | The store (SQLite): a row per item with its three versions, the step memory, and the sync plan |
| `crates/konedrive-fs` | A folder's files on the local filesystem: konedrive's extended attributes and modes, the feature probe |
| `crates/konedrive-reason` | Why a change is kept back: the reasons, their spellings and groups |
| `crates/konedrive-dbus` | D-Bus names and client proxies of the daemon, for `konedrivectl` and tests |
| `crates/konedrive-text` | What the codes mean, in words: the one catalogue of sentences, and the C++ generated from it |
| `dbus/` | The D-Bus interfaces as XML: the contract between the daemon and its clients |
| `app/` | The window and the tray icon (Qt 6, Kirigami) |
| `dolphin/` | The Dolphin plugins: overlay emblems and the context menu |
| `vendor/fuser` | The `fuser` crate with one change (`vendor/fuser/README.konedrive.md`); not mapped here |
| `tests/fuse-probe` | The VM suite: a sync folder mounted for real, in a virtme-ng VM |
| `tests/write-account` | The guarded harness for the test account |
| `tests/stress`, `tests/kio` | Tools run by hand, and the probes of KIO |
| `scripts/`, `packaging/`, `.github/` | Building, installing, the RPM packages, the workflows |

Which crate uses which, lowest first: `konedrive-fs` and `konedrive-reason` (which use nothing);
`konedrive-graph`; `konedrive-tree` (uses `konedrive-fs` and `konedrive-reason`; it does not
know `konedrive-graph`); `konedrive-dbus`; `konedrive-text` (uses `konedrive-reason` and
`konedrive-dbus`); then `konedrived` (which uses `konedrive-text` in its tests only) and
`konedrivectl`.

## How a change travels

- **A program changes the sync folder**: `mount/kernel_requests.rs` takes the kernel's request;
  `mount/item_write_operations/` changes the backing directory and writes the item's local
  version (`konedrive-tree`, `item_store.rs`); the plan's task is woken.
- **OneDrive changes**: `listing/` writes each page of the delta listing into the OneDrive
  versions of the rows, and wakes the plan's task.
- **The plan's task** (`sync_plan_task/`) reads the rows that differ, calls `make_sync_plan`
  (`konedrive-tree`, `sync_plan.rs`) and hands each step to its runner: `onedrive_steps/`,
  `folder_steps/`, or `sync_plan_task/row_steps.rs`. A step's commit
  (`item_store/step_commit.rs`) writes the rows only if they are still what it was planned from.
- **A program reads a file that is not on disk**: `mount/item_read_origin.rs` decides;
  `mount/item_range_fetcher.rs` fetches a range, `mount/item_downloader.rs` the whole file,
  through `content_source/`.
- **What the bus shows** of all this: `upload_queue/` and `status/`, answered by
  `account_folder/` through the wrappers in `dbus/`.

## How each suite is run

| Suite | Command | What it covers |
|---|---|---|
| The Rust workspace | `cargo test --workspace` | The unit tests of every crate and the integration tests in `crates/*/tests/`, on private buses, temporary directories and wiremock; nothing is mounted |
| One crate, or one test binary | `cargo test -p konedrived --lib`, `cargo test -p konedrivectl --test sync_cli` | The same, narrowed while working on one part |
| The development build's tests | `cargo test -p konedrived -p konedrivectl --features konedrived/dev-tools,konedrivectl/dev-tools` | The token export and the signed-out account (`dbus/token_export.rs`, `dbus/dev_tools.rs`, `konedrivectl dev`) |
| The window | `cmake -S app -B build/app -DBUILD_TESTING=ON && cmake --build build/app && ctest --test-dir build/app --output-on-failure` | `app/tests/` |
| The Dolphin plugins | `cmake -S dolphin -B build/dolphin -DBUILD_TESTING=ON && cmake --build build/dolphin && ctest --test-dir build/dolphin --output-on-failure` | `dolphin/tests/` |
| The VM suite | `tests/fuse-probe/run.sh [<group>…]`; `ONLY="<scenario>"` for one scenario | The daemon's service and `konedrivectl` over a sync folder that is mounted for real, on ext4 in a virtme-ng VM, over the fake OneDrive: nothing is mounted on the host. Groups: `service`, `mounted_folder`, `file_writes`, `file_reads`, `download_guard`, `daemon_restarts`, `cli`; by name only, `kio_trash`, `crash_consistency` |
| The test account | `konedrive-write-test` (`tests/write-account`) | Writes to OneDrive itself, through the guards; rare, never the user's real account |
| The structure | `scripts/check-structure.sh` | The files over the advised sizes (listed only), tests outside source files, the daemon's layer order, locks, the tests a module names |

## `crates/konedrived`: the daemon

### `crates/konedrived/`

- `Cargo.toml` — the crate; the features `dev-tools` (the token export), `testing` (test support
  for other crates' tests) and `fault-injection` (the fake OneDrive, for the VM suite).

### `crates/konedrived/src/`

The directories below are in layer order: a directory uses only the directories before it, and
`listing/` does not use `onedrive_steps/`. The order, as `scripts/check-structure.sh` has it:
`download_guard`, `config`, `account`, `folder`, `conditions`, `status`, `content_source`, `local`, `sync_parts`,
`mount`, `folder_steps`, `onedrive_steps`, `listing`, `desktop`, `upload_queue`,
`sync_plan_task`, `sync_tasks`, `account_folder`, `daemon`, `sentences`, `dbus`.

- `main.rs` — the program: reads the configuration, starts the daemon on the session bus, and
  stops when a task it needs is gone; `--tidy`, what the unit runs after the daemon's end.
- `lib.rs` — the list of the directories below.
- `clock.rs` — the wall clock, read in one place.
- `panic.rs` — what a caught panic said, and a lock taken whether or not a holder of it
  panicked (`lock`, `read`, `write`). `[tests]`
- `tests/fake_onedrive/mod.rs` — the module `fake_onedrive`: an in-memory drive behind a
  wiremock server, for the tests of every layer that talks to OneDrive and for the VM suite
  (built for tests and with `fault-injection` only). Its folders answer as OneDrive
  personal's: no cTag, an eTag only their own rename or move changes, and a recursive
  `DELETE` refused `412` without their current eTag; their children in pages with download
  URLs that answer a `Range` with `206`, `Content-Range` and the item's eTag.
- `tests/fake_onedrive/sockets.rs` — the fake's notification socket.

### `crates/konedrived/src/config/`

`config.toml`, where the daemon's files are, the version 1 migration.

- `mod.rs` — what the module gives the rest of the daemon. `[tests]`
- `paths.rs` — where the daemon's files are, and each account's.
- `ids.rs` — `AccountId`, and how `config.toml` holds a `DriveId` (the store's type).
- `model.rs` — what `config.toml` holds (version 2), and the rules of labels, client ids and
  held accounts.
- `store.rs` — `ConfigStore`, the one owner of the file.
- `download_guard.rs` — persistent guard rules and thresholds through `ConfigStore`; memory-only session approval.
- `config_trouble.rs` — `AccountHold`, why an account is held back, and `ConfigTrouble`, what
  `Accounts.LastError` says: cases with their data.
- `atomic.rs` — one file replaced in one step.
- `migrate.rs` — version 1 of `config.toml` and its migration to version 2. `[tests]`

### `crates/konedrived/src/download_guard/`

The machine-wide [download guard](design/download-guard.md), shared by every mount.

- `mod.rs` — app policies, sliding admission windows, byte reservations and block records. `[tests]`
- `downloads/tests.rs` — attribution capacity, invalidation and landing-ticket tests.
- `downloads.rs` — bounded session attribution of whole read downloads, landing tickets and explicit-keep invalidation. `[tests]`
- `identity/tests.rs` — caller identity, systemd units and KIO reader-role tests.
- `space/tests.rs` — shared disk-reservation and concurrent allocation tests.
- `identity.rs` — procfs caller facts, systemd app identities, KIO roles and the reader list. `[tests]`
- `space.rs` — allocations shared by reads, pins and explicit hydrate on the same filesystem. `[tests]`

### `crates/konedrived/src/account/`

One account: its sign-in, mode, state, quota, cached profile, stored secret.

- `mod.rs` — `AccountService`: the sign-in state machine of one Microsoft account; the order of
  its locks.
- `sign_in.rs` — starting, finishing and cancelling a sign-in; sign-out; the account's drive.
- `attempt.rs` — one browser sign-in, up to "these tokens, this drive, this email": an
  account's own, and a new account's.
- `mode.rs` — `Account.SetMode`: the switch between read-only and read-write; the wish for
  read-only that waits until everything is sent, is forced or is taken back. `[tests]`
- `state.rs` — the observable account state (`AccountSnapshot`), with the wish for read-only
  and the mode's note in `LastError` (`ModeNote`, also why the account may not write). `[tests]`
- `account_trouble.rs` — what went wrong for an account, as cases with their data: a sign-in
  in the browser (`SignInFailure`), a sign-in refused (`Refused`), a switch to read-write
  (`ReadWriteFailure`), and what `LastError` says (`AccountTrouble`).
- `quota.rs` — the quota of the account's drive, one copy whoever reads it. `[tests]`
- `cache.rs` — the cached profile and quota (`account.json`), shown offline. `[tests]`
- `secret.rs` — the refresh token's storage in the Secret Service, an item per account.
  `[tests]`
- `testing.rs` — test support, also for `konedrivectl`'s tests and the VM suite: a wallet in
  memory, and one account with no accounts manager. Built only under the `testing` feature.

### `crates/konedrived/src/folder/`

The sync folder on disk as a registration: its root, the names the daemon keeps for itself, the
lock on its directories' modes.

- `mod.rs` — the list of the modules.
- `root.rs` — `SyncRoot`: checking and registering a folder, and opening it — by the
  descriptor taken before the mount for a mounted one; `RegisterError`; the drive a folder
  remembers. `[tests]`
- `disk.rs` — the names the daemon uses for itself in the folder, and `Modes`, the one lock on
  the modes of a folder's directories. `[tests]`
- `walk.rs` — a walk over the folder's files that opens none of them; the reserved names.

### `crates/konedrived/src/conditions/`

When an account may work in the background: the pause, the automatic hold, the network.

- `mod.rs` — the automatic hold's sources (metered, battery, power profile), watched on the
  system bus; the trait `Accounts`. `[tests]`
- `running.rs` — what background work an account runs now: the one place that decides it from
  the user's pause, the automatic hold and the settings. `[tests]`
- `network.rs` — noticing that the network came back (NetworkManager). `[tests]`

### `crates/konedrived/src/status/`

What the sync reports of itself.

- `mod.rs` — the list of the modules.
- `activity.rs` — `Activity`: the activity log and the conflicts; the words of an event.
  `[tests]`
- `transfers.rs` — `Transfers`: the downloads under way. `[tests]`
- `space.rs` — `LocalSpace`: the space the folder's files take on disk. `[tests]`
- `report.rs` — `Report`: the three of them, as everything that reports holds them.
- `throttle_wait.rs` [tests] — whether OneDrive asked the account to wait: the decision that a
  request for content is refused meanwhile, and the first file refused of a wait.
- `totals.rs` — the queue totals: how much is left to download and to upload, and about how
  long it takes. `[tests]`
- `snapshot.rs` — `SyncSnapshot`, the observable state of a folder, grouped by who writes it:
  the folder's state, the check at start (`LocalScan`), live changes, `LastError`. `[tests]`
- `overall.rs` — the state an account is in as a whole and the reason for it
  (`Folder.Overall`, `Trouble`, `NotUpdated`): the one rule every client follows. `[tests]`
- `folder_trouble.rs` — `FolderTrouble`, why a folder is not up or its sync does not run, and
  `TakeDownCause`, why a mount cannot go on: cases with their data.
- `sync_trouble.rs` — `SyncTrouble`, what the folder's sync last ran into: a `CycleError` of
  a cycle with OneDrive, or a start that was refused; whether it blocks, and its kind.

### `crates/konedrived/src/content_source/`

Where a download gets its bytes from, and the download of a whole file.

- `mod.rs` — the list of the modules.
- `source.rs` — `ContentSource`: a fetch of a whole file from an offset, and a range for a
  program that reads.
- `source/download.rs` — a download in one stream: written in place, in order, and checked.
- `source/parts.rs` — a large pinned download in parallel parts.
- `source/guards.rs` — fail-closed remote hash verification and what every download checks: the source's answer, the breaks tried
  again, the checkpoint a download continues from, the hash at the end. `[tests]`
- `source/target.rs` — the file a download writes, and its blocking sections. `[tests]`
- `graph_source.rs` — a range by the listed eTag and size in one request, else by fresh
  Graph metadata around it; first-range version and endpoint ETag retention; whole-download
  metadata. `[tests]`
- `graph_source/links.rs` — the download URLs of a folder's files from one listing of it,
  kept 40 minutes with a 10,000-entry cap; pages release readers as they arrive, and one
  background listing per folder is cancelled on token invalidation or drop. `[tests]`
- `graph_source/links/tests.rs` — URL expiry pruning, the entry cap and background cancellation.
- `graph_source/read_blocks.rs` — aligned 1 MiB pieces and small whole files in one 64 MiB
  memory cache, keyed by item and opened generation; five-minute expiry, oldest-first eviction,
  crossing-range assembly, forget and token invalidation. `[tests]`
- `graph_source/read_blocks/tests.rs` — alignment, clipping, shared memory bound, expiry and
  generation separation.
- `graph_source/tests/blocks.rs` — request counts on both paths, boundary reads, incomplete
  answers and version changes across blocks and eviction.
- `tracked.rs` — `Tracked`: a content source that reports what it fetches to `Transfers`.
  `[tests]`

### `crates/konedrived/src/local/`

Which names stay here, and which OneDrive refuses.

- `mod.rs` — the list of the modules.
- `ignore.rs` — default temporary/build names left local only while unbound; explicit lists replace defaults. `[tests]`
- `names.rs` — the names OneDrive refuses, found before a request is spent; the computer's
  name for a conflict copy. `[tests]`

### `crates/konedrived/src/sync_parts/`

What the tasks of one folder's sync share: each a small owner of one state.

- `state_changes.rs` — listeners for runtime item-state changes, called after the source lock is released.
- `mod.rs` — the list of the parts.
- `running_steps.rs` — `RunningSteps`: the steps of the plan that run now, one per item.
  `[tests]`
- `sync_plan_wake.rs` — what wakes a folder's plan, and why (`WakeCause`). `[tests]`
- `published_plan.rs` — `PublishedPlan`: the plan's last answer for each item, as the bus and
  the mount read it without asking the task. `[tests]`
- `step_outcome.rs` — `StepOutcome`: what came of one step, the one answer every runner gives.
- `sending_holds.rs` — `SendingHolds`: what holds all sending to OneDrive (a pause, a
  sign-out, a read-only folder, a throttle), and its one owner. `[tests]`
- `onedrive_space.rs` — `OneDriveSpace`: whether OneDrive is full, as the plan is told and as
  a refusal for lack of space is read. `[tests]`
- `lease.rs` — the folder's lease as a task sees it: held while it changes the folder, so that
  a Forget, a bring-up or a switch of mode waits.

### `crates/konedrived/src/mount/`

A sync folder served as a FUSE mount over the rows of `items`: the one way the daemon shows
OneDrive in a folder. Every name comes from the store; the backing directory holds only the
downloaded copies and what the user made here, and is reached through the descriptor opened
before the mount. The mount is built of parts that own their state, each tested without a
mount.

- `mod.rs` — the list of the parts, and what each answers for.
- `item_mount.rs` — `ItemMount`: the holder of the mount's parts, what it is made with, `serve`
  and `stop`, and the parts it hands to the folder's sync. `[tests]`
- `item_mount/build_order.rs` — the order the parts are built in: the one place a part is made.
- `item_mount/tests/ignore.rs` — default build-folder names: new local folders left here, cloud-bound
  names kept syncing, the start's refresh (pins, bytes and held uploads) and a new OneDrive folder at
  a left-here name; mounted equivalents run in `tests/fuse-probe/file_writes.sh`.
- `kernel_requests.rs` — `KernelRequests`: the one `fuser::Filesystem`, a thin wrapper over
  `ItemKernelRequests`; interrupt answers EINTR and stop answers EIO, exactly once; what the
  folder never takes (links, special files, set attributes). `[tests]`
- `kernel_requests/pending_reply.rs` — replies moved to tasks and write jobs explicitly
  answer EIO on drop instead of dropping a raw fuser reply. `[tests]`
- `item_kernel_requests.rs` — `ItemKernelRequests`: what each request of the kernel does,
  request by request; `ATTRIBUTE_LIFETIME`. `[tests]`
- `fuse_connection.rs` — the connection of a mount to the kernel, as the kernel numbers it, and
  its abort. `[tests]`
- `errno.rs` — the errno a program is answered with when the disk refused.

The folder on disk:

- `backing_directory.rs` — `BackingDirectory`: the directory the mount lies over, by its
  descriptor. `[tests]`
- `backing_directory/disk_marks.rs` — `DiskMarks`: the marks an entry carries, the OneDrive id
  it belongs to and whether it is that file's downloaded copy. `[tests]`
- `backing_directory/disk_metadata.rs` — `DiskMetadata`: what the filesystem says of an entry,
  its kind, size, blocks and time.
- `backing_directory/file_operations.rs` — make and remove at a path, and the file without a
  name a download is built in: one call of the filesystem each, with nothing decided. `[tests]`
- `backing_directory/write_bits.rs` — the write bits of the folder on disk: taken off while no
  mount lies over it, given back when one does or when the folder is forgotten. `[tests]`
- `backing_directory/legacy_placeholders.rs` — a folder an older konedrive kept, which holds
  placeholders: a mount is never put over one. `[tests]`
- `place_changes.rs` — `PlaceChanges`: every change of a place in the backing directory, and
  every open of a backing file by the path of a row, with the undo of each. `[tests]`
- `upload_rules.rs` — `UploadRules`: what is uploaded at all (the ignore list), and a file
  manager's trash, which no program may make. `[tests]`

Before the mount goes over the folder:

- `start_check.rs` — the check of the backing directory, as the mount's build calls it.
  `[tests]`
- `backing_directory_check.rs` — `BackingDirectoryCheck`: the backing directory compared with
  the rows, once, and the rows made to say what the disk has. The one place the disk is read
  to learn what the folder contains. `[tests]`
- `backing_directory_check/backing_directory_contents.rs` — everything the backing directory
  holds, read once, with what the filesystem and the marks say of each entry. `[tests]`
- `backing_directory_check/item_rows.rs` — all the rows of `items`, read once. `[tests]`
- `backing_directory_check/disk_finding.rs` — `decide_finding`: what the check finds at one
  place, from what the rows say there and what the disk has there. `[tests]`
- `backing_directory_check/mark_change.rs` — `decide_mark_change`: what the check does to the
  marks of an entry. `[tests]`
- `backing_directory_check/place_walk.rs` — the walk over the places of the folder, a directory
  before what is in it, the decision asked at each.
- `backing_directory_check/standing_place_changes.rs` — the place changes a death cut short,
  read from the store with what the rows and the disk say now.
- `backing_directory_check/place_change_settlement.rs` — `decide_settlement`: whether such a
  change is finished or undone. `[tests]`
- `backing_directory_check/row_writer.rs` — what the check writes rows through.
- `backing_directory_check/tests/table_rows.rs` — a test per row of the check's table.
- `backing_directory_check/tests/convergence.rs` — rows and a directory that agree, random
  changes while no daemon runs, the check, and its promise asked each time.
- `backing_directory_check/tests/place_change_deaths.rs` — a death before the disk is touched,
  between disk and row, and in the middle of a change of several steps.
- `backing_directory_check/tests/rebuilt_store.rs` — a store that was lost and made again from
  the disk.
- `backing_directory_check/tests/own_marks.rs` — what the check writes of marks.
- `backing_directory_check/tests/unread_parts.rs` — a part that cannot be read is left as it
  is, and held.
- `backing_directory_check/tests/review_musts.rs` — sequences in which a move, a copy or a
  removal made by hand must not become one in OneDrive.
- `backing_directory_check/tests/cost.rs` — what the check costs for 100 000 files; run by name
  only.

What the kernel asks:

- `item_lookup.rs` — `ItemLookup`: lookup, getattr, a directory's reading and the extended
  attributes, answered from the rows on the asking thread's own connection; the sync mark
  Dolphin's emblem reads. `[tests]`
- `directory_states/tests.rs` — incremental aggregate state, moves, pins, failures and notification tests.
- `directory_states.rs` — in-memory recursive directory xattrs: parent/children links,
  descendant counters, incremental row/move/pin/delete and transfer updates; failed-download
  state. `[tests: precedence, moves, pin/unpin, recovery, randomized full recount, 100k cost]`
- `read_connections.rs` — the mount's connections to the store for reading, one per thread.
  `[tests]`
- `item_attributes.rs` — the attributes the kernel is given for an item: a function of its row,
  the access mode and, where it may speak, the backing file. `[tests]`
- `item_handles.rs` — `ItemHandles`: what programs hold open, by the kernel's `fh`, each file by
  its item. `[tests]`

A change a program makes:

- `item_write_operations.rs` — `ItemWriteOperations`: each change made in the backing directory
  and written into the item's row, in one order of disk and row; the errno of a refusal; a name
  OneDrive holds with an item the folder cannot show, and the user told of it. `[tests]`
- `item_write_operations/make.rs` — mkdir, mknod and create; a trash directory refused, and a
  name OneDrive holds with an item the folder cannot show. `[tests]`
- `item_write_operations/rename.rs` — rename, also over another item (its binding taken over);
  to a name OneDrive holds with an item the folder cannot show, refused. `[tests]`
- `item_write_operations/remove.rs` — unlink and rmdir; a directory that holds in OneDrive
  what the folder never showed is not removed, and the user is told. `[tests]`
- `item_write_operations/content_write.rs` — open for writing, truncate, the time set, the
  last close: the row first. `[tests]`
- `write_operation_thread.rs` — the one thread the write operations and the steps towards the
  folder run on, under the folder's tree lock. `[tests]`
- `access_mode_switch.rs` — `AccessModeSwitch`: whether programs may change the folder; the
  one owner of that fact. `[tests]`

A file that is not on disk:

- `item_read_origin.rs` — where the bytes of a file are read from: its backing file, or
  OneDrive. `[tests]`
- `item_range_fetcher.rs` — `ItemRangeFetcher`: a range asked of OneDrive, of the content the
  reader opened; a range answered short fails the read. `[tests]`
- `range_read_states.rs` — the delayed hydrating mark for program range fetches, per
  asking rather than cumulative: no mark before 500 ms, then through the read linger;
  pool admission and early refusals do not count. Tests in `range_read_states/tests.rs`
  and `item_kernel_requests/tests/fetch_states.rs` cover quick, overlapping and slow
  fetches, disk/memory answers, errors, stale content, interrupts and throttle waits.
- `range_readers.rs` — the files being read as ranges, which go before the background
  downloads. `[tests]`
- `item_downloader.rs` — `ItemDownloader`: a file downloaded whole into the backing directory,
  whoever asked. `[tests]`
- `item_downloader/download_source.rs` — the source one download fetches through: only answers
  of the content asked for, and how far the file is written, for the reads meanwhile.
- `item_downloader/space_checked.rs` — read and pin streams recheck the disk reserve; retry bytes
  are counted against the app's admission budget.
- `item_download_states.rs` — `ItemDownloadStates`: the state of each file's download, by the
  item and the content. `[tests]`
- `item_download_landing.rs` — conditional landings and atomic resident replacements on one existing place-change record; the landing: the last step of every download, the file given
  its name and then the row saying on disk. `[tests]`
- `item_free_up.rs` — free up space: the row first, then the downloaded copy goes; app reclamation also requires the attributed content and no open reader. `[tests]`

Who else asks, and the mount's end:

- `item_files_interface.rs` — what the bus interface `Files` asks of a mounted folder, by path:
  state, download, free up, pins. `[tests]`
- `item_content_files.rs` — the local content of an item as a step towards OneDrive reads it,
  and the marks a confirmed step leaves on what it sent. `[tests]`
- `item_inotify_bridge.rs` — tells whoever watches the folder of a change a step or a landing
  made, through the mount; directory aggregate notifications coalesce in 100 ms windows. `[tests]`
- `item_announcements.rs` — `ItemAnnouncement`: what is told, by local item id and former
  name. `[tests]`
- `mount_take_down.rs` — `MountTakeDown`: how a part says the mount must be taken down and the
  folder started over (its cause is `status::folder_trouble::TakeDownCause`). `[tests]`
- `stale_mount.rs` — the mount a daemon that died left on a folder: how one is told, and
  taken off. `[tests]`
- `tidy_step.rs` — what follows the daemon's end (`konedrived --tidy`): each folder a plain
  directory without its write bits. `[tests]`

Test support:

- `item_fixture.rs` — the one fixture the tests of the mount, the steps and the plan's task
  share: a store in a file, a backing directory under no mount, the mount's parts. `[tests]`
- `item_fixture/content_source.rs` — its content source: the bytes the test gave.
- `item_fixture/row_rules.rs` — the rules that hold of the rows at all times, asked after a
  test changed them.
- `item_fixture/step_stand_in.rs` — a stand-in for the runners of steps: each step answered as
  OneDrive would from what the rows say.

### `crates/konedrived/src/folder_steps/`

The runner of the plan's steps towards the folder.

- `mod.rs` — `FolderSteps`: one function per case of `FolderStep`, each a change of the backing
  directory and its commit, with its undo; the conflict copy made and put on record. `[tests]`
- `tests/races.rs` — the steps against a program's writes, a listing's pages and deaths, in
  orders made from a seed.

### `crates/konedrived/src/onedrive_steps/`

The runner of the plan's steps towards OneDrive.

- `mod.rs` — `OneDriveSteps`: one function per case of `OneDriveStep`, each one request to
  Graph, the answer turned into what its commit takes, and the commit. `[tests]`
- `content_upload.rs` — exact sent-byte and final local-generation checks, metadata fallback and holds for hashless completions; a file's content sent: an empty file by one request, any other through
  an upload session, in fragments, gone on with after a stop or a crash. `[tests]`
- `confirmed_item.rs` — Graph's answer to a step as the commit takes it, and what a request is
  guarded with: a directory's removal by the folder's eTag alone. `[tests]`
- `graph_refusal.rs` — a refusal of Graph, by what a step does about it. `[tests]`
- `lost_answers.rs` — what became of a request whose answer was lost: the step adopts what it
  finds. `[tests]`
- `name_holders.rs` — who holds a name OneDrive answered is taken: the empty file of an upload
  session, or the step's own earlier upload. `[tests]`
- `made_items.rs` — what a step made in OneDrive that its commit did not write. `[tests]`
- `session_cancellations.rs` — what is left to clean in OneDrive after steps that did not end
  well: sessions cancelled, left items removed, unsettled requests settled. `[tests]`

### `crates/konedrived/src/listing/`

The listing of OneDrive into the rows.

- `mod.rs` — the list of the modules.
- `listing_cycle.rs` — one cycle of the delta listing: the pages fetched and given to the
  store page by page. `[tests]`
- `listing_cycle/listing_start.rs` — how a cycle begins, and what a refused page leads to: two
  decisions. `[tests]`
- `listing_poller.rs` — the task that runs a cycle on a schedule, on a refresh and when the
  notification socket says so. `[tests]`
- `listing_poller/poll_wait.rs` — how long the poller waits: two decisions. `[tests]`
- `live.rs` — changes from OneDrive at once: Graph's notification socket kept open, a cycle
  asked for at each event.
- `classify.rs` — what a Graph item becomes in the store, and whether the folder can show it
  (`SkipReason`). `[tests]`

### `crates/konedrived/src/desktop/`

Baloo, KDE's directory notifications and thumbnails.

- `mod.rs` — the list of the modules.
- `baloo.rs` — keeps KDE's file indexer out of a sync folder. `[tests]`
- `dirnotify.rs` — telling KDE's programs to read directories again (`org.kde.KDirNotify`).
- `thumbs.rs` — thumbnails from OneDrive, written into the freedesktop thumbnail cache.
  `[tests]`
- `thumbs/request_start.rs` — `decide_request_start`: whether a thumbnail request may start
  now. `[tests]`

### `crates/konedrived/src/upload_queue/`

What the bus says of what waits and what was left out.

- `mod.rs` — the answers of `org.konedrive.UploadQueue`, `Transfers.Uploads` and `Skipped()`:
  pure functions of the plan's answer for each unsent item, the step memory and the state of
  sending; `publish_queue_status`. `[tests]`

### `crates/konedrived/src/sync_plan_task/`

The task of a folder's sync plan: one per folder, in either access mode.

- `mod.rs` — `SyncPlanTask`: the loop that reads what differs, makes the plan, runs its steps
  and settles what came of each; what wakes it, what bounds the plans, its stop. `[tests]`
- `plan_input.rs` — what a plan is made from: read from the store by its bounded readers,
  closed, planned, and widened by every item the plan answers it was not given; the window
  of a reading (`decide_window`) and the count of what will not be sent by itself. `[tests]`
- `missing_pinned_files.rs` — `MissingPinnedFiles`: whether a pinned file may be missing its
  content, which is when a plan's reading scans below the pinned directories. `[tests]`
- `step_choice.rs` — `decide_steps_to_start`: which steps of a plan start now. `[tests]`
- `step_runners.rs` — the three runners as the task takes them: one seam.
- `row_steps.rs` — the runner of the steps of the rows alone, among them the hash of a local
  content.
- `step_settlement.rs` — what is written and asked for after a step: the one place that counts
  tries, keeps a refusal and asks the listing for a cycle. `[tests]`
- `local_step_tries.rs` — `LocalStepTries`: the failed tries the task keeps itself, for the
  life of its run: of the steps towards the folder and of the rows, of a step towards
  OneDrive from a row that is not unsent, and of a try the store could not be written.
  `[tests]`

### `crates/konedrived/src/sync_tasks/`

The tasks of a folder's sync, and the order in which a folder starts, stops and changes its
access mode.

- `mod.rs` — `SyncTasks`: the poller, the plan's task, the thumbnail filler and the token that
  stops them; what the owner of a folder must do around a start and a stop. `[tests]`
- `start_order.rs` — `start_folder_sync`: a folder's start, one function over a typed input,
  each stage a function of its own. `[tests]`
- `stop_order.rs` — a folder's stop, its take-down, its forgetting, its account's sign-out and
  sign-in, and the daemon's end. `[tests]`
- `access_mode_change.rs` — the switch of a folder between read-only and read-write, and the
  change of the ignore list, with no task stopped. `[tests]`

### `crates/konedrived/src/account_folder/`

`SyncService`: one account's folder, the owner of its state. Every `impl SyncService` is here.

- `mod.rs` — `SyncService`, and what it holds. `[tests]`
- `folder.rs` — `Folder`: what an account's folder is, as a type, and the one way to change it
  (`SyncService::change`). `[tests]`
- `wiring.rs` — `Wiring`: everything `SyncService` takes from the rest of the daemon, given
  once.
- `registry.rs` — the folders of every account of one daemon, and what they share.
- `persisted.rs` — the folder as `config.toml` records it.
- `bring_up.rs` — bringing a folder up: a new registration, the folder taken up at the
  daemon's start, a folder brought back up.
- `start_stop.rs` — a folder's sync started, and asked for a cycle (`Refresh()`, the network
  back). `[tests]`
- `take_down.rs` — taking a folder down: a Forget, a removed account, a folder moved or
  deleted, and a fault of the mount, after which the folder is started again with a growing
  pause. `[tests]`
- `mode.rs` — the one watcher of the account's state: the folder follows the account's mode
  and holds its sending while the account is signed out or may not write. `[tests]`
- `pause.rs` — what holds an account's background work back: the user's pause, the automatic
  hold, the clock of a timed pause. `[tests]`
- `settings.rs` — the account's own settings: thumbnails, the pause, the ignore list, the name
  conflict copies are made with.
- `publish.rs` — what the bus shows of the folder's state, worked out in one place. `[tests]`
- `upload_queue.rs` — what `org.konedrive.UploadQueue` asks of the folder: the answers of
  `upload_queue/`. `[tests]`
- `queries.rs` — the activity, the conflicts and their dismissal, the address of an item in
  OneDrive's web interface.
- `hydrate.rs` — `Hydrate`: a file downloaded now.
- `free_up.rs` — `Dehydrate`, `FreeUpSpace`, `FreeUp` and per-app session download reclamation. `[tests]`
- `pins.rs` — pins put on and taken off. `[tests]`
- `menu.rs` — what the context menu may offer for a selection (`Files.Menu`). `[tests]`
- `testing.rs` — test support: a `Wiring` of fakes, unfinished downloads and direct backing-allocation facts for the VM.
- `staged_folder.rs` — test code: a folder put into a state by hand, with no start of its sync.
- `tests/mod.rs`, `tests/running_sync.rs` — the owner over a folder whose sync really runs,
  with the mount built and not served.

### `crates/konedrived/src/daemon/`

The daemon as a whole: the accounts and their objects on the bus.

- `mod.rs` — the list of the modules.
- `manager.rs` — the account manager; the trait `Bus`: how its objects get on the bus.
  `[tests]`
- `manager/sign_in.rs` — `Accounts.SignIn`: the sign-in that belongs to no account, and the
  account made when it succeeds.
- `startup.rs` — `Daemon`: the configuration's lock, the migration, the start.
- `stop.rs` — the stop on SIGTERM or SIGINT, with its bound; `Tasks`, the tasks whose end
  stops the daemon. `[tests]`

### `crates/konedrived/src/sentences/`

Every sentence the daemon says to a person about what is wrong: the `Display` of each case of
trouble, which the bus, the journal and `LastError` say through.

- `mod.rs` — the list of the modules.
- `account_sentences.rs` — of an account's sign-in, its switch to read-write, and why it runs
  read-only or may not write. `[tests]`
- `config_sentences.rs` — of `config.toml` and the accounts in it. `[tests]`
- `status_sentences.rs` — of a folder's trouble, the sync's, and what holds its uploads and
  downloads. `[tests]`

### `crates/konedrived/src/dbus/`

The D-Bus interfaces: thin zbus wrappers, one file each, named like the XML in `dbus/`.

- `mod.rs` — the interfaces of one account's folder, as types.
- `accounts.rs` — `org.konedrive.Accounts`.
- `download_guard.rs` — `org.konedrive.DownloadGuard`: settings, app rules, session download reclamation across accounts and block signals.
- `account.rs` — `org.konedrive.Account`.
- `folder.rs` — `org.konedrive.Folder`.
- `files.rs` — `org.konedrive.Files`.
- `transfers.rs` — `org.konedrive.Transfers`.
- `upload_queue.rs` — `org.konedrive.UploadQueue`.
- `conflicts.rs` — `org.konedrive.Conflicts`.
- `activity_log.rs` — `org.konedrive.ActivityLog`.
- `local_scan.rs` — `org.konedrive.LocalScan`.
- `token_export.rs` — `org.konedrive.TokenExport`, only in a development build.
- `dev_tools.rs` — `org.konedrive.DevTools`, only in a development build.
- `export.rs` — putting the objects on the bus and taking them off; `OnBus`.
- `properties.rs` — the table of the properties the daemon announces by itself. `[tests]`
- `signals.rs` — what the daemon announces by itself: `PropertiesChanged`, `ActivityLog.Added`.
  `[tests]`
- `fault.rs` — `Fault`: every refusal under the name of a `Refusal`. `[tests]`

### `crates/konedrived/tests/`

Integration tests: the daemon over a private bus, Microsoft as wiremock.

- `common/mod.rs` — what they share: the fake Microsoft, the started daemon.
- `account_flow.rs` — the sign-in, from the client id to signed in.
- `sign_in.rs` — `Accounts.SignIn`: nothing made before it succeeds, its outcomes, its label.
- `accounts.rs` — several accounts.
- `dbus_api.rs` — `org.konedrive.Account`, and the introspection against the XML.
- `mode.rs` — the account's mode.

## `crates/konedrivectl`: the command line

### `crates/konedrivectl/`

- `Cargo.toml` — the crate; the feature `dev-tools` (`konedrivectl dev`).

### `crates/konedrivectl/src/`

The program is `main.rs`, `cli.rs`, `commands/`, `daemon.rs`, `read.rs` and `wait.rs`: it talks
to the daemon and prints. The library is `lib.rs`, `choice.rs`, `secret_file.rs` and `text/`:
what is decided and what is said, with nothing read from the daemon.

- `main.rs` — the program: parses the command line and runs the command.
- `cli.rs` — the commands as `clap` describes them; `sync` is `status`, `FolderCmd` and `PathCmd`.
- `cli/tests.rs` — guard command parsing, including `guard free ID`.
- `daemon.rs` — the daemon on the bus, the account a command chose, and every account's folder.
- `read.rs` — what is read of the daemon into the types `text/` prints.
- `wait.rs` — the waits for a sign-in to end.
- `lib.rs` — the library: its modules by name, the environment variables, whether a browser opens. `[tests]`
- `choice.rs` — the choice of an account. `[tests]`
- `secret_file.rs` — a secret written to a file, atomically and privately. `[tests]`

### `crates/konedrivectl/src/commands/`

- `mod.rs` — the list of the command groups.
- `account.rs` — `account`: list, remove, rename, mode.
- `add.rs` — `account add`: a new account, by signing in.
- `login.rs` — `login`: an account there already signs in again.
- `browser.rs` — opening the sign-in page.
- `status.rs` — `status`.
- `settings.rs` — `settings`.
- `guard.rs` — `guard`: status, app list, allowances, blocks, reset, thresholds and `free ID` reclamation.
- `sync/mod.rs` — `sync`: `status`, `anyway --all`, and which of the two groups a command is in.
- `sync/folder.rs` — the `sync` commands on the chosen account's folder.
- `sync/path.rs` — the `sync` commands that take a path.
- `sync/explain.rs` — what they share: a path made absolute, a refusal explained, the check
  that a folder does not need attention.
- `dev.rs` — `dev`, only in a development build.
- `store.rs` — offline store reset under configuration/store locks; no account or bus connection.
- `version.rs` — `--version`: this build's, and the running daemon's.

### `crates/konedrivectl/src/text/`

What is printed, by topic. Pure: each function is given what was read and returns the text.

- `mod.rs` — the list of the topics.
- `status.rs` — `status` and `sync status`: what was read, and its lines. `[tests]`
- `folder.rs` — what the commands on a folder say when they are done; the warning of unsent
  changes before a Forget; `sync activity`. `[tests]`
- `settings.rs` — `settings`.
- `version.rs` — `--version`.
- `uploads.rs` — the upload queue and what is not uploaded; a reason's sentence is
  `konedrive-text`'s. `[tests]`
- `transfers.rs` — transfers and the queue totals. `[tests]`
- `files.rs` — skipped items, conflicts, pins, free-up. `[tests]`
- `refusals.rs` — a refusal explained: the actions, and what the CLI read after the refusal. `[tests]`
- `refusals/folder.rs` — the sentences of a refused folder call, by the group of the action;
  those of `pin`, `unpin`, `free` and `open` are taken from `konedrive-text`.
- `refusals/account.rs` — the sentences of a refused call on the accounts and the token export.
- `accounts.rs` — `account list`.
- `formats.rs` — sizes, times, durations, shell words. `[tests]`

### `crates/konedrivectl/tests/`

Integration tests: the compiled binary against a daemon on a private bus.

- `common/mod.rs` — what they share: the daemon, started as `konedrived` starts it.
- `accounts_cli.rs` — several accounts.
- `login.rs` — `login` when the sign-in is cancelled elsewhere.
- `status.rs` — `status`.
- `mode_cli.rs` — `account mode`, and the token export's `--read-write`.
- `version_cli.rs` — `--version`.
- `store_reset.rs` — offline preservation of newer stores and pins; daemon/store-lock refusal without a session bus.
- `sync_cli/main.rs` — the test binary `sync_cli`: its harness.
- `sync_cli/registration.rs` — what `sync register` refuses, and a path that does not exist.
- `sync_cli/status.rs` — `sync status`, skipped items.
- `sync_cli/activity.rs` — transfers, conflicts, `sync open`, refresh.
- `sync_cli/refusals.rs` — the named refusals, explained.
- `sync_cli/settings.rs` — pause, the ignore list, the settings.
- `sync_cli/fuse.rs` — a mounted folder driven from the command line; run in the VM only
  (`tests/fuse-probe/run.sh cli`).
- `sync_cli/token_export.rs` — `dev export-access-token`.
- `sync_cli/wording.rs` — the wording shared with the window.

## `crates/konedrive-graph`: Microsoft Graph

### `crates/konedrive-graph/`

- `Cargo.toml` — the crate; the feature `testing`, for the daemon's tests.

### `crates/konedrive-graph/src/`

- `lib.rs` — the list of the modules, and what each offers; `lock`, a lock taken whether or
  not a holder of it panicked.
- `oauth.rs` — the authorization URL and the token endpoint; the scopes. `[tests]`
- `pkce.rs` — PKCE and random `state` values. `[tests]`
- `loopback.rs` — the one-shot listener for the OAuth redirect. `[tests]`
- `token.rs` — access tokens: cached, refreshed on demand, one refresh at a time; local
  permission to use cached content and weak invalidation callbacks. `[tests]`
- `secret.rs` — the refresh token's store, as the token manager sees it.
- `pool.rs` — the transfer pool: how many requests an account has in flight. `[tests]`
- `quickxor.rs` — QuickXorHash. `[tests]`
- `drive/mod.rs` — the drive API, reading: the delta feed, one item, a folder's files with
  their download URLs, content. `[tests]`
- `drive/error.rs` — an answer's status, what it and Graph's error code mean, a read's error.
  `[tests]`
- `drive/send.rs` — the one way a request leaves the client: who it is authorised as, and
  what a `429` or a `503` does to it.
- `drive/account.rs` — the two calls the account page needs: the profile and the drive.
  `[tests]`
- `drive/item.rs` — a `driveItem`. `[tests]`
- `drive/write.rs` — the drive API, writing: a new folder, a move, a delete. `[tests]`
- `drive/upload.rs` — upload sessions. `[tests]`
- `drive/socket.rs` — change notifications over Socket.IO. `[tests]`

## `crates/konedrive-tree`: the store

The store of one sync folder, a SQLite file. It is a map and not the truth: it is rebuilt from a
full listing and the backing directory whenever it cannot be used. Its tables: `items` (a row
per item), `step_memory` and the tables beside it, `place_change_records`, `pins`, `meta`,
`activity`, `conflicts`, `thumbnail_marks`, `own_upload_contents`.

### `crates/konedrive-tree/`

- `Cargo.toml` — the crate; the feature `testing`, for the daemon's tests.

### `crates/konedrive-tree/src/`

- `lib.rs` — the list of the modules, `TreeStore`, `TreeError`. `[tests]`
- `model.rs` — the types of a row: `Item` with its `LocalItemId`, its binding
  (`OneDriveItemReference`), its `ItemKind`, its local and OneDrive `ItemVersion`
  (`ItemPlace`, `ContentVersion`), `ContentPresence`; `LocalVersionChange`; the sync state a
  row gives a file (`FileSyncState`); `Placement` and `SkipReason`; `DriveId`; and a listed
  item as Graph's words give it (`Row`, `Change`). `[tests]`
- `schema.rs` — the schema's version, what a new store is created with, and when a store is
  admitted, migrated or quarantined. `[tests]`
- `schema/old_store.rs` — store admission, sequential migrations, snapshots and newer-schema refusal. `[tests]`
- `schema/quarantine.rs` — durable resumable quarantine, read-only-original inspection, pin carryover and pending-change exports.
- `schema/old_store/tests/safety.rs` — quarantine retention/restart, newer-schema non-mutation, migrations, snapshots, pins and exports.
- `item_changes/tests.rs` — change-feed transaction rollback and observer tests.
- `item_changes.rs` — transaction-safe TEMP-trigger feed for the mount's derived state;
  items, pin roots and step memory, consumed after writer jobs without changing the schema.
- `shared.rs` — `Store`: the store shared by the tasks of one folder, one thread per
  connection.
- `read.rs` — `ReadStore`: the store as its read-only connection gives it, for the bus.
- `meta.rs` — the `meta` table: what the store keeps one of, every key with its typed
  accessors.
- `folded_name.rs` — `FoldedName`: a name as OneDrive compares names; the one place a name is
  folded. `[tests]`
- `path_column.rs` — a path as a column of the store keeps it.
- `activity.rs` — the activity log as the store keeps it; `ActivityKind`.
- `conflicts.rs` — the conflicts on record: the local versions kept.
- `pins.rs` — the `pins` table: "always keep on this device", kept in the store.

The rows:

- `item_store.rs` — `ItemStore`: the rows of `items`, one per item by local item id. Everything
  in a row is written through it, each change one transaction; who writes what, and the rules
  that hold of the rows at all times. `[tests]`
- `item_store/item_row.rs` — a row in the database: the one place it is read from SQLite,
  written and removed.
- `item_store/item_reader.rs` — `ItemReader`: the rows for who only reads them (the threads of
  the mount, the bus): an item by id or by place, its path, what is not shown, counts.
  `[tests]`
- `item_store/plan_readers.rs` — the bounded readers a plan is made from. `[tests]`
- `item_store/bindings.rs` — when one item takes another's place: `take_over_binding`, `exchange_bindings`,
  `give_up_binding`, `merge_items`. `[tests]`
- `item_store/local_content.rs` — what the mount writes of a file's local content: its time,
  its presence on disk with a change, the content before a write. `[tests]`
- `item_store/content_presence.rs` — whether a file's whole content is on disk, as its row
  says it. `[tests]`
- `item_store/local_and_synced_version.rs` — an item of OneDrive the folder ceases to show
  without it being removed here. `[tests]`
- `item_store/onedrive_versions.rs` — what a listing says of an item, written into its row:
  the OneDrive version, the eTag, the placement. `[tests]`
- `item_store/onedrive_versions/listed_change.rs` — what becomes of one listed change:
  written, left because it may be older than what OneDrive confirmed, or refused. `[tests]`
- `item_store/onedrive_versions/listing_under_way.rs` — what the store keeps of a listing from
  its beginning to its end, and the link it goes on from. `[tests]`
- `item_store/onedrive_versions/confirmed_onedrive_version.rs` — what OneDrive confirmed to a
  step, and the count the guard against an outdated listing compares.
- `item_store/onedrive_versions/tests/confirmed_onedrive_version.rs` — tests: what OneDrive
  confirmed to a step.
- `item_store/onedrive_versions/tests/listing_under_way.rs` — tests: a page of a listing the
  store gave up.
- `item_store/onedrive_versions/tests/outdated_row.rs` — tests: a row a page leaves to be read
  again.
- `item_store/full_listing_request.rs` — a full listing that was asked for and has not begun,
  kept across a stop. `[tests]`
- `item_store/step_commit.rs` — the commit of a step: in one transaction it asks whether the
  rows are still what the step was planned from, and writes or changes nothing. `[tests]`
- `item_store/step_commit/tests/removal.rs` — tests: the commit of a removal.
- `item_store/step_commit/tests/listed_before_answer.rs` — tests: a listing that told of an
  item before the step's answer came.
- `item_store/step_record.rs` — what a step leaves on record beside the rows: the conflict
  copy it made, the event of the activity log. `[tests]`
- `item_store/place_change_records.rs` — the table `place_change_records`: each change of the
  backing directory's names, written before the disk is touched. `[tests]`
- `item_store/ignored_names.rs` — the flag `is_name_ignored` gone over once when the ignore
  list changed. `[tests]`
- `item_store/item_pins.rs` — the pins asked and set by item. `[tests]`
- `item_store/pinned_items.rs` — the files a pin holds for, by local item id. `[tests]`
- `item_store/thumbnail_marks.rs` — which content version a thumbnail was last made for.
  `[tests]`
- `item_store/own_upload_content.rs` — the content an upload of this folder left in OneDrive
  that is not the content it sent. `[tests]`
- `item_store/tests/row_rules.rs` — the rules of the rows, asked of rows read whole.
- `item_store/tests/row_refusals.rs` — a test per reason a change of a row is refused by.
- `item_store/tests/unsent_items.rs` — the unsent items a plan reads, a bounded number at a
  time, oldest first.
- `item_store/tests/listed_drive.rs`, `item_store/tests/shown_items.rs` — what the tests build
  a store with: a listed drive, and items the folder shows.

The step memory:

- `step_memory.rs` — what a step remembers beyond the versions, by local item id: the upload
  session, the tries and the next one, a refusal, a name OneDrive answered is taken. No source
  of truth. `[tests]`
- `step_memory/item_memory.rs` — `ItemMemory`: each fact as a value, and whether the item's
  row still says what the fact was learnt for (`FactStanding`). `[tests]`
- `step_memory/memory_row.rs` — a row of `step_memory` in the database.
- `step_memory/memory_reader.rs` — the step memory read on the read-only connection.
- `step_memory/onedrive_answers.rs` — which facts an answer of OneDrive about an item voids.
- `step_memory/refusal_reasons.rs` — the items OneDrive refused, with the reason of each, as
  the bus lists and counts them.
- `step_memory/request_records.rs` — the table `request_records`: each request that changes
  OneDrive, written before it leaves and gone when its outcome is known.
- `step_memory/upload_session_openings.rs` — the table `upload_session_openings`: the sessions
  the folder was about to open, until their address is known.
- `step_memory/upload_session_cancellations.rs` — the table `upload_session_cancellations`:
  the cancels left to send.
- `step_memory/mismatched_item_removals.rs` — the table `mismatched_item_removals`: items
  uploads left with other content, left to remove.
- `step_memory/tests/enumeration.rs` — every fact against every operation of the store that
  can change what the fact was learnt for, a cell each.
- `step_memory/tests/binding_moves.rs`, `step_memory/tests/listing.rs`,
  `step_memory/tests/mismatched_item.rs`, `step_memory/tests/request_records.rs`,
  `step_memory/tests/unsent_change.rs` — tests, by topic.

The sync plan:

- `sync_plan.rs` — `make_sync_plan`: one pure function, given rows and a few facts as values
  (`SyncPlanInput`), answering the steps that can run now and why every other item waits.
  `[tests]`
- `sync_plan/sync_step.rs` — `SyncStep`: a step towards OneDrive (`OneDriveStep`), towards the
  folder (`FolderStep`) or of the rows alone (`RowStep`), with the versions it was planned
  from. `[tests]`
- `sync_plan/side_change.rs` — what one side did to one item since the two last agreed.
  `[tests]`
- `sync_plan/item_decision.rs` — what the two sides' changes mean for one item by itself: the
  three tables (whether it exists, its place, its content), cell by cell.
- `sync_plan/sending_obstacle.rs` — what is not sent to OneDrive, and why: ignored, a name
  OneDrive cannot hold, too large, the quiet time, OneDrive full, the folder's state of
  sending. `[tests]`
- `sync_plan/plan_items.rs` — the items a plan is made over, found by what the plan asks of
  one item about another. `[tests]`
- `sync_plan/removed_directory.rs` — a directory one side removed, and what the other side has
  in it. `[tests]`
- `sync_plan/directory_binding.rs` — the binding of a directory that moves to another row in
  this plan.
- `sync_plan/name_contest.rs` — a name an item is to have in OneDrive, and what else has or
  wants it: what frees a name goes before what takes it. `[tests]`
- `sync_plan/name_exchange.rs` — items that wait for each other's names: through a temporary
  name. `[tests]`
- `sync_plan/onedrive_name_holders.rs` — the items the rows say OneDrive has under one name of
  a directory.
- `sync_plan/move_cycle.rs` — a cycle of moves between the two sides: OneDrive's move stands.
- `sync_plan/conflict_copy_name.rs` — the name of a conflict copy. `[tests]`
- `sync_plan/tried_step.rs` — which steps of an item a next try holds.
- `sync_plan/wait_chain.rs` — the items that wait for one another, followed to where the wait
  ends.
- `sync_plan/tests/existence.rs`, `sync_plan/tests/place.rs`, `sync_plan/tests/content.rs` — a
  test per cell of the three tables.
- `sync_plan/tests/item_relations.rs` — a test per case of what the plan decides between
  items.
- `sync_plan/tests/enumeration.rs` — every kind of row against every kind for another item
  that has to do with it.
- `sync_plan/tests/directory_removal.rs`, `sync_plan/tests/items_left_by_uploads.rs`,
  `sync_plan/tests/own_upload_contents.rs`, `sync_plan/tests/start_up_check.rs`,
  `sync_plan/tests/temporary_names.rs` — tests, by topic.
- `sync_plan/tests/convergence.rs` — the plan converges: rows changed at random on both sides
  are planned and the steps run until none is left; then the two sides are equal and nothing
  was lost.
- `sync_plan/tests/convergence/random_changes.rs` — the changes, from a seed.
- `sync_plan/tests/convergence/drive.rs` — OneDrive in these tests.
- `sync_plan/tests/convergence/plan_input.rs` — the caller of the plan in miniature.
- `sync_plan/tests/convergence/step_stand_in.rs` — the stand-in for OneDrive and the folder:
  nothing but the commits of the steps.
- `sync_plan/tests/convergence/requests_in_flight.rs` — the run in which a request changes
  OneDrive whatever becomes of its answer.
- `sync_plan/tests/convergence/step_rules.rs` — what a step must never be, asked of every step
  of every run.
- `sync_plan/tests/convergence/review_sequences.rs` — named sequences, run through the store.

## `crates/konedrive-fs`: a folder's files

### `crates/konedrive-fs/`

- `Cargo.toml` — the crate.

### `crates/konedrive-fs/src/`

- `lib.rs` — the list of the modules, the limits the crates share (`MAX_DEPTH`, `NAME_MAX`,
  `RESERVED_PREFIX`), and `proc_path`, the path of an open descriptor.
- `placeholder.rs` — what konedrive keeps on a file or a directory: its extended attributes
  and its modes. `[tests]`
- `probe.rs` — whether a directory's filesystem can be a sync folder's: nameless files, sparse
  files, `user.*` attributes. `[tests]`

## `crates/konedrive-reason`: why a change is kept back

### `crates/konedrive-reason/`

- `Cargo.toml` — the crate; it has no dependencies.
- `src/lib.rs` — a row's reason, a local skip, their stored spellings, details and groups. `[tests]`

## `crates/konedrive-dbus`: names and proxies

### `crates/konedrive-dbus/`

- `Cargo.toml` — the crate; the feature `testing` (`testing.rs`).
- `build.rs` — the version and the commit a build shows.
- `src/lib.rs` — the D-Bus names. `[tests]`
- `src/accounts.rs` — the proxies of the accounts' interfaces. `[tests]`
- `src/download_guard.rs` — the machine-wide download guard proxy.
- `src/rows.rs` — the rows the daemon answers with, by name. `[tests]`
- `src/overall.rs` — `Folder.Overall`: the states and the reasons, as they are spelled. `[tests]`
- `src/refusal.rs` — `Refusal`: every name a call is refused under. `[tests]`
- `src/version.rs` — the version line, the same in every program. `[tests]`
- `src/testing.rs` — a private session bus for tests; only with the feature `testing`.
- `tests/test_bus.rs` — tests: the private bus starts no program; a test's connection has the method timeout.
- `tests/version_script.rs` — tests: `scripts/version.sh`.

## `dbus/`: the interfaces

Each has its file in `crates/konedrived/src/dbus/`.

### `dbus/`

- `org.konedrive.Accounts.xml` — the manager: the accounts, the shared settings, the version.
- `org.konedrive.DownloadGuard.xml` — machine-wide app policies, counters, settings and block signals.
- `org.konedrive.Account.xml` — one account: sign-in, label, mode and the wish for read-only
  that waits, profile, quota.
- `org.konedrive.Folder.xml` — one account's folder: registration, refresh, pause, settings.
- `org.konedrive.Files.xml` — the per-file calls, by path, for any account.
- `org.konedrive.Transfers.xml` — downloads and uploads under way, speeds, the pool.
- `org.konedrive.UploadQueue.xml` — the changes waiting to be uploaded, and what is not.
- `org.konedrive.Conflicts.xml` — the local versions kept.
- `org.konedrive.ActivityLog.xml` — the activity log.
- `org.konedrive.LocalScan.xml` — the check of the folder on disk at its start.
- `org.konedrive.TokenExport.xml` — the token export of a development build.
- `org.konedrive.DevTools.xml` — a development build's way to add a signed-out account.

## `crates/konedrive-text`: the words for the codes

### `crates/konedrive-text/`

- `Cargo.toml` — the crate; it uses `konedrive-reason` and `konedrive-dbus` only.
- `src/lib.rs` — a sentence, its places, and who shows it. `[tests]`
- `src/reasons.rs` — why a change is not uploaded: every key of `Reason` and `LocalSkip`.
- `src/waits.rs` — what keeps an item on this computer: every key of `WaitsFor`.
- `src/files.rs` — a file not kept, unpinned, freed up or opened in OneDrive: every name of
  `Refusal`, and the Dolphin plugin's own sentences. `[tests]`
- `src/menu.rs` — why an entry of Dolphin's menu is disabled: the tooltips. `[tests]`
- `src/cpp.rs` — the generator of the C++ files, and the window's (`app/generated/`).
- `src/cpp/plugin.rs` — the Dolphin plugin's (`dolphin/src/generated/`).
- `tests/generated.rs` — tests: the generated files in git are what the generator writes now;
  with `KONEDRIVE_UPDATE_GENERATED=1` it rewrites them.

## `app/`: the window

### `app/`

- `CMakeLists.txt` — the build; reads the version from `Cargo.toml`.
- `main.cpp` — the program: single instance, the window, the tray.
- `Main.qml` — the window: pages chosen from a sidebar, each shown only while there is something
  on it to see or do (On Hold while changes are held); the sidebar's foot names the build, a newer
  release and a daemon of another build.
- `qmlregistration.h` — what the QML finds in `org.konedrive.app`.
- `daemoncontroller.h`, `daemoncontroller.cpp` — the manager object, `org.konedrive.Accounts`.
- `downloadguardcontroller.h`, `downloadguardcontroller.cpp` — the download guard,
  `org.konedrive.DownloadGuard`: its switch and limits, and the apps it blocked or has a rule for.
- `changeguardcontroller.h`, `changeguardcontroller.cpp` — the change guard's settings,
  `org.konedrive.ChangeGuard`: Hold large changes and its limits, for every account.
- `heldchanges.h`, `heldchanges.cpp` — what the change guard holds of one account, with the token
  it was listed with: by kind in the page's words, the open group's rows, Allow and Restore and
  what they did.
- `changeguardtypes.h` — the structured types of `org.konedrive.ChangeGuard`.
- `accountsmodel.h`, `accountsmodel.cpp` — the accounts, each with its controllers and what is
  held of it; Sign In and its link, and Sign In Read-Only Instead.
- `currentaccount.h`, `currentaccount.cpp` — the account the window shows.
- `folderchoice.h`, `folderchoice.cpp` — what a folder picked for an account is, as its folder
  (`checkFolder`), and `FolderChoice`: a free one registered, a folder that stopped forgotten
  first; why one that is not free is refused.
- `foldertext.h` — a folder's path as the window writes it, from `~`.
- `writescope.h` — a trial's folders: a folder picked, relative to the OneDrive folder, and the list
  with a folder added (one covered changes nothing; one that covers others takes their place).
- `timetext.h` — a moment as the window writes it: the time of day today, else the date too.
- `accountcontroller.h`, `accountcontroller.cpp` — one account's `org.konedrive.Account`, a trial's
  folders and its refused changes included.
- `synccontroller.h`, `synccontroller.cpp` — one account's folder, for QML, with the speeds and
  transfers of the last two minutes for the charts.
- `synctypes.h` — the structured types of the folder's interfaces.
- `skippeditem.h` — one entry of `Skipped()` and of `NotUploaded()`.
- `accountstatus.h`, `accountstatus.cpp` — one summary of an account and its folder: the state
  the daemon decided (`Folder.Overall`), in words by its reason; "held" while changes are held.
- `appstatus.h`, `appstatus.cpp` — the whole app's state, for the tray: the worst of the accounts'.
- `activitymodel.h`, `activitymodel.cpp` — the activity list: as many events as the daemon keeps.
- `conflictmodel.h`, `conflictmodel.cpp` — the "Kept Both Versions" list.
- `transfermodel.h`, `transfermodel.cpp` — the downloads under way.
- `uploadreasons.h`, `uploadreasons.cpp` — why a change is not uploaded, in words: the entry
  functions; the sentences are generated.
- `skipreasons.h`, `skipreasons.cpp` — why the folder cannot show an item, in words: the same
  sentences as `konedrivectl`'s, compared by its test.
- `generated/reasontexts.h`, `generated/reasontexts.cpp` — generated from `konedrive-text`, not
  edited: the window's sentence for every reason, and how a reason is cut into its parts.
- `notifier.h`, `notifier.cpp` — the notifications, a change refused outside a trial's folders
  included; `GuardNotifier`, the download guard's;
  `ChangeNotifier`, an account's changes on hold, with Review… and Restore.
- `windowsection.h` — where a click on a notification opens the window: Status, Status with its
  History unfolded, On Hold, a section of Issues, Settings' apps the download guard blocked, or
  OneDrive Settings' Sync card.
- `konedrive.notifyrc` — the notification events.
- `trayicon.h`, `trayicon.cpp` — the app in the tray: one icon for every account, or one each.
- `trayitem.h`, `trayitem.cpp` — one tray icon and its menu.
- `traysettings.h`, `traysettings.cpp` — the setting for an icon per account.
- `downloadprogresscontroller.h`, `downloadprogresscontroller.cpp` — long downloads reported
  to Plasma as jobs.
- `downloadjob.h`, `downloadjob.cpp` — one such job.
- `downloadjobtracker.h`, `downloadjobtracker.cpp` — where a job is registered.
- `downloadprogresssettings.h`, `downloadprogresssettings.cpp` — the setting for it.
- `placescontroller.h`, `placescontroller.cpp` — one entry per folder in KDE's Places.
- `placessettings.h`, `placessettings.cpp` — the setting for it.
- `autostart.h`, `autostart.cpp` — start at login.
- `selfrestart.h`, `selfrestart.cpp` — the window restarts as the installed program after an update.
- `serviceunit.h`, `serviceunit.cpp` — the daemon's systemd unit: starting it, and its log.
- `buildversion.h`, `buildversion.cpp` — a build's version (`X.Y.Z`, `X.Y.Z-dev.N`, a tag
  `vX.Y.Z`), read and ordered as RPM orders the packages.
- `releasecheck.h`, `releasecheck.cpp` — the newer release on GitHub: asked once a day, said in the
  sidebar's foot and notified once.
- `org.konedrive.KOneDrive.desktop.in` — the desktop entry.

### `app/qml/`

- `AccountSwitcher.qml` — the top of the sidebar: the OneDrive the pages show.
- `SignInPage.qml` — adding a OneDrive: Sign In for the first one, the wait for Microsoft's sign-in
  in the browser, its link, Cancel.
- `StatusPage.qml` — the start page: Sync Now, Pause and Open Folder; what the folder is doing or
  what stops it, with the one thing to do, and a line of what Issues lists; what is on hold, with
  Review…; the space, with Free Up
  Space and Unpin All; each way's card, the pool and the files moving now; History, folded, with
  its search. With no working folder, choosing one.
- `StorageBar.qml` — the space in one bar: what is on this device and the rest of what is used in
  OneDrive, of its storage, with what is always kept.
- `TransferCard.qml` — one way's card: the speed, the files moving, what is left, and the chart
  of the last two minutes.
- `StoppedFolderCard.qml` — a folder that stopped, on Status: why, Try Again, what is not uploaded,
  and Use Another Folder….
- `FolderPicker.qml` — the dialog that picks an account's folder, opened again with why while the
  folder picked cannot be it.
- `UnsentChangesPrompt.qml` — a question whose answer leaves the folder behind: it says first how
  many changes made here are not uploaded.
- `DisconnectDialog.qml` — that question before an account is disconnected.
- `AnotherFolderDialog.qml` — that question before another folder is used.
- `ListPage.qml` — a page that is one long list, drawn as a page of FormCards.
- `FormCardRow.qml` — a row of that list, on a card-coloured band.
- `OnHoldPage.qml` — the changes the change guard holds, for review: a card per kind with its two
  buttons, Allow All and Restore All, the open group's items, what was sent before the hold, and
  what the decisions did.
- `IssuesPage.qml` — what needs the user, in one list: the changes that can't upload (Upload for a
  file Don't Upload kept here), the files whose both versions were kept, what can't be shown in the
  folder.
- `OneDriveSettingsPage.qml` — one OneDrive's settings: its account, two-way sync, folder,
  previews and the free space its downloads leave.
- `SettingsPage.qml` — the whole app's settings, the download guard with its limits and its apps,
  Hold large changes with its limits, and Quit.
- `GuardAppRow.qml` — one app the download guard blocked or has a rule for, with its buttons; on
  Settings and Status.
- `ServiceCard.qml` — the KOneDrive service is not running: start it, or read its log.

### `app/tests/`

- `fakedaemon.h` — a stand-in for `konedrived` on the tests' private bus.
- `session-bus.conf` — that bus.
- `accountcontrollertest.cpp` — `AccountController`.
- `accountsmodeltest.cpp` — `DaemonController`, `AccountsModel`, `CurrentAccount`.
- `appstatustest.cpp` — `AccountStatus`, `AppStatus`.
- `folderchoicetest.cpp` — what a folder picked is, a case each, how a folder is written,
  `FolderChoice`.
- `synccontrollertest.cpp` — `SyncController`.
- `reasontextstest.cpp` — every reason reads as it did before its sentence was generated.
- `modelstest.cpp` — the transfer, activity and conflict models.
- `notifiertest.cpp` — `Notifier`.
- `downloadguardtest.cpp` — `DownloadGuardController`, `GuardNotifier`.
- `guardpagestest.cpp` — the download guard on Settings, Status and OneDrive Settings.
- `changeguardtest.cpp` — `ChangeGuardController`, `HeldChanges`, `ChangeNotifier`, and Status
  while changes are held.
- `onholdpagetest.cpp` — the change guard in the window: On Hold, Status' card, Settings' switch
  and limits.
- `trialtest.cpp` — two-way by default and a trial's folders: Sign In Read-Only Instead, OneDrive
  Settings' Sync card, the refused-change notification and Status' line.
- `dialogstest.cpp` — the window's pages and confirmations, against the fake daemon.
- `serviceunittest.cpp` — `ServiceUnit`.
- `releasechecktest.cpp` — `BuildVersion`, what a latest release is to a build, a case each, and
  `ReleaseCheck`.
- `downloadprogresscontrollertest.cpp` — `DownloadProgressController`.
- `downloadprogresssettingstest.cpp` — `DownloadProgressSettings`.
- `placescontrollertest.cpp` — `PlacesController`.
- `autostarttest.cpp` — `Autostart`.
- `singleinstancetest.cpp` — a second launch shows the first one's window.
- `selfrestarttest.cpp` — the window restarts once its program file is replaced and the daemon is back.
- `qmlcachetest.cpp` — the QML runs from the binary, never from Qt's disk cache.
- `CacheProbe.qml` — a file of that test.

## `dolphin/`: the Dolphin plugins

### `dolphin/`

- `CMakeLists.txt` — the build.

### `dolphin/emblems/`

- `generate.py` — generates the hicolor state emblems.
- `8/`, `16/`, `22/`, `32/` — SVG emblems for online-only, downloaded, pinned, syncing and failed states.

### `dolphin/src/`

- `overlayplugin.cpp` — the overlay plugin: emblems on files.
- `konedriveoverlay.json` — its description.
- `overlayengine.h`, `overlayengine.cpp` — its logic: which emblem, and the cache of roots.
- `actionplugin.cpp` — the context menu plugin: the OneDrive section, handed over waiting and
  set from the daemon's answer (`Files.Menu`) when it comes.
- `konedriveactions.json` — its description.
- `filestate.h`, `filestate.cpp` — what a file in the folder is, from its extended attributes
  alone: what the emblems show, and whether a selection is in a sync folder at all.
- `syncclient.h`, `syncclient.cpp` — the calls to the daemon, none of them waited for: the
  menu's question and the entries' calls.
- `refusaltext.h`, `refusaltext.cpp` — what to tell a person when the daemon refused: which
  sentence a failure gets, and how several are summed up; the sentences are generated.
- `generated/refusaltexts.h`, `generated/refusaltexts.cpp` — generated from `konedrive-text`,
  not edited: the plugin's sentence for every operation and refusal, and the menu's tooltips.

### `dolphin/tests/`

- `CMakeLists.txt` — the tests' build, and their private bus.
- `testsupport.h` — what the tests share: folders built from real files, and a stand-in for the
  daemon on a thread of its own.
- `session-bus.conf.in`, `activating-bus.conf.in`, `failing-daemon.service.in` — the tests'
  buses, and a daemon that fails to start.
- `overlayenginetest.cpp` — the overlay logic on its own, including live file transitions
  from cloud to syncing and back after a partial fetch, or to downloaded after a landing.
- `overlayplugintest.cpp` — the overlay plugin as Dolphin loads it.
- `actionplugintest.cpp` — the context menu plugin as `KFileItemActions` loads it.
- `noopentest.cpp` — the plugins never open a file inside the folder.
- `refusaltextstest.cpp`, `refusaltexts.expected` — every operation and refusal reads as it did
  before its sentence was generated: the strings of the hand-written table, a line each.

## `tests/fuse-probe`: the VM suite

A crate of its own, outside the workspace. A sync folder is mounted for real here, in a
virtme-ng VM, never on the host. How to run it: [`developing.md`](developing.md), "The VM
suite".

### `tests/fuse-probe/`

- `Cargo.toml`, `Cargo.lock` — the VM probe crate and its programs.
- `run.sh` — builds the programs, makes the guest's disk, boots the VM and runs `guest.sh` in
  it; fails when a scenario fails that `expected-to-fail.txt` does not name.
- `guest.sh` — inside the VM: an ext4 disk, and the groups run as an ordinary user. The group
  `service` is here: the daemon's service command by command, printed for a person.
- `scenario_checks.sh` — the checks a scenario makes, and the count of what passed and failed.
- `scenario_folder.sh` — a scenario's own folder, service and fake OneDrive, started and
  stopped.
- `mounted_folder.sh` — the group `mounted_folder`: delayed range-fetch and pinned
  whole-download states with real mounted `IN_ATTRIB` at both ends, a start from nothing, range reads and a
  whole download, changes from OneDrive, a directory that cannot be removed, Forget with unsent
  changes.
- `file_writes.sh` — the group `file_writes`: what a program's changes become in OneDrive.
- `file_reads.sh` — the group `file_reads`: reads, downloads, free-up; an interrupted libc
  pread receives EINTR promptly without an unanswered FUSE reply warning.
- `download_guard.sh` — the group `download_guard`: app-download reclamation and its keep/open skip rules, bulk and background refusals, explicit actions,
  initial-placement readiness, missing-name accounting, low-space ranges, already-open handles and mmap.
- `daemon_restarts.sh` — the group `daemon_restarts`: stops and kills in the middle of work.
- `crash_consistency.sh` — by-name-only group: two boots on one disk with guest SysRq power loss, edit/rename/missing-copy recovery and a mid-migration cut.
- `store_durability.py` — focused 10,000-create/1,000-rewrite/delete harness comparing SQLite NORMAL and FULL.
- `src/bin/store-probe.rs` — offline store assertions and real migration entry for the power-loss guest.
- `kio_trash.sh` — the group `kio_trash`, by name only: what KIO does when a file of the folder
  is moved to the trash (`tests/kio/trash_probe.cpp`).
- `expected-to-fail.txt` — the scenarios expected to fail, with the reason of each.
- `try-on-host.sh` — a trial of this tree's daemon on the developer's machine, beside the
  installed one, with a configuration and a state of its own.
- `src/lib.rs`, `src/fake_onedrive_commands.rs` — what the programs share: the commands a
  scenario gives the fake OneDrive.
- `src/bin/fuse-service.rs` — `fuse-service`: the daemon's service for one account, taking a
  command a line.
- `src/bin/fake-onedrive.rs` — `fake-onedrive`: the fake OneDrive as a process of its own.
- `src/bin/watch-client.rs` — `watch-client`: an inotify watch on a directory; optional
  `states` reads the xattr on each `IN_ATTRIB` to verify range/pin start and end states;
  every event printed.

## `tests/write-account`: the test account's harness

The program `konedrive-write-test`, a member of the workspace.

### `tests/write-account/`

- `Cargo.toml` — the crate.
- `src/main.rs` — the entry.
- `src/args.rs` — the command line: the token files and the daemon's `config.toml`.
- `src/harness.rs` — one run: the guards that must hold before anything is written.
- `src/guard.rs` — what every request must pass before it is forwarded.
- `src/proxy.rs` — the one way out to OneDrive: a forwarder on `127.0.0.1`.
- `src/checks.rs` — what the run checks against OneDrive itself.
- `src/checks/notifications.rs` — the change notifications.
- `src/checks/placeholders.rs` — an upload session's placeholder.
- `src/checks/remote/tests.rs` — the mocked folder contract check: descendant create/edit/remove, rename, move, both deletes.
- `src/checks/remote.rs` — the folder contract (no cTag, an eTag only the folder's own rename and move change, conditional deletes) and application ownership.
- `src/token_file.rs` — the explicit owner-only token file reader.
- `src/bin/metadata-probe.rs` — a read-only metadata presence sample.
- `src/bin/metadata-probe/tests.rs` — bounded sampling and metadata-only wire requests.
- `README.md` — the two account tools and their safety boundaries.
- `src/tests.rs` — tests: each guard shown to refuse, against wiremock.

## `tests/stress` and `tests/kio`: tools run by hand

### `tests/stress/`

- `README.md` — what the stress run does, and how to run it.
- `stress_uploads.py` — the run: real files made, changed, moved and deleted in a test
  account's folder.
- `graph_check.py` — the read-only check against Graph.
- `konedrivectl_wrap.py` — `konedrivectl`, called for one account.
- `quickxor.py` — QuickXorHash in Python.
- `account_guard.py` — the proof that the run is against a test account: its drive is listed in
  `write_test_drive_ids`.

### `tests/kio/`

What `docs/kio-behavior.md` was measured with.

- `CMakeLists.txt` — the build of both probes; never installed.
- `kio_probe.cpp` — measures what KIO opens of the files in a folder Dolphin shows.
- `run.sh` — runs `kio_probe` on a private bus, in a scratch home.
- `trash_probe.cpp` — measures what KIO does when a file is moved to the trash; run only in the
  VM suite's guest (`tests/fuse-probe/run.sh kio_trash`).

## Scripts, packaging and CI

### `scripts/`

- `build-rpm.sh` — builds the RPM packages from the committed tree.
- `version.sh` — the version a build carries.
- `check-structure.sh` — the guard of the structure rules.
- `dev-install.sh` — builds and installs for the current user: a development install.
- `dev-uninstall.sh` — removes it.

### `packaging/`

- `rpm/konedrive.spec` — the two packages.
- `deb/README.md` — local Kubuntu package layout, build inputs and use.
- `deb/build.sh` — packages existing release artifacts into two Debian packages without installing them.
- `deb/konedrive.postinst`, `deb/konedrive.prerm`, `deb/konedrive.postrm` — user-service lifecycle hooks.
- `deb/copyright` — Debian copyright metadata.
- `systemd/konedrived.service` — the daemon's user unit.
- `dbus/org.konedrive.Daemon.service` — the daemon started by the bus.

### `.github/workflows/`

- `release.yml` — a release on every push to `main`: the RPMs for each Fedora, each tried in a
  clean container, then the tag and the release.
- `tests.yml` — the tests of a pull request into `dev`: the daemon's unit tests, the window's and
  the Dolphin plugins'.
- `structure.yml` — on every pull request: the guard, the check of the links in the doc comments,
  and the generated C++ against the catalogue of sentences (`cargo test -p konedrive-text`).

## Documents

- `README.md` — what KOneDrive is and does, in short, and where to read more.
- `docs/user-guide.md` — installing and using it: the sync folder, the window, the tray, Dolphin.
- `docs/command-line.md` — the same from a terminal, with `konedrivectl`.
- `docs/developing.md` — building, the development install, what it needs to run, the tests.
- `CONTRIBUTING.md` — building, the tests, the structure rules, the limitations log.
- `SECURITY.md` — what runs with which rights, the credentials, reporting a vulnerability.
- `docs/limitations/` — the technical limits that cannot simply be removed, one file each; its
  `README.md` is the index.
- `docs/kio-behavior.md` — what KIO and Dolphin open, and what KIO does with the trash.
- `docs/releasing.md` — the version, and how a release is made.
- `docs/release-notes/` — the notes of each release, by its version.
- `docs/screenshots/` — the pictures the README shows.
- `docs/design/` — the design of the mechanism before the mount; being rewritten. Its
  `README.md` is the index.
- `docs/kernel-behavior-7.2/` — what fanotify, leases and file handles were measured to do, for
  that earlier mechanism; kept as a record of the kernel. Its `README.md` is the index.
- `docs/design/download-guard.md` — caller policy, admission windows, space and the UI bus handoff.
- `docs/code-map.md` — this file.

Remote namespace regression tests: `crates/konedrived/src/onedrive_steps/tests/remote.rs`
checks a folder removal's eTag guard and listing wait, the window it leaves, a store migrated
from schema 31, and atomic saves crossing upload requests, without a mount or account.

### Write defaults and trial scope

- `crates/konedrived/src/config/write_scope.rs` — relative subtree validation and live account
  scope sources; `config/write_scope/tests.rs` checks validation, configuration round trips,
  existing mode defaults and updates.
- `crates/konedrived/src/config/write_refusals.rs` — per-account bounded refusal channel,
  overflow counts and 10-second coalescing; `config/write_refusals/tests.rs` checks timing,
  saturation, final burst delivery, account isolation and weak entry cleanup.
- `crates/konedrived/src/dbus/account/tests.rs` — actual Account exporter and generated
  client's `WriteRefused` payload and burst contract on a private bus. The FUSE VM harness
  serves SyncService without an Account D-Bus object, so this is its signal-test fallback.
- `crates/konedrived/src/mount/item_write_operations/write_scope.rs` — common scope admission
  used by kernel dispatch, carrying the request PID and existing containing folder into
  refusal reports; tests in `mount/item_kernel_requests/tests/write_scope.rs`.
- `crates/konedrived/src/mount/item_kernel_requests/tests/listed_ranges.rs` — the 491,408-byte
  screenshot's 22 probes use one block on both Graph paths and keep guard counters unchanged;
  range reads of a folder's small files through the Graph source over the fake OneDrive: one listing and one
  request a file, and the download guard's counts the same as through a source that asks for
  each range.
- `crates/konedrived/src/onedrive_steps/write_scope.rs` — local and remote path admission for
  every outgoing step, resumed sessions and cleanup references; tests in `onedrive_steps/tests.rs`
  and `onedrive_steps/content_upload/tests.rs`. Scope-only checks also protect upload retry
  cleanup and name holders, without changing their normal hash or ownership decisions.
- `crates/konedrived/tests/sign_in.rs` — default and explicit mode selection at the first sign-in,
  one code exchange, and the account's write-scope D-Bus contract.
- `tests/fuse-probe/file_writes.sh` — `write_scope_allows_only_the_trial_subtree`, real kernel
  refusal of outside mutations and successful inside writes; run with the `file_writes` group.
  The VM suite builds debug by default (`FUSE_PROBE_PROFILE=release` for a release build).
### Outgoing change guard

- `crates/konedrived/src/change_guard/mod.rs`: per-account mutation admission, immutable batches and retained-copy ownership.
- `crates/konedrived/src/change_guard/batches.rs`: persistent snapshot tokens, scoped decisions, local-only Upload and restore progress.
- `crates/konedrived/src/change_guard/rows.rs`: folded folder rows and per-kind review details.
- `crates/konedrived/src/change_guard/ledger.rs`: durable values, time buckets, thresholds and atomic state-file writes.
- `crates/konedrived/src/change_guard/tests.rs`: window, repeated rewrite, concurrency, restart, descendant and cleanup tests.
- `crates/konedrived/src/config/change_guard.rs` and `config/change_guard/tests.rs`: daemon-wide settings, legacy per-account migration and the guard registry.
- `crates/konedrive-tree/src/item_store/guard_restore.rs` and `guard_restore/tests.rs`: conditional removal/rewrite restore transactions and typed per-item failure reasons; reserved retained paths are validated in `place_change_records.rs`.
- `crates/konedrive-tree/src/sync_plan/tests/change_guard.rs`: outgoing and reconciliation holds, descendants and quiet time.
- `crates/konedrived/src/mount/item_mount/guard_restore.rs`: serialized no-replace disk restoration, conflict copies and specific plain-text failures.
- `crates/konedrived/src/mount/item_write_operations/tests/change_guard.rs`: offline retained copies and collision tests.
- `crates/konedrived/src/account_folder/change_guard/tests.rs`: offline admission, partial restore results and terminal progress.
- `crates/konedrived/src/account_folder/change_guard.rs`: token-scoped account Allow/Restore results and progress orchestration.
- `crates/konedrived/src/dbus/change_guard.rs`: backend interface and per-account signal tasks; contract tested in `crates/konedrived/tests/accounts.rs`.
- `dbus/org.konedrive.ChangeGuard.xml` and `crates/konedrive-dbus/src/change_guard.rs`: checked-in contract and Rust client.
- `crates/konedrivectl/src/commands/changes.rs`: status, held/sent history, Allow, Restore and settings commands.
- `tests/fuse-probe/mass_changes.sh`: fake-cloud bulk mutation drills, token-scoped decisions and restore results; run through `tests/fuse-probe/run.sh mass_changes`.
- `docs/design/change-guard.md`: thresholds, persistence, retained bytes, approval and restore semantics.

- `crates/konedrived/src/change_guard/recovery.rs`: restore receipts, listing retirement and durable retained-cleanup ownership.
- `crates/konedrived/src/mount/item_mount/guard_restore/tests.rs`: off-thread retained verification and inode revalidation.
- `crates/konedrivectl/tests/changes_cli.rs`: account selection rejected for shared guard settings.
