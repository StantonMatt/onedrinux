//! Download guard output uses the daemon's host classification and keeps explicit app rules.
mod common;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_rows_show_daemon_classification_including_configured_hosts() {
    let bus = konedrive_dbus::testing::TestBus::start();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "config_version = 2\n[download_guard_limits]\nprogram_hosts = ['org.example.Host']\n").unwrap();
    let daemon = common::start_daemon(&bus, dir.path()).await;
    assert_eq!(daemon.manager.config().guard().limits().program_hosts, ["org.example.Host"]);
    for id in ["app:org.example.Host", "app:code", "app:org.kde.gwenview"] {
        daemon.manager.config().set_guard_app(id, "block").unwrap();
    }
    let out = common::run(bus.address(), &["guard", "apps"]);
    assert!(out.status.success(), "{out:?}");
    let text = common::out_text(&out);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    for line in lines {
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(columns.len(), 8, "{line}");
        assert_eq!(&columns[2..7], &["blocked-always", "blocked", "0 files", "0 bytes", "user"]);
        assert_eq!(columns[7], if columns[0] == "app:org.example.Host" { "program-host" } else { "ordinary-app" });
    }
    let out = common::run(bus.address(), &["guard", "status"]);
    assert!(out.status.success(), "{out:?}");
    assert!(common::out_text(&out).contains("Limits: 30 files, 2147483648 bytes in 600 seconds"));
}
