# Releasing

## How a release happens

By hand, two steps:

1. Merge `dev` into `main`, with a merge request and a merge commit (not a squash: `main` must hold
   `dev`'s own commits, or the next merge finds them all again). The workflow below then releases `X.Y.Z`, the version in `Cargo.toml`,
   and tags the commit `vX.Y.Z`.
2. At once, a one-line merge request into `dev` that moves `version` in `[workspace.package]` of the
   root `Cargo.toml` from `X.Y.Z` to `X.Y.(Z+1)` (or to the next minor or major version, when that
   is what comes next). No other merge request changes it (below).

Every push to `main` runs the release pipeline
(`.gitlab-ci.yml`, `.gitlab/ci/release.yml`) on GitLab. It builds and publishes; it runs no test of
the code:

1. For Fedora 44 and for Fedora 45, each in its own container (`fedora:44`, `fedora:45`), it
   installs the spec's build dependencies, chooses the version (below) and builds the RPMs with
   `scripts/build-rpm.sh --version X.Y.Z`, the same script a local build uses, with that Fedora's
   own Rust: `konedrive` and `konedrive-kde` for x86_64, and the source RPM. The dist tag in the
   file names tells the two apart (`.fc44`, `.fc45`).
2. It keeps each Fedora's RPMs as the run's artifacts.
3. In a clean container of that Fedora it installs `konedrive` alone, and checks that it brings
   in no Qt and that `konedrived --version` and `konedrivectl --version` run; then `konedrive-kde`,
   as a person on Plasma would, and checks that every program finds its libraries and that the
   window loads its interface and comes up hidden, with no display, on a bus of its own. A missing
   dependency of either package shows here.
4. It tags the commit it built `vX.Y.Z` and publishes a GitLab Release, "KOneDrive X.Y.Z", with the
   six RPMs and one `SHA256SUMS` for them all. The release notes are `docs/release-notes/X.Y.Z.md`, written by hand
   and merged with the rest before the release; without that file they list commit subjects
   since the previous tag (including merged request references).

If a build or an install check fails for either Fedora, nothing is tagged or released.

It uses `CI_JOB_TOKEN` for the Releases API and generic package registry; no personal token is
needed. The Releases API creates a missing tag at the exact built commit using its `ref` field.
An existing tag is checked before any upload and again after publication; it is never moved.
Each publish job uploads to `konedrive/X.Y.Z-<job-id>/` in the generic registry and replaces
release links by filename on a rerun. Previous uploads stay available at their original URLs.
The release page and its direct asset links point to the new files, including `SHA256SUMS`.

A resource group holds the whole build/install/publish child pipeline: releases run one at a
time and are never cancelled automatically. Unlike GitHub's single waiting slot, GitLab queues
waiting releases. Dry runs have their own resource group per branch and cannot replace a
waiting release. MR checks are interruptible and cancel when a newer commit arrives.

A failed run can be rerun. It builds the same version (the file's), finds the tag it pushed on
the same commit and reuses it, and replaces the files of a release that already exists instead of
making a second one. A tag `vX.Y.Z` that already exists on another commit stops the run: that is
what happens when step 2 above was forgotten (the next merge into `main` still carries the version
already released) — merge the bump into `dev` and `dev` into `main` again.

Run by hand with **Build → Pipelines → New pipeline**, selecting a branch and the `dry_run`
variable (default `true`), or `glab ci run --branch <branch> --variables dry_run:true`.
It creates releases only on `main` with `dry_run=false`. The explicit existing-tag republish
mode below is also available; other branch runs remain dry runs.

The migrated fork's branches are `onedrinux` (default) and `dev`; it has no `main`. The trigger
above is preserved from the original workflow, not silently moved to `onedrinux`. The retained
third-party `upstream/main` and release tags show that `main` existed upstream; they do not prove
that this fork's GitHub release workflow ever ran. Automatic release remains dormant until an
explicit branch/trigger decision is made. Manual dry runs work on `onedrinux` or a task branch.

## Where the tests run

Not in the release: on the merge requests (`.gitlab/ci/checks.yml`).

**A merge request into `dev`** runs the fast tests:

- the unit tests of the daemon, the Graph client, the reasons and the tree store
  (`cargo test -p konedrived -p konedrive-graph -p konedrive-reason -p konedrive-tree --lib --features konedrived/dev-tools`),
  in `rust:1.98.1-bookworm`, as an unprivileged user; root only installs `dbus-daemon`
  for the private buses and prepares that user;
- the window's and the Dolphin plugins' tests (`ctest`), in a `fedora:45` container, where Qt and
  KDE Frameworks are the ones the packages are built with;
- the catalogue of sentences and the generated files, with the structure rules
  (`.gitlab/ci/checks.yml`).

**A merge request into any branch** runs structure, rustdoc, generated-text and release-script
checks. The daemon and window/plugin suites run only when targeting `dev`. The suite that mounts a folder
(`tests/fuse-probe/run.sh`) runs by hand, in a `virtme-ng` VM on a developer's machine.

**The Rust of the tests is fixed** (`RUST_VERSION` in `.gitlab-ci.yml`, used by the Rust image),
so that a
new compiler does not fail a merge request by itself; it is moved by hand, in a merge request. The
packages are not tied to it: each is built with its Fedora's own `rust`, as a package is.

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

## A dry run

On GitLab, **Build → Pipelines → New pipeline**, on any branch. `dry_run=true` is the default:
the run builds the RPMs, tries them in clean containers and retains them as job artifacts for
90 days. It tags and releases nothing. The RPMs carry the local version, `X.Y.Z~dev.N`, so a
later release upgrades them normally. `main` with `dry_run=false` publishes `X.Y.Z`.
All Linux jobs use the `ci-public` Docker executor; Ubuntu 24.04, pinned Rust on Debian Bookworm,
and the original Fedora 44/45 images supply the tools the jobs need. Fork MRs run in the fork,
not on this project's runners. No schedules existed to recreate.

RPM CI builds use one Cargo job and disable Rust debug information to fit the
4 GiB worker. The container's RPM macro keeps Fedora's other Rust build flags;
local builds and the tagged RPM spec retain their original settings.

Locally, the same build: `scripts/build-rpm.sh --version X.Y.Z`, with `X.Y.Z` the file's version.


## Republishing an existing release

For migrated downloads, start a manual pipeline on a branch containing the GitLab config and
set `release_tag=v0.2.2` (or the desired existing `vX.Y.Z`). Keep `dry_run=true` first: both
Fedora matrices check out that tag, build its exact source, and install-check it. Once this
pipeline succeeds, repeat with `dry_run=false` to publish its packages. The publisher stays
at the reviewed pipeline revision, verifies both build commit stamps and the remote tag, and
refuses a missing/mismatched tag; this mode cannot create or move a tag. It shares the normal
release lock. This does not enable automatic releases from `onedrinux`.

## If moving back to GitHub

`.github/workflows/` is the unchanged, inactive GitHub version. Keep it in sync by hand only
when switching back; disable GitLab pipelines before re-enabling GitHub Actions to avoid two
publishers. Preserve the branch layout and decide explicitly whether to restore `main`.
The old manual command is `gh workflow run release.yml --ref <branch>` (`dry_run` defaults true);
`-f dry_run=false` publishes only on `main`. GitHub uses `GITHUB_TOKEN` with `contents: write`,
`gh api` for tags, and `gh release create --verify-tag` / `upload --clobber` for release assets.

The current release/download page is <https://gitlab.com/StantonMatt/onedrinux/-/releases>.
The window defaults to GitLab's `releases/permalink/latest` API and reads `_links.self`.
`KONEDRIVE_RELEASE_FORGE=github` selects the retained upstream GitHub API
(`https://api.github.com/repos/kartas39/konedrive/releases/latest`, `html_url` responses).
`KONEDRIVE_LATEST_RELEASE_URL` overrides either URL, including a restored fork's API address.
The third-party `upstream` remote and source attribution are not changed by this migration.
