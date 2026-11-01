use super::*;
use crate::config::header_cache::HeaderCache;

const BUDGET: u64 = 256 * 1024 * 1024;

#[tokio::test]
async fn a_ready_prefetch_answers_without_waiting_for_a_paused_disk_lookup() {
    let (graph, _) = folder_of(1, LARGE).await;
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(HeaderCache::open(dir.path().join("headers.sqlite"), BUDGET));
    let source = cached_source(&graph, &cache);
    let (content, listed) = opened_now(&graph, "P0");
    source
        .read_range("P0", &content, Some(&listed), 0, 4096)
        .await
        .unwrap();
    cache.flush().await;
    let piece = source
        .read_blocks
        .find_piece("P0", &content, 0, LARGE as u64)
        .unwrap();
    source.read_blocks.clear();
    requests_since(&graph);
    let release = cache.pause_for_test();
    // A deterministic ready shared fetch publishes bytes when polled. The disk
    // worker cannot answer first, regardless of timer scheduling or machine load.
    let publish = source.clone();
    let opened = content.clone();
    let prepared = Arc::new(Prepared {
        read: async move {
            publish.read_blocks.keep("P0", &opened, &piece);
            Ok(true)
        }
        .boxed()
        .shared(),
    });
    crate::panic::lock(&source.pending).insert(
        GraphSource::prepared_key("P0", &content, 0, 4096),
        Arc::downgrade(&prepared),
    );
    let answer = source
        .prepared_range("P0", &content, Some(&listed), 0, 4096)
        .now_or_never();
    release.send(()).unwrap();
    assert_eq!(
        read_whole(answer.expect("ready in the first poll").unwrap()).await,
        bytes_of(0, LARGE)[..4096]
    );
    assert!(requests_since(&graph).is_empty());
}

fn cached_source(graph: &FakeGraph, cache: &Arc<HeaderCache>) -> GraphSource {
    GraphSource::with_headers(graph.client(), Some(Arc::clone(cache)))
}

#[tokio::test]
async fn a_header_survives_memory_eviction_and_source_restart_without_graph_requests() {
    for listed_path in [false, true] {
        let (graph, _) = folder_of(1, 3 * 1024 * 1024).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.sqlite");
        let (content, listed) = opened_now(&graph, "P0");
        let source = cached_source(&graph, &Arc::new(HeaderCache::open(path.clone(), BUDGET)));
        let facts = |answer: &Fetched| {
            (
                answer.served_from,
                answer.size,
                answer.mtime,
                answer.version.clone(),
            )
        };
        let first = source
            .read_range("P0", &content, listed_path.then_some(&listed), 33, 4129)
            .await
            .unwrap();
        let original = facts(&first);
        let expected = read_whole(first).await;
        assert!(requests_since(&graph)
            .iter()
            .any(|(kind, _)| *kind == "bytes"));
        source.headers.as_ref().unwrap().flush().await;
        source.read_blocks.clear();
        let second = source
            .read_range("P0", &content, listed_path.then_some(&listed), 33, 4129)
            .await
            .unwrap();
        assert_eq!(facts(&second), original);
        assert_eq!(read_whole(second).await, expected);
        assert!(requests_since(&graph).is_empty());
        drop(source); // Close the source and the cache worker: no memory survives.
        let cache = Arc::new(HeaderCache::open(path, BUDGET));
        cache.flush().await;
        let source = cached_source(&graph, &cache);
        let restarted = source
            .prepared_range("P0", &content, listed_path.then_some(&listed), 33, 4129)
            .await
            .unwrap();
        assert_eq!(facts(&restarted), original);
        assert_eq!(read_whole(restarted).await, expected);
        assert!(
            requests_since(&graph).is_empty(),
            "no Graph traffic across a simulated restart"
        );
        assert!(
            source
                .disk_range("P0", &content, Some(&listed), 0, 33, Some(cache.epoch()))
                .await
                .is_none(),
            "unasked bytes of the fetched 1 MiB block never land in the cache"
        );
    }
}

