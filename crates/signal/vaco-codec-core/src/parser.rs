//! Driving a [`Parser`] over a byte stream.
//!
//! v0.1 ships parsers, not decoders (D5), so this is on the critical path for
//! the whole milestone. A [`Parser`] is a small, incremental, resynchronising
//! state machine, and every one of them gets the same three things wrong at
//! least once: reassembly across chunk boundaries, the final unit at end of
//! stream, and a byte count that does not match reality. [`ParserDriver`] owns
//! all three so no parser has to.

use vaco_core::{Error, MediaType, Result};
use vaco_limits::{Budget, Limits, ProgressGuard};
use vaco_packet::Packet;

use crate::{CodecId, CodecParameters, Parser};

/// Static description of a bitstream parser, and how to build one.
///
/// The counterpart of [`DecoderDesc`](crate::DecoderDesc), and the descriptor
/// type the registry's `parser` fragment kind was waiting for. A `vaco-parse-*`
/// crate exports one of these as a `const`, names it in its
/// `vaco-component.toml`, and `cargo xtask gen-registry` collects them into
/// `vaco_registry::PARSERS`, which is what
/// [`ParserProvider`](../../vaco_format_core/trait.ParserProvider.html) reads.
/// That indirection is D14.1: a demuxer asks for a parser by [`CodecId`] and
/// never names a codec crate.
///
/// # Why `make` is a `fn` field and not a trait method
///
/// The registry's rule is that a descriptor is **inspectable without
/// constructing anything** — `-parsers` and `-h parser=h264` must print
/// capabilities without allocating. A `const` descriptor holding a function
/// pointer satisfies that; a `Box<dyn ParserFactory>` would not, because
/// `Box::new` is not a `const` operation.
///
/// # Why `codecs` is a slice
///
/// One implementation genuinely covers several [`CodecId`]s — the AAC parser
/// answers for `Aac`, and the H.264 parser would answer for an `H264Mvc` if one
/// existed. A one-to-one field would force a second descriptor per alias, and
/// the two would drift.
#[derive(Clone, Copy)]
pub struct ParserDesc {
    /// Registry name, e.g. `"h264"`. Unique among parsers.
    pub name: &'static str,
    pub long_name: &'static str,
    /// Every codec this implementation parses, in preference order.
    pub codecs: &'static [CodecId],
    pub media_type: MediaType,
    /// Build one, bounded by `limits`.
    ///
    /// Takes [`Limits`] rather than nothing because a parser on the probe path
    /// is handed attacker-controlled bytes before anything has validated them,
    /// and a parser that allocates from an unbounded budget is a denial of
    /// service in a tool people point at untrusted media. There is no
    /// `Default`-limits constructor here on purpose: every caller states the
    /// budget.
    pub make: fn(Limits) -> Box<dyn Parser>,
}

impl ParserDesc {
    /// Whether this implementation parses `codec`.
    #[must_use]
    pub fn handles(&self, codec: CodecId) -> bool {
        self.codecs.contains(&codec)
    }

    /// Build an instance bounded by `limits`.
    #[must_use]
    pub fn build(&self, limits: Limits) -> Box<dyn Parser> {
        (self.make)(limits)
    }
}

impl core::fmt::Debug for ParserDesc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParserDesc")
            .field("name", &self.name)
            .field("long_name", &self.long_name)
            .field("codecs", &self.codecs)
            .field("media_type", &self.media_type)
            .finish_non_exhaustive()
    }
}

/// The default cap on the reassembly buffer.
///
/// A parser that never finds a complete unit would otherwise buffer the whole
/// file. Two megabytes comfortably exceeds any legitimate access unit in the
/// formats v0.1 covers while keeping a hostile input's memory cost bounded.
pub const DEFAULT_MAX_PENDING: usize = 2 << 20;

/// Feeds bytes to a [`Parser`] and collects the access units it produces.
///
/// ```text
///   push(chunk)  ──►  [ reassembly buffer ] ──► parser.parse(&buf) ──► Packet
///                            ▲                        │
///                            └── unconsumed tail ─────┘
///   finish()     ──►  parse(&[]) once the buffer is empty  ──► final Packet
/// ```
///
/// # What it enforces
///
/// * a parser may not report consuming more bytes than it was handed;
/// * a parser that consumes nothing and produces nothing, repeatedly, is a hang
///   — [`ProgressGuard`] converts that into a localised error instead of a
///   fuzzer timeout;
/// * the reassembly buffer is capped, so a stream that never yields a unit
///   cannot exhaust memory;
/// * end of stream is signalled exactly once, by an empty input slice.
#[derive(Debug)]
pub struct ParserDriver<P> {
    parser: P,
    buf: Vec<u8>,
    /// Read cursor into `buf`; bytes before it are consumed and are reclaimed
    /// by compaction rather than by an O(n) drain on every call.
    pos: usize,
    max_pending: usize,
    guard: ProgressGuard,
    budget: Budget,
    eos: bool,
    eos_delivered: bool,
    consumed: u64,
    units: u64,
}

impl<P: Parser> ParserDriver<P> {
    /// Drive `parser`, sizing the reassembly buffer from `limits`.
    #[must_use]
    pub fn new(parser: P, limits: Limits) -> Self {
        Self {
            parser,
            buf: Vec::new(),
            pos: 0,
            max_pending: DEFAULT_MAX_PENDING,
            guard: ProgressGuard::new(),
            budget: Budget::new(limits),
            eos: false,
            eos_delivered: false,
            consumed: 0,
            units: 0,
        }
    }

