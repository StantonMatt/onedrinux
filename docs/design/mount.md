# The mount

How a folder is served: the mount and its parts, the backing directory and the order in which the
disk and the rows change, what each request of the kernel does, how a file that is not on disk is
read and downloaded, how space is freed up, the start-up check, the end of a mount, and how
whoever watches the folder is told of a change the daemon made. The words are those of
[README.md](README.md), "The words".

## 1. Scope

This document covers `crates/konedrived/src/mount/` (its `mod.rs` lists the parts), the runner of
the steps towards the folder in `crates/konedrived/src/folder_steps/` as far as the disk goes,
the content sources in `crates/konedrived/src/content_source/`, and the download in
`crates/konedrive-graph/src/drive/` and `pool.rs`.

It does not cover, and points to:

- the item, its three versions, the rules that hold of the rows, and who writes what:
  [sync.md](sync.md) §2–§4;
- what the sync plan decides, and what a step is and remembers: [sync.md](sync.md) §6–§7;
- the order of a folder's start and stop, and the pause after a take-down: [sync.md](sync.md) §9;
- uploads, the access mode from the user's side, conflicts: [writes.md](writes.md);
- what a pin downloads and keeps: [pinning.md](pinning.md);
- the bus, `konedrivectl`, the window and the Dolphin plugins: [desktop.md](desktop.md);
- choosing a folder and the probe of its filesystem: [accounts.md](accounts.md).

## 2. The mount and its parts

### 2.1 `ItemMount`

`ItemMount` (`mount/item_mount.rs`) holds the parts of one folder's mount and has no behaviour of
its own. Each part owns its state; what is decided is a pure function of typed cases inside the
part, tested without a mount. The parts are made in one place, in one order
(`item_mount/build_order.rs`): a stale mount taken off (§8.3), the backing directory opened, its
write bits given back (§8.4), the access mode switch (read-only), the take-down, the
write-operation thread, the place changes, **the start-up check** (§7), the entries still set
aside removed, and then everything that reads the rows.

`ItemMount::build` answers a mount that is not over the folder yet; `ItemMount::serve` puts it
there with `fuser`: file system name `konedrive`, `nosuid`, `nodev`, no `allow_other`, four
threads, each on a connection of its own to the kernel. At its start the mount asks the kernel for
`READDIRPLUS`, parallel directory operations, atomic `O_TRUNC` and a read-ahead of 1 MiB. Before
the mount goes over the folder, a second descriptor of the backing directory is handed to the rest
of the daemon (`folder::root::mounted_over`): from then on the folder's path is the mount, and the
daemon reaches the folder on disk only by descriptor. The tests of the daemon use a mount that was
built and not served; only the VM suite mounts.

| Part | State it owns | Changed by |
|---|---|---|
| `BackingDirectory` | the descriptor of the folder on disk | nobody: opened once |
| `PlaceChanges` | the count of place changes (§3.2) | the write-operation thread |
| `AccessModeSwitch` | read-only, read-write, or read-only for good | the folder's start, a change of the access mode, `MountTakeDown` |
| `WriteOperationThread` | the jobs that wait for its thread | anyone queues; the thread runs them |
| `ReadConnections` | one read-only connection to the store per thread of the mount | opened at a thread's first question, closed at the stop |
| `ItemHandles` | what programs hold open, by the kernel's `fh`; private cloud-reader coverage; which items have a writer | open, create, release, opendir, releasedir |
| `ItemDownloadStates` | per item and content version: the running download and its waiters, a failed one's unfinished file, the fact that OneDrive stated another content; the bytes read as ranges | the downloader and the reads |
| `LaunchIntents` | bounded argv intent by process generation, item/content and desktop identity; immediate emblem leases | PID-bearing requests, expiry and mount stop |
| `RangeReaders` | a slot of the transfer pool per file being read as ranges | the range fetcher |
| `ItemInotifyBridge` | the queue of announcements and its thread (§9) | the steps, the downloader, the free-up, a pin |
| `MountTakeDown` | whether a take-down was asked, and its first cause | the write operations, the steps, the landing |
| `InterruptibleReads` | the reads that wait for OneDrive, by the kernel's request number | the kernel's interface; the stop |

The other parts hold parts and decide: `ItemLookup` (the reading requests), `ItemWriteOperations`
(a program's changes), `ItemDownloader` and `ItemDownloadLanding`, `ItemRangeFetcher`,
`ItemFreeUp`, `ItemContentFiles` (the local content as a step towards OneDrive reads it),
`ItemFilesInterface` (the bus interface `Files`), `UploadRules` (what is never uploaded), and
`ItemKernelRequests`, one function per request, under the `fuser::Filesystem` of
`kernel_requests.rs`. The folder's sync takes from the mount the parts its steps towards the folder
are made of (`FolderStepParts`), the local contents, the open handles, the `Files` interface, the
findings of the start-up check and the access mode switch.

### 2.2 The threads

- **The mount's four threads** answer every reading request, read backing files, and take the
  writes and fsyncs of programs. Each reads the rows through its own connection, an answer in one
  read transaction, so a row and its path are of one moment; none waits for a page of the
  listing, the commit of a step or the tree lock.
- **The runtime** does what waits for OneDrive: a range, a download, the download before a
  write. The mount's thread reads every row it needs first and hands on values, so no worker of
  the runtime reads the rows through a mount's connection, and no thread of the mount is held
  for a fetch.
- **The write-operation thread** makes every change of the backing directory and its row (§2.3).
- **The bridge's thread** performs the announcements (§9).
- **The store's thread** writes every row (`Store::call_blocking`).

### 2.3 The write-operation thread and the tree lock

A change a program asks for — make, remove, rename, an open for writing, a truncation, a time, the
executable bit, a writer's release — is one job of one thread (`write_operation_thread.rs`), and
so is every step towards the folder, a landing, a free-up, the switch to read-only and the go-over
of ignored names. The jobs run one after another, each under the folder's tree lock, which the
commit of a step towards OneDrive takes too. The kernel's reply is moved into the job: a program
hears its change was made only once its row is written, and no two changes of names interleave. A
stopped thread runs nothing more: the job in flight ends, and the queued jobs deliberately
answer `EIO` through their reply guards before being dropped.

A cycle of the listing takes no tree lock ([sync.md](sync.md) §5). What it may write between a
step's disk change and its commit is answered by the commit's condition: a commit that finds the
row changed writes nothing and undoes the disk ([sync.md](sync.md) §7).

The locks are taken in one order, never the other way (`place_changes.rs` has it in full): the
folder's lease (the plan's task, around a step towards the folder); the tree lock; the place
changes' own lock; one job of the store or one read transaction; the leaf mutexes, each held for
a few lines.

### 2.4 The access mode switch

`AccessModeSwitch` holds one value. A mount starts read-only, and the folder's start sets the
account's mode before any task plans ([sync.md](sync.md) §9). A read-only folder shows no write
bit and refuses every change with `EROFS`. A request that changes the folder asks the mode twice:
on the mount's thread before it queues a job, so that a program is told `EROFS` without waiting
for the tree lock; and in the job, whose answer counts. The switch to read-only is itself a job
of the thread, so no operation is half done across it; the switch to read-write is made at once,
since an operation begun in a read-only folder was refused at its start. The last close of a file
is written whatever the mode is by then. A mount that asked for its take-down is read-only for
good (§8.1). What the switch means for uploads is in [writes.md](writes.md).

## 3. The backing directory

### 3.1 What it holds

The backing directory is the folder on disk (`BackingDirectory`, `mount/backing_directory.rs`),
reached through the descriptor opened before the mount. It holds the downloaded copies, the files
and directories made here, and the directories of OneDrive that hold something on disk; a
directory of OneDrive with nothing on disk need not exist there, and is made, with its mark, when
the first content comes. A download is built in a file with no name (`O_TMPFILE`) on the backing
directory's filesystem: no listing ever shows a file that is not whole, and one the daemon leaves
behind is removed by the kernel.

