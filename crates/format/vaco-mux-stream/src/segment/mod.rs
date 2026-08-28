//! `segment`/`stream_segment`: split output across numbered files.
//!
//! # Layout
//!
//! * [`planner`] — the pure cut-decision state machine
//!   ([`planner::SegmentPlanner`]), unit-tested against plain
//!   `(pts, is_key)` sequences with no I/O at all.
//! * [`pattern`] — `%d`/`%0Nd` filename numbering.
//! * [`strftime`] — `-strftime`'s filename expansion, via `vaco-time` (see
//!   that module's docs for why `std::time` cannot be used here).
//! * [`list`] — `-segment_list_type`'s four renderings.
//! * This module — [`SegmentMuxer`], which drives all four together.
//!
//! # Measured (`ffmpeg -h muxer=segment`/`stream_segment`, ffmpeg 8.1)
//!
//! Every option name and default in [`SegmentOptions`]'s fields is taken
//! directly from `-h muxer=segment`'s listing (`segment_time` default `2`
//! seconds, `break_non_keyframes` default `false`,
//! `individual_header_trailer` default `true`, `reset_timestamps` default
//! `false`, `write_empty_segments` default `false`, and so on).
//! `stream_segment` lists the identical option set minus the
//! `segment_format`/`segment_list*` family (measured: `-h
//! muxer=stream_segment` is `segment`'s listing with those five lines
//! removed) — this crate models that as [`MUXER_STREAM_SEGMENT`] sharing
//! [`SegmentMuxer`] with [`MUXER_SEGMENT`], which is a legitimate way to
//! honour a fully-visible options *subset*: nothing stops a
//! [`MUXER_STREAM_SEGMENT`]-driven [`SegmentOptions`] from setting
//! `segment_list`, it is simply never asked to via the descriptor's own
//! name.
//!
//! # The registry seam does not fit this format
//!
//! [`vaco_format_core::MuxerDesc::open`] takes one sink and nothing else —
//! no filename pattern, no `-segment_format` to resolve into an actual
//! inner muxer. [`MUXER_SEGMENT`]/[`MUXER_STREAM_SEGMENT`]'s `open` is
//! [`vaco_core::Error::Unsupported`]; [`SegmentMuxer::new`] is the real
//! constructor, taking a filename pattern, [`SegmentOptions`], and a
//! `factory: FnMut(&str) -> Result<Box<dyn Muxer>>` the caller supplies —
//! called with this module's own resolved filename for each new segment, so
//! the caller only has to turn a name into an open muxer (exactly the
//! "-`segment_format`" resolution this crate cannot do itself).
//!
//! # What is faithfully modelled and what is approximate
//!
//! * The cut decision (interval, explicit times/frames, `min_seg_duration`,
//!   `break_non_keyframes`) is [`planner::SegmentPlanner`], unit-tested in
//!   isolation — see that module.
//! * `reset_timestamps` and `initial_offset` are applied directly in
//!   `TIME_BASE_Q` microseconds — see `planner`'s module docs for why that
//!   is the base every packet this muxer sees is already in.
//! * `segment_wrap`/`segment_start_number`/`segment_wrap_number` control the
//!   numeric index handed to [`pattern::expand_index`].
//! * `individual_header_trailer=false` is **not** modelled precisely: doing
//!   so exactly needs "this is the last segment", which is only knowable at
//!   [`Muxer::write_trailer`] time, after every intermediate segment has
//!   already been opened and closed. This crate always gives every segment
//!   its own header and trailer regardless of this flag — recorded as a
//!   known gap in [`SegmentOptions::individual_header_trailer`] rather than
//!   silently ignored.
//! * `-segment_format_options`/`increment_tc`/`segment_atclocktime`/
//!   `segment_clocktime_offset`/`segment_clocktime_wrap_duration`/
//!   `segment_list_size`/`segment_list_entry_prefix`/
//!   `segment_header_filename` are not implemented at all — out of scope for
//!   the breadth pass; the fields that matter for correctness (cutting,
//!   naming, resetting timestamps, listing) are what this module spent its
//!   budget on.

