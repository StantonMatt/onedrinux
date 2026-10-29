use super::*;
#[test]
fn held_generations_are_blocked_instead_of_sent_or_reconciled() {
    let mut given = input(vec![
        removed_here(synced_file(2, ROOT, "deleted")),
        with_content_here(synced_file(3, ROOT, "rewritten"), written_here()),
        made_file(4, ROOT, "new"),
        with_content_in_onedrive(synced_file(5, ROOT, "theirs"), hashed("newer")),
        removed_here(synced_file(6, ROOT, "unheld")),
    ]);
    given.held_for_approval = [id(2), id(3), id(4), id(5)].into();
    for held in [2, 3, 4, 5] {
        assert_eq!(
            answer(&given, held),
            Answer::Wait(WaitReason::User(Reason::HeldForApproval))
        );
    }
    assert_eq!(
        answer(&given, 6),
        onedrive_step(OneDriveStep::SendFileRemoval {
            item: versions(&given, 6)
        })
    );
}
#[test]
fn a_held_descendant_prevents_a_directory_removal_and_quiet_time_holds_removals_only() {
    let mut given = input(vec![
        removed_here(synced_directory(2, ROOT, "d")),
        removed_here(synced_file(3, id(2), "child")),
    ]);
    given.held_for_approval = [id(3)].into();
    assert!(!make_sync_plan(&given).steps.iter().any(|s| matches!(
        s,
        SyncStep::OneDrive(OneDriveStep::SendDirectoryRemoval { .. })
    )));
    let mut given = input(vec![
        removed_here(synced_file(2, ROOT, "deleted")),
        made_file(3, ROOT, "new"),
    ]);
    given.change_quiet_until.insert(id(2), given.now + 30);
    assert_eq!(
        answer(&given, 2),
        Answer::Wait(WaitReason::Time {
            until: given.now + 30,
            reason: Reason::QuietTime
        })
    );
    assert!(make_sync_plan(&given)
        .steps
        .iter()
        .any(|s| s.local_item_id() == id(3)));
}

#[test]
fn local_only_uploads_keep_their_own_reason_and_protect_their_parents() {
    let mut given = input(vec![made_directory(2, ROOT, "new"), made_file(3, id(2), "local")]);
    given.held_for_approval = [id(2), id(3)].into();
    given.local_only = [id(2), id(3)].into();
    assert_eq!(answer(&given, 2), Answer::Wait(WaitReason::User(Reason::LocalOnly)));
    assert_eq!(answer(&given, 3), Answer::Wait(WaitReason::User(Reason::LocalOnly)));
    assert!(make_sync_plan(&given).steps.is_empty());
}