The daemon's own names are hidden by the mount and refused to programs (`folder/disk.rs`): an
entry set aside (`.konedrive-aside-<local item id>`, in the root), the temporary name of an
exchange of two names (`.konedrive-swap-…`, a row's place), the retained removal directory
`.konedrive-change-guard`, and `.konedrive-holding` and
`.konedrive-new-…`. A folder that holds placeholders of an older konedrive — a file marked with a
state that is not whole, or a `.konedrive-holding` directory — is not mounted
(`ItemMountError::LegacyPlaceholders`).

### 3.2 A row finds its file by the path of its local version

No file handle is stored. While the mount is up nothing but the daemon reaches the backing
directory, so the daemon keeps it in step with the local versions, and a row's file is at the path
of its local version.

For names the disk changes before the row (§3.3), so a reader that reads a row, takes its path and
opens it could meet a rename: a missing file, or after two renames another item's file.
`PlaceChanges` keeps a count that is odd from the begin of a change of names to its end. Every open
of a backing file by a row's path outside the write-operation thread goes through
`PlaceChanges::open_at_row_path`: it waits while the count is odd, reads the row and its path in
one read transaction, opens, and reads the count again; when it moved, what was opened is dropped
and the whole is tried again. After three crossed tries the reader holds the place changes' lock
for its last try, which no change can begin under. A stat at a row's path
(`stat_at_row_path`, for the blocks a file takes) never sleeps: it tries twice and then answers
without facts, and the attributes of that one answer have no blocks.

A change of names begins only on the write-operation thread, and `open_at_row_path` never runs
there; both are asserted in debug builds.

### 3.3 The order of disk and row

The order serves one rule: **a row never says a copy is clean while the disk may hold the user's
bytes, and never says content is on disk that the disk does not hold for good.**

| What | Order |
|---|---|
| A program changes a file's content | the row first: the first writer's open writes `OpenedForWriting` — the content is changed here, and the content of before the write is kept in the row; then bytes go to the backing file with no row touched per write; the last close writes `ClosedChanged` with the size and times of the backing file, or `ClosedUnchanged`, which puts the former content back; only then the writer leaves the handles |
| A backing file made for a writer: an open for writing of an empty file not on disk, an open with `O_TRUNC`, a truncation to nothing | the record, the place change begun, the row, the file made, the change ended, the record forgotten: a reader that comes between the row and the file waits |
| Make, rename, remove, by a program or a step | the record, the place change begun, the disk, the row (which takes the record in its own transaction), the change ended, what was set aside removed for good |
| A landing | the file's data, time and marks synced with no lock held; then, as one job of the thread: the record, the link, its directory synced, the row says on disk (taking the record) |
| A free-up | the row says not on disk first; then the copy is set aside and removed |

A row the store refuses is undone on disk, its record is taken by itself, and the program gets the
errno of the refusal. An undo that fails leaves the record standing and asks for the mount's
take-down (§8.1); the next start's check settles the change from the record.

**Nothing is replaced.** Every move is `RENAME_NOREPLACE`; a place the rows say is free and the
disk holds is `EEXIST` to a program and an obstacle to a step. A removal, and a move over another
entry, first set that entry aside: it is renamed to its item's aside name in the root, and removed
for good only after the row is written and the change ended. A directory that holds what no row
says is not set aside: it stays where it is, without its mark. Leftovers of a death are put back by
the start-up check, and only after the check are the entries still set aside removed
(`PlaceChanges::remove_aside_entries`): before it, the entry a record puts back would be deleted.

The [change guard](change-guard.md) keeps a resident file's unlink aside beyond the row
write, in its private retained directory on this filesystem. Its recorded aside path is
confined to that directory; startup's temporary-aside cleanup leaves it alone. Cloud
confirmation drops the retained bytes, and Restore can rename them back without a
content download. Held versions are refused both by free-up's conditional commit and
by a reconciliation step, including one queued before the trip.

A step towards the folder keeps the same order; `folder_steps/mod.rs` has the record, the disk and
the commit of each case. Taking OneDrive's content for a downloaded copy sets the old copy aside
before its commit, unlike a free-up: its copy is of the old content, and a death after a commit
that came first would leave it under a row with the new content, where the check would take it for
content put there by the user.

### 3.4 A name's change is on record before the disk

Before a change of names touches the disk, what it is about to do is written to the store, in a
transaction of its own: the table `place_change_records`
(`crates/konedrive-tree/src/item_store/place_change_records.rs`). A record is one case of
`PlaceChangeRecord` — an entry made, moved, moved over another, set aside, a landed file linked —
in the rows' own terms: an item by its local item id, a place by its directory and name. It goes in
the transaction that writes the row of the change, or by itself when the change was undone. A
record does not say whose change it was, a program's or a step's.

A daemon that dies between the record and the row leaves the record, and the start-up check
finishes or undoes exactly that change from it (§7.1), with no guessing: the entry is at one of
the places the record names. The marks are not used for this.

### 3.5 The marks

The backing directory carries extended attributes, the marks:

| Mark | On | Says |
|---|---|---|
| `user.konedrive.root`, `user.konedrive.drive` | the folder itself | the folder's id and its account's drive ([accounts.md](accounts.md)) |
| `user.konedrive.item-id` | a downloaded copy, a directory of OneDrive, a file sent, a file made here for an item of OneDrive | which item of OneDrive the entry belongs to |
| `user.konedrive.state` = `hydrated` | a downloaded copy, a file sent | the file holds that item's content whole |
| `user.konedrive.ctag` | a downloaded copy, a file sent | which content of it |
| `user.konedrive.progress` | the nameless file of an unfinished download | the cTag and the bytes from the start that are on disk (§5.3) |

They are written by a landing (`write_whole_copy_marks`, the item id last), by the commit of an
upload (`ItemContentFiles`), by the directories made above a place, and by the start-up check. A
file made for an overwrite or a truncation of an item of OneDrive carries the item id alone: it
holds the user's content, not OneDrive's. No program can write a mark: the mount refuses
`setxattr` (§4.2). No request of the kernel reads them. They serve the start-up check and a lost
store (§7). A write-open clears the old cTag before allowing an edit; recovery without rows
hashes the bytes regardless of any surviving mark.

The attributes a program reads through the mount (`user.konedrive.item-id`, `.state`, `.pin`,
`.sync`) are not these marks: the mount answers them from the row and the plan
(`item_lookup.rs`), lists none of them, and writes nothing on disk. Dolphin's emblems read them
([desktop.md](desktop.md)).

Through the mount, `user.konedrive.state` on a bound file is `online-only`,
`hydrating`, `hydrated`, or `failed`. `failed` means a whole download exhausted the
content source's existing retries or failed to land (disk I/O or an occupied backing
place), or a pinned file's download is held by `free_space_reserve_gb`. A retry that
stays in flight past the delay below reports `hydrating`; success clears the remembered failure. Failures are keyed
by item and content version, so new content does not inherit them. Unpinning clears
the reserve-held case and a failure of a download only the pin asked for; failures
of downloads also requested by a read, write-open or explicit hydration remain until
recovery. Throttling, sign-out, cancellation and stale-content answers
are not download failures. Whole downloads triggered by reads use the same rule;
individual range-read errors do not. Download failures and retry memory last for the
mount's run; they are not persisted across restart.

A fetch first shows `hydrating` after one asking for the item's bytes has stayed in
flight for 500 ms. This applies to ranges read for a program and to whole downloads,
starting once their transfer-pool slot is held. Waiting for a slot, a disk read, and an
immediate answer from the source's memory cache never trigger the mark. The delay is
per asking: repeated or overlapping reads that each finish within 500 ms do not add
up, so a viewer's quick reads of many images do not flash their emblems.

A desktop argv launch is the exception to the fetch delay: its cloud-only file shows
`hydrating` immediately when the first PID-bearing request identifies the launch
(§5.2), without fetching bytes. Ten seconds without a read clears the emblem. A whole
download started by that intent inherits the shown state, so it stays continuous.

