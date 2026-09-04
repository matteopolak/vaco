//! DASH muxer (ISO/IEC 23009-1): segments each stream into its own fMP4
//! representation, writes the MPD, and optionally a companion HLS playlist
//! set over the same segment files.
//!
//! # How it works
//!
//! Unlike `vaco-mux-hls`, which multiplexes every stream into one shared
//! MPEG-TS segment per rotation, DASH puts each elementary stream in its own
//! `Representation`, so [`DashMuxer`] segments each stream **independently**:
//! every added stream gets its own [`RepresentationState`], its own
//! keyframe-triggered rotation, and its own `init-stream$RepresentationID$`/
//! `chunk-stream$RepresentationID$-$Number%05d$` files. `-adaptation_sets`
//! only changes how representations are *grouped* in the MPD for player
//! switching purposes; it does not change how they are segmented.
//!
//! Every representation's actual segment boundaries are recorded in
//! microseconds (this crate's fixed `@timescale`) and rendered as a compact
//! `SegmentTimeline` via [`vaco_format_adaptive::timeline::compact`] — the
//! mux-side use of the same function `vaco-demux-dash`'s round-trip
//! properties exercise from the read side.
//!
//! `-hls_playlist` writes a companion `master.m3u8` plus one
//! `media_<RepresentationID>.m3u8` per representation, naming the **same**
//! segment files DASH just wrote — not a second encode, just a second index
//! over one set of bytes.
//!
//! # What is deferred
//!
//! - **`-single_file`**: parsed and stored, has no effect. Byte-range
//!   single-file DASH output needs the same `CountingSink`-before-the-muxer
//!   trick `vaco-mux-hls` uses; wiring it up for N independent
//!   representations rather than one shared segment stream was not done
//!   this wave.
//! - **`-streaming`**: parsed and stored, has no effect. True per-frame
//!   fragmentation needs `SegmentMuxerProvider` to expose a
//!   fragment-per-packet mode, which the trait does not have yet.
//! - **`-adaptation_sets` grouping is parsed** (`id=0,streams=0,1 id=1,streams=2`)
//!   and reflected in the MPD's `<AdaptationSet>` boundaries, but every
//!   representation is still segmented as if it were alone — grouping is
//!   purely a manifest-shape decision here, which is what the option
//!   actually is on the reference too (segmentation is per `-map`/stream,
//!   not per adaptation set).
//!
//! # Configuration
//!
//! [`DashMuxOptions`] — names, types and defaults measured against
//! `ffmpeg -h muxer=dash` (ffmpeg 8.1).
//!
//! # Dependencies
//!
//! `vaco-format-adaptive`, `vaco-protocol-core` (never a concrete protocol
//! crate), `vaco-format-core`, `vaco-io`, `vaco-limits`, `vaco-packet`,
//! `vaco-codec-core`. Reaches fMP4 muxers only through `SegmentMuxerProvider`.

#![forbid(unsafe_code)]

pub mod adaptation_sets;

use std::collections::VecDeque;
use std::fmt::Write as _;

use vaco_codec_core::CodecParameters;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_format_adaptive::{
    SegmentContainerHint, SegmentMuxerProvider, TimelineEntry, WriteAccess,
};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

pub use adaptation_sets::parse_adaptation_sets;

/// This crate's fixed `@timescale`, in every `SegmentTemplate`/
/// `SegmentTimeline` it writes: one microsecond. Simpler than deriving one
/// from each stream's own rate, and every value this crate computes is
/// already carried in microseconds internally, so no rescale can disagree
/// with what was actually written.
pub const TIMESCALE: u64 = 1_000_000;

