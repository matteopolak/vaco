//! `tee`: one input, several inner muxers.
//!
//! # What this module provides
//!
//! [`grammar`] is the pure `[opt=val:...]path|...` parser — see its module
//! docs for what was measured and reused from [`vaco_core::escape`]. This
//! module is the layer that actually fans packets out: [`TeeMuxer`] owns a
//! list of already-open inner [`vaco_format_core::Muxer`]s (one per parsed
//! output) and, per output, a stream selector and an `onfail` policy.
//!
//! # The registry seam does not fit this format
//!
//! [`vaco_format_core::MuxerDesc::open`] is `fn(Box<dyn MediaSink>) ->
//! Result<Box<dyn Muxer>>` — one sink, no URL string, no way to name even one
//! output let alone several. [`MUXER_TEE`]'s `open` therefore always returns
//! [`vaco_core::Error::Unsupported`]: there is nothing it could construct
//! that would not be a lie. [`TeeMuxer::new`] is the real constructor, for a
//! caller that has parsed a URL with [`grammar::parse`] and opened one inner
//! muxer per output itself (which needs `-f`/`f=` resolved against a muxer
//! registry this crate does not have access to — see the crate docs).
//!
//! # `select=` stream selection
//!
//! [`StreamSelector`] supports the two forms probing actually exercised: a
//! bare media-type letter (`select=v`) and a `type:index` pair
//! (`select=a:0`, confirmed via a quoted value in [`grammar`]'s probes).
//! Program-group and complex multi-clause specifiers
//! (`-map`'s full grammar) are out of scope — a selector this module cannot
//! parse is treated as "select everything", which is the safer of the two
//! wrong answers (dropping a stream from an output the user asked to keep
//! it in is a silent, harder-to-notice data loss than including an extra
//! one).
//!
//! # `onfail`
//!
//! Measured: `onfail=ignore` on one output lets [`TeeMuxer::write_header`]
//! succeed with the rest ("continuing with N/M slaves"); without it, one
//! failing output's `write_header` fails the whole tee ("aborting"). See
//! [`OnFail`].
//!
//! # What is not wired up
//!
//! `bsfs=`/`bsfs/<type>=` parses ([`grammar::TeeOutput::option`]) but is not
//! applied — bitstream filtering needs a
//! [`vaco_format_core::BsfProvider`] and a per-stream [`vaco_format_core::mux::BsfChain`],
//! which would make `TeeMuxer::new`'s signature noticeably heavier for a
//! feature no test in this crate currently exercises end to end; recorded
//! here rather than silently dropped. `use_fifo`/`fifo_options` (the tee
//! muxer's own top-level options, not part of the per-output grammar) are
//! likewise not auto-applied — a caller that wants each output fed through
//! [`crate::fifo::FifoMuxer`] wraps the inner muxer itself before handing it
//! to [`TeeMuxer::new`], since this crate already exposes that type for
//! exactly this purpose.

pub mod grammar;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::metadata::MuxMetadata;
use vaco_format_core::mux::{BitstreamAction, CodecSupport};
use vaco_format_core::{Muxer, MuxerDesc, StreamSpec};
use vaco_io::MediaSink;
use vaco_packet::Packet;

pub use grammar::{GrammarError, TeeOption, TeeOutput};

/// `select=` parsed into something this crate can test a stream against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamSelector {
    /// No `select=` option, or one this crate could not parse: everything
    /// passes.
    All,
    /// `select=v` / `select=a` / …: one media type.
    Media(MediaType),
    /// `select=a:0`: one media type, at a zero-based index *within that
    /// media type* (matching `-map`'s own `a:0` convention).
    MediaIndex(MediaType, usize),
}

