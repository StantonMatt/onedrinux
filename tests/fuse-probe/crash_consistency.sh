#!/bin/bash
# A real unclean reboot, followed by a second boot on the SAME ext4 disk. The fake's
# initial cloud metadata is seeded identically on each boot; outgoing work is paused
# before all local mutations, so the remote fixture is unchanged at the power cut.
# Invoked by run.sh, by name only; it must never run on the host.
set -u
# The host never satisfies this guard, even when the script is invoked as root.
grep -q virtme /proc/cmdline || { echo "crash_consistency must run inside virtme"; exit 1; }
bin=$1
phase=$2
here=$(cd "$(dirname "$0")" && pwd)
. "$here/scenario_checks.sh"
. "$here/scenario_folder.sh"
modprobe fuse
mount -t fusectl none /sys/fs/fuse/connections 2>/dev/null || true
mount -t tmpfs -o size=64M tmpfs /mnt
mkdir -p /mnt/ext4 /mnt/bin
disk=$(lsblk -dn -o NAME,TYPE | awk '$2=="disk" && $1 ~ /^(vd|sd)/ {print "/dev/" $1; exit}')
[ -n "$disk" ] || exit 1
[ "$phase" != prepare ] || mkfs.ext4 -q -F "$disk"
mount "$disk" /mnt/ext4
cp "$(command -v fusermount3)" /mnt/bin/fusermount3
chmod 4755 /mnt/bin/fusermount3
export PATH=/mnt/bin:$PATH
chmod 666 /dev/fuse
W=/mnt/ext4
mkdir -p "$W/plain" "$W/logs"
chown 1000:1000 "$W/plain" "$W/logs"
U="setpriv --reuid 1000 --regid 1000 --clear-groups env PATH=$PATH HOME=$W/plain"
mounted() { grep -q " $1 fuse" /proc/self/mounts; }
ip link set lo up

crash_consistency_prepare() {
    begin_folder
    seed_file A R edit.txt aaa
    seed_file B R rename.txt renamed
    seed_file C R missing.txt present
    register_folder
    download edit.txt
    download rename.txt
    download missing.txt
    daemon pause > /dev/null
    # Baseline fixture and cloud metadata are durable. There is deliberately no sync
    # of the whole filesystem after the mutations below.
    sync
    user "mv rename.txt moved.txt"
    # Remove a backing copy behind the mount, as a torn disk operation or a restore
    # would. This MUST NOT manufacture a cloud unlink on the next start.
    local backing
    backing=$($U python3 - "$SERVICE_PID" "$F" <<'PY'
import os,sys
pid,folder=sys.argv[1:]
for fd in os.listdir('/proc/'+pid+'/fd'):
    path='/proc/'+pid+'/fd/'+fd
    try:
        if os.readlink(path)==folder and os.path.isdir(path):
            print(path); break
    except OSError: pass
PY
)
    [ -n "$backing" ] || exit 1
    $U python3 - "$backing" <<'PY'
import os,sys
root=sys.argv[1]
os.unlink(root+'/missing.txt')
fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY); os.fsync(fd); os.close(fd)
PY
    # Make the edit durable while its writer is still open, retaining size and the
    # exact old timestamp. Even those observations together must not classify it clean.
    $U python3 - "$F/edit.txt" "$WORLD/edit-ready" <<'PY' &
import os,sys,time
path,ready=sys.argv[1:]
st=os.stat(path)
f=os.open(path,os.O_RDWR)
os.write(f,b'bbb')
os.utime(f,ns=(st.st_atime_ns,st.st_mtime_ns))
os.fsync(f)
with open(ready,'w') as r: r.write('ready'); r.flush(); os.fsync(r.fileno())
d=os.open(os.path.dirname(ready),os.O_RDONLY|os.O_DIRECTORY); os.fsync(d); os.close(d)
while True: time.sleep(30)
PY
    wait_for 20 test -f "$WORLD/edit-ready" || exit 1
    # Pause an actual schema migration after ALTER but before its transaction commits.
    $U "$bin/store-probe" seed31 "$WORLD/migrate.sqlite"
    $U env KONEDRIVE_TEST_MIGRATION_PAUSE="$WORLD/migration-ready" "$bin/store-probe" open "$WORLD/migrate.sqlite" > "$LOGS/migration.out" 2>&1 &
    wait_for 20 test -f "$WORLD/migration-ready" || exit 1
    echo 'power-cut-now: open same-size same-time edit, rename, missing copy, uncommitted migration'
    echo b > /proc/sysrq-trigger
    exit 1 # reset must never return
}

