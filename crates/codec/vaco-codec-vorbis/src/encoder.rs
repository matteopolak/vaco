//! Native Vorbis encode, fixed low-complexity setup (issues #309/#310/#311:
//! codebook/header construction, a real but simple floor1 curve fit, and
//! residue quantisation — see the module docs on [`crate::enc_setup`],
//! [`crate::enc_codebook`] and [`crate::floor1::encode_values`] for exactly
//! what each of those covers and what they deliberately leave out).
//!
//! **What this does**: one fixed [`enc_setup`] configuration — floor type 1
//! with a flat/ordered codebook, residue type 1 with a single partition and
//! a uniform scalar-VQ codebook, no channel coupling, a single mode with no
//! block-size switching — applied to every stream regardless of content.
//! The floor fit is a real (if simple) local-magnitude envelope sampled at
//! each floor breakpoint and inverted through
//! [`crate::floor1::encode_values`]; the residue is the floor-normalised
//! spectrum, uniformly quantised.
//!
//! **What this does not do** (left for #310's/#311's fuller sophistication,
//! or for #312 entirely): no psychoacoustic masking curve (the floor target
//! is a magnitude envelope, not an auditory-masking threshold), no residue
//! partition classification (every coefficient costs the same number of
//! bits regardless of how quiet it is, which is the main reason this
//! encoder's bitrate is far above a tuned real encoder's at a comparable
//! block size), no channel coupling, no block-size switching (so a sharp
//! transient can pre-echo across the whole 2048-sample block), and no
//! bitrate/quality control — every stream gets the same fixed quantiser.
//! None of these affect whether the bitstream is well-formed or decodes
//! cleanly.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 4.2, 4.3 and 7.2`

use vaco_codec_core::{Accept, Caps, Encoder, Machine};
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

use crate::bitreader::BitWriterLsb;
use crate::enc_codebook::flat_code_bits;
use crate::enc_setup::{
    self, FLOOR_MULTIPLIER, FLOOR_RANGE, FLOOR_X, HALF, RESIDUE_DELTA, RESIDUE_ENTRIES, RESIDUE_MIN,
};
use crate::floor1::{Floor1Config, compute_curve, encode_values};
use crate::floor1_table::FLOOR1_INVERSE_DB_TABLE;
use crate::mdct::{MdctForward, window};

const BLOCK_SIZE: usize = enc_setup::BLOCK_SIZE as usize;
#[allow(
    clippy::integer_division,
    reason = "BLOCK_SIZE is a fixed power of two (2048); the halving is exact"
)]
const HOP: usize = BLOCK_SIZE / 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamState {
    channels: u32,
    sample_rate: u32,
}

/// A [`vaco_codec_core::Encoder`] over [`Frame`]/[`Packet`]: native Vorbis
/// encode. See the module doc for exactly which encoding choices this makes.
#[derive(Debug)]
pub struct VorbisEncoder {
    limits: Limits,
    machine: Machine<Packet>,
    state: Option<StreamState>,
    /// Per-channel sample history: at least `HALF` samples carry over
    /// between windows (the 50% overlap every Vorbis block needs), plus
    /// whatever new input hasn't formed a full window yet.
    buffered: Vec<Vec<f32>>,
    fwd: MdctForward,
    /// The window function, computed once (fixed block size, no switching).
    win: Vec<f32>,
    /// Floor breakpoint positions, `[0, HALF] ++ FLOOR_X`, in the exact
    /// order the setup header's partition list appends them — see
    /// [`crate::enc_setup::FLOOR_X`]'s doc.
    full_x: Vec<u32>,
    flushed_tail: bool,
    /// The first real frame's own `pts`, in the stream's time base (samples
    /// at `state.sample_rate`, the same convention every other audio
    /// encoder's input in this tree uses — `vaco-codec-flac`'s own
    /// `base_pts` is the model this mirrors). `None` until the first frame
    /// arrives.
    base_pts: Option<i64>,
    /// How many `emit_window` calls have produced a packet so far. Each one
    /// after the first advances the decoded output by exactly `HOP` samples
    /// (the 50% overlap every window shares with its predecessor), so
    /// packet `n`'s `pts` is `base_pts + n * HOP` — the same
    /// `base + count * step` shape `vaco-codec-flac`'s `frame_number *
    /// BLOCK_SIZE` uses, with this format's own step.
    windows_emitted: u32,
}

