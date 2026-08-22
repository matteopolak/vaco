//! Unit and property tests.
//!
//! The three that matter most, and why:
//!
//! * **`tail_is_not_lost`** — a decoder with a reorder delay and an encoder
//!   with a lookahead. If either drain is wrong the output is short by exactly
//!   the delay, which is invisible in a spot check and obvious here.
//! * **`minimal_capacity_still_completes`** — every wire bounded to one item.
//!   If backpressure could deadlock, this is where it would.
//! * **`threaded_matches_serial`** — the two drivers are one state machine, so
//!   they must produce byte-identical muxer input. This is the claim D6 rests
//!   on and the one an argument alone should not be trusted for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::single_match_else,
    clippy::cast_possible_wrap,
    reason = "test code: a failed assumption should fail the test loudly"
)]

use std::sync::{Arc, Mutex};

use vaco_codec_core::mock::{MockDecoder, MockProgram, Step};
use vaco_codec_core::{AsDecoder, Caps, CodecParameters, Decoder, Encoder, SendReceive, Validated};
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Muxer, Stream};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::{Advance, Capacity, Driver, Finish, PipelineSpec};

// ------------------------------------------------------------------ fixtures

/// A demuxer that hands out a scripted packet list.
#[derive(Debug)]
struct ScriptedDemuxer {
    streams: Vec<Stream>,
    packets: std::collections::VecDeque<Packet>,
}

impl ScriptedDemuxer {
    fn new(streams: Vec<Stream>, packets: Vec<Packet>) -> Self {
        Self {
            streams,
            packets: packets.into(),
        }
    }
}

impl Demuxer for ScriptedDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        self.packets.pop_front().ok_or(Error::Eof)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }
}

/// What a [`RecordingMuxer`] saw, shared with the test after the muxer itself
/// has been moved into the pipeline.
#[derive(Debug, Default)]
struct MuxLog {
    streams: Vec<MediaType>,
    header: bool,
    trailer: bool,
    /// `(stream index, pts, dts)` in write order.
    packets: Vec<(u32, Option<i64>, Option<i64>)>,
}

#[derive(Debug)]
struct RecordingMuxer {
    log: Arc<Mutex<MuxLog>>,
    time_base: Rational,
}

impl RecordingMuxer {
    fn pair(time_base: Rational) -> (Self, Arc<Mutex<MuxLog>>) {
        let log = Arc::new(Mutex::new(MuxLog::default()));
        (
            Self {
                log: Arc::clone(&log),
                time_base,
            },
            log,
        )
    }
}

impl Muxer for RecordingMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let mut log = self.log.lock().expect("mux log");
        log.streams
            .push(params.effective_media_type().unwrap_or(MediaType::Data));
        Ok(log.streams.len() as u32 - 1)
    }

    fn write_header(&mut self) -> Result<()> {
        let mut log = self.log.lock().expect("mux log");
        assert!(!log.header, "the header was written twice");
        assert!(log.packets.is_empty(), "a packet preceded the header");
        log.header = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let mut log = self.log.lock().expect("mux log");
        assert!(log.header, "a packet was written before the header");
        assert!(!log.trailer, "a packet was written after the trailer");
        log.packets
            .push((packet.stream_index, packet.pts.ticks(), packet.dts.ticks()));
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        let mut log = self.log.lock().expect("mux log");
        assert!(log.header, "the trailer was written without a header");
        assert!(!log.trailer, "the trailer was written twice");
        log.trailer = true;
        Ok(())
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        Some(self.time_base)
    }
}

/// Frames in, packets out, with a configurable lookahead so the drain at end of
/// stream has something to give back.
#[derive(Debug)]
struct MockEncoder {
    held: std::collections::VecDeque<Timestamp>,
    ready: std::collections::VecDeque<Timestamp>,
    delay: usize,
    draining: bool,
}

impl MockEncoder {
    fn new(delay: usize) -> Self {
        Self {
            held: std::collections::VecDeque::new(),
            ready: std::collections::VecDeque::new(),
            delay,
            draining: false,
        }
    }
}