if [ "$phase" = prepare ]; then
    crash_consistency_prepare
fi
WORLD=$W/scenarios/crash_consistency_prepare
LOGS=$W/logs/crash_consistency_prepare
F=$WORLD/OneDrive
SRC=$WORLD/src
[ -f "$WORLD/edit-ready" ] && [ -f "$WORLD/migration-ready" ]
rm -f "$WORLD/onedrive.in" "$WORLD/service.in"
mkfifo "$WORLD/service.in"
chown 1000:1000 "$WORLD/service.in"
start_onedrive
seed_file A R edit.txt aaa
seed_file B R rename.txt renamed
seed_file C R missing.txt present
# The fake listens on a new ephemeral port after boot. Keep every item/pending row,
# but discard its old server's delta URL so the real listing runs against this fake.
$U python3 - "$WORLD/state/fuse.sqlite" <<'PY_SQL'
import sqlite3,sys
with sqlite3.connect(sys.argv[1]) as db:
    db.execute("DELETE FROM meta WHERE key='delta_link'")
PY_SQL
start_service
wait_for_folder
# The service persists the pause across restart; allow work only after recovery.
daemon resume > /dev/null

crash_same_size_same_second_edit_is_not_clean() {
    edit_is_preserved() {
        onedrive_holds edit.txt "$WORLD/edit-expected" && return 0
        # Existing reconciliation may keep a conflict if the recreated fake reports a
        # new remote version. Require that conflict AND the exact edited bytes.
        [ -n "$(daemon conflicts)" ] || return 1
        user "find . -type f -name 'edit*' -exec cmp -s {} '$WORLD/edit-expected' \\; -print" | grep -q .
    }
    check_within 30 "the edit uploads or survives as a conflict, never clean/evictable" edit_is_preserved
}
printf bbb > "$WORLD/edit-expected"
chown 1000:1000 "$WORLD/edit-expected"
scenario crash_same_size_same_second_edit_is_not_clean print_folder_evidence

crash_rename_yields_at_most_a_duplicate() {
    # Losing an un-fsynced namespace commit may re-show its original cloud name;
    # recovery must retain the bytes at one or both names.
    local copies
    copies=$(user "find . -type f -name '*txt' -exec sh -c 'test \"\$(cat \"\$1\")\" = renamed && echo copy' sh {} \\;" | wc -l)
    check "the renamed bytes remain at one or two names" "$([ "$copies" -ge 1 ] && [ "$copies" -le 2 ] && echo yes)" yes
    check "rename recovery deletes nothing in the cloud" "$(onedrive bin)" ""
}
scenario crash_rename_yields_at_most_a_duplicate print_folder_evidence

crash_missing_copy_does_not_delete_in_cloud() {
    check "the cloud item is retained" "$(onedrive compare missing.txt "$SRC/seed-C")" equal
    check_folder_holds missing.txt present
    check "there is no manufactured cloud deletion" "$(onedrive count DELETE /items/)" 0
}
scenario crash_missing_copy_does_not_delete_in_cloud print_folder_evidence

crash_migration_keeps_old_version_and_rolls_back_schema() {
    check "the uncommitted schema and version rolled back" "$($U "$bin/store-probe" verify31 "$WORLD/migrate.sqlite" && echo yes)" yes
    check "a later migration keeps the pin" "$($U "$bin/store-probe" verify32 "$WORLD/migrate.sqlite" && echo yes)" yes
    check "the old snapshot is retained" "$(test -f "$WORLD/migrate.sqlite.pre-31" && echo yes)" yes
}
scenario crash_migration_keeps_old_version_and_rolls_back_schema print_folder_evidence
stop_folder
summarize_scenarios
echo guest-done
