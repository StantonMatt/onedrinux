//! Whole aligned pieces over fake OneDrive, including version changes between blocks.

use super::*;
use crate::content_source::graph_source::read_blocks::BLOCK;

async fn byte_ranges(graph: &FakeGraph) -> Vec<String> {
    graph.server.received_requests().await.unwrap().iter()
        .filter(|r| r.url.path().starts_with("/dl/"))
        .map(|r| r.headers.get("range").unwrap().to_str().unwrap().to_owned()).collect()
}

#[tokio::test]
async fn a_five_mib_file_in_128_kib_reads_costs_five_blocks() {
    let size = 5 * BLOCK as usize;
    let (graph, source) = folder_of(1, size).await;
    let (content, listed) = opened_now(&graph, "P0");
    let expected = bytes_of(0, size);
    for from in (0..size).step_by(128 * 1024) {
        let end = from + 128 * 1024;
        assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), from as u64, end as u64).await.unwrap()).await, expected[from..end]);
    }
    let asked = requests_since(&graph);
    assert_eq!(asked.len(), 6, "one listing and five range requests: {asked:?}");
    assert_eq!(asked.iter().filter(|(k, _)| *k == "bytes").count(), 5);
    assert_eq!(byte_ranges(&graph).await, (0..5).map(|n| format!("bytes={}-{}", n * BLOCK, (n + 1) * BLOCK - 1)).collect::<Vec<_>>());
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), BLOCK - 33, BLOCK + 70).await.unwrap()).await, expected[BLOCK as usize - 33..BLOCK as usize + 70]);
    assert!(requests_since(&graph).is_empty());
}

#[tokio::test]
async fn an_uncached_crossing_read_fetches_both_blocks_and_clips_the_last() {
    let size = BLOCK as usize + 173;
    let (graph, source) = folder_of(1, size).await;
    let (content, listed) = opened_now(&graph, "P0");
    let from = BLOCK - 17;
    let end = BLOCK + 100;
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), from, end).await.unwrap()).await, bytes_of(0, size)[from as usize..end as usize]);
    assert_eq!(byte_ranges(&graph).await, [format!("bytes=0-{}", size - 1)]);
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), BLOCK + 100, size as u64).await.unwrap()).await, bytes_of(0, size)[BLOCK as usize + 100..]);
    assert_eq!(requests_since(&graph).len(), 2, "one listing and one crossing range");
}

#[tokio::test]
async fn the_careful_path_fetches_and_remembers_whole_blocks() {
    let size = 2 * BLOCK as usize + 91;
    let (graph, source) = folder_of(1, size).await;
    let (content, _) = opened_now(&graph, "P0");
    for from in [0, 99, BLOCK, BLOCK + 77, 2 * BLOCK] {
        assert_eq!(read_whole(source.read_range("P0", &content, None, from, from + 10).await.unwrap()).await, bytes_of(0, size)[from as usize..from as usize + 10]);
    }
    assert_eq!(requests_since(&graph), [of("item", "P0"), of("bytes", "P0"), of("item", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0"), of("item", "P0"), of("bytes", "P0"), of("item", "P0")]);
    assert_eq!(byte_ranges(&graph).await, [format!("bytes=0-{}", BLOCK - 1), format!("bytes={}-{}", BLOCK, 2 * BLOCK - 1), format!("bytes={}-{}", 2 * BLOCK, size - 1)]);
}

#[tokio::test]
async fn a_later_block_cannot_switch_the_version_even_after_cache_eviction() {
    for listed_path in [true, false] {
        let size = 2 * BLOCK as usize;
        let (graph, source) = folder_of(1, size).await;
        let (content, listed) = opened_now(&graph, "P0");
        let binding = listed_path.then_some(&listed);
        source.read_range("P0", &content, binding, 0, 10).await.unwrap();
        graph.with(|cloud| cloud.edit("P0", &bytes_of(9, size)));
        assert_eq!(read_whole(source.read_range("P0", &content, binding, 10, 20).await.unwrap()).await, bytes_of(0, size)[10..20]);
        requests_since(&graph);
        assert!(matches!(source.read_range("P0", &content, binding, BLOCK, BLOCK + 10).await, Err(SourceError::NotFound(_))));
        assert_eq!(requests_since(&graph), if listed_path { vec![of("bytes", "P0"), of("item", "P0"), of("bytes", "P0")] } else { vec![of("item", "P0"), of("bytes", "P0")] });
        source.read_blocks.clear();
        assert!(matches!(source.read_range("P0", &content, binding, 0, 10).await, Err(SourceError::NotFound(_))), "eviction never resets the generation's version proof");
        let (new_content, new_listed) = opened_now(&graph, "P0");
        assert_eq!(read_whole(source.read_range("P0", &new_content, Some(&new_listed), BLOCK, BLOCK + 10).await.unwrap()).await, bytes_of(9, size)[BLOCK as usize..BLOCK as usize + 10]);
        assert!(source.read_blocks.find("P0", &content, BLOCK, BLOCK + 10).is_none());
    }
}

#[tokio::test]
async fn an_incomplete_block_is_never_cached_or_partly_answered() {
    let (graph, source) = folder_of(1, BLOCK as usize + 1).await;
    let (content, listed) = opened_now(&graph, "P0");
    let short = ResponseTemplate::new(206).insert_header("ETag", listed.etag.as_str())
        .insert_header("Content-Range", format!("bytes 0-{}/{}", BLOCK - 1, BLOCK + 1))
        .set_body_bytes(vec![1; 10]);
    graph.with(|cloud| cloud.script("GET", "dl/P0", short, 1));
    assert!(matches!(source.read_range("P0", &content, Some(&listed), 0, 5).await, Err(SourceError::Transient(_))));
    assert!(source.read_blocks.find("P0", &content, 0, 5).is_none());
    assert_eq!(read_whole(source.read_range("P0", &content, Some(&listed), 0, 5).await.unwrap()).await, bytes_of(0, BLOCK as usize + 1)[..5]);
}