/// Muxer-level options. Names, types and defaults measured against
/// `ffmpeg -h muxer=dash` (ffmpeg 8.1).
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option; grouping them would break the 1:1 mapping the CLI needs"
)]
#[derive(Debug, Clone, Default)]
pub struct DashMuxOptions {
    /// `-seg_duration`: target segment length, in seconds. Default `5`.
    pub seg_duration: f64,
    /// `-use_template`: `SegmentTemplate` addressing rather than
    /// `SegmentList`. Default `true`. This crate does not implement
    /// `SegmentList` output at all, so `false` is refused at `write_header`.
    pub use_template: bool,
    /// `-use_timeline`: a `SegmentTimeline` inside the `SegmentTemplate`,
    /// stating each segment's actual duration, rather than one fixed
    /// `@duration` for all of them. Default `true`.
    pub use_timeline: bool,
    /// `-adaptation_sets`: `"id=0,streams=0,1 id=1,streams=2"`. `None`
    /// groups every stream into its own adaptation set.
    pub adaptation_sets: Option<String>,
    /// `-window_size`: segments kept in the manifest. `0` (default) means
    /// unlimited.
    pub window_size: usize,
    /// `-hls_playlist`: also write a companion HLS playlist set. Default
    /// `false`.
    pub hls_playlist: bool,
    /// `-hls_master_name`. Default `"master.m3u8"`.
    pub hls_master_name: String,
    /// `-single_file`. Parsed, not implemented — see the crate docs.
    pub single_file: bool,
    /// `-streaming`. Parsed, not implemented — see the crate docs.
    pub streaming: bool,
}

impl DashMuxOptions {
    /// Defaults exactly as measured from the reference.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seg_duration: 5.0,
            use_template: true,
            use_timeline: true,
            adaptation_sets: None,
            window_size: 0,
            hls_playlist: false,
            hls_master_name: "master.m3u8".to_owned(),
            single_file: false,
            streaming: false,
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn pointer type"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    // See the crate docs and `vaco-mux-hls`'s identical gap: `MuxerDesc::open`
    // has no filename and no protocol write access, both of which a
    // multi-file format needs. This writes the one sink it is given, once,
    // as the MPD, and refuses every `write_packet` since there is nowhere
    // to create a segment file.
    Ok(Box::new(DashMuxer {
        mpd_url: String::new(),
        write: None,
        mpd_sink: Some(sink),
        segments: Box::new(vaco_format_adaptive::NoSegmentMuxers),
        opts: DashMuxOptions::new(),
        streams: Vec::new(),
        reps: Vec::new(),
        header_written: false,
        trailer_written: false,
    }))
}

/// The descriptor a registry would hold.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "dash",
    long_name: "DASH Muxer",
    extensions: &["mpd"],
    default_video: Some(vaco_codec_core::CodecId::H264),
    default_audio: Some(vaco_codec_core::CodecId::Aac),
    open: open_muxer,
};

#[derive(Debug, Clone)]
struct WrittenSegment {
    number: u64,
    start_us: u64,
    duration_us: u64,
}

struct CurrentSegment {
    muxer: Box<dyn Muxer>,
    start_dts: Option<i64>,
    time_base: Option<Rational>,
}

struct RepresentationState {
    stream_index: u32,
    id: String,
    params: CodecParameters,
    header_written: bool,
    next_number: u64,
    current: Option<CurrentSegment>,
    written: VecDeque<WrittenSegment>,
    last_dts_us: Option<u64>,
    /// The most recent *non-zero* packet duration on this representation, in
    /// microseconds. `finish_segment`'s `end_us` used to be `last_dts_us`
    /// alone — the span *to* the last packet's own timestamp, not *through*
    /// it — so every segment's `mediaPresentationDuration` contribution came
    /// up one packet short, worst on a single-packet segment where it made
    /// the segment's own contribution zero before the `.max(1)` floor. A
    /// packet that states no duration leaves this at whatever the last one
    /// that did left it, mirroring `vaco-mux-avi`'s
    /// `last_video_duration_ticks` and `vaco-mux-hls`'s
    /// `last_ref_duration_us`.
    last_duration_us: u64,
}

impl RepresentationState {
    fn new(stream_index: u32, params: CodecParameters) -> Self {
        // Seeded from the stream's own declared frame rate, not an invented
        // constant, so a source whose packets never state a duration at all
        // (an ordinary `-c copy` remux out of a demuxer that reports none)
        // still gets a real `end_us` for its very first segment. Audio has
        // no samples-per-frame field to derive one from, so it starts at 0
        // and picks up the first packet that states a real duration, same
        // as every sibling fix in this sweep.
        let last_duration_us = params
            .video
            .as_ref()
            .filter(|v| v.frame_rate.num > 0 && v.frame_rate.den > 0)
            .and_then(|v| {
                let num = u64::try_from(v.frame_rate.num).ok()?;
                let den = u64::try_from(v.frame_rate.den).ok()?;
                (num > 0).then(|| 1_000_000u64.saturating_mul(den).checked_div(num).unwrap_or(0))
            })
            .unwrap_or(0);
        Self {
            id: stream_index.to_string(),
            stream_index,
            params,
            header_written: false,
            next_number: 1,
            current: None,
            written: VecDeque::new(),
            last_dts_us: None,
            last_duration_us,
        }
    }

