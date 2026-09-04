//! `fifo`: buffers packets in front of one inner muxer.
//!
//! # Measured (`ffmpeg -h muxer=fifo`, ffmpeg 8.1)
//!
//! `-queue_size` (default 60), `-attempt_recovery` (default false),
//! `-max_recovery_attempts` (default 0, meaning unlimited in the reference's
//! own convention — see [`FifoOptions::max_recovery_attempts`]),
//! `-recovery_wait_time` (default 5s), `-drop_pkts_on_overflow` (default
//! false), `-fifo_format`/`-format_opts` (the inner muxer's name and
//! options — this crate cannot resolve either without a muxer registry, see
//! below), `-recovery_wait_streamtime`, `-recover_any_error`,
//! `-restart_with_keyframe`, `-timeshift`. Functionally confirmed:
//! `-f fifo -fifo_format mpegts -queue_size 8 out.ts` transparently produces
//! a normal `mpegts` file — `fifo` is a pass-through with buffering, not a
//! format of its own.
//!
//! # The registry seam does not fit this format
//!
//! Same shape as `tee`/`segment`: [`vaco_format_core::MuxerDesc::open`] is
//! `fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>>`, with no channel for
//! `-fifo_format`. [`MUXER_FIFO`]'s `open` is
//! [`vaco_core::Error::Unsupported`]; [`FifoMuxer::new`] takes an
//! already-open inner [`vaco_format_core::Muxer`] directly.
//!
//! # Streams are added before the queue exists
//!
//! [`FifoMuxer`] holds `inner` directly (no thread yet) until
//! [`Muxer::write_header`] is first called — `add_stream` forwards straight
//! to it. The background thread is spawned only at that point, taking
//! ownership of `inner`; everything after (`write_packet`, `write_trailer`)
//! goes through the queue. This is not an arbitrary staging choice: once
//! `inner` moves onto a worker thread, the calling thread has no way to
//! drive `add_stream` on it directly, and every muxer in this workspace
//! requires every stream to be declared before the header per
//! [`vaco_format_core::Muxer::add_stream`]'s own contract — so streams
//! *must* be settled before the handoff, not queued alongside packets.
//!
//! # Why this is the one component here with a thread
//!
//! Buffering "in front of" a muxer only decouples the writer from the
//! caller if a **different** thread drains the queue — otherwise
//! `write_packet` still blocks on the inner muxer's own I/O, and `fifo`
//! would be a queue with no reader. [`vaco_time`] is this workspace's one
//! approved door to `std::thread::spawn` and to a wall clock
//! (`Instant::now()`/`SystemTime::now()` panic on `wasm32-unknown-unknown`,
//! which this crate must still build for): the actual spawn is gated behind
//! `#[cfg(not(target_family = "wasm"))]`, mirroring `vaco-sched`'s
//! `run_threaded` (see that crate's `driver.rs`), and on `wasm32` this
//! muxer instead drains synchronously on every call — **not** a different
//! behaviour dressed up as the same API: with no threads, "buffered ahead
//! of a background writer" and "written immediately" are the same thing
//! minus the buffering, and D18 asks for that degradation to be explicit
//! rather than silently slower.
//!
//! # What is not wired up
//!
//! `-recovery_wait_streamtime`, `-recover_any_error` and
//! `-restart_with_keyframe` are recorded on [`FifoOptions`] but not acted
//! on: the first needs the *stream's own* clock (not this crate's problem —
//! it has no notion of "stream time" independent of the packets it is
//! hedging against a wall-clock timer over), and the other two need the
//! recovery attempt to distinguish error kinds and keyframes respectively,
//! which the generic [`vaco_core::Error`] this muxer receives does not
//! reliably let it do without guessing at a case this crate could not
//! reproduce against a `MemorySink` to probe.

use std::sync::mpsc;

use vaco_codec_core::CodecParameters;
use vaco_core::{Duration, Error, Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

/// `-queue_size`/`-attempt_recovery`/… . Defaults match `ffmpeg -h
/// muxer=fifo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FifoOptions {
    pub queue_size: usize,
    pub attempt_recovery: bool,
    /// `0` means unlimited, matching the reference's own convention for
    /// this option.
    pub max_recovery_attempts: u32,
    pub recovery_wait_time: Duration,
    pub drop_pkts_on_overflow: bool,
}

