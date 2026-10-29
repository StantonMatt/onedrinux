//! Replies moved off the mount's thread are answered deliberately, even when the job
//! is dropped before it runs. A normal answer consumes the callback; Drop answers EIO.

// test: an_unrun_write_reply_and_a_panicking_job_answer_eio_once
// test: the_kernels_interrupt_aborts_the_read_of_its_request_and_no_other

use fuser::Errno;
use super::ReplyTo;

pub(super) struct PendingReply<T> {
    reply: Option<ReplyTo<T>>,
}

impl<T> PendingReply<T> {
    pub(super) fn new(reply: ReplyTo<T>) -> Self { Self { reply: Some(reply) } }

    pub(super) fn answer(mut self, answer: Result<T, Errno>) {
        if let Some(reply) = self.reply.take() { reply(answer); }
    }
}

impl<T> Drop for PendingReply<T> {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() { reply(Err(Errno::EIO)); }
    }
}

/// Wrap before a callback enters a future or queue: even an unpolled task then owns
/// the guard, so cancelling it cannot drop a raw fuser reply unanswered.
pub(super) fn reply_to<T: 'static>(reply: impl FnOnce(Result<T, Errno>) + Send + 'static) -> ReplyTo<T> {
    let pending = PendingReply::new(Box::new(reply));
    Box::new(move |answer| pending.answer(answer))
}
