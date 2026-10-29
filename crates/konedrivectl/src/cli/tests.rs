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

#[test]
fn store_reset_is_an_offline_command_with_an_explicit_path() {
    let cli = Cli::try_parse_from(["konedrivectl", "store", "reset", "/state/fuse.sqlite", "--config", "/config/config.toml"]).unwrap();
    assert!(matches!(cli.command, Some(super::Cmd::Store { command: super::StoreCmd::Reset { path, config: Some(_) } }) if path == std::path::Path::new("/state/fuse.sqlite")));
    assert!(Cli::try_parse_from(["konedrivectl", "store", "reset"]).is_err());
}

#[test]
fn account_add_and_write_scope_parse() {
    for (args, read_only) in [(vec!["konedrivectl", "account", "add"], false), (vec!["konedrivectl", "account", "add", "--read-only"], true)] {
        assert!(matches!(Cli::try_parse_from(args).unwrap().command, Some(Cmd::Account { command: AccountCmd::Add { read_only: value } }) if value == read_only));
    }
    for tail in [vec!["show"], vec!["set", "Trial", "Documents/Trial"], vec!["clear"]] {
        let mut args = vec!["konedrivectl", "--account", "Trial account", "account", "write-scope"];
        args.extend(tail);
        assert!(matches!(Cli::try_parse_from(args).unwrap().command, Some(Cmd::Account { command: AccountCmd::WriteScope { .. } })));
    }
    assert!(Cli::try_parse_from(["konedrivectl", "account", "write-scope", "set"]).is_err());
}
