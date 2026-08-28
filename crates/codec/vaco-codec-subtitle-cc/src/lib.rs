//! CEA-608 (line-21) and CEA-708 (DTVCC) closed-caption decode.
//!
//! # What this crate is for
//!
//! [`CodecId::Eia608`](https://docs.rs/vaco-codec-core) covers both formats in
//! this workspace's codec taxonomy: a single elementary stream of `cc_data`
//! triplets carries CEA-608 (analog line-21, two fields x two channels) and
//! CEA-708 (DTVCC, assembled from its own triplets) interleaved, distinguished
//! triplet-by-triplet by the `cc_type` field (ANSI/CTA-708's own framing, also
//! reproduced as `MPEG_cc_data()` in ATSC A/53 Part 4 §6.2.3.1, Table 6.10:
//! `cc_data()` is defined by CEA-708 Table 2). [`CcDecoder`] is the entry
//! point that demultiplexes a `cc_data` byte slice into both.
//!
//! # Two gaps this crate does not close
//!
//! 1. Nothing in this workspace yet extracts `cc_data` from a compressed
//!    stream (H.264 `user_data_registered_itu_t_t35` SEI, HEVC's equivalent,
//!    or MPEG-2 picture user data) and attaches it as
//!    `vaco_frame::FrameSideData::ClosedCaptions`. That population work
//!    belongs to the H.264/HEVC/MPEG-2 parsers, not here. Until it lands,
//!    this crate is reachable only by constructing `cc_data` bytes directly
//!    (as the fixtures in `tests/` do), not from a real compressed file
//!    through this workspace's pipeline.
//! 2. `vaco_frame::Frame` has no subtitle payload variant and
//!    `Decoder::receive_frame` is fixed to return `Frame`, so there is
//!    nowhere honest to plug this crate in as a `kind = "decoder"` registry
//!    component. This crate is therefore a standalone library with its own
//!    output type ([`Event`]), not a registered decoder, until that gap
//!    closes too.
//!
//! Because of both gaps, the public API takes raw `cc_data` bytes — exactly
//! what `FrameSideData::ClosedCaptions`'s buffer holds today, and what
//! ffmpeg's own `A53_CC` side data holds — rather than reaching into a
//! `Frame` itself. That keeps this crate correct the moment a producer exists
//! without needing to change.
//!
//! # Allocation
//!
//! Every one of this format's containers has a hard cap fixed by the wire
//! format itself, not by policy: a DTVCC packet is at most 127 bytes (its
//! length field is 6 bits, ANSI/CTA-708 §5.2), a service block is at most 31
//! bytes (5-bit length, §6.2.2), and a `cc_data` triplet is exactly 3 bytes.
//! Every buffer sized from one of these is therefore a fixed-size stack
//! array, never a heap allocation sized from a value an attacker chose — so
//! there is no declared-length amplification for `vaco_limits::Budget` to
//! guard against, and this crate takes no dependency on it. The only `Vec`s
//! here grow by pushing one decoded element per input byte already in hand,
//! which cannot exceed the size of the slice the caller passed in.
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`triplet`] | `cc_data` triplet framing: `cc_valid`, `cc_type`, the two data bytes |
//! | [`event`] | shared output types: [`Screen`], [`Row`], [`Cell`], [`Style`], [`Color`] |
//! | [`cea608`] | line-21 field/channel demux, pop-on/roll-up/paint-on, PAC styling |
//! | [`cea708`] | DTVCC packet assembly, service blocks, window and pen commands |
//! | [`srt`] | rendering an [`Event`] to SRT-like text, for fixture verification |

#![forbid(unsafe_code)]

pub mod cea608;
pub mod cea708;
pub mod event;
pub mod srt;
pub mod triplet;

pub use cea608::Cea608Decoder;
pub use cea708::Cea708Decoder;
pub use event::{Cell, Color, Row, Screen, Style};
pub use triplet::{CcType, Triplet};

/// Demultiplexes a `cc_data` byte slice into CEA-608 and CEA-708 decode, one
/// call per elementary-stream access unit (one video frame's worth of
/// caption triplets).
///
/// `feed` never fails: malformed or out-of-sequence triplets are dropped
/// exactly as a real decoder must be lenient about them (a channel that has
/// never sent a resume command yet, a DTVCC continuation with no packet in
/// progress), and each drop is counted in [`CcDecoder::stats`] rather than
/// silently discarded, per the project's rule that a discarded error must be
/// countable.
#[derive(Debug, Default)]
pub struct CcDecoder {
    field1: cea608::Cea608Decoder,
    field2: cea608::Cea608Decoder,
    dtvcc: cea708::Cea708Decoder,
    stats: CcStats,
}

/// Counters for triplets this crate could not act on.
///
/// Every field here corresponds to a `let _ = ...`-shaped discard elsewhere
/// in this crate: a place where dropping bad input is the right behaviour,
/// but a silent drop would hide a decoder that is quietly wrong.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CcStats {
    /// `cc_data` triplets with `cc_valid` unset, or a leftover byte that did
    /// not form a full triplet.
    pub skipped_triplets: u64,
    /// CEA-608 byte pairs that failed the odd-parity check.
    pub parity_errors: u64,
    /// DTVCC continuation triplets seen with no packet in progress, or a
    /// packet abandoned because a new `DTVCC_PACKET_START` arrived before it
    /// was complete.
    pub dtvcc_desync: u64,
}

impl CcDecoder {
    /// Construct a decoder with all four CEA-608 channels and every DTVCC
    /// service in their initial state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one access unit's raw `cc_data` bytes (the packed
    /// `cc_count` triplets, three bytes each; a trailing partial triplet is
    /// dropped and counted).
    ///
    /// Returns every caption event produced by this call, in the order the
    /// triplets that caused them appeared in `cc_data`.
    pub fn feed(&mut self, cc_data: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for triplet in triplet::iter_triplets(cc_data, &mut self.stats.skipped_triplets) {
            match triplet.cc_type {
                CcType::Ntsc608Field1 => {
                    if let Some(screen) =
                        self.field1.feed(triplet.data, &mut self.stats.parity_errors)
                    {
                        events.push(Event::Cea608 { field: 1, screen });
                    }
                }
                CcType::Ntsc608Field2 => {
                    if let Some(screen) =
                        self.field2.feed(triplet.data, &mut self.stats.parity_errors)
                    {
                        events.push(Event::Cea608 { field: 2, screen });
                    }
                }
                CcType::Dtvcc708PacketStart | CcType::Dtvcc708PacketData => {
                    self.dtvcc.feed(triplet, &mut events, &mut self.stats.dtvcc_desync);
                }
            }
        }
        events
    }

    /// Counters for triplets this decoder could not act on. See [`CcStats`].
    #[must_use]
    pub const fn stats(&self) -> CcStats {
        self.stats
    }
}

/// One decoded caption update.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A CEA-608 screen change on line-21 field 1 or 2 (`field` is 1 or 2).
    Cea608 {
        /// Which line-21 field this channel was carried on (1 or 2).
        field: u8,
        /// The visible screen after this change.
        screen: Screen,
    },
    /// A CEA-708 window's content or visibility changed.
    Cea708 {
        /// The DTVCC service number (1-63) this window belongs to.
        service_no: u8,
        /// The window ID (0-7) within that service.
        window_id: u8,
        /// The window's on-screen geometry at the time of this event.
        geometry: cea708::WindowGeometry,
        /// The visible screen after this change, or `None` if the window
        /// was just hidden or deleted.
        screen: Option<Screen>,
    },
}
