# The scenarios of reading while something goes wrong or goes on beside it: a download that
# fails, a folder forgotten, a free-up, a slow file, many readers, a full disk. In each, a
# reader gets the file's content or an error, never bytes that are not the file's. Sourced by
# guest.sh; each scenario has a read-only folder of its own (scenario_folder.sh).

# seed_random <id> <parent id> <name> <bytes>: a file of random bytes in OneDrive; what it
# holds stays in $SRC/seed-<id>.
seed_random() {
    head -c "$4" /dev/urandom > "$SRC/seed-$1"
    onedrive file "$1" "$2" "$3" "$SRC/seed-$1" > /dev/null
}

# is_seed <path> <id>: whether a read of the path gives what OneDrive holds of the item.
is_seed() { user "cmp -s '$1' '$SRC/seed-$2'"; }

# check_read_gives_the_file_or_fails <path> <id>: a read gives the file whole or an error, and
# after an error the file is shown as downloaded only if it is whole.
check_read_gives_the_file_or_fails() {
    if user "cat '$1' > '$SRC/got' 2> /dev/null"; then
        check "a read that went through gave the file whole" "$(cmp "$SRC/got" "$SRC/seed-$2" 2>&1 && echo equal)" equal
    else
        echo "    ok    the read was given an error"
        # The download that goes on behind the reads may have ended whole meanwhile, by its own tries: only a
        # file that is not whole must not be shown as downloaded.
        if [ "$(state_of "$1")" = hydrated ]; then
            check "after a read that failed, a file shown as downloaded is whole" "$(is_seed "$1" "$2" && echo equal)" equal
        else
            echo "    ok    after a read that failed, the file is not shown as downloaded"
        fi
    fi
}

download_cut_off_gives_no_wrong_bytes() {
    begin_folder
    seed_random X R big.bin 8388608
    register_folder read-only
    # The next downloads of the whole file end after 1 MiB.
    onedrive cut X 1048576 6 > /dev/null
    check_read_gives_the_file_or_fails big.bin X
    check "its size is still the file's" "$(user "stat -c %s big.bin")" 8388608
    check_holds "a download asked for by name does not end as if it were whole" "$(daemon hydrate "$F/big.bin"; is_seed big.bin X && echo ' equal')" equal
    for _ in 1 2 3 4 5 6; do user "cat big.bin > /dev/null 2>&1" && break; done
    check "a later read gives the file whole" "$(is_seed big.bin X && echo equal)" equal
    check_state big.bin hydrated
}

# A download asked for by name whose every try OneDrive ends after the first half: it fails,
# keeps what it has with the place it is checked up to, and the next asking goes on from
# there. (A file that is read, as `cat` reads it, is not cut by these answers: its reads ask
# for ranges, which the first half answers rightly.)
download_cut_off_keeps_what_it_has_and_goes_on_from_there() {
    begin_folder
    seed_random X R big.bin 41943040
    register_folder read-only
    check_within 10 "the named file is placed" folder_has big.bin
    $U "$bin/watch-client" "$F" 12 states > "$LOGS/recovery-watch" &
    local watcher=$!
    check_within 3 "the recovery watcher is ready" grep -q '^watching$' "$LOGS/recovery-watch"
    # As many as the download tries before it gives up.
    onedrive cut X 20971520 3 > /dev/null
    check_holds "the download fails" "$(daemon hydrate "$F/big.bin")" "refused"
    check "the exhausted download is shown as failed" "$(state_of big.bin)" failed
    check "its parent reports the failure too" "$(state_of .)" failed
    check "the daemon's state answer agrees" "$(daemon state "$F/big.bin")" failed
    check_within 3 "Dolphin is notified of the failure" grep -q '^state: big.bin failed$' "$LOGS/recovery-watch"
    local left
    left=$(daemon staged)
    check_holds "the first half is kept, unfinished" "$left" "[(20971520, Some("
    check "OneDrive was asked three times" "$(onedrive count GET dl/X)" 3
    check "asked again, the download ends" "$(daemon hydrate "$F/big.bin")" downloaded
    check "with one more request" "$(onedrive count GET dl/X)" 4
    check "nothing is left unfinished" "$(daemon staged)" "[]"
    check "the file is whole" "$(is_seed big.bin X && echo equal)" equal
    check_state big.bin hydrated
    check "the parent no longer reports failure" "$(state_of .)" hydrated
    check "the daemon's state answer recovered" "$(daemon state "$F/big.bin")" hydrated
    check_within 3 "Dolphin is notified of recovery" grep -q '^state: big.bin hydrated$' "$LOGS/recovery-watch"
    wait "$watcher"
}