Once a range fetch is shown, the item stays `hydrating` while any range is in flight
and for `READ_LINGERS` (two seconds) after its last range ends, or while a whole download
is shown. Afterwards its row decides `online-only`, `hydrated` or `failed` again. Errors,
refusals and interrupts drop the same range guard and end that linger; a throttle wait
cancels an unanswered program fetch rather than leaving it marked throughout the wait.
The file is announced only when its shown state changes. A successful whole download
combines its final-state announcement and page-cache invalidation after its running
state is cleared; its start no longer pretends that content changed. `Files.ItemState`
and `konedrivectl sync state` read this same attribute, and the menu's path status uses the
same presentation state. This changes no admission, free-up or download-threshold rule.

Every directory, including the sync root, has `user.konedrive.state`, aggregated
recursively over all files shown below it, with this precedence:

| Condition below the directory | State |
|---|---|
| Any failed file | `failed` |
| Any hydrating file, or pinned file not yet on disk | `hydrating` |
| Any online-only file | `online-only` |
| All files on disk, or no files | `hydrated` |

`user.konedrive.sync` on a directory is `blocked` if any descendant file is blocked,
else `uploading` if any is uploading, else `pending` if any is pending, else absent.
These aggregates include unbound local files (on disk, with their existing upload
mark) and exclude items the mount does not show. A directory's own upload mark is not
part of the aggregate. File sync values and the pin on files and directories are
unchanged. `listxattr` still lists none of these virtual attributes.

`mount/directory_states.rs` keeps a parent/children index and seven counters per
node in memory, rebuilt before the mount is served. This avoids a persistent schema
change and keeps runtime transfer states beside durable row facts. Directory
`getxattr` reads counters, never descendants. A file-state update changes only its
ancestor counters; a move subtracts the old subtree totals and adds them to the new
ancestors. A moved or pinned subtree also refreshes inherited pins. Connection-local
TEMP triggers on items, pins and step memory record every writer's changes, including
listing/resync and deletes, in the same transaction. The store observer refreshes the
index before answering the writer job; rolled-back changes leave no feed entries.
Download, running-step and published-plan changes update the same counters directly.

Aggregate changes queue the existing inotify bridge's `MarksChanged` operation on
the affected directories (including the root), so parent watches receive `IN_ATTRIB`.
Each directory has a fixed 100 ms debounce window: bursts coalesce, and continuous
changes cannot postpone notification indefinitely. Counter updates remain immediate.
No aggregate or failure mark is written into the backing directory.


## 4. What a request of the kernel does

### 4.1 The requests

A node id is the item's local item id, from the first line on (`item_attributes.rs`). Every name,
place and attribute answered comes from the item's local version, with the three exceptions of
[sync.md](sync.md) §3. `item_kernel_requests.rs` has the table in full, with the function each
request calls.

| Request | Thread | What it does | Writes |
|---|---|---|---|
| lookup, getattr | mount | one row by directory and name, or by local item id; the blocks from a stat of the backing file | — |
| opendir, readdir, readdirplus | mount | the directory's rows, listed once at offset 0 and kept with the handle; later parts read the kept listing; each item of readdirplus is described by its own row | — |
| getxattr, listxattr | mount | the attributes of §3.5's last paragraph | — |
| open to read | mount | content on disk: the backing file, through `open_at_row_path`; not on disk: a handle that holds the item and the content it was opened as; nothing is downloaded | — |
| read | mount, then runtime | §5 | — |
| open to write | mount, runtime, write | a file not on disk and not empty is downloaded whole first (§5.3), then the open is a job | `OpenedForWriting` for the first writer |
| open with `O_TRUNC` | mount, write | the backing file made empty; nothing is downloaded | `OpenedForWriting` |
| create, mknod, mkdir | mount, write | the entry made; a created file made and opened in one transaction | `Made` |
| unlink, rmdir | mount, write | the entry set aside, the row, the entry removed | `Removed` |
| rename | mount, write | the entry moved, over another one set aside | `Moved` or `SavedOver` |
| setattr with a size | mount, runtime, write | a file not on disk is downloaded first, unless cut to nothing; a file open for writing is cut with no row | `Truncated` |
| setattr with a time, with a mode | mount, write | the time into the row and the backing file; of a mode the owner's executable bit alone ([F303](../limitations/F303.md)) | the time; the executable bit |
| write, fsync, flush | mount | writes into the backing file; fsync/flush sync it, then the WAL; errors propagate | none per write |
| fsyncdir | mount | the directory's backing directory synced, then the store's write-ahead log, for the names only the rows hold | — |
| release | mount, write | a reader's handle closed at once; a writer's last close is a job | `ClosedChanged` or `ClosedUnchanged` |

The attributes are made by one code for getattr and for a write operation's reply
(`item_attributes::to_item_attributes`): the content's size and time for a file, the backing
file's size and times while a program holds it open for writing, the blocks of the backing file,
0644 or 0755 and no write bit in a read-only folder, and for a directory the time of its backing
directory, or the time the mount started when it has none on disk. The kernel keeps attributes
and names for one second.

**One content through a handle.** A handle opened on a file that is not on disk reads the content
it was opened as, or nothing (§5.5). A node whose row is gone or shows nothing is `ESTALE`; a
request that carries the handle of a file removed or saved over while a program held it is
answered from the handle's own file (getattr, a truncation).

### 4.2 The requests answered as a local folder answers them

| Request | Answer |
|---|---|
| statfs | the space of the backing directory's filesystem; names up to 255 bytes |
| access | `EROFS` for a write in a read-only folder; allowed otherwise |
| ioctl | `ENOTTY` |
| poll | `ENOSYS`: the kernel answers every poll of the mount as always ready |
| fallocate | `ENOSYS`: the kernel answers `EOPNOTSUPP`, and `posix_fallocate` writes the range |
| copy_file_range | `ENOSYS`: the kernel copies by reads and writes through the mount |
| lseek | `ENOSYS`: the kernel seeks by itself, as for a file with no holes |
| symlink, link, mknod of anything but a regular file | `EPERM` ([F300](../limitations/F300.md)) |
| setxattr, removexattr | `ENOTSUP`, for every name, the daemon's own among them ([F300](../limitations/F300.md)) |
| rename with `RENAME_EXCHANGE` | `EINVAL` |
| a daemon's own name (§3.1) | hidden: lookup `ENOENT`; make and rename to it `EPERM` |
| a file manager's trash directory (`.Trash…`) in the root | mkdir `EPERM`: what is removed goes to OneDrive's recycle bin |

In a read-only folder every request that would change it is `EROFS`, before anything else is
asked.

Pending reads keep their replies separately from their tasks. A kernel `INTERRUPT` answers
that request `EINTR` exactly once, then aborts its task; mount shutdown answers all pending
reads `EIO`, including reads arriving after the stop. Completion and cancellation take the
same reply, so neither can answer it twice. The aborted future drops its active range reader
and removes its request. Replies queued for writes or hydration also have a guard that
explicitly answers `EIO` if their task or job is dropped, without fuser's unanswered-reply warning.

### 4.3 Errnos

| A program gets | When |
|---|---|
| `EROFS` | a change in a read-only folder, or in one whose mount asked for its take-down |
| `ESTALE` | the row is gone or shows nothing; the content a handle was opened as is not the row's any more; OneDrive stated another content (§5.5) |
| `EEXIST` | the place is taken in the rows, or held on disk by what no row says, or the name in OneDrive by an item the folder cannot show, as OneDrive compares names ([F304](../limitations/F304.md)) |
| `ENOTEMPTY`, `ENOENT` | the store's refusal of a removal or a make ([F299](../limitations/F299.md) for a directory that holds in OneDrive what it does not show) |
| `EACCES` | the download guard refuses remote content ([download-guard.md](download-guard.md)); local bytes remain readable |
| `EAGAIN` | OneDrive asks the account to wait, and the content is needed now (§5.6) |
| `ENOSPC`, `EDQUOT` | a download or a write ran out of space; or the reserve prevents a verified whole download required for a read |
| `EIO` | the store failed; a download failed after its tries; a row against the rules; a pending read or queued write is cancelled by mount shutdown |
| `EINTR` | the kernel interrupts a pending read |
| `EBADF` | a handle that holds no file, or may not write |

