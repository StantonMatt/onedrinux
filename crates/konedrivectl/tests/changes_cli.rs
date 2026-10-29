//! Shared guard settings reject account selection before contacting any bus.
mod common;
#[test]
fn account_selection_is_refused_for_shared_change_guard_settings() {
    for tail in [
        vec!["enable"],
        vec!["disable"],
        vec!["thresholds", "removals=0"],
    ] {
        let mut args = vec!["--account", "Trial", "changes"];
        args.extend(tail);
        let out = common::run("unix:path=/no-test-bus", &args);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(
            common::err_text(&out).contains("leave out --account"),
            "{out:?}"
        );
    }
}
