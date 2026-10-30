//! Speculation uses only the listed proof and shares pending pieces with demand.
use super::*;

#[tokio::test]
async fn read_ahead_uses_only_listed_proof_and_never_falls_back_to_metadata() {
    for failure in ["listing", "etag", "expired", "short"] {
        let (graph, source) = folder_of(1, LARGE).await;
        let (content, listed) = opened_now(&graph, "P0");
        if failure == "listing" {
            graph.with(|cloud| cloud.script("GET", "/children", ResponseTemplate::new(403), 1));
        } else {
            source.links.list_folder(&source.drive, "F", "P0").await;
            match failure {
                "etag" => graph.with(|cloud| cloud.edit("P0", &bytes_of(9, LARGE))),
                "expired" => {
                    graph.with(|cloud| cloud.script("GET", "dl/P0", ResponseTemplate::new(410), 1))
                }
                "short" => graph.with(|cloud| {
                    cloud.script(
                        "GET",
                        "dl/P0",
                        ResponseTemplate::new(206)
                            .insert_header("ETag", listed.etag.as_str())
                            .insert_header(
                                "Content-Range",
                                format!("bytes 0-{}/{LARGE}", LARGE - 1).as_str(),
                            )
                            .set_body_bytes(vec![0; 10]),
                        1,
                    )
                }),
                _ => unreachable!(),
            }
            requests_since(&graph);
        }
        source.read_ahead("P0", &content, &listed).await;
        let requests = requests_since(&graph);
        assert!(
            !requests.iter().any(|(kind, _)| *kind == "item"),
            "{failure}: {requests:?}"
        );
        assert!(
            source.cached_range("P0", &content, 0, 10).is_none(),
            "{failure}"
        );
    }
}

#[tokio::test]
async fn read_ahead_and_a_real_read_share_one_request_and_keep_the_single_version_binding() {
    let size = 3 * read_blocks::BLOCK as usize;
    let (graph, source) = folder_of(1, size).await;
    let (content, listed) = opened_now(&graph, "P0");
    graph.with(|cloud| cloud.delay("GET", "dl/P0", Duration::from_millis(100), 1));
    let ((), fetched) = tokio::join!(
        source.read_ahead("P0", &content, &listed),
        source.read_range("P0", &content, Some(&listed), 0, 16384)
    );
    assert_eq!(
        read_whole(fetched.unwrap()).await,
        bytes_of(0, size)[..16384]
    );
    assert_eq!(
        requests_since(&graph),
        [of("children", "F"), of("bytes", "P0")]
    );
    assert!(source
        .cached_range("P0", &content, 0, read_blocks::BLOCK)
        .is_some());
    assert!(source
        .cached_range("P0", &content, read_blocks::BLOCK, read_blocks::BLOCK + 1)
        .is_none());
    source.read_ahead("P0", &content, &listed).await;
    assert!(
        requests_since(&graph).is_empty(),
        "held pieces cost no request"
    );
    graph.with(|cloud| cloud.edit("P0", &bytes_of(9, size)));
    assert!(
        matches!(
            source
                .read_range(
                    "P0",
                    &content,
                    Some(&listed),
                    read_blocks::BLOCK,
                    read_blocks::BLOCK + 10
                )
                .await,
            Err(SourceError::NotFound(_))
        ),
        "a later block cannot switch versions after speculation"
    );
    let (new_content, new_listed) = opened_now(&graph, "P0");
    assert!(
        source.cached_range("P0", &new_content, 0, 10).is_none(),
        "the new generation cannot inherit the old prefetched head"
    );
    assert_eq!(
        read_whole(
            source
                .read_range("P0", &new_content, Some(&new_listed), 0, 10)
                .await
                .unwrap()
        )
        .await,
        bytes_of(9, size)[..10]
    );
}

#[tokio::test]
async fn signing_out_cancels_speculative_bodies() {
    use konedrive_graph::token::TokenSource;
    let (graph, _) = folder_of(1, LARGE).await;
    let token = Arc::new(StaticToken::new("T"));
    let source = GraphSource::new(
        DriveClient::new(
            url::Url::parse(&format!("{}/", graph.server.uri())).unwrap(),
            token.clone(),
        )
        .unwrap(),
    );
    let (content, listed) = opened_now(&graph, "P0");
    source.links.list_folder(&source.drive, "F", "P0").await;
    graph.with(|cloud| cloud.delay("GET", "dl/P0", Duration::from_secs(1), 1));
    let prefetch = source.read_ahead("P0", &content, &listed);
    let sign_out = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.invalidate().await;
    };
    let began = std::time::Instant::now();
    tokio::join!(prefetch, sign_out);
    assert!(began.elapsed() < Duration::from_millis(500));
    assert!(source.cached_range("P0", &content, 0, 10).is_none());
}

#[tokio::test]
async fn a_real_read_keeps_its_joined_request_when_speculation_is_cancelled() {
    let (graph, source) = folder_of(1, LARGE).await;
    let source = Arc::new(source);
    let (content, listed) = opened_now(&graph, "P0");
    graph.with(|cloud| cloud.delay("GET", "dl/P0", Duration::from_millis(300), 1));
    let prefetch = tokio::spawn({
        let (source, content, listed) = (Arc::clone(&source), content.clone(), listed.clone());
        async move { source.read_ahead("P0", &content, &listed).await }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !graph.with(|cloud| cloud.log.iter().any(|(_, route)| route == "dl/P0")) {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let reading = tokio::spawn({
        let (source, content) = (Arc::clone(&source), content.clone());
        async move { source.prepared_range("P0", &content, 0, 16384).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    prefetch.abort();
    let _ = prefetch.await;
    let fetched = tokio::time::timeout(Duration::from_secs(1), reading)
        .await
        .unwrap()
        .unwrap()
        .expect("the real read owns its joined request");
    assert_eq!(read_whole(fetched).await, bytes_of(0, LARGE)[..16384]);
    assert_eq!(
        requests_since(&graph),
        [of("children", "F"), of("bytes", "P0")],
        "no foreground retry"
    );
}