A download's own failure is clamped to `EPERM`, `EIO`, `EAGAIN`, `EBUSY`, `ETXTBSY`, `ENOSPC` or
`EDQUOT`; anything else the local filesystem reports becomes `EIO` (`guards.rs`).

## 5. Reading a file that is not on disk

### 5.1 Ranges of the content opened

A read through a handle opened not on disk is decided on the mount's thread
(`ItemKernelRequests::decide_read`, `item_read_origin.rs`), against the row as it is now:

- the content landed since the open, or is kept in the row from before a writer's open: the
  handle goes on from the backing file, opened through `open_at_row_path`;
- otherwise the range is fetched from OneDrive on the runtime, by the item's binding, and the
  kernel's interrupt aborts it.

A range asks the content source for the program's bytes (`ItemRangeFetcher::fetch_range`). Over
Graph, a cache miss fetches the aligned 1 MiB block containing the range, clipped to the file's
size; a range crossing blocks fetches from the first block's start to the last block's end.
Listed reads trim complete cached blocks from either edge of an aligned miss,
so a playback read crossing a block boundary fetches only its missing edge. The
answer is assembled only from version-checked bytes of that opened generation;
concurrent eviction can fall back to the checked metadata path. A missing middle
still uses one contiguous request. Files of at most 256 KiB retain their whole-file fetch. The same rule applies on both paths:

- **By the listed version, in one request.** Where the row's OneDrive version is the opened
  content, the read carries the eTag and size OneDrive listed with it, and the OneDrive id of
  its folder (`decide_listed_content`, `ItemLookup::find_listed_folder`). The source asks the
  range of a download URL of the file and takes the answer only if its `ETag` is that eTag and
  the whole size it says (`Content-Range`, or a `200`'s length) is that size. The URLs come from
  one listing of the folder's children with their download URLs (`graph_source/links.rs`: up
  to 1,000 children and at most six pages, kept 40 minutes, at most 10,000 URLs). Each page
  publishes its links at once: a read waits only until its item arrives or the listing ends,
  while remaining pages continue in the background, one listing per folder. A failed listing
  is not asked again for a minute. URLs also come from earlier metadata reads, only where
  the download ETag matched that metadata's eTag. Accepted blocks and small whole files of
  the same opened content share a 64 MiB memory bound (`graph_source/read_blocks.rs`), oldest
  out first, each kept five minutes from fetching. Adjacent cached blocks can answer a crossing
  range together. Expired URLs and bytes are freed on lookup or insertion; bytes are also
  dropped when the item is forgotten. The disk tier below keeps only small intervals
  actually returned to a reader.
- **By the metadata, in three.** The item's metadata, the range from the address it gives, and
  the metadata again, which must not have changed. Whenever the first way cannot be taken: no
  listed eTag (a listing found another content the step has not taken yet, a content changed
  here), no folder, no URL, an expired URL, an answer with another `ETag` or size or none said.
  A URL that expired, failed with an invalid response, or answered another version is
  dropped; the metadata's URL replaces it only if its download ETag matches the metadata.
  Network failures retain their usual retry behavior. The aligned piece is kept only after
  the second metadata check, and only under a compatible opened generation.

Before either cached URLs or cached bytes are used, the source checks that the account has
an authenticated, fresh access token, in memory without a secret lookup or network call.
Otherwise it takes the authenticated metadata path. Token invalidation, sign-out and detected
grant revocation immediately clear both caches and cancel background listings. A refused
opened-generation binding fails before any request, including a folder listing.

A download URL is the item's, not one version's: after an edit it serves the new content, with
the new eTag as its `ETag` (measured on OneDrive personal, 2026-10-29). The eTag moves at a
rename or move too, so such a file is read by the metadata until the row has its new eTag.
Every range of one opened content is held to the version the first was, by whichever way it
came: its `ETag`, or its metadata version and response `ETag`. A changed version is `ESTALE`.
Eviction or expiry of bytes never resets this version binding. Old cached blocks may still
answer the old content, but a later uncached block must prove that same version.
An answer that starts elsewhere than asked, or ends early, fails the read with `EIO`. A listing
moves no content; the download guard counts the bytes each read asks, also those served from
memory or the header cache, as before.

Measured on 2026-10-29 from Chile on a personal account (read-only): a metadata request took
0.7–2.1 s, a 16 KiB range through a download URL about 0.35 s, and a listing of 200 children
with their URLs about 1.6 s. Gwenview, opening one screenshot of a folder of 200, reads the
first 16 KiB of every image in the folder one after the other on its window's thread: by the
metadata each took 1.3–2.1 s and the window froze for about a minute (its crash on close that
day was its own, set off by the slow reads). By the listed version the folder costs one
listing and one range request an image. Later that day, a 491,408-byte screenshot cost 22
small reads (mostly 4 KiB) at about 210 ms each, about five seconds. With aligned blocks,
the regression's 22 probes cost one listing and one range request; the range path for a
5 MiB file in 128 KiB pieces costs one listing and five ranges (or five metadata/body/metadata
exchanges without listed facts). Threshold-triggered whole hydration is separate (§5.2).
These are fake-server request counts, not a new live latency measurement.
While a file is read as ranges it holds one slot
of the transfer pool as a file being opened does, until two seconds after its last read
(`range_readers.rs`): what a program reads goes before the background downloads.

### 5.1.1 Reading ahead during a desktop folder scan

**Persistent header ranges.** After the memory store misses, foreground reads look in a
per-account SQLite cache (`config/header_cache/`, `graph_source/headers.rs`) before asking
OneDrive. It holds only the exact bytes actually answered through the range-read path,
including memory hits, when the requested interval is at most 64 KiB. Read-ahead still
fetches the first min(size, 1 MiB) into memory, even when a MIME head persists: a head
cannot answer later Exiv2 probes beyond it. The disk tier never suppresses that work or
replaces a full memory block with a late head lookup. It never retains unasked read-ahead
or block padding, and never reads hydrated backing files. Up to 256 KiB of ranges per item and opened `ContentVersion` are kept, first come; overlapping stored
ranges conservatively consume that budget too. One row must cover the whole request:
partial coverage misses, without stitching disk rows or mixing sources. An in-memory
catalog of the stored intervals and version facts is loaded on the cache worker at
startup and updated after publication, eviction and invalidation. A range absent from
that catalog skips SQLite immediately, including during startup. Covered memory reads
coalesce their last-use timestamps on the indexed rows instead of queuing duplicate
content or one LRU command per read; covering queued publications are coalesced too. This leaves the bounded content queue available for new
foreground heads. The catalog stores metadata, never speculative or cached content bytes.

Rows preserve the original size, modification time, cTag/hash and first-range metadata /
endpoint ETag binding. Lookup uses the same full opened generation as the memory store,
checks authentication again after the lookup, and restores only compatible bindings.
The metadata eTag is retained separately: a checked download may legitimately use a
different endpoint ETag or none. Only metadata eTags are compared for freshness.
When current listed facts say another metadata eTag (including a rename with unchanged cTag),
the stale disk binding misses and is deleted before it can prevent later uncached reads. A
changed generation deletes that item's other versions; content-change/deletion `forget`
also deletes its rows. The account owner registers token invalidation even with no mounted
source: it fences queued work immediately and clears the database. A missing stored sign-in
also clears it at startup. Sign-out waits for its worker to close and delete the file and SQLite sidecars,
and reports cleanup failure after signing out; account removal signs out too. A failed
deletion blocks disk lookups and writes until deletion succeeds. A private deletion marker
retains this obligation across restarts; opening the cache retries deletion first.
Free Up Space leaves these small ranges available for the
next listing. They are not a hydrated copy and change no badge, guard, scan, per-handle
coverage, launch-intent or whole-download decision.

The cache lives at `$XDG_CACHE_HOME/konedrive/<account-id>/headers.sqlite`, in a 0700 account
directory with a 0600 database. Schema 1 (`PRAGMA user_version`) stores item id, serialized
content version, offset, bytes, source facts and last-use time; no file paths or download
URLs. `[transfers] header_cache` sets its per-account disk limit in MiB (default 256,
clamped to 0–4096 with a warning). Zero disables it and deletes the file at startup; a
changed setting takes effect at the next daemon start. Rows unused for 30 days expire
at startup, lookup and insertion; the worker also prunes hourly while idle.
Insertions evict least recently used rows in batches; full auto-vacuum bounds the actual
SQLite file, including indexes and metadata, rather than only the content bytes.

