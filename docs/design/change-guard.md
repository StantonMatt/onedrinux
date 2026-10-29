# The change guard

The change guard admits outgoing changes for each account before Graph receives a
mutating request. It covers file and folder removals, existing-file rewrites, moves,
new uploads and the deletion of upload leftovers. The mount can still accept local
work while sending is held. This is protection against accidental bulk operations by
programs running as the user, like the download guard. A program which deliberately
calls AllowHeld has the same authority as the user's CLI; this is no separate-user
security boundary.

## Limits and quiet time

The defaults are enabled for every account, including accounts with no explicit
`change_guard` setting:

| Change | Ten-minute count | Ten-minute original bytes | Daily count |
| --- | ---: | ---: | ---: |
| Removals | 20 | 1 GiB | 100 |
| Rewrites | 50 | 1 GiB | — |
| Moves | 200 | — | — |
| New uploads | 500 | 5 GiB of new content | — |

A limit trips when the next generation exceeds it. A zero count holds the first matching change; a zero byte limit holds the first
positive-byte change and is useful for a trial. Directory removals count the directory and every
known descendant individually; one DELETE cannot conceal hundreds of files. Empty
directories also count toward upload limits. Rewrites charge the cloud version's
original size, so truncating a large file to nothing is expensive. Successive rewrite
generations of one file count separately; making a plan again or filling its hash does
not count a second time.

Every removal waits thirty seconds after the latest pending removal in its account.
Other kinds use a two-second settling interval after their latest pending generation.
These intervals gather a continuous burst. They run independently of a file's
content's modification time. This gathers a fast burst before its first DELETE.
Quiet times and counters survive a daemon restart. The ledger clamps backwards wall
clock observations so they cannot expire a counter. Each kind has a separate latch:
a trip holds all still-pending changes of that kind, then every later generation until
an explicit decision. The decision releases that latch but retains window counters, so
new work needs fresh admission and can immediately trip again. Other changes continue. A completed small operation before a trip cannot
be undone by this guard; ListHeld's sent rows report it separately; later admissions prune the old sent history.

The plan reads all unsent rows into the ledger before scheduling outgoing work, then
uses `SyncPlanInput.held_for_approval` and `change_quiet_until` in its ordinary bounded
reading. Held rows become `Reason::HeldForApproval` and a blocked sync state, giving the
existing folder aggregate/error emblem. They are omitted from Issues and its blocked
badge: the app lists them on On Hold. Local-only uploads instead use `Reason::LocalOnly`
(`local-only`, per-file), remain in Issues and its badge, and have an Upload action. The sender checks again, and
metadata writes and cleanup recheck scope and admission after acquiring their slot.
Uploads recheck after a transfer slot is acquired and before every mutating request
and chunk. A request already in flight can finish; the guard cannot retract a request
OneDrive has received.

## State and approval

`ConfigStore` owns one ledger per account. The switch and every limit live in one
daemon-wide `[change_guard]` table, including before any account exists. Updating a
setting configures every open guard under the configuration/registry lock; new accounts
use the same values. On read, legacy `[accounts.change_guard]` tables migrate atomically
into the shared table and are removed. If they disagree, enabled is true if any account
had protection enabled, count/byte limits take the minimum and windows the maximum.
Accounts without a legacy table contribute the guarded defaults. An existing shared
table wins. This preserves the strictest previous protection without guessing which
account's settings the user preferred. Migration write failure refuses the configuration.
The separate
`accounts/<id>/change-guard.json` file holds the window counters, latches, current
versions, decisions, retained-copy names and recent sent history. It changes no store
schema. Each write replaces a private temporary file, syncs that file before rename,
and syncs its parent directory. One mutex spans counting, the decision and persistence.
An unreadable or unwritable ledger stops outgoing admission; it never silently resets
or overwrites damaged evidence.

