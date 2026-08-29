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
use vaco_core::{Error, Result};
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
}
