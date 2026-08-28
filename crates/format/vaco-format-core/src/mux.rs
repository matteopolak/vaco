//! The muxer state machine, and the rest of the mux-side chain around it.
//!
//! [`crate::interleave`] holds M1–M5 — the timestamp chain and the interleave
//! queue. This module holds everything that decides *when* those run: the
//! init/header/packet/trailer ordering (M8–M11), the checks that ordering makes
//! possible (M12–M17, M21–M22), and the bitstream-filter-in-muxer stage M6
//! (plan 18 §1.10).
//!
//! # Why there is a wrapper type at all
//!
//! Before this module, [`crate::Muxer`]'s doc comments described the ordering —
//! "all streams must be added before `write_header`", "packets must arrive in
//! interleaved order" — and nothing checked any of it. Every implementation
//! re-derived the same four booleans, or forgot to. `VacoRawMuxer` carries
//! `header_written` and `trailer_written` and four hand-written guards; five
//! more containers landing in one wave means five more copies of those guards,
//! written five different ways, each with its own error string.
//!
//! [`MuxBuilder`] and [`MuxWriter`] make the ordering a property of the *type*
//! rather than of the caller's discipline:
//!
//! ```text
//!   MuxBuilder ── add_stream ──▶ MuxBuilder
//!        │
//!        └── open() ─────────▶ MuxWriter ── write_packet ──▶ MuxWriter
//!            (init + header)       │
//!                                  └── finish() ──▶ MuxReport
//!                                      (drain + trailer)
//! ```
//!
//! `MuxBuilder` has no `write_packet`. `MuxWriter` has no `add_stream`. `open`
//! and `finish` **consume** the value they transition from, so there is no
//! second header and no second trailer — not "an error at run time", but no
//! spelling that compiles.
//!
//! # Why this shape and not the other two
//!
//! *Runtime checks on the trait* is what we had. A check that the caller can
//! skip is not a state machine; it is documentation with a panic in it. And
//! because the guard lives in each muxer, a muxer that forgets it is
//! indistinguishable from one that does not need it.
//!
//! *A phantom typestate* — `Mux<Building>` / `Mux<Writing>` — gives the same
//! guarantee, and puts a type parameter in the signature of every function that
//! touches a muxer. A caller that must hold "either phase" in one struct field
//! then needs an enum over both instantiations anyway, so the parameter buys
//! nothing that consuming transitions do not.
//!
//! *Changing the `Muxer` trait itself* — `fn write_header(self) -> Writer` —
//! is the version that would make the guarantee unavoidable for implementors
//! too. It is also the one thing that could not be done: **five container
//! crates are being written against this trait right now**, in parallel, and a
//! trait change lands underneath all five at once. Every addition here is a
//! defaulted method or a new type, so an implementation written against
//! yesterday's trait still compiles today. The wrapper gives *callers* the
//! guarantee without asking implementors for anything, and a muxer that wants
//! to keep its own internal guards may — `VacoRawMuxer` does, and the two agree.
//!
//! The cost is honest and worth stating: an implementor can still be driven
//! directly through `dyn Muxer` and get the old, unpoliced behaviour. The
//! wrapper is the supported path, not the only one.
//!
//! # The rules, and where they come from
//!
//! `planning/18-formats.md` §8.2 names FW-08 as "M1–M28", and §1.7.7 defines
//! **M1–M7** and nothing else; §7.1 repeats the "M1–M28" span. M8 upward do not
//! exist in the plan under any spelling. Rather than guess at twenty-one rules
//! somebody else wrote down, the numbering below is *ours*, and each row cites
//! the plan section that motivates it. Anything that turns out to be a
//! renumbering of a real list should be renumbered, not re-derived.
//!
//! | # | Rule | Source |
//! |---|---|---|
//! | M1–M4 | rescale · `output_ts_offset` · `avoid_negative_ts` · monotonicity | §1.7.7, [`crate::interleave::MuxTimestamps`] |
//! | M5 | the interleave queue | §1.9, [`crate::interleave::InterleaveQueue`] |
//! | M6 | the bitstream-filter chain | §1.10, [`BsfChain`] |
//! | M7 | `Muxer::write_packet` | §1.7.7 |
//! | M8 | streams may only be added before the header | §1.3; type-enforced |
//! | M9 | the header is written exactly once | §1.3; type-enforced |
//! | M10 | packets only between header and trailer | §1.3; type-enforced |
//! | M11 | the trailer runs once, after the queue is drained | §1.3, §1.9 N4 |
//! | M12 | `init` runs before the header and may rewrite time bases | §1.3 |
//! | M13 | zero streams needs `NOSTREAMS` | §1.1 flags |
//! | M14 | `max_streams` caps the mux side too | §1.11 #36 |
//! | M15 | the container is asked whether it can carry the codec | §1.3 `query_codec` |
//! | M16 | `GLOBALHEADER` without extradata asks for `extract_extradata` | §1.10 B5 |
//! | M17 | an `EXPERIMENTAL` container needs `-strict experimental` | §1.11 #27 |
//! | M18 | `NOTIMESTAMPS` clears both fields and the queue accepts it | §1.7 R27 |
//! | M19 | `+flush_packets` / `flush_packets=1` flushes after every packet | §1.11 #5, #23 |
//! | M20 | a flush marker only reaches a muxer declaring `ALLOW_FLUSH` | §1.3 |
//! | M21 | a packet naming an undeclared stream is refused | §1.9 |
//! | M22 | a packet on a stream already ended is refused | §1.9 N4 |
//! | M23 | the resolved shift and policy are reported | §1.7 R25 |
//! | M24 | `bitexact` suppresses `start_time_realtime` | §1.11 #13 |
//! | M25 | `metadata_header_padding` is surfaced to the muxer | §1.11 #24 |
//! | M26 | packet and byte counts are recorded for the caller's stats | plan 14 |
//! | M27 | ending a stream lets the rest drain | §1.9 N4 |
//! | M28 | aborting writes no trailer, and says so | §1.3 |
//! | M29 | muxer-private options (`-movflags`) are applied before `init` | gap 5, `planning/INTERFACE-GAPS.md` |
//! | M30 | metadata reaches the muxer after `init`, before the header | gap 1, `planning/INTERFACE-GAPS.md` |

use std::sync::Arc;

use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result, TimeBase};
use vaco_packet::Packet;

use crate::flags::FormatFlags;
use crate::interleave::{InterleaveQueue, MuxTimestamps};
use crate::metadata::MuxMetadata;
use crate::options::{AvoidNegativeTs, FFlags, FormatOptions};
use crate::time::TIME_BASE_Q;
use crate::{Muxer, StreamSpec, StreamType};

/// Whether a container will carry a codec, at a given compliance level (M15).
///
/// Three states rather than a `bool` because "only with `-strict
/// experimental`" is a real and common answer — every container has a handful
/// of codecs it can technically hold and does not officially support — and
/// collapsing it into either `true` or `false` loses the one thing the user
/// needs to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodecSupport {
    /// Carried, at any compliance level.
    #[default]
    Supported,
    /// Carried only when `strict` is at or below `experimental` (-2).
    Experimental,
    /// Not carried at all.
    Unsupported,
}

