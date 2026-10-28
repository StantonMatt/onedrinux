# Remote-content admission, over the fake Graph only, in the VM's own mount.

a_bulk_reader_is_cut_off_at_the_default_file_limit() {
    begin_folder
    for n in $(seq 1 35); do seed_file F$n R f$n.txt "file $n"; done
    register_folder read-only
    local result
    result=$($U python3 - "$F" <<'PY'
import errno, os, sys
ok, refused = 0, []
for n in range(1, 36):
    try:
        with open(f'{sys.argv[1]}/f{n}.txt', 'rb') as f:
            assert f.read() == f'file {n}'.encode()
        ok += 1
    except OSError as e:
        refused.append(errno.errorcode[e.errno])
print(ok, ','.join(refused))
PY
)
    check "one bulk reader gets thirty files, then EACCES" "$result" "30 EACCES,EACCES,EACCES,EACCES,EACCES"
    for n in $(seq 31 35); do
        check_state f$n.txt online-only
        check "file $n fetched no content" "$(onedrive count GET dl/F$n)" 0
    done
    check_holds "the block is recorded" "$(daemon guard apps)" "true bulk 30"
    check "refusal changes no folder error" "$(daemon status | sed 's/.*last error //')" '""'
    check_holds "refusal records no failed file activity" "$(daemon activity)" "Downloaded"
    check "no guard refusal is a sync failure" "$(daemon activity | grep -c 'kind: Failed')" 0
}

background_readers_are_refused_before_fetch_and_local_content_still_works() {
    begin_folder
    seed_file A R away.txt "remote content"
    seed_file B R here.txt "local content"
    register_folder read-only
    daemon hydrate "$F/here.txt" > /dev/null
    # A copied Python interpreter has the same procfs executable basename as the reader.
    # Changing argv[0] alone would not exercise executable-based identification.
    for reader in baloo_file baloo_file_extractor dolphin; do
        cp /usr/bin/python3 "$SRC/$reader"
        chown 1000:1000 "$SRC/$reader"
        check_holds "$reader cannot open remote content" "$($U "$SRC/$reader" -c 'import sys; print(open(sys.argv[1]).read())' "$F/away.txt" 2>&1)" "Permission denied"
        check "$reader can still read local content" "$($U "$SRC/$reader" -c 'import sys; print(open(sys.argv[1]).read())' "$F/here.txt")" "local content"
    done
    # Simulate KF6's command line with a real executable whose basename is kioworker.
    cp /usr/bin/python3 "$SRC/kioworker"
    chown 1000:1000 "$SRC/kioworker"
    local thumbnail
    thumbnail=$($U "$SRC/kioworker" - "$F/away.txt" /usr/lib/qt6/plugins/kf6/kio/thumbnail.so <<'PY'
import errno, sys
try:
    open(sys.argv[1], 'rb').read()
    print('unexpected success')
except OSError as e:
    print(errno.errorcode[e.errno])
PY
)
    check "the KIO thumbnail role is refused" "$thumbnail" EACCES
    check "nothing was fetched" "$(onedrive count GET dl/A)" 0
    check_state away.txt online-only
    check_holds "blocking is visible" "$(daemon guard apps)" background
}

normal_open_and_kio_file_copy_still_hydrate() {
    begin_folder
    seed_file A R normal.txt "opened deliberately"
    seed_file B R copied.txt "copied deliberately"
    register_folder read-only
    check "a normal single open reads the file" "$(user 'cat normal.txt')" "opened deliberately"
    check_within 10 "the normal file lands" state_is normal.txt hydrated
    cp /usr/bin/python3 "$SRC/kioworker"
    chown 1000:1000 "$SRC/kioworker"
    check "file.so can copy cloud content" "$($U "$SRC/kioworker" -c 'import sys; print(open(sys.argv[1]).read())' "$F/copied.txt" /usr/lib/qt6/plugins/kf6/kio/file.so)" "copied deliberately"
    check_within 10 "the copied file lands" state_is copied.txt hydrated
}