# OneDrive changes the file while its first version is still being downloaded: what the
# folder shows afterwards is the version OneDrive holds now.
file_changed_in_onedrive_during_its_download_is_read_as_it_is_now() {
    begin_folder
    seed_random X R big.bin 4194304
    register_folder read-only
    onedrive delay GET dl/X 3000 1 > /dev/null
    user "cat big.bin > /dev/null 2>&1" &
    local reader=$!
    sleep 1
    cp "$SRC/seed-X" "$SRC/first"
    head -c 4194304 /dev/urandom > "$SRC/seed-X"
    onedrive edit X "$SRC/seed-X" > /dev/null
    daemon refresh > /dev/null
    wait $reader
    sleep 3
    check "a read gives the version OneDrive holds now" "$(is_seed big.bin X && echo equal || { user "cmp -s big.bin '$SRC/first'" && echo "the version before the change"; })" equal
    daemon refresh > /dev/null
    sleep 1
    check "and still does after a cycle" "$(is_seed big.bin X && echo equal || { user "cmp -s big.bin '$SRC/first'" && echo "the version before the change"; })" equal
}

download_in_flight_when_the_folder_is_forgotten_leaves_no_wrong_bytes() {
    begin_folder
    seed_random X R big.bin 4194304
    register_folder read-only
    onedrive delay GET dl/X 3000 1 > /dev/null
    user "cat big.bin > '$SRC/got' 2> /dev/null; echo \$? > '$SRC/got.status'" &
    local reader=$!
    sleep 1
    check "forgotten while the reader waits" "$(daemon forget)" forgotten
    wait $reader
    if [ "$(cat "$SRC/got.status")" = 0 ]; then
        check "the reader that got the file got it whole" "$(cmp -s "$SRC/got" "$SRC/seed-X" && echo equal)" equal
    else
        echo "    ok    the reader was given an error"
    fi
    check "nothing of the file is left in the folder itself" "$(user "ls -A" | tr '\n' ' ')" ""
    daemon register "$F" > /dev/null
    wait_for_folder read-only
    check "registered again, the file is not shown as downloaded" "$(state_of big.bin)" online-only
    check "and is read whole" "$(is_seed big.bin X && echo equal)" equal
}

free_up_while_a_reader_holds_the_file_gives_it_no_wrong_bytes() {
    begin_folder
    seed_random X R big.bin 2097152
    register_folder read-only
    download big.bin
    # A reader that stops in the middle of the file for two seconds.
    $U python3 -c "
import sys, time
with open(sys.argv[1], 'rb') as f:
    start = f.read(65536)
    time.sleep(2)
    rest = f.read()
open(sys.argv[2], 'wb').write(start + rest)" "$F/big.bin" "$SRC/got" &
    local reader=$!
    sleep 0.5
    echo "    note  the free-up while the file is held open: $(daemon dehydrate "$F/big.bin")"
    wait $reader
    check "the reader got the file whole" "$(cmp -s "$SRC/got" "$SRC/seed-X" && echo equal)" equal
    check "a read afterwards gives the file whole" "$(is_seed big.bin X && echo equal)" equal
}

