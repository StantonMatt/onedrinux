# Using KOneDrive

How to install KOneDrive, connect your OneDrive and use it — in the window, the tray and Dolphin.
Everything here can also be done from a terminal: [command-line.md](command-line.md). What
KOneDrive is, in short: [the README](../README.md). What does not work and cannot simply be
fixed: [limitations/](limitations/README.md).

## Install

KOneDrive is two packages, for Fedora 44 and 45 (x86_64):

- `konedrive` — the service and `konedrivectl`;
- `konedrive-kde` — the window, with its tray icon, and the Dolphin plugins, for KDE Plasma.

It needs KDE Plasma 6 with KDE Frameworks 6.21 or later, and FUSE 3 (`fuse3`, which the package
asks for).

Download both for your Fedora from the Releases page of the repository on GitHub
(`kartas39/konedrive`): the files with `.fc45` in the name for Fedora 45, `.fc44` for Fedora 44 —
not the other Fedora's, and not the `.src.rpm`. Then, in the directory they are in:

```
sudo dnf install ./konedrive-*.rpm
```

There is no package repository yet, and the packages are not signed; `SHA256SUMS` on the same page
lists their checksums (`sha256sum -c --ignore-missing SHA256SUMS`).

## First steps

1. Open **KOneDrive** from the launcher.
2. Choose **Sign In…** under **Add your OneDrive**. Your browser opens Microsoft's sign-in, with
   its account picker; there is nothing to register or type in first.
3. When the sign-in is done, KOneDrive asks for a folder: choose an **empty** one, for example a
   new `~/OneDrive`. A folder with anything in it, or another account's, is refused, and the
   dialog asks again. If you close it, **Choose Folder…** on Status opens it again.

New accounts start with two-way sync; existing accounts keep their mode (see
"Uploading changes").

Your OneDrive appears in that folder: every file with its real name and size, taking no space until
it is read.

A cancelled or failed sign-in leaves nothing behind. Only personal Microsoft accounts work; work and
school accounts are not supported yet.

## The sync folder

The sync folder is served by the KOneDrive service while it runs. What that means:

- **Every file is there from the start**, with its name, size and time, and takes no space on
  this computer.
- **Reading a file fetches it.** A program that reads a part of a file gets that part from
  OneDrive. Once more of it is read, the file is downloaded whole and stays on this computer; the
  next read asks OneDrive nothing.
- **A downloaded file stays** until you free up its space (see "Freeing up space").
- **When OneDrive asks to wait.** OneDrive sometimes tells a computer that asked much of it to
  wait, for some seconds or a few minutes. Until then a file that is not downloaded cannot be
  opened: the program is told at once ("Resource temporarily unavailable") and is not kept
  waiting, a notification says how long the wait is and names the file, and the window, the tray
  and `konedrivectl sync status` say "OneDrive asked to wait" with the time left. Downloaded
  files open as always. Open the file again after the wait.
- **While the service is not running** — it was stopped, or it is being upgraded — the folder
  shows only what is on this computer: the downloaded files and files of your
  own that were not sent yet. It cannot be changed then. Everything is back when the service
  runs again.
- **Only you reach it.** Another user, and a program run as root, cannot open the folder.

To download a file without opening it: `konedrivectl sync hydrate <path>`. To keep files
downloaded for good, pin them ("Always keep on this device", under "Dolphin").

## The window

The sidebar starts with the **account switcher**: the OneDrive shown, and a menu with every one and
**Add Another OneDrive…**. Below it are the pages of that OneDrive.

- **Status** — what the folder is doing now: up to date, syncing, paused, or what needs your
  attention, with a line that leads to Issues. At the top: **Sync Now**, **Pause** or **Resume**,
  and **Open Folder**. Below:
  - the space, in one bar as long as your OneDrive's storage: bright for what is on this computer,
    pale for the rest of what your OneDrive holds; how many items are always kept here, and their
    size; **Free Up Space…** and **Unpin All…**;
  - **Downloading** and **Uploading**: the speed, the files moving, what is left with its size and
    time, and a chart of the last two minutes; then the files moving now;
  - **History**, folded until you open it: what happened lately, with a search by name or folder.