impl StreamSelector {
    /// Parse a `select=` value. Never fails — an unrecognised spelling is
    /// [`StreamSelector::All`], see the module docs for why that is the
    /// safer default.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        fn media(c: char) -> Option<MediaType> {
            MediaType::ALL.into_iter().find(|m| m.specifier_char() == c)
        }
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            return Self::All;
        };
        if chars.as_str().is_empty() {
            return media(first).map_or(Self::All, Self::Media);
        }
        if let Some(rest) = value.split_once(':')
            && let Some(m) = media(first)
            && let Ok(idx) = rest.1.parse::<usize>()
        {
            return Self::MediaIndex(m, idx);
        }
        Self::All
    }

    /// Whether the stream at `global_index`, with media type `media` and
    /// the `nth`-within-its-media-type position, is selected.
    #[must_use]
    pub fn matches(&self, media: Option<MediaType>, nth_in_media: usize) -> bool {
        match self {
            Self::All => true,
            Self::Media(m) => media == Some(*m),
            Self::MediaIndex(m, idx) => media == Some(*m) && nth_in_media == *idx,
        }
    }
}

/// `onfail=`. Default [`OnFail::Abort`], matching the reference's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnFail {
    #[default]
    Abort,
    Ignore,
}

impl OnFail {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if value == "ignore" {
            Self::Ignore
        } else {
            Self::Abort
        }
    }
}

/// One output slot: the already-open inner muxer, its selector, its
/// `onfail` policy, and (once `add_stream` has run) the map from a global
/// stream index to this output's own local index.
struct Slot {
    muxer: Box<dyn Muxer>,
    selector: StreamSelector,
    on_fail: OnFail,
    /// `local_index[global_index]`, `None` where this output did not select
    /// that stream.
    local_index: Vec<Option<u32>>,
    /// Cleared by [`TeeMuxer::write_header`] on an `onfail=ignore` failure;
    /// every later call skips this slot entirely.
    alive: bool,
}

/// `tee`: fans one input out to N already-open inner muxers.
pub struct TeeMuxer {
    slots: Vec<Slot>,
    /// How many streams of each media type have been seen so far, for
    /// resolving [`StreamSelector::MediaIndex`].
    media_seen: Vec<(MediaType, usize)>,
    stream_media: Vec<Option<MediaType>>,
}

impl core::fmt::Debug for TeeMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TeeMuxer")
            .field("outputs", &self.slots.len())
            .field("alive", &self.slots.iter().filter(|s| s.alive).count())
            .finish_non_exhaustive()
    }
}

impl TeeMuxer {
    /// Build a tee from parsed outputs and one already-open inner muxer per
    /// output, in the same order.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if `outputs.len() != muxers.len()`.
    pub fn new(outputs: &[TeeOutput], muxers: Vec<Box<dyn Muxer>>) -> Result<Self> {
        if outputs.len() != muxers.len() {
            return Err(Error::Unsupported(
                "tee: one inner muxer is required per parsed output",
            ));
        }
        let slots = outputs
            .iter()
            .zip(muxers)
            .map(|(out, muxer)| Slot {
                muxer,
                selector: out
                    .option("select")
                    .map_or(StreamSelector::All, StreamSelector::parse),
                on_fail: out.option("onfail").map_or(OnFail::Abort, OnFail::parse),
                local_index: Vec::new(),
                alive: true,
            })
            .collect();
        Ok(Self {
            slots,
            media_seen: Vec::new(),
            stream_media: Vec::new(),
        })
    }
}

impl Muxer for TeeMuxer {
    fn flags(&self) -> FormatFlags {
        // The loosest reading: individual slots enforce their own
        // discipline, and this layer must not additionally reject a packet
        // one slot wants and another does not.
        FormatFlags::TS_NONSTRICT.union(FormatFlags::TS_NEGATIVE)
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        // `add_stream_with`'s default already forwards to a slot's own
        // `add_stream` when it has not overridden the wider method, so this
        // is behaviourally identical to what stood here before — it just
        // stops being a second, divergent copy of the fan-out bookkeeping.
        self.add_stream_with(params, &StreamSpec::default())
    }

