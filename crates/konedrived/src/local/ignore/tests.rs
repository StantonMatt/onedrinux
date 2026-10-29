use super::*;

fn ignored(name: &str) -> bool {
    IgnoreList::default().matches(OsStr::new(name))
}

#[test]
fn the_defaults_catch_temporary_files_and_build_folders() {
    for name in [
        "node_modules", ".venv", "venv", "__pycache__", ".gradle",
        ".report.txt.swp",
        "notes.txt~",
        ".~lock.budget.ods#",
        "~$budget.xlsx",
        "#draft.org#",
        ".#draft.org",
        ".goutputstream-ABC123",
        "file.kate-swp",
        "movie.mkv.part",
        "setup.exe.crdownload",
        "lu12345abc.tmp",
        ".fuse_hidden0001",
        ".nfs000123",
    ] {
        assert!(ignored(name), "{name}");
    }
    for name in ["report.txt", "swp", "a.swp.txt", "~notes", "lock.budget.ods#", "draft#", "tmp", "target", "Node_Modules", "node_modules.old", "myvenv"] {
        assert!(!ignored(name), "{name}");
    }
}

#[test]
fn globs_have_classes_escapes_and_are_case_sensitive() {
    let list = IgnoreList::new(["[a-c]?.log", "[!x]z", "\\*star", "*.TMP"]);
    assert!(list.matches(OsStr::new("b1.log")));
    assert!(!list.matches(OsStr::new("d1.log")));
    assert!(list.matches(OsStr::new("yz")) && !list.matches(OsStr::new("xz")));
    assert!(list.matches(OsStr::new("*star")) && !list.matches(OsStr::new("astar")));
    assert!(list.matches(OsStr::new("A.TMP")) && !list.matches(OsStr::new("a.tmp")));
    assert!(IgnoreList::new(["[unclosed"]).matches(OsStr::new("[unclosed")));
    assert!(IgnoreList::new(["a*b*c"]).matches(OsStr::new("aXbYbZc")));
    assert!(!IgnoreList::new(Vec::<String>::new()).matches(OsStr::new("x.swp")));
}

#[test]
fn an_existing_accounts_unset_ignore_list_uses_the_current_defaults() {
    let defaults = IgnoreList::configured(None);
    for name in ["node_modules", ".venv", "venv", "__pycache__", ".gradle"] {
        assert!(defaults.matches(OsStr::new(name)));
        assert!(!IgnoreList::configured(Some(&[])).matches(OsStr::new(name)), "an explicit list stays the user's choice");
    }
}
