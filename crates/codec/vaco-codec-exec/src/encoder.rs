//! [`Encoder`] impls that spawn `x264`/`x265` as a child process.
//!
//! # Two different input plumbings, and why
//!
//! `x264 --demuxer y4m` reads a Y4M stream from stdin (`-`) directly —
//! measured working via a real pipe, see [`crate::process`]'s tests. The
//! `x265` binary on this machine does **not**: `--input -` (or any
//! non-seekable path without a `.y4m` suffix) fails outright with
//! `unable to open input file`, even though the same bytes through a named
//! pipe *do* work the moment the path ends in `.y4m` — x265 selects its Y4M
//! reader from the filename suffix, not by sniffing the stream's own magic
//! header. Measured directly (D17), not assumed:
//!
//! ```text
//! $ mkfifo in.y4m && (cat test.y4m > in.y4m &) \
//!     && x265 --input in.y4m --bframes 0 --aud -o - --bitrate 200 > out.hevc
//! encoded 5 frames ...                              # works
//! $ mkfifo in2 && (cat test.y4m > in2 &) && x265 --input in2 ...
//! x265 [error]: yuv: width, height, and FPS must be specified   # fails, same bytes
//! ```
//!
//! So `x264` streams over a pipe as frames arrive (see [`InputMode::Stdin`]);
//! `x265` buffers the whole clip to a real temporary file named `*.y4m`
//! first, and is not spawned until end of stream closes it (see
//! [`InputMode::TempY4mFile`]). Both converge on the same output path: read
//! Annex-B off stdout, split on `--aud` boundaries (`crate::annexb`).

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use vaco_codec_core::machine::{Accept, Machine};
use vaco_codec_core::{Caps, CodecId, Encoder, EncoderDesc, EncoderPass};
use vaco_core::{Duration, Error, MediaType, Result, Timestamp};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

use crate::annexb::{NalFamily, Splitter, is_keyframe, to_buffer};
use crate::process::{ExecProcess, is_on_path};
use crate::y4m::{self, Y4mGeometry};

/// How this tool wants its Y4M input delivered — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Stdin,
    TempY4mFile,
}

/// Everything that differs between `x264` and `x265`: the binary name, the
/// NAL family (for AUD/keyframe recognition), how it wants input, and how
/// its command line spells "bitrate" and "quality".
struct ToolSpec {
    program: &'static str,
    family: NalFamily,
    input_mode: InputMode,
    /// Fixed arguments every invocation needs: y4m demuxing, `--aud`, and
    /// `--bframes 0` (see the crate doc for why B-frames are refused).
    fixed_args: &'static [&'static str],
    output_args: fn() -> Vec<String>,
    bitrate_flag: &'static str,
    crf_flag: &'static str,
}

const X264: ToolSpec = ToolSpec {
    program: "x264",
    family: NalFamily::H264,
    input_mode: InputMode::Stdin,
    fixed_args: &["--demuxer", "y4m", "--aud", "--bframes", "0", "--quiet"],
    output_args: || vec!["-o".to_owned(), "-".to_owned(), "-".to_owned()],
    bitrate_flag: "--bitrate",
    crf_flag: "--crf",
};

const X265: ToolSpec = ToolSpec {
    program: "x265",
    family: NalFamily::H265,
    input_mode: InputMode::TempY4mFile,
    fixed_args: &["--aud", "--bframes", "0", "--log-level", "error"],
    output_args: || vec!["-o".to_owned(), "-".to_owned()],
    bitrate_flag: "--bitrate",
    crf_flag: "--crf",
};

/// A rate-control choice from [`Encoder::set_option`]. `None` means "let the
/// tool use its own default", exactly like every other encoder in this
/// workspace treats an unset option.
#[derive(Debug, Clone, Copy, Default)]
struct RateChoice {
    bitrate_kbps: Option<u32>,
    crf: Option<f64>,
}

