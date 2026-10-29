#!/bin/sh
# Builds the suite's programs and runs its tests in a virtme-ng guest. Nothing is mounted on the
# host. Usage: tests/fuse-probe/run.sh [<group>…] (guest.sh names the ordinary groups); ONLY="<scenario>…"
# runs those scenarios alone. `crash_consistency` is by name only: two boots, power loss.
# The run fails when a scenario fails that is not named in
# expected-to-fail.txt, or when the guest does not reach its end.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
target=${CARGO_TARGET_DIR:-$here/target}
export CARGO_TARGET_DIR="$target"
mkdir -p "$target"
# Debug by default: release builds are unnecessary for correctness and use much more
# shared disk. Opt in explicitly when measuring performance through FUSE.
profile=${FUSE_PROBE_PROFILE:-debug}
case "$profile" in
    debug) cargo build --locked --manifest-path "$here/Cargo.toml" ;;
    release) cargo build --release --locked --manifest-path "$here/Cargo.toml" ;;
    *) echo "unknown FUSE_PROBE_PROFILE: $profile"; exit 1 ;;
esac
work=$(mktemp -d "$target/run.XXXXXX")
trap 'rm -rf "$work"' EXIT
# `konedrivectl`'s tests of a mounted folder (crates/konedrivectl/tests/sync_cli), built here
# when the `cli` group is among those asked for: the binary the guest runs is this tree's.
cli_test=
case " ${*:-cli} " in
    *" cli "*)
        cli_test=$(cargo test --locked --manifest-path "$here/../../Cargo.toml" -p konedrivectl --test sync_cli --no-run --message-format=json \
            | sed -n 's/.*"executable":"\([^"]*sync_cli-[^"]*\)".*/\1/p' | tail -1)
        [ -x "$cli_test" ] || { echo "the tests of crates/konedrivectl/tests/sync_cli were not built"; exit 1; }
        ;;
esac
# The probe of KIO's trash (tests/kio/trash_probe.cpp), built here when the `kio_trash` group is
# asked for, which it is by name only: it needs the development packages of Qt 6 and KDE
# Frameworks 6 on the host, and the guest runs it with the host's libraries.
trash_probe=
case " $* " in
    *" kio_trash "*)
        cmake -S "$here/../kio" -B "$target/kio-probe" -DCMAKE_BUILD_TYPE=RelWithDebInfo > /dev/null
        cmake --build "$target/kio-probe" --target trash_probe > /dev/null
        trash_probe=$target/kio-probe/trash_probe
        ;;
esac
{
    echo '#!/bin/sh'
    echo "export CLI_TEST='$cli_test'"
    echo "export TRASH_PROBE='$trash_probe'"
    echo "export ONLY='${ONLY:-}'"
    # For a person who reads a failure: the level of the daemon's log, and how much of it a failed scenario prints.
    echo "export SERVICE_LOG='${SERVICE_LOG:-info}'"
    echo "export EVIDENCE_LOG_LINES='${EVIDENCE_LOG_LINES:-25}'"
    echo "bash '$here/guest.sh' '$target/$profile' $*"
} > "$work/inner.sh"
chmod +x "$work/inner.sh"
# The guest's disk: a file on the host's own disk, with no copy-on-write and opened with O_DIRECT,
# so the guest's writes and cold reads are the disk's and not the host's memory.
img="$work/disk.img"
if [ ! -f "$img" ]; then
    touch "$img"
    chattr +C "$img" 2> /dev/null || true
    fallocate -l 24G "$img"
fi
# By-name-only, two boots on the same image, with no graceful shutdown between them.
# Other groups keep their existing single-boot runner below.
case " $* " in
    *" crash_consistency "*)
        [ "$#" -eq 1 ] || { echo "run crash_consistency on its own (two boots)"; exit 1; }
        for phase in prepare verify; do
            printf '#!/bin/sh\nbash "%s/crash_consistency.sh" "%s/%s" "%s"\n' "$here" "$target" "$profile" "$phase" > "$work/crash-$phase.sh"
            chmod +x "$work/crash-$phase.sh"
            timeout --signal=TERM --kill-after=15s "${CRASH_BOOT_SECONDS:-300}" vng --run "${VM_KERNEL:-}" --memory "${VM_MEMORY:-8G}" --user root \
                --qemu-opts="-no-reboot -drive if=none,id=probe,file=$img,format=raw,cache=none,aio=native -device virtio-blk-pci,drive=probe" \
                --exec "$work/crash-$phase.sh" < /dev/null 2>&1 | stdbuf -oL tr -d '\r' | tee "$work/output-$phase"
        done
        grep -q '^power-cut-now:' "$work/output-prepare"
        grep -q '^guest-done$' "$work/output-verify"
        grep -q '^scenarios: 4 passed, 0 failed,' "$work/output-verify"
        exit
        ;;
esac
# Keep the host tree read-only; guest writes use virtme-ng's temporary overlays.
vng --run "${VM_KERNEL:-}" --memory "${VM_MEMORY:-8G}" --user root \
    --qemu-opts="-drive if=none,id=probe,file=$img,format=raw,cache=none,aio=native -device virtio-blk-pci,drive=probe" \
    --exec "$work/inner.sh" < /dev/null 2>&1 | stdbuf -oL tr -d '\r' | tee "$work/output"
grep -q '^guest-done$' "$work/output" || { echo "the guest did not reach its end"; exit 1; }
grep -q '^scenarios: [0-9]* passed, 0 failed,' "$work/output"
