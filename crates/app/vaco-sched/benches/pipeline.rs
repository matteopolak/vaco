//! Where the scheduler's time goes, and whether threads pay for themselves.
//!
//! Plan 12's PF-0.1, PF-0.2 and PF-0.3 amendments record four confident
//! performance predictions on this project that measured backwards. So this
//! file states hypotheses and prints ratios; it does not contain a verdict.
//!
//! The hypotheses under test:
//!
//! 1. **Planning is not free.** `check_out` scans every node's every port each
//!    step, which is `O(nodes x ports)` per unit of work. For a five-node
//!    transcode that should be lost in the noise; for a fifty-stream remux it
//!    might not be. `plan` measures it against node count.
//! 2. **A scope per wave costs a thread spawn per job**, so the threaded driver
//!    can only win when a job is much more expensive than a spawn. `grain`
//!    sweeps the per-frame work from 0 to ~100us against thread count, so the
//!    break-even point is a measurement rather than a guess.
//! 3. **Shallow queues cost planning passes.** A capacity of one means one job
//!    per plan; a deeper queue lets a wave carry more. `capacity` sweeps it.

#![allow(
    clippy::single_match_else,
    clippy::default_trait_access,
    reason = "benchmark harness: shape mirrors the real components it stands in for"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use vaco_codec_core::{CodecParameters, Decoder, Encoder};
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Muxer, Stream};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sched::{Capacity, Driver, Finish, PipelineSpec};

fn main() {
    divan::main();
}

const TB: Rational = Rational { num: 1, den: 1000 };

// ------------------------------------------------------------------ harness

struct Source {
    streams: Vec<Stream>,
    packets: VecDeque<Packet>,
}

impl Demuxer for Source {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }
    fn read_packet(&mut self) -> Result<Packet> {
        self.packets.pop_front().ok_or(Error::Eof)
    }
    fn seek(&mut self, _t: SeekTarget, _f: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }
}

#[derive(Default)]
struct Sink {
    written: Arc<Mutex<usize>>,
    streams: u32,
}

impl Muxer for Sink {
    fn add_stream(&mut self, _p: &CodecParameters) -> Result<u32> {
        let index = self.streams;
        self.streams += 1;
        Ok(index)
    }
    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }
    fn write_packet(&mut self, _p: &Packet) -> Result<()> {
        if let Ok(mut n) = self.written.lock() {
            *n += 1;
        }
        Ok(())
    }
    fn write_trailer(&mut self) -> Result<()> {
        Ok(())
    }
    fn stream_time_base(&self, _i: u32) -> Option<Rational> {
        Some(TB)
    }
}

/// Burns `work` units per item so a job has a realistic cost. A decoder that
/// costs nothing measures the scheduler; one that costs something measures the
/// pipeline, and the two answers are different.
struct Busy {
    queue: VecDeque<Timestamp>,
    draining: bool,
    work: u64,
}

impl Busy {
    fn new(work: u64) -> Self {
        Self {
            queue: VecDeque::new(),
            draining: false,
            work,
        }
    }

    fn burn(&self) {
        let mut acc = 0_u64;
        for i in 0..self.work {
            acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i);
        }
        divan::black_box(acc);
    }
}

impl Decoder for Busy {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match packet {
            Some(p) => {
                self.burn();
                self.queue.push_back(p.pts);
                Ok(())
            }
            None => {
                self.draining = true;
                Ok(())
            }
        }
    }
    fn receive_frame(&mut self) -> Result<Frame> {
        match self.queue.pop_front() {
            Some(pts) => Ok(Frame {
                data: FrameData::Video {
                    format: vaco_pixfmt::PixFmt::Gray8,
                    width: 16,
                    height: 16,
                    planes: Default::default(),
                },
                pts,
                duration: vaco_core::Duration::ZERO,
                time_base: TB,
                color: Default::default(),
                sample_aspect_ratio: Rational::ONE,
                flags: Default::default(),
                side_data: Default::default(),
            }),
            None if self.draining => Err(Error::Eof),
            None => Err(Error::NeedMoreInput),
        }
    }
    fn flush(&mut self) {
        self.queue.clear();
        self.draining = false;
    }
}

struct BusyEnc {
    queue: VecDeque<Timestamp>,
    draining: bool,
    work: u64,
}

impl BusyEnc {
    fn new(work: u64) -> Self {
        Self {
            queue: VecDeque::new(),
            draining: false,
            work,
        }
    }
}

impl Encoder for BusyEnc {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        match frame {
            Some(f) => {
                let mut acc = 0_u64;
                for i in 0..self.work {
                    acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i);
                }
                divan::black_box(acc);
                self.queue.push_back(f.pts);
                Ok(())
            }
            None => {
                self.draining = true;
                Ok(())
            }
        }
    }
    fn receive_packet(&mut self) -> Result<Packet> {
        match self.queue.pop_front() {
            Some(pts) => {
                let mut p = Packet::empty();
                p.pts = pts;
                p.dts = pts;
                Ok(p)
            }
            None if self.draining => Err(Error::Eof),
            None => Err(Error::NeedMoreInput),
        }
    }
    fn flush(&mut self) {
        self.queue.clear();
        self.draining = false;
    }
}

