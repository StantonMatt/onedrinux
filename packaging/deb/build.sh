#!/usr/bin/env bash
# Package existing release builds; never rebuild or install anything here.
set -euo pipefail
umask 022

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=${KONEDRIVE_SCRATCH:-$HOME/.cache/agent-scratch/onedrinux}
rust_build=${CARGO_TARGET_DIR:-$scratch/target}/release
app_build=${KONEDRIVE_APP_BUILD:-$scratch/app}
dolphin_build=${KONEDRIVE_DOLPHIN_BUILD:-$scratch/dolphin}
output=${1:-$scratch/deb}

for tool in cmake dpkg-architecture dpkg-deb dpkg-shlibdeps desktop-file-validate git python3 strip; do
    command -v "$tool" >/dev/null || { echo "Required tool: $tool" >&2; exit 1; }
done
arch=$(dpkg-architecture -qDEB_HOST_ARCH)
multiarch=$(dpkg-architecture -qDEB_HOST_MULTIARCH)
commit=$(git -C "$repo" rev-parse HEAD)
short_commit=$(git -C "$repo" rev-parse --short=12 HEAD)
maintainer=${KONEDRIVE_DEB_MAINTAINER:-Local Packaging <konedrive@packages.invalid>}
cargo_version=$(python3 - "$repo/Cargo.toml" <<'PY'
import sys, tomllib
with open(sys.argv[1], 'rb') as f:
    print(tomllib.load(f)['workspace']['package']['version'])
PY
)
version="$cargo_version~git$short_commit"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$repo" show -s --format=%ct HEAD)}

for artifact in "$rust_build/konedrived" "$rust_build/konedrivectl" \
    "$app_build/bin/konedrive" \
    "$dolphin_build/bin/kf6/overlayicon/konedriveoverlay.so" \
    "$dolphin_build/bin/kf6/kfileitemaction/konedriveactions.so"; do
    test -f "$artifact" || { echo "Missing build artifact: $artifact" >&2; exit 1; }
done
# Match packaging/rpm/konedrive.spec: release packages never export tokens.
# Read the entire file (not grep -q), so a read error cannot pass the guard.
if LC_ALL=C grep -aF org.konedrive.TokenExport "$rust_build/konedrived" >/dev/null; then
    echo "Refusing to package konedrived: dev-tools/token export is present" >&2
    exit 1
else
    result=$?
    test "$result" -eq 1 || exit "$result"
fi

mkdir -p -- "$output"
output=$(cd -- "$output" && pwd)
# Each invocation owns a fresh stage; it never deletes a caller's files.
stage=$(mktemp -d "$output/stage.XXXXXX")
core="$stage/konedrive"
kde="$stage/konedrive-kde"
plugin_dir="usr/lib/$multiarch/qt6/plugins"
install -Dm755 "$rust_build/konedrived" "$core/usr/bin/konedrived"
install -Dm755 "$rust_build/konedrivectl" "$core/usr/bin/konedrivectl"
DESTDIR="$kde" cmake --install "$app_build" --prefix /usr --strip
DESTDIR="$kde" cmake --install "$dolphin_build" --prefix /usr --strip
for file in "$core/usr/bin/konedrived" "$core/usr/bin/konedrivectl"; do
    strip --strip-unneeded "$file"
done

# CMake's cached layout must agree with Debian's multiarch Qt directory.
test -f "$kde/$plugin_dir/kf6/overlayicon/konedriveoverlay.so"
test -f "$kde/$plugin_dir/kf6/kfileitemaction/konedriveactions.so"
# The overlay plugin's emblems, from the same build: the 8 px set is hicolor's scalable one.
test -f "$kde/usr/share/icons/hicolor/scalable/emblems/konedrive-online-only.svg"
test -f "$kde/usr/share/icons/hicolor/16x16/emblems/konedrive-online-only.svg"
grep -qx 'Exec=/usr/bin/konedrive' "$kde/usr/share/applications/org.konedrive.KOneDrive.desktop"
desktop-file-validate "$kde/usr/share/applications/org.konedrive.KOneDrive.desktop"

mkdir -p "$core/usr/lib/systemd/user" "$core/usr/share/dbus-1/services"
sed -e 's|^ExecStart=%h/\.local/bin/konedrived$|ExecStart=/usr/bin/konedrived|' \
    -e 's|^ExecStopPost=%h/\.local/bin/konedrived --tidy$|ExecStopPost=/usr/bin/konedrived --tidy|' \
    "$repo/packaging/systemd/konedrived.service" > "$core/usr/lib/systemd/user/konedrived.service"
grep -qx 'ExecStart=/usr/bin/konedrived' "$core/usr/lib/systemd/user/konedrived.service"
grep -qx 'ExecStopPost=/usr/bin/konedrived --tidy' "$core/usr/lib/systemd/user/konedrived.service"
sed 's|^Exec=/bin/false$|Exec=/usr/bin/konedrived|' \
    "$repo/packaging/dbus/org.konedrive.Daemon.service" > "$core/usr/share/dbus-1/services/org.konedrive.Daemon.service"
