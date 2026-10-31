use super::*;
use database::{now, Database};
use std::os::unix::fs::PermissionsExt;

fn record(from: u64, size: usize) -> Record {
    Record {
        from,
        bytes: vec![(from / MAX_RANGE) as u8; size],
        facts: "original facts".into(),
    }
}

#[test]
fn header_cache_keeps_ranges_across_restart_and_preserves_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account/headers.sqlite");
    let at = now();
    let mut db = Database::open(&path, 1024 * 1024).unwrap();
    db.keep("item", "v1", &record(1024, 4096), at).unwrap();
    assert!(
        db.find("item", "v1", 0, 4096, at).unwrap().is_none(),
        "partial coverage misses"
    );
    drop(db);
    let mut db = Database::open(&path, 1024 * 1024).unwrap();
    let got = db.find("item", "v1", 1033, 1040, at + 1).unwrap().unwrap();
    assert_eq!(
        (got.from, got.bytes.len(), got.facts.as_str()),
        (1024, 4096, "original facts")
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        db.conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn header_cache_bounds_ranges_items_and_evicts_lru_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let mut db = Database::open(&path, 2 * 1024 * 1024).unwrap();
    let at = now();
    db.keep("large", "v", &record(0, MAX_RANGE as usize + 1), at)
        .unwrap();
    assert_eq!(db.stats().unwrap().1, 0);
    for n in 0..5 {
        db.keep(
            "item",
            "v",
            &record(n * MAX_RANGE, MAX_RANGE as usize),
            at + n as i64,
        )
        .unwrap();
    }
    assert_eq!(
        db.stats().unwrap().1,
        4,
        "first four ranges use the per-item budget"
    );
    assert!(db
        .find("item", "v", 4 * MAX_RANGE, 4 * MAX_RANGE + 1, at + 9)
        .unwrap()
        .is_none());
    db.keep("item", "v", &record(0, 33), at + 10).unwrap();
    assert_eq!(
        db.stats().unwrap().1,
        4,
        "a repeated smaller read never shrinks its row"
    );
    assert!(db
        .find("item", "v", 0, MAX_RANGE, at + 11)
        .unwrap()
        .is_some());

    let lru_path = dir.path().join("lru.sqlite");
    let limit = 180 * 1024;
    let mut lru = Database::open(&lru_path, limit).unwrap();
    lru.keep("first", "v", &record(0, MAX_RANGE as usize), at)
        .unwrap();
    lru.keep("second", "v", &record(0, MAX_RANGE as usize), at + 1)
        .unwrap();
    assert_eq!(lru.stats().unwrap().1, 2);
    assert!(lru.find("first", "v", 0, 4, at + 2).unwrap().is_some());
    lru.keep("third", "v", &record(0, MAX_RANGE as usize), at + 3)
        .unwrap();
    assert!(
        lru.find("second", "v", 0, 4, at + 4).unwrap().is_none(),
        "the least recently used range is evicted"
    );
    assert!(lru.find("first", "v", 0, 4, at + 4).unwrap().is_some());
    assert!(lru.find("third", "v", 0, 4, at + 4).unwrap().is_some());
    assert!(
        std::fs::metadata(&lru_path).unwrap().len() <= limit,
        "budget includes SQLite pages, not only payload"
    );
}

#[test]
fn header_cache_expires_unused_rows_and_removes_other_versions() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Database::open(&dir.path().join("headers.sqlite"), 1024 * 1024).unwrap();
    let at = now();
    db.keep("item", "old", &record(0, 1024), at).unwrap();
    assert!(db.find("item", "new", 0, 1, at + 1).unwrap().is_none());
    assert_eq!(
        db.stats().unwrap().1,
        0,
        "a changed content drops old versions, even on a miss"
    );
    db.keep("item", "new", &record(0, 1024), at).unwrap();
    db.keep("idle", "v", &record(0, 1024), at).unwrap();
    assert!(db
        .find("item", "new", 0, 4, at + LIFETIME - 1)
        .unwrap()
        .is_some());
    assert!(db.find("idle", "v", 0, 4, at + LIFETIME).unwrap().is_none());
    assert!(
        db.find("item", "new", 0, 4, at + LIFETIME)
            .unwrap()
            .is_some(),
        "hits refresh last-used"
    );
    db.forget("item").unwrap();
    assert_eq!(db.stats().unwrap().1, 0);
}