resident_replacement_keeps_readers_and_mmaps_on_one_version() {
    begin_folder
    seed_random X R big.bin 4194304
    register_folder read-only
    daemon pin "$F/big.bin" > /dev/null
    check_state big.bin hydrated
    $U python3 -c "
import mmap, pathlib, sys, time
with open(sys.argv[1], 'rb', buffering=0) as f:
    try:
        mapping = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ, trackfd=False)
    except TypeError:
        mapping = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    start = f.read(65536)
    _ = mapping[:65536]
    pathlib.Path(sys.argv[2] + '/reader-ready').touch()
    deadline = time.monotonic() + 60
    while not pathlib.Path(sys.argv[2] + '/reader-continue').exists():
        if time.monotonic() >= deadline: raise RuntimeError('reader release timed out')
        time.sleep(.01)
    pathlib.Path(sys.argv[2] + '/reader-got').write_bytes(start + f.read())
    f.close()
    pathlib.Path(sys.argv[2] + '/reader-closed').touch()
    while not pathlib.Path(sys.argv[2] + '/mapping-continue').exists():
        if time.monotonic() >= deadline: raise RuntimeError('mapping release timed out')
        time.sleep(.01)
    pathlib.Path(sys.argv[2] + '/mapping-got').write_bytes(mapping[:])
    mapping.close()" "$F/big.bin" "$SRC" &
    local reader=$!
    check_within 10 "the reader and mapping are open" test -f "$SRC/reader-ready"
    python3 -c "import pathlib, os; pathlib.Path('$SRC/new-version').write_bytes(os.urandom(4194304))"
    onedrive edit X "$SRC/new-version" > /dev/null
    daemon refresh > /dev/null
    replacement_started() { [ "$(onedrive count GET dl/X)" -ge 2 ]; }
    check_within 20 "the replacement download started" replacement_started
    check "new opens still read the complete old version while it is held" "$(is_seed big.bin X && echo equal)" equal
    touch "$SRC/reader-continue"
    check_within 10 "the file descriptor closed while its mmap stays" test -f "$SRC/reader-closed"
    check "the mmap alone still keeps the complete old version" "$(is_seed big.bin X && echo equal)" equal
    touch "$SRC/mapping-continue"
    wait $reader
    check "the held reader got exactly the old version" "$(cmp -s "$SRC/reader-got" "$SRC/seed-X" && echo equal)" equal
    check "the held mmap got exactly the old version" "$(cmp -s "$SRC/mapping-got" "$SRC/seed-X" && echo equal)" equal
    replacement_landed() { user "cmp -s big.bin '$SRC/new-version'"; }
    check_within 20 "after close the complete new version lands" replacement_landed
    check_state big.bin hydrated
    python3 -c "import pathlib; pathlib.Path('$SRC/grown-version').write_bytes(b'G' * 8388608)"
    onedrive delay GET dl/X 3000 1 > /dev/null
    onedrive edit X "$SRC/grown-version" > /dev/null
    daemon refresh > /dev/null
    growth_started() { [ "$(onedrive count GET dl/X)" -ge 3 ]; }
    check_within 20 "the larger replacement started" growth_started
    check_state big.bin hydrated
    # Resident state and an admitted request do not prove replacement landing:
    # the old verified copy stays resident while the new transfer runs.
    growth_landed() { user "cmp -s big.bin '$SRC/grown-version'"; }
    check_within 20 "a new open sees the complete larger version" growth_landed
    python3 -c "import pathlib; pathlib.Path('$SRC/shrunk-version').write_bytes(b'S' * 2048)"
    onedrive delay GET dl/X 3000 1 > /dev/null
    onedrive edit X "$SRC/shrunk-version" > /dev/null
    daemon refresh > /dev/null
    shrink_started() { [ "$(onedrive count GET dl/X)" -ge 4 ]; }
    check_within 20 "the smaller replacement started" shrink_started
    check_state big.bin hydrated
    shrink_landed() { user "cmp -s big.bin '$SRC/shrunk-version'"; }
    check_within 20 "a new open sees the smaller version with no cached tail" shrink_landed

}

slow_download_does_not_stall_another_file() {
    begin_folder
    seed_random X R slow.bin 1048576
    seed_random Y R other.bin 1048576
    register_folder read-only
    onedrive delay GET dl/X 6000 1 > /dev/null
    user "cat slow.bin > '$SRC/got'" &
    local reader=$! began ended
    sleep 1
    began=$(date +%s%N)
    check "the other file is read whole meanwhile" "$(is_seed other.bin Y && echo equal)" equal
    ended=$(date +%s%N)
    check "within three seconds" "$([ $(((ended - began) / 1000000)) -lt 3000 ] && echo yes || echo "no: $(((ended - began) / 1000000)) ms")" yes
    check "while the slow file still waits" "$(state_of slow.bin)" hydrating
    wait $reader
    check "the slow file is read whole in the end" "$(cmp -s "$SRC/got" "$SRC/seed-X" && echo equal)" equal
}