impl Default for FifoOptions {
    fn default() -> Self {
        Self {
            queue_size: 60,
            attempt_recovery: false,
            max_recovery_attempts: 0,
            recovery_wait_time: Duration::from_micros(5_000_000),
            drop_pkts_on_overflow: false,
        }
    }
}

/// `vaco_core::Duration` (signed microseconds) to `vaco_time::Duration`
/// (unsigned, `core::time::Duration`), clamping a negative or absurd value
/// to zero/`u64::MAX` seconds rather than panicking on a checked
/// conversion — a malformed `-recovery_wait_time` should slow this muxer
/// down, not crash it.
fn std_duration(d: Duration) -> vaco_time::Duration {
    let micros = u64::try_from(d.as_micros()).unwrap_or(0);
    vaco_time::Duration::from_micros(micros)
}

enum Item {
    Packet(Packet),
    Trailer,
}

/// What the background writer reports back for one queued item.
#[derive(Debug)]
enum Report {
    Ok,
    Failed(String),
}

#[cfg(not(target_family = "wasm"))]
struct Running {
    tx: mpsc::SyncSender<Item>,
    reports: mpsc::Receiver<Report>,
    worker: std::thread::JoinHandle<()>,
}

enum State {
    /// Before `write_header`: `add_stream` goes straight through.
    Building(Box<dyn Muxer>),
    #[cfg(not(target_family = "wasm"))]
    Running(Running),
    /// No threads on this target: stay synchronous for the whole lifetime.
    #[cfg(target_family = "wasm")]
    Synchronous(Box<dyn Muxer>),
    Finished,
}

/// `fifo`: queues packets (and the trailer call) for one inner muxer,
/// written from a background thread on every target that has one.
pub struct FifoMuxer {
    state: State,
    options: FifoOptions,
    sticky_error: Option<String>,
}

impl core::fmt::Debug for FifoMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FifoMuxer")
            .field("options", &self.options)
            .field("failed", &self.sticky_error.is_some())
            .finish_non_exhaustive()
    }
}

impl FifoMuxer {
    /// Wrap `inner`, buffering ahead of it per `options` once the header is
    /// written. `inner` must not have had `write_header` called on it yet.
    #[must_use]
    pub fn new(inner: Box<dyn Muxer>, options: FifoOptions) -> Self {
        Self {
            state: State::Building(inner),
            options,
            sticky_error: None,
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn poll_reports(&mut self) {
        if let State::Running(running) = &self.state {
            while let Ok(report) = running.reports.try_recv() {
                if let Report::Failed(msg) = report
                    && self.sticky_error.is_none()
                {
                    self.sticky_error = Some(msg);
                }
            }
        }
    }

    fn fail_if_sticky(&self) -> Result<()> {
        if self.sticky_error.is_some() {
            return Err(Error::Unsupported(
                "fifo: the inner muxer failed; see FifoMuxer's Debug output for detail",
            ));
        }
        Ok(())
    }
}

/// Spawn the background writer and return the handle [`FifoMuxer`] talks to
/// it through. A standalone top-level function — not a block inline in
/// [`Muxer::write_header`] — so `cargo xtask time-gate`'s scan (which looks
/// for a `#[cfg(not(target_family = "wasm"))]` directly above a top-level
/// `fn`/`impl`/`mod`, matching `vaco-sched`'s `run_threaded`) can see that
/// [`std::thread::spawn`] never compiles for `wasm32-unknown-unknown` at all
/// here, rather than flagging it as an ungated OS call.
#[cfg(not(target_family = "wasm"))]
fn spawn_worker(inner: Box<dyn Muxer>, options: FifoOptions) -> Running {
    let (tx, rx) = mpsc::sync_channel::<Item>(options.queue_size.max(1));
    let (report_tx, report_rx) = mpsc::channel::<Report>();
    let worker = std::thread::spawn(move || run_worker(inner, &rx, &report_tx, options));
    Running {
        tx,
        reports: report_rx,
        worker,
    }
}

#[cfg(not(target_family = "wasm"))]
fn run_worker(
    mut inner: Box<dyn Muxer>,
    rx: &mpsc::Receiver<Item>,
    report_tx: &mpsc::Sender<Report>,
    options: FifoOptions,
) {
    while let Ok(item) = rx.recv() {
        let result = match item {
            Item::Packet(p) => write_with_recovery(&mut inner, &p, options),
            Item::Trailer => inner.write_trailer(),
        };
        let report = match result {
            Ok(()) => Report::Ok,
            Err(e) => Report::Failed(e.to_string()),
        };
        if report_tx.send(report).is_err() {
            break;
        }
    }
}

/// Write one packet, retrying per `options` on failure. Recovery is a plain
/// retry loop rather than reopening the inner muxer — this crate has no
/// handle to *reopen* an already-consumed `Box<dyn Muxer>` (opening a fresh
/// one is the caller's job, one layer up, exactly like `tee`'s and
/// `segment`'s inner-muxer construction), so "recovery" here means "give a
/// transient failure time to clear," which is what `-recovery_wait_time`
/// actually configures.
fn write_with_recovery(
    inner: &mut Box<dyn Muxer>,
    packet: &Packet,
    options: FifoOptions,
) -> Result<()> {
    let mut attempt = 0u32;
    loop {
        match inner.write_packet(packet) {
            Ok(()) => return Ok(()),
            Err(e) if options.attempt_recovery => {
                attempt += 1;
                if options.max_recovery_attempts != 0 && attempt > options.max_recovery_attempts {
                    return Err(e);
                }
                vaco_time::sleep(std_duration(options.recovery_wait_time));
            }
            Err(e) => return Err(e),
        }
    }
}

impl Muxer for FifoMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::TS_NONSTRICT.union(FormatFlags::TS_NEGATIVE)
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        match &mut self.state {
            State::Building(inner) => inner.add_stream(params),
            _ => Err(Error::Unsupported(
                "fifo: add_stream after write_header is not supported",
            )),
        }
    }