explicit_hydrate_and_pin_bypass_a_blocked_app() {
    begin_folder
    seed_file A R blocked.txt "blocked"
    seed_file B R hydrated.txt "explicit hydrate"
    seed_file C R pinned.txt "explicit pin"
    register_folder read-only
    # Kubuntu may implement cat as a symlink to a multicall coreutils executable.
    local cat_identity="exe:$(readlink -f /usr/bin/cat)"
    daemon guard rule block "$cat_identity" > /dev/null
    check_holds "the app is blocked" "$(user 'cat blocked.txt 2>&1')" "Permission denied"
    check_holds "explicit hydrate succeeds" "$(daemon hydrate "$F/hydrated.txt")" downloaded
    check "the blocked app reads that local content" "$(user 'cat hydrated.txt')" "explicit hydrate"
    check_holds "explicit pin succeeds" "$(daemon pin "$F/pinned.txt")" pinned
    check_within 10 "pin lands despite the app block" state_is pinned.txt hydrated
    check "the blocked app reads the pin" "$(user 'cat pinned.txt')" "explicit pin"
    check_holds "free up also bypasses the block" "$(daemon free-up "$F/pinned.txt")" "Freed"
    check_state pinned.txt online-only
    check_state blocked.txt online-only
    check "the refused file fetched no content" "$(onedrive count GET dl/A)" 0
    daemon guard rule session "$cat_identity" > /dev/null
    check "session approval unblocks new reads" "$(user 'cat blocked.txt')" blocked
    daemon guard rule reset "$cat_identity" > /dev/null
}

read_below_the_reserve_stays_as_ranges() {
    begin_folder small-disk
    seed_random A R range.bin 2097152
    register_folder read-only
    # Default reserve is 10 GiB, more than this isolated 64 MiB filesystem.
    check "the range read is whole" "$(is_seed range.bin A && echo equal)" equal
    check_state range.bin online-only
    check "no unfinished whole download consumed disk" "$(daemon staged)" "[]"
    check_holds "an explicit hydrate still works on request" "$(daemon hydrate "$F/range.bin")" downloaded
    check_state range.bin hydrated
}

byte_budget_applies_to_handles_opened_before_the_block() {
    begin_folder
    seed_file A R first.txt "12345678"
    seed_file B R second.txt "abcdefgh"
    seed_file C R third.txt "ABCDEFGH"
    register_folder read-only
    daemon guard limits 30 16 600 > /dev/null
    local result
    result=$($U python3 - "$F" <<'PYCODE'
import errno, sys
root = sys.argv[1]
with open(root + '/first.txt', 'rb') as first, open(root + '/second.txt', 'rb') as second:
    assert first.read() == b'12345678'
    try:
        second.read()
        print('unexpected read')
    except OSError as e:
        print('read', errno.errorcode[e.errno])
    try:
        open(root + '/third.txt', 'rb')
        print('unexpected open')
    except OSError as e:
        print('open', errno.errorcode[e.errno])
PYCODE
)
    check "open handles cannot bypass a new byte block" "$result" $'read EACCES\nopen EACCES'
    check_within 10 "the already-admitted transfer can finish" state_is first.txt hydrated
    check_state second.txt online-only
    check_state third.txt online-only
    check "the refused read fetches no bytes" "$(onedrive count GET dl/B)" 0
    check_holds "range and whole bytes were reserved" "$(daemon guard apps)" "true bulk 1 16"
    check "the same blocked app reads local content" "$($U python3 -c 'import sys; print(open(sys.argv[1]).read())' "$F/first.txt")" "12345678"
}

mmap_of_a_cloud_file_uses_the_callers_budget() {
    begin_folder
    seed_file A R mapped.txt "mapped deliberately"
    register_folder read-only
    local result
    result=$($U python3 - "$F/mapped.txt" <<'PYCODE'
import mmap, sys
with open(sys.argv[1], 'rb') as f:
    with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
        print(m[:].decode())
PYCODE
)
    check "an application can mmap cloud content" "$result" "mapped deliberately"
    check_within 10 "the mapped content lands" state_is mapped.txt hydrated
}

