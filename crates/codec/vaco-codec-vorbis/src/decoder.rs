//! `Decoder` implementation: header setup via `set_extradata`, then
//! per-packet mode/window/floor/residue/coupling/MDCT/overlap-add (spec
//! section 4.3).
//!
//! `Vaco-Spec-Ref: vorbis-i sections 4.1, 4.2 and 4.3`

use std::collections::VecDeque;

use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

use crate::bitreader::{BitReaderLsb, ilog};
use crate::floor0::{self, Floor0Decoded};
use crate::floor1::{self, Floor1Decoded};
use crate::ident::{self, Ident};
use crate::mdct::{self, Imdct};
use crate::setup::{self, FloorConfig, Setup};

const IDENT_MAGIC: &[u8] = b"\x01vorbis";
const SETUP_MAGIC: &[u8] = b"\x05vorbis";

/// Unpack the Xiph-laced `extradata` blob a container hands a Vorbis stream:
/// a packet count minus one, each packet's length lace-encoded except the
/// last (whose length is simply what remains), then every packet's raw
/// bytes concatenated. Both Ogg's `pack_xiph_headers` and Matroska's
/// `A_VORBIS` `CodecPrivate` use exactly this shape (D14.1 keeps this crate
/// from depending on either container crate to reuse their copy).
fn split_xiph_headers(data: &[u8]) -> Option<Vec<&[u8]>> {
    let (&count_minus_one, mut cursor) = data.split_first()?;
    let count = usize::from(count_minus_one).saturating_add(1);
    let mut lens = Vec::new();
    for _ in 0..count.saturating_sub(1) {
        let mut len = 0usize;
        loop {
            let (&b, rest) = cursor.split_first()?;
            cursor = rest;
            len = len.saturating_add(usize::from(b));
            if b != 255 {
                break;
            }
        }
        lens.push(len);
    }
    let mut headers = Vec::new();
    for len in lens {
        if cursor.len() < len {
            return None;
        }
        let (head, rest) = cursor.split_at(len);
        headers.push(head);
        cursor = rest;
    }
    headers.push(cursor);
    Some(headers)
}

#[derive(Debug, Clone, Default)]
struct ChannelOverlap {
    tail: Vec<f32>,
    prev_n: Option<usize>,
}

#[derive(Debug)]
pub struct VorbisDecoder {
    limits: Limits,
    ident: Option<Ident>,
    setup: Option<Setup>,
    pending: VecDeque<Frame>,
    imdct: Imdct,
    overlap: Vec<ChannelOverlap>,
}