    /// [`Muxer::add_stream`], plus [`StreamSpec`] — forwarded per slot rather
    /// than dropped, which is the exact "tee... has the same obligation"
    /// case `Muxer::add_stream_with`'s own doc comment names (gap 9,
    /// `planning/INTERFACE-GAPS.md`). Before this override, every tee output
    /// silently lost a stream-copy time base a slot's own muxer (a
    /// `FrameHashMuxer`, say) would otherwise have used.
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        let global_index = self.stream_media.len() as u32;
        let media = params.media_type;
        let nth = self
            .media_seen
            .iter()
            .find(|(m, _)| Some(*m) == media)
            .map_or(0, |(_, n)| *n);
        self.stream_media.push(media);
        if let Some(entry) = self.media_seen.iter_mut().find(|(m, _)| Some(*m) == media) {
            entry.1 += 1;
        } else if let Some(m) = media {
            self.media_seen.push((m, 1));
        }
        for slot in &mut self.slots {
            let selected = slot.selector.matches(media, nth);
            let local = if selected {
                slot.muxer.add_stream_with(params, spec).ok()
            } else {
                None
            };
            slot.local_index.push(local);
        }
        Ok(global_index)
    }

    /// Forwarded per slot, honouring each output's own `onfail=` policy
    /// exactly as [`Muxer::write_header`] does below — a slot whose `init`
    /// fails is exactly as dead as one whose header write fails, and treating
    /// the two differently would let a slot survive `init` failure only to
    /// be asked to write a header it was never actually settled for.
    fn init(&mut self) -> Result<()> {
        let mut any_hard_failure = None;
        for slot in &mut self.slots {
            if let Err(e) = slot.muxer.init() {
                match slot.on_fail {
                    OnFail::Ignore => slot.alive = false,
                    OnFail::Abort => {
                        any_hard_failure.get_or_insert(e);
                    }
                }
            }
        }
        any_hard_failure.map_or(Ok(()), Err)
    }

    fn write_header(&mut self) -> Result<()> {
        let mut any_hard_failure = None;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Err(e) = slot.muxer.write_header() {
                match slot.on_fail {
                    OnFail::Ignore => slot.alive = false,
                    OnFail::Abort => {
                        any_hard_failure.get_or_insert((i, e));
                    }
                }
            }
        }
        if let Some((_, e)) = any_hard_failure {
            return Err(e);
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let global = packet.stream_index as usize;
        for slot in &mut self.slots {
            if !slot.alive {
                continue;
            }
            let Some(Some(local)) = slot.local_index.get(global).copied() else {
                continue;
            };
            let mut remapped = packet.clone();
            remapped.stream_index = local;
            slot.muxer.write_packet(&remapped)?;
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        let mut first_err = None;
        for slot in &mut self.slots {
            if !slot.alive {
                continue;
            }
            if let Err(e) = slot.muxer.write_trailer() {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        // Slots may disagree; there is no single answer this layer can give
        // without knowing which slot the caller means.
        None
    }

    // `interleave` deliberately keeps the trait's own default
    // (`interleave_per_dts`) rather than delegating to any one slot: this
    // layer owns a single shared queue upstream of the per-slot fan-out in
    // `write_packet`, and slots can want incompatible policies (MPEG-TS
    // wants none at all; MOV fragmented mode wants per-fragment). Per-DTS is
    // the same "loosest common reading" `flags` already gives, and it is not
    // overridden below only because there is nothing to forward it *to* —
    // no single slot's answer would be correct for every other slot.

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<BitstreamAction> {
        // Deliberately not forwarded to a slot. This layer runs no BSF chain
        // of its own across slots (the module doc's `bsfs=`/`bsfs/<type>=`
        // note: parsed but "not applied"), so `write_packet` hands every
        // slot the same, unconverted bytes. Answering a slot's real
        // preference here would make the caller convert the packet *once*,
        // upstream of the fan-out, for every slot regardless of whether that
        // slot wanted it — which could feed a converted-away form to a slot
        // that needed the original. `Keep`, the default, matches what
        // `write_packet` already does today.
        let _ = (params, packet);
        Ok(BitstreamAction::Keep)
    }

    /// Best effort, matching `add_stream`'s own per-slot tolerance: a codec
    /// this tee can carry on *any* output must not be blocked upfront just
    /// because another output cannot take it — `add_stream` already lets a
    /// slot that cannot take a stream silently drop it rather than failing
    /// the whole tee, and refusing here would be stricter than that.
    fn query_codec(&self, codec: CodecId, strict: i32) -> CodecSupport {
        let mut best = CodecSupport::Unsupported;
        for slot in &self.slots {
            let support = slot.muxer.query_codec(codec, strict);
            if support == CodecSupport::Supported {
                return support;
            }
            if support == CodecSupport::Experimental && best == CodecSupport::Unsupported {
                best = support;
            }
        }
        best
    }

    fn write_flush(&mut self) -> Result<()> {
        let mut first_err = None;
        for slot in &mut self.slots {
            if !slot.alive {
                continue;
            }
            if let Err(e) = slot.muxer.write_flush() {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Forwarded to every alive slot. Before this override, `-metadata` and
    /// chapters/attachments were silently dropped for every tee'd output —
    /// the most consequential of this file's gaps, since it produces wrong
    /// files quietly on ordinary use rather than failing loudly.
    fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
        let mut first_err = None;
        for slot in &mut self.slots {
            if !slot.alive {
                continue;
            }
            if let Err(e) = slot.muxer.set_metadata(metadata) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Forwarded to every alive slot, for the same reason as
    /// [`Muxer::set_metadata`] above: before this override, `-bitexact`
    /// silently had no effect on any tee'd output.
    fn set_bitexact(&mut self, bitexact: bool) {
        for slot in &mut self.slots {
            if slot.alive {
                slot.muxer.set_bitexact(bitexact);
            }
        }
    }

    /// Broadcast to every alive slot; an option name is inherently specific
    /// to one container, so most slots are expected to reject it. Answering
    /// `Ok` if *any* slot accepted mirrors `query_codec`'s best-effort
    /// reading above — a `-movflags` meant for the one MOV output in a tee
    /// must not fail the whole write because a sibling MPEG-TS output does
    /// not recognise it.
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        let mut last_err = None;
        let mut any_ok = false;
        for slot in &mut self.slots {
            if !slot.alive {
                continue;
            }
            match slot.muxer.set_option(name, value) {
                Ok(()) => any_ok = true,
                Err(e) => last_err = Some(e),
            }
        }
        if any_ok {
            return Ok(());
        }
        Err(last_err.unwrap_or(Error::Option {
            name: name.to_owned(),
            detail: "tee: no live output slot accepted this option".to_owned(),
        }))
    }

    // `bind_url` deliberately keeps the trait's default (`Unsupported`).
    // Unlike a muxer reached through `MuxerDesc::open`'s placeholder-sink
    // dance, a `TeeMuxer` is never constructed that way at all — the module
    // doc's "the registry seam does not fit this format" note already
    // explains why `open_tee` below always refuses. Every slot's own sink is
    // already fully resolved by the time `TeeMuxer::new` runs, so there is
    // no placeholder state left for a URL to rebind.
}

/// The registry `open` path: always [`vaco_core::Error::Unsupported`] — see
/// the module docs for why the bare `fn(Box<dyn MediaSink>)` signature
/// cannot express even one output URL, let alone several.
#[allow(clippy::needless_pass_by_value, reason = "MuxerDesc::open's signature")]
fn open_tee(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Err(Error::Unsupported(
        "tee: MuxerDesc::open has no channel for an output URL list; use TeeMuxer::new with grammar::parse",
    ))
}

/// `tee`: `ffmpeg -h muxer=tee` names it "Multiple muxer tee".
pub static MUXER_TEE: MuxerDesc = MuxerDesc {
    name: "tee",
    long_name: "Multiple muxer tee",
    extensions: &[],
    default_video: None,
    default_audio: None,
    open: open_tee,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use vaco_format_core::options::FormatOptions;
    use vaco_format_core::vacoraw::{MemorySink, VacoRawMuxer};

    fn raw_muxer() -> Box<dyn Muxer> {
        let opts = FormatOptions::default();
        Box::new(VacoRawMuxer::new(Box::new(MemorySink::new()), &opts).unwrap())
    }

    fn params(media: MediaType) -> CodecParameters {
        CodecParameters::new(media)
    }

    #[test]
    fn stream_selector_parses_media_letter_and_media_index() {
        assert_eq!(
            StreamSelector::parse("v"),
            StreamSelector::Media(MediaType::Video)
        );
        assert_eq!(
            StreamSelector::parse("a:0"),
            StreamSelector::MediaIndex(MediaType::Audio, 0)
        );
        assert_eq!(StreamSelector::parse("garbage"), StreamSelector::All);
        assert_eq!(StreamSelector::parse(""), StreamSelector::All);
    }

    #[test]
    fn a_video_only_and_an_audio_only_output_each_get_only_their_stream() {
        let outputs = grammar::parse("[select=v]v.out|[select=a]a.out").unwrap();
        let mut tee = TeeMuxer::new(&outputs, vec![raw_muxer(), raw_muxer()]).unwrap();
        let v = tee.add_stream(&params(MediaType::Video)).unwrap();
        let a = tee.add_stream(&params(MediaType::Audio)).unwrap();
        assert_eq!(tee.slots[0].local_index[v as usize], Some(0));
        assert_eq!(tee.slots[0].local_index[a as usize], None);
        assert_eq!(tee.slots[1].local_index[v as usize], None);
        assert_eq!(tee.slots[1].local_index[a as usize], Some(0));
    }

    #[test]
    fn onfail_ignore_keeps_the_muxer_usable_after_one_slot_fails() {
        // A muxer that always fails add_stream/write_header to simulate a
        // slave that could not open.
        struct AlwaysFails;
        impl Muxer for AlwaysFails {
            fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
                Ok(0)
            }
            fn write_header(&mut self) -> Result<()> {
                Err(Error::Unsupported("simulated open failure"))
            }
            fn write_packet(&mut self, _p: &Packet) -> Result<()> {
                Ok(())
            }
            fn write_trailer(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let outputs = grammar::parse("[onfail=ignore]bad.out|good.out").unwrap();
        let mut tee = TeeMuxer::new(&outputs, vec![Box::new(AlwaysFails), raw_muxer()]).unwrap();
        tee.add_stream(&params(MediaType::Video)).unwrap();
        assert!(tee.write_header().is_ok());
        assert!(!tee.slots[0].alive);
        assert!(tee.slots[1].alive);
    }

    #[test]
    fn without_onfail_one_failing_slot_aborts_the_whole_open() {
        struct AlwaysFails;
        impl Muxer for AlwaysFails {
            fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
                Ok(0)
            }
            fn write_header(&mut self) -> Result<()> {
                Err(Error::Unsupported("simulated open failure"))
            }
            fn write_packet(&mut self, _p: &Packet) -> Result<()> {
                Ok(())
            }
            fn write_trailer(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let outputs = grammar::parse("bad.out|good.out").unwrap();
        let mut tee = TeeMuxer::new(&outputs, vec![Box::new(AlwaysFails), raw_muxer()]).unwrap();
        tee.add_stream(&params(MediaType::Video)).unwrap();
        assert!(tee.write_header().is_err());
    }

    #[test]
    fn mismatched_output_and_muxer_counts_is_an_error() {
        let outputs = grammar::parse("a.out|b.out").unwrap();
        assert!(TeeMuxer::new(&outputs, vec![raw_muxer()]).is_err());
    }

    #[test]
    fn the_registry_open_path_reports_the_gap() {
        let sink = Box::new(MemorySink::new());
        assert!(open_tee(sink).is_err());
        assert!(MUXER_TEE.matches_name("tee"));
    }

    /// Records what it was told, rather than doing anything with it — the
    /// only way to observe whether `TeeMuxer` actually forwards a call
    /// through to a slot, since `VacoRawMuxer` does not expose its own
    /// received metadata/bitexact state for a test to inspect.
    #[derive(Default)]
    struct RecordingMuxer {
        metadata: Arc<Mutex<Option<MuxMetadata>>>,
        bitexact: Arc<Mutex<Option<bool>>>,
    }
    impl Muxer for RecordingMuxer {
        fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
            Ok(0)
        }
        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }
        fn write_packet(&mut self, _p: &Packet) -> Result<()> {
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
        fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
            *self.metadata.lock().unwrap() = Some(metadata.clone());
            Ok(())
        }
        fn set_bitexact(&mut self, bitexact: bool) {
            *self.bitexact.lock().unwrap() = Some(bitexact);
        }
    }

    /// The consequential case named in `planning/TECH-DEBT.md`'s wrapper
    /// audit: without `TeeMuxer::set_metadata`/`set_bitexact` forwarding,
    /// `-metadata` and `-bitexact` are silently dropped for every tee'd
    /// output, which is wrong output produced quietly rather than a loud
    /// failure. Reverting either override — replacing its body with `Ok(())`
    /// / `()` — makes this test fail, confirmed by hand before this comment
    /// was written.
    #[test]
    fn set_metadata_and_set_bitexact_reach_every_slot() {
        let metadata_a = Arc::new(Mutex::new(None));
        let bitexact_a = Arc::new(Mutex::new(None));
        let metadata_b = Arc::new(Mutex::new(None));
        let bitexact_b = Arc::new(Mutex::new(None));
        let slot_a: Box<dyn Muxer> = Box::new(RecordingMuxer {
            metadata: metadata_a.clone(),
            bitexact: bitexact_a.clone(),
        });
        let slot_b: Box<dyn Muxer> = Box::new(RecordingMuxer {
            metadata: metadata_b.clone(),
            bitexact: bitexact_b.clone(),
        });
        let outputs = grammar::parse("a.out|b.out").unwrap();
        let mut tee = TeeMuxer::new(&outputs, vec![slot_a, slot_b]).unwrap();

        let mut meta = MuxMetadata::default();
        meta.tags.push(("title".to_owned(), "a tee'd file".to_owned()));
        tee.set_metadata(&meta).unwrap();
        tee.set_bitexact(true);

        for (m, b) in [(&metadata_a, &bitexact_a), (&metadata_b, &bitexact_b)] {
            assert_eq!(
                m.lock().unwrap().as_ref().map(|md| md.tags.clone()),
                Some(vec![("title".to_owned(), "a tee'd file".to_owned())])
            );
            assert_eq!(*b.lock().unwrap(), Some(true));
        }
    }

    #[test]
    fn add_stream_with_forwards_the_stream_spec_to_every_selected_slot() {
        struct SpecCapturingMuxer {
            seen: Arc<Mutex<Option<StreamSpec>>>,
        }
        impl Muxer for SpecCapturingMuxer {
            fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
                Ok(0)
            }
            fn add_stream_with(&mut self, _p: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
                *self.seen.lock().unwrap() = Some(*spec);
                Ok(0)
            }
            fn write_header(&mut self) -> Result<()> {
                Ok(())
            }
            fn write_packet(&mut self, _p: &Packet) -> Result<()> {
                Ok(())
            }
            fn write_trailer(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let seen = Arc::new(Mutex::new(None));
        let outputs = grammar::parse("a.out").unwrap();
        let mut tee = TeeMuxer::new(
            &outputs,
            vec![Box::new(SpecCapturingMuxer { seen: seen.clone() })],
        )
        .unwrap();
        let spec = StreamSpec {
            time_base: Some(Rational::new(1, 90_000)),
        };
        tee.add_stream_with(&params(MediaType::Video), &spec)
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().and_then(|s| s.time_base),
            spec.time_base
        );
    }
}
