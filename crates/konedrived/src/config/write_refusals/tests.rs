use super::*;
use futures_util::FutureExt;

#[tokio::test(start_paused = true)]
async fn write_refusals_emit_first_then_coalesce_and_count_overflow() {
    let reports = WriteRefusals::default();
    let mut receiver = reports.take_receiver().unwrap();
    assert!(reports.take_receiver().is_none());
    // Simulate a stalled exporter: admission never waits, even with a full channel.
    for n in 0..1000 {
        reports.record(format!("Outside/{n}").into(), "Outside".into(), 42);
    }
    let (first, count) = receiver.next().await.unwrap();
    assert_eq!(first.path, PathBuf::from("Outside/0"));
    assert_eq!(count, 1);
    assert!(receiver.next().now_or_never().is_none());
    tokio::time::advance(WINDOW - Duration::from_millis(1)).await;
    assert!(receiver.next().now_or_never().is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    let (next, count) = receiver.next().await.unwrap();
    assert_eq!(next.path, PathBuf::from("Outside/1"));
    assert_eq!(next.folder, PathBuf::from("Outside"));
    assert_eq!(next.pid, 42);
    assert_eq!(count, 999);
    for _ in 0..23 {
        reports.record("top.txt".into(), PathBuf::new(), 0);
    }
    assert!(receiver.next().now_or_never().is_none());
    tokio::time::advance(WINDOW).await;
    let (next, count) = receiver.next().await.unwrap();
    assert!(next.folder.as_os_str().is_empty());
    assert_eq!(count, 23);
    // The producer saw Full before the drain, but published its overflow count
    // afterwards. No further detail arrives to wake recv: the count must still flush.
    reports.overflow.fetch_add(1, Ordering::Relaxed);
    reports.overflow_ready.notify_one();
    tokio::time::advance(WINDOW).await;
    assert_eq!(receiver.next().await.unwrap().1, 1);
    tokio::time::advance(WINDOW * 100).await;
    assert!(
        receiver.next().now_or_never().is_none(),
        "quiet accounts send no empty reports"
    );
    reports.record("after quiet".into(), PathBuf::new(), 0);
    assert_eq!(receiver.next().now_or_never().unwrap().unwrap().1, 1);
}

#[tokio::test]
async fn write_refusals_keep_accounts_separate_and_forget_dead_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        ConfigStore::open(&super::super::Paths::in_dir(dir.path()), async { false }).await,
    );
    let first = store.add_account("First").unwrap().id;
    let second = store.add_account("Second").unwrap().id;
    let source = store.write_scope(&first);
    let same = store.write_refusals(&first);
    assert!(Arc::ptr_eq(&same, &source.refusals));
    let other = store.write_refusals(&second);
    assert!(!Arc::ptr_eq(&same, &other));
    let mut receiver = other.take_receiver().unwrap();
    same.record("first".into(), PathBuf::new(), 0);
    assert!(receiver.next().now_or_never().is_none());
    drop(source);
    drop(same);
    store.write_refusals(&second);
    assert_eq!(crate::panic::lock(&store.write_refusals).len(), 1);
}