fn packets(n: i64, bytes: usize) -> Vec<Packet> {
    let mut budget = Budget::new(Limits::permissive());
    let payload = vec![0_u8; bytes];
    (0..n)
        .map(|i| {
            let mut p =
                Packet::from_slice(&mut budget, &payload).unwrap_or_else(|_| Packet::empty());
            p.stream_index = 0;
            p.pts = Timestamp::new(i + 1);
            p.dts = Timestamp::new(i + 1);
            p
        })
        .collect()
}

fn transcode(count: i64, work: u64, capacity: Capacity) -> vaco_sched::Pipeline {
    let mut spec = PipelineSpec::new().with_capacity(capacity);
    let input = spec.add_input(Box::new(Source {
        streams: vec![Stream::new(0, MediaType::Video, TB)],
        packets: packets(count, 256).into(),
    }));
    let output = spec.add_output(Box::new(Sink::default()));
    let tap = spec
        .input_stream(input, 0)
        .unwrap_or_else(|_| unreachable());
    let frames = spec
        .add_decoder(tap, Box::new(Busy::new(work)))
        .unwrap_or_else(|_| unreachable());
    let encoded = spec
        .add_encoder(frames, Box::new(BusyEnc::new(work)), TB)
        .unwrap_or_else(|_| unreachable());
    let _ = spec.map(encoded, output, &CodecParameters::new(MediaType::Video));
    spec.build().unwrap_or_else(|_| unreachable())
}

fn remux(streams: u32, count: i64) -> vaco_sched::Pipeline {
    let mut spec = PipelineSpec::new();
    let mut all = Vec::new();
    for s in 0..streams {
        for i in 0..count {
            let mut p = Packet::empty();
            p.stream_index = s;
            p.pts = Timestamp::new(i + 1);
            p.dts = Timestamp::new(i + 1);
            all.push(p);
        }
    }
    all.sort_by_key(|p| p.dts.ticks());
    let input = spec.add_input(Box::new(Source {
        streams: (0..streams)
            .map(|s| Stream::new(s, MediaType::Video, TB))
            .collect(),
        packets: all.into(),
    }));
    let output = spec.add_output(Box::new(Sink::default()));
    for s in 0..streams {
        if let Ok(tap) = spec.input_stream(input, s) {
            let _ = spec.map(tap, output, &CodecParameters::new(MediaType::Video));
        }
    }
    spec.build().unwrap_or_else(|_| unreachable())
}

#[track_caller]
fn unreachable() -> ! {
    std::process::abort()
}

// --------------------------------------------------------------- benchmarks

/// Hypothesis 2: a scope per wave costs a spawn per job, so threads only pay
/// above some per-item grain. Read down a column for the thread scaling at one
/// grain; read across a row for how grain changes the answer.
#[divan::bench(args = [
    (1_usize, 0_u64), (1, 2_000), (1, 20_000), (1, 200_000),
    (2, 0), (2, 2_000), (2, 20_000), (2, 200_000),
    (4, 0), (4, 2_000), (4, 20_000), (4, 200_000),
])]
fn grain(bencher: divan::Bencher<'_, '_>, arg: (usize, u64)) {
    let (threads, work) = arg;
    let driver = Driver::with_threads(threads);
    bencher
        .with_inputs(|| transcode(120, work, Capacity::DEFAULT))
        .bench_local_values(|mut pipeline| {
            let finish = driver.run(&mut pipeline);
            divan::black_box(matches!(finish, Ok(Finish::Complete)))
        });
}

/// Hypothesis 3: a shallower queue means more planning passes for the same
/// work. How much more is the question.
#[divan::bench(args = [1_usize, 2, 4, 16, 64])]
fn capacity(bencher: divan::Bencher<'_, '_>, items: usize) {
    bencher
        .with_inputs(|| transcode(200, 0, Capacity::items(items)))
        .bench_local_values(|mut pipeline| {
            let finish = pipeline.run();
            divan::black_box(matches!(finish, Ok(Finish::Complete)))
        });
}

/// Hypothesis 1: readiness is scanned per node per step, so the cost per packet
/// should grow with stream count. Divide by `streams` to compare.
#[divan::bench(args = [1_u32, 4, 16, 64])]
fn plan(bencher: divan::Bencher<'_, '_>, streams: u32) {
    bencher
        .with_inputs(|| remux(streams, 64))
        .bench_local_values(|mut pipeline| {
            let finish = pipeline.run();
            divan::black_box(matches!(finish, Ok(Finish::Complete)))
        });
}

/// What one step costs with nothing to do: the floor under everything above.
#[divan::bench]
fn idle_step(bencher: divan::Bencher<'_, '_>) {
    bencher
        .with_inputs(|| transcode(0, 0, Capacity::DEFAULT))
        .bench_local_refs(|pipeline| divan::black_box(pipeline.classify()));
}