An account writer runs publication, maintenance and file management; an independent
read-only connection and worker answer known disk hits without waiting behind its queue.
SQLite lock contention waits at most 5 ms; the overall foreground lookup waits at most
20 ms per attempt before falling through to the existing memory/network path. On a miss,
the mount can try both `prepared_range` and `read_range`, each with that budget. A slow
disk cannot extend that FUSE lookup wait. Cancelled queued lookups are skipped.
Invalidation fences both workers immediately; sign-out and rebuild close the reader
with an acknowledged barrier before deleting or replacing the database. Rebuilding a
published database advances the epoch and suspends catalog admission until the replacement
inventory is published, so old lookups cannot reopen the closed database generation. An
existing read-ahead request and a known disk hit race to answer a foreground
prepared range; either may finish first. If the speculative owner disappears during a
lookup, memory is checked again before acquiring a foreground pool slot.
Lookup answers enqueue expiry, version-pruning and LRU maintenance on the writer
without waiting for it. Dirty last-use timestamps are bounded by the stored catalog rows. After 50 ms
without a hit, the writer flushes at most 64 in one transaction; active warm scans
do no LRU writes. An insertion or shutdown flushes pending timestamps before eviction,
and invalidation discards them. Up to 64 best-effort miss/version-pruning observations
may queue separately, without consuming content slots.
Writes never wait for that worker or queue space: at most 1024 records and 4 MiB of content may be queued, and excess writes are skipped.
The independent byte bound lets small 4 KiB probes use the same content budget that
previously allowed only 64 maximum-sized records. Read/write failures are warned once and ignored;
awaited deletion failures are returned to the caller. A corrupt or
unsupported database is deleted and rebuilt once, then disabled on further failure;
lock contention disables it without deleting another process's database. One startup info
line reports its disk size and row count; hits are debug messages.

On 2026-10-30, opening one image in Gwenview in a folder of 653 cloud-only PNGs
made Qt/KIO sniff the first 16 KiB of each file serially. At about 360 ms a request,
listing took about four minutes. Aligned blocks save repeated probes of one file;
reading ahead hides the latency between different files without changing Qt, MIME
rules or the computer's configuration.

`mount/read_ahead.rs` recognizes head reads (starting below 1 MiB) of two distinct
cloud-only files in the same folder, by the same admitted desktop guard identity,
within three seconds. Repeats of the first file alone do not trigger it. Executable
identities, program hosts, background readers, unknown callers and the daemon's own
bridge never qualify, even with a session or always allowance. A block stops a scan,
including an always block temporarily overridden for explicit reads by a session allowance.

The window is the next 16 eligible files in readdir order, with at most six requests
at once. Readdir's handle snapshot is used when available; otherwise `list_children`
gives the same `ORDER BY local_name`. Only cloud-only files with an extension already
seen in this scan qualify, case-insensitively. Each is rechecked against its current row
before fetching. At most one scan runs per mount, so other programs and folders cannot
multiply the speculative memory budget. Switching scans cancels the previous work.

A piece is the whole file through 256 KiB, otherwise the first aligned 1 MiB block.
The window therefore accounts for at most 16 MiB inside the existing 64 MiB range
store, leaving room for consumed pieces and ordinary reads. Six overlapping requests
hide the observed per-file latency while bounding network concurrency. `[transfers]
read_ahead` selects 0–16 files (default 16); zero disables both speculation and scan
threshold suppression, so every foreground range counts as before. Out-of-range values
are clamped with a warning.

Read-ahead has its own six-request limit and takes no foreground pool or range-reader
slot. A real read of a held piece needs no network slot, and a real read reaching a
pending piece joins that request before taking any foreground slot, instead of fetching it twice. A joined real read retains the shared fetch when the scan is cancelled or replaced;
stopping speculation cannot force that read to retry. Other foreground
requests can start while all six prefetches are in flight. This isolates slot admission;
network bandwidth is still shared. The source respects authentication and account throttle
state, and speculation uses only the listed eTag, size and download URL proof of §5.1,
never the metadata fallback. It retains exactly the same single-version binding (F297).

Speculation touches no disk, download-guard budget, `download_after` coverage, whole
download or hydrating state. Its messages are debug read-ahead messages. Real reads
still undergo ordinary guard admission and charging, including hits in memory.

Once the second distinct head activates a scan, reads by that identity in that
folder use coverage private to the open cloud-only handle, for its opened
`ContentVersion`. They never touch item-level coverage, including the activating
head and non-head reads. Handle coverage includes successful reads from before
scan activation. When that handle's own distinct returned ranges reach
`min(download_after, size)`, and the file is larger than 16 KiB, it starts the usual
guarded whole download. Separate handles never combine. A 16 KiB MIME peek cannot
land a larger image; a decoder reading 16 KiB then the remaining 30,258 bytes of a
46,642-byte image on one handle can. Sparse Exiv2 reads count only their union,
never the gaps. Files of at most 16 KiB remain in memory during a scan unless they
have launch intent (§5.2). A neighbour decoded in full by the viewer's preload can
also qualify; the daemon cannot distinguish that decoder from the viewed file.

Detection lives in a mutex-held record updated synchronously before the foreground
fetch, using folder facts from the row that decided its origin. It works even when
the scheduling actor has not run or its queue is full. No lock spans an await.
Outside a scan, ordinary item-level coverage resumes; scan bytes are never replayed
into it. Expiry, cancellation, replacement or lost eligibility ends the scan; guard
notifications about other readers do not. A debug line records handle qualification.

A desktop identity's outside-scan threshold download of a file at most 16 KiB waits
three seconds. If that identity activates a scan in the same folder during the
pending read or grace period, the promotion is dropped, even if that scan is later
replaced. This prevents a stray first tiny file landing before detection. Repeated
reads share one pending promotion per identity/content; expiry rechecks the row,
then rechecks current policy, scan cancellation and mount lifetime after the store
await, immediately before the space check and download start. Charging is bound to
the reader before waiting, so an
exited or reused PID cannot lose or inherit the asking. Launched files, larger files and guarded readers do not wait.
A whole download already started is never cancelled by scan detection.

A failed or cancelled foreground read cancels its scan; the scanning identity losing
eligibility, three seconds without a head, mount stop and sign-out cancel pending work
too. One scan lasts at most five minutes, and cached pieces retain the range store's
existing five-minute expiry.
The fake-server regression scans 40 mixed-size PNGs with 300 ms content latency and
requires at least four times the speed with exactly one content request per file.
The VM scenario exercises this through actual FUSE readdir and an app-scoped caller.

### 5.2 The threshold

Distinct bytes returned to programs as ranges are counted per item and opened content:
the union of the returned ranges, clipped to the file's size. Repeated or overlapping
reads add only their new coverage; bytes fetched ahead into aligned blocks add nothing.
The download guard continues to admit each program's requested bytes before fetching,
including repeats and cache hits ([download-guard.md](download-guard.md)).
Once distinct coverage reaches `min(download_after, file size)` (`[transfers]
download_after` is 1 MiB by default; 0 downloads at the first read), the file is
downloaded whole, with no waiter: reads continue as checked ranges until the verified
backing file lands. Every cloud-only handle also records its successful coverage
outside scans; item coverage remains the decision rule there. During a desktop
scan only that handle's full coverage counts, including bytes returned before
activation, and
only files larger than 16 KiB qualify (§5.1.1), even with `download_after = 0`.
The whole download fetches afresh and checks quickXorHash; cached ranges and unfinished
staging are never a verified landing source.

