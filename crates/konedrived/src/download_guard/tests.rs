use super::*;

fn who(id: &str, reader: &str) -> Caller {
    Caller { identity: id.into(), display_name: reader.into(), reader: reader.into() }
}
fn limits() -> GuardConfig {
    GuardConfig { max_files: 2, max_bytes: 20, window_seconds: 10, ..GuardConfig::default() }
}

#[test]
fn distinct_files_and_the_byte_boundary_are_admitted_atomically() {
    let now = Instant::now();
    let mut window = Window::default();
    assert!(window.admit("a", 5, now, &limits()));
    assert!(window.admit("a", 5, now, &limits()));
    assert!(window.admit("b", 10, now, &limits()));
    assert!(!window.admit("a", 1, now, &limits()));
    assert!(!window.admit("c", 0, now, &limits()));
    assert_eq!(window.files.len(), 2);
    assert_eq!(window.total, 20);
}
#[test]
fn files_and_bytes_expire_in_a_sliding_window() {
    let now = Instant::now();
    let mut window = Window::default();
    assert!(window.admit("a", 10, now, &limits()));
    assert!(window.admit("b", 10, now + Duration::from_secs(6), &limits()));
    assert!(!window.admit("c", 1, now + Duration::from_secs(9), &limits()));
    assert!(window.admit("c", 10, now + Duration::from_secs(10), &limits()));
    assert!(!window.files.contains_key("a"));
    assert_eq!(window.total, 20);
}
#[test]
fn repeated_reads_refresh_distinct_file_expiry() {
    let now = Instant::now();
    let mut window = Window::default();
    assert!(window.admit("a", 1, now, &limits()));
    assert!(window.admit("a", 1, now + Duration::from_secs(9), &limits()));
    // Two callers can capture their timestamps in one order and acquire the lock in
    // the other. The older request must not shorten the newer request's window.
    assert!(window.admit("a", 1, now + Duration::from_secs(8), &limits()));
    window.expire(now + Duration::from_secs(18), Duration::from_secs(10));
    assert_eq!(window.files.len(), 1);
    assert_eq!(window.total, 2);
}
#[test]
fn open_does_not_charge_and_blocks_are_sticky_and_reported_once() {
    let guard = DownloadGuard::new(true, limits());
    let mut news = guard.subscribe();
    let app = who("exe:/usr/bin/rg", "rg");
    let now = Instant::now();
    guard.decide(app.clone(), None, now).unwrap();
    assert_eq!(guard.apps()[0].files, 0);
    guard.decide(app.clone(), Some(("a", 10)), now).unwrap();
    guard.decide(app.clone(), Some(("b", 10)), now).unwrap();
    assert_eq!(guard.decide(app.clone(), Some(("c", 1)), now), Err(fuser::Errno::EACCES));
    let block = news.try_recv().unwrap();
    assert_eq!((block.files, block.bytes, block.reason.as_str()), (2, 20, "bulk"));
    assert_eq!(guard.decide(app.clone(), None, now + Duration::from_secs(100)), Err(fuser::Errno::EACCES));
    assert!(news.try_recv().is_err());
    guard.allow_session(&app.identity);
    guard.decide(app, Some(("c", 100)), now).unwrap();
}
#[test]
fn apps_share_no_budget_and_background_is_never_allowed() {
    let guard = DownloadGuard::new(true, limits());
    let now = Instant::now();
    guard.decide(who("app:terminal", "rg"), Some(("a", 20)), now).unwrap();
    guard.decide(who("app:editor", "editor"), Some(("b", 20)), now).unwrap();
    guard.allow_session("app:dolphin");
    assert_eq!(guard.decide(who("app:dolphin", "kio-thumbnail"), Some(("c", 1)), now), Err(fuser::Errno::EACCES));
    // file.so is eligible even when the thumbnail worker shares the launcher unit.
    guard.decide(who("app:dolphin", "kioworker"), Some(("c", 1)), now).unwrap();
}
#[test]
fn background_list_can_be_replaced_and_master_off_bypasses_inspection() {
    let guard = DownloadGuard::new(true, GuardConfig { background_readers: vec!["custom-extractor".into()], ..limits() });
    let now = Instant::now();
    guard.decide(who("exe:/bin/dolphin", "dolphin"), Some(("a", 1)), now).unwrap();
    assert_eq!(guard.decide(who("exe:/bin/custom-extractor", "custom-extractor"), None, now), Err(fuser::Errno::EACCES));
    guard.configure(false, limits());
    assert!(guard.admit(u32::MAX, "unknown", u64::MAX).is_ok());
    assert!(guard.check_open(u32::MAX).is_ok());
}
#[tokio::test]
async fn persistent_rules_and_thresholds_survive_restart_session_rules_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let config = crate::config::ConfigStore::open(&paths, async { false }).await;
    config.set_download_guard(false).unwrap();
    config.set_guard_thresholds(7, 1234, 42).unwrap();
    config.set_guard_app("exe:/bin/rg", "always").unwrap();
    config.set_guard_app("app:backup", "block").unwrap();
    config.set_guard_app("app:editor", "session").unwrap();
    assert!(config.set_guard_thresholds(0, 1, 1).is_err());
    assert!(config.set_guard_app("", "always").is_err());
    assert!(config.set_guard_app("app:x", "invalid").is_err());
    let reloaded = crate::config::ConfigStore::open(&paths, async { false }).await;
    assert!(!reloaded.guard().enabled());
    assert_eq!(reloaded.guard().limits().max_files, 7);
    assert_eq!(reloaded.guard().limits().max_bytes, 1234);
    assert_eq!(reloaded.guard().limits().window_seconds, 42);
    let apps = reloaded.guard().apps();
    assert_eq!(apps.len(), 2);
    assert!(apps.iter().any(|app| app.identity == "exe:/bin/rg" && app.policy == "allowed-always"));
    assert!(apps.iter().any(|app| app.identity == "app:backup" && app.blocked));
    config.set_guard_app("app:backup", "session").unwrap();
    assert!(config.guard().limits().blocked_apps.contains(&"app:backup".into()), "temporary approval keeps the always rule");
    assert_eq!(config.guard().apps().iter().find(|app| app.identity == "app:backup").unwrap().policy, "allowed-session");
    let restarted = crate::config::ConfigStore::open(&paths, async { false }).await;
    assert!(restarted.guard().apps().iter().any(|app| app.identity == "app:backup" && app.blocked));
    config.set_guard_app("exe:/bin/rg", "reset").unwrap();
    assert!(!config.guard().apps().iter().any(|app| app.identity == "exe:/bin/rg"));
}
#[tokio::test]
async fn a_failed_persistence_changes_no_policy() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let config = crate::config::ConfigStore::open(&paths, async { false }).await;
    std::fs::write(&paths.config_file, "unreadable = [").unwrap();
    assert!(config.set_guard_app("exe:/bin/rg", "always").is_err());
    assert!(config.guard().apps().is_empty());
    assert!(config.guard().enabled());
    config.set_guard_app("exe:/bin/rg", "session").unwrap();
    assert_eq!(config.guard().apps()[0].policy, "allowed-session");
}