#[tokio::test]
async fn disk_headers_compare_metadata_etags_separately_from_endpoint_bindings() {
    for listed_path in [false, true] {
        for endpoint_etag in [Some("endpoint-only"), None] {
            let (graph, _) = folder_of(1, LARGE).await;
            let (content, listed) = opened_now(&graph, "P0");
            let mut answer = ResponseTemplate::new(206)
                .insert_header("Content-Range", format!("bytes 0-{}/{LARGE}", LARGE - 1))
                .set_body_bytes(bytes_of(0, LARGE));
            if let Some(etag) = endpoint_etag {
                answer = answer.insert_header("ETag", etag);
            }
            graph.with(|cloud| cloud.script("GET", "dl/P0", answer, 2));
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("headers.sqlite");
            let cache = Arc::new(HeaderCache::open(path.clone(), BUDGET));
            let source = cached_source(&graph, &cache);
            let first = source
                .read_range("P0", &content, listed_path.then_some(&listed), 0, 4096)
                .await
                .unwrap();
            let facts = (
                first.served_from,
                first.size,
                first.mtime,
                first.version.clone(),
            );
            let bytes = read_whole(first).await;
            cache.flush().await;
            drop(source);
            drop(cache);
            requests_since(&graph);

            let cache = Arc::new(HeaderCache::open(path, BUDGET));
            cache.flush().await;
            let restarted = cached_source(&graph, &cache);
            let hit = restarted
                .prepared_range("P0", &content, Some(&listed), 0, 4096)
                .await
                .expect(
                    "metadata ETag is unchanged even when the endpoint uses another ETag or none",
                );
            assert_eq!(
                (hit.served_from, hit.size, hit.mtime, hit.version.clone()),
                facts
            );
            assert_eq!(read_whole(hit).await, bytes);
            assert!(
                requests_since(&graph).is_empty(),
                "restart uses disk with no Graph requests"
            );
            assert_eq!(
                crate::panic::lock(&restarted.ranges)["P0"][&content]
                    .checked
                    .as_ref()
                    .unwrap()
                    .2
                    .as_deref(),
                endpoint_etag
            );
            drop(restarted);

            graph.with(|cloud| cloud.rename("P0", "F", "renamed.png"));
            let (renamed, listed) = opened_now(&graph, "P0");
            assert_eq!(renamed, content);
            let renamed_source = cached_source(&graph, &cache);
            assert!(
                renamed_source
                    .prepared_range("P0", &content, Some(&listed), 0, 4096)
                    .await
                    .is_none(),
                "a changed metadata ETag still discards the old endpoint binding"
            );
        }
    }
}

