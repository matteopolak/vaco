//! Cooperative cancellation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vaco_core::{Error, Result};

/// The `AVIOInterruptCB` equivalent: a flag any thread can set and every I/O
/// boundary checks.
///
/// Cloning shares the flag. A blocking transport is expected to poll with a
/// timeout and call [`CancelToken::check`] between polls, so cancellation
/// latency is bounded by the timeout rather than by the read.
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

    /// The form used at I/O boundaries.
    ///
    /// # Errors
    /// [`Error::Io`] with [`std::io::ErrorKind::Interrupted`] once cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            )));
        }
        Ok(())
    }
}