#[tokio::test]
async fn header_cache_disabled_corrupt_and_new_schema_files_are_disposable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    std::fs::write(&path, "this is not SQLite").unwrap();
    {
        let cache = HeaderCache::open(path.clone(), 1024 * 1024);
        cache.keep(0, "item", "v".into(), record(0, 1024));
        cache.flush().await;
        assert!(
            cache.find(0, "item", "v".into(), 0, 1024).await.is_some(),
            "recreated after corruption"
        );
    }
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 999).unwrap();
    }
    {
        let cache = HeaderCache::open(path.clone(), 1024 * 1024);
        cache.flush().await;
        assert!(
            cache.find(0, "item", "v".into(), 0, 1024).await.is_none(),
            "schema rebuilt, old rows gone"
        );
        cache.keep(0, "item", "v".into(), record(0, 1024));
        cache.flush().await;
        assert!(cache.find(0, "item", "v".into(), 0, 1024).await.is_some());
    }
    let disabled = HeaderCache::open(path.clone(), 0);
    disabled.flush().await;
    assert!(!path.exists());
    disabled.keep(0, "item", "v".into(), record(0, 1024));
    disabled.flush().await;
    assert!(disabled
        .find(0, "item", "v".into(), 0, 1024)
        .await
        .is_none());
    assert!(
        !path.exists(),
        "disabled writes never recreate the database"
    );
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, "a file, not a directory").unwrap();
    let impossible = HeaderCache::open(blocked.join("headers.sqlite"), 1024 * 1024);
    impossible.keep(0, "item", "v".into(), record(0, 1024));
    impossible.flush().await;
    assert!(
        impossible
            .find(0, "item", "v".into(), 0, 1024)
            .await
            .is_none(),
        "unavailable cache never breaks reads"
    );
}

