use super::*;
#[tokio::test]
async fn change_guard_settings_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let store = ConfigStore::open(&paths, async { false }).await;
    let account = store.add_account("test").unwrap().id;
    store.set_changes_enabled(false).unwrap();
    store
        .set_changes_thresholds(
            [
                ("removals".into(), 7),
                ("rewrite-bytes".into(), 77),
                ("window-seconds".into(), 60),
                ("daily-window-seconds".into(), 3600),
            ]
            .into(),
        )
        .unwrap();
    assert!(store
        .set_changes_thresholds([("unknown".into(), 1)].into())
        .is_err());
    let loaded = ConfigStore::open(&paths, async { false }).await;
    let config = loaded.change_guard(&account).unwrap().config();
    assert_eq!(
        (config.window_seconds, config.daily_window_seconds),
        (60, 3600)
    );
    assert!(!config.enabled);
    assert_eq!(
        (config.removals, config.rewrite_bytes, config.moves),
        (7, 77, 200)
    );
}

#[tokio::test]
async fn change_guard_settings_are_shared_with_open_and_future_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let store = ConfigStore::open(&paths, async { false }).await;
    // Settings work before the first account is added.
    store
        .set_changes_thresholds(
            [("removal-bytes".into(), 77), ("daily-removals".into(), 19)].into(),
        )
        .unwrap();
    let a = store.add_account("a").unwrap().id;
    let first = store.change_guard(&a).unwrap();
    let b = store.add_account("b").unwrap().id;
    let second = store.change_guard(&b).unwrap();
    store.set_changes_enabled(false).unwrap();
    assert!(!first.config().enabled);
    assert!(!second.config().enabled);
    assert_eq!(first.config().removal_bytes, 77);
    assert_eq!(second.config().daily_removals, 19);
    assert!(store
        .set_changes_thresholds([("moves".into(), 9), ("invalid".into(), 2)].into())
        .is_err());
    assert_eq!(first.config().moves, 200);
    let third = store.add_account("c").unwrap().id;
    assert_eq!(store.change_guard(&third).unwrap().config(), first.config());
    let reopened = ConfigStore::open(&paths, async { false }).await;
    assert_eq!(reopened.change_guard(&a).unwrap().config(), first.config());
}

#[tokio::test]
async fn legacy_change_guard_settings_migrate_with_the_strictest_limits() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let store = ConfigStore::open(&paths, async { false }).await;
    store.add_account("a").unwrap();
    store.add_account("b").unwrap();
    let mut legacy = store.snapshot();
    legacy.accounts[0].change_guard = Some(GuardConfig {
        enabled: false,
        removals: 2,
        moves: 400,
        window_seconds: 1200,
        ..Default::default()
    });
    super::super::store::write_config(&paths.config_file, &legacy).unwrap();
    let reopened = ConfigStore::open(&paths, async { false }).await;
    let snapshot = reopened.snapshot();
    let config = snapshot.change_guard.unwrap();
    assert!(
        config.enabled,
        "the account with default settings remains protected"
    );
    assert_eq!(
        (config.removals, config.moves, config.window_seconds),
        (2, 200, 1200)
    );
    assert!(snapshot.accounts.iter().all(|a| a.change_guard.is_none()));
    let disk = std::fs::read_to_string(&paths.config_file).unwrap();
    assert!(!disk.contains("accounts.change_guard"));
    assert!(disk.contains("[change_guard]"));
    let reopened = ConfigStore::open(&paths, async { false }).await;
    assert_eq!(reopened.snapshot().change_guard.unwrap(), config);
}