impl VorbisDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ident: None,
            setup: None,
            pending: VecDeque::new(),
            imdct: Imdct::new(),
            overlap: Vec::new(),
        }
    }

    fn decode_audio_packet(&mut self, payload: &[u8]) -> Result<()> {
        let Some(ident) = self.ident else {
            return Err(Error::InvalidData("vorbis: audio packet before headers"));
        };
        let Some(setup) = self.setup.as_ref() else {
            return Err(Error::InvalidData("vorbis: audio packet before headers"));
        };
        let mut budget = Budget::new(self.limits.clone());
        let mut r = BitReaderLsb::new(payload);

        if r.get(1) != 0 {
            // Not an audio packet; per spec, ignore it.
            return Ok(());
        }
        if setup.modes.is_empty() {
            return Err(Error::InvalidData("vorbis: stream has no modes"));
        }
        let mode_count = u32::try_from(setup.modes.len()).unwrap_or(u32::MAX);
        let mode_number = r.get(ilog(i64::from(mode_count).saturating_sub(1)));
        let Some(mode) = setup.modes.get(mode_number as usize).copied() else {
            return Ok(());
        };
        let n = if mode.blockflag {
            ident.blocksize_1
        } else {
            ident.blocksize_0
        } as usize;
        let (prev_flag, next_flag) = if mode.blockflag {
            (r.get_bool(), r.get_bool())
        } else {
            (false, false)
        };
        if r.overran() {
            // Spec 4.3.1: EOP up to this point discards the whole packet.
            return Ok(());
        }
        let Some(mapping) = setup.mappings.get(mode.mapping as usize) else {
            return Ok(());
        };
        let channels = ident.channels as usize;
        let half = half_len(n);

        let mut no_residue = vec![false; channels];
        let mut floor_curve: Vec<Vec<f32>> = vec![vec![0f32; half]; channels];
        let mut eop_during_floor = false;
        let mut floor_decode_failed = false;
        for (i, (no_res_slot, curve_slot)) in no_residue
            .iter_mut()
            .zip(floor_curve.iter_mut())
            .enumerate()
        {
            let submap = mapping.mux.get(i).copied().unwrap_or(0) as usize;
            let floor_idx = mapping.submap_floor.get(submap).copied().unwrap_or(0) as usize;
            let Some(floor_cfg) = setup.floors.get(floor_idx) else {
                floor_decode_failed = true;
                break;
            };
            match floor_cfg {
                FloorConfig::Type0(cfg) => {
                    match floor0::decode_packet(cfg, &mut r, &setup.codebooks, &mut budget)? {
                        Floor0Decoded::Unused => *no_res_slot = true,
                        Floor0Decoded::Used {
                            amplitude,
                            coefficients,
                        } => {
                            *curve_slot =
                                floor0::compute_curve(cfg, amplitude, &coefficients, half);
                        }
                    }
                }
                FloorConfig::Type1(cfg) => {
                    match floor1::decode_packet(cfg, &mut r, &setup.codebooks) {
                        Floor1Decoded::Unused => *no_res_slot = true,
                        Floor1Decoded::Used { y } => {
                            *curve_slot = floor1::compute_curve(cfg, &y, half);
                        }
                    }
                }
            }
            if r.overran() {
                eop_during_floor = true;
                break;
            }
        }
        if floor_decode_failed {
            return Ok(());
        }

        let spectrum = if eop_during_floor {
            vec![vec![0f32; half]; channels]
        } else {
            for &(mag, ang) in &mapping.coupling {
                let (mag, ang) = (mag as usize, ang as usize);
                let mag_clear = !no_residue.get(mag).copied().unwrap_or(true);
                let ang_clear = !no_residue.get(ang).copied().unwrap_or(true);
                if mag_clear || ang_clear {
                    if let Some(v) = no_residue.get_mut(mag) {
                        *v = false;
                    }
                    if let Some(v) = no_residue.get_mut(ang) {
                        *v = false;
                    }
                }
            }

            let submap_count = mapping.submap_floor.len();
            let mut residue_vectors: Vec<Vec<f32>> =
                (0..channels).map(|_| vec![0f32; half]).collect();
            for i in 0..submap_count {
                let member_channels: Vec<usize> = (0..channels)
                    .filter(|&j| mapping.mux.get(j).copied().unwrap_or(0) as usize == i)
                    .collect();
                if member_channels.is_empty() {
                    continue;
                }
                let do_not_decode: Vec<bool> = member_channels
                    .iter()
                    .map(|&j| no_residue.get(j).copied().unwrap_or(true))
                    .collect();
                let residue_idx = mapping.submap_residue.get(i).copied().unwrap_or(0) as usize;
                let Some(residue_cfg) = setup.residues.get(residue_idx) else {
                    return Ok(());
                };
                let decoded = crate::residue::decode(
                    residue_cfg,
                    &mut r,
                    &setup.codebooks,
                    member_channels.len(),
                    half,
                    &do_not_decode,
                    &mut budget,
                )?;
                for (k, &j) in member_channels.iter().enumerate() {
                    if let (Some(v), Some(dst)) = (decoded.get(k), residue_vectors.get_mut(j)) {
                        dst.clone_from(v);
                    }
                }
            }

            for &(mag, ang) in mapping.coupling.iter().rev() {
                apply_inverse_coupling(&mut residue_vectors, mag as usize, ang as usize);
            }

            (0..channels)
                .map(|i| {
                    let curve = floor_curve.get(i).map_or(&[][..], Vec::as_slice);
                    let residue = residue_vectors.get(i).map_or(&[][..], Vec::as_slice);
                    curve.iter().zip(residue).map(|(&c, &r)| c * r).collect()
                })
                .collect()
        };

        self.overlap_add(
            &spectrum,
            n,
            mode.blockflag,
            prev_flag,
            next_flag,
            ident,
            &mut budget,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the spec's own window/overlap parameters"
    )]
    fn overlap_add(
        &mut self,
        spectrum: &[Vec<f32>],
        n: usize,
        long_block: bool,
        prev_flag: bool,
        next_flag: bool,
        ident: Ident,
        budget: &mut Budget,
    ) -> Result<()> {
        let channels = ident.channels as usize;
        let win = mdct::window(
            n,
            ident.blocksize_0 as usize,
            long_block,
            prev_flag,
            next_flag,
        );
        let mut windowed: Vec<Vec<f32>> = Vec::new();
        for ch in 0..channels {
            let coeffs = spectrum.get(ch).map_or(&[][..], Vec::as_slice);
            let mut pcm = self.imdct.transform(coeffs, n)?;
            for (s, &w) in pcm.iter_mut().zip(win.iter()) {
                *s *= w;
            }
            windowed.push(pcm);
        }
        if self.overlap.len() != channels {
            self.overlap = vec![ChannelOverlap::default(); channels];
        }

        let prev_n = self.overlap.first().and_then(|o| o.prev_n);
        if let Some(prev_n) = prev_n {
            let (output_len, offset) = overlap_geometry(prev_n, n);
            if output_len > 0 {
                let layout = ident::output_channel_layout(ident.channels);
                let out_samples = u32::try_from(output_len).unwrap_or(u32::MAX);
                let mut frame = Frame::alloc_audio(
                    budget,
                    SampleFmt::F32P,
                    layout,
                    out_samples,
                    ident.sample_rate,
                )?;
                for ch in 0..channels {
                    let tail = self.overlap.get(ch).map_or(&[][..], |o| o.tail.as_slice());
                    let pcm = windowed.get(ch).map_or(&[][..], Vec::as_slice);
                    if let Some(mut plane) = frame.plane_mut(ch)
                        && let Some(row) = plane.row_mut(0)
                    {
                        for pos in 0..output_len {
                            let j = i64::try_from(pos)
                                .unwrap_or(i64::MAX)
                                .saturating_add(offset);
                            let a = tail.get(pos).copied().unwrap_or(0.0);
                            let b = usize::try_from(j)
                                .ok()
                                .and_then(|j| pcm.get(j))
                                .copied()
                                .unwrap_or(0.0);
                            let sample = a + b;
                            let byte_pos = pos.saturating_mul(4);
                            if let Some(bytes) = row.get_mut(byte_pos..byte_pos.saturating_add(4)) {
                                bytes.copy_from_slice(&sample.to_le_bytes());
                            }
                        }
                    }
                }
                self.pending.push_back(frame);
            }
        }
        let half = half_len(n);
        for (ov, pcm) in self.overlap.iter_mut().zip(windowed.iter()) {
            ov.tail = pcm.get(half..).unwrap_or(&[]).to_vec();
            ov.prev_n = Some(n);
        }
        Ok(())
    }
}

