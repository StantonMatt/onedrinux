//! Offline reset must preserve a newer store without connecting to a session bus, and
//! must refuse both a daemon's configuration lock and an open store.
use std::path::Path;
use std::process::{Command, Output};

use konedrive_tree::TreeStore;

fn reset(store: &Path, config: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_konedrivectl"))
        .arg("store")
        .arg("reset")
        .arg(store)
        .arg("--config")
        .arg(config)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/no-such-test-bus")
        .output()
        .unwrap()
}

#[test]
fn offline_reset_preserves_a_newer_store_and_recovers_pins_without_a_bus() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fuse.sqlite");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let store = TreeStore::open(&path).unwrap();
    store.set_pin("KEPT", true).unwrap();
    store
        .bench_sql("UPDATE meta SET value='99' WHERE key='schema_version';")
        .unwrap();
    drop(store);
    let before = std::fs::read(&path).unwrap();
    let result = reset(&path, &config);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("Store preserved at"));
    assert!(!path.exists());
    let quarantine = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.file_name().unwrap().to_string_lossy().ends_with('Z'))
        .unwrap();
    assert_eq!(std::fs::read(quarantine).unwrap(), before);
    assert_eq!(TreeStore::open(&path).unwrap().pins().unwrap(), ["KEPT"]);
}

#[test]
fn offline_reset_refuses_a_running_daemon_or_an_open_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fuse.sqlite");
    let config = dir.path().join("config.toml");
    let lock = std::fs::File::create(dir.path().join("config.toml.lock")).unwrap();
    lock.try_lock().unwrap();
    drop(TreeStore::open(&path).unwrap());
    let before = std::fs::read(&path).unwrap();
    let result = reset(&path, &config);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("daemon is running"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(lock);
    let store = TreeStore::open(&path).unwrap();
    let result = reset(&path, &config);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("store is in use"));
    drop(store);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