impl VorbisEncoder {
    /// An encoder bounding its output packets by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        let full_x: Vec<u32> = std::iter::once(0)
            .chain(std::iter::once(HALF))
            .chain(FLOOR_X.iter().copied())
            .collect();
        Self {
            limits,
            machine: Machine::new(Caps::DELAY.union(Caps::SUBFRAMES)),
            state: None,
            buffered: Vec::new(),
            fwd: MdctForward::new(),
            win: window(BLOCK_SIZE, BLOCK_SIZE, false, false, false),
            full_x,
            flushed_tail: false,
            base_pts: None,
            windows_emitted: 0,
        }
    }

    /// The Xiph-laced identification/comment/setup header blob — what a
    /// container's `extradata` channel carries (`vaco-mux-ogg`'s writer
    /// expects exactly this shape for Vorbis; see that crate's own doc).
    /// Empty before the first frame, since channel count and sample rate
    /// are not known until then.
    #[must_use]
    pub fn extradata(&self) -> Vec<u8> {
        let Some(state) = self.state else {
            return Vec::new();
        };
        let channels = u8::try_from(state.channels).unwrap_or(u8::MAX);
        enc_setup::build_extradata(channels, state.sample_rate)
    }

    fn ingest(&mut self, frame: &Frame) -> Result<()> {
        let FrameData::Audio {
            format,
            sample_rate,
            samples,
            ref layout,
            ..
        } = frame.data
        else {
            return Err(Error::Unsupported("vorbis: encoder needs an audio frame"));
        };
        if format != SampleFmt::F32P {
            return Err(Error::Unsupported(
                "vorbis: encoder accepts f32p input only",
            ));
        }
        let channels = layout.channels;
        if channels == 0 {
            return Err(Error::Unsupported(
                "vorbis: encoder needs at least one channel",
            ));
        }

        match self.state {
            None => {
                self.state = Some(StreamState {
                    channels,
                    sample_rate,
                });
                self.buffered = (0..channels).map(|_| Vec::new()).collect();
            }
            Some(state) => {
                if state.channels != channels || state.sample_rate != sample_rate {
                    return Err(Error::Unsupported(
                        "vorbis: channel count or sample rate changed mid-stream",
                    ));
                }
            }
        }
        // The very first real frame's own `pts` anchors every packet this
        // encoder ever emits — see `base_pts`'s field docs. `Encoder::
        // prime_audio` (if it ran) only seeded `state`/`buffered`, not a
        // timestamp, since it runs before any frame (and so any `pts`)
        // exists at all.
        if self.base_pts.is_none() {
            self.base_pts = frame.pts.ticks();
        }

        for ch in 0..channels as usize {
            let Some(plane) = frame.plane(ch) else {
                continue;
            };
            let Some(row) = plane.row(0) else { continue };
            let Some(dst) = self.buffered.get_mut(ch) else {
                continue;
            };
            for chunk in row.chunks_exact(4).take(samples as usize) {
                let bytes: [u8; 4] = chunk.try_into().unwrap_or([0, 0, 0, 0]);
                dst.push(f32::from_ne_bytes(bytes));
            }
        }
        Ok(())
    }

    fn drain_full_windows(&mut self, budget: &mut Budget) -> Result<()> {
        loop {
            let available = self.buffered.first().map_or(0, Vec::len);
            if available < BLOCK_SIZE {
                break;
            }
            self.emit_window(budget)?;
            for buf in &mut self.buffered {
                let drop_n = HOP.min(buf.len());
                buf.drain(..drop_n);
            }
        }
        Ok(())
    }

    fn emit_final(&mut self, budget: &mut Budget) -> Result<()> {
        let Some(_state) = self.state else {
            return Ok(());
        };
        let has_tail = self.buffered.first().is_some_and(|b| !b.is_empty());
        if has_tail {
            for buf in &mut self.buffered {
                buf.resize(BLOCK_SIZE, 0.0);
            }
            self.emit_window(budget)?;
            for buf in &mut self.buffered {
                buf.clear();
            }
        }
        // Vorbis's overlap-add always trails by `HOP` samples (Caps::DELAY):
        // the last real window's second half only appears once another
        // window follows it. One all-silence window flushes it.
        if !self.flushed_tail {
            self.flushed_tail = true;
            for buf in &mut self.buffered {
                buf.resize(BLOCK_SIZE, 0.0);
            }
            self.emit_window(budget)?;
            for buf in &mut self.buffered {
                buf.clear();
            }
        }
        Ok(())
    }

    /// Encode exactly one `BLOCK_SIZE`-sample window (already the head of
    /// every channel's buffer) into one audio packet.
    fn emit_window(&mut self, budget: &mut Budget) -> Result<()> {
        let Some(state) = self.state else {
            return Ok(());
        };
        let channels = state.channels as usize;

        let mut coeffs_per_channel: Vec<Vec<f32>> = Vec::new();
        for ch in 0..channels {
            let samples = self.buffered.get(ch).map_or(&[][..], Vec::as_slice);
            let windowed: Vec<f32> = samples
                .iter()
                .zip(&self.win)
                .map(|(&s, &w)| s * w)
                .collect();
            let coeffs = self.fwd.transform(&windowed, BLOCK_SIZE)?;
            coeffs_per_channel.push(coeffs);
        }

        let mut w = BitWriterLsb::new();
        w.put(0, 1); // packet type: audio

        let mut floor_curves: Vec<Vec<f32>> = Vec::new();
        for coeffs in &coeffs_per_channel {
            let desired = fit_floor(coeffs);
            let vals = encode_values(&self.full_x, &desired, i64::from(FLOOR_RANGE));
            write_floor(&mut w, &desired, &vals);

            let mut y_for_curve = desired.clone();
            if let Some(tail) = y_for_curve.get_mut(2..) {
                tail.clone_from_slice(vals.get(2..).unwrap_or(&[]));
            }
            let cfg = Floor1Config::for_curve_fit(self.full_x.clone(), FLOOR_MULTIPLIER);
            floor_curves.push(compute_curve(&cfg, &y_for_curve, HALF as usize));
        }

        // Residue: one classword per channel (single-entry classbook, spec
        // errata 20150226 — any bit value, always consumes exactly one),
        // then every channel's HALF quantised residue symbols in full.
        for _ in 0..channels {
            w.put_tree_bit(0);
        }
        let residue_bits = flat_code_bits(RESIDUE_ENTRIES);
        for (coeffs, curve) in coeffs_per_channel.iter().zip(&floor_curves) {
            for i in 0..HALF as usize {
                let c = coeffs.get(i).copied().unwrap_or(0.0);
                let f = curve.get(i).copied().unwrap_or(1.0).max(1e-6);
                let residual = c / f;
                let idx = quantize_residue(residual);
                write_flat_symbol(&mut w, idx, residue_bits);
            }
        }

        let bytes = w.finish();
        let mut packet = Packet::from_slice(budget, &bytes)?;
        packet.flags = PacketFlags::KEY;
        packet.pts = self
            .base_pts
            .map(|base| {
                base.saturating_add(i64::from(self.windows_emitted) * i64::try_from(HOP).unwrap_or(i64::MAX))
            })
            .map_or(vaco_core::Timestamp::NONE, vaco_core::Timestamp::new);
        // Same bug class as `vaco-codec-flac`/`vaco-codec-alac`'s encoders,
        // measured worse here: this was never set at all, and
        // `vaco-mux-ogg::Writer::write_packet` sums exactly this field into
        // each stream's granule position (`st.granule_cursor =
        // st.granule_cursor.saturating_add(duration_ticks)`), which is
        // Ogg's *only* authoritative duration/seek marker. Leaving it at
        // the `Duration` default did not just undercount the last packet
        // the way MP4's `stts` did -- it froze the granule position at
        // zero for the *entire* file. Measured: `vaco -i mono.wav -c:a
        // vorbis out.ogg` produced a file `ffprobe` reported as
        // `duration=N/A` for, and decoding it through `ffmpeg` logged
        // "timestamp discontinuity" and "non monotonically increasing
        // dts". Every emitted window is exactly `HOP` samples of new
        // decoder output, including the silence-padded tail windows
        // `emit_final` emits (Vorbis trims padding via the container's
        // truncated last granule position, not a short final packet the
        // way FLAC/ALAC's block coders do), so unlike those two this
        // encoder's per-packet duration is uniform -- no separate
        // short-final-block case to get right.
        let time_base = Rational::new(1, i32::try_from(state.sample_rate).unwrap_or(1).max(1));
        packet.duration = Timestamp::new(i64::try_from(HOP).unwrap_or(0))
            .to_duration(time_base)
            .unwrap_or(Duration::ZERO);
        self.windows_emitted = self.windows_emitted.wrapping_add(1);
        self.machine.emit(packet);
        Ok(())
    }
}