Restore persists a new copy's local-only hold before committing its SQL row. A failed
hold write rolls back the row and the disk rename. Startup may adopt a made entry under
a new local id; only its conservative local-only hold can follow the retained path.
Approvals never follow a path to a replacement item.

`ListHeld` and `ChangesHeld` publish an opaque `epoch:sequence` token. The random
epoch is persisted before its first publication, and the sequence advances for new
generations, provisional writers and newly held membership. A decision under the same
mutex selects only held entries whose generation and membership were shown by that
token. Later holds are untouched, even within the same second or below a folded folder;
a newer generation of a previously shown entry fails with a review-again reason. The
token survives restart, rejects other accounts' ledgers and cannot clear a latch while
later work remains held. Replaying a token cannot select newly held work.
The version includes the binding, original cloud version, local place, content size/time,
unsent stamp and the local rewrite counter. Hashing the same local generation preserves
admission. A later edit cannot inherit an approval. A restart does not clear a latch.
Disabling the guard affects new threshold trips and removal quiet time; it does not
approve an existing held batch or erase its retained copies. Allow is always explicit.

## Local removal copies and Restore

For a resident file, unlink records a retained name in the guard ledger, writes the
ordinary place-change record, and renames the backing file there before removing its
local row. The retained directory is `.konedrive-change-guard` in the backing directory.
It has no tree row, is outside the FUSE namespace and is ignored by the startup scan.
Putting it there, rather than assuming the state directory shares a filesystem, keeps
set-aside a rename. The source parent and retained directory are synced. The existing
place-change recovery can put an interrupted unlink back. Startup's temporary-aside
cleanup never prunes retained files. Unique unsent bytes are not evicted to free space.
A set-aside failure refuses the local unlink rather than discarding unique bytes.

A successful remote removal records durable retained-cleanup ownership before the
SQL commit. Startup and every plan retry cleanup on the mutation thread once the row
confirms absence, syncing the retained directory even when an earlier unlink succeeded
but its sync failed. Allow also cleans retained bytes of unbound files whose rows went
on unlink; unapproved unique bytes stay retained. Restore can run
without a network connection, while the account's folder is mounted:

- A removal restores the row from the known cloud version and renames its retained
  file back, when present. Without a retained file it restores a cloud-only row.
  A formerly clean copy returns clean only after its bytes are hashed against the
  persisted content snapshot and that snapshot still matches the current remote
  baseline. Otherwise the retained bytes become an unbound local-only conflict copy
  and the bound row restores cloud-only. Old bytes never become a rewrite of a newer
  remote baseline. Verification runs off the mutation thread and tree lock; the commit
  checks the generation and the verified file's inode, size and nanosecond times again. Never-uploaded files
  whose rows disappeared can be rescued with their missing parents as held local-only
  work, preserving their unique bytes without starting an upload.
- A rewrite becomes a new, unbound local conflict copy, while the original row takes
  the cloud content. The copy stays local-only until the Issues Upload action re-admits it.
- A move uses the existing conditional TakeMoveBack row operation and a no-replace
  backing rename.
- A new upload stays local, flagged `local-only`. Restore of an upload cleanup leaves
  the orphan remotely rather than deleting it. `ListHeld` excludes local-only entries;
  `UploadLocal(account, relative_path)` re-admits an Issues entry to the ordinary
  thresholds, rather than bypassing them. For a local-only directory this includes its
  local-only descendants and required local-only parent directories. Ordinary Allow
  never uploads a Don't Upload entry. Disabling protection or restarting keeps it local.

Restores run on the mount's serialized write thread and check the selected generation
again. Their place-change record is WAL-synced before a backing rename, and the
committed row is WAL-synced before reporting success or forgetting retained-copy
ownership. A failed barrier keeps the hold; after a committed-row barrier failure the
mount stops without undoing the committed names. Staged pinned replacements and
content reader waits also honor holds: a trip wakes a reader wait, and the final local
mutation runs under the guard lock so held local content is never discarded. Conditional tree transactions also recheck it, covering a concurrent listing.
A name collision, open writer or newer generation keeps the change held; Restore never
replaces another file. Parents are restored before removed descendants. Already-sent
operations are separate history and are never offered as restorable.

