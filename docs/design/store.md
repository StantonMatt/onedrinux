# Store preservation and durability

The store holds user choices and pending work as well as a cache of OneDrive. Losing its
rows can lose the meaning of a move or removal even when every resident byte survives.
`konedrive-tree/src/schema/old_store.rs` admits a store before the original database is
opened for writing. Inspection uses a private, short-lived copy of the database, WAL and any rollback journal
in the state directory, so SQLite cannot change a newer build's shared-memory file.
A per-store ownership lock prevents a simultaneous open or offline reset.

| Finding | Action |
|---|---|
| No store, or no tables | Create schema 33 |
| Schema 33, `quick_check` succeeds | Use it |
| Schema 30, 31 or 32, `quick_check` succeeds | Snapshot, then migrate sequentially |
| Unsupported older schema, missing metadata, not a database, failed `quick_check` | Quarantine, carry over readable state, create schema 33 |
| Newer schema | Refuse; preserve all original file contents and permissions |
| Permission, lock, or I/O error | Return the error; preserve the store |

A newer schema's refusal reaches the account's LastError. It names both remedies: install
the newer build, or stop the daemon and run `konedrivectl store reset <store-path>`.
Reset takes the daemon's configuration lock too; `--config <config.toml>` is required
when using a custom configuration. It never signs in or contacts OneDrive.

## Quarantine

For a store named `fuse.sqlite` (the same applies to `items.db`), the preserved name is
`fuse.sqlite.<version-or-corrupt>-<UTC timestamp>`, with `-wal`, `-shm` and any older rollback `-journal` appended to that
same name. Every rename is followed by a directory fsync. The renamed members are also fsynced
before a replacement is admitted, preserving dirty WAL bytes left by a killed process. A private, durable
`.quarantine-intent` names the destination before any rename. Restart finishes the members
and checks the destination's `.members-moved` marker before opening a replacement: a new
WAL can never be confused with the old one after a cut between the renames.

Readable `(id, is_kept)` pins, including exceptions, are reinserted by OneDrive item id
before the first listing. No item rows or remote operations are replayed. Pending
removals and moves are written as escaped text in `<quarantine>.unsent-changes.txt` in
the state directory; this contains item ids and full old and intended paths resolved from
the quarantined rows, and legacy outbox paths when available. Paths survive retention pruning
of the database they came from. The count and export path persist in the replacement's
metadata and appear in LastError (the issue/trouble surface), including after restart.
The activity log also records the recovery notice. A damaged pins or changes table is
best effort: it cannot prevent preservation of the original files.

The replacement is checkpointed and synced before the intent is removed. Only then can
completed quarantines beyond the newest three be pruned, together with their WAL and SHM.
Pruning requires the exact timestamp format and the daemon's completion mark; other backup
names are left alone. Exports
are retained independently. Forgetting a folder quarantines its store too. Removing an account archives its entire state
directory beside the original as `<account>.removed-<timestamp>`; archived stores are not deleted.
The daemon never unlinks a live or quarantined store except
for that explicit retention pruning. Manually deleting a store is not a recovery step.

## Migrations and snapshots

Schema 30 to 31 retains the thumbnail-index and `is_written` migration. Schema 31 to 32
adds nullable `items.onedrive_folder_ctag TEXT` and discards legacy upload-session opening
windows, whose machine-clock times cannot prove ownership against Graph timestamps. The
column is unused and always null: OneDrive personal gives folders no cTag (measured
2026-10-29), and a folder removal is guarded by the folder's eTag ([writes.md](writes.md)
§4.2). The additive media-facts migration retains it.
Schema 32 to 33 adds `media_facts`, keyed by local item id. It stores compact v1 JSON
of only the allowed image/photo/video fields, bound to the delivering drive/item id,
eTag and all six content columns. An indexed lookup checks the binding and content
before answering; a newer content version cannot use stale details. Every accepted
listing replaces or clears facts, including listings of unchanged versions. Triggers
remove facts with an item's row or lost/replaced binding. Binding transfers preserve
the facts and their original fences in the same transaction, including conflict copies,
merges and atomic saves. No location is stored.

