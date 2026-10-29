# Outgoing mutation drills. All paths and accounts belong to this VM and its fake Graph.

# A thousand durable conflict copies can take more than converse's ordinary minute.
# Wait for this exact reply before another command, so a late answer cannot be taken
# for the following request. The production CLI likewise gives Restore a long call.
mass_restore_long() {
    local before answer prefix='done changes restore: '
    before=$(grep -c '^done ' "$LOGS/service.out")
    echo 'changes restore' >&8
    for _ in $(seq 12000); do
        if [ "$(grep -c '^done ' "$LOGS/service.out")" -gt "$before" ]; then
            answer=$(grep '^done ' "$LOGS/service.out" | sed -n "$((before + 1))p")
            printf '%s\n' "${answer#"$prefix"}"
            return
        fi
        sleep 0.05
    done
    echo 'NO ANSWER'
}
mass_settled() { [ "$(daemon changes unsettled)" -eq 0 ] 2>/dev/null; }
mass_held() { [ "$(daemon changes count)" -gt 0 ] 2>/dev/null; }
mass_no_cloud_deletes() { check "no DELETE reached Graph" "$(onedrive count DELETE items)" 0; }
mass_enable() {
    daemon changes enable > /dev/null
}
mass_hold_every_kind() {
    mass_enable
    for kind in removals rewrites moves uploads; do daemon changes limit "$kind" 0 > /dev/null; done
}
mass_tree_is_shown() { [ "$(user 'find tree -type f | wc -l')" -eq "$1" ] 2>/dev/null; }
mass_wait_tree() { check_within 30 "every seeded file is shown before the drill" mass_tree_is_shown "$1"; }
mass_seed_tree() {
    local count=$1
    seed_folder D R tree
    for n in $(seq 1 "$count"); do seed_file F$n D f$n.txt "original $n"; done
}
mass_rmtree_allow() {
    begin_folder
    mass_seed_tree 500
    register_folder
    mass_wait_tree 500
    mass_enable
    user 'rm -rf tree'
    check_within 20 "the 500-file burst is held" mass_held
    mass_no_cloud_deletes
    check "the cloud tree stays" "$(onedrive id tree)" D
    check "allow selects the held batch" "$(daemon changes allow)" allowed
    check_within 60 "the allowed tree is deleted in the fake" onedrive_lacks tree
}
mass_rmtree_restore_offline() {
    begin_folder
    mass_seed_tree 500
    register_folder
    mass_wait_tree 500
    # Explicit hydration bypasses the download guard's per-reader budget.
    daemon hydrate "$F/tree" > /dev/null
    local downloads_before
    downloads_before=$(onedrive count GET dl)
    mass_enable
    user 'rm -rf tree'
    check_within 20 "the removal burst is held" mass_held
    mass_no_cloud_deletes
    check "restore completes" "$(daemon changes restore)" restored
    check_within 20 "every path is shown again" folder_has tree/f500.txt
    # No new content GET is needed: all 500 copies were set aside by rename.
    check "the first copy is restored offline" "$(user 'cat tree/f1.txt')" 'original 1'
    check "the last copy is restored offline" "$(user 'cat tree/f500.txt')" 'original 500'
    check "restoration fetches no content" "$(onedrive count GET dl)" "$downloads_before"
    mass_no_cloud_deletes
}
mass_git_clean_allow_and_restore() {
    begin_folder
    mass_seed_tree 50
    register_folder
    mass_wait_tree 50
    daemon hydrate "$F/tree" > /dev/null
    mass_hold_every_kind
    # Git metadata is local and held too. Exclude it from the clean while deleting
    # every untracked cloud file, as a runaway build cleanup does.
    user 'git init -q && git clean -xfd'
    check_within 20 "git clean is held" mass_held
    mass_no_cloud_deletes
    check "git clean restore completes" "$(daemon changes restore)" restored
    check_within 20 "git clean's files are back" folder_has tree/f50.txt
    check "the resident copy survives" "$(user 'cat tree/f1.txt')" 'original 1'
    user 'git clean -xfd'
    check_within 20 "the next clean needs fresh admission" mass_held
    daemon changes allow > /dev/null
    check_within 60 "allow sends the second clean" onedrive_lacks tree
}
mass_sed_rewrite_allow_and_restore() {
    begin_folder
    mass_seed_tree 1000
    register_folder
    mass_wait_tree 1000
    daemon hydrate "$F/tree" > /dev/null
    mass_hold_every_kind
    user "sed -i 's/original/rewrite/' tree/*.txt"
    check_within 30 "all rewrites are held" mass_held
    check_within 20 "all rewrite generations have closed" mass_settled
    check "the first cloud generation is unchanged" "$(onedrive compare tree/f1.txt "$SRC/seed-F1")" equal
    check "the last cloud generation is unchanged" "$(onedrive compare tree/f1000.txt "$SRC/seed-F1000")" equal
    mass_no_cloud_deletes
    check "restore keeps the rewrites as local conflict copies" "$(mass_restore_long)" restored
    check "the original cloud content is shown again" "$(user 'cat tree/f1.txt')" 'original 1'
    check_holds "kept conflict copies remain flagged" "$(daemon changes held)" local-only
    check_holds "kept conflict copies are in Issues" "$(daemon not-uploaded)" local-only
    check "the first cloud file still has its original bytes" "$(onedrive compare tree/f1.txt "$SRC/seed-F1")" equal
}
mass_rename_allow_and_restore() {
    begin_folder
    mass_seed_tree 250
    register_folder
    mass_wait_tree 250
    mass_enable
    user 'find tree -type f -exec sh -c '\''for p do mv "$p" "$p.moved"; done'\'' sh {} +'
    check_within 20 "the rename burst is held" mass_held
    check "cloud names are unchanged" "$(onedrive id tree/f1.txt)" F1
    check "moved cloud names do not appear" "$(onedrive id tree/f1.txt.moved)" absent
    check "restore takes the moves back" "$(daemon changes restore)" restored
    check_within 20 "the old local names return" folder_has tree/f250.txt
    user 'find tree -type f -exec sh -c '\''for p do mv "$p" "$p.moved"; done'\'' sh {} +'
    check_within 20 "the second burst needs approval" mass_held
    daemon changes allow > /dev/null
    check_within 90 "allowed moves reach the fake" onedrive_has tree/f250.txt.moved
}
mass_new_uploads_stay_local_until_allowed() {
    begin_folder
    register_folder
    mass_hold_every_kind
    user 'mkdir flood; for n in $(seq 1 501); do echo new > flood/f$n; done'
    check_within 20 "the upload flood is held" mass_held
    check_within 20 "all new upload generations have closed" mass_settled
    check "no flood folder was created remotely" "$(onedrive id flood)" absent
    check "restore leaves uploads local" "$(daemon changes restore)" restored
    check "local new bytes remain" "$(user 'cat flood/f501')" new
    check_holds "new uploads remain flagged" "$(daemon changes held)" local-only
    daemon changes limit uploads 10000 > /dev/null
    daemon changes upload flood > /dev/null
    check_within 90 "explicit approval uploads the flood" onedrive_has flood/f501
}
mass_token_scoped_allow_and_restore_results() {
    begin_folder
    seed_file A R first.txt first
    seed_file B R second.txt second
    seed_file C R third.txt third
    seed_file D R fourth.txt fourth
    register_folder
    check_within 20 "first file is shown" folder_has first.txt
    check_within 20 "second file is shown" folder_has second.txt
    check_within 20 "third file is shown" folder_has third.txt
    check_within 20 "fourth file is shown" folder_has fourth.txt
    mass_hold_every_kind
    user 'rm first.txt'
    check_within 20 "first removal is held" mass_held
    local token
    token=$(daemon changes token)
    user 'rm second.txt'
    check_holds "allow reports the shown removal only" "$(daemon changes allow "$token")" 'done=1 pending-offline=0 failed=0 kept-both-versions=0'
    check_within 30 "the shown removal reaches Graph" onedrive_lacks first.txt
    check "the later removal stays in Graph" "$(onedrive id second.txt)" B
    token=$(daemon changes token)
    user 'rm third.txt'
    check "restore reports the shown removal only" "$(daemon changes restore "$token")" 'done=1 pending-offline=0 failed=0 kept-both-versions=0'
    check_within 20 "the shown removal is restored" folder_has second.txt
    check "the later removal is not restored" "$(user 'test -e third.txt && echo present || echo absent')" absent
    check "the later removal stays in Graph" "$(onedrive id third.txt)" C
    # A same-name local replacement must cause a failed restore with the old hold kept.
    user 'mkdir third.txt; echo replacement > third.txt/kept'
    token=$(daemon changes token)
    check_holds "restore reports a collision" "$(daemon changes restore "$token")" 'failed=1'
    check_holds "the failed deletion remains held" "$(daemon changes rows)" removal
    check "the replacement survives" "$(user 'cat third.txt/kept')" replacement
    # A real rewrite restore reports the retained second version.
    user 'echo changed > fourth.txt'
    check_within 20 "the rewrite generation has closed" mass_settled
    token=$(daemon changes token)
    check_holds "rewrite restore reports a retained local copy" "$(daemon changes restore "$token")" 'kept-both-versions=1'
    check "cloud bytes return" "$(user 'cat fourth.txt')" fourth
}
mass_changes() {
    echo "==== mass outgoing changes ===="
    ip link set lo up
    for name in mass_rmtree_allow mass_rmtree_restore_offline mass_git_clean_allow_and_restore mass_sed_rewrite_allow_and_restore mass_rename_allow_and_restore mass_new_uploads_stay_local_until_allowed mass_token_scoped_allow_and_restore_results; do
        folder_scenario "$name"
    done
}