Restore writes an exact completion receipt into the guard ledger before its SQL
transaction commits. After place-change recovery, startup settles a receipt only if
all resulting rows still match; an uncommitted receipt or newer work stays held.
Startup also retires held uploads of rows its ignored-name refresh left here (they are
never sent), keeping Don't Upload entries.
Restore refuses a bound content switch while readers remain, under the same reader
gate as normal replacement. Conflict names use the bounded ordinary naming helper and
try free alternatives. Cleanup DELETEs use the account-wide removal quiet deadline;
shared settings apply to them too. One-shot upload leftovers are queued durably before
deferral and settled on success, disappearance or abandonment. The CLI refuses explicit
account selection for the shared enable, disable and threshold settings.

## D-Bus and CLI

`org.konedrive.ChangeGuard` is exported at `/org/konedrive/Accounts`. Its XML is the
contract and documents each argument, unit and count. Account arguments are stable ids.
Settings methods and properties have no account argument, so Settings works with no
accounts. `GetError(account)` separately reports a ledger failure.

`ListHeld(account)` returns `(token, rows)`. Rows include kind, relative path, Unix event
time, folder flag, original/new sizes, old/new paths for moves, known descendant count
and a sent flag. The daemon folds removed and new folder subtrees into their topmost
selected folder, summing original/new bytes and counting descendants excluding the
folder itself. Decisions still work on every individual immutable entry. Moves retain
independent rows; a moved folder counts its known remote descendants. Rows sort by
relative path; the app may apply Dolphin's natural filename collation. Sent history uses
the same rows, restricted to currently held kinds in the window preceding the trip.
It records the first admission time; later explicitly allowed work is excluded. It is never
restored or allowed. Relative paths are intentionally exposed for user review.

`AllowHeld(account, token, kinds)` and `RestoreHeld(account, token, kinds)` return a
`GuardResult`: done, pending-offline, failed, kept-both-versions, and per-entry plain-text
failure reasons. Empty kinds selects all four kinds within the token. Counts are
individual changes, including folder descendants, rather than display rows. Failure of
one restore leaves it held and does not prevent unrelated restores. A combined rewrite
and move is restored together and counts both changes, with one kept local copy.

Per-entry reasons are lower-case sentence fragments without a trailing full stop, ready
for the row's second line. The XML documents the same set:

| Reason | Cause |
|---|---|
| a file of the same name is in its place | The destination is occupied on disk or in the tree transaction |
| it is open in another program | A writer is still open, or a reader prevents a content switch |
| it was changed on this computer after it was held | The local generation, write count or verified retained file changed |
| it was changed in OneDrive after it was held | The remote version or binding changed |
| it changed after it was held | The review token is stale, or an older ledger cannot distinguish which side changed |
| the folder it belongs in is missing or has moved | The destination's parent cannot be used |
| its local copy is no longer available | A required row, retained copy or source file is missing |
| there is no free name for a copy of the local file | All 100 conflict-copy names are occupied |
| this account's folder is not mounted | A restore needs a mount and the account has none |
| this computer couldn't save the restored file | A disk, database, durable ledger or rollback write failed |
| this account's folder stopped before it could be restored | The mutation thread stopped before the restore ran |

New ledger entries persist a separate remote identity solely to explain a refusal;
old entries remain readable and keep the same generation safety checks. Conditional
store commits distinguish local changes, remote changes and occupied or missing parents
inside the transaction. Raw database errors and filesystem debug paths are logged rather
than returned as row text. Allow uses the open-file and stale-review reasons too.

