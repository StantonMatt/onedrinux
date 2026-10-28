use super::*;
#[test]
fn reserve_is_fail_closed_and_counts_concurrent_allocations() {
    assert!(!fits(None, 10, 0, 1));
    assert!(!fits(Some(9), 10, 0, 1));
    assert!(fits(Some(20), 10, 0, 10));
    assert!(!fits(Some(20), 10, 1, 10));
    let disks = Arc::new(DiskReservations::default());
    let first = disks.reserve(1, Some(20), 10, 8).unwrap();
    assert!(disks.reserve(1, Some(20), 10, 3).is_none());
    assert!(disks.reserve(2, Some(20), 10, 8).is_some());
    drop(first);
    assert!(disks.reserve(1, Some(20), 10, 8).is_some());
}
#[test]
fn reads_and_pins_cannot_spend_the_same_free_space() {
    let disks = Arc::new(DiskReservations::default());
    let pin = disks.keep(1, 8, false);
    assert!(disks.reserve(1, Some(20), 10, 3).is_none());
    assert_eq!(disks.available_to_plan(1, 20), 20, "the plan counts its own running pin");
    drop(pin);
    let read = disks.reserve(1, Some(20), 10, 8).unwrap();
    assert_eq!(disks.available_to_plan(1, 20), 12, "future pins account for the read");
    let hydrate = disks.keep(1, 4, true);
    assert_eq!(disks.available_to_plan(1, 20), 8);
    assert!(disks.reserve(1, Some(20), 10, 1).is_none());
    drop((read, hydrate));
    assert_eq!(disks.available_to_plan(1, 20), 20);
}
