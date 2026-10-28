# The download guard

A read of a file that is not on disk costs network traffic and may start a whole download.
An indexer, thumbnail worker, backup or recursive search must not quietly copy a large drive
onto a small disk. `download_guard/` owns admission for those reads, shared by every account
through the daemon's one `ConfigStore`. It writes no tree row, conflict, sync failure or file.
Explicit `Files.Hydrate`, `Pin` and `FreeUp` bypass admission; hydrate and pin also protect
the requested copies from app-download reclamation.

## Callers and background readers

At a cloud-only open and before each remote fetch, the mount uses the request's PID. Procfs
provides the executable, cgroup and, for KIO, the thumbnail plugin in the command line. The
innermost `app-<desktop-id>@<instance>.service` or `app-<desktop-id>-<instance>.scope` becomes
`app:<desktop-id>` (systemd hex escapes decoded). Without an app unit, the identity is
`exe:<absolute executable path>`. Separate launches of the same program share the budget;
children of a desktop app share its app unit. A terminal's children may therefore share one
budget. No parent command line, environment, file names or arguments are published.

A cache of at most 1024 PIDs keeps the normalized facts. Every use rechecks the process start
tick, executable, cgroup and KIO role; PID reuse, exec and changes of scope do not keep an old
identity. Procfs failures refuse remote reads as `EACCES`, recorded as `unidentified`; the UI
cannot allow an unidentified caller. The bridge's own thread bypasses inspection. A read
answered from a backing file or bytes already written by a running download asks no guard.

The default reader list is data (`download_guard/identity.rs`):

- `dolphin`: its automatic MIME sniffing opens extensionless and ambiguous files
  ([kio-behavior.md](../kio-behavior.md) C). Launching an application does not require Dolphin
  to read content, so the application reads under its own identity.
- `kio-thumbnail`: `kioworker`/`kioslave5`/`kioslave` with the `thumbnail` protocol or a
  `thumbnail.so` plugin argument. Cached previews need no content; missing or stale previews
  open it ([kio-behavior.md](../kio-behavior.md) B). `kioworker` with `file.so` remains eligible:
  it serves a deliberate copy out of the folder. Both roles may share a launcher's app unit;
  a thumbnail refusal does not itself block its file worker. File workers remain
  subject to bulk limits because automated KIO clients can use them too; deliberate bulk copies
  can use an app allowance.
- `baloo_file`, `baloo_file_extractor`: KDE's file indexer and content extractor, in addition
  to the existing folder exclusion ([desktop.md](desktop.md) §9).
- `tracker-extract`, `tracker-extract-3`, `tracker-miner-fs`, `tracker-miner-fs-3`,
  `localsearch-extractor`, `localsearch-3`: the corresponding GNOME indexer/extractor services.
- `tumblerd`, `ffmpegthumbnailer`: thumbnail generation outside KIO.

`[download_guard_limits] background_readers` replaces this list. Reader rules take precedence
also over session and persistent allowances. Removing a reader deliberately or switching the
guard off permits it. The existing account `thumbnails` setting still defaults to on: it
controls the small Graph thumbnails the daemon fills, independently of local content readers.

## Admission and accounting

Defaults: **30 distinct files or 2 GiB in 600 seconds**, per app across all mounted accounts.
The first 30 files and bytes up to the byte limit are admitted; a request that would exceed
either is refused before content is fetched. Opening alone consumes no budget. File identity
includes the account store and Graph item, never a display name or path in a notification.
Each fetched range refreshes the file's last admission time. Byte buckets are at most a second
wide and expire by their last admission, conservatively retaining at most one extra second.
The clock is monotonic. A bulk block remains until `AllowApp` or `ResetApp`, even after its
window expires; blocked counters remain available for a later notification.

Admission charges the requested range length, and reserves a whole download's known size
before it starts. This includes range/whole overlap and unsuccessful requests; displayed bytes
are a conservative admission budget, not a network meter. Retries of a whole download charge
bytes beyond that reservation as they arrive. Already-started transfers finish if their retries
exceed the limit, but subsequent requests are refused. An allowance bypasses counting. A file
larger than the byte limit can need approval at its first whole-download attempt.

Cloud-only read handles use `FOPEN_DIRECT_IO`. This prevents speculative readahead and avoids
the page cache changing a denied read's errno to `EIO` (the behavior already measured for
throttle refusals). `FUSE_DIRECT_IO_ALLOW_MMAP` is requested when the kernel supports it. Older
kernels may require explicit hydrate before an app can mmap a cloud-only file. Backing-file
handles keep their existing cache behavior. Known blocked/background callers fail at open;
handles already open fail at their next remote request. Local data remains readable. This is
an accidental-download guard, not a security boundary against a program changing its identity
or invoking the explicit D-Bus methods.

## Space

Before a new read-triggered whole download, the downloader measures backing-filesystem free
space. The account's free-space reserve (10 GiB by default) must remain after its whole size,
other running downloads on that filesystem, block rounding and 64 KiB of headroom per transfer.
Reservations are shared across accounts and released at completion, failure or cancellation;
a join reserves nothing twice. Pins and explicit hydrate register their allocations too, so
new reads cannot spend that space again. The plan's space reader subtracts allocations outside
its own steps, so future pins also leave room for reads and explicit hydrate across accounts.
Unknown free space authorizes no whole allocation.

When there is no room, ordinary reads continue as ranges and nothing lands. If a previous
version disagreement requires a verified whole download before ranges are safe, the read fails
`ENOSPC`. During an admitted read download, each network-buffer read rechecks available space
before handing bytes to the disk writer: external allocations or a changed reserve can stop
it with `ENOSPC`. Pin streams also check the reserve while writing, closing the race between
planning a pin and admitting a read. Other processes can still race disk allocation; the guard
cannot reserve space on their behalf. Explicit hydrate keeps its existing behavior, and pin
downloads also keep the plan's existing space check. The download guard switch does not disable disk protection.

