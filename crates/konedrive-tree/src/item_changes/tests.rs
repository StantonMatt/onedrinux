use super::*;
use crate::Store;
use std::sync::{Arc, Mutex};

#[test]
fn committed_changes_are_seen_before_the_job_returns_and_rollback_is_invisible() {
    let store = Store::new(TreeStore::in_memory().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    store
        .observe_items(move |_, changes| {
            capture.lock().unwrap().push(changes.items);
            Ok(())
        })
        .unwrap();
    seen.lock().unwrap().clear();
    store
        .call_blocking(|store| {
            store.bench_sql("BEGIN; INSERT INTO items(kind,local_parent_id,local_name,local_folded_name) VALUES('directory',1,'a','a'); ROLLBACK;")
        })
        .unwrap();
    assert!(seen.lock().unwrap().is_empty());
    store
        .call_blocking(|store| store.bench_sql("INSERT INTO items(kind,local_parent_id,local_name,local_folded_name) VALUES('directory',1,'b','b');"))
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}