many_files_read_at_once_all_get_their_content() {
    begin_folder
    seed_folder M R many
    for n in $(seq -w 1 200); do seed_file M$n M f$n.txt "the content of file $n, which no other file has"; done
    register_folder read-only
    # A finished listing can precede the planner placing all of its children.
    many_files_are_shown() { [ "$(user "ls many" | wc -l)" -eq 200 ]; }
    check_within 20 "all 200 files are placed before the concurrent reads" many_files_are_shown
    # This scenario exercises the transfer pool with 200 deliberate concurrent reads.
    # The guard's cutoff is exercised separately in download_guard.sh.
    daemon guard enabled off > /dev/null
    mkdir "$SRC/got"
    chown 1000:1000 "$SRC/got"
    user "ls many | xargs -P 64 -I{} sh -c 'cat many/{} > $SRC/got/{} 2> /dev/null || echo {} >> $SRC/refused'"
    check "every reader was answered with the file" "$(cat "$SRC/refused" 2> /dev/null | wc -l)" 0
    local wrong=0
    for n in $(seq -w 1 200); do cmp -s "$SRC/got/f$n.txt" "$SRC/seed-M$n" || wrong=$((wrong + 1)); done
    check "no reader got bytes that are not the file's" "$wrong" 0
    check "all 200 were read" "$(ls "$SRC/got" | wc -l)" 200
}

download_onto_a_full_disk_fails_and_works_once_there_is_room() {
    begin_folder small-disk
    seed_random X R big.bin 8388608
    register_folder read-only
    # The disk filled to its last byte, and then 2 MiB given back: a download begins and
    # cannot end.
    user "dd if=/dev/zero of=../ballast bs=1M 2> /dev/null; dd if=/dev/zero of=../ballast bs=1 oflag=append conv=notrunc 2> /dev/null; sync; truncate -s -2M ../ballast; sync"
    check_read_gives_the_file_or_fails big.bin X
    check_holds "a download asked for by name is refused" "$(daemon hydrate "$F/big.bin")" "refused"
    check "the exhausted download is shown as failed" "$(state_of big.bin)" failed
    check "the folder still answers" "$(user "ls")" big.bin
    user "rm ../ballast; sync"
    check "with room again, the download goes through" "$(daemon hydrate "$F/big.bin")" downloaded
    check "and the file is read whole" "$(is_seed big.bin X && echo equal)" equal
    check_state big.bin hydrated
}

# OneDrive asks the account to wait (a throttle with a Retry-After): a file that is not on
# this computer is refused at once, with "Resource temporarily unavailable", and not kept for
# the wait; what is on this computer and what the store answers goes on; after the wait the
# same read gives the file. The wait is started by a refused write (a new directory): the
# service of this suite waits out a refused read for 20 ms at most (`fuse-service.rs`,
# `RetryPolicy`), and a refused write tells the pool the whole Retry-After.
read_while_onedrive_asks_to_wait_fails_at_once_and_works_after() {
    begin_folder
    seed_random X R away.bin 1048576
    seed_random Y R here.bin 1048576
    register_folder
    check_holds "a file is downloaded before the wait" "$(daemon hydrate "$F/here.bin")" downloaded
    onedrive throttle POST children 0 8 1 > /dev/null
    user "mkdir made"
    asked_to_wait() { [ "$(onedrive count POST children)" -ge 1 ]; }
    check_within 30 "OneDrive asked to wait" asked_to_wait
    sleep 0.5
    local began ended
    began=$(date +%s%N)
    check_holds "a read of a file that is not on this computer says so" "$(user "cat away.bin 2>&1 > /dev/null")" "Resource temporarily unavailable"
    ended=$(date +%s%N)
    check "within two seconds, not after the wait" "$([ $(((ended - began) / 1000000)) -lt 2000 ] && echo yes || echo "no: $(((ended - began) / 1000000)) ms")" yes
    check "the errno is EAGAIN" "$(errno_of "open('away.bin', 'rb').read(1)")" EAGAIN
    check_holds "a download asked for by name is refused" "$(daemon hydrate "$F/away.bin")" "refused"
    check "the file is still not on this computer" "$(state_of away.bin)" online-only
    check "a file that is on this computer is read whole" "$(is_seed here.bin Y && echo equal)" equal
    check "its attributes are answered" "$(user "stat -c %s away.bin")" 1048576
    check_holds "the folder is listed" "$(user "ls")" away.bin
    sleep 9
    check "after the wait the file is read whole" "$(is_seed away.bin X && echo equal)" equal
}