## Free up what an app downloaded

`FreeUpAppDownloads(identity)` reclaims the whole copies whose downloads were started by
reads attributed to that app in this daemon session, across all accounts. A read joining
another app's transfer does not take ownership. Range-only reads, unsuccessful downloads,
and an already-local copy land nothing attributable. Attribution follows the store, item
id and downloaded content version through renames; it never follows a display path to a
replacement file. It also works for identifiable reads while admission is switched off.

The daemon keeps at most **10,000 attribution entries globally**, including whole downloads
in flight. At capacity, new downloads remain untracked and cannot be reclaimed by this
action; existing entries are retained. Failed/cancelled transfers, explicitly kept copies,
and reclaimed copies release entries. The ledger lives only in memory; restart loses it.
`ResetApp` resets admission policy/counters, but does not discard this session's attribution.

Each candidate uses Free Up Space's backing-copy removal and conditional row commit. Own
and inherited pins, local changes/pending uploads, open writers, running transfers or sync
steps, and copies already freed are skipped without error. This action also skips open
read handles and a content version different from the one attributed. It never removes a
pin, deletes a cloud file, or changes sync rows beyond ordinary free-up. The reader-open
gate and write-operation thread serialize the final checks with new opens and writers.

An explicit hydrate or pin protects the requested file or directory's copies, even if
already local, and invalidates any in-flight attribution too. Unpinning later does not
restore that attribution. Hydrate invalidates when the request is accepted, conservatively
keeping copies even if its later network operation fails. Repeating reclamation can free
a temporarily busy copy once its reader/writer closes; successful copies count only once.
Stopped/unmounted accounts contribute zero; reclamation uses each active mount.
The result is actual files freed and allocated backing bytes returned, not logical file
sizes or the guard's admission budget. Unknown identities return `(0, 0)`. AppsChanged is
emitted when the action frees copies; it neither allows the app nor resets its block.

## Bus and lifetime

`org.konedrive.DownloadGuard` is on `/org/konedrive/Accounts`, under `org.konedrive.Daemon`.
The checked-in contract is [org.konedrive.DownloadGuard.xml](../../dbus/org.konedrive.DownloadGuard.xml).

| Member | Signature and meaning |
|---|---|
| `Enabled`, `MaxFiles`, `MaxBytes`, `WindowSeconds` | read-only properties: `b`, `u`, `t`, `u`; ordinary `PropertiesChanged` |
| `SetEnabled` | `(b enabled)`; writes top-level `download_guard` |
| `SetThresholds` | `(u max_files, t max_bytes, u window_seconds)`; all positive, written together |
| `ListApps` | `() → a(sssbstt)`; identity, display name, policy, blocked, reason, files, bytes |
| `AllowApp` | `(s identity, b always)`; false: until daemon restart, preserving any always rule; true: persistent |
| `BlockApp` | `(s identity)`; persistent, replacing an allowance |
| `FreeUpAppDownloads` | `(s identity) → (t freed_files, t freed_bytes)`; reclaim safe session-attributed copies |
| `ResetApp` | `(s identity)`; removes persistent/session rules, counters and remembered block |
| `DownloadsBlocked` | signal `(s identity, s display_name, t files, t bytes, s reason)` |
| `AppsChanged` | signal `()`; read `ListApps` again |

Policies: `automatic`, `allowed-session`, `allowed-always`, `blocked-always`. Reasons: empty,
`bulk`, `background`, `user`, `unidentified`. A background refusal still wins when policy reads
`allowed-*`. `blocked` records a refusal or a persistent block, even while the master switch
is off. Properties say whether enforcement is enabled. Display names are untrusted text: a
normalized desktop id or executable basename, not a resolved desktop entry or localized label.

Subscribe before calling `ListApps`, reconcile by identity, and deduplicate notifications:
the list is authoritative when the UI was absent or a signal receiver lagged. One block
transition emits one `DownloadsBlocked`. Settings/method calls emit their property or app-list
changes. No daemon notification UI is built here. The app must explain background refusals,
unknown identities and approvals, refresh after daemon restarts, and retry a failed user open
once approved. The daemon does not automatically reopen applications or retry denied reads.

A session allowance overrides an always block for this daemon process, after background-reader
classification, without writing or requiring a writable config. The always rule returns at
restart. Always rules and thresholds use `ConfigStore`'s atomic config writes, then take effect. A failed
write changes no policy. Rule methods reject invalid identities as `InvalidArgs`, as do zero
thresholds; config I/O
failures are `Failed`. Session allowances, seen apps, counters and pending notifications last
for this daemon process; only always rules survive its restart. Hand edits are loaded at daemon
start or its next successful config update. The guard never watches or rewrites a live config
from a file read request.

## Verification

The pure identity, window, policy, persistence and space tests need no mount. Kernel-request
fixtures check refusals leave rows untouched, local reads bypass inspection, explicit hydrate
and pin bypass reader policy, and low-space reads remain ranges. A private session-bus test
checks method behavior and live XML signatures. Attribution tests cover the global bound,
failed/cancelled transfers, explicit keep invalidation, identity/account separation and stale
snapshots. Mount fixtures check reclamation skip rules, exact-once counting and expected content. CLI tests parse every guard command.
`tests/fuse-probe/download_guard.sh` checks the cutoff, background/KIO roles, normal opens,
explicit actions, low-space ranges, a new byte block on already-open handles, and mmap on
a real FUSE mount over fake Graph, only in the VM. Its reclamation scenario checks exact
file/allocated-byte totals, pins, explicit hydrate, open readers/writers, retries after close,
and unchanged cloud copies with no Graph deletion.
