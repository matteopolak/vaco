//! `asetnsamples` — re-block audio into frames of an exact sample count.
//!
//! # Why this does not use `vaco_filter_core::adapt::AudioFilter`
//!
//! That adapter's own doc comment says it plainly: its `SampleFifo` is
//! "frame-granular rather than sample-granular... refuses rather than
//! guesses when a block would have to be cut mid-frame". `asetnsamples`
//! exists specifically to cut mid-frame — an input arriving in, say, 1152-
//! sample blocks re-blocked to `nb_out_samples=1024` needs an exact split at
//! sample 1024, which the core adapter cannot do. So this filter keeps its
//! own per-channel `f64` accumulator (via [`crate::sample`]) and implements
//! [`FrameFilter`] directly rather than through `AudioFilter`.
//!
//! This is worth flagging upward: any other T2 audio filter that needs an
//! exact frame size (the FFT-domain ones — `afftdn`, `firequalizer`, and
//! anything built on `vaco-tx`) will hit the same wall and will need either
//! this same workaround or a sample-granular FIFO added to
//! `vaco-filter-core` itself.
//!
//! `ffmpeg -h filter=asetnsamples` documents `nb_out_samples`/`n` (default
//! 1024) and `pad`/`p` (default true). Both implemented.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "asetnsamples",
    description: "set the number of samples for each output audio frame",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug)]
pub(crate) struct Filter {
    n: usize,
    pad_last: bool,
    buf: SmallVec<[Vec<f64>; 8]>,
    fmt: vaco_sampfmt::SampleFmt,
    rate: u32,
    layout: vaco_chlayout::ChannelLayout,
    next_pts: i64,
    have_pts: bool,
}

impl Filter {
    fn take_block(&mut self, n: usize) -> Option<SmallVec<[Vec<f64>; 8]>> {
        let available = self.buf.first().map_or(0, Vec::len);
        if available < n {
            return None;
        }
        let mut out: SmallVec<[Vec<f64>; 8]> = SmallVec::new();
        for ch in &mut self.buf {
            out.push(ch.drain(..n).collect());
        }
        Some(out)
    }

    fn emit(&mut self, data: &SmallVec<[Vec<f64>; 8]>) -> Result<Frame> {
        let mut f = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            self.fmt,
            self.layout.clone(),
            self.rate,
            data,
        )?;
        f.time_base = vaco_core::Rational::new(1, i32::try_from(self.rate.max(1)).unwrap_or(1));
        if self.have_pts {
            f.pts = vaco_core::Timestamp::new(self.next_pts);
        }
        let samples = data.first().map_or(0, Vec::len);
        self.next_pts = self
            .next_pts
            .saturating_add(i64::try_from(samples).unwrap_or(0));
        f.set_duration_ticks(i64::try_from(samples).unwrap_or(0));
        Ok(f)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _, layout, channels) = crate::sample::decode(&input)?;
        self.fmt = fmt;
        self.rate = rate;
        self.layout = layout;
        if !self.have_pts
            && let Some(p) = input.pts.ticks()
        {
            self.next_pts = p;
            self.have_pts = true;
        }
        if self.buf.is_empty() {
            self.buf = channels;
        } else {
            for (dst, src) in self.buf.iter_mut().zip(channels) {
                dst.extend(src);
            }
        }

        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        while let Some(block) = self.take_block(self.n) {
            out.push(self.emit(&block)?);
        }
        Ok(FrameOut::from_iter(out))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let remaining = self.buf.first().map_or(0, Vec::len);
        if remaining == 0 {
            return Ok(FrameOut::None);
        }
        let mut data: SmallVec<[Vec<f64>; 8]> = SmallVec::new();
        for ch in &mut self.buf {
            let mut v: Vec<f64> = std::mem::take(ch);
            if self.pad_last {
                v.resize(self.n, 0.0);
            }
            data.push(v);
        }
        Ok(FrameOut::One(self.emit(&data)?))
    }

    fn flush_state(&mut self) {
        self.buf.clear();
        self.have_pts = false;
        self.next_pts = 0;
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> signature every filter in this crate's registry.rs dispatches through, even though this particular filter never fails today"
)]
pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let n = req
        .named("nb_out_samples")
        .or_else(|| req.named("n"))
        .or_else(|| req.positional(0))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1024)
        .max(1);
    let pad_last = req
        .named("pad")
        .or_else(|| req.named("p"))
        .is_none_or(|s| matches!(s.as_str(), "1" | "true" | "yes"));

    let filter = Filter {
        n,
        pad_last,
        buf: SmallVec::new(),
        fmt: vaco_sampfmt::SampleFmt::F32,
        rate: 0,
        layout: vaco_chlayout::ChannelLayout::STEREO,
        next_pts: 0,
        have_pts: false,
    };

    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}