grep -qx 'Exec=/usr/bin/konedrived' "$core/usr/share/dbus-1/services/org.konedrive.Daemon.service"

for package in konedrive konedrive-kde; do
    doc="$stage/$package/usr/share/doc/$package"
    mkdir -p "$doc" "$stage/$package/DEBIAN"
    install -m644 "$repo/packaging/deb/copyright" "$doc/copyright"
    install -m644 "$repo/LICENSE" "$doc/LICENSE"
    gzip -n -9 "$doc/LICENSE"
    {
        printf 'konedrive (%s) unstable; urgency=medium\n\n' "$version"
        printf '  * Local Kubuntu packages of upstream commit\n    %s.\n\n' "$commit"
        printf ' -- %s  %s\n' "$maintainer" "$(LC_ALL=C date -Ru -d "@$SOURCE_DATE_EPOCH")"
    } | gzip -n -9 > "$doc/changelog.gz"
done
install -m644 "$repo/README.md" "$repo/SECURITY.md" "$core/usr/share/doc/konedrive/"
cp -R "$repo/docs" "$core/usr/share/doc/konedrive/"
find "$core/usr/share/doc" -type d -exec chmod 755 {} +
find "$core/usr/share/doc" -type f -exec chmod 644 {} +
for script in postinst prerm postrm; do
    install -m755 "$repo/packaging/deb/konedrive.$script" "$core/DEBIAN/$script"
done

# dpkg-shlibdeps needs source control metadata. Keep it in the scratch stage,
# never create a top-level debian/ in the checkout or change a build artifact.
mkdir -p "$stage/debian"
cat > "$stage/debian/control" <<'EOF'
Source: konedrive
Section: net
Priority: optional
Maintainer: Local Packaging <konedrive@packages.invalid>

Package: konedrive
Architecture: any
Description: OneDrive daemon and command line

Package: konedrive-kde
Architecture: any
Description: KOneDrive window and Dolphin plugins
EOF
shlibs() {
    (cd "$stage" && dpkg-shlibdeps -O "$@") | sed -n 's/^shlibs:Depends=//p'
}
core_shlibs=$(shlibs -e"$core/usr/bin/konedrived" -e"$core/usr/bin/konedrivectl")
kde_shlibs=$(shlibs -e"$kde/usr/bin/konedrive" \
    -e"$kde/$plugin_dir/kf6/overlayicon/konedriveoverlay.so" \
    -e"$kde/$plugin_dir/kf6/kfileitemaction/konedriveactions.so")
test -n "$core_shlibs"
test -n "$kde_shlibs"

# QML imports/style and fusermount3 are runtime-loaded: ELF scanning misses them.
# kitemmodels is used by Kirigami Addons; preserve the RPM's explicit dependency.
qml_deps='qml6-module-qtcore, qml6-module-qtqml, qml6-module-qtqml-models, qml6-module-qtqml-workerscript, qml6-module-qtquick, qml6-module-qtquick-controls, qml6-module-qtquick-dialogs, qml6-module-qtquick-layouts, qml6-module-qtquick-templates, qml6-module-qtquick-window, qml6-module-org-kde-kirigami (>= 6.21), qml6-module-org-kde-kirigamiaddons-components (>= 1.0), qml6-module-org-kde-kirigamiaddons-formcard (>= 1.0), qml6-module-org-kde-kitemmodels, qml6-module-org-kde-quickcharts, qml6-module-org-kde-desktop'

control() {
    local package=$1 depends=$2 summary=$3 description=$4
    cat > "$stage/$package/DEBIAN/control" <<EOF
Package: $package
Version: $version
Architecture: $arch
Section: net
Priority: optional
Maintainer: $maintainer
Homepage: https://github.com/kartas39/konedrive
Installed-Size: $(du -sk "$stage/$package/usr" | cut -f1)
Depends: $depends
Description: $summary
 $description
EOF
}
control konedrive "$core_shlibs, fuse3, init-system-helpers (>= 1.69)" \
    'OneDrive files-on-demand daemon and command line' \
    'Per-user OneDrive FUSE service and konedrivectl. Accounts start read-only.'
# Keep the wire protocol split in lockstep, as the RPM does.
control konedrive-kde "konedrive (= $version), $kde_shlibs, libkf6kiocore6 (>= 6.21), dolphin, breeze-icon-theme, $qml_deps" \
    'KOneDrive window, tray icon and Dolphin plugins' \
    'Kirigami account window, file state overlays and context actions for Dolphin.'

for package in konedrive konedrive-kde; do
    (cd "$stage/$package" && find usr -type f -print0 | sort -z | xargs -0 md5sum) \
        > "$stage/$package/DEBIAN/md5sums"
    dpkg-deb --root-owner-group --build "$stage/$package" "$output/${package}_${version}_${arch}.deb"
done
printf 'Version: %s\nCommit: %s\nStage: %s\n' "$version" "$commit" "$stage"
sha256sum "$output/konedrive_${version}_${arch}.deb" "$output/konedrive-kde_${version}_${arch}.deb"
