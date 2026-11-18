# Releasing

## Manual release workflow

GitHub is the primary host; GitLab is a read-only mirror with CI disabled. The default branch
is `onedrinux`, and development happens on `dev`. Ordinary pushes publish nothing and create
no tags. The old automatic rules targeted a nonexistent `main`; releases remain manual.

Merge `dev` into `onedrinux` with a pull request and a merge commit, so the development commits
remain in the default branch's history. Then run **Actions → Release → Run workflow**, selecting
`onedrinux`. Leave `dry_run=true` first. Once the RPM build and install checks pass, run it again
with `dry_run=false` to publish the version in `Cargo.toml`. `v0.2.3` is ready for an explicit
release when the maintainer chooses; this migration does not publish it.

The CLI equivalent is `gh workflow run release.yml --ref onedrinux`; adding `-f dry_run=false`
publishes. A run on another branch is always dry unless an existing `release_tag` is supplied.
After publishing, merge a version bump into `dev` before the next release.

`.github/workflows/release.yml` builds Fedora 44 and 45 RPMs in their respective containers:
`konedrive`, `konedrive-kde` and a source RPM for each Fedora. It uses each Fedora's own Rust,
one Cargo job, and disables Rust debug information while retaining Fedora's other build flags.
Each build records its source commit and retains its artifacts for 90 days.

Each Fedora's packages are installed in a clean container. The daemon must bring in no Qt;
`konedrived --version` and `konedrivectl --version` must run; every program must find its libraries;
and the window must load its interface and come up hidden on a private bus with no display.
Either build or install failure prevents publication.

The publisher retained from the workflow revision checks both build commit stamps, the six
RPM filenames and the remote tag before writing. It creates `vX.Y.Z` at the built commit only
when absent; an existing tag on another commit is an error and is never moved. It creates a
draft GitHub release, uploads the six RPMs and `SHA256SUMS`, then publishes the draft. A failed
upload leaves a new release draft; retry reuses the tag and replaces assets by filename.
An existing published release stays published during replacement.

Release notes come from `docs/release-notes/X.Y.Z.md` of the built source; without that file,
they list commit subjects since the previous version tag. Publication uses `GITHUB_TOKEN`
with `contents: write`; build and install jobs have only `contents: read`.

Publishing runs share one concurrency group and are never cancelled. GitHub keeps one waiting
run per group, replacing it when a newer one arrives. Dry runs have their own group per branch
and cannot replace a waiting publication.

## Rebuilding an existing release

Run the workflow on `onedrinux` with `release_tag=v0.2.2` (or another existing `vX.Y.Z`). With
`dry_run=true`, both Fedora builds check out that exact tag and install-check its packages.
With `dry_run=false`, it replaces that release's assets. The publisher remains at the reviewed
workflow revision. The requested tag must exist and agree with the source version and both
build stamps; this mode cannot create or move a tag. It shares the publication concurrency group.

## Where the tests run

Every pull request and push to `onedrinux` runs structure, rustdoc, generated-text and offline release-script checks
(`.github/workflows/structure.yml`). Pull requests into `dev` also run the daemon, Graph,
reason and tree unit tests, the dead-code guard, and the window/Dolphin tests
(`.github/workflows/tests.yml`). Rust is pinned to 1.98.1 for tests and documentation; the RPMs
use Fedora's compiler. Jobs use GitHub-hosted Ubuntu runners, with Fedora containers for KDE.
The mount suite (`tests/fuse-probe/run.sh`) runs by hand in a developer's `virtme-ng` VM.
No schedules or Pages deployments existed to migrate.

## The version

**One file holds it**: `version` in `[workspace.package]` of the root `Cargo.toml`. It is the
**next** release's version, `X.Y.Z`. Nothing else holds it: the spec's `Version:` is a placeholder
that `scripts/build-rpm.sh` fills in, and the window's CMake reads the file. Only the bump right
after a release (above) changes it, so feature merge requests never touch it and can merge in any
order. `scripts/version.sh` reads it, for the workflow and for `scripts/build-rpm.sh` alike:

```
scripts/version.sh release          # X.Y.Z: the file's version
scripts/version.sh local            # X.Y.Z~dev.N: any other build
scripts/version.sh commit           # HEAD's full hash
scripts/version.sh previous 0.1.2   # the tag before v0.1.2 (the release notes)
```

**A release** is `X.Y.Z` exactly. `scripts/build-rpm.sh --version X.Y.Z` refuses any version but
the file's. A tag only records a release; it plays no part in choosing the version.

**Any other build** — a local package, a dry run — is `X.Y.Z~dev.N` in the spec (RPM's `~`) and
`X.Y.Z-dev.N` in `Cargo.toml` and on screen (a semver pre-release; Cargo refuses `~`, RPM refuses
`-`). `N` is the number of commits in `HEAD`'s history, `git rev-list --count HEAD`: local history
only, no tags, no network. Every merge into `dev` is one squashed commit, so `N` grows with each,
and RPM orders the builds as they were made:

```
0.1.1~dev.9  <  0.1.1~dev.12  <  0.1.1  <  0.1.2~dev.1
```

A newer build upgrades an older one with a plain `dnf upgrade`, and the release `X.Y.Z` upgrades
every `X.Y.Z~dev.N`. Packages are built only from `dev` (a branch's `N` can pass a later `dev`
build's).

**What is shown**, as `Version 0.1.1-dev.57 · commit 5254595`: `konedrivectl --version` (its own
build, then the running daemon's, and a line asking to restart the daemon when the two differ),
`konedrived --version`, the daemon's `Version` and `Commit` properties on `org.konedrive.Accounts`,
and the window (`konedrive --version`, from `KAboutData`, and the foot of its sidebar, with a
line while a newer release is available and one while the running daemon is another build:
`docs/design/desktop.md` §4). The rule is one for Rust
(`crates/konedrive-dbus/build.rs`) and for CMake (`app/CMakeLists.txt`):

- the version: `KONEDRIVE_BUILD_VERSION` when `scripts/build-rpm.sh` sets it (the spec's `%build`
  exports it for Cargo and passes `-DKONEDRIVE_BUILD_VERSION=` to CMake); otherwise, a plain
  `cargo build` or CMake build, the file's version with `-dev`, so that it never looks like a
  release;
- the commit: `KONEDRIVE_COMMIT` when set (the same way), else `git rev-parse HEAD` at build time,
  else `unknown`.

**Nothing is committed back.** `scripts/build-rpm.sh` writes the build's version into its own
copies of `Cargo.toml`, `Cargo.lock` and the spec (`Version:`, the `commit` and `build_version`
globals, and a `%changelog` entry naming the commit). The user agent sent to Microsoft Graph
carries Cargo's version of that copy.

`--dev-tools` makes a local development package whose daemon serves the token export, for the developer's own machine; it is refused together with `--version`, and CI never
passes it.

## Downloads and update checks

Downloads are at <https://github.com/StantonMatt/onedrinux/releases>.
The window defaults to `https://api.github.com/repos/StantonMatt/onedrinux/releases/latest`
and reads GitHub's `html_url`. `KONEDRIVE_RELEASE_FORGE=gitlab` selects the retained GitLab API;
`KONEDRIVE_LATEST_RELEASE_URL` overrides either endpoint. The third-party `upstream` remote and
source attribution stay unchanged.