#[tokio::test]
async fn a_disk_header_of_another_content_misses_and_old_rows_are_deleted() {
    let (graph, _) = folder_of(1, LARGE).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let cache = Arc::new(HeaderCache::open(path.clone(), BUDGET));
    let source = cached_source(&graph, &cache);
    let (old, listed) = opened_now(&graph, "P0");
    read_whole(
        source
            .read_range("P0", &old, Some(&listed), 0, 4096)
            .await
            .unwrap(),
    )
    .await;
    cache.flush().await;
    requests_since(&graph);
    graph.with(|cloud| cloud.edit("P0", &vec![93; LARGE]));
    let (new, listed) = opened_now(&graph, "P0");
    assert_ne!(old, new);
    let source = cached_source(&graph, &cache);
    assert_eq!(
        read_whole(
            source
                .read_range("P0", &new, Some(&listed), 0, 4096)
                .await
                .unwrap()
        )
        .await,
        vec![93; 4096]
    );
    assert_eq!(
        requests_since(&graph)
            .iter()
            .filter(|(kind, _)| *kind == "bytes")
            .count(),
        1
    );
    cache.flush().await;
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM headers WHERE version = ?1",
            [serde_json::to_string(&old).unwrap()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    source.forget("P0");
    cache.flush().await;
    assert_eq!(
        db.query_row("SELECT count(*) FROM headers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "content-change/deletion forget removes disk rows too"
    );
}

#[tokio::test]
async fn read_ahead_bytes_reach_disk_only_when_a_reader_asks_for_them() {
    let (graph, _) = folder_of(1, LARGE).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.sqlite");
    let cache = Arc::new(HeaderCache::open(path.clone(), BUDGET));
    let source = cached_source(&graph, &cache);
    let (content, listed) = opened_now(&graph, "P0");
    source.read_ahead("P0", &content, &listed).await;
    cache.flush().await;
    let db = rusqlite::Connection::open(&path).unwrap();
    let stored = || {
        db.query_row(
            "SELECT coalesce(sum(length(bytes)),0) FROM headers",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(stored(), 0, "read ahead alone writes nothing");
    requests_since(&graph);
    let memory = source
        .cached_range("P0", &content, 65536, 65536 + 4096)
        .unwrap();
    assert_eq!(
        read_whole(memory).await,
        bytes_of(0, LARGE)[65536..65536 + 4096]
    );
    cache.flush().await;
    assert_eq!(
        stored(),
        4096,
        "only the actual memory-hit reader's interval is kept"
    );
    assert!(requests_since(&graph).is_empty());
    read_whole(
        source
            .read_range("P0", &content, Some(&listed), 0, 65537)
            .await
            .unwrap(),
    )
    .await;
    cache.flush().await;
    assert_eq!(stored(), 4096, "large reader ranges are not kept either");
}

#[tokio::test]
async fn disk_headers_restore_the_binding_before_an_uncached_range() {
    let (graph, _) = folder_of(1, 3 * 1024 * 1024).await;
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(HeaderCache::open(dir.path().join("headers.sqlite"), BUDGET));
    let source = cached_source(&graph, &cache);
    let (content, listed) = opened_now(&graph, "P0");
    read_whole(
        source
            .read_range("P0", &content, Some(&listed), 0, 4096)
            .await
            .unwrap(),
    )
    .await;
    cache.flush().await;
    drop(source);
    let source = cached_source(&graph, &cache);
    read_whole(
        source
            .read_range("P0", &content, Some(&listed), 0, 4096)
            .await
            .unwrap(),
    )
    .await;
    requests_since(&graph);
    graph.with(|cloud| cloud.edit("P0", &vec![7; 3 * 1024 * 1024]));
    assert!(
        matches!(
            source
                .read_range(
                    "P0",
                    &content,
                    None,
                    2 * 1024 * 1024,
                    2 * 1024 * 1024 + 4096
                )
                .await,
            Err(SourceError::NotFound(_))
        ),
        "restored binding refuses a new endpoint version"
    );
}

#[tokio::test]
async fn read_ahead_prefetches_past_persisted_heads_after_restart() {
    let (graph, _) = folder_of(3, LARGE).await;
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(HeaderCache::open(dir.path().join("headers.sqlite"), BUDGET));
    let first = cached_source(&graph, &cache);
    for n in 0..3 {
        let id = format!("P{n}");
        let (content, listed) = opened_now(&graph, &id);
        read_whole(
            first
                .read_range(&id, &content, Some(&listed), 0, 16384)
                .await
                .unwrap(),
        )
        .await;
    }
    cache.flush().await;
    drop(first);
    requests_since(&graph);
    let restarted = cached_source(&graph, &cache);
    for n in 0..3 {
        let id = format!("P{n}");
        let (content, listed) = opened_now(&graph, &id);
        restarted.read_ahead(&id, &content, &listed).await;
    }
    assert_eq!(
        requests_since(&graph)
            .iter()
            .filter(|(kind, _)| *kind == "bytes")
            .count(),
        3,
        "each reopened file still prefetches its full first block"
    );
    for n in 0..3 {
        let id = format!("P{n}");
        let (content, listed) = opened_now(&graph, &id);
        // A disk lookup may have begun before the block arrived. Its late result
        // must not replace that full block with just the persisted 16 KiB head.
        // Drain prior readers' best-effort writes so this check exercises a hit,
        // independently of the busy-writer timeout covered by the worker test.
        cache.flush().await;
        assert!(restarted
            .disk_range(&id, &content, Some(&listed), 0, 16384, Some(cache.epoch()))
            .await
            .is_some());
        assert_eq!(
            read_whole(
                restarted
                    .read_range(&id, &content, Some(&listed), 65536, 65536 + 4096)
                    .await
                    .unwrap()
            )
            .await,
            bytes_of(n, LARGE)[65536..65536 + 4096]
        );
    }
    assert!(
        requests_since(&graph).is_empty(),
        "late disk hits preserve prefetched bytes"
    );
}

#[tokio::test]
async fn signed_out_or_revoked_accounts_never_serve_disk_headers() {
    for revoke in [false, true] {
        let (graph, _) = folder_of(1, LARGE).await;
        let tokens = Arc::new(TokenManager::new(
            Arc::new(MemoryStore::with_token("RT")),
            Report,
        ));
        let base = url::Url::parse(&format!("{}/", graph.server.uri())).unwrap();
        tokens.set_oauth(Some(OAuthClient::new(
            reqwest::Client::new(),
            Endpoints {
                authority: base.clone(),
                graph: base.clone(),
            },
            "client".into(),
        )));
        let response = |expires_in| TokenResponse {
            access_token: "AT".into(),
            expires_in,
            refresh_token: None,
            scope: None,
        };
        tokens.seed(&response(3600)).await;
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("headers.sqlite");
        let cache = Arc::new(HeaderCache::open(cache_path.clone(), BUDGET));
        let source = GraphSource::with_headers(
            DriveClient::new(base, tokens.clone()).unwrap(),
            Some(cache.clone()),
        );
        let (content, listed) = opened_now(&graph, "P0");
        source
            .read_range("P0", &content, Some(&listed), 0, 4096)
            .await
            .unwrap();
        cache.flush().await;
        source.read_blocks.clear();
        assert!(cache_path.exists());
        if revoke {
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(
                    ResponseTemplate::new(400).set_body_json(json!({"error":"invalid_grant"})),
                )
                .mount(&graph.server)
                .await;
            tokens.seed(&response(60)).await;
            assert_eq!(tokens.access_token().await, Err(AuthError::SignedOut));
        } else {
            tokens.forget().await.unwrap();
        }
        cache.flush().await;
        assert!(!cache_path.exists());
        requests_since(&graph);
        assert!(source
            .prepared_range("P0", &content, Some(&listed), 0, 4096)
            .await
            .is_none());
        assert!(matches!(
            source
                .read_range("P0", &content, Some(&listed), 0, 4096)
                .await,
            Err(SourceError::SignedOut(_))
        ));
        assert!(requests_since(&graph).is_empty());
        assert!(
            !cache_path.exists(),
            "a refused read cannot recreate the signed-out cache"
        );
    }
}

#[tokio::test]
async fn a_rename_drops_a_stale_disk_binding_before_uncached_ranges_are_read() {
    for listed_path in [false, true] {
        let (graph, _) = folder_of(1, 3 * 1024 * 1024).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(HeaderCache::open(dir.path().join("headers.sqlite"), BUDGET));
        let source = cached_source(&graph, &cache);
        let (content, listed) = opened_now(&graph, "P0");
        read_whole(
            source
                .read_range("P0", &content, listed_path.then_some(&listed), 0, 4096)
                .await
                .unwrap(),
        )
        .await;
        cache.flush().await;
        drop(source);
        graph.with(|cloud| cloud.rename("P0", "F", "renamed.png"));
        let (renamed, listed) = opened_now(&graph, "P0");
        assert_eq!(content, renamed, "rename changes eTag, not content");
        let restarted = cached_source(&graph, &cache);
        requests_since(&graph);
        assert!(
            restarted
                .prepared_range("P0", &renamed, Some(&listed), 0, 4096)
                .await
                .is_none(),
            "obsolete binding is discarded before use"
        );
        assert_eq!(
            read_whole(
                restarted
                    .read_range("P0", &renamed, Some(&listed), 0, 4096)
                    .await
                    .unwrap()
            )
            .await,
            bytes_of(0, 3 * 1024 * 1024)[..4096]
        );
        assert_eq!(
            read_whole(
                restarted
                    .read_range(
                        "P0",
                        &renamed,
                        Some(&listed),
                        2 * 1024 * 1024,
                        2 * 1024 * 1024 + 4096
                    )
                    .await
                    .unwrap()
            )
            .await,
            bytes_of(0, 3 * 1024 * 1024)[2 * 1024 * 1024..2 * 1024 * 1024 + 4096]
        );
        assert_eq!(
            requests_since(&graph)
                .iter()
                .filter(|(kind, _)| *kind == "bytes")
                .count(),
            2,
            "fresh head and uncached tail are readable after the rename"
        );
    }
}

#[tokio::test]
async fn forgotten_prepared_ranges_cannot_repopulate_memory_or_disk() {
    for listed_path in [false, true] {
        let (graph, _) = folder_of(1, LARGE).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(HeaderCache::open(dir.path().join("headers.sqlite"), BUDGET));
        let source = cached_source(&graph, &cache);
        let (content, listed) = opened_now(&graph, "P0");
        let epoch = cache.epoch();
        // Prepare the old-generation work without polling it. It will complete
        // strictly after forget, with no sleeps or timing assumptions.
        let prepared = source.listed_read("P0", &content, &listed, 0, 4096, true);
        source.forget("P0");
        if listed_path {
            assert!(!prepared.read.clone().await.unwrap());
        } else {
            assert_eq!(
                read_whole(
                    source
                        .read_checked("P0", &content, 0, 4096, Some(epoch))
                        .await
                        .unwrap()
                )
                .await,
                bytes_of(0, LARGE)[..4096]
            );
        }
        cache.flush().await;
        assert!(source.read_blocks.find("P0", &content, 0, 4096).is_none());
        assert!(cache
            .find(
                cache.epoch(),
                "P0",
                serde_json::to_string(&content).unwrap(),
                0,
                4096
            )
            .await
            .is_none());
    }
}