Allow reports admission to the existing sender: done means released for sending, and
pending-offline means durably released while the most recent listing reports OneDrive
unreachable. It does not claim a Graph acknowledgement. Later network failures, remote
conflicts and completed cloud writes remain the ordinary upload queue/Conflicts/history
interfaces' responsibility. The kept-both count is the number of successful local rewrite
restores that made a copy. Restore needs no network: removed cloud-only entries return
as placeholders, resident entries from retained copies. With no mount each affected
entry reports failure; Don't Upload still works because it only changes the ledger.

`RestoreProgress(account, token, kinds, done, total)` starts before processing and ends
at total; done counts processed entries including failures. Subscribe before calling.
Clients should use the final method reply for result counts, even after a lost signal.
`HeldChanged` wakes clients to re-list after decisions and edits.

`konedrivectl --account <account> changes held [--sent]` prints the token and detailed
rows. Pass it explicitly to `changes allow --token TOKEN [kinds ...]` or
`changes restore --token TOKEN [kinds ...]`; the CLI never silently takes a fresh token
at decision time. `changes upload RELATIVE_PATH` is the Issues Upload action.
`changes status`, `thresholds [NAME=VALUE ...]`, `enable` and `disable` read/write shared
settings. Threshold keys cover all limits: `removals`, `removal-bytes`, `daily-removals`
(the 24-hour count), `rewrites`, `rewrite-bytes`, `moves`, `uploads`, `upload-bytes`,
`window-seconds`, `daily-window-seconds` (86400 by default). Omitted keys keep exact
values; bytes/seconds are integers, windows must be positive and unknown keys refuse
the whole update. Settings commands and D-Bus methods also work before an account exists.

## In the window

Settings → Sync has **Hold large changes** for every account, its limits in the switch's
words, and **Change Limits…** (Large Change Limits: every count, byte and window limit,
saved together; a box left as it was keeps the daemon's exact value). Turning it off asks
first and never releases what is held; off, one warning line says what that means.

While an account holds something, the sidebar has **On Hold** with the count of held
changes, Status has a card of the held kinds with Review…, and the account's state is
`held` (`state-error` in the tray, "Changes on hold" at the head of Status) unless
something worse goes on. On Hold lists one card per kind with its two buttons, the undo
first and both naming the count, so a search can't hide what they act on (Restore 1,999 /
Delete 1,999 in OneDrive, Restore 58 / Upload 58 Changes, Move 240 Back / Move 240 in
OneDrive, Don't Upload 500 / Upload 500), Restore All and Allow All for several kinds, the
open group's rows (built by the list, only for the open group), and what was sent before
the hold. While deletions are among them, Allow All first asks "Send All N Changes to
OneDrive?", naming what each kind will do. A restore that left items on hold shows each
one's reason (the daemon's per-change failure) as its second line. Every decision uses the
token of the listing the page shows; what it did shows at the top of the page. The page is
`app/qml/OnHoldPage.qml` over `HeldChanges` (`app/heldchanges.*`).

One notification per account (`changesHeld`) says what came on hold, naming the folder when
one folder holds it all, and what was already sent before the hold, with **Review…** and
Restore only (Move Back, Don't Upload, Restore All): allowing is on the page. Its button acts
on the token of what it says, and what it did takes its place; a restore result offers
**Open Recycle Bin** when items were sent before the hold. What is held when the app starts
is no news. Issues lists a file Don't Upload kept here with **Upload** (`UploadLocal`).

## Checks and follow-up

Unit tests exercise window boundaries, original-byte charging, repeated rewrites,
concurrent admission, restart persistence, descendant admission, collisions and offline
restoration. The D-Bus integration test compares live signatures with the checked-in
XML. `tests/fuse-probe/run.sh mass_changes` runs the removal, git-clean, sed, rename and
upload-flood drills over the fake OneDrive. `FUSE_PROBE_PROFILE=debug` runs that group
without a release build. Other VM groups explicitly disable this guard so their
immediate-send assertions continue to exercise their original behavior.

Incoming mass deletes made on another device remain a separate follow-up. This guard
also cannot restore cloud operations sent before a trip; OneDrive's recycle bin and
version history remain the recovery tools for those.
