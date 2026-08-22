//! The shared send/receive state machine.
//!
//! Every component in Vaco that turns a stream of inputs into a stream of
//! outputs — decoders, encoders and bitstream filters — runs the *same* three
//! state protocol. [`Machine`] is that protocol, factored out once so no codec
//! has to reinvent it and so the rules are executable rather than merely
//! documented.
//!
//! ```text
//!                send(Some) ─┐        ┌─ receive → output
//!                            ▼        │
//!    open ───────────►  Feeding ──────┴──►  Feeding
//!                          │  send(None)
//!                          ▼
//!                      Draining ──receive*──► Drained ──receive──► Err(Eof)
//!                          │                     │
//!                          └──── flush() ────────┴──────► Feeding
//! ```
//!
//! See `docs/signal/vaco-codec-core.md` for the normative rule table.

use std::collections::VecDeque;

use vaco_core::{Error, Result};

use crate::Caps;

/// Where a component is in the send/receive protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Accepting input. The steady state.
    Feeding,
    /// End of input has been signalled; the last outputs are still coming.
    Draining,
    /// Everything has been handed over. Only [`Error::Eof`] from here.
    Drained,
}

impl Stage {
    /// Whether [`Machine::accept`] will consider new input at all.
    #[must_use]
    pub const fn accepts_input(self) -> bool {
        matches!(self, Self::Feeding)
    }
}

/// What [`Machine::accept`] decided the caller should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Accept {
    /// Decode/encode/filter the input that was just handed in.
    Input,
    /// End of stream: produce whatever is still buffered, then call
    /// [`Machine::finish`].
    Drain,
}

/// Outputs queued before [`Machine::accept`] refuses more input, for a
/// component that declares neither [`Caps::DELAY`] nor [`Caps::SUBFRAMES`].
pub const TIGHT_CAPACITY: usize = 1;

/// Default queue depth for a component that buffers.
///
/// Deep enough that a reorder buffer or a packet expanding into several frames
/// never trips backpressure spuriously, shallow enough that a caller ignoring
/// `OutputPending` cannot grow memory without bound.
pub const DEFAULT_CAPACITY: usize = 16;

/// The send/receive state machine, generic over the output type.
///
/// Input is never stored: `Error::OutputPending` means "you still own that
/// packet, drain and retry", so the machine only ever needs to track the
/// outputs a component has produced but not yet handed over.
///
/// A codec embeds one of these and calls, in order:
///
/// 1. [`Machine::accept`] at the top of `send_*`, which validates the state
///    transition and applies backpressure;
/// 2. [`Machine::emit`] zero or more times as it produces output;
/// 3. [`Machine::finish`] once, when a drain has produced its last output;
/// 4. [`Machine::receive`] from `receive_*`.
#[derive(Debug)]
pub struct Machine<O> {
    stage: Stage,
    queue: VecDeque<O>,
    capacity: usize,
    caps: Caps,
    source_done: bool,
    accepted: u64,
    produced: u64,
    delivered: u64,
    produced_this_input: u32,
}

impl<O> Machine<O> {
    /// A machine sized from `caps`.
    ///
    /// A component that declares neither [`Caps::DELAY`] nor
    /// [`Caps::SUBFRAMES`] gets [`TIGHT_CAPACITY`], which turns "this codec
    /// secretly buffers" into immediate backpressure rather than unbounded
    /// growth.
    #[must_use]
    pub fn new(caps: Caps) -> Self {
        let capacity = if caps.intersects(Caps::DELAY.union(Caps::SUBFRAMES)) {
            DEFAULT_CAPACITY
        } else {
            TIGHT_CAPACITY
        };
        Self::with_capacity(caps, capacity)
    }

    /// A machine with an explicit output queue depth.
    ///
    /// The depth is clamped to at least one: a machine that can hold no output
    /// could never make progress.
    #[must_use]
    pub fn with_capacity(caps: Caps, capacity: usize) -> Self {
        Self {
            stage: Stage::Feeding,
            queue: VecDeque::new(),
            capacity: capacity.max(1),
            caps,
            source_done: false,
            accepted: 0,
            produced: 0,
            delivered: 0,
            produced_this_input: 0,
        }
    }