impl SendReceive for MockEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        if self.delay > 0 {
            Caps::DELAY
        } else {
            Caps::empty()
        }
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        if self.draining {
            return Err(Error::Eof);
        }
        match input {
            Some(frame) => {
                self.held.push_back(frame.pts);
                while self.held.len() > self.delay {
                    if let Some(ts) = self.held.pop_front() {
                        self.ready.push_back(ts);
                    }
                }
                Ok(())
            }
            None => {
                self.draining = true;
                while let Some(ts) = self.held.pop_front() {
                    self.ready.push_back(ts);
                }
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Packet> {
        match self.ready.pop_front() {
            Some(ts) => {
                let mut packet = Packet::empty();
                packet.pts = ts;
                packet.dts = ts;
                Ok(packet)
            }
            None if self.draining => Err(Error::Eof),
            None => Err(Error::NeedMoreInput),
        }
    }

    fn flush(&mut self) {
        self.held.clear();
        self.ready.clear();
        self.draining = false;
    }
}

impl Encoder for MockEncoder {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        SendReceive::send(self, frame)
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        SendReceive::receive(self)
    }

    fn flush(&mut self) {
        SendReceive::flush(self);
    }
}

fn video_stream(index: u32, time_base: Rational) -> Stream {
    Stream::new(index, MediaType::Video, time_base)
}

/// `n` packets on `stream`, one tick apart, each `bytes` long.
fn packets(stream: u32, n: i64, bytes: usize) -> Vec<Packet> {
    let mut budget = Budget::new(Limits::permissive());
    let payload = vec![0_u8; bytes];
    (0..n)
        .map(|i| {
            let mut packet = if bytes == 0 {
                Packet::empty()
            } else {
                Packet::from_slice(&mut budget, &payload).expect("packet")
            };
            packet.stream_index = stream;
            // Start at 1: `pts == 0` is a legal timestamp but the mock decoder
            // derives an identity from it, and 0 is the value a missing one
            // would also have.
            packet.pts = Timestamp::new(i + 1);
            packet.dts = Timestamp::new(i + 1);
            packet
        })
        .collect()
}

fn decoder(delay: usize) -> Box<dyn Decoder> {
    let mut program = MockProgram::new(vec![Step::Reorder]);
    if delay == 0 {
        program = MockProgram::default();
    } else {
        program = program.with_reorder_delay(delay);
    }
    Box::new(AsDecoder(Validated::new(MockDecoder::new(program))))
}

const TB: Rational = Rational { num: 1, den: 1000 };
const MUX_TB: Rational = Rational {
    num: 1,
    den: 90_000,
};

// -------------------------------------------------------------------- tests

#[test]
fn stream_copy_moves_every_packet() {
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 10, 4),
    )));
    let (muxer, log) = RecordingMuxer::pair(MUX_TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, output, &CodecParameters::new(MediaType::Video))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);

    let log = log.lock().unwrap();
    assert!(log.header && log.trailer);
    assert_eq!(log.packets.len(), 10);
    // 1/1000 into 1/90000 is a factor of 90, exactly.
    assert_eq!(log.packets[0].1, Some(90));
    assert_eq!(log.packets[9].1, Some(900));
    assert!(log.packets.windows(2).all(|w| w[0].2 < w[1].2));
}

#[test]
fn tail_is_not_lost() {
    // Three frames held by the decoder's reorder buffer and two by the
    // encoder's lookahead: five packets that only exist if both drains happen
    // in the right order.
    for (dec_delay, enc_delay) in [(0, 0), (3, 0), (0, 2), (3, 2), (7, 5)] {
        let mut spec = PipelineSpec::new();
        let input = spec.add_input(Box::new(ScriptedDemuxer::new(
            vec![video_stream(0, TB)],
            packets(0, 20, 4),
        )));
        let (muxer, log) = RecordingMuxer::pair(TB);
        let output = spec.add_output(Box::new(muxer));
        let tap = spec.input_stream(input, 0).unwrap();
        let frames = spec.add_decoder(tap, decoder(dec_delay)).unwrap();
        let encoded = spec
            .add_encoder(frames, Box::new(MockEncoder::new(enc_delay)), TB)
            .unwrap();
        spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
            .unwrap();

        let mut pipeline = spec.build().unwrap();
        assert_eq!(
            pipeline.run().unwrap(),
            Finish::Complete,
            "delays {dec_delay}/{enc_delay}"
        );
        let log = log.lock().unwrap();
        assert!(log.trailer);
        assert_eq!(
            log.packets.len(),
            20,
            "delays {dec_delay}/{enc_delay} lost the tail"
        );
        let seen: Vec<i64> = log.packets.iter().filter_map(|p| p.1).collect();
        assert_eq!(seen, (1..=20).collect::<Vec<_>>());
    }
}