/// The overlap-add output length and the index offset between the current
/// window's samples and the running output position (spec section 4.3.8):
/// `output_len = prev_n/4 + n/4`, and a current-window sample at index `j`
/// lands at output position `j - (n/4 - prev_n/4)`.
#[allow(
    clippy::integer_division,
    clippy::cast_possible_wrap,
    reason = "spec 4.3.8's overlap length is exact floor division on block sizes that are always powers of two and capped at 8192, far under i64::MAX"
)]
const fn overlap_geometry(prev_n: usize, n: usize) -> (usize, i64) {
    let output_len = prev_n / 4 + n / 4;
    let offset = (n / 4) as i64 - (prev_n / 4) as i64;
    (output_len, offset)
}

#[allow(clippy::integer_division, reason = "n is always even (a power of two)")]
const fn half_len(n: usize) -> usize {
    n / 2
}

/// Inverse coupling (spec section 4.3.5). `mag` and `ang` are distinct
/// channel indices (verified at setup time), so a split-at-mut gives safe,
/// panic-free mutable access to both.
fn apply_inverse_coupling(vectors: &mut [Vec<f32>], mag: usize, ang: usize) {
    if mag == ang {
        return;
    }
    let (lo, hi, mag_is_lo) = if mag < ang {
        (mag, ang, true)
    } else {
        (ang, mag, false)
    };
    let Some((low_slice, high_slice)) = vectors.split_at_mut_checked(hi) else {
        return;
    };
    let Some(low) = low_slice.get_mut(lo) else {
        return;
    };
    let Some(high) = high_slice.first_mut() else {
        return;
    };
    let (magnitude, angle) = if mag_is_lo { (low, high) } else { (high, low) };
    for (m, a) in magnitude.iter_mut().zip(angle.iter_mut()) {
        let (new_m, new_a) = if *m > 0.0 {
            if *a > 0.0 {
                (*m, *m - *a)
            } else {
                (*m + *a, *m)
            }
        } else if *a > 0.0 {
            (*m, *m + *a)
        } else {
            (*m - *a, *m)
        };
        *m = new_m;
        *a = new_a;
    }
}