impl CodecSupport {
    /// Whether this answer permits the write at compliance level `strict`.
    #[must_use]
    pub const fn permitted_at(self, strict: i32) -> bool {
        match self {
            Self::Supported => true,
            Self::Experimental => strict <= -2,
            Self::Unsupported => false,
        }
    }
}

/// What a muxer wants done to a stream's packets before it sees them (§1.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitstreamAction {
    /// Nothing. The chain is complete.
    #[default]
    Keep,
    /// Insert this filter, then ask again on its *output*, so chains compose:
    /// MP4 output of an Annex-B H.264 stream needs `extract_extradata` and the
    /// length-prefix conversion, and neither muxer knows about the other.
    Insert { name: &'static str },
}

/// Supplies bitstream filters to a muxer without the muxer naming a codec crate.
///
/// The mux-side mirror of [`crate::ParserProvider`], and the same seam for the
/// same reason (D14.1): `vaco-mux-mp4` needs `h264_annexb2mp4`, and a
/// dependency edge from every container crate to every bitstream-filter crate
/// would make the graph a mesh. `vaco-registry` implements this.
pub trait BsfProvider: Send + Sync {
    /// Open the filter named `name` for a stream with `params`.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when the name is not known.
    fn open(&self, name: &str, params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>>;
}

/// The default provider: knows no filters.
///
/// What every muxer unit test and fuzz target uses. A muxer that asks for a
/// filter under `NoBsfs` gets an error rather than silently-unfiltered packets,
/// because a container that needed `aac_adtstoasc` and did not get it produces
/// a file no player will open — a failure that is much cheaper at mux time.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoBsfs;

impl BsfProvider for NoBsfs {
    fn open(&self, name: &str, params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
        let _ = (name, params);
        Err(Error::Unsupported(
            "this muxer needs a bitstream filter and no BsfProvider was supplied",
        ))
    }
}

/// The most filters that may be stacked on one stream (B2).
pub const MAX_BSF_DEPTH: usize = 4;

/// The most packets one input packet may expand into before we call it a bug.
///
/// A filter that splits (`vp9_superframe_split`) legitimately produces several;
/// one that produces thousands is either broken or being driven by a crafted
/// file, and either way the queue behind it must not be allowed to grow without
/// bound. The cap is ours — no format dictates one — and it is here because
/// packets reaching a muxer came from a demuxer that read attacker bytes.
pub const MAX_BSF_EXPANSION: usize = 4096;

/// The M6 stage for one stream: the filters, and the decision that chose them.
///
/// Built lazily on the stream's first packet and then frozen (B3). A stream
/// whose bitstream form changes mid-file is deliberately *not* re-examined —
/// that is what `avc3`/`hev1` sample entries exist for, and re-deciding
/// mid-stream would change the container's own configuration record after it
/// had been written.
#[derive(Default)]
pub struct BsfChain {
    filters: Vec<Box<dyn BitstreamFilter>>,
    names: Vec<&'static str>,
    decided: bool,
}

impl core::fmt::Debug for BsfChain {
    /// Hand-written because `Box<dyn BitstreamFilter>` is not `Debug`; the
    /// names are the part anybody debugging a chain wants anyway.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BsfChain")
            .field("filters", &self.filters.len())
            .field("names", &self.names)
            .field("decided", &self.decided)
            .finish()
    }
}

impl BsfChain {
    /// Whether the chain has been decided for this stream.
    #[must_use]
    pub const fn is_decided(&self) -> bool {
        self.decided
    }

    /// The filter names in chain order, for reporting.
    #[must_use]
    pub fn names(&self) -> &[&'static str] {
        &self.names
    }

    /// Run `pkt` through the chain, appending the results to `out`.
    ///
    /// A filter may produce zero packets (it is buffering), one, or several
    /// (a superframe split). All three are normal.
    ///
    /// # Errors
    ///
    /// Whatever a filter returns, other than the two protocol signals
    /// [`Error::NeedMoreInput`] and [`Error::Eof`]; or
    /// [`Error::LimitExceeded`] if one packet expands past
    /// [`MAX_BSF_EXPANSION`].
    pub fn filter(&mut self, pkt: Packet, out: &mut Vec<Packet>) -> Result<()> {
        if self.filters.is_empty() {
            out.push(pkt);
            return Ok(());
        }
        let mut stage = vec![pkt];
        let mut next = Vec::new();
        for f in &mut self.filters {
            next.clear();
            for p in stage.drain(..) {
                f.send_packet(Some(&p))?;
                drain_filter(f.as_mut(), &mut next)?;
            }
            core::mem::swap(&mut stage, &mut next);
        }
        out.append(&mut stage);
        Ok(())
    }

    /// Flush every filter at end of stream, appending what falls out.
    ///
    /// # Errors
    ///
    /// As [`BsfChain::filter`].
    pub fn flush(&mut self, out: &mut Vec<Packet>) -> Result<()> {
        let mut carried: Vec<Packet> = Vec::new();
        for f in &mut self.filters {
            let mut next = Vec::new();
            for p in carried.drain(..) {
                f.send_packet(Some(&p))?;
                drain_filter(f.as_mut(), &mut next)?;
            }
            // `None` is the protocol's end-of-stream marker.
            f.send_packet(None)?;
            drain_filter(f.as_mut(), &mut next)?;
            carried = next;
        }
        out.append(&mut carried);
        Ok(())
    }
}