#[tokio::test]
async fn header_cache_invalidation_fences_queued_writes_and_deletes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let cache = HeaderCache::open(path.clone(), 1024 * 1024);
    cache.keep(0, "item", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(path.exists());
    cache.invalidate();
    cache.keep(0, "item", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(
        !path.exists(),
        "old queued publication cannot recreate a cleared database"
    );
    assert!(cache.find(0, "item", "v".into(), 0, 1024).await.is_none());
    cache.keep(cache.epoch(), "item", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(cache
        .find(cache.epoch(), "item", "v".into(), 0, 1024)
        .await
        .is_some());
    let old = cache.epoch();
    cache.forget("item");
    cache.keep(old, "item", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(cache
        .find(cache.epoch(), "item", "v".into(), 0, 1024)
        .await
        .is_none());
    cache.clear().await.unwrap();
    assert!(!path.exists(), "sign-out waits for physical deletion");
}

#[tokio::test]
async fn sign_out_and_account_retirement_delete_headers_without_a_mounted_folder() {
    for retire in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let base = url::Url::parse("http://127.0.0.1:1/").unwrap();
        let service = crate::account::testing::single_account(
            dir.path(),
            konedrive_graph::oauth::Endpoints {
                authority: base.clone(),
                graph: base,
            },
            Arc::new(konedrive_graph::secret::MemoryStore::default()),
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();
        let cache = service.config().header_cache(service.id()).unwrap();
        let paths = super::super::Paths::in_dir(dir.path())
            .account(service.id())
            .unwrap();
        cache.keep(cache.epoch(), "item", "v".into(), record(0, 4096));
        cache.flush().await;
        assert!(paths.headers.exists());
        if retire {
            service.retire().await.unwrap();
            service.config().remove_account(service.id()).unwrap();
        } else {
            service.sign_out().await.unwrap();
        }
        cache.flush().await;
        assert!(
            !paths.headers.exists(),
            "cache file deleted before sign-out/removal completes"
        );
    }
}

#[tokio::test]
async fn header_cache_failed_deletion_blocks_reuse_and_retries_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let blocked = dir.path().join("headers.sqlite-shm");
    let cache = HeaderCache::open(path.clone(), 1024 * 1024);
    cache.keep(0, "private", "v".into(), record(0, 1024));
    cache.flush().await;
    let saved = std::fs::read(&path).unwrap();
    // A directory cannot be unlinked as a SQLite sidecar, even when tests run as root.
    std::fs::create_dir(&blocked).unwrap();
    assert!(
        cache.clear().await.is_err(),
        "cleanup failure reaches the caller"
    );
    cache.keep(cache.epoch(), "new", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(
        !path.exists(),
        "failed cleanup must not reopen SQLite for a keep"
    );
    assert!(cache
        .find(cache.epoch(), "private", "v".into(), 0, 1024)
        .await
        .is_none());
    assert!(
        !path.exists(),
        "failed cleanup must not reopen SQLite for a lookup"
    );
    drop(cache);

    // Simulate a surviving database, as when unlink of the main file itself failed.
    // The pending deletion must be honored before SQLite is opened on restart.
    std::fs::write(&path, saved).unwrap();
    let cache = HeaderCache::open(path.clone(), 1024 * 1024);
    cache.flush().await;
    assert!(cache
        .find(cache.epoch(), "private", "v".into(), 0, 1024)
        .await
        .is_none());
    cache.keep(cache.epoch(), "new", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(
        !path.exists(),
        "restart keeps deletion pending while a sidecar is blocked"
    );
    std::fs::remove_dir(&blocked).unwrap();
    assert!(cache
        .find(cache.epoch(), "private", "v".into(), 0, 1024)
        .await
        .is_none());
    cache.keep(cache.epoch(), "new", "v".into(), record(0, 1024));
    cache.flush().await;
    assert!(cache
        .find(cache.epoch(), "new", "v".into(), 0, 1024)
        .await
        .is_some());
    cache.clear().await.unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn header_cache_failed_unlink_is_remembered_without_directory_write_permission() {
    // Root bypasses directory permissions; the sidecar-blocker regression above
    // exercises failed deletion and restart independently of the test user's UID.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    for awaited in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.sqlite");
        let cache = HeaderCache::open(path.clone(), 1024 * 1024);
        cache.keep(0, "private", "v".into(), record(0, 1024));
        cache.flush().await;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = if awaited {
            cache.clear().await
        } else {
            cache.invalidate();
            cache.flush().await;
            Ok(())
        };
        drop(cache);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if awaited {
            assert!(result.is_err());
        }
        assert!(
            path.exists(),
            "unlink really failed, leaving the private rows on disk"
        );
        let cache = HeaderCache::open(path, 1024 * 1024);
        assert!(
            cache
                .find(0, "private", "v".into(), 0, 1024)
                .await
                .is_none(),
            "startup deletes surviving invalidated rows before opening SQLite"
        );
        cache.keep(0, "fresh", "v".into(), record(0, 1024));
        cache.flush().await;
        assert!(cache.find(0, "fresh", "v".into(), 0, 1024).await.is_some());
    }
}

#[tokio::test]
async fn account_header_cleanup_failures_are_reported_after_sign_out() {
    for retire in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let base = url::Url::parse("http://127.0.0.1:1/").unwrap();
        let secrets = Arc::new(konedrive_graph::secret::MemoryStore::with_token("RT"));
        let service = crate::account::testing::single_account(
            dir.path(),
            konedrive_graph::oauth::Endpoints {
                authority: base.clone(),
                graph: base,
            },
            secrets.clone(),
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();
        let cache = service.config().header_cache(service.id()).unwrap();
        let path = super::super::Paths::in_dir(dir.path())
            .account(service.id())
            .unwrap()
            .headers;
        cache.keep(cache.epoch(), "private", "v".into(), record(0, 4096));
        cache.flush().await;
        let mut blocked = path.as_os_str().to_owned();
        blocked.push("-shm");
        std::fs::create_dir(&blocked).unwrap();
        let result = if retire {
            service.retire().await
        } else {
            service.sign_out().await
        };
        assert!(
            matches!(result, Err(crate::account::AccountError::Failed(detail)) if detail.contains("header cache"))
        );
        assert_eq!(
            service.state().get().state,
            crate::account::state::SignInState::SignedOut
        );
        assert!(secrets.current().is_none());
        cache.keep(cache.epoch(), "new", "v".into(), record(0, 4096));
        cache.flush().await;
        assert!(!path.exists());
        std::fs::remove_dir(&blocked).unwrap();
        service.sign_out().await.unwrap();
        assert!(!path.exists(), "retry completes cleanup");
    }
}

#[tokio::test]
async fn header_cache_publication_never_waits_for_a_paused_writer_and_has_a_bounded_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let cache = HeaderCache::open(path.clone(), 1024 * 1024);
    let (entered, inside) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    cache.send(Command::Pause {
        entered,
        release: wait,
    });
    inside
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    // SQLite cannot run at all until after every publication has returned. Sending
    // beyond the bound must return too, rather than retaining unbounded content.
    for n in 0..100 {
        cache.keep(
            0,
            &format!("item-{n}"),
            "v".into(),
            record(0, MAX_RANGE as usize),
        );
    }
    assert_eq!(cache.writes.available_permits(), 0);
    cache.invalidate();
    release.send(()).unwrap();
    cache.flush().await;
    assert_eq!(cache.writes.available_permits(), 64);
    assert!(
        !path.exists(),
        "all queued pre-invalidation writes were fenced"
    );
}

#[tokio::test]
async fn header_cache_lock_contention_never_deletes_a_good_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let mut db = Database::open(&path, 1024 * 1024).unwrap();
    db.keep("item", "v", &record(0, 4096), now()).unwrap();
    db.conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let cache = HeaderCache::open(path.clone(), 1024 * 1024);
    cache.flush().await;
    assert!(cache.find(0, "item", "v".into(), 0, 4096).await.is_none());
    db.conn.execute_batch("COMMIT").unwrap();
    assert!(db.find("item", "v", 0, 4096, now()).unwrap().is_some());
    drop(cache);
    drop(db);
    let mut db = Database::open(&path, 1024 * 1024).unwrap();
    assert!(
        db.find("item", "v", 0, 4096, now()).unwrap().is_some(),
        "contention did not unlink or rebuild the file"
    );
}

#[tokio::test]
async fn header_cache_lookup_falls_through_while_writes_are_queued() {
    let dir = tempfile::tempdir().unwrap();
    let cache = HeaderCache::open(dir.path().join("headers.sqlite"), 8 * 1024 * 1024);
    cache.keep(0, "head", "v".into(), record(0, 4096));
    cache.flush().await;
    let (entered, inside) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    cache.send(Command::Pause {
        entered,
        release: wait,
    });
    inside
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    for n in 0..64 {
        cache.keep(
            0,
            &format!("item-{n}"),
            "v".into(),
            record(0, MAX_RANGE as usize),
        );
    }
    assert_eq!(cache.writes.available_permits(), 0);
    let began = std::time::Instant::now();
    let found = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        cache.find(0, "head", "v".into(), 0, 4096),
    )
    .await;
    let elapsed = began.elapsed();
    // Release before asserting, so even a failing test can join the worker.
    release.send(()).unwrap();
    cache.flush().await;
    eprintln!("header lookup behind 64 paused writes: {elapsed:?}");
    assert!(
        matches!(found, Ok(None)),
        "busy disk must become a prompt miss"
    );
    let began = std::time::Instant::now();
    let idle = cache.find(0, "head", "v".into(), 0, 4096).await;
    eprintln!("header lookup after draining writes: {:?}", began.elapsed());
    assert!(
        idle.is_some(),
        "timing out must not disable or delete the cache"
    );
}

#[tokio::test]
async fn header_cache_delayed_lookup_maintenance_preserves_newer_keeps_and_forgets() {
    let dir = tempfile::tempdir().unwrap();
    let cache = HeaderCache::open(dir.path().join("headers.sqlite"), 1024 * 1024);
    cache.keep(0, "item", "old".into(), record(0, 4096));
    cache.flush().await;
    assert!(cache.find(0, "item", "old".into(), 0, 4096).await.is_some());
    // The old lookup's delayed version pruning must precede the new insertion.
    cache.keep(0, "item", "new".into(), record(0, 4096));
    cache.flush().await;
    assert!(cache.find(0, "item", "new".into(), 0, 4096).await.is_some());
    // A pending LRU touch cannot restore a row after content invalidation.
    cache.forget("item");
    cache.flush().await;
    assert!(cache
        .find(cache.epoch(), "item", "new".into(), 0, 4096)
        .await
        .is_none());
}