pub mod list;
pub mod pattern;
pub mod planner;
pub mod strftime;

use std::collections::HashMap;

use vaco_codec_core::CodecParameters;
use vaco_core::{Duration, Error, MediaType, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::metadata::MuxMetadata;
use vaco_format_core::mux::BitstreamAction;
use vaco_format_core::{Muxer, MuxerDesc, StreamSpec};
use vaco_io::MediaSink;
use vaco_packet::Packet;

pub use list::{SegmentListType, SegmentRecord};
pub use planner::{SegmentPlanner, SegmentTrigger};

/// `-segment_time`/`-break_non_keyframes`/… . Defaults match `ffmpeg -h
/// muxer=segment`.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent, directly-named CLI flag (`-break_non_keyframes`, `-reset_timestamps`, …); folding them into enums would not make any of them less independent, just harder to look up by name"
)]
pub struct SegmentOptions {
    /// `-segment_time`, default 2s. Ignored when [`Self::segment_times`] or
    /// [`Self::segment_frames`] is non-empty.
    pub segment_time: Duration,
    pub segment_time_delta: Duration,
    pub min_seg_duration: Duration,
    pub segment_times: Vec<Duration>,
    pub segment_frames: Vec<u64>,
    /// `0` means no wrap.
    pub segment_wrap: u32,
    pub segment_start_number: u32,
    pub segment_wrap_number: u32,
    pub strftime: bool,
    pub break_non_keyframes: bool,
    /// See the module docs: not honoured precisely when `false`.
    pub individual_header_trailer: bool,
    pub reset_timestamps: bool,
    pub initial_offset: Duration,
    pub write_empty_segments: bool,
    pub segment_list_type: SegmentListType,
}

impl Default for SegmentOptions {
    fn default() -> Self {
        Self {
            segment_time: Duration(2_000_000),
            segment_time_delta: Duration(0),
            min_seg_duration: Duration(0),
            segment_times: Vec::new(),
            segment_frames: Vec::new(),
            segment_wrap: 0,
            segment_start_number: 0,
            segment_wrap_number: 0,
            strftime: false,
            break_non_keyframes: false,
            individual_header_trailer: true,
            reset_timestamps: false,
            initial_offset: Duration(0),
            write_empty_segments: false,
            segment_list_type: SegmentListType::Flat,
        }
    }
}

impl SegmentOptions {
    fn trigger(&self) -> SegmentTrigger {
        if !self.segment_times.is_empty() {
            SegmentTrigger::ExplicitTimes(self.segment_times.clone())
        } else if !self.segment_frames.is_empty() {
            SegmentTrigger::ExplicitFrames(self.segment_frames.clone())
        } else {
            SegmentTrigger::Interval(self.segment_time)
        }
    }
}

/// Builds one segment's inner muxer, given this module's own resolved
/// filename for it.
pub type SegmentFactory = Box<dyn FnMut(&str) -> Result<Box<dyn Muxer>> + Send>;