/// Pull everything a filter has ready, treating the two protocol signals as
/// "nothing more for now" rather than as failures.
fn drain_filter(f: &mut dyn BitstreamFilter, out: &mut Vec<Packet>) -> Result<()> {
    loop {
        match f.receive_packet() {
            Ok(p) => {
                if out.len() >= MAX_BSF_EXPANSION {
                    return Err(Error::LimitExceeded {
                        limit: "bsf expansion",
                        requested: u64::try_from(out.len()).unwrap_or(u64::MAX),
                        cap: u64::try_from(MAX_BSF_EXPANSION).unwrap_or(u64::MAX),
                    });
                }
                out.push(p);
            }
            Err(Error::NeedMoreInput | Error::Eof) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// What a finished mux run did (M23, M26).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MuxReport {
    /// Packets handed to [`Muxer::write_packet`], after filtering.
    pub packets: u64,
    /// Payload bytes handed to the muxer. Not the file size — the container's
    /// own overhead is the muxer's business and it does not report it.
    pub payload_bytes: u64,
    /// Per stream, in stream-index order.
    pub per_stream_packets: Vec<u64>,
    /// What `avoid_negative_ts` resolved to for this container (R25).
    pub avoid_negative_ts: AvoidNegativeTs,
    /// The shift M3 settled on, microseconds, once a first packet was seen.
    /// `Some(0)` means "decided, and no shift"; `None` means no packet arrived.
    pub ts_offset_us: Option<i64>,
    /// Filters M6 inserted, per stream, in chain order.
    pub bitstream_filters: Vec<Vec<&'static str>>,
    /// Whether the trailer was written. False after [`MuxWriter::abort`] (M28).
    pub trailer_written: bool,
}

/// Per-stream bookkeeping the session keeps and the muxer does not have to.
struct StreamState {
    params: CodecParameters,
    /// The time base packets arrive in. M1's `from`.
    input_time_base: TimeBase,
    /// The time base the container chose. M1's `to`; re-read after `init` (M12).
    output_time_base: TimeBase,
    ended: bool,
    packets: u64,
    bsf: BsfChain,
}

/// Phase one: declare streams (M8).
///
/// Has no `write_packet`, by construction. [`MuxBuilder::open`] consumes it and
/// returns the only type that has one.
pub struct MuxBuilder {
    muxer: Box<dyn Muxer>,
    opts: FormatOptions,
    flags: FormatFlags,
    streams: Vec<StreamState>,
    bsfs: Arc<dyn BsfProvider>,
    metadata: MuxMetadata,
    /// Muxer-private options queued for [`Muxer::set_option`] (gap 5).
    options: Vec<(String, String)>,
}

impl core::fmt::Debug for MuxBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MuxBuilder")
            .field("flags", &self.flags)
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl MuxBuilder {
    /// Start a session over `muxer`.
    ///
    /// The flags are read from the muxer once, here, and not consulted again:
    /// they are a property of the container and cannot change while a file is
    /// being written.
    #[must_use]
    pub fn new(muxer: Box<dyn Muxer>, opts: &FormatOptions) -> Self {
        let flags = muxer.flags();
        Self {
            muxer,
            opts: opts.clone(),
            flags,
            streams: Vec::new(),
            bsfs: Arc::new(NoBsfs),
            metadata: MuxMetadata::default(),
            options: Vec::new(),
        }
    }

    /// Supply the bitstream filters M6 may need (B4).
    #[must_use]
    pub fn with_bsfs(mut self, provider: Arc<dyn BsfProvider>) -> Self {
        self.bsfs = provider;
        self
    }

    /// Attach file- and stream-level metadata for [`Muxer::set_metadata`] to
    /// receive at [`MuxBuilder::open`] (M30, gap 1).
    ///
    /// Not calling this at all is what every existing caller of `MuxBuilder`
    /// does today, and it is indistinguishable from calling it with
    /// [`MuxMetadata::default`]: [`MuxBuilder::open`] always calls
    /// `set_metadata`, and its default implementation drops whatever it is
    /// handed, empty or not.
    #[must_use]
    pub fn with_metadata(mut self, metadata: MuxMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Queue muxer-private options — `-movflags` and the like — applied one
    /// by one through [`Muxer::set_option`] before [`Muxer::init`] runs (M29,
    /// gap 5).
    ///
    /// # Errors
    /// Not here: a name this muxer does not recognise fails at
    /// [`MuxBuilder::open`], the point the caller can still act on it.
    #[must_use]
    pub fn with_private_options(mut self, options: Vec<(String, String)>) -> Self {
        self.options = options;
        self
    }

    /// The container's flags.
    #[must_use]
    pub const fn flags(&self) -> FormatFlags {
        self.flags
    }

    /// Streams declared so far.
    #[must_use]
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Declare a stream, in `input_time_base`.
    ///
    /// # Why the input time base is a parameter here
    ///
    /// M1 rescales every packet from the base it arrives in to the base the
    /// container chose, and both halves have to come from somewhere.
    /// [`Muxer::add_stream`] takes only [`CodecParameters`], which carry no
    /// time base, so a `write_packet` that did not know the input base would
    /// have to take it per call — where it can be forgotten, or supplied
    /// inconsistently between two packets of the same stream. Stating it once,
    /// at the point the stream is declared, makes that unrepresentable.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] past `max_streams` (M14);
    /// [`Error::Unsupported`] when the container will not carry the codec at
    /// the configured `strict` level (M15); whatever [`Muxer::add_stream`]
    /// returns.
    pub fn add_stream(
        &mut self,
        params: &CodecParameters,
        input_time_base: TimeBase,
    ) -> Result<u32> {
        // M14 — the same cap the demux side enforces. A muxer driven from a
        // crafted file gets its stream count from that file.
        let cap = u64::try_from(self.opts.max_streams).unwrap_or(0);
        let have = u64::try_from(self.streams.len()).unwrap_or(u64::MAX);
        if have >= cap {
            return Err(Error::LimitExceeded {
                limit: "max_streams",
                requested: have.saturating_add(1),
                cap,
            });
        }
        // M15 — ask the container before asking it to write anything.
        if let Some(codec) = params.codec_id {
            let support = self.muxer.query_codec(codec, self.opts.strict);
            if !support.permitted_at(self.opts.strict) {
                return Err(if support == CodecSupport::Experimental {
                    Error::Unsupported(
                        "this container carries this codec only with -strict experimental",
                    )
                } else {
                    Error::Unsupported("this container cannot carry this codec")
                });
            }
        }
        // Gap 9: hand the input time base down through `add_stream_with`,
        // not just the plain `add_stream`. Every existing `Muxer` still gets
        // exactly the call it always did — the default `add_stream_with`
        // forwards straight to `add_stream`, ignoring `spec` — so this is
        // additive; only a muxer that overrides `add_stream_with` (today:
        // `vaco-mux-hash`'s `framecrc`/`framemd5`/`framehash`, which print a
        // `#tb` line and cannot answer it correctly from `CodecParameters`
        // alone — see `CONFORMANCE-FINDINGS.md` 32) sees a different value.
        let spec = StreamSpec {
            time_base: Some(input_time_base),
        };
        let index = self.muxer.add_stream_with(params, &spec)?;
        // A muxer that renumbers is telling us something we cannot honour:
        // every table in the session is indexed by position.
        if usize::try_from(index).ok() != Some(self.streams.len()) {
            return Err(Error::InvalidData(
                "muxer returned a stream index out of sequence",
            ));
        }
        self.streams.push(StreamState {
            params: params.clone(),
            input_time_base,
            output_time_base: input_time_base,
            ended: false,
            packets: 0,
            bsf: BsfChain::default(),
        });
        Ok(index)
    }

    /// Run `init`, then write the header, and move to the packet phase.
    ///
    /// Consumes `self`: M9 is that the header is written exactly once, and the
    /// simplest way to say "exactly once" in a type is to make the value that
    /// could say it again unavailable.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a stream-less file in a container that
    /// requires streams (M13); [`Error::Unsupported`] for an experimental
    /// container below the required `strict` level (M17); whatever
    /// [`Muxer::init`] or [`Muxer::write_header`] returns.
    ///
    /// On failure the session is dropped and the partial output abandoned:
    /// a file whose header did not write has nothing worth finalising, and
    /// keeping the muxer alive would only offer a caller the chance to write a
    /// trailer onto it.
    pub fn open(mut self) -> Result<MuxWriter> {
        // M17 — the container itself is the experimental thing.
        if self.flags.contains(FormatFlags::EXPERIMENTAL) && self.opts.strict > -2 {
            return Err(Error::Unsupported(
                "this container is experimental; pass -strict experimental to write it",
            ));
        }
        // M13 — an empty file is only valid where the container says so.
        if self.streams.is_empty() && !self.flags.allows_no_streams() {
            return Err(Error::InvalidData(
                "this container needs at least one stream",
            ));
        }

        // M29 — muxer-private options land before init, so a flag like
        // `-movflags` can still change what init decides.
        for (name, value) in &self.options {
            self.muxer.set_option(name, value)?;
        }

        // M12 — init may rewrite time bases, so it runs before we read them.
        self.muxer.init()?;
        for (i, st) in self.streams.iter_mut().enumerate() {
            let idx = u32::try_from(i).unwrap_or(u32::MAX);
            // A muxer with no opinion keeps the caller's base, so M1 is the
            // identity. `Muxer::stream_time_base`'s own doc offers
            // [`TIME_BASE_Q`] as the fallback, and that is the worse of the
            // two: rescaling a 1/90000 stream into microseconds when nobody
            // asked for it can only lose ticks, and the container that would
            // have stored them exactly is precisely the one that declined to
            // state a preference. `TIME_BASE_Q` remains the fallback when the
            // caller's own base is unusable.
            st.output_time_base = self
                .muxer
                .stream_time_base(idx)
                .filter(|tb| tb.is_defined() && !tb.is_zero())
                .or(Some(st.input_time_base))
                .filter(|tb| tb.is_defined() && !tb.is_zero())
                .unwrap_or(TIME_BASE_Q);
        }

        // Same point as M30 below: `FormatOptions` was always known here, but
        // never handed to the muxer itself before `Muxer::set_bitexact`
        // existed (see its doc comment) — `vaco-mux-hash`'s `#software` line
        // is the first caller.
        self.muxer
            .set_bitexact(self.opts.fflags.contains(FFlags::BITEXACT));

        // M30 — metadata reaches the muxer after time bases are settled but
        // before the header, the same point M12 settles anything else that
        // depends on the whole stream set.
        self.muxer.set_metadata(&self.metadata)?;

        self.muxer.write_header()?;

        let n = self.streams.len();
        let mut queue = InterleaveQueue::new(n, &self.opts);
        if self.flags.has_no_timestamps() {
            queue = queue.without_timestamps();
        }
        for (i, st) in self.streams.iter().enumerate() {
            let idx = u32::try_from(i).unwrap_or(u32::MAX);
            queue.set_time_base(idx, st.output_time_base);
            // N5's `audio_preload` biases audio only; the session knows the
            // media type and the queue does not.
            queue.set_preloaded(
                idx,
                st.params.effective_media_type() == Some(StreamType::Audio),
            );
        }
        let chain = MuxTimestamps::new(n, self.flags, &self.opts);
        // M19 — either spelling of "do not buffer" means the same thing here.
        // `max_interleave_delta == 0` deliberately does *not* join them: the
        // reference's reading of zero there is unmeasured, and the queue's own
        // sparse escape already fires on every packet at that setting.
        let flush_each =
            self.opts.flush_packets == 1 || self.opts.fflags.contains(FFlags::FLUSH_PACKETS);
        Ok(MuxWriter {
            muxer: self.muxer,
            opts: self.opts,
            flags: self.flags,
            streams: self.streams,
            bsfs: self.bsfs,
            queue,
            chain,
            flush_each,
            report: MuxReport {
                per_stream_packets: vec![0; n],
                bitstream_filters: vec![Vec::new(); n],
                ..MuxReport::default()
            },
            scratch: Vec::new(),
        })
    }
}

/// Phase two: write packets (M10), then finalise (M11).
///
/// Has no `add_stream`, by construction. [`MuxWriter::finish`] consumes it, so
/// the trailer cannot be written twice and no packet can follow it.
pub struct MuxWriter {
    muxer: Box<dyn Muxer>,
    opts: FormatOptions,
    flags: FormatFlags,
    streams: Vec<StreamState>,
    bsfs: Arc<dyn BsfProvider>,
    queue: InterleaveQueue,
    chain: MuxTimestamps,
    flush_each: bool,
    report: MuxReport,
    /// Reused between calls so the packet path allocates nothing steady-state.
    scratch: Vec<Packet>,
}

impl core::fmt::Debug for MuxWriter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MuxWriter")
            .field("flags", &self.flags)
            .field("streams", &self.streams.len())
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl MuxWriter {
    /// The container's flags.
    #[must_use]
    pub const fn flags(&self) -> FormatFlags {
        self.flags
    }

    /// Statistics so far. The same shape [`MuxWriter::finish`] returns.
    #[must_use]
    pub fn report(&self) -> &MuxReport {
        &self.report
    }

    /// The time base the container chose for `stream_index` (M12).
    #[must_use]
    pub fn output_time_base(&self, stream_index: u32) -> Option<TimeBase> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map(|s| s.output_time_base)
    }

    /// Whether volatile fields must be suppressed (M24).
    ///
    /// `start_time_realtime` is a wall clock, so writing it makes two runs of
    /// the same command produce different bytes. `+bitexact` is the switch that
    /// says not to, and the muxer asks here rather than re-deriving it.
    #[must_use]
    pub const fn is_bitexact(&self) -> bool {
        self.opts.fflags.contains(FFlags::BITEXACT)
    }

    /// The wall clock to stamp on the file, or `None` under `+bitexact` (M24).
    #[must_use]
    pub const fn start_time_realtime(&self) -> Option<i64> {
        if self.is_bitexact() || self.opts.start_time_realtime == i64::MIN {
            None
        } else {
            Some(self.opts.start_time_realtime)
        }
    }

    /// Bytes of padding to reserve in the metadata header, if asked (M25).
    #[must_use]
    pub const fn metadata_header_padding(&self) -> Option<i32> {
        if self.opts.metadata_header_padding < 0 {
            None
        } else {
            Some(self.opts.metadata_header_padding)
        }
    }

    /// Write one packet through the whole chain: M1–M4, M5, M6, M7.
    ///
    /// `pkt` is in the time base declared for its stream at
    /// [`MuxBuilder::add_stream`]; everything after that is this function's
    /// business.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a packet naming an undeclared stream (M21) or
    /// a stream already ended (M22), and for a DTS that does not advance as the
    /// container requires (M4); whatever the chain, the filters or the muxer
    /// return.
    pub fn write_packet(&mut self, pkt: Packet) -> Result<()> {
        let idx = self.check_stream(pkt.stream_index)?;
        let mut pkt = pkt;
        let (from, to) = {
            let Some(st) = self.streams.get(idx) else {
                return Err(Error::InvalidData("packet names an undeclared stream"));
            };
            (st.input_time_base, st.output_time_base)
        };
        // M1–M4.
        self.chain.apply(&mut pkt, from, to)?;
        // M5 — the muxer's own policy, defaulting to per-DTS.
        let ready = self.muxer.interleave(&mut self.queue, Some(pkt), false)?;
        if let Some(out) = ready {
            self.emit(out)?;
        }
        if self.flush_each {
            self.drain_ready(true)?;
            self.write_flush_marker()?;
        }
        Ok(())
    }

    /// The N6 non-interleaved path: write this packet now, in this order.
    ///
    /// The caller owns DTS ordering; M4 still applies, so a caller that gets it
    /// wrong is told rather than silently producing an unplayable file. This is
    /// what low-latency muxing and the segmenting muxers use internally.
    ///
    /// # Errors
    ///
    /// As [`MuxWriter::write_packet`].
    pub fn write_frame(&mut self, pkt: Packet) -> Result<()> {
        let idx = self.check_stream(pkt.stream_index)?;
        let mut pkt = pkt;
        let (from, to) = {
            let Some(st) = self.streams.get(idx) else {
                return Err(Error::InvalidData("packet names an undeclared stream"));
            };
            (st.input_time_base, st.output_time_base)
        };
        self.chain.apply(&mut pkt, from, to)?;
        self.emit(pkt)
    }

    /// Declare a stream finished (M27 / N4).
    ///
    /// The queue then interleaves what remains among the survivors, so a short
    /// audio track does not stall a long video one at the end of a file. Any
    /// later packet on that stream is refused (M22).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for an undeclared stream; whatever draining the
    /// queue returns.
    pub fn end_stream(&mut self, stream_index: u32) -> Result<()> {
        let idx = self.check_stream(stream_index)?;
        if let Some(st) = self.streams.get_mut(idx) {
            st.ended = true;
        }
        self.queue.end_stream(stream_index);
        self.drain_ready(false)?;
        // M6 runs *after* the queue, so a filter holding the tail of the
        // stream flushes straight to the muxer rather than back into a queue
        // that has already been told this stream is over.
        self.flush_bitstream(idx)
    }

    /// Emit everything the queue is willing to give up, and tell the muxer to
    /// flush if it can (M20).
    ///
    /// # Errors
    ///
    /// As [`MuxWriter::write_packet`].
    pub fn flush(&mut self) -> Result<()> {
        self.drain_ready(true)?;
        self.write_flush_marker()
    }

    /// Drain the queue, write the trailer, and report (M11).
    ///
    /// Consumes `self`. There is no second trailer and no packet after it, for
    /// the same reason there is no second header.
    ///
    /// # Errors
    ///
    /// Whatever draining or [`Muxer::write_trailer`] returns.
    pub fn finish(mut self) -> Result<MuxReport> {
        for i in 0..self.streams.len() {
            let idx = u32::try_from(i).unwrap_or(u32::MAX);
            if self.streams.get(i).is_some_and(|s| !s.ended) {
                self.end_stream(idx)?;
            }
        }
        self.drain_ready(true)?;
        self.muxer.write_trailer()?;
        self.report.trailer_written = true;
        self.report.avoid_negative_ts = self.chain.policy();
        self.report.ts_offset_us = self.chain.offset_us();
        Ok(self.report)
    }

    /// Give up without writing a trailer (M28).
    ///
    /// Returns the muxer and a report whose `trailer_written` is false, so a
    /// caller cleaning up after a failed run can tell a finished file from an
    /// abandoned one — which is the difference between "delete this" and "keep
    /// this". Buffered packets are discarded.
    #[must_use]
    pub fn abort(mut self) -> (Box<dyn Muxer>, MuxReport) {
        self.report.avoid_negative_ts = self.chain.policy();
        self.report.ts_offset_us = self.chain.offset_us();
        (self.muxer, self.report)
    }

    /// M21 / M22 in one place.
    fn check_stream(&self, stream_index: u32) -> Result<usize> {
        let idx = usize::try_from(stream_index)
            .ok()
            .filter(|&i| i < self.streams.len())
            .ok_or(Error::InvalidData("packet names an undeclared stream"))?;
        if self.streams.get(idx).is_some_and(|s| s.ended) {
            return Err(Error::InvalidData(
                "packet arrived on a stream already declared finished",
            ));
        }
        Ok(idx)
    }

    /// Pull from the queue until it stops offering, then hand each packet on.
    fn drain_ready(&mut self, flush: bool) -> Result<()> {
        loop {
            let Some(p) = self.muxer.interleave(&mut self.queue, None, flush)? else {
                return Ok(());
            };
            self.emit(p)?;
        }
    }

    /// M6 then M7: filter, then write.
    fn emit(&mut self, pkt: Packet) -> Result<()> {
        let idx = usize::try_from(pkt.stream_index)
            .ok()
            .filter(|&i| i < self.streams.len())
            .ok_or(Error::InvalidData("packet names an undeclared stream"))?;
        self.decide_bitstream(idx, &pkt)?;

        let mut out = core::mem::take(&mut self.scratch);
        out.clear();
        let filtered = match self.streams.get_mut(idx) {
            Some(st) => st.bsf.filter(pkt, &mut out),
            None => Err(Error::InvalidData("packet names an undeclared stream")),
        };
        let result = filtered.and_then(|()| self.write_out(idx, &mut out));
        self.scratch = out;
        result
    }

    /// M7 plus M26's bookkeeping. Everything here has already been filtered.
    fn write_out(&mut self, idx: usize, packets: &mut Vec<Packet>) -> Result<()> {
        for p in packets.drain(..) {
            self.report.packets = self.report.packets.saturating_add(1);
            self.report.payload_bytes = self
                .report
                .payload_bytes
                .saturating_add(u64::try_from(p.len).unwrap_or(u64::MAX));
            if let Some(slot) = self.report.per_stream_packets.get_mut(idx) {
                *slot = slot.saturating_add(1);
            }
            if let Some(st) = self.streams.get_mut(idx) {
                st.packets = st.packets.saturating_add(1);
            }
            self.muxer.write_packet(&p)?;
        }
        Ok(())
    }

    /// Push the end-of-stream marker through a stream's filter chain.
    fn flush_bitstream(&mut self, idx: usize) -> Result<()> {
        let mut out = Vec::new();
        match self.streams.get_mut(idx) {
            Some(st) if st.bsf.is_decided() => st.bsf.flush(&mut out)?,
            _ => return Ok(()),
        }
        self.write_out(idx, &mut out)
    }

    /// B1–B3: ask once per stream, chain up to [`MAX_BSF_DEPTH`], then cache.
    fn decide_bitstream(&mut self, idx: usize, pkt: &Packet) -> Result<()> {
        if self.streams.get(idx).is_some_and(|s| s.bsf.is_decided()) {
            return Ok(());
        }
        // B1 — `-fflags -autobsf` disables the stage entirely.
        if !self.opts.fflags.contains(FFlags::AUTOBSF) {
            if let Some(st) = self.streams.get_mut(idx) {
                st.bsf.decided = true;
            }
            return Ok(());
        }
        let params = match self.streams.get(idx) {
            Some(st) => st.params.clone(),
            None => return Err(Error::InvalidData("packet names an undeclared stream")),
        };
        let mut filters: Vec<Box<dyn BitstreamFilter>> = Vec::new();
        let mut names: Vec<&'static str> = Vec::new();
        for _ in 0..MAX_BSF_DEPTH {
            let action = self.muxer.check_bitstream(&params, pkt)?;
            let BitstreamAction::Insert { name } = action else {
                break;
            };
            // A muxer asking for the same filter twice is a loop, not a chain.
            if names.contains(&name) {
                return Err(Error::InvalidData(
                    "muxer asked for the same bitstream filter twice",
                ));
            }
            filters.push(self.bsfs.open(name, &params)?);
            names.push(name);
            // B2 asks again "on the filter's output". We cannot see that output
            // without running the filter, and running it here would consume the
            // packet before the queue had ordered it, so the muxer is re-asked
            // against the same parameters. The duplicate-name check below is
            // what stops that from looping: a muxer that would answer `Insert`
            // forever answers with the same name forever.
        }
        if names.len() >= MAX_BSF_DEPTH {
            // Still asking after four is a muxer that will never say Keep.
            if self.muxer.check_bitstream(&params, pkt)? != BitstreamAction::Keep {
                return Err(Error::InvalidData(
                    "bitstream filter chain did not terminate within the depth limit",
                ));
            }
        }
        if let Some(slot) = self.report.bitstream_filters.get_mut(idx) {
            slot.clone_from(&names);
        }
        if let Some(st) = self.streams.get_mut(idx) {
            st.bsf.filters = filters;
            st.bsf.names = names;
            st.bsf.decided = true;
        }
        Ok(())
    }

    /// M20 — only a muxer that declared `ALLOW_FLUSH` is told about a flush.
    fn write_flush_marker(&mut self) -> Result<()> {
        if self.flags.allows_flush() {
            self.muxer.write_flush()?;
        }
        Ok(())
    }
}

/// The default `check_bitstream` answer for a `GLOBALHEADER` container (M16).
///
/// A muxer that wants extradata out of band and is handed a stream that has
/// none needs `extract_extradata` inserted; the condition is the same for every
/// such container, so it is written once here rather than in each of them.
/// A muxer calls this from its own `check_bitstream` when it has no more
/// specific opinion.
#[must_use]
pub fn global_header_action(flags: FormatFlags, params: &CodecParameters) -> BitstreamAction {
    if flags.wants_global_header() && params.extradata.as_ref().is_none_or(Vec::is_empty) {
        BitstreamAction::Insert {
            name: "extract_extradata",
        }
    } else {
        BitstreamAction::Keep
    }
}

/// A muxer's static answer to "can you carry this?", for the common case of a
/// fixed allow-list.
#[must_use]
pub fn codec_in(list: &[CodecId], codec: CodecId) -> CodecSupport {
    if list.contains(&codec) {
        CodecSupport::Supported
    } else {
        CodecSupport::Unsupported
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::field_reassign_with_default,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::metadata::MuxAttachment;
    use std::sync::Mutex;
    use vaco_core::{Rational, Timestamp};
    use vaco_limits::{Budget, Limits};

    /// A muxer that records what it was told, in order.
    ///
    /// The log is shared rather than owned so a test can still read it after
    /// the muxer has been moved into a `Box<dyn Muxer>` — which every test
    /// here does, because that is how a real caller holds one.
    #[derive(Debug, Default)]
    struct Recorder {
        flags: FormatFlags,
        log: Arc<Mutex<Vec<String>>>,
        streams: usize,
        time_base: Option<Rational>,
        support: Option<CodecSupport>,
        bsf_asks: Vec<BitstreamAction>,
        ask_count: usize,
        fail_header: bool,
    }

    impl Recorder {
        fn say(&self, what: String) {
            if let Ok(mut g) = self.log.lock() {
                g.push(what);
            }
        }
    }

    fn log_of(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// A bitstream filter that passes packets straight through, so the chain
    /// machinery can be tested without a real filter crate.
    #[derive(Default)]
    struct PassThrough {
        held: Vec<Packet>,
    }

    impl BitstreamFilter for PassThrough {
        fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
            if let Some(p) = packet {
                self.held.push(p.clone());
            }
            Ok(())
        }
        fn receive_packet(&mut self) -> Result<Packet> {
            self.held.pop().ok_or(Error::NeedMoreInput)
        }
    }

    struct PassThroughProvider;

    impl BsfProvider for PassThroughProvider {
        fn open(&self, _name: &str, _params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
            Ok(Box::new(PassThrough::default()))
        }
    }

    impl Muxer for Recorder {
        fn flags(&self) -> FormatFlags {
            self.flags
        }
        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            let i = self.streams as u32;
            self.streams += 1;
            self.say(format!("add_stream {i}"));
            Ok(i)
        }
        fn init(&mut self) -> Result<()> {
            self.say("init".to_owned());
            Ok(())
        }
        fn write_header(&mut self) -> Result<()> {
            self.say("header".to_owned());
            if self.fail_header {
                return Err(Error::InvalidData("header refused"));
            }
            Ok(())
        }
        fn write_packet(&mut self, packet: &Packet) -> Result<()> {
            self.say(format!(
                "pkt s{} dts{:?}",
                packet.stream_index,
                packet.dts.ticks()
            ));
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            self.say("trailer".to_owned());
            Ok(())
        }
        fn write_flush(&mut self) -> Result<()> {
            self.say("flush".to_owned());
            Ok(())
        }
        fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
            self.time_base
        }
        fn query_codec(&self, _codec: CodecId, _strict: i32) -> CodecSupport {
            self.support.unwrap_or_default()
        }
        fn check_bitstream(
            &mut self,
            _params: &CodecParameters,
            _pkt: &Packet,
        ) -> Result<BitstreamAction> {
            let a = self
                .bsf_asks
                .get(self.ask_count)
                .copied()
                .unwrap_or(BitstreamAction::Keep);
            self.ask_count += 1;
            Ok(a)
        }
    }

    /// A muxer that overrides only [`Muxer::set_metadata`] and
    /// [`Muxer::set_option`], logging every call alongside the phases it
    /// still gets from the trait's other defaults and from its own minimal
    /// required methods — so the log's order proves M29/M30's placement
    /// relative to `init` and the header, not just that the calls happened.
    #[derive(Debug, Default)]
    struct ConfigurableMuxer {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Muxer for ConfigurableMuxer {
        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            Ok(0)
        }
        fn init(&mut self) -> Result<()> {
            if let Ok(mut g) = self.log.lock() {
                g.push("init".to_owned());
            }
            Ok(())
        }
        fn write_header(&mut self) -> Result<()> {
            if let Ok(mut g) = self.log.lock() {
                g.push("header".to_owned());
            }
            Ok(())
        }
        fn write_packet(&mut self, _packet: &Packet) -> Result<()> {
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            if let Ok(mut g) = self.log.lock() {
                g.push("trailer".to_owned());
            }
            Ok(())
        }
        fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
            if let Ok(mut g) = self.log.lock() {
                g.push(format!(
                    "metadata tags={} chapters={} attachments={} stream0_tags={}",
                    metadata.tags.len(),
                    metadata.chapters.len(),
                    metadata.attachments.len(),
                    metadata.tags_for_stream(0).len(),
                ));
            }
            Ok(())
        }
        fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
            if name == "known" {
                if let Ok(mut g) = self.log.lock() {
                    g.push(format!("option {name}={value}"));
                }
                Ok(())
            } else {
                Err(Error::Option {
                    name: name.to_owned(),
                    detail: "unknown".to_owned(),
                })
            }
        }
    }

    fn pkt(stream: u32, dts: i64) -> Packet {
        let mut budget = Budget::new(Limits::strict());
        let mut p = Packet::from_slice(&mut budget, b"payload").unwrap();
        p.stream_index = stream;
        p.dts = Timestamp::new(dts);
        p.pts = p.dts;
        p
    }

    fn video() -> CodecParameters {
        CodecParameters::video().with_codec(CodecId::H264)
    }

    fn tb() -> Rational {
        Rational::new(1, 1000)
    }

    #[test]
    fn the_phases_run_in_order_and_only_once() {
        let opts = FormatOptions::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let rec = Recorder {
            log: Arc::clone(&log),
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        w.write_packet(pkt(0, 0)).unwrap();
        w.write_packet(pkt(0, 10)).unwrap();
        let report = w.finish().unwrap();
        assert_eq!(report.packets, 2);
        assert!(report.trailer_written);
        assert_eq!(
            log_of(&log),
            vec![
                "add_stream 0",
                "init",
                "header",
                "pkt s0 dtsSome(0)",
                "pkt s0 dtsSome(10)",
                "trailer",
            ]
        );
        // A second header or a second trailer is not a runtime failure here:
        // `open` and `finish` consumed the values that could ask for one, so
        // neither has a spelling that compiles.
    }

    #[test]
    fn init_runs_before_the_header_and_its_time_base_is_used() {
        let opts = FormatOptions::default();
        let rec = Recorder {
            time_base: Some(Rational::new(1, 90_000)),
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        assert_eq!(w.output_time_base(0), Some(Rational::new(1, 90_000)));
        // 10 ms at 1/1000 is 900 ticks at 1/90000. M1 did the rescale, and it
        // used the base `init` settled on, not the one the caller declared.
        w.write_packet(pkt(0, 10)).unwrap();
        w.finish().unwrap();
    }

    #[test]
    fn a_container_that_needs_streams_refuses_an_empty_file() {
        let opts = FormatOptions::default();
        let b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        assert!(b.open().is_err());

        let rec = Recorder {
            flags: FormatFlags::NOSTREAMS,
            ..Recorder::default()
        };
        let b = MuxBuilder::new(Box::new(rec), &opts);
        assert!(b.open().is_ok());
    }

    #[test]
    fn max_streams_caps_the_mux_side() {
        let mut opts = FormatOptions::default();
        opts.max_streams = 2;
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        b.add_stream(&video(), tb()).unwrap();
        assert!(b.add_stream(&video(), tb()).is_err());
    }

    #[test]
    fn an_unsupported_codec_is_refused_at_add_stream() {
        let opts = FormatOptions::default();
        let rec = Recorder {
            support: Some(CodecSupport::Unsupported),
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        assert!(b.add_stream(&video(), tb()).is_err());
    }

    #[test]
    fn an_experimental_codec_needs_the_strict_level() {
        let mut opts = FormatOptions::default();
        let rec = Recorder {
            support: Some(CodecSupport::Experimental),
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        assert!(b.add_stream(&video(), tb()).is_err());

        opts.strict = -2;
        let rec = Recorder {
            support: Some(CodecSupport::Experimental),
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        assert!(b.add_stream(&video(), tb()).is_ok());
    }

    #[test]
    fn an_experimental_container_needs_the_strict_level() {
        let opts = FormatOptions::default();
        let rec = Recorder {
            flags: FormatFlags::EXPERIMENTAL,
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        assert!(b.open().is_err());
    }

    #[test]
    fn a_packet_on_an_undeclared_or_finished_stream_is_refused() {
        let opts = FormatOptions::default();
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        assert!(w.write_packet(pkt(7, 0)).is_err());
        w.write_packet(pkt(0, 0)).unwrap();
        w.end_stream(0).unwrap();
        assert!(w.write_packet(pkt(0, 10)).is_err());
    }

    #[test]
    fn flush_packets_writes_each_packet_immediately() {
        let mut opts = FormatOptions::default();
        opts.flush_packets = 1;
        let rec = Recorder {
            flags: FormatFlags::ALLOW_FLUSH,
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        // Stream 1 has nothing queued, so a buffering queue would hold this.
        w.write_packet(pkt(0, 0)).unwrap();
        assert_eq!(w.report().packets, 1, "flush_packets did not flush");
        w.finish().unwrap();
    }

    #[test]
    fn a_flush_marker_only_reaches_a_muxer_that_asked_for_one() {
        let mut opts = FormatOptions::default();
        opts.flush_packets = 1;
        for (flags, want) in [(FormatFlags::empty(), 0), (FormatFlags::ALLOW_FLUSH, 2)] {
            let log = Arc::new(Mutex::new(Vec::new()));
            let rec = Recorder {
                flags,
                log: Arc::clone(&log),
                ..Recorder::default()
            };
            let mut b = MuxBuilder::new(Box::new(rec), &opts);
            b.add_stream(&video(), tb()).unwrap();
            let mut w = b.open().unwrap();
            w.write_packet(pkt(0, 0)).unwrap();
            w.write_packet(pkt(0, 10)).unwrap();
            let flushes = log_of(&log).iter().filter(|e| *e == "flush").count();
            assert_eq!(flushes, want, "flags {flags}");
            let _ = w.abort();
        }
    }

    #[test]
    fn aborting_writes_no_trailer() {
        let opts = FormatOptions::default();
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        w.write_packet(pkt(0, 0)).unwrap();
        let (_muxer, report) = w.abort();
        assert!(!report.trailer_written);
    }

    #[test]
    fn the_report_carries_the_resolved_shift() {
        let opts = FormatOptions::default();
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        w.write_packet(pkt(0, -250)).unwrap();
        let r = w.finish().unwrap();
        assert_eq!(r.avoid_negative_ts, AvoidNegativeTs::MakeNonNegative);
        assert_eq!(r.ts_offset_us, Some(250_000));
    }

    #[test]
    fn autobsf_off_asks_nothing() {
        let mut opts = FormatOptions::default();
        opts.fflags.remove(FFlags::AUTOBSF);
        let rec = Recorder {
            bsf_asks: vec![BitstreamAction::Insert {
                name: "extract_extradata",
            }],
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        // With no provider, an honoured request would fail. It is not honoured.
        w.write_packet(pkt(0, 0)).unwrap();
        let r = w.finish().unwrap();
        assert!(r.bitstream_filters[0].is_empty());
    }

    #[test]
    fn a_requested_filter_with_no_provider_is_an_error_not_a_silent_pass() {
        let opts = FormatOptions::default();
        let rec = Recorder {
            bsf_asks: vec![BitstreamAction::Insert {
                name: "extract_extradata",
            }],
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        assert!(w.write_packet(pkt(0, 0)).is_err());
    }

    #[test]
    fn a_muxer_asking_for_the_same_filter_twice_is_a_loop() {
        let opts = FormatOptions::default();
        let ask = BitstreamAction::Insert {
            name: "extract_extradata",
        };
        let rec = Recorder {
            bsf_asks: vec![ask, ask],
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts).with_bsfs(Arc::new(PassThroughProvider));
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        // With a working provider the first insert succeeds, so the failure
        // that follows is the loop check and nothing else.
        assert!(w.write_packet(pkt(0, 0)).is_err());
    }

    #[test]
    fn an_inserted_filter_sits_between_the_queue_and_the_muxer() {
        let opts = FormatOptions::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let rec = Recorder {
            log: Arc::clone(&log),
            bsf_asks: vec![BitstreamAction::Insert {
                name: "passthrough",
            }],
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts).with_bsfs(Arc::new(PassThroughProvider));
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        w.write_packet(pkt(0, 0)).unwrap();
        w.write_packet(pkt(0, 10)).unwrap();
        let r = w.finish().unwrap();
        assert_eq!(r.bitstream_filters[0], vec!["passthrough"]);
        assert_eq!(r.packets, 2);
        assert!(log_of(&log).contains(&"pkt s0 dtsSome(10)".to_owned()));
    }

    #[test]
    fn the_chain_stops_at_the_depth_limit() {
        let opts = FormatOptions::default();
        let rec = Recorder {
            bsf_asks: vec![
                BitstreamAction::Insert { name: "a" },
                BitstreamAction::Insert { name: "b" },
                BitstreamAction::Insert { name: "c" },
                BitstreamAction::Insert { name: "d" },
                BitstreamAction::Insert { name: "e" },
            ],
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts).with_bsfs(Arc::new(PassThroughProvider));
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        assert!(
            w.write_packet(pkt(0, 0)).is_err(),
            "a chain that never says Keep must be refused, not stacked forever"
        );
    }

    #[test]
    fn the_non_interleaved_path_writes_in_call_order() {
        let opts = FormatOptions::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let rec = Recorder {
            log: Arc::clone(&log),
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        // Stream 1 never speaks, so the interleaving path would buffer all of
        // these. N6 says the caller owns the order.
        w.write_frame(pkt(0, 0)).unwrap();
        w.write_frame(pkt(1, 5)).unwrap();
        w.write_frame(pkt(0, 10)).unwrap();
        assert_eq!(w.report().packets, 3);
        assert_eq!(
            log_of(&log)
                .iter()
                .filter(|e| e.starts_with("pkt"))
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "pkt s0 dtsSome(0)",
                "pkt s1 dtsSome(5)",
                "pkt s0 dtsSome(10)"
            ]
        );
        w.finish().unwrap();
    }

    #[test]
    fn global_header_asks_for_extradata_only_when_it_is_missing() {
        let mut p = video();
        assert_eq!(
            global_header_action(FormatFlags::GLOBALHEADER, &p),
            BitstreamAction::Insert {
                name: "extract_extradata"
            }
        );
        p.extradata = Some(vec![1, 2, 3]);
        assert_eq!(
            global_header_action(FormatFlags::GLOBALHEADER, &p),
            BitstreamAction::Keep
        );
        assert_eq!(
            global_header_action(FormatFlags::empty(), &video()),
            BitstreamAction::Keep
        );
    }

    #[test]
    fn bitexact_suppresses_the_wall_clock() {
        let mut opts = FormatOptions::default();
        opts.start_time_realtime = 1_700_000_000_000_000;
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let w = b.open().unwrap();
        assert_eq!(w.start_time_realtime(), Some(1_700_000_000_000_000));

        opts.fflags.insert(FFlags::BITEXACT);
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        let w = b.open().unwrap();
        assert_eq!(w.start_time_realtime(), None);
    }

    #[test]
    fn codec_support_gates_on_the_strict_level() {
        assert!(CodecSupport::Supported.permitted_at(0));
        assert!(!CodecSupport::Experimental.permitted_at(0));
        assert!(CodecSupport::Experimental.permitted_at(-2));
        assert!(!CodecSupport::Unsupported.permitted_at(-2));
    }

    #[test]
    fn finishing_drains_every_stream() {
        let opts = FormatOptions::default();
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts);
        b.add_stream(&video(), tb()).unwrap();
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        // Stream 1 never speaks; a naive drain would leave stream 0 queued.
        for i in 0..4i64 {
            w.write_packet(pkt(0, i * 10)).unwrap();
        }
        let r = w.finish().unwrap();
        assert_eq!(r.packets, 4);
        assert_eq!(r.per_stream_packets, vec![4, 0]);
    }

    #[test]
    fn a_failed_header_does_not_reach_the_packet_phase() {
        let opts = FormatOptions::default();
        let rec = Recorder {
            fail_header: true,
            ..Recorder::default()
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts);
        b.add_stream(&video(), tb()).unwrap();
        assert!(b.open().is_err());
    }

    // --------------------------------------------------- gap 1: set_metadata

    /// The default does the harmless thing: a muxer that does not override
    /// [`Muxer::set_metadata`] simply drops whatever [`MuxBuilder::with_metadata`]
    /// supplied, exactly as if the channel did not exist — which, before this
    /// gap closed, it did not.
    #[test]
    fn set_metadata_default_silently_drops_it() {
        let opts = FormatOptions::default();
        let mut meta = MuxMetadata::default();
        meta.tags
            .push(("title".to_owned(), "not written anywhere".to_owned()));
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts).with_metadata(meta);
        b.add_stream(&video(), tb()).unwrap();
        let mut w = b.open().unwrap();
        w.write_packet(pkt(0, 0)).unwrap();
        assert!(w.finish().is_ok());
    }

    /// An override receives exactly what [`MuxBuilder::with_metadata`] supplied,
    /// at the point M30 promises: after `init`, before the header.
    #[test]
    fn an_override_receives_the_supplied_metadata_before_the_header() {
        let opts = FormatOptions::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let rec = ConfigurableMuxer {
            log: Arc::clone(&log),
        };
        let mut meta = MuxMetadata::default();
        meta.tags.push(("title".to_owned(), "x".to_owned()));
        meta.chapters.push(crate::Chapter {
            id: 0,
            time_base: tb(),
            start: Timestamp::new(0),
            end: Timestamp::new(1000),
            metadata: vec![("title".to_owned(), "chapter one".to_owned())],
        });
        meta.attachments.push(MuxAttachment {
            filename: "cover.jpg".to_owned(),
            mime_type: "image/jpeg".to_owned(),
            description: String::new(),
            data: vec![1, 2, 3],
        });
        meta.stream_tags = vec![vec![("language".to_owned(), "eng".to_owned())]];
        let mut b = MuxBuilder::new(Box::new(rec), &opts).with_metadata(meta);
        b.add_stream(&video(), tb()).unwrap();
        let w = b.open().unwrap();
        drop(w);
        assert_eq!(
            log_of(&log),
            vec![
                "init".to_owned(),
                "metadata tags=1 chapters=1 attachments=1 stream0_tags=1".to_owned(),
                "header".to_owned(),
            ]
        );
    }

    // ---------------------------------------------------- gap 5: set_option

    /// The default does the safe thing: an option nobody had a channel to
    /// carry before this gap closed is refused, not silently ignored — the
    /// same philosophy [`NoBsfs`] applies to an unfulfillable bitstream-filter
    /// request.
    #[test]
    fn set_option_default_refuses_every_name() {
        let opts = FormatOptions::default();
        let mut b = MuxBuilder::new(Box::new(Recorder::default()), &opts)
            .with_private_options(vec![("movflags".to_owned(), "+faststart".to_owned())]);
        b.add_stream(&video(), tb()).unwrap();
        assert!(b.open().is_err());
    }

    /// An override applies queued options before `init` runs (M29), so a
    /// muxer's `init` can see their effect.
    #[test]
    fn an_override_applies_options_before_init() {
        let opts = FormatOptions::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let rec = ConfigurableMuxer {
            log: Arc::clone(&log),
        };
        let mut b = MuxBuilder::new(Box::new(rec), &opts)
            .with_private_options(vec![("known".to_owned(), "1".to_owned())]);
        b.add_stream(&video(), tb()).unwrap();
        let w = b.open().unwrap();
        drop(w);
        let entries = log_of(&log);
        let opt_at = entries.iter().position(|e| e == "option known=1").unwrap();
        let init_at = entries.iter().position(|e| e == "init").unwrap();
        assert!(opt_at < init_at, "{entries:?}");
    }

    /// An unrecognised option fails at [`MuxBuilder::open`], the point the
    /// caller can still act on it, rather than being dropped on the floor.
    #[test]
    fn an_override_refuses_an_unrecognised_option() {
        let opts = FormatOptions::default();
        let rec = ConfigurableMuxer::default();
        let mut b = MuxBuilder::new(Box::new(rec), &opts)
            .with_private_options(vec![("nope".to_owned(), "1".to_owned())]);
        b.add_stream(&video(), tb()).unwrap();
        assert!(b.open().is_err());
    }
}