In the same transaction, the migration requests one full listing for backfill, keeping
any earlier outstanding full-listing cause. Beginning that listing consumes the request;
reopening schema 33 never requests it again. The listing changes no local/synced version,
content presence, pin or pending upload for an unchanged tree, and downloads no content.
The one-off network cost is a metadata enumeration (about 12 delta pages for 11,000 items).
New stores create the table immediately and fill it through their usual initial listing.

Every shipped version from 32 onward must retain its sequential migration step. Each
step's statements and version update run in one transaction, with FULL synchronous
commits for the migration itself.

Before migration, `VACUUM INTO <store>.pre-<from-version>` includes committed WAL data.
A temporary snapshot is made 0600, as the store's own files are, fsynced, renamed into place, and its parent synced before the
original changes. Existing snapshots are retained; a migration retry makes another timestamped snapshot so
new WAL-only work cannot be missed. A crash before a step's commit leaves
that step's old version and schema together. A failed migration returns an error and
retains both original and snapshot. A successful mount start stamps each snapshot;
subsequent successful starts may prune it after seven days. A failed start never starts
that clock.

## Content and names after a crash

A recovered file without a row is always changed locally, even when its size, timestamp,
hydration mark and cTag all look familiar. The plan hashes its bytes before agreement;
equal hashes agree without upload, different bytes follow existing upload/conflict rules.
The first write-open clears the old cTag mark before a program can edit. An open writer
left at death is changed locally regardless of the timestamp. Changed-here rows compare
nanoseconds; hashed rows retain whole-second comparison because the daemon set those times.
A missing backing copy clears the local and synced view while retaining the cloud binding:
recovery never manufactures a cloud removal.

Ordinary transactions retain `synchronous=NORMAL`. The WAL is explicitly synced, on its
owning thread and serially with checkpoints, before opening/cutting a resident file after
its write intent; after recording a close; after the backing-file fsync; and for pins.
Flush also syncs the backing file and WAL so close can return a sync failure. The real
kernel release waits for the recorded close. A failed sync is an error, and a failed close
asks for take-down rather than proceeding to further writes.

Place-change intents are synced before disk mutation. A move, replacement, or set-aside
syncs both parents and any newly created ancestor before committing its row; the committed
row is synced before displaced bytes can be discarded. Undo renames sync their parents
before forgetting the record. On a directory-sync failure the operation undoes and syncs
its old names, or retains the intent and asks for take-down when that fails too. Creation
and linking already sync parents. Final unlink of a discarded aside and removal of empty
scaffolding need no extra sync: loss can only leave a duplicate/re-shown entry, never
remove the unique bytes of an acknowledged replacement.

## Measurement and checks

`tests/fuse-probe/store_durability.py --work-dir <scratch-directory>` creates 10,000 64-byte
backing files, commits their rows individually, rewrites 1,000 in place with open/close
row commits, then removes all 10,000 with individual commits. Three alternating pairs,
local Linux filesystem, Python SQLite 3.46.1, 2026-10-28:

| Setting | Total seconds, each run | Median seconds |
|---|---|---|
| NORMAL | 0.894, 1.159, 1.206 | 1.159 |
| FULL | 65.778, 58.155, 68.873 | 65.778 |

FULL added 5,577%, above the 10% threshold, so the explicit-boundary approach was selected.
This focused SQLite/backing-file harness measures the setting's cost; it is not a claim
about whole-daemon FUSE throughput or physical-drive power-loss protection.

Run `tests/fuse-probe/run.sh crash_consistency` by name, separately from other groups.
It uses debug binaries by default, two boots on the same ext4 image, QEMU `cache=none`,
and guest `echo b > /proc/sysrq-trigger`, with no graceful shutdown. It covers an open
same-size same-time edit, rename recovery with at most a duplicate, a missing backing
copy that must not cause a cloud deletion, and a real migration paused after ALTER and
before commit. The fake cloud starts from the same metadata each boot; outgoing work
is paused before the cut. Only the fake server's persisted delta URL is cleared between
boots because its ephemeral port changes; item and pending-change rows remain intact. The
test image is deleted by the runner on exit.