#[test]
fn retry_bytes_beyond_the_reservation_block_future_requests_but_not_the_transfer() {
    let guard = Arc::new(DownloadGuard::new(true, limits()));
    let app = who("exe:/bin/reader", "reader");
    guard.decide(app.clone(), Some(("a", 15)), Instant::now()).unwrap();
    let charge = DownloadCharge { guard: Arc::clone(&guard), identity: app.identity.clone(), included: 15, received: AtomicU64::new(0) };
    charge.received(15); // already included, not double counted
    assert_eq!(guard.apps()[0].bytes, 15);
    charge.received(10); // retry traffic
    assert_eq!(guard.apps()[0].bytes, 25);
    assert!(guard.apps()[0].blocked);
    charge.received(2); // already-started transfer can finish
    assert_eq!(guard.apps()[0].bytes, 27);
    assert_eq!(guard.decide(app, None, Instant::now()), Err(fuser::Errno::EACCES));
}

#[test]
fn a_session_approval_temporarily_overrides_an_always_block() {
    let guard = DownloadGuard::new(true, GuardConfig { blocked_apps: vec!["app:editor".into()], ..limits() });
    let app = who("app:editor", "editor");
    assert_eq!(guard.decide(app.clone(), None, Instant::now()), Err(fuser::Errno::EACCES));
    guard.allow_session(&app.identity);
    guard.decide(app.clone(), Some(("a", 100)), Instant::now()).unwrap();
    assert_eq!(guard.apps()[0].policy, "allowed-session");
    guard.reset(&app.identity);
    assert_eq!(guard.decide(app, None, Instant::now()), Err(fuser::Errno::EACCES));
}