#[test]
fn minimal_capacity_still_completes() {
    // Every wire bounded to one item and one byte. If backpressure could
    // deadlock, it would here — and the empty-wire rule in `Wire::has_room` is
    // what stops a packet larger than the byte cap being unschedulable.
    let mut spec = PipelineSpec::new().with_capacity(Capacity::MINIMAL);
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 25, 512),
    )));
    let (muxer, log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    let frames = spec.add_decoder(tap, decoder(4)).unwrap();
    let encoded = spec
        .add_encoder(frames, Box::new(MockEncoder::new(3)), TB)
        .unwrap();
    spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    assert_eq!(log.lock().unwrap().packets.len(), 25);
}

#[test]
fn queues_stay_shallow_because_the_muxer_runs_first() {
    // Priority is the memory policy: the demuxer only reads when nothing
    // downstream can move. With a 64-item bound and 200 packets, the peak
    // should be a handful of items, not 200.
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 200, 1024),
    )));
    let (muxer, _log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    let frames = spec.add_decoder(tap, decoder(2)).unwrap();
    let encoded = spec
        .add_encoder(frames, Box::new(MockEncoder::new(1)), TB)
        .unwrap();
    spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    let stats = pipeline.stats();
    let peak = stats.wires.iter().map(|w| w.high_water).max().unwrap_or(0);
    assert!(
        peak <= 8,
        "queues reached {peak} items; a pull pipeline should stay shallow"
    );
    assert_eq!(pipeline.queued_bytes(), 0, "budget was not fully released");
}

#[test]
fn fan_out_reaches_every_output() {
    // `-map 0:0` twice: one stream copy and one transcode, from the same tap.
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 12, 4),
    )));
    let (copy_mux, copy_log) = RecordingMuxer::pair(TB);
    let (enc_mux, enc_log) = RecordingMuxer::pair(TB);
    let out_copy = spec.add_output(Box::new(copy_mux));
    let out_enc = spec.add_output(Box::new(enc_mux));

    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, out_copy, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let frames = spec.add_decoder(tap, decoder(2)).unwrap();
    let encoded = spec
        .add_encoder(frames, Box::new(MockEncoder::new(1)), TB)
        .unwrap();
    spec.map(encoded, out_enc, &CodecParameters::new(MediaType::Video))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    assert_eq!(copy_log.lock().unwrap().packets.len(), 12);
    assert_eq!(enc_log.lock().unwrap().packets.len(), 12);
}

#[test]
fn two_inputs_into_one_output() {
    let mut spec = PipelineSpec::new();
    let a = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 8, 4),
    )));
    let b = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 5, 4),
    )));
    let (muxer, log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let ta = spec.input_stream(a, 0).unwrap();
    let tb = spec.input_stream(b, 0).unwrap();
    spec.map(ta, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    spec.map(tb, output, &CodecParameters::new(MediaType::Audio))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    let log = log.lock().unwrap();
    assert_eq!(log.streams, vec![MediaType::Video, MediaType::Audio]);
    assert_eq!(log.packets.len(), 13);
    assert_eq!(log.packets.iter().filter(|p| p.0 == 0).count(), 8);
    assert_eq!(log.packets.iter().filter(|p| p.0 == 1).count(), 5);
    // The interleave queue orders across streams by DTS, so the two are
    // merged, not concatenated.
    assert!(log.packets.windows(2).all(|w| w[0].2 <= w[1].2));
}

#[test]
fn unmapped_streams_are_dropped_at_the_demuxer() {
    let mut all = packets(0, 6, 4);
    all.extend(packets(1, 6, 4));
    all.sort_by_key(|p| p.dts.ticks());
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB), video_stream(1, TB)],
        all,
    )));
    let (muxer, log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 1).unwrap();
    spec.map(tap, output, &CodecParameters::new(MediaType::Video))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    let log = log.lock().unwrap();
    assert_eq!(log.packets.len(), 6);
    // Stream 0's packets never entered a wire.
    assert_eq!(pipeline.stats().pushed, 6);
}

