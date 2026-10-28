use super::*;
#[test]
fn units_are_stable_across_launches() {
    for unit in ["app-org.kde.okular@abcd.service", "app-org.kde.okular-123.scope"] {
        let who = caller("/usr/bin/okular", &format!("0::/user.slice/{unit}"), "okular".into());
        assert_eq!(who.identity, "app:org.kde.okular");
    }
    assert_eq!(app_unit("app-org.example.some\\x2dapp@2.service").as_deref(), Some("org.example.some-app"));
    assert_eq!(caller("/usr/bin/rg", "0::/user.slice/session-1.scope", "rg".into()).identity, "exe:/usr/bin/rg");
}
#[test]
fn kio_roles_are_distinct() {
    assert_eq!(reader_role("/usr/lib/kioworker", b"kioworker\0/usr/lib/qt6/plugins/kf6/kio/thumbnail.so\0local:/x\0"), "kio-thumbnail");
    assert_eq!(reader_role("/usr/lib/kioworker", b"kioworker\0thumbnail\0"), "kio-thumbnail");
    assert_eq!(reader_role("/usr/lib/kioworker", b"kioworker\0/usr/lib/qt6/plugins/kf6/kio/file.so\0"), "kioworker");
    assert_eq!(reader_role("/usr/bin/cat", b"cat\0thumbnail\0"), "cat");
}
#[test]
fn proc_stat_handles_unusual_comm() {
    let tail = (3..=22).map(|n| n.to_string()).collect::<Vec<_>>().join(" ");
    assert_eq!(start_time(&format!("12 (a ) b) {tail}")), Some(22));
}
#[test]
fn callers_are_revalidated_and_unknown_pids_are_refused() {
    let cache = Callers::default();
    assert!(cache.identify(std::process::id()).is_some());
    assert!(cache.identify(std::process::id()).is_some());
    assert!(cache.identify(u32::MAX).is_none());
}
#[test]
fn background_list_is_specific() {
    for name in ["dolphin", "kio-thumbnail", "baloo_file", "baloo_file_extractor", "tracker-extract-3", "tumblerd"] {
        assert!(BACKGROUND_READERS.contains(&name));
    }
    for name in ["kioworker", "okular", "cat", "rg"] {
        assert!(!BACKGROUND_READERS.contains(&name));
    }
}