/// An [`Encoder`] that spawns [`ToolSpec::program`] and pipes frames through
/// it. See the module doc for the two input plumbings this switches between.
pub struct ExecEncoder {
    tool: &'static ToolSpec,
    machine: Machine<Packet>,
    limits: Limits,
    rate: RateChoice,
    geometry: Option<Y4mGeometry>,
    proc: Option<ExecProcess>,
    splitter: Splitter,
    /// `(pts, duration)` of every frame sent, in order, matched one-to-one
    /// against completed access units as they arrive — exact because
    /// `--bframes 0` guarantees the tool never reorders output relative to
    /// input (see the crate doc).
    timings: VecDeque<(Timestamp, Duration)>,
    /// [`InputMode::TempY4mFile`] state: the open file frames are written
    /// to, and the path to spawn the tool against and delete afterwards.
    temp_file: Option<File>,
    temp_path: Option<PathBuf>,
    scratch: Option<PathBuf>,
    pass: EncoderPass,
}

impl std::fmt::Debug for ExecEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecEncoder")
            .field("tool", &self.tool.program)
            .finish_non_exhaustive()
    }
}

impl ExecEncoder {
    fn new(tool: &'static ToolSpec, limits: Limits) -> Self {
        Self {
            tool,
            machine: Machine::new(Caps::DELAY | Caps::AVOID_PROBING),
            limits,
            rate: RateChoice::default(),
            geometry: None,
            proc: None,
            splitter: Splitter::new(tool.family),
            timings: VecDeque::new(),
            temp_file: None,
            temp_path: None,
            scratch: None,
            pass: EncoderPass::Single,
        }
    }

    fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = self.tool.fixed_args.iter().map(|&s| s.to_owned()).collect();
        if !matches!(self.pass, EncoderPass::Single) {
            args.extend([
                "--pass".to_owned(),
                if matches!(self.pass, EncoderPass::First) {
                    "1"
                } else {
                    "2"
                }
                .to_owned(),
            ]);
            if let Some(scratch) = &self.scratch {
                args.extend([
                    "--stats".to_owned(),
                    scratch.join("pass.log").display().to_string(),
                ]);
            }
            // Keep the opaque statistics in one file rather than requiring
            // codec-specific sidecar files in the caller's pass-log API.
            args.push(
                if self.tool.program == "x264" {
                    "--no-mbtree"
                } else {
                    "--no-cutree"
                }
                .to_owned(),
            );
        }
        if let Some(kbps) = self.rate.bitrate_kbps {
            args.push(self.tool.bitrate_flag.to_owned());
            args.push(kbps.to_string());
        } else if let Some(crf) = self.rate.crf {
            args.push(self.tool.crf_flag.to_owned());
            args.push(format!("{crf}"));
        }
        if self.tool.input_mode == InputMode::TempY4mFile {
            let path = self
                .temp_path
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string());
            args.push("--input".to_owned());
            args.push(path);
        }
        args.extend((self.tool.output_args)());
        args
    }

    /// Spawn the child for [`InputMode::Stdin`], or write the header into a
    /// freshly created temp file for [`InputMode::TempY4mFile`]. Called once,
    /// on the first frame.
    fn start(&mut self, geometry: Y4mGeometry) -> Result<()> {
        if !is_on_path(self.tool.program) {
            return Err(Error::Unsupported(match self.tool.program {
                "x264" => "vaco-codec-exec: 'x264' is not installed (libx264 needs it on PATH)",
                _ => "vaco-codec-exec: 'x265' is not installed (libx265 needs it on PATH)",
            }));
        }
        if !matches!(self.pass, EncoderPass::Single) && self.rate.bitrate_kbps.is_none() {
            return Err(Error::Option {
                name: "pass".to_owned(),
                detail: "two-pass encoding requires a positive target bitrate (-b:v)".to_owned(),
            });
        }
        let scratch = self.scratch_dir()?;
        if let EncoderPass::Second(stats) = &self.pass {
            std::fs::write(scratch.join("pass.log"), stats).map_err(Error::Io)?;
        }
        self.geometry = Some(geometry);
        match self.tool.input_mode {
            InputMode::Stdin => {
                let args = self.build_args();
                let mut proc = ExecProcess::spawn(self.tool.program, &args)?;
                let mut header = Vec::new();
                y4m::write_header(&mut header, &geometry).map_err(Error::Io)?;
                proc.write_stdin(&header)?;
                self.proc = Some(proc);
            }
            InputMode::TempY4mFile => {
                let path = scratch.join("input.y4m");
                let mut file = File::create(&path).map_err(Error::Io)?;
                y4m::write_header(&mut file, &geometry).map_err(Error::Io)?;
                self.temp_path = Some(path);
                self.temp_file = Some(file);
            }
        }
        Ok(())
    }

    fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        match self.tool.input_mode {
            InputMode::Stdin => {
                let proc = self
                    .proc
                    .as_mut()
                    .ok_or(Error::Unsupported("vaco-codec-exec: encoder not started"))?;
                let mut bytes = Vec::new();
                y4m::write_frame(&mut bytes, frame)?;
                proc.write_stdin(&bytes)
            }
            InputMode::TempY4mFile => {
                let file = self
                    .temp_file
                    .as_mut()
                    .ok_or(Error::Unsupported("vaco-codec-exec: encoder not started"))?;
                y4m::write_frame(file, frame)
            }
        }
    }

    /// Turn newly-arrived stdout bytes into queued packets.
    fn ingest(&mut self, chunk: &[u8]) -> Result<()> {
        let mut budget = Budget::new(self.limits.clone());
        let units = self.splitter.push(chunk, &mut budget)?;
        for unit in units {
            self.emit_unit(&unit)?;
        }
        Ok(())
    }

    fn emit_unit(&mut self, unit: &[u8]) -> Result<()> {
        let (pts, duration) = self
            .timings
            .pop_front()
            .unwrap_or((Timestamp::NONE, Duration::ZERO));
        let mut budget = Budget::new(self.limits.clone());
        let buffer = to_buffer(unit, &mut budget)?;
        let mut packet = Packet::new(buffer, unit.len());
        packet.pts = pts;
        packet.dts = pts;
        packet.duration = duration;
        if is_keyframe(unit, self.tool.family) {
            packet.flags = PacketFlags::KEY;
        }
        self.machine.emit(packet);
        Ok(())
    }

    /// Non-blocking: pull whatever the reader thread has queued so far.
    fn poll(&mut self) -> Result<()> {
        if let Some(proc) = &self.proc {
            let chunk = proc.try_recv_stdout();
            if !chunk.is_empty() {
                self.ingest(&chunk)?;
            }
        }
        Ok(())
    }

    /// Runs exactly once, from the `Accept::Drain` arm: close stdin
    /// (`Stdin`) or flush the temp file and spawn the child against it
    /// (`TempY4mFile`). Does not block on the child finishing — that is
    /// `receive_packet`'s job, since it can take real wall-clock time.
    fn begin_drain(&mut self) -> Result<()> {
        match self.tool.input_mode {
            InputMode::Stdin => {
                if let Some(proc) = &mut self.proc {
                    proc.close_stdin();
                }
            }
            InputMode::TempY4mFile => {
                if let Some(mut file) = self.temp_file.take() {
                    file.flush().map_err(Error::Io)?;
                }
                if self.geometry.is_some() {
                    let args = self.build_args();
                    self.proc = Some(ExecProcess::spawn(self.tool.program, &args)?);
                }
            }
        }
        Ok(())
    }

    /// Blocks until the child has genuinely produced its next chunk of
    /// output or exited. Only called once feeding is over and the queue is
    /// empty — see [`Encoder::receive_packet`].
    fn block_for_more(&mut self) -> Result<()> {
        // No frames were ever sent (or the tool never started for some other
        // reason): nothing more will ever arrive.
        let Some(chunk) = self
            .proc
            .as_ref()
            .and_then(ExecProcess::recv_stdout_blocking)
        else {
            if self.proc.is_none() {
                self.machine.finish();
                return Ok(());
            }
            // Reader thread saw EOF: wait() joins it and surfaces a
            // non-zero exit as a real error rather than silent truncation.
            if let Some(proc) = self.proc.as_mut() {
                proc.wait()?;
            }
            let tail =
                std::mem::replace(&mut self.splitter, Splitter::new(self.tool.family)).finish();
            if let Some(tail) = tail {
                self.emit_unit(&tail)?;
            }
            self.cleanup_temp_file();
            self.machine.finish();
            return Ok(());
        };
        self.ingest(&chunk)
    }

    fn cleanup_temp_file(&mut self) {
        if let Some(path) = self.temp_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn scratch_dir(&mut self) -> Result<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        if let Some(path) = &self.scratch {
            return Ok(path.clone());
        }
        loop {
            let path = std::env::temp_dir().join(format!(
                "vaco-codec-exec-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    self.scratch = Some(path.clone());
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }
}

impl Encoder for ExecEncoder {
    fn set_pass(&mut self, pass: EncoderPass) -> Result<()> {
        if self.geometry.is_some() {
            return Err(Error::InvalidData(
                "cannot change encoding pass after the first frame",
            ));
        }
        if matches!(&pass, EncoderPass::Second(stats) if stats.is_empty()) {
            return Err(Error::InvalidData("second-pass statistics are empty"));
        }
        self.pass = pass;
        Ok(())
    }

    fn pass_stats(&self) -> Result<Option<Vec<u8>>> {
        if !matches!(self.pass, EncoderPass::First) {
            return Ok(None);
        }
        if self.machine.stage() != vaco_codec_core::Stage::Drained {
            return Err(Error::InvalidData(
                "first-pass statistics requested before drain completed",
            ));
        }
        let Some(scratch) = &self.scratch else {
            return Ok(Some(Vec::new()));
        };
        std::fs::read(scratch.join("pass.log"))
            .map(Some)
            .map_err(Error::Io)
    }

    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        self.poll()?;
        match self.machine.accept(frame.is_none())? {
            Accept::Drain => self.begin_drain(),
            Accept::Input => {
                let Some(frame) = frame else { return Ok(()) };
                if self.geometry.is_none() {
                    let geometry = Y4mGeometry::from_frame(frame)?;
                    self.start(geometry)?;
                }
                self.timings.push_back((frame.pts, frame.duration));
                self.write_frame(frame)?;
                self.poll()
            }
        }
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        loop {
            match self.machine.receive() {
                Ok(packet) => return Ok(packet),
                Err(Error::NeedMoreInput) if self.draining_now() => self.block_for_more()?,
                Err(e) => return Err(e),
            }
        }
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.cleanup_temp_file();
        self.proc = None;
        self.geometry = None;
        self.temp_file = None;
        self.splitter = Splitter::new(self.tool.family);
        self.timings.clear();
    }

    fn accepted_pix_fmts(&self) -> &'static [PixFmt] {
        &[y4m::SUPPORTED_FORMAT]
    }

    /// `"b"` (bits/second, the CLI's `-b:v`) sets a target bitrate; `"crf"`/
    /// `"qscale"`/`"global_quality"` sets the tool's own quality-based VBR
    /// knob directly (the value is passed straight through as `--crf`; the
    /// two tools' CRF scales are not identical, which is exactly the kind of
    /// per-codec quality difference this option was never meant to hide).
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "b" => {
                let bps: f64 = value.parse().map_err(|_| Error::Option {
                    name: "b".to_owned(),
                    detail: format!("expected a bitrate in bits/second, got '{value}'"),
                })?;
                if bps > 0.0 {
                    self.rate = RateChoice {
                        bitrate_kbps: Some((bps / 1000.0).round() as u32),
                        crf: None,
                    };
                }
                Ok(())
            }
            "crf" | "qscale" | "global_quality" => {
                let crf: f64 = value.parse().map_err(|_| Error::Option {
                    name: key.to_owned(),
                    detail: format!("expected a quality value, got '{value}'"),
                })?;
                self.rate = RateChoice {
                    bitrate_kbps: None,
                    crf: Some(crf),
                };
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl Drop for ExecEncoder {
    fn drop(&mut self) {
        self.proc = None;
        self.temp_file = None;
        if let Some(path) = self.scratch.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl ExecEncoder {
    fn draining_now(&self) -> bool {
        matches!(
            self.machine.stage(),
            vaco_codec_core::machine::Stage::Draining
        )
    }
}

/// `vaco-component.toml`'s registration point for `libx264`.
pub static LIBX264_ENCODER: EncoderDesc = EncoderDesc {
    name: "libx264",
    long_name: "H.264 via a user-installed x264 binary (process boundary, C-46)",
    id: CodecId::H264,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(ExecEncoder::new(&X264, limits)),
};

/// `vaco-component.toml`'s registration point for `libx265`.
pub static LIBX265_ENCODER: EncoderDesc = EncoderDesc {
    name: "libx265",
    long_name: "HEVC via a user-installed x265 binary (process boundary, C-46)",
    id: CodecId::Hevc,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(ExecEncoder::new(&X265, limits)),
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the crate, not the untrusted-input surface"
)]
mod tests {
    use vaco_pixfmt::PixFmt;

    use super::*;

    /// 64x64: below this, real `x265` refuses the input outright
    /// (`unable to open input file`, even though `ffmpeg`'s own Y4M mux
    /// produced it correctly) — measured directly, not assumed. 32x32 and
    /// smaller fail; 64x64 is the smallest size that worked in that sweep.
    /// `x264` has no such floor, but one fixture size covers both tools.
    fn synth_frame(budget: &mut Budget, n: u32) -> Frame {
        let mut frame = Frame::alloc_video(budget, PixFmt::Yuv420p, 64, 64).expect("alloc");
        if let vaco_frame::FrameData::Video { planes, .. } = &mut frame.data {
            for plane in planes.iter_mut() {
                let v = u8::try_from((n * 17) % 251).unwrap_or(0);
                plane.data.make_mut().fill(v);
            }
        }
        frame.pts = Timestamp::from(i64::from(n));
        frame.time_base = vaco_core::Rational { num: 1, den: 25 };
        frame.set_duration_ticks(1);
        frame
    }

    /// Full round trip against the real, user-installed `x264` if present on
    /// this machine's `PATH`; skips (loudly) otherwise rather than reporting
    /// a false pass — see AGENT-CONSTRAINTS.md on tests that skip on error.
    #[test]
    fn x264_encodes_a_short_synthetic_clip() {
        if !is_on_path("x264") {
            eprintln!("skipping: x264 not on PATH");
            return;
        }
        let limits = Limits::permissive();
        let mut enc = ExecEncoder::new(&X264, limits.clone());
        let mut budget = Budget::new(limits);
        for n in 0..5 {
            let frame = synth_frame(&mut budget, n);
            enc.send_frame(Some(&frame)).expect("send_frame");
        }
        enc.send_frame(None).expect("send_frame(None)");

        let mut packets = Vec::new();
        loop {
            match enc.receive_packet() {
                Ok(p) => packets.push(p),
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(
            packets.len(),
            5,
            "one access unit per input frame (bframes=0)"
        );
        assert!(
            packets[0].flags.contains(PacketFlags::KEY),
            "first frame is a keyframe"
        );
        assert!(
            packets[0]
                .data
                .as_slice()
                .windows(4)
                .any(|w| w == [0, 0, 0, 1])
                || packets[0]
                    .data
                    .as_slice()
                    .windows(3)
                    .any(|w| w == [0, 0, 1])
        );
    }

    /// The [`InputMode::TempY4mFile`] path — real `x265`, if installed.
    #[test]
    fn x265_encodes_a_short_synthetic_clip() {
        if !is_on_path("x265") {
            eprintln!("skipping: x265 not on PATH");
            return;
        }
        let limits = Limits::permissive();
        let mut enc = ExecEncoder::new(&X265, limits.clone());
        let mut budget = Budget::new(limits);
        for n in 0..5 {
            let frame = synth_frame(&mut budget, n);
            enc.send_frame(Some(&frame)).expect("send_frame");
        }
        enc.send_frame(None).expect("send_frame(None)");

        let mut packets = Vec::new();
        loop {
            match enc.receive_packet() {
                Ok(p) => packets.push(p),
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(
            packets.len(),
            5,
            "one access unit per input frame (bframes=0)"
        );
        assert!(
            packets[0].flags.contains(PacketFlags::KEY),
            "first frame is a keyframe"
        );
        assert!(
            enc.temp_path.is_none(),
            "the temp file is cleaned up once the child exits"
        );
    }
}