# A real kernel INTERRUPT reaches the pending read: a libc pread avoids Python's
# automatic EINTR retry, and records the errno returned to the program.
an_interrupted_range_read_answers_eintr_without_dropping_its_reply() {
    begin_folder
    seed_random X R slow.bin 491408
    register_folder read-only
    check_within 10 "the slow file is placed before reading" folder_has slow.bin
    onedrive delay GET dl/X 6000 1 > /dev/null
    $U python3 -c "
import ctypes, errno, os, pathlib, signal, sys
root = pathlib.Path(sys.argv[2])
signal.signal(signal.SIGUSR1, lambda *_: None)
signal.siginterrupt(signal.SIGUSR1, True)
libc = ctypes.CDLL(None, use_errno=True)
libc.pread.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_longlong]
libc.pread.restype = ctypes.c_ssize_t
fd = os.open(sys.argv[1], os.O_RDONLY)
(root / 'reader-pid').write_text(str(os.getpid()))
buffer = ctypes.create_string_buffer(4096)
result = libc.pread(fd, buffer, len(buffer), 0)
(root / 'read-errno').write_text(errno.errorcode.get(ctypes.get_errno(), 'none') if result < 0 else 'none')
os.close(fd)" "$F/slow.bin" "$SRC" &
    local reader=$!
    range_is_waiting() { [ -f "$SRC/reader-pid" ] && [ "$(onedrive count GET dl/X)" -ge 1 ]; }
    check_within 10 "the range reached OneDrive before interruption" range_is_waiting
    kill -USR1 "$(cat "$SRC/reader-pid")"
    check_within 3 "the interrupted read is answered promptly" test -f "$SRC/read-errno"
    wait "$reader"
    check "the kernel read is answered EINTR" "$(cat "$SRC/read-errno")" EINTR
    check "no raw FUSE reply was dropped" "$(grep -c 'Reply not sent' "$LOGS/service.err")" 0
    check "an interrupted header read does not start a whole download" "$(state_of slow.bin)" online-only
    check "a later read still gives the right bytes" "$(is_seed slow.bin X && echo equal)" equal
}