/// `segment`/`stream_segment`: successive spans, each its own inner muxer.
pub struct SegmentMuxer {
    pattern: String,
    options: SegmentOptions,
    factory: SegmentFactory,
    /// Each declared stream's [`CodecParameters`] plus the [`StreamSpec`] it
    /// was declared with (gap 9, `planning/INTERFACE-GAPS.md`) — replayed
    /// into every new segment's inner muxer in [`Self::open_next_segment`],
    /// since each segment is its own fresh [`Muxer`] that never itself saw
    /// the original [`Muxer::add_stream_with`] call.
    stream_params: Vec<(CodecParameters, StreamSpec)>,
    reference_stream: Option<u32>,
    planner: SegmentPlanner,
    current: Option<Box<dyn Muxer>>,
    /// Index into `stream_params`/output, used to remap into each fresh
    /// inner muxer's own (also sequential, from zero) indices — every
    /// segment's inner muxer sees the same `add_stream` calls in the same
    /// order, so the mapping is always the identity, kept explicit anyway
    /// so a future inner muxer that refuses one stream type does not
    /// silently misroute another.
    segment_index: u32,
    /// Per-stream pts (in `TIME_BASE_Q` microseconds) of the first packet in
    /// the *current* segment — subtracted back out when
    /// [`SegmentOptions::reset_timestamps`] is set.
    segment_base: HashMap<u32, i64>,
    records: Vec<SegmentRecord>,
    current_start_seconds: f64,
    list_sink: Option<Box<dyn MediaSink>>,
    /// Captured from [`Muxer::set_metadata`] and replayed onto every new
    /// segment's inner muxer. Before this field existed there was nowhere to
    /// keep the call at all: each segment is a fresh [`Muxer`] created after
    /// the one and only call `MuxBuilder::open` makes, so a plain forward to
    /// `self.current` (as `TeeMuxer` does, where there is exactly one
    /// muxer for the whole write) would have reached only the first segment.
    metadata: MuxMetadata,
    /// Captured from [`Muxer::set_bitexact`], for the same reason as
    /// `metadata` above.
    bitexact: bool,
    /// Captured from [`Muxer::set_option`], for the same reason as
    /// `metadata` above — a muxer-private option (`-movflags`, say, for an
    /// mp4-per-segment factory) is meant for every segment, not just
    /// whichever one happened to be open when it was set.
    options_set: Vec<(String, String)>,
}

impl core::fmt::Debug for SegmentMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SegmentMuxer")
            .field("pattern", &self.pattern)
            .field("segment_index", &self.segment_index)
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

impl SegmentMuxer {
    /// `pattern` is a `%d`/`%0Nd`-numbered (and, if
    /// [`SegmentOptions::strftime`], `strftime`-expanded) filename template;
    /// `factory` turns one resolved filename into an open inner muxer, and
    /// is called once per segment, lazily, as each is about to start.
    #[must_use]
    pub fn new(
        pattern: impl Into<String>,
        options: SegmentOptions,
        factory: SegmentFactory,
    ) -> Self {
        let planner = SegmentPlanner::new(
            options.trigger(),
            options.segment_time_delta,
            options.min_seg_duration,
            options.break_non_keyframes,
        );
        Self {
            pattern: pattern.into(),
            factory,
            planner,
            options,
            stream_params: Vec::new(),
            reference_stream: None,
            current: None,
            segment_index: 0,
            segment_base: HashMap::new(),
            records: Vec::new(),
            current_start_seconds: 0.0,
            list_sink: None,
            metadata: MuxMetadata::default(),
            bitexact: false,
            options_set: Vec::new(),
        }
    }

    /// Attach a sink the rendered segment list is rewritten into after every
    /// segment closes. See the module docs for the "list only grows"
    /// simplification this relies on.
    pub fn set_list_sink(&mut self, sink: Box<dyn MediaSink>) {
        self.list_sink = Some(sink);
    }

    /// The list as it stands right now, for a caller that wants to manage
    /// writing it itself instead of [`Self::set_list_sink`].
    #[must_use]
    pub fn rendered_list(&self, finished: bool) -> String {
        list::render(self.options.segment_list_type, &self.records, finished)
    }

    fn resolve_reference_stream(&self) -> Option<u32> {
        self.stream_params
            .iter()
            .position(|(p, _)| p.media_type == Some(MediaType::Video))
            .or(if self.stream_params.is_empty() {
                None
            } else {
                Some(0)
            })
            .map(|i| i as u32)
    }

    fn next_filename(&mut self) -> String {
        let wrapped = if self.options.segment_wrap == 0 {
            self.options.segment_start_number + self.segment_index
        } else {
            self.options.segment_wrap_number
                + (self.options.segment_start_number + self.segment_index)
                    % self.options.segment_wrap
        };
        self.segment_index += 1;
        let name = pattern::expand_index(&self.pattern, u64::from(wrapped));
        if self.options.strftime {
            strftime::expand_now(&name)
        } else {
            name
        }
    }