#[test]
fn threaded_matches_serial() {
    let build = || {
        let mut spec = PipelineSpec::new().with_capacity(Capacity::items(4));
        let a = spec.add_input(Box::new(ScriptedDemuxer::new(
            vec![video_stream(0, TB)],
            packets(0, 40, 64),
        )));
        let b = spec.add_input(Box::new(ScriptedDemuxer::new(
            vec![video_stream(0, TB)],
            packets(0, 40, 64),
        )));
        let (muxer, log) = RecordingMuxer::pair(MUX_TB);
        let output = spec.add_output(Box::new(muxer));
        let ta = spec.input_stream(a, 0).unwrap();
        let tb = spec.input_stream(b, 0).unwrap();
        let frames = spec.add_decoder(ta, decoder(3)).unwrap();
        let encoded = spec
            .add_encoder(frames, Box::new(MockEncoder::new(2)), TB)
            .unwrap();
        spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
            .unwrap();
        spec.map(tb, output, &CodecParameters::new(MediaType::Audio))
            .unwrap();
        (spec.build().unwrap(), log)
    };

    let (mut serial, serial_log) = build();
    assert_eq!(Driver::serial().run(&mut serial).unwrap(), Finish::Complete);

    for threads in [2, 4, 8] {
        let (mut parallel, parallel_log) = build();
        assert_eq!(
            Driver::with_threads(threads).run(&mut parallel).unwrap(),
            Finish::Complete
        );
        assert_eq!(
            serial_log.lock().unwrap().packets,
            parallel_log.lock().unwrap().packets,
            "{threads} threads produced a different muxer input order"
        );
    }
}

#[test]
fn driver_is_the_same_api_everywhere() {
    // The D18 claim, as an assertion: `with_threads` compiles and runs on every
    // target and reports what it actually got.
    let d = Driver::with_threads(8);
    if Driver::threads_available() {
        assert_eq!(d.threads(), 8);
    } else {
        assert_eq!(d.threads(), 1);
    }
    assert_eq!(Driver::with_threads(0).threads(), 1);
    assert_eq!(Driver::serial().threads(), 1);
}

#[test]
fn cancel_aborts_without_a_trailer() {
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 50, 4),
    )));
    let (muxer, log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let mut pipeline = spec.build().unwrap();

    for _ in 0..6 {
        assert_eq!(pipeline.step().unwrap(), Advance::Stepped);
    }
    pipeline.cancel();
    assert_eq!(pipeline.step().unwrap(), Advance::Idle);
    assert_eq!(pipeline.classify(), Finish::Cancelled);
    let log = log.lock().unwrap();
    assert!(!log.trailer, "an aborted output must not be finalised");
    assert!(log.packets.len() < 50);
}

#[test]
fn stop_reading_finishes_cleanly_and_short() {
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 50, 4),
    )));
    let (muxer, log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    let frames = spec.add_decoder(tap, decoder(3)).unwrap();
    let encoded = spec
        .add_encoder(frames, Box::new(MockEncoder::new(2)), TB)
        .unwrap();
    spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let mut pipeline = spec.build().unwrap();

    for _ in 0..20 {
        assert_eq!(pipeline.step().unwrap(), Advance::Stepped);
    }
    pipeline.stop_reading();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    let log = log.lock().unwrap();
    assert!(
        log.trailer,
        "a graceful stop must still finalise the output"
    );
    assert!(
        log.packets.len() < 50 && !log.packets.is_empty(),
        "got {} packets",
        log.packets.len()
    );
    // The tail the codecs were holding still came out.
    let seen: Vec<i64> = log.packets.iter().filter_map(|p| p.1).collect();
    assert_eq!(seen, (1..=seen.len() as i64).collect::<Vec<_>>());
}

