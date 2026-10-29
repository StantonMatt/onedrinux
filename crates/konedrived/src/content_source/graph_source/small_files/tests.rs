use super::*;

fn file() -> WholeFile {
    WholeFile { size: Some(3), mtime: SystemTime::UNIX_EPOCH, version: None, bytes: b"abc".to_vec() }
}

#[test]
fn small_files_prune_expired_bytes_on_find_and_keep() {
    let cache = SmallFiles::default();
    let content = ContentVersion::Hashed { size: 3, mtime: 0, mtime_nsec: 0, hash: None, ctag: Some("c".into()) };
    cache.keep("expired", &content, file());
    crate::panic::lock(&cache.held)[0].since = Instant::now() - HELD_FOR;
    assert!(cache.find("missing", &content).is_none());
    assert!(crate::panic::lock(&cache.held).is_empty(), "find frees all expired files, even on a miss");
    cache.keep("expired", &content, file());
    crate::panic::lock(&cache.held)[0].since = Instant::now() - HELD_FOR;
    cache.keep("fresh", &content, file());
    assert_eq!(crate::panic::lock(&cache.held).len(), 1, "keep frees expired files too");
    assert_eq!(cache.find("fresh", &content).unwrap().bytes, b"abc");
}
