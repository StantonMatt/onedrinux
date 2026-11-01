# Always-keep edits under complete transport loss, against the VM's fake Graph only.
# Unlike `outage`, `network down` closes GETs and notification sockets too.

offline_pin_of() {
    $U python3 -c "import os,sys
try: print(os.getxattr(sys.argv[1], 'user.konedrive.pin').decode())
except OSError: print('none')" "$F/$1"
}
offline_sync_of() {
    $U python3 -c "import os,sys
try: print(os.getxattr(sys.argv[1], 'user.konedrive.sync').decode())
except OSError: print('none')" "$F/$1"
}
offline_overall_is() { [ "$(daemon overall)" = "$1" ]; }
offline_socket_is() { [ "$(daemon live-changes)" = "$1" ]; }
offline_sync_is() { [ "$(offline_sync_of "$1")" = "$2" ]; }
offline_held_removal() { daemon changes held | grep -q 'removal'; }

offline_kept_saves_coalesce_and_reconnect_automatically() {
    begin_folder
    seed_file A R kept.txt 'the original'
    register_folder
    daemon pin "$F/kept.txt" > /dev/null
    check_state kept.txt hydrated
    check "the file has its keep mark" "$(offline_pin_of kept.txt)" 1
    check_within 40 "the notification socket is established before loss" offline_socket_is connected
    local lost_at=$SECONDS
    onedrive network down > /dev/null
    check "an offline save succeeds" "$(user "printf 'save 1' > kept.txt && echo saved")" saved
    check_folder_holds kept.txt 'save 1'
    check_within 10 "transport loss drops the notification socket" offline_socket_is connecting
    check_within 70 "network loss becomes Offline without a refresh" offline_overall_is 'offline unreachable'
    echo "    timing: Offline after $((SECONDS - lost_at)) s (no NetworkManager or refresh)"
    check "offline is a running folder" "$(daemon status | cut -d, -f1)" 'state Ready'
    check_holds "the file has a pending update" "$(daemon upload-queue)" 'update '
    check "network retries do not block the queue" "$(daemon upload-queue-status | cut -d, -f2)" ' blocked 0'
    check "unsent content has the pending emblem" "$(offline_sync_of kept.txt)" pending
    check "ten more offline saves succeed" "$(user 'for n in $(seq 2 11); do printf "save %s" "$n" > kept.txt || exit 1; done; echo saved')" saved
    check_folder_holds kept.txt 'save 11'
    check "nothing reached Graph offline" "$(onedrive changes)" 0
    local returned_at=$SECONDS
    onedrive network up > /dev/null
    check_onedrive_holds kept.txt 'save 11' 70
    echo "    timing: final content uploaded after $((SECONDS - returned_at)) s (no NetworkManager or refresh)"
    check_sent
    check "only the final content was uploaded" "$(onedrive count POST createUploadSession)" 1
    check "one small-file fragment" "$(onedrive count PUT upload/)" 1
    check "the original cloud item was updated" "$(onedrive id kept.txt)" A
    check "the keep mark remains" "$(offline_pin_of kept.txt)" 1
    check_within 10 "the pending emblem clears" offline_sync_is kept.txt none
    check_state kept.txt hydrated
    check_within 70 "status returns to synced" offline_overall_is 'ok up-to-date'
}