    fn init_name(&self) -> String {
        format!("init-stream{}.m4s", self.id)
    }

    fn media_name(&self, number: u64) -> String {
        format!("chunk-stream{}-{number:05}.m4s", self.id)
    }
}

/// The DASH muxer.
pub struct DashMuxer {
    mpd_url: String,
    write: Option<WriteAccess>,
    mpd_sink: Option<Box<dyn MediaSink>>,
    segments: Box<dyn SegmentMuxerProvider>,
    opts: DashMuxOptions,
    streams: Vec<CodecParameters>,
    reps: Vec<RepresentationState>,
    header_written: bool,
    trailer_written: bool,
}

impl core::fmt::Debug for DashMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DashMuxer")
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl DashMuxer {
    /// The real entry point: `mpd_url` is where the MPD will be written
    /// (and, resolved against it, every segment file and the optional HLS
    /// companion playlists).
    #[must_use]
    pub fn new(
        mpd_url: String,
        write: Option<WriteAccess>,
        segments: Box<dyn SegmentMuxerProvider>,
        opts: DashMuxOptions,
    ) -> Self {
        Self {
            mpd_url,
            write,
            mpd_sink: None,
            segments,
            opts,
            streams: Vec::new(),
            reps: Vec::new(),
            header_written: false,
            trailer_written: false,
        }
    }

    fn write_text_file(&self, url: &str, content: &str) -> Result<()> {
        let Some(write) = &self.write else {
            return Err(Error::Unsupported(
                "DASH output needs protocol write access, and none was supplied",
            ));
        };
        let mut sink = write.create(url)?;
        sink.write(content.as_bytes())?;
        sink.flush()
    }

    fn rotate(&mut self, rep_index: usize, pdt_dts: i64) -> Result<()> {
        let Some(write) = self.write.clone() else {
            return Err(Error::Unsupported(
                "DASH output needs protocol write access, and none was supplied",
            ));
        };
        self.finish_segment(rep_index)?;
        let Some(rep) = self.reps.get_mut(rep_index) else {
            return Ok(());
        };
        let number = rep.next_number;
        rep.next_number = rep.next_number.saturating_add(1);
        let name = rep.media_name(number);
        let url = vaco_format_adaptive::resolve(&self.mpd_url, &name);
        let sink = write.create(&url)?;
        let mut m = self.segments.open_segment(
            SegmentContainerHint::Fmp4,
            sink,
            core::slice::from_ref(&rep.params),
            false,
        )?;
        m.init()?;
        m.write_header()?;
        let time_base = m.stream_time_base(0);
        rep.current = Some(CurrentSegment {
            muxer: m,
            start_dts: Some(pdt_dts),
            time_base,
        });
        let _ = number;
        Ok(())
    }

    fn finish_segment(&mut self, rep_index: usize) -> Result<()> {
        let Some(rep) = self.reps.get_mut(rep_index) else {
            return Ok(());
        };
        let Some(mut cur) = rep.current.take() else {
            return Ok(());
        };
        let start_us = cur
            .start_dts
            .zip(cur.time_base)
            .and_then(|(t, tb)| Timestamp::new(t).to_duration(tb))
            .map_or(0, |d| d.as_micros().max(0) as u64);
        let end_us = rep
            .last_dts_us
            .map_or(start_us, |last| last.saturating_add(rep.last_duration_us));
        let duration_us = end_us.saturating_sub(start_us).max(1);
        cur.muxer.write_trailer()?;
        rep.written.push_back(WrittenSegment {
            number: rep.next_number.saturating_sub(1),
            start_us,
            duration_us,
        });
        while self.opts.window_size != 0 && rep.written.len() > self.opts.window_size {
            rep.written.pop_front();
        }
        Ok(())
    }

    fn write_init_segments(&mut self) -> Result<()> {
        let Some(write) = self.write.clone() else {
            return Ok(()); // degraded mode: no init segments can be written.
        };
        for rep in &mut self.reps {
            let url = vaco_format_adaptive::resolve(&self.mpd_url, &rep.init_name());
            let sink = write.create(&url)?;
            let mut m = self.segments.open_segment(
                SegmentContainerHint::Fmp4,
                sink,
                core::slice::from_ref(&rep.params),
                true,
            )?;
            m.init()?;
            m.write_header()?;
            m.write_trailer()?;
            rep.header_written = true;
        }
        Ok(())
    }

    fn render_mpd(&self) -> String {
        let groups = parse_adaptation_sets(self.opts.adaptation_sets.as_deref(), self.reps.len());
        let total_us = self
            .reps
            .iter()
            .flat_map(|r| r.written.iter())
            .map(|s| s.start_us + s.duration_us)
            .max()
            .unwrap_or(0);
        let total_duration = vaco_format_adaptive::walltime::format_iso8601_duration(
            Duration::from_micros(i64::try_from(total_us).unwrap_or(i64::MAX)),
        );

        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        let _ = writeln!(
            out,
            "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" \
             mediaPresentationDuration=\"{total_duration}\" minBufferTime=\"PT{:.1}S\" \
             profiles=\"urn:mpeg:dash:profile:isoff-live:2011\">",
            self.opts.seg_duration
        );
        out.push_str("  <Period id=\"0\" start=\"PT0S\">\n");
        for group in &groups {
            out.push_str("    <AdaptationSet segmentAlignment=\"true\">\n");
            for &stream_index in group {
                if let Some(rep) = self.reps.iter().find(|r| r.stream_index == stream_index) {
                    self.render_representation(&mut out, rep);
                }
            }
            out.push_str("    </AdaptationSet>\n");
        }
        out.push_str("  </Period>\n</MPD>\n");
        out
    }

    fn render_representation(&self, out: &mut String, rep: &RepresentationState) {
        let bandwidth = rep.params.bit_rate.unwrap_or(1_000_000);
        let _ = writeln!(
            out,
            "      <Representation id=\"{}\" bandwidth=\"{bandwidth}\" mimeType=\"video/mp4\">",
            rep.id
        );
        if self.opts.use_template {
            let media_pattern = format!("chunk-stream{}-$Number%05d$.m4s", rep.id);
            let duration_attr = if self.opts.use_timeline {
                String::new()
            } else {
                let dur = rep
                    .written
                    .front()
                    .map_or((self.opts.seg_duration * TIMESCALE as f64) as u64, |s| {
                        s.duration_us
                    });
                format!(" duration=\"{dur}\"")
            };
            let _ = writeln!(
                out,
                "        <SegmentTemplate media=\"{media_pattern}\" initialization=\"{}\" timescale=\"{TIMESCALE}\" startNumber=\"1\"{duration_attr}>",
                rep.init_name(),
            );
            if self.opts.use_timeline {
                out.push_str("          <SegmentTimeline>\n");
                let timings: Vec<vaco_format_adaptive::SegmentTiming> = rep
                    .written
                    .iter()
                    .map(|s| vaco_format_adaptive::SegmentTiming {
                        start: s.start_us,
                        duration: s.duration_us,
                    })
                    .collect();
                for entry in vaco_format_adaptive::timeline::compact(&timings) {
                    Self::render_s(out, &entry);
                }
                out.push_str("          </SegmentTimeline>\n");
            }
            out.push_str("        </SegmentTemplate>\n");
        }
        out.push_str("      </Representation>\n");
    }

    fn render_s(out: &mut String, entry: &TimelineEntry) {
        out.push_str("            <S");
        if let Some(t) = entry.t {
            let _ = write!(out, " t=\"{t}\"");
        }
        let _ = write!(out, " d=\"{}\"", entry.d);
        if let Some(r) = entry.r
            && r != 0
        {
            let _ = write!(out, " r=\"{r}\"");
        }
        out.push_str("/>\n");
    }

    fn write_hls_companion(&self) -> Result<()> {
        if !self.opts.hls_playlist {
            return Ok(());
        }
        let master_url = vaco_format_adaptive::resolve(&self.mpd_url, &self.opts.hls_master_name);
        let mut master = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
        for rep in &self.reps {
            let media_name = format!("media_{}.m3u8", rep.id);
            let bandwidth = rep.params.bit_rate.unwrap_or(1_000_000);
            let _ = writeln!(master, "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}");
            let _ = writeln!(master, "{media_name}");

            let mut media = String::from("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-PLAYLIST-TYPE:VOD\n");
            let target = rep
                .written
                .iter()
                .map(|s| s.duration_us)
                .max()
                .unwrap_or(0)
                .div_ceil(1_000_000)
                .max(1);
            let _ = writeln!(media, "#EXT-X-TARGETDURATION:{target}");
            let _ = writeln!(media, "#EXT-X-MEDIA-SEQUENCE:0");
            let _ = writeln!(media, "#EXT-X-MAP:URI=\"{}\"", rep.init_name());
            for seg in &rep.written {
                let secs = seg.duration_us as f64 / 1_000_000.0;
                let _ = writeln!(media, "#EXTINF:{secs:.3},");
                let _ = writeln!(media, "{}", rep.media_name(seg.number));
            }
            media.push_str("#EXT-X-ENDLIST\n");
            let media_url = vaco_format_adaptive::resolve(&self.mpd_url, &media_name);
            self.write_text_file(&media_url, &media)?;
        }
        self.write_text_file(&master_url, &master)
    }
}

