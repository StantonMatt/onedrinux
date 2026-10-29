use super::*;

fn content() -> ContentVersion {
    ContentVersion::Hashed { size: 3 * BLOCK, mtime: 0, mtime_nsec: 0, hash: None, ctag: Some("c".into()) }
}

fn piece(from: u64, bytes: Vec<u8>) -> Piece {
    Piece { from, size: Some(3 * BLOCK), mtime: SystemTime::UNIX_EPOCH, version: None, bytes }
}

#[test]
fn read_blocks_prune_expired_bytes_on_find_and_keep() {
    let cache = ReadBlocks::default();
    cache.keep("expired", &content(), &piece(0, b"abc".to_vec()));
    crate::panic::lock(&cache.held)[0].since = Instant::now() - HELD_FOR;
    assert!(cache.find("missing", &content(), 0, 1).is_none());
    assert!(crate::panic::lock(&cache.held).is_empty());
    cache.keep("expired", &content(), &piece(0, b"abc".to_vec()));
    crate::panic::lock(&cache.held)[0].since = Instant::now() - HELD_FOR;
    cache.keep("fresh", &content(), &piece(0, b"abc".to_vec()));
    assert_eq!(crate::panic::lock(&cache.held).len(), 1);
    assert!(cache.find("fresh", &content(), 0, 3).is_some());
    cache.forget("fresh");
    assert!(crate::panic::lock(&cache.held).is_empty());
}

#[test]
fn read_blocks_share_a_bound_and_evict_the_oldest_without_refreshing_on_find() {
    let cache = ReadBlocks::default();
    // A small file and 63 full blocks fit; a cache hit does not make the small file newer.
    cache.keep("small", &content(), &piece(0, vec![1; SMALL_FILE as usize]));
    for n in 0..63 { cache.keep(&n.to_string(), &content(), &piece(0, vec![2; BLOCK as usize])); }
    assert!(cache.find("small", &content(), 0, 10).is_some());
    cache.keep("last", &content(), &piece(0, vec![3; BLOCK as usize]));
    assert!(cache.find("small", &content(), 0, 10).is_none(), "oldest bytes evicted first across small files and blocks");
    assert!(cache.find("0", &content(), 0, 10).is_some());
    // Replacing an existing block uses no extra budget and starts its life again.
    cache.keep("0", &content(), &piece(0, vec![4; BLOCK as usize]));
    cache.keep("overflow", &content(), &piece(0, vec![5; BLOCK as usize]));
    assert!(cache.find("1", &content(), 0, 10).is_none());
    assert!(cache.find("0", &content(), 0, 10).is_some());
    let held = crate::panic::lock(&cache.held);
    assert_eq!(held.iter().map(|h| h.piece.bytes.len()).sum::<usize>(), HELD_AT_MOST);
    drop(held);
    cache.clear();
    assert!(cache.find("last", &content(), 0, 10).is_none());
}

#[test]
fn adjacent_blocks_answer_a_crossing_range_only_of_the_same_generation() {
    let cache = ReadBlocks::default();
    cache.keep("a", &content(), &piece(0, vec![7; BLOCK as usize]));
    assert!(cache.find("a", &content(), BLOCK - 2, BLOCK + 2).is_none());
    let mut other = content();
    if let ContentVersion::Hashed { ctag, .. } = &mut other { *ctag = Some("other".into()); }
    cache.keep("a", &other, &piece(BLOCK, vec![8; BLOCK as usize]));
    assert!(cache.find("a", &content(), BLOCK - 2, BLOCK + 2).is_none());
    cache.keep("a", &content(), &piece(BLOCK, vec![9; BLOCK as usize]));
    let mut answer = cache.find("a", &content(), BLOCK - 2, BLOCK + 2).unwrap();
    let mut bytes = Vec::new();
    use tokio::io::AsyncReadExt;
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(answer.stream.read_to_end(&mut bytes)).unwrap();
    assert_eq!(bytes, [7, 7, 9, 9]);
}

#[test]
fn block_ranges_are_aligned_clipped_and_do_not_overflow() {
    assert_eq!(covering(33, 4096, 491408), (0, 491408));
    assert_eq!(covering(BLOCK - 33, BLOCK + 10, 5 * BLOCK), (0, 2 * BLOCK));
    assert_eq!(covering(4 * BLOCK + 7, 5 * BLOCK, 5 * BLOCK - 11), (4 * BLOCK, 5 * BLOCK - 11));
    assert_eq!(covering(10, 20, SMALL_FILE), (0, SMALL_FILE));
    assert_eq!(covering(u64::MAX - 1, u64::MAX, u64::MAX), (u64::MAX / BLOCK * BLOCK, u64::MAX));
}
