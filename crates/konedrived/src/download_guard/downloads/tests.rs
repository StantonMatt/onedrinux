use super::*;
use crate::mount::item_fixture::ItemFixture;

fn content() -> ContentVersion {
    ItemFixture::onedrive_content_of(b"copy")
}
fn item(n: u64) -> LocalItemId {
    LocalItemId::new(n as i64)
}

#[test]
fn only_landed_starters_remain_and_accounts_and_apps_are_separate() {
    let ledger = Arc::new(AppDownloads::default());
    let first = ledger.account(Path::new("first.db"));
    let second = ledger.account(Path::new("second.db"));
    let failed = first.begin("exe:/reader", item(1), content()).unwrap();
    assert!(first.landed("exe:/reader").is_empty());
    drop(failed);
    assert!(crate::panic::lock(&ledger.inner).copies.is_empty());
    let landed = first.begin("exe:/reader", item(1), content()).unwrap();
    landed.landed();
    drop(landed);
    assert_eq!(first.landed("exe:/reader").len(), 1);
    assert!(first.landed("exe:/other").is_empty());
    assert!(second.landed("exe:/reader").is_empty());
    let other_account = second.begin("exe:/reader", item(1), content()).unwrap();
    other_account.landed();
    first.forget_item(item(1));
    assert!(first.landed("exe:/reader").is_empty());
    assert_eq!(second.landed("exe:/reader").len(), 1);
}

#[test]
fn explicit_keep_cancels_in_flight_tickets_and_stale_snapshots() {
    let account = Arc::new(AppDownloads::default()).account(Path::new("tree.db"));
    let ticket = account.begin("app:reader", item(1), content()).unwrap();
    account.forget_item(item(1));
    ticket.landed();
    assert!(
        account.landed("app:reader").is_empty(),
        "completion cannot undo a keep request"
    );
    let later = account.begin("app:reader", item(1), content()).unwrap();
    later.landed();
    let copy = account.landed("app:reader").pop().unwrap();
    assert!(account.contains(&copy));
    account.forget_item(item(1));
    assert!(!account.contains(&copy));
    let replacement = account.begin("app:reader", item(1), content()).unwrap();
    replacement.landed();
    account.forget(&copy);
    assert_eq!(
        account.landed("app:reader").len(),
        1,
        "an old snapshot cannot consume a newer copy"
    );
}

#[test]
fn the_global_bound_includes_in_flight_entries_and_new_copies_are_untracked_at_capacity() {
    let ledger = Arc::new(AppDownloads::default());
    let first = ledger.account(Path::new("first.db"));
    let second = ledger.account(Path::new("second.db"));
    let mut active = Vec::new();
    for n in 1..=MAX_DOWNLOADS {
        active.push(
            first
                .begin("app:reader", item(n as u64), content())
                .unwrap(),
        );
    }
    assert!(second.begin("app:other", item(1), content()).is_none());
    active[0].landed();
    drop(active.remove(0));
    assert_eq!(first.landed("app:reader").len(), 1);
    assert!(
        second.begin("app:other", item(1), content()).is_none(),
        "landing does not bypass the bound"
    );
    drop(active.pop());
    assert!(
        second.begin("app:other", item(1), content()).is_some(),
        "failed transfers release capacity"
    );
    let oldest = first.landed("app:reader").pop().unwrap();
    first.forget(&oldest);
    assert!(first.landed("app:reader").is_empty());
}