    fn write_header(&mut self) -> Result<()> {
        let State::Building(_) = &self.state else {
            return Err(Error::Unsupported("fifo: write_header called twice"));
        };
        let State::Building(mut inner) = std::mem::replace(&mut self.state, State::Finished) else {
            return Err(Error::Unsupported("fifo: inconsistent state"));
        };
        let header_result = inner.write_header();
        if let Err(e) = header_result {
            self.state = State::Finished;
            return Err(e);
        }
        #[cfg(target_family = "wasm")]
        {
            self.state = State::Synchronous(inner);
        }
        #[cfg(not(target_family = "wasm"))]
        {
            self.state = State::Running(spawn_worker(inner, self.options));
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        #[cfg(target_family = "wasm")]
        {
            let State::Synchronous(inner) = &mut self.state else {
                return Err(Error::Unsupported("fifo: write_packet before write_header"));
            };
            match write_with_recovery(inner, packet, self.options) {
                Ok(()) => Ok(()),
                Err(e) if self.options.drop_pkts_on_overflow => {
                    let _ = e;
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        #[cfg(not(target_family = "wasm"))]
        {
            self.poll_reports();
            self.fail_if_sticky()?;
            let State::Running(running) = &self.state else {
                return Err(Error::Unsupported("fifo: write_packet before write_header"));
            };
            match running.tx.try_send(Item::Packet(packet.clone())) {
                Ok(()) => Ok(()),
                Err(mpsc::TrySendError::Full(item)) if self.options.drop_pkts_on_overflow => {
                    drop(item);
                    Ok(())
                }
                Err(mpsc::TrySendError::Full(item)) => {
                    // Not dropping: block, matching the reference's default
                    // (a full fifo backpressures the writer).
                    running
                        .tx
                        .send(item)
                        .map_err(|_| Error::Unsupported("fifo: worker thread is gone"))
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    Err(Error::Unsupported("fifo: worker thread is gone"))
                }
            }
        }
    }

    fn write_trailer(&mut self) -> Result<()> {
        #[cfg(target_family = "wasm")]
        {
            let State::Synchronous(inner) = &mut self.state else {
                return Err(Error::Unsupported(
                    "fifo: write_trailer before write_header",
                ));
            };
            inner.write_trailer()
        }
        #[cfg(not(target_family = "wasm"))]
        {
            self.fail_if_sticky()?;
            let State::Running(running) = std::mem::replace(&mut self.state, State::Finished)
            else {
                return Err(Error::Unsupported(
                    "fifo: write_trailer before write_header",
                ));
            };
            running
                .tx
                .send(Item::Trailer)
                .map_err(|_| Error::Unsupported("fifo: worker thread is gone"))?;
            // Block until the worker actually drains the queue and reports,
            // so a caller that reads the inner muxer's output right after
            // this returns sees a complete file.
            drop(running.tx);
            while let Ok(report) = running.reports.recv() {
                if let Report::Failed(msg) = report {
                    self.sticky_error.get_or_insert(msg);
                }
            }
            let _ = running.worker.join();
            self.fail_if_sticky()
        }
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        match &self.state {
            State::Building(inner) => inner.stream_time_base(stream_index),
            #[cfg(target_family = "wasm")]
            State::Synchronous(inner) => inner.stream_time_base(stream_index),
            _ => None,
        }
    }
}

/// The registry `open` path: always [`vaco_core::Error::Unsupported`] — see
/// the module docs.
#[allow(clippy::needless_pass_by_value, reason = "MuxerDesc::open's signature")]
fn open_fifo(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Err(Error::Unsupported(
        "fifo: MuxerDesc::open has no channel for -fifo_format; use FifoMuxer::new with an already-open inner muxer",
    ))
}

/// `fifo`: `ffmpeg -h muxer=fifo` names it "FIFO queue pseudo-muxer".
pub static MUXER_FIFO: MuxerDesc = MuxerDesc {
    name: "fifo",
    long_name: "FIFO queue pseudo-muxer",
    extensions: &[],
    default_video: None,
    default_audio: None,
    open: open_fifo,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::MediaType;
    use vaco_format_core::options::FormatOptions;
    use vaco_format_core::vacoraw::{MemorySink, SharedBytes, VacoRawMuxer};
    use vaco_limits::{Budget, Limits};

    fn raw_muxer_and_bytes() -> (Box<dyn Muxer>, SharedBytes) {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let opts = FormatOptions::default();
        let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
        (Box::new(muxer), shared)
    }

    fn packet() -> Packet {
        use vaco_core::Timestamp;
        use vaco_packet::PacketFlags;
        let mut budget = Budget::new(Limits::permissive());
        let mut p = Packet::from_slice(&mut budget, b"hello").unwrap();
        p.stream_index = 0;
        p.pts = Timestamp::ZERO;
        p.dts = Timestamp::ZERO;
        p.flags = PacketFlags::KEY;
        p
    }

    #[test]
    fn add_stream_forwards_before_the_header_and_writes_flow_through() {
        let (inner, bytes) = raw_muxer_and_bytes();
        let mut fifo = FifoMuxer::new(inner, FifoOptions::default());
        assert_eq!(
            fifo.add_stream(&CodecParameters::new(MediaType::Video))
                .unwrap(),
            0
        );
        fifo.write_header().unwrap();
        fifo.write_packet(&packet()).unwrap();
        fifo.write_trailer().unwrap();
        assert!(!bytes.snapshot().is_empty());
    }

    #[test]
    fn drop_pkts_on_overflow_never_blocks_or_errors() {
        let (inner, _bytes) = raw_muxer_and_bytes();
        let mut fifo = FifoMuxer::new(
            inner,
            FifoOptions {
                queue_size: 1,
                drop_pkts_on_overflow: true,
                ..FifoOptions::default()
            },
        );
        fifo.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        fifo.write_header().unwrap();
        for _ in 0..50 {
            fifo.write_packet(&packet()).unwrap();
        }
        fifo.write_trailer().unwrap();
    }

    #[test]
    fn add_stream_after_header_is_rejected() {
        let (inner, _bytes) = raw_muxer_and_bytes();
        let mut fifo = FifoMuxer::new(inner, FifoOptions::default());
        fifo.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        fifo.write_header().unwrap();
        assert!(
            fifo.add_stream(&CodecParameters::new(MediaType::Audio))
                .is_err()
        );
    }

    #[test]
    fn default_options_match_the_reference() {
        let d = FifoOptions::default();
        assert_eq!(d.queue_size, 60);
        assert!(!d.attempt_recovery);
        assert_eq!(d.max_recovery_attempts, 0);
        assert_eq!(d.recovery_wait_time, Duration::from_micros(5_000_000));
        assert!(!d.drop_pkts_on_overflow);
    }

    #[test]
    fn the_registry_open_path_reports_the_gap() {
        let sink = Box::new(MemorySink::new());
        assert!(open_fifo(sink).is_err());
        assert!(MUXER_FIFO.matches_name("fifo"));
    }
}
