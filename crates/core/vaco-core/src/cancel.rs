//! Cooperative cancellation, in one place.
//!
//! # Why it lives here
//!
//! `vaco-io` and `vaco-codec-core` each defined a `CancelToken`, both
//! `Arc<AtomicBool>`, with identical bodies and identical memory orderings. Two
//! definitions of one concept (D19), and worse than merely redundant: a
//! transcode holds both an I/O token and a decode token, and cancelling one
//! does not cancel the other, so "stop" means whichever half the caller
//! happened to reach for.
//!
//! Neither crate depends on the other, so the shared home has to sit below
//! both. That is here.
//!
//! # `check` returns [`Error::Cancelled`], not an interrupted read
//!
//! The `vaco-io` version returned `Error::Io(ErrorKind::Interrupted)`, which is
//! a hazard rather than a nicety. `Interrupted` is the one `io::ErrorKind` the
//! standard library *tells* you to retry, and this workspace already has two
//! loops that do — `vaco_io::raw::ReaderSource::read` and
//! `vaco_protocol_file`'s `read_once`, both correct EINTR handling. They sit
//! below the point where cancellation is raised today, so nothing loops. But a
//! cancellation that surfaces as "please try again" is one refactor away from
//! being retried forever, and cancellation is precisely the signal that must
//! not be retried.
//!
//! So it is its own variant. A caller that wants the old shape can map it.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{Error, Result};

/// A cooperative cancellation flag, shared by every holder of a clone.
///
/// Cloning shares the flag. A blocking transport is expected to poll with a
/// timeout and call [`CancelToken::check`] between polls, so cancellation
/// latency is bounded by the timeout rather than by the read. A decode task
/// polls at picture, slice or row granularity.
///
/// `Release`/`Acquire` rather than `Relaxed`: a thread that observes the
/// cancellation must also observe everything the canceller did before setting
/// it, which is what makes "cancel, then tear down" safe to write.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent, and visible to every clone.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// The form used at an I/O or task boundary.
    ///
    /// # Errors
    /// [`Error::Cancelled`] once cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_sees_the_cancellation() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(a.check().is_ok() && b.check().is_ok());
        b.cancel();
        assert!(
            a.is_cancelled(),
            "a clone shares the flag, it does not copy it"
        );
        assert!(matches!(a.check(), Err(Error::Cancelled)));
    }

    #[test]
    fn cancelling_twice_is_the_same_as_once() {
        let t = CancelToken::new();
        t.cancel();
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn cancellation_is_not_an_interrupted_read() {
        // The shape this type exists to avoid: `Interrupted` is the one kind
        // the standard library tells you to retry, and retrying a cancellation
        // is an infinite loop. Two EINTR retry loops already exist in the
        // workspace; this keeps them from ever seeing one.
        let t = CancelToken::new();
        t.cancel();
        assert!(
            matches!(t.check(), Err(Error::Cancelled)),
            "cancellation is its own error, not an interrupted read"
        );
    }
}