/// A local-magnitude envelope, sampled at every floor breakpoint (position
/// `0` and `HALF` use the boundary bins directly; each `FLOOR_X` entry
/// averages a small window around it — see the module doc on why this is a
/// magnitude envelope and not a masking curve).
fn fit_floor(coeffs: &[f32]) -> Vec<u32> {
    let half = coeffs.len();
    let sample_at = |center: usize| -> f32 {
        let lo = center.saturating_sub(3);
        let hi = (center + 4).min(half);
        if hi <= lo {
            return 0.0;
        }
        let mut sum = 0.0f64;
        let mut n = 0u32;
        for v in coeffs.get(lo..hi).unwrap_or(&[]) {
            sum += f64::from(v.abs());
            n += 1;
        }
        if n == 0 {
            0.0
        } else {
            (sum / f64::from(n)) as f32
        }
    };

    let mut out = Vec::new();
    out.push(quantize_floor(sample_at(0)));
    out.push(quantize_floor(sample_at(half.saturating_sub(1))));
    for &x in FLOOR_X {
        let center = (x as usize).min(half.saturating_sub(1));
        out.push(quantize_floor(sample_at(center)));
    }
    out
}

/// Nearest floor1 quantiser index (spec 7.2.1's inverse-dB table, section
/// 10.1): the `y` in `0..FLOOR_RANGE` whose
/// `FLOOR1_INVERSE_DB_TABLE[y * FLOOR_MULTIPLIER]` is closest to `magnitude`.
/// A 64-entry linear scan (`FLOOR_RANGE`), run twice per channel per window —
/// cheap enough not to need the table's monotonicity for a binary search.
fn quantize_floor(magnitude: f32) -> u32 {
    let mut best = 0u32;
    let mut best_err = f32::INFINITY;
    for y in 0..FLOOR_RANGE {
        let table_idx = (y as usize).saturating_mul(FLOOR_MULTIPLIER as usize);
        let v = FLOOR1_INVERSE_DB_TABLE
            .get(table_idx)
            .copied()
            .unwrap_or(0.0);
        let err = (v - magnitude).abs();
        if err < best_err {
            best_err = err;
            best = y;
        }
    }
    best
}