    fn open_next_segment(&mut self) -> Result<()> {
        let name = self.next_filename();
        let mut inner = (self.factory)(&name)?;
        for (params, spec) in self.stream_params.clone() {
            inner.add_stream_with(&params, &spec)?;
        }
        inner.init()?;
        for (name, value) in &self.options_set {
            // Best effort: an option meant for one segment format that a
            // later `-segment_format` change made irrelevant should not
            // abort the whole write. `Muxer::set_option`'s own default
            // already always errs for a name a muxer does not know, so a
            // factory whose muxer never grew options behaves exactly as
            // before this loop existed.
            let _ = inner.set_option(name, value);
        }
        inner.set_metadata(&self.metadata)?;
        inner.set_bitexact(self.bitexact);
        inner.write_header()?;
        self.current = Some(inner);
        self.segment_base.clear();
        self.records.push(SegmentRecord {
            filename: name,
            start_time: self.current_start_seconds,
            duration: 0.0,
        });
        self.write_list()?;
        Ok(())
    }

    fn write_list(&mut self) -> Result<()> {
        if self.list_sink.is_none() {
            return Ok(());
        }
        let rendered = self.rendered_list(false);
        if let Some(sink) = &mut self.list_sink {
            sink.seek(0)?;
            sink.write(rendered.as_bytes())?;
            sink.flush()?;
        }
        Ok(())
    }

    fn close_current(&mut self, elapsed_seconds: f64) -> Result<()> {
        if let Some(mut inner) = self.current.take() {
            inner.write_trailer()?;
            if let Some(last) = self.records.last_mut() {
                last.duration = elapsed_seconds;
            }
            self.current_start_seconds += elapsed_seconds;
        }
        Ok(())
    }

    /// Adjust `ts` for [`SegmentOptions::reset_timestamps`] and
    /// [`SegmentOptions::initial_offset`], recording this stream's segment
    /// base on the first call for it in the current segment.
    fn adjust_timestamp(&mut self, stream_index: u32, ts: Timestamp) -> Timestamp {
        let Some(ticks) = ts.ticks() else { return ts };
        let ticks = ticks.saturating_add(self.options.initial_offset.0);
        if !self.options.reset_timestamps {
            return Timestamp::new(ticks);
        }
        let base = *self.segment_base.entry(stream_index).or_insert(ticks);
        Timestamp::new(ticks.saturating_sub(base))
    }
}