impl Decoder for VorbisDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            return Ok(());
        };
        self.decode_audio_packet(packet.payload())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.overlap.clear();
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let headers = split_xiph_headers(extradata).ok_or(Error::InvalidData(
            "vorbis: extradata is not Xiph-laced Vorbis headers",
        ))?;
        let ident_packet = headers.first().copied().ok_or(Error::InvalidData(
            "vorbis: extradata missing identification header",
        ))?;
        let setup_packet = headers
            .get(2)
            .copied()
            .ok_or(Error::InvalidData("vorbis: extradata missing setup header"))?;

        let ident_body = ident_packet
            .strip_prefix(IDENT_MAGIC)
            .ok_or(Error::InvalidData(
                "vorbis: identification header magic mismatch",
            ))?;
        let ident = Ident::parse(ident_body)?;

        let setup_body = setup_packet
            .strip_prefix(SETUP_MAGIC)
            .ok_or(Error::InvalidData("vorbis: setup header magic mismatch"))?;
        let mut budget = Budget::new(self.limits.clone());
        let setup = setup::parse(setup_body, &mut budget, ident.channels)?;

        self.ident = Some(ident);
        self.setup = Some(setup);
        self.overlap.clear();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn split_xiph_headers_matches_the_measured_shape() {
        let headers = vec![vec![1u8, 2, 3], vec![4u8, 5], vec![6u8; 300]];
        let mut packed = vec![2u8]; // count - 1
        packed.push(3); // len of header 0
        packed.push(2); // len of header 1
        for h in &headers {
            packed.extend_from_slice(h);
        }
        let split = split_xiph_headers(&packed).unwrap();
        assert_eq!(split.len(), 3);
        assert_eq!(split[0], &headers[0][..]);
        assert_eq!(split[1], &headers[1][..]);
        assert_eq!(split[2], &headers[2][..]);
    }

    #[test]
    fn garbage_extradata_is_a_decode_error_not_a_panic() {
        let mut dec = VorbisDecoder::new(Limits::permissive());
        assert!(dec.set_extradata(&[0xFF; 4]).is_err());
        assert!(dec.set_extradata(&[]).is_err());
    }

    #[test]
    fn truncated_setup_header_is_a_decode_error() {
        let mut dec = VorbisDecoder::new(Limits::permissive());
        // A packed blob with plausible framing but a setup header that is
        // just the magic and nothing else must not panic.
        let mut ident = vec![1u8, b'v', b'o', b'r', b'b', b'i', b's'];
        ident.extend_from_slice(&0u32.to_le_bytes()); // version
        ident.push(2); // channels
        ident.extend_from_slice(&44100u32.to_le_bytes());
        ident.extend_from_slice(&0i32.to_le_bytes());
        ident.extend_from_slice(&0i32.to_le_bytes());
        ident.extend_from_slice(&0i32.to_le_bytes());
        ident.push(0b1000_1001); // blocksize nibbles (8,8) + framing bit; exact bits not critical for this error path
        let comment = vec![
            3u8, b'v', b'o', b'r', b'b', b'i', b's', 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let setup = vec![5u8, b'v', b'o', b'r', b'b', b'i', b's'];
        let mut packed = vec![2u8];
        for h in [&ident, &comment] {
            let mut len = h.len();
            while len >= 255 {
                packed.push(255);
                len -= 255;
            }
            packed.push(len as u8);
        }
        packed.extend_from_slice(&ident);
        packed.extend_from_slice(&comment);
        packed.extend_from_slice(&setup);

        assert!(dec.set_extradata(&packed).is_err());
    }
}
