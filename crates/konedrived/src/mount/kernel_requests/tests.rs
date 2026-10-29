//! The pure pieces of the kernel's interface and the reads it keeps for the kernel's
//! interrupt and the mount's stop. The requests themselves are tested under it
//! (`item_kernel_requests/tests.rs`); the session is the VM suite's.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{Errno, RenameFlags, TimeOrNow};
use konedrive_tree::ItemKind;

use super::*;
use crate::mount::item_fixture::ItemFixture;

#[test]
fn an_extended_attribute_is_answered_by_the_size_of_the_callers_buffer() {
    let cases = [
        // (the value's length, the buffer's size, the answer)
        (5, 0, Ok(XattrReply::Length(5))),
        (0, 0, Ok(XattrReply::Length(0))),
        (5, 4, Err(Errno::ERANGE)),
        (5, 5, Ok(XattrReply::Value)),
        (5, 64, Ok(XattrReply::Value)),
        (0, 8, Ok(XattrReply::Value)),
    ];
    for (value_length, size, reply) in cases {
        assert_eq!(decide_xattr_reply(value_length, size), reply, "a value of {value_length} into a buffer of {size}");
    }
}

#[test]
fn the_kernels_flags_of_a_rename_say_what_becomes_of_the_new_name() {
    let cases = [
        (RenameFlags::empty(), RenameMode::Replace),
        (RenameFlags::RENAME_NOREPLACE, RenameMode::NoReplace),
        (RenameFlags::RENAME_EXCHANGE, RenameMode::Exchange),
        // An exchange is refused below whatever else is asked with it.
        (RenameFlags::RENAME_EXCHANGE | RenameFlags::RENAME_NOREPLACE, RenameMode::Exchange),
    ];
    for (flags, mode) in cases {
        assert_eq!(to_rename_mode(flags), mode, "{flags:?}");
    }
}

#[test]
fn a_time_asked_of_setattr_is_in_whole_seconds_and_the_rest() {
    assert_eq!(to_unix_time(TimeOrNow::SpecificTime(UNIX_EPOCH + Duration::new(1_700_000_000, 123))), (1_700_000_000, 123));
    assert_eq!(to_unix_time(TimeOrNow::SpecificTime(UNIX_EPOCH)), (0, 0));
    assert_eq!(to_unix_time(TimeOrNow::SpecificTime(UNIX_EPOCH - Duration::from_secs(5))), (0, 0), "a time before 1970 is 0");
    assert!(to_unix_time(TimeOrNow::Now).0 > 1_700_000_000, "now is read from the clock");
}

#[test]
fn a_listed_item_is_named_by_its_local_place_and_one_with_no_local_version_has_no_name() {
    let fixture = ItemFixture::new();
    let shown = fixture.show_onedrive_file("docs.txt", b"docs");
    let listed_only = fixture.list_onedrive_file("only-listed.txt", b"listed");
    let directory = fixture.show_onedrive_directory("dir");

    assert_eq!(find_listed_name(&shown), Some(OsString::from("docs.txt").as_os_str()));
    assert_eq!(find_listed_name(&directory), Some(OsString::from("dir").as_os_str()));
    assert_eq!(directory.kind, ItemKind::Directory);
    assert_eq!(find_listed_name(&listed_only), None, "nothing shows it");
}

/// Uses the same ownership and completion path as a kernel range fetch, with an answer
/// callback that records every reply. `held` stands for resources owned by the future.
fn start_read(reads: &Arc<InterruptibleReads>, runtime: &tokio::runtime::Handle, request: u64,
    future: impl std::future::Future<Output = Result<Vec<u8>, Errno>> + Send + 'static,
) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Result<Vec<u8>, Errno>>>>) {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let answered = Arc::clone(&answers);
    let reply = Arc::new(Mutex::new(Some(PendingReply::new(Box::new(move |answer| crate::panic::lock(&answered).push(answer))))));
    let completion = ReadCompletion { request, reads: Arc::clone(reads), reply: Arc::clone(&reply) };
    let mut task = None;
    reads.start(request, reply, || {
        let spawned = runtime.spawn(async move { completion.answer(future.await); });
        let abort = spawned.abort_handle();
        task = Some(spawned);
        abort
    });
    (task.unwrap(), answers)
}

