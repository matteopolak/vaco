//! `random` — reservoir-shuffle the stream: fill a cache of `frames` frames,
//! then for every further arrival, emit a uniformly-chosen cached frame and
//! store the new one in its place.
//!
//! `ffmpeg -h filter=random`: `frames` (`2..=512`, default `30`), `seed`
//! (`-1..=UINT32_MAX`, default `-1` = seed from local entropy).
//!
//! # Not a reproduction of the reference's bit stream
//!
//! The reservoir-shuffle *shape* (buffer, swap-and-emit) is this option
//! pair's documented behaviour ("return random frames" from a cache of a
//! given size, reseedable). The actual sequence of pseudo-random draws is
//! not: reproducing the reference's specific generator would mean reading
//! its source (D7), and this workspace has no `rand`-family crate pulled in
//! for one filter to justify adding — [`crate::rng::SplitMix64`] is used
//! instead, seeded from the `seed` option when given. Same seed always
//! shuffles the same way; the *shuffle itself* differs from the reference's
//! — a documented divergence, matching this plan's T3 "algorithmically
//! faithful, not bit-exact" bar.
//!
//! # Independent oracle
//!
//! A reservoir shuffle is a bijection on frame *identity*: every input
//! frame is stored in the cache exactly once and every output either drains
//! the cache or is immediately replaced by the next input, so the output
//! stream is always a **permutation** of the input stream — same length,
//! same multiset of frame contents — for *any* correct shuffle, not a
//! property of this file's particular draws. That is checked directly (as a
//! sorted-multiset comparison of a tagged synthetic stream), independent of
//! which frame ends up in which output slot. A stream shorter than `frames`
//! never fills the cache, so it is a second, distinct closed-form case: no
//! draws ever happen and the output must equal the input **in order**.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::rng::SplitMix64;
use crate::video::{VIDEO_PAD, i64_opt, usize_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "random",
    description: "Return random frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
pub(crate) struct Filter {
    capacity: usize,
    rng: SplitMix64,
    cache: Vec<Frame>,
}

impl Filter {
    pub(crate) fn new(capacity: usize, seed: i64) -> Self {
        #[allow(
            clippy::cast_sign_loss,
            reason = "seed<0 means 'pick one'; a fixed fallback keeps this deterministic \
                      for tests, which is the property this crate can promise (see module doc)"
        )]
        let seed_u64 = if seed < 0 { 0x5EED_5EED_5EED_5EEDu64 } else { seed as u64 };
        Self {
            capacity: capacity.max(2),
            rng: SplitMix64::new(seed_u64),
            cache: Vec::new(),
        }
    }

    /// The per-frame step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        if self.cache.len() < self.capacity {
            self.cache.push(frame);
            return FrameOut::None;
        }
        let idx = self.rng.next_below(self.cache.len());
        let Some(slot) = self.cache.get_mut(idx) else {
            return FrameOut::One(frame);
        };
        let out = std::mem::replace(slot, frame);
        FrameOut::One(out)
    }

    fn eof(&mut self) -> FrameOut {
        if self.cache.is_empty() {
            return FrameOut::None;
        }
        self.cache.drain(..).collect::<FrameOut>()
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        Ok(self.eof())
    }

    fn flush_state(&mut self) {
        self.cache.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let capacity = usize_opt(req, "frames", 30).clamp(2, 512);
    let seed = i64_opt(req, "seed", -1);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(capacity, seed))),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn tagged_frame(tag: u8) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 1, 1).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(tag);
        }
        f
    }

    fn tag_of(f: &Frame) -> u8 {
        f.plane(0).unwrap().row(0).unwrap()[0]
    }

    #[test]
    fn output_is_a_permutation_of_the_input_multiset() {
        let mut f = Filter::new(4, 123);
        let input: Vec<u8> = (0..20u8).collect();
        let mut output = Vec::new();
        for &tag in &input {
            if let FrameOut::One(fr) = f.step(tagged_frame(tag)) {
                output.push(tag_of(&fr));
            }
        }
        if let FrameOut::Many(rest) = f.eof() {
            output.extend(rest.iter().map(tag_of));
        }
        let mut sorted_in = input.clone();
        let mut sorted_out = output.clone();
        sorted_in.sort_unstable();
        sorted_out.sort_unstable();
        assert_eq!(sorted_in, sorted_out, "shuffle must be a permutation, not a lossy filter");
        assert_eq!(output.len(), input.len());
    }

    #[test]
    fn a_stream_shorter_than_the_cache_passes_through_in_order() {
        let mut f = Filter::new(30, 1);
        let input = [1u8, 2, 3];
        let mut output = Vec::new();
        for &tag in &input {
            match f.step(tagged_frame(tag)) {
                FrameOut::One(fr) => output.push(tag_of(&fr)),
                FrameOut::None => {}
                FrameOut::Many(_) => panic!("unexpected"),
            }
        }
        if let FrameOut::Many(rest) = f.eof() {
            output.extend(rest.iter().map(tag_of));
        } else if let FrameOut::One(fr) = f.eof() {
            output.push(tag_of(&fr));
        }
        assert_eq!(output, vec![1, 2, 3], "never filled the cache: no shuffling occurs");
    }

    #[test]
    fn same_seed_reproduces_the_same_output_order() {
        let mut a = Filter::new(4, 42);
        let mut b = Filter::new(4, 42);
        let mut out_a = Vec::new();
        let mut out_b = Vec::new();
        for tag in 0..20u8 {
            if let FrameOut::One(fr) = a.step(tagged_frame(tag)) {
                out_a.push(tag_of(&fr));
            }
            if let FrameOut::One(fr) = b.step(tagged_frame(tag)) {
                out_b.push(tag_of(&fr));
            }
        }
        assert_eq!(out_a, out_b);
    }
}