impl Muxer for DashMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "streams must be added before the header is written",
            ));
        }
        let index = u32::try_from(self.streams.len())
            .map_err(|_| Error::InvalidData("too many streams"))?;
        self.streams.push(params.clone());
        self.reps
            .push(RepresentationState::new(index, params.clone()));
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        if self.streams.is_empty() {
            return Err(Error::InvalidData("DASH output needs at least one stream"));
        }
        if !self.opts.use_template {
            return Err(Error::Unsupported(
                "DASH SegmentList output (-use_template 0) is not implemented",
            ));
        }
        self.header_written = true;
        self.write_init_segments()
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        let Some(rep_index) = self
            .reps
            .iter()
            .position(|r| r.stream_index == packet.stream_index)
        else {
            return Err(Error::InvalidData("packet names an unknown stream"));
        };

        let raw_ts = packet.dts.ticks().or_else(|| packet.pts.ticks());
        let target_us = (self.opts.seg_duration * TIMESCALE as f64) as i64;

        let needs_rotation = match self.reps.get(rep_index).and_then(|r| r.current.as_ref()) {
            None => true,
            Some(cur) => {
                packet.is_key()
                    && cur
                        .start_dts
                        .zip(cur.time_base)
                        .zip(raw_ts)
                        .and_then(|((start, tb), now)| {
                            Timestamp::new(now.saturating_sub(start)).to_duration(tb)
                        })
                        .is_some_and(|elapsed| elapsed.as_micros() >= target_us)
            }
        };

        if needs_rotation {
            let pdt = raw_ts.unwrap_or(0);
            self.rotate(rep_index, pdt)?;
        }

        let Some(rep) = self.reps.get_mut(rep_index) else {
            return Err(Error::InvalidData("representation vanished"));
        };
        let Some(cur) = rep.current.as_mut() else {
            return Err(Error::InvalidData("no active segment"));
        };
        cur.muxer.write_packet(packet)?;

        if let (Some(tb), Some(ts)) = (cur.time_base, raw_ts)
            && let Some(us) = Timestamp::new(ts).to_duration(tb)
        {
            let us = us.as_micros().max(0) as u64;
            rep.last_dts_us = Some(rep.last_dts_us.map_or(us, |prev| prev.max(us)));
        }
        let packet_duration_us = packet.duration.as_micros().max(0) as u64;
        if packet_duration_us > 0 {
            rep.last_duration_us = packet_duration_us;
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        for i in 0..self.reps.len() {
            self.finish_segment(i)?;
        }
        self.trailer_written = true;

        if let Some(mut sink) = self.mpd_sink.take() {
            let text = self.render_mpd();
            sink.write(text.as_bytes())?;
            sink.flush()?;
            return Ok(());
        }

        let text = self.render_mpd();
        self.write_text_file(&self.mpd_url, &text)?;
        self.write_hls_companion()
    }
}