Outside scans a record is chosen before fetching, so a late fetch updates only its
original content's coverage. A changed opened `ContentVersion` replaces that item's
record; old and new handles can undercount, never combine versions. Coverage clears
at threshold, successful whole download, or `forget_item`. Both item and handle
coverage keep sorted, coalesced intervals, at most 64. Above that cap the closest
separated pair is reduced to its longer interval (the later on a tie); gaps never
count. The item map holds at most 4,096 records with arbitrary eviction. No scan
pins it. A handle's record is created at cloud-only open, snapshotted into each
`RangeToFetch` with `fh`, and removed at release; outstanding reads retain it only
until they finish. These bounds can delay downloads, never start them early, and do
not change admission accounting.

**Launch intent.** On the first lookup, attribute/access query, directory request,
open or inherited-handle read carrying a non-guarded desktop process's PID, its identity-validated `/proc/<pid>/cmdline` snapshot is
parsed once per remembered process generation. At most 64 arguments, each at most
4 KiB, are examined. Absolute paths and percent-decoded local `file://` URLs under
this mount are resolved with the store's path index alone: no path is statted,
opened or canonicalized. Directories, unknown paths and resident files are ignored.
Up to 1,024 process generations are remembered per mount; eviction loses intent.

Each cloud file is marked launched for that desktop identity for thirty seconds,
or until its owning process generation vanishes or loses eligibility (rechecked each second). Its first admitted read starts a whole download immediately,
regardless of size, threshold or scan, still subject to guard charging and the usual
space reservation. Other processes with that same identity can consume the intent.
The range itself is answered normally. The immediate hydrating emblem expires after
ten seconds without a read; a download started while it is shown inherits that mark
until completion (§3.5). Mount stop cancels intent timers and pending tiny promotions.

Single-instance activation over D-Bus, Flatpak portal paths and navigation within an
already-running app supply no new argv intent. They fall back to item/handle coverage;
small images viewed during a scan can therefore remain cloud-only. Dolphin's open
alone is not launch evidence: an open also serves browsing and other automatic work.
`read_ahead = 0` disables scan detection and handle-only counting; tiny desktop files
still wait three seconds, with no scan cancellation, and launch intent still works.

Measured on build `1983691`, 2026-10-30: Gwenview opened one image in a folder of 653
cloud-only PNGs and probed each image it passed with `16384@0`, four `4096@0` reads,
`33@0`, `4096@33`, 4 KiB at each 64 KiB offset to the end, then `16384@0` again. It did
not read those files through. Counting repeats had made 19 small screenshots (18–56 KB)
land on disk; one 20,187-byte file returned 40,700 bytes across eight reads in the trace.
The regression's stated header/stride pattern covers only 16,384 distinct bytes of a
20,187-byte file, or 98,304 of a 1,360,978-byte file, and starts neither whole download.

### 5.3 The whole download

`ItemDownloader` downloads a file by `ItemToDownload`: the item, its binding and the content to
download. Who asks — a read at the threshold, an open for writing or a truncation, `Files.Hydrate`,
the plan's step for a pinned file — is a `DownloadAsker`. A download of the same item and content
that runs is joined, whoever asks; a download of another content of the same item is another
download, and the older one's landing finds the row changed and drops its file.

One download, in its slot of the transfer pool (§5.6):

1. **Fetched** into a nameless file, or into the unfinished file of an earlier try of that item
   and content, which the download states keep for the latest 16 that failed. Each fetch asks Graph
   for the item afresh — size, cTag, quickXorHash and a download address — and streams from that
   address with a `Range` from where the bytes stop (`content_source/graph_source.rs`,
   `konedrive-graph/src/drive/mod.rs`). A `200` to a `Range` request has its first bytes skipped,
   so the stream starts where it says; a `416` is an empty stream; a `401`, `403`, `404` or `410`
   from the address is an expired one, replaced once.
2. **Held against the asked content.** Every answer — the first, one after a break, each piece of
   a download in parts — is held against the content asked for before a byte of it is written
   (`item_downloader/download_source.rs`): one of another content ends the download (§5.5). An
   answer that says no size is refused; a size of 0 said against a content that has bytes is
   another content.
3. **Checked** (`content_source/source/download.rs`, `guards.rs`). The quickXorHash is computed
   over the bytes as they are written, and a content OneDrive gives a hash for is never landed
   unless it matches; a mismatch starts the download over once, and a second is `EIO`. A file
   OneDrive gives without a hash fails closed with `EIO` ([F34](../limitations/F34.md)).
   A read error or a short stream is a break: the next fetch asks from where the bytes stopped,
   after 200 ms and then 400 ms; the third break ends the download with `EIO`. A minutes-long
   outage is therefore answered promptly, rather than holding the reader indefinitely.
   A later whole-download request tries anew and resumes the checkpoint if usable; its
   successful landing clears the failed state on the file and its ancestors (§3.5).
4. **Checkpointed**: every 16 MiB of a content with a hash, the bytes are synced and
   `user.konedrive.progress` written on the file. An unfinished file is continued only for the same
   cTag, after its bytes are read back into the hash. The file has no name, so nothing of it
   outlives the daemon. After a daemon restart a download starts from zero; interrupted
   bytes are never exposed as a complete resident copy. Upload session progress, in
   contrast, is persistent ([writes.md](writes.md) §4.3).
5. **Given its time and the marks of a whole copy, and synced**, by the downloader with no lock
   held: the landing's job waits for no file to reach the disk.
6. **Landed** (§5.4), then announced (§9), and its waiters answered.

**A large file in parallel parts.** A download that nothing waits to open (a pinned file) of
100 MiB and up goes in pieces of 256 MiB (`content_source/source/parts.rs`). Its first stream runs
in the slot the download took; extra streams each take a free large slot of the pool, only while
nothing waits for a slot, and give it back after a piece when something does, or when another file
in parts has two streams fewer. Each piece is hashed at its offset and the pieces combine into the
whole file's quickXorHash. The checkpoint counts the bytes from the start with no gap. Three breaks
of one piece fail the download.

### 5.4 Landing

The landing (`item_download_landing.rs`) is one job of the write-operation thread. It reads the row
and its path, writes the record (`LinkFile`, with the content that was downloaded), begins a place
change, links the file under the name the row has by then — following a rename made meanwhile —
with the directories above made and marked, syncs each directory a name was made in, and commits
conditionally: for a program, the item's local content must still be the one downloaded
(`commit_landed_content`); for the plan's step, every version it was planned from
(`commit_download_content`). The record goes in the commit.