# The pin belongs to the binding: the original remote file keeps an individual pin;
# a conflict copy within a kept directory inherits that directory's pin instead.
offline_keep_conflict() {
    local kind=$1 original
    begin_folder
    if [ "$kind" = folder ]; then
        seed_folder D R kept
        seed_file A D report.txt 'before'
        original=kept/report.txt
    else
        seed_file A R report.txt 'before'
        original=report.txt
    fi
    register_folder
    if [ "$kind" = folder ]; then daemon pin "$F/kept" > /dev/null; else daemon pin "$F/$original" > /dev/null; fi
    check_state "$original" hydrated
    onedrive network down > /dev/null
    daemon refresh > /dev/null
    check_within 10 "a failed listing reports Offline" offline_overall_is 'offline unreachable'
    user "printf 'my offline edit' > '$original'"
    printf '%s' 'edited in OneDrive' > "$SRC/remote"
    onedrive edit A "$SRC/remote" > /dev/null
    check_folder_holds "$original" 'my offline edit'
    onedrive network up > /dev/null
    daemon refresh > /dev/null
    local copy
    check_within 30 "a conflict copy is recorded" offline_has_conflict
    copy=$(daemon conflicts | awk '{print $2}')
    check "the original remote binding keeps its name" "$(onedrive id "$original")" A
    check_folder_holds "$original" 'edited in OneDrive'
    check "the conflict copy keeps the local bytes" "$(user "cat '$copy'")" 'my offline edit'
    check_onedrive_holds "${copy#"$F/"}" 'my offline edit'
    check_sent
    check "both files are present locally and in Graph" "$(listing)" "$(onedrive paths)"
    check "the original stays pinned" "$(offline_pin_of "$original")" 1
    if [ "$kind" = folder ]; then
        check "the copy inherits the folder pin" "$(offline_pin_of "${copy#"$F/"}")" 1
    else
        check "an individual pin stays with the remote binding" "$(offline_pin_of "${copy#"$F/"}")" none
    fi
}
offline_has_conflict() { [ -n "$(daemon conflicts)" ]; }
offline_kept_file_conflict_preserves_both_versions() { offline_keep_conflict file; }
offline_kept_folder_conflict_preserves_both_versions() { offline_keep_conflict folder; }

offline_kept_folder_creates_upload_and_removals_wait_for_the_guard() {
    begin_folder
    seed_folder D R kept
    seed_file A D remove.txt 'recoverable original'
    register_folder
    daemon pin "$F/kept" > /dev/null
    check_state kept/remove.txt hydrated
    daemon changes enable > /dev/null
    daemon changes limit removals 0 > /dev/null
    onedrive network down > /dev/null
    daemon refresh > /dev/null
    check_within 10 "the account is offline" offline_overall_is 'offline unreachable'
    user "printf 'new offline file' > kept/new.txt && rm kept/remove.txt"
    check_folder_holds kept/new.txt 'new offline file'
    check_within 10 "the offline removal enters the guard" offline_held_removal
    check "the removal made no cloud request" "$(onedrive count DELETE items)" 0
    check "Restore works without the network" "$(daemon changes restore)" restored
    check_folder_holds kept/remove.txt 'recoverable original'
    # Restoring the held removal must leave the unrelated new upload alone.
    user 'rm kept/remove.txt'
    check_within 10 "the second removal needs approval again" offline_held_removal
    local token allowed_at=$SECONDS
    token=$(daemon changes token)
    check_holds "Allow reports the queued offline removal" "$(daemon changes allow "$token")" 'pending-offline=1'
    check "Allow offline cannot delete remotely" "$(onedrive count DELETE items)" 0
    onedrive network up > /dev/null
    daemon refresh > /dev/null
    check_onedrive_holds kept/new.txt 'new offline file' 45
    # A request recorded during the offline interval may first need lost-answer
    # settlement, whose retry is separate from the removal's 30-second quiet time.
    check_within 90 "the approved removal uploads after its quiet time" onedrive_lacks kept/remove.txt
    echo "    timing: allowed removal settled after $((SECONDS - allowed_at)) s"
    check_sent
    check "the original survives in OneDrive's recycle bin" "$(onedrive bin)" A
    check "the new file inherits the keep mark" "$(offline_pin_of kept/new.txt)" 1
    check "the retained folder matches Graph" "$(listing)" "$(onedrive paths)"
}

offline_kept_unsent_edits_survive_an_offline_restart() {
    begin_folder
    seed_file A R kept.txt 'before restart'
    register_folder
    daemon pin "$F/kept.txt" > /dev/null
    check_state kept.txt hydrated
    onedrive network down > /dev/null
    user "printf 'unsent before restart' > kept.txt"
    check_within 10 "the unsent edit is recorded" upload_queue_has_rows
    kill_service
    tidy_folder
    check "the backing copy survives the crash" "$($U cat "$F/kept.txt")" 'unsent before restart'
    start_service
    check_within 20 "the mount comes back while offline" mounted "$F"
    check_within 20 "the folder accepts writes while offline" takes_changes
    check_within 20 "restart still reports Offline" offline_overall_is 'offline unreachable'
    check_folder_holds kept.txt 'unsent before restart'
    check "the pin survives the restart" "$(offline_pin_of kept.txt)" 1
    check "another offline save after restart succeeds" "$(user "printf 'final after restart' > kept.txt && echo saved")" saved
    onedrive network up > /dev/null
    check_onedrive_holds kept.txt 'final after restart' 70
    check_sent
    check "the final content uploads once" "$(onedrive count POST createUploadSession)" 1
    check_folder_holds kept.txt 'final after restart'
}

offline_kept_same_size_and_mtime_edits_through_the_mount_upload() {
    begin_folder
    seed_file A R kept.txt 'before'
    register_folder
    daemon pin "$F/kept.txt" > /dev/null
    check_state kept.txt hydrated
    onedrive network down > /dev/null
    check "same-size save with the exact mtime succeeds" "$(user "python3 -c 'import os; p=\"kept.txt\"; s=os.stat(p); f=open(p,\"r+b\"); f.write(b\"after!\"); f.close(); os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns)); print(os.stat(p).st_size == s.st_size and os.stat(p).st_mtime_ns == s.st_mtime_ns)'")" True
    check_folder_holds kept.txt 'after!'
    check_within 10 "the mount records the edit despite unchanged stat data" upload_queue_has_rows
    onedrive network up > /dev/null
    daemon refresh > /dev/null
    check_onedrive_holds kept.txt 'after!'
    check_sent
}

# F298 is different: a deliberately unlocked backing file changes while no daemon
# runs. Keep this a characterization of the documented limit, not an expected failure.
offline_kept_backing_edit_preserving_stat_is_still_the_documented_limit() {
    begin_folder
    seed_file A R kept.txt 'before'
    register_folder
    daemon pin "$F/kept.txt" > /dev/null
    check_state kept.txt hydrated
    kill_service
    tidy_folder
    $U python3 -c 'import os,sys; p=sys.argv[1]; s=os.stat(p); os.chmod(p,0o644); open(p,"r+b").write(b"after!"); os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns))' "$F/kept.txt"
    onedrive network down > /dev/null
    start_service
    check_within 20 "the mount starts offline" mounted "$F"
    check_within 20 "the mount takes changes" takes_changes
    check_folder_holds kept.txt 'after!'
    onedrive network up > /dev/null
    daemon refresh > /dev/null
    check_within 20 "a listing completes after return" first_cycle_is_over
    check_within 20 "the account considers the file synced" offline_overall_is 'ok up-to-date'
    check "F298: the offline backing edit has no pending upload" "$(daemon upload-queue)" ''
    check "F298: Graph still has the original bytes" "$(onedrive compare kept.txt "$SRC/seed-A")" equal
    check_folder_holds kept.txt 'after!'
}

offline_keep() {
    echo '==== kept files with the network fully down ===='
    ip link set lo up
    for name in \
        offline_kept_saves_coalesce_and_reconnect_automatically \
        offline_kept_file_conflict_preserves_both_versions \
        offline_kept_folder_conflict_preserves_both_versions \
        offline_kept_folder_creates_upload_and_removals_wait_for_the_guard \
        offline_kept_unsent_edits_survive_an_offline_restart \
        offline_kept_same_size_and_mtime_edits_through_the_mount_upload \
        offline_kept_backing_edit_preserving_stat_is_still_the_documented_limit; do
        folder_scenario "$name"
    done
}
