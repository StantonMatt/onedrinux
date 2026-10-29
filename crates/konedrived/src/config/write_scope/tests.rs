use super::*;
use crate::config::{AccountConfig, Config, Mode, Origin};
use std::path::Path;

#[test]
fn write_scope_checks_components_and_fails_closed() {
    let scope = WriteScope::parse(Some(&["Trial".into(), "Docs/Trial".into()])).unwrap();
    for path in ["Trial", "Trial/a", "Trial/sub/a", "Docs/Trial/file"] {
        assert!(scope.allows(Path::new(path)), "{path}");
    }
    for path in ["", "Trials", "trial/a", "Docs", "Docs/Trial2/a", "Trial/../secret", "/Trial/a"] {
        assert!(!scope.allows(Path::new(path)), "{path}");
    }
    assert!(WriteScope::default().allows(Path::new("anything")));
    assert!(!WriteScope::denied().allows(Path::new("anything")));
    for path in [
        "",
        ".",
        "..",
        "../Trial",
        "Trial/..",
        "Trial/./a",
        "/Trial",
        "Trial/",
        "Trial//a",
        "Trial\\secret",
        "x\0y",
    ] {
        assert!(WriteScope::parse(Some(&[path.into()])).is_err(), "{path:?}");
    }
}

#[test]
fn write_scope_config_round_trips_and_keeps_existing_modes() {
    let old = "config_version = 2\n[[accounts]]\nid = 'aaaaaaaaaaaa'\nlabel = 'Old'\n";
    let mut config: Config = toml::from_str(old).unwrap();
    assert_eq!(config.new_account_mode, Mode::ReadWrite);
    assert_eq!(config.accounts[0].mode, Mode::ReadOnly);
    assert_eq!(config.accounts[0].write_paths, None);
    config.accounts[0].write_paths = Some(vec!["Trial".into(), "Docs/Trial".into()]);
    let mut new = AccountConfig::new(AccountId::new("bbbbbbbbbbbb"), "New", Origin::Added);
    new.mode = Mode::ReadWrite;
    new.write_paths = Some(vec![]);
    config.accounts.push(new);
    let text = toml::to_string(&config).unwrap();
    assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    let denied = toml::from_str::<Config>(&text.replace("Trial", "../Trial")).unwrap();
    assert!(WriteScope::parse(denied.accounts[0].write_paths.as_deref()).is_err());
}

#[tokio::test]
async fn write_scope_source_follows_persisted_changes_and_missing_accounts_deny() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let store = Arc::new(ConfigStore::open(&paths, async { false }).await);
    let added = store.add_account("Test").unwrap();
    assert_eq!(added.mode, Mode::ReadWrite);
    let source = store.write_scope(&added.id);
    assert!(source.get().unrestricted());
    store
        .update_account(&added.id, |account| {
            account.write_paths = Some(vec!["Trial".into()]);
            Ok::<_, crate::config::ConfigError>(())
        })
        .unwrap();
    assert!(source.get().allows(Path::new("Trial/a")));
    assert!(!source.get().allows(Path::new("Outside/a")));
    store
        .update_account(&added.id, |account| {
            account.write_paths = Some(vec!["../bad".into()]);
            Ok::<_, crate::config::ConfigError>(())
        })
        .unwrap();
    assert!(!source.get().allows(Path::new("Trial/a")));
    store
        .update(|config| {
            config.new_account_mode = Mode::ReadOnly;
            Ok::<_, crate::config::ConfigError>(())
        })
        .unwrap();
    let other = store.add_account("Read only by default").unwrap();
    assert_eq!(other.mode, Mode::ReadOnly);
    assert_eq!(
        store.account(&added.id).unwrap().mode,
        Mode::ReadWrite,
        "the changed default leaves existing modes alone"
    );
    store.remove_account(&added.id).unwrap();
    assert!(!source.get().allows(Path::new("Trial/a")));
}

/// The sender reads the mode from `config.toml` at each admission: a switch to read-only
/// stops it at once, whatever the scope allows; a missing account is read-only.
#[tokio::test]
async fn a_read_only_account_is_read_only_at_the_sender_whatever_its_scope() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::Paths::in_dir(dir.path());
    let store = Arc::new(ConfigStore::open(&paths, async { false }).await);
    let added = store.add_account("Test").unwrap();
    let source = store.write_scope(&added.id);
    assert!(!source.is_read_only() && source.get().unrestricted());
    store
        .update_account(&added.id, |account| {
            account.mode = Mode::ReadOnly;
            Ok::<_, crate::config::ConfigError>(())
        })
        .unwrap();
    assert!(source.is_read_only());
    assert!(source.get().unrestricted(), "the scope alone would allow it");
    assert!(store.write_scope(&AccountId::new("cccccccccccc")).is_read_only());
    assert!(!WriteScopeSource::default().is_read_only());
}