    /// Validate a `send_*` call before the component does any work.
    ///
    /// `eof` is `true` for the `None` send that begins draining.
    ///
    /// # Errors
    ///
    /// [`Error::OutputPending`] when the output queue is full — pure
    /// backpressure, never a failure: the caller drains and retries with the
    /// *same* input. [`Error::Eof`] when input was offered after draining
    /// began.
    pub fn accept(&mut self, eof: bool) -> Result<Accept> {
        match self.stage {
            Stage::Draining | Stage::Drained => Err(Error::Eof),
            Stage::Feeding => {
                if eof {
                    self.stage = Stage::Draining;
                    self.produced_this_input = 0;
                    return Ok(Accept::Drain);
                }
                if self.queue.len() >= self.capacity {
                    return Err(Error::OutputPending);
                }
                self.accepted = self.accepted.saturating_add(1);
                self.produced_this_input = 0;
                Ok(Accept::Input)
            }
        }
    }

    /// Queue one output.
    ///
    /// # Panics
    ///
    /// In debug builds only, when the component breaks its own declared
    /// capabilities: emitting after the machine is drained, emitting during a
    /// drain without [`Caps::DELAY`], or emitting a second output for one
    /// input without [`Caps::SUBFRAMES`]. Release builds queue the output
    /// regardless; [`crate::Validated`] is the mechanism that turns these into
    /// test failures.
    pub fn emit(&mut self, output: O) {
        debug_assert!(
            self.stage != Stage::Drained,
            "emit() after the machine reported Eof"
        );
        debug_assert!(
            self.stage != Stage::Draining || self.caps.contains(Caps::DELAY),
            "output produced during drain without Caps::DELAY"
        );
        debug_assert!(
            self.produced_this_input == 0
                || self.caps.contains(Caps::SUBFRAMES)
                || self.stage == Stage::Draining,
            "more than one output for one input without Caps::SUBFRAMES"
        );
        self.produced_this_input = self.produced_this_input.saturating_add(1);
        self.produced = self.produced.saturating_add(1);
        self.queue.push_back(output);
    }

    /// Queue several outputs, in order.
    pub fn emit_all<I: IntoIterator<Item = O>>(&mut self, outputs: I) {
        for o in outputs {
            self.emit(o);
        }
    }

    /// Declare that no further output will ever be produced.
    ///
    /// Called once, during or after the [`Accept::Drain`] handler. Until it is
    /// called, [`Machine::receive`] on an empty queue reports
    /// [`Error::NeedMoreInput`] rather than [`Error::Eof`], because reporting
    /// end-of-stream while the component still holds frames would silently
    /// lose them.
    pub const fn finish(&mut self) {
        debug_assert!(
            !matches!(self.stage, Stage::Feeding),
            "finish() before end of stream was signalled"
        );
        self.source_done = true;
    }

    /// Take the next output.
    ///
    /// # Errors
    ///
    /// [`Error::NeedMoreInput`] while feeding and the queue is empty — send
    /// more. [`Error::Eof`] once draining has handed over its last output.
    pub fn receive(&mut self) -> Result<O> {
        if let Some(out) = self.queue.pop_front() {
            self.delivered = self.delivered.saturating_add(1);
            return Ok(out);
        }
        match self.stage {
            Stage::Feeding => Err(Error::NeedMoreInput),
            Stage::Draining => {
                if self.source_done {
                    self.stage = Stage::Drained;
                    Err(Error::Eof)
                } else {
                    // The component signalled EOF but has not called finish().
                    // Asking for more input is wrong but recoverable; the
                    // alternative — claiming Eof — would drop buffered output.
                    Err(Error::NeedMoreInput)
                }
            }
            Stage::Drained => Err(Error::Eof),
        }
    }

    /// Discard buffered output and return to [`Stage::Feeding`].
    ///
    /// Infallible and total: the post-state is exactly what a fresh machine
    /// looks like, which is what makes seeking cheap.
    pub fn flush(&mut self) {
        self.queue.clear();
        self.stage = Stage::Feeding;
        self.source_done = false;
        self.produced_this_input = 0;
    }

    /// Current protocol state.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// The capabilities this machine was built from.
    #[must_use]
    pub const fn caps(&self) -> Caps {
        self.caps
    }

    /// Outputs queued but not yet taken.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Whether the next non-EOF [`Machine::accept`] would report
    /// [`Error::OutputPending`].
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.queue.len() >= self.capacity
    }

    /// Whether [`Machine::receive`] would hand something over.
    #[must_use]
    pub fn has_output(&self) -> bool {
        !self.queue.is_empty()
    }

    /// The configured queue depth.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Inputs accepted since the last flush.
    #[must_use]
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// Outputs queued since construction.
    #[must_use]
    pub const fn produced(&self) -> u64 {
        self.produced
    }

    /// Outputs handed over since construction.
    #[must_use]
    pub const fn delivered(&self) -> u64 {
        self.delivered
    }
}
