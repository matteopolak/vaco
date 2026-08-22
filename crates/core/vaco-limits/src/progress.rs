//! The structural progress guarantee (plan 13 §2.2.4a).

use crate::{LimitError, Result};

/// Enforces the progress contract every stepping API in Vaco carries.
///
/// The contract: a call to `Demuxer::read_packet`, `Decoder::receive_frame` or
/// `Filter::activate` either advances the input position, produces output, or
/// reports that it is done. A component that returns "I made progress" without
/// consuming or producing anything is a scheduler infinite loop.
///
/// `ProgressGuard` turns that from a 10-second fuzzer timeout with no stack into
/// an immediate, localised, reproducible [`LimitError::NoProgress`] at a known
/// call site. It counts *consecutive* no-progress steps and gives up at
/// [`ProgressGuard::DEFAULT_MAX_STALLS`]; a single stall is legitimate (a
/// demuxer skipping a corrupt box, a decoder buffering a reference frame), a
/// run of them is not.
///
/// It counts, it does not time. A stall count is a function of the input alone,
/// so a finding replays identically on a different machine.
///
/// # Example
///
/// ```
/// use vaco_limits::ProgressGuard;
///
/// let mut guard = ProgressGuard::new();
/// for _ in 0..1000 {
///     guard.tick(true)?;             // real work happened
/// }
/// for _ in 0..64 {
///     guard.tick(false)?;            // stalls, but not yet fatal
/// }
/// assert!(guard.tick(false).is_err());
/// # Ok::<(), vaco_limits::LimitError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ProgressGuard {
    stalls: u32,
    max_stalls: u32,
    last_position: Option<u64>,
}

impl ProgressGuard {
    /// Consecutive no-progress steps tolerated before giving up.
    ///
    /// 64 is deliberately generous: it is far above any legitimate run of
    /// buffering steps and far below anything a human would call a hang.
    pub const DEFAULT_MAX_STALLS: u32 = 64;

    /// A guard with the default tolerance.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_max_stalls(Self::DEFAULT_MAX_STALLS)
    }

    /// A guard with a custom tolerance.
    #[must_use]
    pub const fn with_max_stalls(max_stalls: u32) -> Self {
        Self {
            stalls: 0,
            max_stalls,
            last_position: None,
        }
    }

    /// Record one step. `progressed` is what the component claims.
    ///
    /// # Errors
    ///
    /// [`LimitError::NoProgress`] once the stall run exceeds the tolerance.
    pub const fn tick(&mut self, progressed: bool) -> Result<()> {
        if progressed {
            self.stalls = 0;
            return Ok(());
        }
        self.stalls = self.stalls.saturating_add(1);
        if self.stalls > self.max_stalls {
            return Err(LimitError::NoProgress { ticks: self.stalls });
        }
        Ok(())
    }

    /// Record one step by input position, deriving `progressed` rather than
    /// trusting the component to report it honestly.
    ///
    /// This is the stronger form and the one to prefer for anything that reads
    /// bytes: a demuxer that returns a packet without advancing the input is
    /// exactly the bug this catches, and it cannot lie about its own offset.
    ///
    /// # Errors
    ///
    /// [`LimitError::NoProgress`] once the stall run exceeds the tolerance.
    pub const fn tick_position(&mut self, position: u64) -> Result<()> {
        let advanced = match self.last_position {
            Some(prev) => position > prev,
            None => true,
        };
        self.last_position = Some(position);
        self.tick(advanced)
    }

    /// Consecutive stalls recorded.
    #[must_use]
    pub const fn stalls(&self) -> u32 {
        self.stalls
    }

    /// Clear the stall run and the remembered position, at a boundary where a
    /// fresh start is correct (a seek, a new stream).
    pub const fn reset(&mut self) {
        self.stalls = 0;
        self.last_position = None;
    }
}

impl Default for ProgressGuard {
    fn default() -> Self {
        Self::new()
    }
}