#[tokio::test(start_paused = true)]
async fn the_kernels_interrupt_aborts_the_read_of_its_request_and_no_other() {
    let runtime = tokio::runtime::Handle::current();
    let reads = Arc::new(InterruptibleReads::default());
    let (pool, ranges) = crate::mount::range_readers::tests::range_readers();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let (first, first_answers) = start_read(&reads, &runtime, 1, async move {
        let _active = ranges.begin_read("A").await;
        started.send(()).unwrap();
        std::future::pending().await
    });
    let (second, second_answers) = start_read(&reads, &runtime, 2, std::future::pending());
    waiting.await.unwrap();
    reads.interrupt(1);
    reads.interrupt(1);
    reads.interrupt(7);
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(*crate::panic::lock(&first_answers), [Err(Errno::EINTR)], "EINTR exactly once, no raw reply dropped");
    assert!(!second.is_finished());
    assert!(crate::panic::lock(&second_answers).is_empty());
    assert_eq!(crate::panic::lock(&reads.0).by_request.len(), 1);
    tokio::time::sleep(super::super::range_readers::READ_LINGERS * 2).await;
    assert_eq!(pool.held(konedrive_graph::pool::Class::Open), 0, "abort dropped ActiveRangeRead and its slot expired");
    reads.stop();
    assert!(second.await.unwrap_err().is_cancelled());
    assert_eq!(*crate::panic::lock(&second_answers), [Err(Errno::EIO)]);
}

#[tokio::test]
async fn a_read_that_ended_is_kept_no_more() {
    let reads = Arc::new(InterruptibleReads::default());
    let (read, answers) = start_read(&reads, &tokio::runtime::Handle::current(), 3, async { Ok(b"done".to_vec()) });
    read.await.unwrap();
    assert!(crate::panic::lock(&reads.0).by_request.is_empty());
    reads.interrupt(3);
    reads.stop();
    assert_eq!(*crate::panic::lock(&answers), [Ok(b"done".to_vec())]);
}

#[tokio::test]
async fn a_stop_gives_up_every_read_and_each_one_started_after_it() {
    let reads = Arc::new(InterruptibleReads::default());
    let runtime = tokio::runtime::Handle::current();
    let held = Arc::new(());
    let in_task = Arc::clone(&held);
    let (first, answers) = start_read(&reads, &runtime, 1, async move { let _held = in_task; std::future::pending().await });
    // No yield: stop also answers a task that has never been polled.
    reads.stop();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(*crate::panic::lock(&answers), [Err(Errno::EIO)]);
    assert_eq!(Arc::strong_count(&held), 1);
    assert!(crate::panic::lock(&reads.0).by_request.is_empty());
    let (late, answers) = start_read(&reads, &runtime, 3, std::future::pending());
    assert!(late.await.unwrap_err().is_cancelled());
    assert_eq!(*crate::panic::lock(&answers), [Err(Errno::EIO)]);
    assert!(crate::panic::lock(&reads.0).by_request.is_empty());
}

#[tokio::test]
async fn an_unpolled_interrupt_and_a_panicking_read_still_answer_once() {
    let reads = Arc::new(InterruptibleReads::default());
    let runtime = tokio::runtime::Handle::current();
    let (task, answers) = start_read(&reads, &runtime, 1, std::future::pending());
    reads.interrupt(1);
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(*crate::panic::lock(&answers), [Err(Errno::EINTR)]);
    let (task, answers) = start_read(&reads, &runtime, 2, async { panic!("a failed range task") });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(*crate::panic::lock(&answers), [Err(Errno::EIO)]);
    assert!(crate::panic::lock(&reads.0).by_request.is_empty());
}

#[test]
fn an_unrun_write_reply_and_a_panicking_job_answer_eio_once() {
    for panic in [false, true] {
        let answers = Arc::new(Mutex::new(Vec::new()));
        let answered = Arc::clone(&answers);
        let reply = reply_to(move |answer: Result<(), Errno>| crate::panic::lock(&answered).push(answer));
        let job = move || { if panic { panic!("a failed write job"); } reply(Ok(())); };
        if panic { assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err()); }
        else { drop(job); }
        assert_eq!(*crate::panic::lock(&answers), [Err(Errno::EIO)]);
    }
}

#[test]
fn a_read_on_a_stopped_runtime_is_answered_eio_and_forgotten() {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let handle = runtime.handle().clone();
    drop(runtime);
    let reads = Arc::new(InterruptibleReads::default());
    let (task, answers) = start_read(&reads, &handle, 1, std::future::pending());
    assert!(task.is_finished());
    assert_eq!(*crate::panic::lock(&answers), [Err(Errno::EIO)]);
    assert!(crate::panic::lock(&reads.0).by_request.is_empty());
}