#[test]
fn a_failing_component_cancels_the_pipeline() {
    #[derive(Debug)]
    struct Broken(Vec<Stream>);
    impl Demuxer for Broken {
        fn streams(&self) -> &[Stream] {
            &self.0
        }
        fn read_packet(&mut self) -> Result<Packet> {
            Err(Error::Io(std::io::Error::other("disk fell off")))
        }
        fn seek(&mut self, _t: SeekTarget, _f: SeekFlags) -> Result<()> {
            Err(Error::NotSeekable)
        }
    }
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(Broken(vec![video_stream(0, TB)])));
    let (muxer, log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let mut pipeline = spec.build().unwrap();

    // An unrecoverable error stops the run, and stops it *cancelled*: no
    // trailer, so a truncated output cannot pass for a finished one.
    assert!(matches!(pipeline.run(), Err(Error::Io(_))));
    assert_eq!(pipeline.classify(), Finish::Cancelled);
    assert!(!log.lock().unwrap().trailer);
    // And the pipeline stays stopped rather than resuming on the next call.
    assert_eq!(pipeline.step().unwrap(), Advance::Idle);
}

#[test]
fn a_tap_that_does_not_exist_is_an_error() {
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        Vec::new(),
    )));
    assert!(spec.input_stream(input, 7).is_err());
    assert_eq!(spec.input_stream_count(input), 1);
}

#[test]
fn encoder_time_base_rescales_frames_on_the_way_in() {
    // Stream at 1/1000, encoder at 1/50: pts 1..=10 ms become 0..=1 ticks after
    // rounding to nearest, and the muxer sees the encoder's base.
    const ENC_TB: Rational = Rational { num: 1, den: 50 };
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 200, 4),
    )));
    let (muxer, log) = RecordingMuxer::pair(ENC_TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    let frames = spec.add_decoder(tap, decoder(0)).unwrap();
    let encoded = spec
        .add_encoder(frames, Box::new(MockEncoder::new(0)), ENC_TB)
        .unwrap();
    spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let mut pipeline = spec.build().unwrap();
    // Several input timestamps collapse onto one output tick, which the
    // muxer's monotonicity check (M4) refuses. That is the correct answer:
    // rate conversion is a filter's job, not a silent repair here.
    let outcome = pipeline.run();
    match outcome {
        Err(Error::InvalidData(_)) => {}
        other => panic!("expected the muxer to refuse a non-advancing DTS, got {other:?}"),
    }
    assert!(!log.lock().unwrap().trailer);
}

#[test]
fn the_no_progress_guard_catches_a_livelock() {
    // A demuxer that returns recoverable errors forever: every step is
    // "progress" by the demuxer's own reckoning, so the guard cannot see it —
    // but the error budget can, and does.
    #[derive(Debug)]
    struct AlwaysCorrupt(Vec<Stream>);
    impl Demuxer for AlwaysCorrupt {
        fn streams(&self) -> &[Stream] {
            &self.0
        }
        fn read_packet(&mut self) -> Result<Packet> {
            Err(Error::InvalidData("corrupt"))
        }
        fn seek(&mut self, _t: SeekTarget, _f: SeekFlags) -> Result<()> {
            Err(Error::NotSeekable)
        }
    }
    let mut spec = PipelineSpec::new().with_max_input_errors(8);
    let input = spec.add_input(Box::new(AlwaysCorrupt(vec![video_stream(0, TB)])));
    let (muxer, _log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let mut pipeline = spec.build().unwrap();
    assert!(matches!(pipeline.run(), Err(Error::InvalidData(_))));
    assert_eq!(pipeline.classify(), Finish::Cancelled);
}

#[test]
fn a_tiny_budget_is_an_error_not_a_stall() {
    let mut spec = PipelineSpec::new().with_limits(Limits::permissive().with_alloc_total(16));
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 10, 4096),
    )));
    let (muxer, _log) = RecordingMuxer::pair(TB);
    let output = spec.add_output(Box::new(muxer));
    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, output, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let mut pipeline = spec.build().unwrap();
    assert!(matches!(pipeline.run(), Err(Error::LimitExceeded { .. })));
}

// --------------------------------------------------------------- properties

