use super::*;
#[test]
fn guard_commands_parse_identities_and_exact_byte_thresholds() {
    for args in [
        vec!["konedrivectl", "guard", "status"],
        vec!["konedrivectl", "guard", "apps"],
        vec![
            "konedrivectl",
            "guard",
            "allow",
            "app:org.kde.okular",
            "--always",
        ],
        vec!["konedrivectl", "guard", "block", "exe:/usr/bin/rg"],
        vec!["konedrivectl", "guard", "reset", "exe:/usr/bin/rg"],
        vec!["konedrivectl", "guard", "free", "exe:/usr/bin/rg"],
        vec!["konedrivectl", "guard", "enable", "off"],
        vec![
            "konedrivectl",
            "guard",
            "thresholds",
            "--files",
            "30",
            "--bytes",
            "2147483648",
            "--seconds",
            "600",
        ],
    ] {
        assert!(matches!(
            Cli::try_parse_from(args).unwrap().command,
            Some(Cmd::Guard { .. })
        ));
    }
}