| The landing finds | What happens | The waiter hears |
|---|---|---|
| the row as downloaded, not on disk | linked; the row says on disk | landed |
| that content on disk already (a program's download) | nothing linked | landed |
| the row changed, the item gone or not shown any more | nothing linked; or, when the commit finds it, the named file removed again and the record forgotten | `ESTALE` |
| something at the name though the row says not on disk | nothing replaced | `EEXIST` |
| the named file cannot be removed again | the record stands; take-down (§8.1) | the errno |

### 5.5 Another content

A program never reads the bytes of one content at the size of another. How a read learns that
OneDrive has another content than the one it was opened as:

- **A listing's step** changes the row's local content: every handle opened as the old content is
  `ESTALE` from then on, and the next open reads the new one.
- **A download's answer** states another content: the download ends at that first answer, before a
  byte is written; its waiters hear `ESTALE`; nothing lands; and the download states remember the
  fact for that item and content (the latest 256). It answers nothing by itself: a read of a
  remembered content asks for the whole download anew, or joins the one that runs and has no
  answer yet, and waits for it. The fact is void at the first answer that download accepts. Stated
  another content again: `ESTALE`, remembered again. Stated the asked one: it lands, and the read
  gets its range. A file OneDrive does not have, and an answer that says no size, are failures of
  that one asking and are not remembered.
- **A range** carries the listed eTag and size its answer must say, or metadata checked before
  and after its body, and is held against the opened content and the first range's version. A
  changed cTag/eTag or supplied hash is `ESTALE`.
  Individual ranges have no whole-file hash proof ([F297](../limitations/F297.md)).

### 5.6 The transfer pool, a throttle, a pause

Foreground and sync transfers take a slot of the account's pool (`konedrive-graph/src/pool.rs`):
downloads, uncached ranges, uploads and metadata requests. Speculative read-ahead has its
own six-request limit (§5.1.1); held or joined pieces need no additional foreground slot. The pool starts at 16 slots and grows by one for
each transfer that succeeds while work waits and every slot is busy, up to `[transfers] max` (32 by
default, 1 to 256). A `429` or `503` halves it, once per burst, and no slot is handed out for the
whole `Retry-After` (10 s when none is given); for five minutes after that wait it grows by one
slot per round only. A free slot goes first to a file being opened, which may take two slots
above the pool, and while any open waits or runs nothing else takes a new slot; then metadata;
then background downloads and uploads in turn. The classes are `Class::Open` (a read, an open for
writing, `Files.Hydrate`), `Metadata`, `Download` (the plan's pinned files) and `Upload`. At most
`[transfers] large` (4 by default) streams of large sync transfers, 100 MiB and up, run at once; a
file being opened is never counted large. Every number is chosen, not measured against a real
account.

**A throttle.** While OneDrive asks the account to wait (`status::throttle_wait`), what a program
waits for fails at once with `EAGAIN` and fetches nothing: an open to read a file not on disk, a
range, the download before a write or a truncation, `Files.Hydrate`. A download that runs when the
wait starts is given up where it waits for its slot or for OneDrive's answer, and keeps its
unfinished file; one whose bytes are coming is read to its end and lands. The plan's download of a
pinned file nobody else asked for waits the time out. The first file refused during a wait is told
once, and a refusal is no failure in the activity log.

**A pause** holds back everything but opens. A download the plan started that still waits for its
slot when a program asks for it gives that wait up and takes an open's slot: a program is never
kept waiting for a pause. Pausing is described in [writes.md](writes.md).

## 6. Freeing up space

Freeing up removes a file's downloaded copy; the file stays in the folder, not on disk
(`item_free_up.rs`). Each file is freed as one job of the write-operation thread, by the first case
of `FreeUpDecision` that holds:

| The file | Free-up |
|---|---|
| a pin holds for it ([pinning.md](pinning.md)) | left |
| changed here, or never in OneDrive | left: its copy is the only one |
| open for writing | left |
| being downloaded, or a step runs for it | left |
| the local content is the synced one | freed |
| not on disk, or a directory | nothing to free |

The row says not on disk first (`commit_freed_content`, conditional on the content decided on);
then the copy is set aside and removed, and the change announced (§9). A copy that cannot be
removed is said on disk in its row again. A daemon that dies between the two leaves a whole copy
where the row says not on disk, which the start-up check takes for a copy that landed before its
row (§7.2): nothing is lost. A program that reads the file through a handle keeps reading the
removed copy; the next open reads from OneDrive.

A directory is freed file by file, a part of its rows at a time, each file in its own job, so that
a program's change waits for one file of a long free-up. `Files.FreeUp` and `Folder.FreeUpSpace`
are the bus's ways in ([desktop.md](desktop.md)); `konedrivectl sync dehydrate`, `sync free` and
`sync free-up-space` are the command line's.

**A copy that is missing.** An open that finds nothing where its row says the content is on disk
(at a moment no place change crossed) has the row mended when it is a clean copy: written not on
disk, and the open reads from OneDrive. A row whose content was changed here is left for the next
start-up check.

## 7. The start-up check

### 7.1 One place at a time

The start-up check (`backing_directory_check.rs`, called by `start_check.rs`) runs in the mount's
build, before the mount goes over the folder and before any part reads the rows; nothing else reads
or writes the rows or the backing directory meanwhile. It is the one place where the disk is read
to learn what the folder contains. **It does not guess what a person did while no daemon ran**: it
looks at one place at a time, what the rows say of it and what the disk has there, and nothing is
matched by a mark across places ([F298](../limitations/F298.md)).

1. **The records first** (`standing_place_changes.rs`, decided by
   `place_change_settlement::decide_settlement`). A row that was written is not undone. A made entry
   of a new item gets its row. A move or a set-aside is undone on disk, so the rows stand as they
   were and the plan or the person does it again; no row is moved or removed by a record. The
   backing file of a writer's open that was never made puts the row back as it was before the
   open. A landed file whose row still has the downloaded content is on disk; one whose row has
   another content by now is removed. An undo that cannot be done (the directory is gone, the name
   is taken, the entry at the new place is not the one moved) is not tried again: the record goes
   and the check decides the places as it finds them. A record stands only where a place it names
   cannot be looked at.
2. **The rows**, read whole once (`item_rows.rs`).
3. **The backing directory**, read once, a level of directories at a time on up to eight threads,
   to 128 levels, with the marks of each entry (`backing_directory_contents.rs`).
4. **The walk** over every place, a directory before what is in it (`place_walk.rs`): each place
   decided by `disk_finding::decide_finding`, a function with no disk and no store in it, and
   written through `row_writer.rs`. A row that leaves the folder leaves first, with what is below
   it; then the entry at that place, if no row has it now, becomes a new item.

### 7.2 The cases

A file's content is compared by its size, its time (nanoseconds for changed-here rows, whole
seconds for hashed rows), and its cTag mark where the
row has a cTag (`compare_content`).

| The row says, at that place | The disk has there | Finding |
|---|---|---|
| a file or a directory | an entry of that kind | the item; a file's content the same (`NothingChanged`) or changed here (`ContentChanged`) |
| a file a writer held open when the daemon stopped | a file | changed here, whatever its size and time say: bytes may have been written that neither shows |
| a file not on disk | a whole copy marked with the row's item id, of the row's content | the copy, landed before its row (`CopyLandedBeforeRow`) |
| a file not on disk | any other file | content put there: changed here |
| a file of OneDrive not on disk, or a directory of OneDrive | nothing | nothing changed: neither needs an entry |
| a file of OneDrive on disk | nothing | not shown any more (below) |
| an item of OneDrive | an entry of the other kind | not shown any more (`NotShownAnyMore`): its local version and its synced version are taken away, its binding and its OneDrive version stay, and the plan shows it again as OneDrive has it. **Nothing is removed in OneDrive**, also when the copy was changed here and not sent |
| an item OneDrive never had | nothing, or an entry of the other kind | the row goes (`Removed`) |
| nothing | an entry marked with an item id no row has, which no other entry carries | a new item bound to that id (`MarkedWithoutRow`) |
| nothing | any other entry | a new item (`Made`) |

The check writes marks for two things only (`mark_change.rs`): the item id on the own entry of an
item of OneDrive, and a mark taken off an entry that becomes a new item of the user's own. A file
with several names is left as its marks are: the marks are the file's, and another name of it may
be another item's.

### 7.3 What cannot be read

A part of the backing directory that cannot be read — a directory that cannot be listed, one
deeper than 128 levels, a name that is neither a file nor a directory, marks that cannot be read,
a record that stands — is left as it is: no row in it or below it is changed. Those rows are held
(`DiskFindings::list_held_items`): nothing of them, or of what is below them, is sent for as long
as the mount of this start is up, and the queue on the bus shows them as "could not be read on
disk" ([writes.md](writes.md)). Each path is said in the journal with its reason. No other row, and
not the folder, is held for them.

### 7.4 A lost store

A missing store is made again from a full listing and the backing directory. Unsupported or
corrupt stores are preserved in quarantine first; readable pins and exceptions are reinserted
before listing, pending removals/moves exported for manual recovery, and a count/path shown as
trouble. Newer schemas refuse to start; supported predecessors migrate from a durable snapshot
([store.md](store.md), [D36](../limitations/D36.md)).

An entry marked with an item id that no other entry carries is bound to that id. Every recovered
file is changed locally, even a whole copy with a cTag mark: the planner hashes it before agreement.
An unmarked entry is a new item. Missing backing copies never manufacture cloud deletions.

## 8. The mount's end

### 8.1 The take-down

A part that cannot keep the disk and the rows saying the same — an undo that failed, a backing
file that could not be made after its row, an entry set aside that could not be put back — asks
for the mount's take-down (`MountTakeDown::ask`). The ask holds the folder read-only for good, so
no further change is taken, and reports the cause to the owner of the folder's state, which stops
the sync, takes the folder down and starts it again after a growing pause
([sync.md](sync.md) §9). The new start's check settles the change from its record.

### 8.2 The stop

`ItemMount::stop`, once, in this order: the write-operation thread (the job in flight waited for,
the queued ones dropped); the bridge; every download aborted, its waiters answered `EIO`, and none
started from then on; every read that waits for OneDrive aborted; the mount's connection aborted
through `/sys/fs/fuse/connections/<n>/abort`, so that a program still holding a file does not keep
the unmount waiting; the mount taken off and its session joined; the reading connections closed;
and the write bits taken off the folder on disk. A program that held a file of the folder gets an
error ([F296](../limitations/F296.md)). A Forget gives the write bits back for good.

### 8.3 A stale mount

A daemon that dies leaves its mount on the folder, and every path in it then fails with "not
connected". It is told without trusting the folder's attributes, which the kernel answers from
memory for a second after the daemon's death (`stale_mount.rs`): the kernel's table of mounts says
whether a FUSE mount lies over the folder, and `statfs` of it, which the kernel hands to the
daemon each time, is refused at once by a mount whose daemon is gone. The question is asked on a
thread of its own and waited for two seconds: a mount whose daemon answers slowly is left alone. A
stale mount is taken off with `fusermount3 -u -z`, by the mount's build and by the tidy step.

### 8.4 The tidy step and the write bits

While a mount lies over the folder the backing directory is the daemon's alone and has its write
bits; while none does, nothing has them (0555 and 0444), so nothing is put into the folder by
mistake. The build gives them back before the start-up check, a stop and a build that fails take
them off.

The tidy step (`tidy_step.rs`) follows the daemon's end however it ended: the unit's
`ExecStopPost`, `konedrived --tidy`. For every folder the configuration records as showing
OneDrive it takes a stale mount off and, when no mount lies over the folder, removes the write
bits; then it tells the desktop to read those directories again. The folder is then a plain
directory that shows the downloaded files and the files made here that were not sent, and no file
that is not on disk ([F296](../limitations/F296.md)).

## 9. Telling others what changed

The kernel tells whoever watches the folder (inotify, a file manager) of what a program does
through the mount. A step towards the folder, a landing, a free-up and a pin change the backing
directory and the rows behind the kernel's back: it tells nobody, and keeps the pages and the
attributes it cached.

The inotify bridge (`item_inotify_bridge.rs`, `item_announcements.rs`) is a queue of announcements,
each by local item id and place: `Appeared`, `Vanished` with the former place and the kind,
`ContentChanged`, `MarksChanged`; a move is a vanishing and an appearing. The former place is taken
by the step before its commit, since the rows then have the new one only. The bridge's own thread
performs each one as a system call by path through the mount — `mknod` or `mkdir`, `unlink` or
`rmdir`, `utimensat` — so that the kernel tells its watchers. The mount knows the bridge's
requests by the id of the thread that asks, and answers them from the rows and the announcement in
progress, changing nothing:

- a lookup of a vanishing name gets the attributes of the item that had it, with no lifetime; a
  lookup of an appearing name gets `ENOENT`, and its `mknod` or `mkdir` is answered from the rows;
- its `unlink` or `rmdir` is done when the rows show no such name;
- its `utimensat` is answered as getattr. For `ContentChanged` the kernel's cached pages of that
  node are dropped first (`FUSE_NOTIFY_INVAL_INODE`); without it a read through the mount would get
  the old content from the cache.

A program's own requests are never announced. `MarksChanged` tells a file manager to read the
attributes of §3.5's last paragraph again: after a pin, and after the plan changed what the sync
mark says ([desktop.md](desktop.md)).

## 10. Known limits

- **No folder while no daemon runs.** The folder is the backing directory then, without write bits,
  and a program that held a file open gets an error ([F296](../limitations/F296.md)). Only the user
  who runs the daemon reaches the folder, root included ([F301](../limitations/F301.md)).
- **Ranges have no independent whole-file hash.** Their version is proven by the listed eTag
  and size, or by the metadata before and after the response, and changes fail the read
  ([F297](../limitations/F297.md)); a
  file without a hash is downloaded unverified ([F34](../limitations/F34.md)).
- **Programs that read to look download.** A file read whole, or past the threshold, is downloaded;
  Dolphin reads some files to find their type or draw a preview ([K1](../limitations/K1.md)).
- **No links, special files or attributes of the user's own** ([F300](../limitations/F300.md)); the
  executable bit is kept on this computer only ([F303](../limitations/F303.md)).
- **Hand changes while no daemon runs** are a new item and an item shown again, never a move or a
  removal in OneDrive; an edit that keeps the size and the time in whole seconds is not seen
  ([F298](../limitations/F298.md)).
- **A lost store** costs a full listing and local hashing. A quarantine restores readable pins
  and exports pending namespace changes for manual recovery ([D36](../limitations/D36.md)).
- **An unfinished download does not outlive the daemon**: its file has no name, so a large file
  whose download broke is downloaded from its start after a restart.
- **A row the start-up check held** stays held for as long as that mount is up, also when a
  program writes the item meanwhile; the next start's check decides it.
- **A read refused during a wait** of OneDrive's reaches a program that reads through the kernel's
  page cache as `EIO`, not `EAGAIN`; an open is therefore refused at once, which reaches it as
  `EAGAIN`.
- **A directory with nothing on disk** shows the time the mount started, which changes at every
  start.

## 11. Download admission

Cloud-only opens and remote reads use the machine-wide [download guard](download-guard.md).
It owns caller identification and app budgets; the downloader owns backing-filesystem space
reservations. Guard refusals are `EACCES`, before network I/O and without a tree change.
Reads that cannot leave the disk reserve stay as ranges, or answer `ENOSPC` when a verified
whole download is required. Explicit hydrate and pin do not enter the app guard.

### Verified downloads and resident replacements

A remote whole download without quickXorHash fails closed with EIO. A cTag alone is not
hash verification: such bytes are never marked as a clean, evictable copy or exposed for an
editing open. This is deliberately stricter than accepting a hashless download after a
metadata recheck. A supplied hash remains usable when Graph omits the cTag. Local test sources
that say no remote version make no remote-verification claim.

Range reads buffer only the requested range, check its metadata before and after the body,
and keep the first cTag/eTag and optional download response ETag for each opened content
generation. A reconciled generation does not inherit an earlier generation's range identity. A version change returns
ESTALE, including same-size edits; unfinished staging is never served to another reader.

Resident pinned files use `ReplaceContent`; resident files with readers use the same path.
The old version remains named while the replacement downloads, verifies and syncs, through
the existing downloader, guard and space reserve. Under one `SetAside` record, staging is
linked under the item's aside name, then exchanged atomically with the visible inode using
`RENAME_EXCHANGE`. Both directories are synced before a transaction checks all planned
versions and writes the remote content as local, synced and on disk. A changed row undoes the
exchange; the old aside is discarded only after commit. The verified stage waits for readers (including mmap lifetimes) to close;
the final job rechecks under the reader gate. Reader waits run outside the folder lease
and cancel when sync stops; the final mutation holds the lease and revalidates the row. New opens invalidate the previous kernel page
cache. Cloud-only metadata changes also wait for old readers and mappings to close. This avoids mixing old and new backing inodes through one FUSE inode's shared cache.
The wait retains the stage without redownloading it and holds no network transfer slot. A crash before the exchange leaves the old file; a crash after it leaves a
complete verified replacement and the old aside. The existing startup check reconciles the
standing record and content marks before aside cleanup. No new record format or schema is
needed. The replacement record is synced before the exchange, and the committed row is synced before
the old aside is discarded. A failed row sync retains both copies, reports an error and asks
for take-down without undoing the committed row. The old file and replacement occupy space simultaneously; ENOSPC keeps the old file.

Free-up also requires the local place to equal the synced place, no unsent generation or
step-memory hold, and no request or place mutation on record. The store repeats these checks
in the eviction transaction. Recovery uncertainty and published policy holds protect copies
through the mount's protection predicate. A cloud-only rename followed by a remote deletion
stays visible as a conflict for resolution.