- **Issues** — what needs you: the changes that **Can't Upload** and why (see "Uploading
  changes"); files changed on both sides, of which both versions are kept so that nothing is lost
  (**Kept Both Versions**, with **Show Both** and **Dismiss**); and what **Can't Be Shown in the
  Folder**, and why: the Personal Vault, a OneNote notebook, a shared folder, a name Linux cannot
  hold. Such an item keeps its name: nothing here can be given it, also in another case, until
  the item is renamed in OneDrive.
- **OneDrive Settings** — its name (**Rename…**), the Microsoft account, **Two-way sync**, the
  folder (**Use Another Folder…**), previews, the free space to keep, what is never uploaded,
  **Sign Out** and **Disconnect This OneDrive…**.

**Settings** is for the whole app:

- **Start at login** — on after the first run.
- **Show download and upload progress** — a long transfer shows in Plasma's notifications.
- **Show in Places** — each account's folder is in Dolphin's Places panel and in file dialogs.
- **Show a tray icon for each account**.
- **Pause on metered connections** and **On battery** — when every account pauses by itself.
- **Quit KOneDrive**.

Closing the window does not stop anything: KOneDrive stays in the tray, and the service that syncs
runs without the window at all. The foot of the sidebar names the version, for a bug report; when
a newer release is out, **Update available** below it opens the release's page, and a notification
says so once.

## The tray

The tray icon shows an account's state: synced, syncing, paused, signed out, or needing your
attention. With several accounts each has its own icon, named in its tooltip; with that setting
off, one icon shows the most serious state of them all.

A click shows the window on that account, or hides it. The menu opens the folder or the window,
refreshes, pauses for 2, 8 or 24 hours or until resumed, resumes, and quits.

## Dolphin

In the OneDrive folder every file and folder has an emblem:

- a blue cloud — only in OneDrive, takes no space;
- blue circling arrows — downloading, about to be downloaded because it is always kept, or waiting
  to be uploaded;
- a green check in a ring — downloaded;
- a solid green check — always kept on this device;
- a red square with an X — something needs attention: a change that can't be uploaded (Issues
  says why), or a file that couldn't be downloaded for now.

A folder's emblem sums up what is inside it: the red square if anything in it needs attention,
the arrows if anything in it is moving, the solid check if it is always kept, the cloud if any
file in it is only in OneDrive, and the check in a ring once everything in it is downloaded.

The Details view shows small emblems; zoom it in to 32 px icons or more (View → Zoom In) for
bigger ones.

The right-click menu has, under **OneDrive**:

- **Always Keep on This Device** — downloads it and keeps it, for a file or a whole folder; what
  comes into a pinned folder later is downloaded too. Status says how many files are still to
  download, how much the folder takes on this computer, and how much of it is always kept;
- **Free Up Space** — makes it online-only again, and unpins it. It is greyed out for a file that
  is only in OneDrive already, and for one with changes not uploaded yet;
- **View Online** — opens its page in OneDrive's web interface, where it can be shared and its
  versions seen. With two personal accounts this opens under the account your browser is signed in
  to (limitations log, F295).

Previews of images, videos, PDF and PostScript files, office documents (OpenDocument and Microsoft
Office) and EPUB books come from OneDrive's own thumbnails, so showing them downloads nothing. A
preview of another kind of file, plain text for one, or one at the largest zoom, does download the
file; to avoid that, turn previews off in that folder (View → Show Previews).