impl Muxer for SegmentMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::TS_NONSTRICT.union(FormatFlags::TS_NEGATIVE)
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        // `add_stream_with`'s default already forwards to `add_stream` on a
        // segment's own inner muxer when that muxer has no opinion on
        // `StreamSpec`, so this is behaviourally identical to storing the
        // params directly — it just goes through the one place that also
        // captures `spec` for the segments not yet opened.
        self.add_stream_with(params, &StreamSpec::default())
    }

    /// [`Muxer::add_stream`], plus [`StreamSpec`] — captured and replayed
    /// into every future segment's own `add_stream_with` in
    /// [`Self::open_next_segment`] (gap 9, `planning/INTERFACE-GAPS.md`).
    /// Before this override, a stream-copy time base was dropped for every
    /// segment this muxer ever opened, not just the first.
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        let idx = self.stream_params.len() as u32;
        self.stream_params.push((params.clone(), *spec));
        Ok(idx)
    }

    fn write_header(&mut self) -> Result<()> {
        self.reference_stream = self.resolve_reference_stream();
        self.open_next_segment()
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        // `SegmentMuxer` declares no `stream_time_base` opinion, so a caller
        // driving it through `MuxWriter` has every packet already rescaled
        // to `TIME_BASE_Q` (microseconds) by the M1 step before it reaches
        // here — see `planner`'s module docs for why that lets the cut
        // decision treat `pts.ticks()` as microseconds directly, with no
        // per-stream time base lookup (there is nowhere to look one up:
        // `CodecParameters` does not carry one at all; only a `Stream`,
        // which this muxer never receives, does).
        let is_reference = Some(packet.stream_index) == self.reference_stream;
        if is_reference {
            let cut = self
                .planner
                .on_reference_packet(packet.pts, packet.is_key());
            if cut {
                let now_seconds = packet.pts.ticks().map_or(self.current_start_seconds, |us| {
                    vaco_core::Duration(us).as_secs_f64()
                });
                let elapsed = (now_seconds - self.current_start_seconds).max(0.0);
                self.close_current(elapsed)?;
                self.open_next_segment()?;
            }
        }
        let mut remapped = packet.clone();
        remapped.pts = self.adjust_timestamp(packet.stream_index, packet.pts);
        remapped.dts = self.adjust_timestamp(packet.stream_index, packet.dts);
        let Some(inner) = self.current.as_mut() else {
            return Err(Error::Unsupported(
                "segment: write_packet before write_header",
            ));
        };
        inner.write_packet(&remapped)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.close_current(0.0)?;
        if self.list_sink.is_some() {
            let rendered = self.rendered_list(true);
            if let Some(sink) = &mut self.list_sink {
                sink.seek(0)?;
                sink.write(rendered.as_bytes())?;
                sink.flush()?;
            }
        }
        Ok(())
    }

    // `init` deliberately keeps the trait's default (`Ok(())`): unlike
    // `write_header` below, no inner muxer exists yet for this to forward
    // to — `self.current` is only created lazily inside
    // `open_next_segment`, which is not called until `write_header`. Each
    // segment's own inner muxer still gets its own `init()` call, from
    // `open_next_segment` itself, once there is something to call it on.

    // `stream_time_base` deliberately keeps the trait's default (`None`),
    // and this one is not optional: `write_packet` above is written against
    // "every packet arrives already in `TIME_BASE_Q` microseconds", which is
    // only true because `None` here tells the M1 rescale step upstream to
    // leave packets in that base. Forwarding to `self.current`'s own answer
    // (an inner MP4 or MPEG-TS muxer's real time base) would make the
    // upstream rescale target *that* base instead, silently breaking the
    // cut-decision arithmetic `planner`'s module docs describe. Not touched
    // by this pass — flagged rather than guessed at.

    // `interleave` deliberately keeps the trait's default
    // (`interleave_per_dts`), for the same reason as `TeeMuxer`: this layer
    // owns one shared queue upstream of `write_packet`'s per-segment
    // routing, and `self.current` is swapped out mid-stream by every cut —
    // an inner muxer's own interleave preference from *before* a cut is not
    // even meaningfully "the same muxer" as the one after it.

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<BitstreamAction> {
        // Unlike `TeeMuxer`, there is exactly one live inner muxer at a
        // time here, so its own answer is unambiguous and safe to forward.
        self.current
            .as_mut()
            .map_or(Ok(BitstreamAction::Keep), |m| {
                m.check_bitstream(params, packet)
            })
    }

    // `query_codec` deliberately keeps the trait's default
    // (`Supported`): unlike `check_bitstream` above, this is asked by
    // `MuxBuilder::add_stream` *before* any segment exists to ask —
    // `self.current` is `None` until `write_header`, and the only way to
    // get a real answer would be invoking `factory` speculatively, which
    // has the side effect of actually opening a file. Not forwarded rather
    // than forwarded wrongly.

    fn write_flush(&mut self) -> Result<()> {
        self.current
            .as_mut()
            .map_or(Ok(()), vaco_format_core::Muxer::write_flush)
    }

    /// Captured, then forwarded to every future segment's own inner muxer
    /// from [`Self::open_next_segment`] — see the `metadata` field's doc for
    /// why capture rather than a plain forward is required here. Also
    /// forwarded to `self.current` immediately, for a caller that calls this
    /// after `write_header` (unusual, but not prohibited by the trait).
    fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
        self.metadata = metadata.clone();
        self.current
            .as_mut()
            .map_or(Ok(()), |m| m.set_metadata(metadata))
    }

    /// Captured and forwarded, for the same reason as [`Muxer::set_metadata`]
    /// above.
    fn set_bitexact(&mut self, bitexact: bool) {
        self.bitexact = bitexact;
        if let Some(m) = self.current.as_mut() {
            m.set_bitexact(bitexact);
        }
    }

    /// Captured and forwarded, for the same reason as [`Muxer::set_metadata`]
    /// above. Errors from the immediate forward to `self.current` are
    /// reported (a caller setting an option it expects this exact segment's
    /// muxer to understand should hear about a typo); errors from replaying
    /// a captured option onto a *later* segment are not, since a later
    /// `-segment_format` change can legitimately make an earlier option
    /// meaningless for the new format.
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        self.options_set.push((name.to_owned(), value.to_owned()));
        self.current
            .as_mut()
            .map_or(Ok(()), |m| m.set_option(name, value))
    }

    // `bind_url` deliberately keeps the trait's default (`Unsupported`), for
    // the same reason as `TeeMuxer`: `SegmentMuxer` is never constructed
    // through `MuxerDesc::open`'s placeholder-sink dance at all — see the
    // module doc's "the registry seam does not fit this format" note above.
}