proptest::proptest! {
    /// Whatever the delays and however shallow the queues, every packet comes
    /// out exactly once and in order. This is the invariant the whole crate
    /// exists to preserve.
    #[test]
    fn every_packet_arrives_exactly_once(
        count in 1_usize..40,
        dec_delay in 0_usize..6,
        enc_delay in 0_usize..6,
        cap_items in 1_usize..8,
        threads in 1_usize..5,
    ) {
        let mut spec = PipelineSpec::new().with_capacity(Capacity::items(cap_items));
        let input = spec.add_input(Box::new(ScriptedDemuxer::new(
            vec![video_stream(0, TB)],
            packets(0, count as i64, 8),
        )));
        let (muxer, log) = RecordingMuxer::pair(TB);
        let output = spec.add_output(Box::new(muxer));
        let tap = spec.input_stream(input, 0).unwrap();
        let frames = spec.add_decoder(tap, decoder(dec_delay)).unwrap();
        let encoded = spec
            .add_encoder(frames, Box::new(MockEncoder::new(enc_delay)), TB)
            .unwrap();
        spec.map(encoded, output, &CodecParameters::new(MediaType::Video)).unwrap();

        let mut pipeline = spec.build().unwrap();
        proptest::prop_assert_eq!(
            Driver::with_threads(threads).run(&mut pipeline).unwrap(),
            Finish::Complete
        );
        let log = log.lock().unwrap();
        let seen: Vec<i64> = log.packets.iter().filter_map(|p| p.1).collect();
        proptest::prop_assert_eq!(seen, (1..=count as i64).collect::<Vec<_>>());
        proptest::prop_assert!(log.trailer);
    }

    /// The pipeline never holds more than it was allowed to, whatever the
    /// bound. Peak occupancy may exceed the item cap by a codec's delay — that
    /// is documented on `Capacity` — but never without limit.
    #[test]
    fn queues_respect_their_bound(
        cap_items in 1_usize..6,
        dec_delay in 0_usize..5,
    ) {
        let mut spec = PipelineSpec::new().with_capacity(Capacity::items(cap_items));
        let input = spec.add_input(Box::new(ScriptedDemuxer::new(
            vec![video_stream(0, TB)],
            packets(0, 60, 8),
        )));
        let (muxer, _log) = RecordingMuxer::pair(TB);
        let output = spec.add_output(Box::new(muxer));
        let tap = spec.input_stream(input, 0).unwrap();
        let frames = spec.add_decoder(tap, decoder(dec_delay)).unwrap();
        let encoded = spec
            .add_encoder(frames, Box::new(MockEncoder::new(0)), TB)
            .unwrap();
        spec.map(encoded, output, &CodecParameters::new(MediaType::Video)).unwrap();

        let mut pipeline = spec.build().unwrap();
        proptest::prop_assert_eq!(pipeline.run().unwrap(), Finish::Complete);
        let ceiling = cap_items + dec_delay + 1;
        for wire in pipeline.stats().wires {
            proptest::prop_assert!(
                wire.high_water <= ceiling,
                "a wire reached {} with a bound of {}",
                wire.high_water,
                cap_items
            );
        }
        proptest::prop_assert_eq!(pipeline.queued_bytes(), 0);
    }
}

// ------------------------------------------------------------ filter graphs

/// A decoder producing frames a real filter graph will accept: 16x16 Gray8,
/// with a configurable reorder delay so the drain still has something to give.
#[derive(Debug)]
struct GrayDecoder {
    held: std::collections::VecDeque<Timestamp>,
    ready: std::collections::VecDeque<Timestamp>,
    delay: usize,
    draining: bool,
}

impl GrayDecoder {
    fn new(delay: usize) -> Self {
        Self {
            held: std::collections::VecDeque::new(),
            ready: std::collections::VecDeque::new(),
            delay,
            draining: false,
        }
    }
}

impl Decoder for GrayDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        if self.draining {
            return Err(Error::Eof);
        }
        match packet {
            Some(p) => {
                self.held.push_back(p.pts);
                while self.held.len() > self.delay {
                    if let Some(ts) = self.held.pop_front() {
                        self.ready.push_back(ts);
                    }
                }
                Ok(())
            }
            None => {
                self.draining = true;
                while let Some(ts) = self.held.pop_front() {
                    self.ready.push_back(ts);
                }
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.ready.pop_front() {
            Some(ts) => Ok(vaco_filter_core::mock::gray_frame(
                16,
                16,
                ts.ticks().unwrap_or(0),
                0x20,
            )),
            None if self.draining => Err(Error::Eof),
            None => Err(Error::NeedMoreInput),
        }
    }

    fn flush(&mut self) {
        self.held.clear();
        self.ready.clear();
        self.draining = false;
    }
}