app_downloads_can_be_reclaimed_without_touching_kept_open_or_cloud_copies() {
    begin_folder
    for n in $(seq 1 8); do seed_file F$n R f$n.txt "file $n"; done
    register_folder
    daemon guard limits 6 2147483648 600 > /dev/null
    local result identity holder bytes reclaimed
    identity="exe:$(readlink -f /usr/bin/python3)"
    result=$($U python3 - "$F" <<'PYCODE'
import errno, sys
root = sys.argv[1]
for n in range(1, 7):
    with open(f'{root}/f{n}.txt', 'rb') as f:
        assert f.read() == f'file {n}'.encode()
try:
    open(root + '/f7.txt', 'rb').read()
    print('unexpected success')
except OSError as e:
    print(errno.errorcode[e.errno])
PYCODE
)
    check "the reader downloads six files then is blocked" "$result" EACCES
    check_holds "this app was blocked for bulk reads" "$(daemon guard apps)" "$identity automatic true bulk 6"
    for n in $(seq 1 6); do check_within 10 "file $n landed" state_is f$n.txt hydrated; done
    check_state f7.txt online-only
    daemon pin "$F/f3.txt" > /dev/null
    daemon hydrate "$F/f6.txt" > /dev/null
    daemon hydrate "$F/f8.txt" > /dev/null
    # Keep a writer and a reader open after the bulk reader exits. No bytes are
    # changed: the open writer alone must be sufficient to protect its copy.
    $U python3 - "$F" "$SRC" <<'PYCODE' &
import os, signal, sys
root, scratch = sys.argv[1:]
writer = os.open(root + '/f4.txt', os.O_WRONLY)
with open(root + '/f5.txt', 'rb') as reader:
    with open(scratch + '/open-ready', 'w'):
        pass
    signal.pause()
os.close(writer)
PYCODE
    holder=$!
    check_within 10 "the writer and reader are open" test -f "$SRC/open-ready"
    # FUSE may still cache the pre-landing zero-block stat. Ask the test service for
    # backing allocation facts, independently of the reclaim operation's own count.
    bytes=$(daemon backing-bytes "$F/f1.txt" "$F/f2.txt")
    check "the backing copies have allocated bytes" "$([ "$bytes" -gt 0 ] 2> /dev/null && echo yes)" yes
    reclaimed=$(daemon guard free "$identity")
    check "exactly the two eligible copies are freed, with actual allocated bytes" "$reclaimed" "2 files $bytes bytes"
    check_state f1.txt online-only
    check_state f2.txt online-only
    for n in 3 4 5 6 8; do check_state f$n.txt hydrated; done
    check "a repeated call cannot count freed files again" "$(daemon guard free "$identity")" "0 files 0 bytes"
    kill -TERM "$holder"
    wait "$holder" 2> /dev/null || true
    # Release is asynchronous. Retry reclamation until both handles have closed,
    # accumulating actual results so even a partial first retry is counted exactly.
    local after_close=0
    reclaim_closed_app_copies() {
        local result
        result=$(daemon guard free "$identity")
        after_close=$((after_close + ${result%% files*}))
        state_is f4.txt online-only && state_is f5.txt online-only
    }
    check_within 10 "the former writer and reader can now be reclaimed" reclaim_closed_app_copies
    unset -f reclaim_closed_app_copies
    check "exactly two copies are reclaimed after close" "$after_close" 2
    check_state f4.txt online-only
    check_state f5.txt online-only
    daemon unpin "$F/f3.txt" > /dev/null
    check "an explicit pin keeps its copy even after unpin" "$(daemon guard free "$identity")" "0 files 0 bytes"
    check_state f3.txt hydrated
    check_state f6.txt hydrated
    check_state f8.txt hydrated
    for n in $(seq 1 8); do
        check "cloud copy $n is unchanged" "$(onedrive compare f$n.txt "$SRC/seed-F$n")" equal
    done
    check "reclamation never calls Graph delete" "$(onedrive count DELETE items)" 0
    check "no cloud version was uploaded by reclamation" "$(daemon upload-queue)" ""
    check "unknown identities are harmless" "$(daemon guard free exe:/unknown)" "0 files 0 bytes"
}

download_guard() {
    echo "==== download guard ===="
    ip link set lo up
    for name in \
        a_bulk_reader_is_cut_off_at_the_default_file_limit \
        background_readers_are_refused_before_fetch_and_local_content_still_works \
        normal_open_and_kio_file_copy_still_hydrate \
        explicit_hydrate_and_pin_bypass_a_blocked_app \
        read_below_the_reserve_stays_as_ranges \
        byte_budget_applies_to_handles_opened_before_the_block \
        mmap_of_a_cloud_file_uses_the_callers_budget \
        app_downloads_can_be_reclaimed_without_touching_kept_open_or_cloud_copies; do
        folder_scenario "$name"
    done
}