/// The registry `open` path: always [`vaco_core::Error::Unsupported`] — see
/// the module docs.
#[allow(clippy::needless_pass_by_value, reason = "MuxerDesc::open's signature")]
fn open_segment(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Err(Error::Unsupported(
        "segment: MuxerDesc::open has no channel for a filename pattern or -segment_format; use SegmentMuxer::new",
    ))
}

/// `segment`: `ffmpeg -h muxer=segment` names it "segment".
pub static MUXER_SEGMENT: MuxerDesc = MuxerDesc {
    // Measured, `ffmpeg -muxers`: `segment` has **no** alias — the alias pair
    // in this family is `stream_segment,ssegment`. Declaring `segment,segments`
    // put an alias in the typed row that no component fragment declared, which
    // `vaco-registry`'s `every_typed_row_has_a_metadata_row` caught.
    name: "segment",
    long_name: "segment",
    extensions: &[],
    default_video: None,
    default_audio: None,
    open: open_segment,
};

/// `stream_segment`: same options minus the segment-list family (measured —
/// see the module docs), same [`SegmentMuxer`] underneath.
pub static MUXER_STREAM_SEGMENT: MuxerDesc = MuxerDesc {
    name: "stream_segment,ssegment",
    long_name: "streaming segment muxer",
    extensions: &[],
    default_video: None,
    default_audio: None,
    open: open_segment,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use vaco_format_core::options::FormatOptions;
    use vaco_format_core::vacoraw::{MemorySink, VacoRawMuxer};
    use vaco_limits::{Budget, Limits};
    use vaco_packet::PacketFlags;

    fn params(media: MediaType) -> CodecParameters {
        CodecParameters::new(media)
    }

    /// `pts_ms` is milliseconds, converted to the microseconds
    /// [`SegmentMuxer`] actually reads `pts` as (see `planner`'s module
    /// docs) — kept in milliseconds at the call site purely for readable
    /// test numbers.
    fn packet(stream: u32, pts_ms: i64, key: bool) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut p = Packet::from_slice(&mut budget, b"x").unwrap();
        p.stream_index = stream;
        p.pts = Timestamp::new(pts_ms * 1000);
        p.dts = Timestamp::new(pts_ms * 1000);
        if key {
            p.flags = PacketFlags::KEY;
        }
        p
    }

    struct RecordingMuxer {
        pts_seen: Arc<Mutex<Vec<i64>>>,
    }
    impl Muxer for RecordingMuxer {
        fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
            Ok(0)
        }
        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }
        fn write_packet(&mut self, p: &Packet) -> Result<()> {
            self.pts_seen.lock().unwrap().push(p.pts.ticks().unwrap());
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn counting_factory(opened: Arc<Mutex<Vec<String>>>) -> SegmentFactory {
        Box::new(move |name: &str| {
            opened.lock().unwrap().push(name.to_owned());
            let opts = FormatOptions::default();
            Ok(Box::new(VacoRawMuxer::new(Box::new(MemorySink::new()), &opts)?) as Box<dyn Muxer>)
        })
    }

    #[test]
    fn opens_a_new_segment_every_interval_at_a_keyframe() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let mut seg = SegmentMuxer::new(
            "out%d.raw",
            SegmentOptions {
                segment_time: Duration(2_000_000),
                ..SegmentOptions::default()
            },
            counting_factory(opened.clone()),
        );
        seg.add_stream(&params(MediaType::Video)).unwrap();
        seg.write_header().unwrap();
        for (ms, key) in [
            (0, true),
            (1000, false),
            (2000, false),
            (2500, true),
            (3000, false),
        ] {
            seg.write_packet(&packet(0, ms, key)).unwrap();
        }
        seg.write_trailer().unwrap();
        assert_eq!(*opened.lock().unwrap(), vec!["out0.raw", "out1.raw"]);
    }

    #[test]
    fn segment_start_number_and_wrap_control_the_index() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let mut seg = SegmentMuxer::new(
            "out%d.raw",
            SegmentOptions {
                segment_time: Duration(1_000_000),
                segment_start_number: 5,
                segment_wrap: 2,
                ..SegmentOptions::default()
            },
            counting_factory(opened.clone()),
        );
        seg.add_stream(&params(MediaType::Video)).unwrap();
        seg.write_header().unwrap();
        for ms in [0, 1000, 2000] {
            seg.write_packet(&packet(0, ms, true)).unwrap();
        }
        seg.write_trailer().unwrap();
        // start_number=5, wrap=2: indices 5%2=1, 6%2=0, 7%2=1 (wrap_number=0).
        assert_eq!(
            *opened.lock().unwrap(),
            vec!["out1.raw", "out0.raw", "out1.raw"]
        );
    }

    #[test]
    fn reset_timestamps_zeroes_each_segments_first_pts() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let pts_seen: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_for_factory = pts_seen.clone();
        let factory: SegmentFactory = Box::new(move |name: &str| {
            opened.lock().unwrap().push(name.to_owned());
            Ok(Box::new(RecordingMuxer {
                pts_seen: seen_for_factory.clone(),
            }) as Box<dyn Muxer>)
        });
        let mut seg = SegmentMuxer::new(
            "out%d.raw",
            SegmentOptions {
                segment_time: Duration(2_000_000),
                reset_timestamps: true,
                ..SegmentOptions::default()
            },
            factory,
        );
        seg.add_stream(&params(MediaType::Video)).unwrap();
        seg.write_header().unwrap();
        for (ms, key) in [(0, true), (2500, true), (3000, false)] {
            seg.write_packet(&packet(0, ms, key)).unwrap();
        }
        seg.write_trailer().unwrap();
        // Segment 1: 0. Segment 2 starts at 2500ms, so 2500->0, 3000->500.
        assert_eq!(*pts_seen.lock().unwrap(), vec![0, 0, 500_000]);
    }

    #[test]
    fn the_registry_open_path_reports_the_gap() {
        let sink = Box::new(MemorySink::new());
        assert!(open_segment(sink).is_err());
        assert!(MUXER_SEGMENT.matches_name("segment"));
        assert!(MUXER_STREAM_SEGMENT.matches_name("stream_segment"));
    }

    /// Records what it was told rather than doing anything with it, so a
    /// test can see whether a given segment's own inner muxer actually
    /// received a forwarded call.
    struct RecordingMetaMuxer {
        opened_index: usize,
        metadata_seen: Arc<Mutex<Vec<(usize, MuxMetadata)>>>,
        bitexact_seen: Arc<Mutex<Vec<(usize, bool)>>>,
    }
    impl Muxer for RecordingMetaMuxer {
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
            self.metadata_seen
                .lock()
                .unwrap()
                .push((self.opened_index, metadata.clone()));
            Ok(())
        }
        fn set_bitexact(&mut self, bitexact: bool) {
            self.bitexact_seen
                .lock()
                .unwrap()
                .push((self.opened_index, bitexact));
        }
    }

    /// The clearer-cut case named in `planning/TECH-DEBT.md`'s wrapper
    /// audit: `SegmentMuxer` had no stored field for metadata or bitexact at
    /// all, so a future fix could not even replay a call that was never
    /// captured. This checks the capture actually reaches every segment
    /// opened *after* the call, not only the one open at the time.
    #[test]
    fn set_metadata_and_set_bitexact_are_replayed_into_every_later_segment() {
        let opened = Arc::new(Mutex::new(0usize));
        let metadata_seen = Arc::new(Mutex::new(Vec::new()));
        let bitexact_seen = Arc::new(Mutex::new(Vec::new()));
        let (opened_c, metadata_c, bitexact_c) =
            (opened.clone(), metadata_seen.clone(), bitexact_seen.clone());
        let factory: SegmentFactory = Box::new(move |_name: &str| {
            let mut idx = opened_c.lock().unwrap();
            let this_index = *idx;
            *idx += 1;
            Ok(Box::new(RecordingMetaMuxer {
                opened_index: this_index,
                metadata_seen: metadata_c.clone(),
                bitexact_seen: bitexact_c.clone(),
            }) as Box<dyn Muxer>)
        });
        let mut seg = SegmentMuxer::new(
            "out%d.raw",
            SegmentOptions {
                segment_time: Duration(1_000_000),
                ..SegmentOptions::default()
            },
            factory,
        );
        seg.add_stream(&params(MediaType::Video)).unwrap();
        seg.write_header().unwrap(); // opens segment 0
        let mut meta = MuxMetadata::default();
        meta.tags.push(("title".to_owned(), "segmented".to_owned()));
        seg.set_metadata(&meta).unwrap();
        seg.set_bitexact(true);
        // Force a second segment to open after metadata/bitexact were set.
        seg.write_packet(&packet(0, 0, true)).unwrap();
        seg.write_packet(&packet(0, 1500, true)).unwrap(); // cuts to segment 1
        seg.write_trailer().unwrap();

        assert!(*opened.lock().unwrap() >= 2, "expected at least 2 segments to open");
        let metadata_seen = metadata_seen.lock().unwrap();
        let bitexact_seen = bitexact_seen.lock().unwrap();
        assert!(
            metadata_seen.iter().any(|(i, _)| *i == 1),
            "segment 1 should also have received set_metadata: {metadata_seen:?}"
        );
        assert!(
            bitexact_seen.contains(&(1, true)),
            "segment 1 should also have received set_bitexact(true): {bitexact_seen:?}"
        );
    }

    #[test]
    fn add_stream_with_time_base_is_replayed_into_every_segment() {
        struct SpecCapturingMuxer {
            opened_index: usize,
            seen: Arc<Mutex<Vec<(usize, Option<vaco_core::Rational>)>>>,
        }
        impl Muxer for SpecCapturingMuxer {
            fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
                Ok(0)
            }
            fn add_stream_with(&mut self, _p: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
                self.seen
                    .lock()
                    .unwrap()
                    .push((self.opened_index, spec.time_base));
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
        let opened = Arc::new(Mutex::new(0usize));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (opened_c, seen_c) = (opened.clone(), seen.clone());
        let factory: SegmentFactory = Box::new(move |_name: &str| {
            let mut idx = opened_c.lock().unwrap();
            let this_index = *idx;
            *idx += 1;
            Ok(Box::new(SpecCapturingMuxer {
                opened_index: this_index,
                seen: seen_c.clone(),
            }) as Box<dyn Muxer>)
        });
        let mut seg = SegmentMuxer::new(
            "out%d.raw",
            SegmentOptions {
                segment_time: Duration(1_000_000),
                ..SegmentOptions::default()
            },
            factory,
        );
        let tb = vaco_core::Rational::new(1, 90_000);
        seg.add_stream_with(&params(MediaType::Video), &StreamSpec { time_base: Some(tb) })
            .unwrap();
        seg.write_header().unwrap();
        seg.write_packet(&packet(0, 0, true)).unwrap();
        seg.write_packet(&packet(0, 1500, true)).unwrap(); // cuts to segment 1
        seg.write_trailer().unwrap();

        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().all(|(_, got)| *got == Some(tb)),
            "every segment's own add_stream_with should see the original time base: {seen:?}"
        );
        assert!(seen.len() >= 2, "expected at least 2 segments to open");
    }

    #[test]
    fn default_options_match_the_reference() {
        let d = SegmentOptions::default();
        assert_eq!(d.segment_time, Duration(2_000_000));
        assert!(!d.break_non_keyframes);
        assert!(d.individual_header_trailer);
        assert!(!d.reset_timestamps);
        assert!(!d.write_empty_segments);
    }
}