/// `[in] invert [out]`, configured and ready to schedule.
fn invert_graph(
    time_base: Rational,
) -> Result<(
    vaco_filter_core::Graph,
    vaco_filter_core::NodeId,
    vaco_filter_core::NodeId,
)> {
    use vaco_filter_core::mock;

    let mut graph = vaco_filter_core::Graph::new();
    let source = graph.add_source(
        "in",
        MediaType::Video,
        mock::video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
    );
    let invert = mock::Invert::node(&mut graph, "invert");
    let sink = graph.add_sink("out", MediaType::Video, mock::any_video_sink("out"));
    graph.connect(source, 0, invert, 0)?;
    graph.connect(invert, 0, sink, 0)?;
    graph.set_source_format(source, mock::gray_link(16, 16, time_base))?;
    graph.configure()?;
    Ok((graph, source, sink))
}

#[test]
fn a_filter_graph_in_the_middle_flushes_its_tail() {
    for threads in [1_usize, 4] {
        let (graph, source, sink) = invert_graph(TB).unwrap();
        let mut spec = PipelineSpec::new().with_capacity(Capacity::items(3));
        let input = spec.add_input(Box::new(ScriptedDemuxer::new(
            vec![video_stream(0, TB)],
            packets(0, 24, 4),
        )));
        let (muxer, log) = RecordingMuxer::pair(TB);
        let output = spec.add_output(Box::new(muxer));
        let tap = spec.input_stream(input, 0).unwrap();
        let frames = spec
            .add_decoder(tap, Box::new(GrayDecoder::new(3)))
            .unwrap();
        let filtered = spec
            .add_filter(
                graph,
                &[crate::SourceBind::new(frames, source, TB)],
                &[sink],
            )
            .unwrap();
        let encoded = spec
            .add_encoder(filtered[0], Box::new(MockEncoder::new(2)), TB)
            .unwrap();
        spec.map(encoded, output, &CodecParameters::new(MediaType::Video))
            .unwrap();

        let mut pipeline = spec.build().unwrap();
        assert_eq!(
            Driver::with_threads(threads).run(&mut pipeline).unwrap(),
            Finish::Complete,
            "{threads} threads"
        );
        let log = log.lock().unwrap();
        assert!(log.trailer);
        let seen: Vec<i64> = log.packets.iter().filter_map(|p| p.1).collect();
        assert_eq!(seen, (1..=24).collect::<Vec<_>>(), "{threads} threads");
    }
}

#[test]
fn a_filter_graph_feeds_two_outputs() {
    // The filter's frames go to one output; the input stream is copied to
    // another. Two routes from one input, with a graph on only one of them.
    let (graph, source, sink) = invert_graph(TB).unwrap();
    let mut spec = PipelineSpec::new();
    let input = spec.add_input(Box::new(ScriptedDemuxer::new(
        vec![video_stream(0, TB)],
        packets(0, 15, 4),
    )));
    let (copy_mux, copy_log) = RecordingMuxer::pair(TB);
    let (filt_mux, filt_log) = RecordingMuxer::pair(TB);
    let out_copy = spec.add_output(Box::new(copy_mux));
    let out_filt = spec.add_output(Box::new(filt_mux));

    let tap = spec.input_stream(input, 0).unwrap();
    spec.map(tap, out_copy, &CodecParameters::new(MediaType::Video))
        .unwrap();
    let frames = spec
        .add_decoder(tap, Box::new(GrayDecoder::new(1)))
        .unwrap();
    let filtered = spec
        .add_filter(
            graph,
            &[crate::SourceBind::new(frames, source, TB)],
            &[sink],
        )
        .unwrap();
    let encoded = spec
        .add_encoder(filtered[0], Box::new(MockEncoder::new(0)), TB)
        .unwrap();
    spec.map(encoded, out_filt, &CodecParameters::new(MediaType::Video))
        .unwrap();

    let mut pipeline = spec.build().unwrap();
    assert_eq!(pipeline.run().unwrap(), Finish::Complete);
    assert_eq!(copy_log.lock().unwrap().packets.len(), 15);
    assert_eq!(filt_log.lock().unwrap().packets.len(), 15);
}