    /// Override the reassembly cap. Clamped to at least one byte.
    #[must_use]
    pub const fn with_max_pending(mut self, bytes: usize) -> Self {
        self.max_pending = if bytes == 0 { 1 } else { bytes };
        self
    }

    /// Add bytes to the reassembly buffer.
    ///
    /// # Errors
    ///
    /// [`Error::Eof`] if [`ParserDriver::finish`] has already been called, or
    /// [`Error::LimitExceeded`] if the buffer would grow past its cap — which
    /// means the parser is not finding units in what it is being given.
    pub fn push(&mut self, chunk: &[u8]) -> Result<()> {
        if self.eos {
            return Err(Error::Eof);
        }
        self.compact();
        let would_be = self.buf.len().saturating_sub(self.pos) + chunk.len();
        if would_be > self.max_pending {
            return Err(Error::LimitExceeded {
                limit: "parser_reassembly",
                requested: would_be as u64,
                cap: self.max_pending as u64,
            });
        }
        self.budget.check(chunk.len() as u64)?;
        self.buf.extend_from_slice(chunk);
        if !chunk.is_empty() {
            // New bytes ARE progress, even though the parser consumed none from
            // the previous ones. Without this, feeding a stream in chunks
            // smaller than a frame aborts it: each `next_unit` that finds the
            // buffer still too short ticks the guard, and 65 of those trip
            // `NoProgress` — so an 88-byte ADTS frame pushed a byte at a time
            // died before it could be confirmed. Found by `vaco-parse-aac`'s
            // fuzzer; it affects every byte-stream parser, not that one.
            //
            // The hang this guard exists to catch is unaffected: a caller that
            // loops `next_unit` WITHOUT pushing still re-parses the same bytes,
            // still ticks, and still aborts. And a parser that never consumes
            // while the caller keeps pushing is caught by `max_pending` above.
            self.guard.reset();
        }
        Ok(())
    }

    /// Declare that no further bytes will arrive.
    ///
    /// Idempotent. The parser still gets to emit whatever it has buffered:
    /// [`ParserDriver::next_unit`] keeps working until it reports [`Error::Eof`].
    pub const fn finish(&mut self) {
        self.eos = true;
    }

    /// Take the next access unit.
    ///
    /// # Errors
    ///
    /// [`Error::NeedMoreInput`] when the buffer is exhausted and more bytes may
    /// still arrive; [`Error::Eof`] once the stream is finished and the parser
    /// has emitted its last unit; [`Error::InvalidData`] when the parser
    /// misreports its byte count; whatever the parser itself returns otherwise.
    pub fn next_unit(&mut self) -> Result<Packet> {
        loop {
            if self.eos_delivered {
                return Err(Error::Eof);
            }
            let available = self.buf.len().saturating_sub(self.pos);
            if available == 0 {
                if !self.eos {
                    return Err(Error::NeedMoreInput);
                }
                // End of stream: one final call with an empty slice, which is
                // the convention documented on `Parser`.
                let (unit, used) = self.parser.parse(&[])?;
                if used != 0 {
                    return Err(Error::InvalidData(
                        "parser consumed bytes from an empty end-of-stream slice",
                    ));
                }
                if let Some(pkt) = unit {
                    self.units = self.units.saturating_add(1);
                    return Ok(pkt);
                }
                self.eos_delivered = true;
                return Err(Error::Eof);
            }

            let Some(input) = self.buf.get(self.pos..) else {
                return Err(Error::InvalidData("parser reassembly cursor out of range"));
            };
            let (unit, used) = self.parser.parse(input)?;
            if used > input.len() {
                return Err(Error::InvalidData(
                    "parser consumed more bytes than it was given",
                ));
            }
            self.pos += used;
            self.consumed = self.consumed.saturating_add(used as u64);
            if let Some(pkt) = unit {
                self.guard.reset();
                self.units = self.units.saturating_add(1);
                return Ok(pkt);
            }
            // No unit. Progress means bytes were consumed; a run of calls that
            // neither consume nor produce is a hang, not patience.
            self.guard.tick(used != 0)?;
            if used == 0 {
                if self.eos {
                    // The parser will not consume the tail and no more bytes
                    // are coming: discard it and let the final flush run.
                    self.pos = self.buf.len();
                    continue;
                }
                return Err(Error::NeedMoreInput);
            }
        }
    }

    /// Stream properties the parser has discovered so far.
    #[must_use]
    pub fn parameters(&self) -> Option<&CodecParameters> {
        self.parser.parameters()
    }

    /// Total bytes the parser has consumed.
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Total access units emitted.
    #[must_use]
    pub const fn units(&self) -> u64 {
        self.units
    }

    /// Bytes buffered but not yet consumed.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Discard buffered bytes and the end-of-stream state, after a seek.
    ///
    /// The parser's own state is its business: it is reached through
    /// [`ParserDriver::parser_mut`], because only the parser knows what
    /// survives a seek (a parameter set does; a half-assembled access unit
    /// does not).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.pos = 0;
        self.eos = false;
        self.eos_delivered = false;
        self.guard.reset();
    }

    /// Borrow the parser.
    pub const fn parser(&self) -> &P {
        &self.parser
    }

    /// Borrow the parser mutably, to reset codec-specific state on a seek.
    pub const fn parser_mut(&mut self) -> &mut P {
        &mut self.parser
    }

    /// Drop the consumed prefix once it is worth the memmove.
    fn compact(&mut self) {
        if self.pos == 0 {
            return;
        }
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
            return;
        }
        if self.pos * 2 >= self.buf.len() {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }
}
