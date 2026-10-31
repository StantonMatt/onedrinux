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

#[test]
fn t3_agents_konsole_children_and_flatpak_scopes_keep_their_identities() {
    // Slices are not app units, and the agents' final scope has no app- prefix.
    let t3 = caller("/opt/t3code/claude", "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-t3code.slice/app-t3code-agents.slice/app-t3code-agents-ab12.slice/t3code-claude-cd34.scope", "claude".into());
    assert_eq!(t3.identity, "exe:/opt/t3code/claude");
    let konsole = caller("/usr/bin/python3", "0::/user.slice/app-org.kde.konsole@abcdef.service", "python3".into());
    assert_eq!(konsole.identity, "app:org.kde.konsole");
    let flatpak = caller("/app/bin/code", "0::/user.slice/app-flatpak-com.visualstudio.code-12345.scope", "code".into());
    assert_eq!(flatpak.identity, "app:flatpak-com.visualstudio.code");
}

#[test]
fn host_matching_keeps_launch_prefixes_case_and_jetbrains_rules() {
    let hosts = super::super::GuardConfig::default().program_hosts;
    for id in ["org.kde.konsole", "org.kde.yakuake", "kitty", "Alacritty", "org.gnome.Ptyxis", "debian-xterm", "code", "flatpak-com.visualstudio.code", "com.t3tools.T3Code", "jetbrains-pycharm-ce", "org.kde.kate", "codex-shared-client", "t3code-url-handler", "t3-code-nightly", "org.kde.kdevelop", "com.mitchellh.ghostty", "org.wezfurlong.wezterm", "foot", "com.gexperts.Tilix", "terminator", "code-insiders", "gnome-org.gnome.Ptyxis", "com.jetbrains.CLion", "com.anthropic.Claude", "chatgpt", "t3code", "cursor", "zed"] {
        assert!(runs_programs(&format!("app:{id}"), &hosts), "{id}");
    }
    for id in ["app:org.kde.gwenview", "app:gnome-org.gnome.Nautilus", "app:org.kde.konsolekalendar", "exe:/usr/bin/konsole", "unidentified"] {
        assert!(!runs_programs(id, &hosts), "{id}");
    }
}

#[test]
fn process_generations_change_on_pid_reuse_exec_scope_role_or_failed_inspection() {
    struct Fake(Mutex<Option<ProcessInfo>>);
    impl ProcessReader for Fake {
        fn read(&self, _: u32) -> Option<ProcessInfo> { self.0.lock().unwrap().clone() }
    }
    let fake = Arc::new(Fake(Mutex::new(Some(ProcessInfo {
        start: 1, executable: "/usr/bin/viewer".into(), cgroup: "0::/app-org.example.viewer-123.scope".into(),
        cmdline: Arc::new(b"viewer\0/mount/file\0".to_vec()),
    }))));
    let callers = Callers::with_reader(fake.clone());
    let mut last = callers.process(42).unwrap().generation;
    assert_eq!(callers.process(42).unwrap().generation, last);
    for change in 0..4 {
        let mut data = fake.0.lock().unwrap();
        let p = data.as_mut().unwrap();
        match change {
            0 => p.start += 1,
            1 => p.executable = "/usr/lib/kioworker".into(),
            2 => p.cgroup = "0::/app-org.example.other-123.scope".into(),
            3 => p.cmdline = Arc::new(b"kioworker\0thumbnail\0".to_vec()),
            _ => unreachable!(),
        }
        drop(data);
        let next = callers.process(42).unwrap().generation;
        assert_ne!(next, last);
        last = next;
    }
    let saved = fake.0.lock().unwrap().take();
    assert!(callers.process(42).is_none());
    *fake.0.lock().unwrap() = saved;
    assert_ne!(callers.process(42).unwrap().generation, last);
}