# An actual app scope and FUSE readdir exercise the automatic trigger, not a test
# injection. Every first 16 KiB read must share its prefetched piece, without hydration.
a_desktop_folder_scan_reads_ahead_without_landing_files() {
    begin_folder
    for n in $(seq -w 0 39); do
        if [ $((10#$n % 2)) -eq 0 ]; then
            seed_random P$n R "$n.png" 24576
        else
            seed_random P$n R "$n.PNG" 1072032
        fi
    done
    seed_random J R other.jpg 24576
    onedrive folder D R directory.png > /dev/null
    register_folder read-only
    scan_names_are_shown() { [ "$(user 'ls | wc -l')" -eq 42 ]; }
    check_within 20 "all scan names are shown" scan_names_are_shown
    onedrive delay GET dl/ 300 100 > /dev/null
    mkdir -p /mnt/scan-cgroups
    mount -t cgroup2 none /mnt/scan-cgroups
    local app_group=/mnt/scan-cgroups/app-org.kde.gwenview-123.scope result
    mkdir "$app_group"
    result=$(bash -c 'echo $$ > "$1/cgroup.procs"; shift; exec "$@"' bash "$app_group" $U python3 - "$F" "$SRC" <<'PYCODE'
import os, pathlib, sys, time
folder, seeds = map(pathlib.Path, sys.argv[1:])
# Preserve the order answered by FUSE; a directory with the same suffix is skipped.
entries = [entry for entry in os.scandir(folder) if entry.name.lower().endswith('.png') and entry.is_file()]
assert len(entries) == 40
began = time.monotonic()
for entry in entries:
    fd = os.open(entry.path, os.O_RDONLY)
    try:
        answer = os.pread(fd, 16384, 0)
    finally:
        os.close(fd)
    with open(seeds / ('seed-P' + entry.name[:2]), 'rb') as expected:
        assert answer == expected.read(16384), entry.name
assert all(entry.stat().st_blocks == 0 for entry in entries)
print('40 correct cloud-only heads')
print(f'scan seconds: {time.monotonic() - began:.3f}', file=sys.stderr)
PYCODE
)
    rmdir "$app_group"
    umount /mnt/scan-cgroups
    check "the app reads the right heads and no file lands" "$result" '40 correct cloud-only heads'
    check "every file costs exactly one content request" "$(onedrive count GET dl/)" 40
    for n in $(seq -w 0 39); do
        check "file $n shares its one request with demand" "$(onedrive count GET dl/P$n)" 1
    done
    check "a JPEG is not prefetched" "$(onedrive count GET dl/J)" 0
    check "a directory is not prefetched" "$(onedrive count GET dl/D)" 0
    check "speculation creates no admission counters" "$(daemon guard apps)" ''
    check "no whole download is staged" "$(daemon staged)" '[]'
}

# The trace's first tiny peek waits for detection. Only the opened image's decoder
# reads its full content on one handle; separate MIME and sparse Exiv2 handles do not.
a_desktop_scan_of_tiny_files_keeps_only_the_decoded_image() {
    begin_folder
    for n in $(seq -w 00 09); do
        seed_random T$n R "$n.png" "$((512 + 10#$n * 3072))"
    done
    seed_random O R opened.png 46642
    register_folder read-only
    tiny_names_are_shown() { [ "$(user 'ls | wc -l')" -eq 11 ]; }
    check_within 20 "all tiny scan names are shown" tiny_names_are_shown
    mkdir -p /mnt/tiny-scan-cgroups
    mount -t cgroup2 none /mnt/tiny-scan-cgroups
    local app_group=/mnt/tiny-scan-cgroups/app-org.kde.gwenview-123.scope
    mkdir "$app_group"
    bash -c 'echo $$ > "$1/cgroup.procs"; shift; exec "$@"' bash "$app_group" $U python3 - "$F" "$SRC" <<'PYCODE'
import os, pathlib, sys, time
folder, seeds = map(pathlib.Path, sys.argv[1:])
entries = sorted((entry for entry in os.scandir(folder) if entry.name.endswith('.png') and entry.name != 'opened.png'), key=lambda entry: entry.name)
assert len(entries) == 10
for entry in entries:
    expected = (seeds / ('seed-T' + entry.name[:2])).read_bytes()
    fd = os.open(entry.path, os.O_RDONLY)
    assert os.pread(fd, 16384, 0) == expected[:16384], entry.name
    os.close(fd)
    for at in range(0, len(expected), 4096):
        fd = os.open(entry.path, os.O_RDONLY)
        assert os.pread(fd, 4096, at) == expected[at:at + 4096], entry.name
        os.close(fd)
expected = (seeds / 'seed-O').read_bytes()
peek = os.open(folder / 'opened.png', os.O_RDONLY)
assert os.pread(peek, 16384, 0) == expected[:16384]
os.close(peek)
opened = os.open(folder / 'opened.png', os.O_RDONLY)
assert os.pread(opened, 16384, 0) == expected[:16384]
assert os.pread(opened, 30258, 16384) == expected[16384:]
os.close(opened)
time.sleep(3.2)
assert all(entry.stat().st_blocks == 0 for entry in entries)
PYCODE
    rmdir "$app_group"
    umount /mnt/tiny-scan-cgroups
    check_state opened.png hydrated
    check "the decoded image has one range and one whole download" "$(onedrive count GET dl/O)" 2
    check "eleven range fetches and only one whole download" "$(onedrive count GET dl/)" 12
    for n in $(seq -w 00 09); do
        check "peeked file $n has no whole download" "$(onedrive count GET dl/T$n)" 1
        check "peeked file $n stays cloud-only" "$(state_of "$n.png")" online-only
    done
}

# argv identifies the tiny launch before any bytes are read, without using a desktop
# program, the host's session bus, or a real account. One byte must suffice to keep it.
a_desktop_launch_argument_shows_hydrating_and_keeps_a_tiny_file() {
    begin_folder
    # The fake server command protocol tokenizes names; URL spaces are tested in Rust.
    seed_random O R opened.png 130
    seed_random A R a.png 32768
    seed_random B R b.png 32768
    register_folder read-only
    launch_names_are_shown() { [ "$(user 'ls | wc -l')" -eq 3 ]; }
    check_within 20 "launch files are shown" launch_names_are_shown
    mkdir -p /mnt/launch-cgroups
    mount -t cgroup2 none /mnt/launch-cgroups
    local app_group=/mnt/launch-cgroups/app-org.kde.gwenview-123.scope
    mkdir "$app_group"
    bash -c 'echo $$ > "$1/cgroup.procs"; shift; exec "$@"' bash "$app_group" $U python3 - "$F/opened.png" "$SRC/seed-O" <<'PYCODE'
import os, pathlib, sys, time
opened = pathlib.Path(sys.argv[1])
expected = pathlib.Path(sys.argv[2]).read_bytes()
os.stat(opened)  # earliest PID-bearing request sees argv
assert os.getxattr(opened, 'user.konedrive.state') == b'hydrating'
for name in ['a.png', 'b.png']:
    fd = os.open(opened.parent / name, os.O_RDONLY)
    os.pread(fd, 16384, 0)
    os.close(fd)
fd = os.open(opened, os.O_RDONLY)
assert os.pread(fd, 1, 0) == expected[:1]
os.close(fd)
deadline = time.monotonic() + 10
while os.getxattr(opened, 'user.konedrive.state') != b'hydrated':
    assert time.monotonic() < deadline
    time.sleep(.02)
PYCODE
    rmdir "$app_group"
    umount /mnt/launch-cgroups
    check_state "opened.png" hydrated
    check "a launch read starts one whole download" "$(onedrive count GET dl/O)" 2
    check_state a.png online-only
    check_state b.png online-only
}

# The same cloud-only metadata probes across a real daemon stop/restart. The fake
# OneDrive outlives the service, so its content-request count proves disk reuse.
header_ranges_survive_a_daemon_restart_without_refetching() {
    begin_folder
    seed_random X R image.png 3145728
    register_folder read-only
    read_header_probes() {
        $U python3 - "$F/image.png" "$SRC/seed-X" <<'PYCODE'
import os, pathlib, sys
expected = pathlib.Path(sys.argv[2]).read_bytes()
fd = os.open(sys.argv[1], os.O_RDONLY)
for at, size in [(0, 16384), (65536, 4096), (1048576, 4096), (2097152, 4096)]:
    assert os.pread(fd, size, at) == expected[at:at+size]
os.close(fd)
PYCODE
    }
    read_header_probes
    check_state image.png online-only
    check "the first probes fetched three blocks" "$(onedrive count GET dl/X)" 3
    header_writes_are_done() {
        python3 - "$WORLD/state/cache" <<'PYCODE'
import pathlib, sqlite3, sys
files = list(pathlib.Path(sys.argv[1]).glob('*/headers.sqlite'))
if len(files) != 1:
    raise SystemExit(1)
with sqlite3.connect(files[0]) as db:
    count, size = db.execute("SELECT count(*), sum(length(bytes)) FROM headers WHERE item='X'").fetchone()
    assert (count, size) == (4, 28672), (count, size)
PYCODE
    }
    check_within 10 "only the four requested intervals were persisted" header_writes_are_done
    check "the daemon stops cleanly" "$(stop_service)" Finished
    restart_service read-only
    read_header_probes
    check "the same probes fetched no bytes after restart" "$(onedrive count GET dl/X)" 3
    check_state image.png online-only
    # Unasked padding in the earlier 1 MiB blocks must not have survived the restart.
    $U python3 - "$F/image.png" "$SRC/seed-X" <<'PYCODE'
import os, pathlib, sys
fd = os.open(sys.argv[1], os.O_RDONLY)
assert os.pread(fd, 4096, 131072) == pathlib.Path(sys.argv[2]).read_bytes()[131072:135168]
os.close(fd)
PYCODE
    check "an unasked range needs a new fetch" "$(onedrive count GET dl/X)" 4
}

# The group: every scenario of reading beside a fault.
file_reads() {
    echo "==== reading beside a fault ===="
    ip link set lo up
    for name in \
        header_ranges_survive_a_daemon_restart_without_refetching \
        a_desktop_folder_scan_reads_ahead_without_landing_files \
        a_desktop_scan_of_tiny_files_keeps_only_the_decoded_image \
        a_desktop_launch_argument_shows_hydrating_and_keeps_a_tiny_file \
        an_interrupted_range_read_answers_eintr_without_dropping_its_reply \
        download_cut_off_gives_no_wrong_bytes \
        download_cut_off_keeps_what_it_has_and_goes_on_from_there \
        file_changed_in_onedrive_during_its_download_is_read_as_it_is_now \
        download_in_flight_when_the_folder_is_forgotten_leaves_no_wrong_bytes \
        free_up_while_a_reader_holds_the_file_gives_it_no_wrong_bytes \
        resident_replacement_keeps_readers_and_mmaps_on_one_version \
        slow_download_does_not_stall_another_file \
        many_files_read_at_once_all_get_their_content \
        read_while_onedrive_asks_to_wait_fails_at_once_and_works_after \
        download_onto_a_full_disk_fails_and_works_once_there_is_room; do
        folder_scenario $name
    done
}