/// Nearest residue quantiser index: `RESIDUE_ENTRIES` uniform levels over
/// `[RESIDUE_MIN, RESIDUE_MIN + (RESIDUE_ENTRIES-1)*RESIDUE_DELTA]`, the same
/// scalar-VQ codebook [`crate::enc_setup::build_extradata`]'s setup header
/// declares (`BOOK_RESIDUE`).
fn quantize_residue(value: f32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    let steps = ((value - RESIDUE_MIN) / RESIDUE_DELTA).round();
    if steps <= 0.0 {
        0
    } else if steps >= f32::from(u16::try_from(RESIDUE_ENTRIES - 1).unwrap_or(u16::MAX)) {
        RESIDUE_ENTRIES - 1
    } else {
        steps as u32
    }
}

/// Write one channel's floor1 packet fields: the nontrivial flag, the two
/// raw endpoints, then every partition's symbol through `BOOK_FLOOR` — the
/// exact field order [`crate::floor1::decode_packet`] reads.
fn write_floor(w: &mut BitWriterLsb, desired: &[u32], vals: &[u32]) {
    w.put_bool(true); // nontrivial floor: always present
    let ilog_range = crate::bitreader::ilog(i64::from(FLOOR_RANGE) - 1);
    w.put(desired.first().copied().unwrap_or(0), ilog_range);
    w.put(desired.get(1).copied().unwrap_or(0), ilog_range);
    let floor_bits = flat_code_bits(FLOOR_RANGE);
    for &v in vals.get(2..).unwrap_or(&[]) {
        write_flat_symbol(w, v, floor_bits);
    }
}