Desktop search (KRunner, Dolphin's search) finds nothing in the folder: indexing it would
download every file.

## Freeing up space

- One file or folder: **Free Up Space** in Dolphin's menu.
- Everything that is not in use: **Free Up Space…** on Status.

The file stays in the folder with its name and size; only its copy on this computer goes. A file
you changed that has not been uploaded yet, and a file a program has open for writing, is never
freed. Neither is a file always kept on this device: **Unpin All…** on Status makes nothing always
kept any more, and then **Free Up Space…** frees those files too.

## Pausing

**Pause** at the top of Status, or **Pause Syncing** in the tray menu: for 2, 8 or 24 hours, or
until you resume. Nothing is uploaded and OneDrive is not asked for changes meanwhile; opening a
file still downloads it. The pause stays across a restart.

An account can also pause by itself on a metered connection or on battery (Settings). **Sync
Anyway** on Status or in the tray lifts that, until the connection or the power changes
again.

## Several accounts

**Add Another OneDrive…** in the account switcher's menu opens the browser to sign another
Microsoft account in, then asks for its own empty folder. Two accounts' folders cannot be inside one another, and one Microsoft account can
be connected once. A file moved from one account's folder to another's is copied and removed, as
between two disks.

**Disconnect This OneDrive…** on OneDrive Settings signs the account out and forgets it on this
computer. Its files stay where they are — those that were downloaded, and your own; a file that was
never downloaded is not on this computer — and nothing in OneDrive is deleted. If changes were not
sent to OneDrive yet, the window says how many before it goes on: they stay only on this computer.

## Uploading changes

**New accounts are two-way by default.** Their first browser sign-in asks for permission to
change files, so only one sign-in is needed. To add an account read-only, use
`konedrivectl account add --read-only`. Existing accounts keep their mode. To switch an existing
read-only account, use **Two-way sync** on OneDrive Settings or
`konedrivectl account mode read-write`; that switch asks for write consent in the browser.

For a staged trial, limit writes before changing any files. On an existing read-only account,
set this before `account mode read-write`, so pending changes outside the trial are held too:

```sh
konedrivectl --account ACCOUNT account write-scope set Trial
konedrivectl --account ACCOUNT account write-scope show
```

Only `Trial` and everything below it can then be changed. Programs receive “Read-only file
system” outside it; moving a file across the scope boundary is refused in either direction.
Reading, downloading, keeping files on this device and freeing space still work everywhere.
The sender also refuses pending changes outside the scope. Set this while no changes are being
sent: operations already admitted may finish. To allow writes throughout the drive again:

```sh
konedrivectl --account ACCOUNT account write-scope clear
```

- **What goes up**: new files and folders, edits, renames, moves and deletes. A changed file goes
  up a couple of seconds after the last change, once no program has it open for writing. Status
  counts what is waiting; Dolphin shows an emblem on each.
- **Changed on both sides**: both are kept. OneDrive's version keeps the name, and yours stays
  beside it as `Report-<computer name>.docx` and goes up under that name. Issues lists it under
  **Kept Both Versions**.
- **What stays on this computer**, listed on Issues under **Can't Upload**: a file whose name
  OneDrive refuses (rename it to upload it), a file larger than OneDrive takes, and, while OneDrive
  is full, every file with new content.
- **What the folder does not take**: symbolic and hard links, device files, pipes and sockets.
  OneDrive holds none of them.
- **Moving a file out of the folder** removes it in OneDrive. The file is copied to where it
  goes first, so one that was not downloaded is downloaded on the way.

### The ignore list

Editors' temporary, swap, lock and backup files stay on this computer: `*.swp`, `*.swo`,
`*.swx`, `~$*`, `.~lock.*#`, `*.part`, `*.crdownload`, `*.tmp`, `*~`, `.#*`, `#*#`,
`.goutputstream-*`, `*.kate-swp`, `.fuse_hidden*` and `.nfs*`. Build and dependency folders
`node_modules`, `.venv`, `venv`, `__pycache__` and `.gradle` stay here too, with everything
inside them. They remain visible in the sync folder; their bytes are kept here.

The list matches names at any depth, case-sensitively. Edit it in OneDrive Settings
("Never uploaded") or with `konedrivectl sync ignore`. Your own saved list replaces the
defaults; accounts using the defaults receive new defaults after an upgrade.

Anything already in OneDrive keeps syncing whatever its name, including a downloaded or
pinned build folder. The ignore list never removes or hides it. A new file inside such a
cloud folder also syncs unless its own name is ignored, so a `node_modules` made inside a
`node_modules` that is in OneDrive (npm nests them) stays on this computer. New local-only
matching folders are skipped, including ones that were waiting to upload before the upgrade.
One exception: something in OneDrive that you move into a folder kept on this computer waits
there, and syncs again once you move it out. If OneDrive gets a new folder with the very name
of one kept here, your folder keeps its name and stays here, and OneDrive's appears once you
rename or remove yours.

### Removing files

A file or folder removed here is removed in OneDrive, and goes to **OneDrive's recycle bin**.

- **There is no trash on this computer for the sync folder**, and no question before a large
  removal: removing a folder with a thousand files removes them in OneDrive.
- **To get something back**, open the recycle bin in OneDrive on the web and restore it there; it
  appears in the folder again.
- **"Move to Trash" in Dolphin** asks whether to delete instead, and then deletes. Nothing is
  downloaded and nothing lands in the trash of your home directory. Shift+Delete deletes without
  the detour.
- **A folder that will not go.** A folder that holds in OneDrive something this computer cannot
  show (Issues lists it under **Can't Be Shown in the Folder**) cannot be removed here: removing
  it fails with "directory not empty", though it looks empty. The files shown in it are removed as
  you asked. A notification and Status's History name the folder; remove it in OneDrive on the
  web.

### Turning uploading off

Turning **Two-way sync** off does not drop what is still to be sent.

- **Nothing waiting**: the folder is read-only again at once.
- **Changes still to send**: the account sends them first and then switches by itself. OneDrive
  Settings says **Switching to read-only: 3 changes still to send** meanwhile.
- **Switch Now** there switches at once. What is not sent stays on this computer as it is, and is
  sent when two-way sync is turned on again.
- **Keep Two-Way Sync** leaves it on.

The wish survives a restart of the service.

## Using another folder

**Use Another Folder…** on OneDrive Settings stops keeping the folder in step, and Status then has
**Choose Folder…** for another empty one. The old folder becomes a plain folder again: the files that were downloaded
and your own stay in it, and can be changed; files that were never downloaded are not in it.
Nothing in OneDrive is deleted.

If changes were not sent to OneDrive yet, the window says how many before it goes on. Leaving the
folder then leaves them as plain local files, and they are not sent.

If the OneDrive folder itself is moved, renamed or deleted while the service is not running,
KOneDrive stops syncing that account, and Status says why, with **Try Again**. Move the folder back
and try again, or choose **Use Another Folder…** right there: the old one is forgotten once you
picked the new one.

## While the service was not running

What you do in the folder while the service is stopped is not followed as you did it. The folder
is locked then, to keep that from happening by mistake. If it is changed anyway:

- a new file, and a file whose content you changed, is uploaded once the service runs and
  uploading is on;
- a file of OneDrive that you moved is a new file at its new place, and the file is shown again
  at its old place;
- a file of OneDrive that you removed is shown again, not downloaded. Nothing is removed in
  OneDrive.

To move or remove something in OneDrive, do it while the service runs.

## Upgrading and removing

- **Upgrading** is the same `dnf install` with the newer packages. The service and the window
  restart by themselves. While the service restarts, the folder shows only the files that are on
  this computer, and a program reading a file in it gets an error: close programs that have files
  of the folder open first.
- **Removing**: first **Disconnect This OneDrive…** for every account, then
  `sudo dnf remove konedrive konedrive-kde`. A folder still registered when the package goes
  stays locked, and shows only the files that are on this computer.

## Downloads requested by programs

The download guard is on by default. It prevents thumbnailers and indexers from fetching
cloud-only content, and stops an app before it reads more than 30 distinct files or 2 GiB in
10 minutes. This also limits recursive searches and backups. Files already on this computer
remain readable. Opening a file in its app still fetches it within those limits; a large file
or bulk copy can need an allowance. KIO file copies work, while Dolphin's automatic MIME
sniffing and thumbnail workers cannot fetch content. Some ambiguous file types can therefore
show a generic icon until opened or explicitly downloaded.

If an open says "Permission denied", `konedrivectl guard apps` shows the caller and reason.
Use `konedrivectl guard allow '<identity>'` for this daemon session, or add `--always` to
remember it. `guard block '<identity>'` always blocks it; `guard reset '<identity>'` restores
automatic rules and clears its counters. Background-reader rules still apply to allowed apps.
The [command-line guide](command-line.md#download-guard) lists settings and thresholds.

"Always keep on this device", explicit hydrate and "Free up space" bypass the app guard.
The existing thumbnails setting still controls Graph's small cached previews. If a whole
read-triggered download would use the disk's reserve, reads fetch only their requested ranges
and do not keep a whole copy. For an application that cannot mmap a cloud-only file on an
older kernel, hydrate the file explicitly first.

## When an app's downloads are blocked

When an app downloads more than the limits allow, a notification says so ("Too many downloads by
Konsole"), and the app cannot download anything more from OneDrive until you choose:

- **Allow for This Session**: it downloads freely until the KOneDrive service restarts, normally
  until you log out.
- **Always Allow**: it is never counted again.
- **Always Block**: it never downloads from OneDrive. Files already on this computer stay readable.
- **Free Up What It Downloaded**: what its reads downloaded since the service started becomes
  online-only again, except files that are open, changed or kept on this device. It stays blocked.

After allowing it, open the file again in the app: a refused read is not retried by itself.

The notification goes after a while; the app stays on the Status page under **Blocked apps** until
you choose, and in **Settings → Sync** under **Blocked and Allowed Apps**, with the same buttons
(Always Block and Free Up What It Downloaded are in the ⋮ menu). Apps you allowed or always block
are listed there too: **Remove** takes the rule away, and the app is counted from zero again. A
program KOneDrive cannot identify is refused with a "Download blocked" notification, and can't
be allowed. Indexers and thumbnailers are refused without a notification: with the guard on, a file
only in OneDrive gets the small preview OneDrive makes, if "Previews without downloading" is on,
or none, and is never downloaded for a preview.

**Block bulk downloads**, in the same place, turns the guard off and on; **Change Limits…** sets
how many files, how many GiB and within how many minutes. With it off, any app can download your
whole OneDrive; the apps you allowed or blocked are kept for when it is on again.

## Troubleshooting

- The service's log: `journalctl --user -u konedrived -f`; more detail with
  `systemctl --user edit konedrived` →
  `Environment=RUST_LOG=konedrived=debug,konedrive_graph=debug,konedrive_tree=debug`.
- **Every file in the folder fails with "Transport endpoint is not connected".** The service
  ended without taking its folder off. Start it again (`systemctl --user start konedrived`): it
  clears the folder and serves it again.
- **The folder stopped by itself.** After a fault the service takes the folder down, shows the
  cause on Status and starts it again by itself: after 1 minute, then 5, then 30, then every hour.
  **Try Again** there tries at once.
- Files: `~/.config/konedrive/config.toml` (the client ID, and each account with its name and
  folder; `config.toml.v1` is the single-account file it was migrated from, if any). Each
  account's state is in `~/.local/state/konedrive/accounts/<id>/`: `account.json` (cached name
  and quota) and `fuse.sqlite` (the items of the drive, the pins, the activity log and the
  conflicts). Keep that database: it also holds unsent moves, removals and pin exceptions.
  Normal upgrades preserve its rows through a migration and snapshot. An unsupported older or
  damaged store is quarantined; readable pins return and pending namespace changes are exported
  for manual recovery, with their count and path in Status's error. The newest three quarantines
  are kept. A downgrade that finds a newer schema stops with instructions: install the newer
  build, or stop the service and use `konedrivectl store reset <store-path>`. Reset preserves the
  old store; it never replays pending removals or moves. Disconnecting also preserves the store;
  removing an account archives its state directory beside the original with `.removed-<timestamp>`.
  Each account's refresh token is in
  KWallet, under `KOneDrive: <email>`, and never leaves it (see SECURITY.md).
