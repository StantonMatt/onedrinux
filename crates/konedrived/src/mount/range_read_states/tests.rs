use super::*;

fn states() -> (Arc<RangeReadStates>, Arc<Mutex<Vec<LocalItemId>>>) {
    let changes = Arc::new(StateChanges::default());
    let announced = Arc::new(Mutex::new(Vec::new()));
    let saved = Arc::clone(&announced);
    changes.listen(Arc::new(move |items, _| {
        saved.lock().unwrap().extend(items)
    }));
    (Arc::new(RangeReadStates::new(changes)), announced)
}

async fn pass(duration: Duration) {
    tokio::time::sleep(duration).await;
    tokio::task::yield_now().await;
}

#[tokio::test(start_paused = true)]
async fn a_slow_range_is_shown_once_and_stays_through_its_last_reads_linger() {
    let (states, announced) = states();
    let item = LocalItemId::new(5);
    let first = states.begin_read(item);
    pass(SHOW_AFTER - Duration::from_millis(1)).await;
    assert!(!states.is_hydrating(item));
    assert!(announced.lock().unwrap().is_empty());
    pass(Duration::from_millis(1)).await;
    assert!(states.is_hydrating(item));
    assert_eq!(*announced.lock().unwrap(), [item]);
    pass(READ_LINGERS * 5).await;
    let second = states.begin_read(item);
    drop(first);
    pass(READ_LINGERS * 2).await;
    assert!(states.is_hydrating(item));
    assert_eq!(*announced.lock().unwrap(), [item]);
    drop(second);
    pass(READ_LINGERS - Duration::from_millis(1)).await;
    assert!(states.is_hydrating(item));
    let next = states.begin_read(item);
    pass(Duration::from_millis(2)).await;
    assert!(
        states.is_hydrating(item),
        "a later read keeps the shown state"
    );
    drop(next);
    pass(READ_LINGERS).await;
    assert!(!states.is_hydrating(item));
    assert_eq!(*announced.lock().unwrap(), [item, item]);
    assert!(states.readers.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn fifty_quick_files_and_repeated_quick_reads_of_one_file_are_never_shown() {
    let (states, announced) = states();
    for n in 1..=50 {
        let one = states.begin_read(LocalItemId::new(n));
        pass(Duration::from_millis(300)).await;
        drop(one);
    }
    let item = LocalItemId::new(70);
    for _ in 0..50 {
        let one = states.begin_read(item);
        pass(Duration::from_millis(300)).await;
        drop(one);
    }
    pass(READ_LINGERS * 2).await;
    assert!(announced.lock().unwrap().is_empty());
    assert!(states.readers.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn overlapping_quick_reads_do_not_add_their_times() {
    let (states, announced) = states();
    let item = LocalItemId::new(5);
    let mut previous = states.begin_read(item);
    for _ in 0..50 {
        pass(Duration::from_millis(200)).await;
        let next = states.begin_read(item);
        pass(Duration::from_millis(100)).await;
        drop(previous);
        previous = next;
    }
    drop(previous);
    pass(READ_LINGERS * 2).await;
    assert!(!states.is_hydrating(item));
    assert!(announced.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn dropping_an_interrupted_fetch_ends_its_shown_state() {
    let (states, announced) = states();
    let item = LocalItemId::new(5);
    let task = tokio::spawn({
        let states = Arc::clone(&states);
        async move {
            let _reading = states.begin_read(item);
            std::future::pending::<()>().await;
        }
    });
    tokio::task::yield_now().await;
    pass(SHOW_AFTER).await;
    assert!(states.is_hydrating(item));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    pass(READ_LINGERS).await;
    assert!(!states.is_hydrating(item));
    assert_eq!(*announced.lock().unwrap(), [item, item]);
}

#[tokio::test(start_paused = true)]
async fn waiting_for_a_pool_slot_and_an_early_refusal_never_show_hydrating() {
    let (states, announced) = states();
    let pool = konedrive_graph::pool::TransferPool::starting_at(1, 1);
    let mut occupied = Vec::new();
    for _ in 0..=konedrive_graph::pool::RESERVE {
        occupied.push(pool.acquire(konedrive_graph::pool::Class::Open).await);
    }
    let readers = Arc::new(
        super::super::range_readers::RangeReaders::new(pool, tokio::runtime::Handle::current())
            .with_read_states(states.clone()),
    );
    let item = LocalItemId::new(5);
    let task = tokio::spawn({
        let states = states.clone();
        let readers = readers.clone();
        async move {
            let _pending = states.prepare("cloud-id", item);
            let _reading = readers.begin_read("cloud-id").await;
            std::future::pending::<()>().await;
        }
    });
    pass(SHOW_AFTER * 5).await;
    assert!(
        !states.is_hydrating(item),
        "waiting for the pool is not fetching"
    );
    assert!(announced.lock().unwrap().is_empty());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        states.pending.lock().unwrap().is_empty(),
        "a refusal before the slot was acquired drops the binding too"
    );
    drop(occupied);
    let _pending = states.prepare("cloud-id", item);
    let reading = readers.begin_read("cloud-id").await;
    pass(SHOW_AFTER).await;
    assert!(
        states.is_hydrating(item),
        "the delay starts once the pool slot is held"
    );
    drop(reading);
    pass(READ_LINGERS).await;
    assert!(!states.is_hydrating(item));
}