/// Write `symbol` through a flat/ordered codebook's canonical codeword:
/// entry `i`'s codeword is `i` itself, `MSb` (root decision) first — see
/// [`crate::enc_codebook`]'s module doc.
fn write_flat_symbol(w: &mut BitWriterLsb, symbol: u32, bits: u32) {
    for bit_index in (0..bits).rev() {
        w.put_tree_bit((symbol >> bit_index) & 1);
    }
}

impl Encoder for VorbisEncoder {
    /// This encoder's one accepted format — without this override, the
    /// trait's own empty-slice default ("whatever arrives") told a caller
    /// nothing, so `-c:a vorbis` fed straight from a demuxer's native
    /// format (`s16`/`s16p`, never `f32p`) reached `Self::ingest` and hit
    /// its "encoder accepts f32p input only" refusal on the very first
    /// frame — after `add_stream` had already run, too late for a caller to
    /// have inserted a converter first. Measured directly: `vaco -i in.wav
    /// -c:a vorbis out.mkv`/`out.ogg` failed this way. Same fix E2E-GAPS #3
    /// already gave `vaco-codec-flac`/`vaco-codec-alac`/`vaco-codec-pcm`,
    /// simply never applied here.
    fn accepted_sample_fmts(&self) -> &'static [SampleFmt] {
        &[SampleFmt::F32P]
    }

    /// Seeds [`StreamState`] from the pipeline's own already-known shape, the
    /// same reason [`Encoder::prime_audio`]'s own doc gives: the setup
    /// header this encoder's [`Self::extradata`] builds needs channel count
    /// and sample rate, and without this they were not known until the
    /// first [`Self::send_frame`] — one call too late for a container's
    /// `add_stream`, which is what an Ogg or Matroska output needs the
    /// identification/setup header for at all (`vaco-mux-ogg` already
    /// refuses a `CodecId::Vorbis` stream with no extradata; `vaco-mux-
    /// matroska` does the same for `A_VORBIS`, having previously written
    /// nothing and let the file through). `format` is unused because
    /// `enc_setup::build_extradata` needs only channels and sample rate —
    /// this encoder's format is fixed to `F32P` regardless of what the
    /// pipeline negotiated it from.
    fn prime_audio(
        &mut self,
        sample_rate: u32,
        layout: vaco_chlayout::ChannelLayout,
        _format: SampleFmt,
    ) {
        if self.state.is_some() {
            return;
        }
        let channels = layout.channels;
        if channels == 0 {
            return;
        }
        self.state = Some(StreamState {
            channels,
            sample_rate,
        });
        // `Self::ingest`'s own `None` arm is what sizes `buffered` to one
        // inner `Vec` per channel; priming `state` here bypasses that arm
        // entirely (the first real frame now takes the "already known,
        // just checked for a mismatch" branch), so this has to do the same
        // sizing or every sample handed to `ingest` afterwards silently
        // finds no per-channel buffer to land in (`buffered.get_mut(ch)`
        // returning `None`, then a `continue`) and this encoder produces no
        // packets at all instead of one clean error.
        self.buffered = (0..channels).map(|_| Vec::new()).collect();
    }

    /// Delegates to the inherent [`Self::extradata`], answering `None`
    /// instead of an empty blob when nothing is known yet (before
    /// [`Encoder::prime_audio`] or a first frame) — the same shape
    /// `vaco-codec-flac`'s own trait-level override uses.
    fn extradata(&self) -> Option<Vec<u8>> {
        (self.state.is_some()).then(|| self.extradata())
    }

    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        match self.machine.accept(frame.is_none())? {
            Accept::Drain => {
                let mut budget = Budget::new(self.limits.clone());
                self.emit_final(&mut budget)?;
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = frame else { return Ok(()) };
                self.ingest(frame)?;
                let mut budget = Budget::new(self.limits.clone());
                self.drain_full_windows(&mut budget)
            }
        }
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        for buf in &mut self.buffered {
            buf.clear();
        }
        self.flushed_tail = false;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn quantize_floor_is_monotone_in_magnitude() {
        let low = quantize_floor(1e-6);
        let high = quantize_floor(1.0);
        assert!(high >= low);
    }

    #[test]
    fn quantize_residue_clamps_to_valid_range() {
        assert_eq!(quantize_residue(f32::NAN), 0);
        assert_eq!(quantize_residue(-1000.0), 0);
        assert_eq!(quantize_residue(1000.0), RESIDUE_ENTRIES - 1);
    }

    #[test]
    fn write_flat_symbol_matches_binary_encoding() {
        let mut w = BitWriterLsb::new();
        write_flat_symbol(&mut w, 5, 3);
        let bytes = w.finish();
        let mut r = crate::bitreader::BitReaderLsb::new(&bytes);
        let mut decoded = 0u32;
        for _ in 0..3 {
            decoded = (decoded << 1) | r.read_tree_bit();
        }
        assert_eq!(decoded, 5);
    }

    fn mono_f32p_frame(budget: &mut Budget, samples: &[f32], pts: i64) -> Frame {
        let mut frame = Frame::alloc_audio(
            budget,
            SampleFmt::F32P,
            vaco_chlayout::ChannelLayout::MONO,
            samples.len() as u32,
            48_000,
        )
        .unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in samples.iter().enumerate() {
                if let Some(dst) = row.get_mut(i * 4..i * 4 + 4) {
                    dst.copy_from_slice(&s.to_le_bytes());
                }
            }
        }
        frame.pts = vaco_core::Timestamp::new(pts);
        frame
    }

    /// The regression this crate needed for a real transcode: without a
    /// real `pts`, every packet reached the muxer with none, and a strict
    /// container (Matroska among them) refuses that outright ("this
    /// container needs timestamps and the packet has none") — reproduced
    /// end to end via `vaco -c:a vorbis out.mkv`/`out.ogg`. Packet `n`'s
    /// `pts` must be `first_frame.pts + n * HOP` (`HOP` samples of new
    /// decoded output per window after the first), never `Timestamp::NONE`,
    /// once a real `pts` reaches `send_frame` at all.
    #[test]
    fn packets_carry_real_monotonically_spaced_timestamps() {
        let mut budget = Budget::new(Limits::permissive());
        let mut enc = VorbisEncoder::new(Limits::permissive());
        let samples = vec![0.1f32; BLOCK_SIZE * 3];
        let frame = mono_f32p_frame(&mut budget, &samples, 1000);
        enc.send_frame(Some(&frame)).unwrap();

        let mut pts_values = Vec::new();
        while let Ok(p) = enc.receive_packet() {
            pts_values.push(p.pts);
        }
        assert!(
            !pts_values.is_empty(),
            "expected at least one packet from three full windows"
        );
        for &p in &pts_values {
            assert_ne!(p, vaco_core::Timestamp::NONE, "packet pts must not be NONE");
        }
        for w in pts_values.windows(2) {
            assert!(
                w[1].ticks().unwrap_or(0) > w[0].ticks().unwrap_or(0),
                "pts must strictly increase: {w:?}"
            );
        }
        assert_eq!(pts_values[0].ticks(), Some(1000));
        assert_eq!(
            pts_values[1].ticks(),
            Some(1000 + i64::try_from(HOP).unwrap())
        );
    }

    /// The `Packet::duration` twin of the `pts` regression above, and the
    /// more severe one: `vaco-mux-ogg::Writer::write_packet` sums exactly
    /// this field into the stream's granule position, Ogg's only
    /// authoritative duration/seek marker, so leaving it unset did not
    /// merely lose a timestamp on one packet -- it froze every page's
    /// granule position at zero for the whole file (`ffprobe` reported
    /// `duration=N/A`, and `ffmpeg`'s own decode logged a timestamp
    /// discontinuity), reproduced end to end via `vaco -i mono.wav -c:a
    /// vorbis out.ogg`. Every window is exactly `HOP` samples of new
    /// decoder output including the silence-padded tail `emit_final`
    /// emits, so unlike `vaco-codec-flac`/`vaco-codec-alac` there is no
    /// separate short-final-block shape to check here: every packet's
    /// duration must be the same non-zero `HOP`-samples-at-48kHz value.
    #[test]
    fn every_packet_carries_a_real_nonzero_duration() {
        let mut budget = Budget::new(Limits::permissive());
        let mut enc = VorbisEncoder::new(Limits::permissive());
        let samples = vec![0.1f32; BLOCK_SIZE * 3];
        let frame = mono_f32p_frame(&mut budget, &samples, 1000);
        enc.send_frame(Some(&frame)).unwrap();
        enc.send_frame(None).unwrap();

        let expected = Timestamp::new(i64::try_from(HOP).unwrap())
            .to_duration(Rational::new(1, 48_000))
            .unwrap();
        assert_ne!(expected, Duration::ZERO);

        let mut durations = Vec::new();
        while let Ok(p) = enc.receive_packet() {
            durations.push(p.duration);
        }
        assert!(
            !durations.is_empty(),
            "expected at least one packet from three full windows plus the flush tail"
        );
        for d in durations {
            assert_eq!(
                d, expected,
                "every Vorbis packet is exactly HOP samples of new decoder output, tail included"
            );
        }
    }

    /// `Encoder::prime_audio` seeding `state` early must not bypass
    /// `Self::ingest`'s per-channel buffer sizing — the exact regression
    /// found while wiring `prime_audio` in for the extradata fix: without
    /// this, `send_frame` after `prime_audio` silently produced zero
    /// packets instead of the real encoder output, because every sample
    /// handed to `ingest` found no per-channel `Vec` to land in.
    #[test]
    fn prime_audio_then_send_frame_still_produces_packets() {
        let mut budget = Budget::new(Limits::permissive());
        let mut enc = VorbisEncoder::new(Limits::permissive());
        Encoder::prime_audio(
            &mut enc,
            48_000,
            vaco_chlayout::ChannelLayout::MONO,
            SampleFmt::F32P,
        );
        assert!(
            Encoder::extradata(&enc).is_some(),
            "extradata must be ready after prime_audio"
        );
        let samples = vec![0.1f32; BLOCK_SIZE * 2];
        let frame = mono_f32p_frame(&mut budget, &samples, 0);
        enc.send_frame(Some(&frame)).unwrap();
        assert!(
            enc.receive_packet().is_ok(),
            "priming must not stop send_frame from producing real output"
        );
    }
}
