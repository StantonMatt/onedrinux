use super::*;
use crate::download_guard::GuardConfig;

#[test]
fn read_ahead_never_runs_for_hosts_background_or_unidentified_even_when_allowed() {
    let mut limits = GuardConfig::default();
    for (identity, reader) in [
        ("exe:/usr/bin/cat", "cat"),
        ("app:org.kde.konsole", "cat"),
        ("app:com.visualstudio.code", "code"),
        ("app:org.kde.gwenview", "kio-thumbnail"),
        ("app:org.kde.gwenview", "unidentified"),
        ("unidentified", "unidentified"),
    ] {
        limits.allowed_apps.push(identity.into());
        let guard = DownloadGuard::new(true, limits.clone());
        guard.allow_session(identity);
        assert!(
            !guard.permits_read_ahead(&ReadAheadCaller::testing(identity, reader)),
            "{identity}, {reader}"
        );
    }
}

#[test]
fn read_ahead_stops_on_policy_changes_without_charging_the_app() {
    let guard = DownloadGuard::default();
    let app = ReadAheadCaller::testing("app:org.kde.gwenview", "kioworker");
    assert!(guard.permits_read_ahead(&app));
    assert!(guard.apps().is_empty());
    let mut changes = guard.read_ahead_changes();
    let mut limits = guard.limits();
    limits.blocked_apps.push(app.identity().into());
    guard.configure(true, limits);
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    assert!(!guard.permits_read_ahead(&app));
    guard.allow_session(app.identity());
    assert!(
        !guard.permits_read_ahead(&app),
        "speculation stops even when an explicit real read has a session allowance"
    );
    assert!(guard
        .apps()
        .iter()
        .all(|app| app.files == 0 && app.bytes == 0));
}
