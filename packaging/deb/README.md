# Local Kubuntu packages

`build.sh` packages existing release artifacts into `konedrive` (daemon, CLI,
user unit, D-Bus activation and documentation) and `konedrive-kde` (window,
launcher, notification configuration and Dolphin plugins). It follows the RPM
file split, with Debian multiarch Qt plugin paths. The launcher uses Breeze's
`folder-cloud` icon, as upstream does; the only project icons are the Dolphin
emblems, which the Dolphin build installs into `hicolor`.

From the repository root:

```sh
packaging/deb/build.sh
```

The default artifact root is `~/.cache/agent-scratch/onedrinux`: `target/release`,
`app` and `dolphin`. Override it with `KONEDRIVE_SCRATCH`, or set
`CARGO_TARGET_DIR`, `KONEDRIVE_APP_BUILD` and `KONEDRIVE_DOLPHIN_BUILD` separately.
The optional first argument is the output directory (default: `$KONEDRIVE_SCRATCH/deb`).
Every run retains a fresh `stage.*` directory containing the two DESTDIR trees.
The script never builds, installs, or deletes existing artifacts. The caller
must supply builds from the current HEAD and target architecture, with CMake
configured for `/usr` and the distribution's Qt plugin directory. Existing
test-only source edits are not included in the packages.

Versions are `<Cargo version>~git<12-character HEAD>`. The binaries retain the
version embedded by their original build (a plain upstream build shows
`0.2.3-dev`). The maintainer is explicitly local, with a reserved `.invalid`
contact address; override it with `KONEDRIVE_DEB_MAINTAINER='Name <email>'` if needed.
No `dev-tools` daemon is accepted: the builder checks for
`org.konedrive.TokenExport` just like the upstream RPM spec. ELF dependencies
come from `dpkg-shlibdeps`; explicit dependencies cover FUSE, Dolphin, the
Breeze theme and the QML imports/style. Build tools needed here are Bash,
Python 3.11+, CMake, Git, binutils, dpkg-dev and desktop-file-utils, alongside
the dependencies required to produce the upstream artifacts.

Install the two exact generated filenames with `sudo apt-get install ./…deb ./…deb`.
The first install only reloads user managers: the daemon is not enabled or
started. A call to `org.konedrive.Daemon` activates `konedrived.service` through
the session bus, with `/usr/bin/konedrived` as a non-systemd fallback. Upgrades
restart an active daemon; removal stops it before removing its executable.
The app's own first-run/start-at-login behavior is unchanged. Close and reopen
Dolphin after installing to load its new plugins.

Open **KOneDrive** from Plasma's launcher, sign in yourself, and choose an empty
folder. Accounts start read-only; leave **Two-way sync** off. Only personal
Microsoft accounts are currently supported upstream. No sign-in or folder
creation is performed by this packaging.

Before uninstalling a configured installation, disconnect each account in the
app, as upstream recommends. Then:

```sh
sudo apt-get remove konedrive-kde konedrive
```

Account settings, local file data and wallet entries are never deleted by
these packages. These are local, unsigned packages, not an upstream Debian
release or packages prepared for public redistribution.
