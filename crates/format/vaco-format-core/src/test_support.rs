#![allow(
    clippy::cast_possible_wrap,
    clippy::indexing_slicing,
    reason = "test fixtures"
)]
//! Fixtures shared by this crate's unit tests.
//!
//! Deliberately not `pub`: these exist to exercise the generic machinery
//! without pulling in a real container. The *public* worked example is
//! [`crate::vacoraw`], which is a format rather than a stub.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_io::MediaSource;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::probe::{ProbeData, ProbeScore};
use crate::seek::{SeekFlags, SeekTarget};
use crate::{Demuxer, DemuxerDesc, ParserProvider, Stream};

fn unopenable(_src: Box<dyn MediaSource>, _p: &dyn ParserProvider) -> Result<Box<dyn Demuxer>> {
    Err(Error::Unsupported("test descriptor cannot be opened"))
}

fn probe_a(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(b"AAAA") {
        ProbeScore::MAX
    } else if data.starts_with(b"TIE!") {
        ProbeScore::CONTENT
    } else {
        ProbeScore::NONE
    }
}

fn probe_b(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(b"BBBB") {
        ProbeScore::MAX
    } else if data.starts_with(b"TIE!") {
        ProbeScore::CONTENT
    } else {
        ProbeScore::NONE
    }
}

fn probe_mime(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(b"MIME") {
        ProbeScore(20)
    } else {
        ProbeScore::NONE
    }
}

fn probe_weak(_data: &ProbeData<'_>) -> ProbeScore {
    ProbeScore::weak(10)
}

pub(crate) const DESC_A: DemuxerDesc = DemuxerDesc {
    name: "fmt-a",
    long_name: "test format A",
    extensions: &["fa"],
    mime_types: &["video/x-a"],
    flags: crate::FormatFlags::empty(),
    probe: probe_a,
    open: unopenable,
};

pub(crate) const DESC_B: DemuxerDesc = DemuxerDesc {
    name: "fmt-b",
    long_name: "test format B",
    extensions: &["fb"],
    mime_types: &["video/x-b"],
    flags: crate::FormatFlags::empty(),
    probe: probe_b,
    open: unopenable,
};

pub(crate) const DESC_MIME: DemuxerDesc = DemuxerDesc {
    name: "fmt-mime",
    long_name: "test format with a mime type",
    extensions: &["fm"],
    mime_types: &["video/x-test"],
    flags: crate::FormatFlags::empty(),
    probe: probe_mime,
    open: unopenable,
};

pub(crate) const DESC_WEAK: DemuxerDesc = DemuxerDesc {
    name: "fmt-weak",
    long_name: "test format that always guesses",
    extensions: &[],
    mime_types: &[],
    flags: crate::FormatFlags::empty(),
    probe: probe_weak,
    open: unopenable,
};

/// A demuxer that produces a fixed number of synthetic packets.
///
/// Time base 1/1000, one packet every 100 ticks — ten frames a second, which
/// makes the frame-rate estimate a round number and therefore a real assertion
/// rather than an approximate one.
#[derive(Debug)]
pub(crate) struct MockDemuxer {
    streams: Vec<Stream>,
    total: u64,
    next: u64,
    first_pts: i64,
    duration: Option<Duration>,
    budget: Budget,
}

impl MockDemuxer {
    pub(crate) fn new(stream_count: usize, media: MediaType) -> Self {
        let streams = (0..stream_count)
            .map(|i| {
                let mut params = CodecParameters::new(media);
                params = params.with_codec(match media {
                    MediaType::Audio => CodecId::Opus,
                    _ => CodecId::H264,
                });
                match media {
                    MediaType::Video => {
                        params.video = Some(vaco_codec_core::VideoParameters::default());
                    }
                    MediaType::Audio => {
                        params.audio = Some(vaco_codec_core::AudioParameters::default());
                    }
                    _ => {}
                }
                let mut s = Stream::new(i as u32, media, Rational::new(1, 1000));
                s.id = Some(i as i64);
                s.params = params;
                s
            })
            .collect();
        Self {
            streams,
            total: 0,
            next: 0,
            first_pts: 0,
            duration: None,
            budget: Budget::new(Limits::permissive()),
        }
    }

    pub(crate) const fn with_packets(mut self, n: u64) -> Self {
        self.total = n;
        self
    }

    /// Give every stream out-of-band configuration, as MP4's `avcC` and
    /// Matroska's `CodecPrivate` do.
    ///
    /// The container's own record is the *only* source of an H.264 sequence
    /// parameter set in MP4, so a discovery test that never sets one cannot
    /// tell a parser that is being fed from one that is not.
    pub(crate) fn with_extradata(mut self, extra: &[u8]) -> Self {
        for s in &mut self.streams {
            s.params.extradata = Some(extra.to_vec());
        }
        self
    }

    pub(crate) const fn with_first_pts(mut self, pts: i64) -> Self {
        self.first_pts = pts;
        self
    }

    /// Declare both printed frame rates on every stream, as a container that
    /// states them does.
    pub(crate) fn set_frame_rates(&mut self, r: Rational, avg: Rational) {
        for s in &mut self.streams {
            s.r_frame_rate = r;
            s.avg_frame_rate = avg;
        }
    }

    /// State a container-level duration, as a container with a header field
    /// does. Needed to exercise the rule that hands it to a stream with no
    /// timing of its own.
    pub(crate) const fn with_duration(mut self, micros: i64) -> Self {
        self.duration = Some(Duration::from_micros(micros));
        self
    }
}

impl Demuxer for MockDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.next >= self.total {
            return Err(Error::Eof);
        }
        let i = self.next as i64;
        self.next += 1;
        let mut p = Packet::from_slice(&mut self.budget, &[0u8; 8])?;
        p.stream_index = 0;
        p.dts = Timestamp::new(i * 100);
        p.pts = Timestamp::new(self.first_pts + i * 100);
        p.flags = PacketFlags::KEY;
        Ok(p)
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        match target {
            SeekTarget::Timestamp { ts, .. } => {
                let t = ts.ticks().unwrap_or(0).max(0);
                #[allow(clippy::integer_division, reason = "test fixture; 100 is a literal")]
                let n = (t / 100) as u64;
                self.next = n.min(self.total);
                Ok(())
            }
            _ => Err(Error::NotSeekable),
        }
    }
}
