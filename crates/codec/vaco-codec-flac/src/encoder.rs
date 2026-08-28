//! Native FLAC encode.
//!
//! Fixed block size (every frame but a possible short final one is exactly
//! [`BLOCK_SIZE`] samples). Per subframe: `CONSTANT` when every sample in
//! the channel is equal, otherwise the cheapest of `VERBATIM` and a fixed
//! predictor of order 0 through 4 (see [`crate::fixed`]), each measured by
//! its *actual* encoded bit cost rather than an estimate, so the choice is
//! always at least as good as always picking one fixed strategy. Residuals
//! are Rice-coded as a single partition (see [`crate::rice`] for why).
//!
//! **Not implemented, on purpose, for this batch**: LPC (only the fixed
//! predictor family), multi-partition residual coding, wasted-bits
//! detection, and stereo decorrelation (left/side, right/side, mid/side —
//! every channel is coded independently). None of these affect
//! correctness; all of them trade code size for a smaller compressed
//! stream, which is not the bar this batch was asked to clear.
//!
//! Every frame header defers sample rate to `STREAMINFO` (the frame
//! header's own "get this from `STREAMINFO`" code), so this encoder's
//! output is only fully decodable together with [`FlacEncoder::extradata`]
//! — exactly the shape a container's own extradata channel exists for. Bit
//! depth is written explicitly instead: Claxon 0.4.3 (this crate's decode
//! boundary) never implemented the "get this from `STREAMINFO`" case for
//! bit depth — it errors outright — so relying on it here would make this
//! encoder's own output undecodable by this crate's own decoder. Only 16
//! and 24 bits per sample are supported for exactly this reason: they are
//! the two explicit codes this crate's two accepted input formats
//! (`S16P`, `S32P`) map onto.

use vaco_bitstream::BitWriter;
use vaco_codec_core::{Accept, Caps, Encoder, Machine};
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

use crate::crc::{crc8, crc16};
use crate::fixed;
use crate::rice;
use crate::streaminfo::{to_block_bytes, wrap_as_last_metadata_block};

/// Samples per block for every frame but a possible short final one.
pub const BLOCK_SIZE: u32 = 4096;

/// A subframe header is always exactly one zero bit, a 6-bit type code and
/// one wasted-bits flag (always 0 here) — 8 bits, regardless of type.
const SUBFRAME_HEADER_BITS: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamState {
    channels: u32,
    sample_rate: u32,
    bits_per_sample: u32,
}

/// A [`vaco_codec_core::Encoder`] over [`Frame`]/[`Packet`]: native FLAC
/// encode. See the module doc for exactly which encoding choices this
/// makes and which spec features it does not implement.
#[derive(Debug)]
pub struct FlacEncoder {
    limits: Limits,
    machine: Machine<Packet>,
    state: Option<StreamState>,
    /// Per-channel sample buffer, accumulated across `send_frame` calls
    /// until a whole block is ready. Every buffer is kept the same length
    /// as every other (`ingest` always appends the same count to each).
    buffered: Vec<Vec<i32>>,
    frame_number: u32,
    max_block_used: u32,
}

impl FlacEncoder {
    /// An encoder bounding its output packets by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            machine: Machine::new(Caps::DELAY.union(Caps::SUBFRAMES)),
            state: None,
            buffered: Vec::new(),
            frame_number: 0,
            max_block_used: 0,
        }
    }

    /// A synthetic `"fLaC" + STREAMINFO` prefix describing every frame this
    /// encoder has produced so far — what a container's extradata channel
    /// would carry, and what [`crate::decoder::FlacDecoder::set_extradata`]
    /// (or Claxon directly) needs to make sense of them, since every frame
    /// header this encoder writes defers sample rate and bit depth to
    /// `STREAMINFO` rather than restating them.
    ///
    /// Empty before the first frame is sent, since channel count and
    /// sample rate are not known until then.
    #[must_use]
    pub fn extradata(&self) -> Vec<u8> {
        let Some(state) = self.state else {
            return Vec::new();
        };
        let max_block = u16::try_from(self.max_block_used.max(BLOCK_SIZE)).unwrap_or(u16::MAX);
        let payload = to_block_bytes(
            state.sample_rate,
            state.channels,
            state.bits_per_sample,
            max_block,
        );
        let mut out = Vec::new();
        out.extend_from_slice(b"fLaC");
        out.extend_from_slice(&wrap_as_last_metadata_block(&payload));
        out
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
            return Err(Error::Unsupported("flac: encoder needs an audio frame"));
        };
        // 24, not 32, for S32P: see the module doc on why this crate's
        // frame headers need an explicit, Claxon-decodable bit depth
        // rather than FLAC's full 4-to-32-bit range. A 32-bit-range value
        // in an `S32P` frame is therefore an input contract violation, not
        // something this encoder can detect after the fact.
        let bits_per_sample = match format {
            SampleFmt::S16P => 16,
            SampleFmt::S32P => 24,
            _ => {
                return Err(Error::Unsupported(
                    "flac: encoder accepts s16p or s32p input only",
                ));
            }
        };
        let channels = layout.channels;
        if channels == 0 || channels > 8 {
            return Err(Error::Unsupported(
                "flac: encoder supports 1 to 8 independently-coded channels",
            ));
        }

        match self.state {
            None => {
                self.state = Some(StreamState {
                    channels,
                    sample_rate,
                    bits_per_sample,
                });
                self.buffered = (0..channels).map(|_| Vec::new()).collect();
            }
            Some(state) => {
                if state.channels != channels
                    || state.sample_rate != sample_rate
                    || state.bits_per_sample != bits_per_sample
                {
                    return Err(Error::Unsupported(
                        "flac: channel count, sample rate or sample format changed mid-stream",
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
            append_samples(dst, row, format, samples);
        }
        Ok(())
    }

    fn drain_full_blocks(&mut self, budget: &mut Budget) -> Result<()> {
        while let Some(state) = self.state {
            let available = self.buffered.first().map_or(0, Vec::len);
            if available < BLOCK_SIZE as usize {
                break;
            }
            self.emit_block(state, BLOCK_SIZE as usize, budget)?;
        }
        Ok(())
    }

    fn emit_final_partial(&mut self, budget: &mut Budget) -> Result<()> {
        let Some(state) = self.state else {
            return Ok(());
        };
        // At most one iteration in practice (whatever is left after
        // `drain_full_blocks` is by definition under `BLOCK_SIZE`), but
        // looping costs nothing and needs no extra invariant.
        loop {
            let available = self.buffered.first().map_or(0, Vec::len);
            if available == 0 {
                break;
            }
            let block_size = available.min(BLOCK_SIZE as usize);
            self.emit_block(state, block_size, budget)?;
        }
        Ok(())
    }

    fn emit_block(
        &mut self,
        state: StreamState,
        block_size: usize,
        budget: &mut Budget,
    ) -> Result<()> {
        let mut per_channel: Vec<Vec<i32>> = Vec::new();
        for buf in &mut self.buffered {
            let take_n = block_size.min(buf.len());
            let mut taken = Vec::new();
            for v in buf.drain(..take_n) {
                taken.push(v);
            }
            per_channel.push(taken);
        }
        let bytes = encode_frame(&per_channel, state.bits_per_sample, self.frame_number);
        self.frame_number = self.frame_number.wrapping_add(1);
        self.max_block_used = self.max_block_used.max(block_size as u32);
        let mut packet = Packet::from_slice(budget, &bytes)?;
        packet.flags = PacketFlags::KEY;
        self.machine.emit(packet);
        Ok(())
    }
}

/// Copy `row` (raw native-endian sample bytes for one channel) into `dst`
/// as `i32`, `count` samples' worth.
fn append_samples(dst: &mut Vec<i32>, row: &[u8], format: SampleFmt, count: u32) {
    match format {
        SampleFmt::S16P => {
            for chunk in row.chunks_exact(2).take(count as usize) {
                let bytes: [u8; 2] = chunk.try_into().unwrap_or([0, 0]);
                dst.push(i32::from(i16::from_ne_bytes(bytes)));
            }
        }
        SampleFmt::S32P => {
            for chunk in row.chunks_exact(4).take(count as usize) {
                let bytes: [u8; 4] = chunk.try_into().unwrap_or([0, 0, 0, 0]);
                dst.push(i32::from_ne_bytes(bytes));
            }
        }
        _ => {}
    }
}

impl Encoder for FlacEncoder {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        match self.machine.accept(frame.is_none())? {
            Accept::Drain => {
                let mut budget = Budget::new(self.limits.clone());
                self.emit_final_partial(&mut budget)?;
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = frame else { return Ok(()) };
                self.ingest(frame)?;
                let mut budget = Budget::new(self.limits.clone());
                self.drain_full_blocks(&mut budget)
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
        self.frame_number = 0;
    }
}

/// One subframe's chosen encoding — the cheapest of the candidates
/// [`choose_subframe`] tries.
enum SubframePlan {
    Constant(i32),
    Verbatim,
    Fixed { order: usize, residual: Vec<i32> },
}

/// Pick the cheapest valid encoding for one channel's `samples`, all
/// exactly `bps` bits wide.
fn choose_subframe(samples: &[i32], bps: u32) -> SubframePlan {
    let first = samples.first().copied().unwrap_or(0);
    if samples.iter().all(|&s| s == first) {
        return SubframePlan::Constant(first);
    }

    let verbatim_bits = SUBFRAME_HEADER_BITS + u64::from(bps) * samples.len() as u64;
    let mut best_plan = SubframePlan::Verbatim;
    let mut best_bits = verbatim_bits;

    for order in 0..=fixed::MAX_ORDER {
        if samples.len() <= order {
            continue;
        }
        let residual = fixed::residual(samples, order);
        // RFC 9639 §9.2.7.3: a residual sample must not be i32::MIN. A
        // predictor that produces one is disqualified outright rather than
        // patched, leaving the other candidates (worst case, VERBATIM,
        // which has no such restriction) to win instead.
        if residual.contains(&i32::MIN) {
            continue;
        }
        let bits = SUBFRAME_HEADER_BITS
            + u64::from(bps) * order as u64
            + rice::encoded_len_bits(&residual);
        if bits < best_bits {
            best_bits = bits;
            best_plan = SubframePlan::Fixed { order, residual };
        }
    }

    best_plan
}

fn write_subframe(bw: &mut BitWriter, plan: &SubframePlan, samples: &[i32], bps: u32) {
    let type_code = match plan {
        SubframePlan::Constant(_) => 0b00_0000u32,
        SubframePlan::Verbatim => 0b00_0001u32,
        SubframePlan::Fixed { order, .. } => 0b00_1000u32 | *order as u32,
    };
    bw.put(1, 0); // Subframe header padding bit: MUST be 0.
    bw.put(6, type_code);
    bw.put(1, 0); // No wasted bits per sample.
    match plan {
        SubframePlan::Constant(value) => bw.put(bps, *value as u32),
        SubframePlan::Verbatim => {
            for &s in samples {
                bw.put(bps, s as u32);
            }
        }
        SubframePlan::Fixed { order, residual } => {
            for &s in samples.iter().take(*order) {
                bw.put(bps, s as u32);
            }
            rice::write(bw, residual);
        }
    }
}

/// Build one complete FLAC frame (header, subframes, footer) from
/// `per_channel` (one `Vec<i32>` per channel, all the same length — the
/// block size).
fn encode_frame(per_channel: &[Vec<i32>], bits_per_sample: u32, frame_number: u32) -> Vec<u8> {
    let block_size = per_channel.first().map_or(0, Vec::len);
    let channels = per_channel.len() as u32;

    let mut hdr = BitWriter::new();
    hdr.put(14, 0b11_1111_1111_1110); // Frame sync code.
    hdr.put(1, 0); // Reserved.
    hdr.put(1, 0); // Fixed block size stream.
    hdr.put(4, 0b0111); // Block size: uncommon, 16-bit escape follows.
    hdr.put(4, 0b0000); // Sample rate: from STREAMINFO.
    hdr.put(4, channels.saturating_sub(1)); // Independent channels, no decorrelation.
    // Bit depth: written explicitly (see the module doc on why "from
    // STREAMINFO" is not an option here). `ingest` only ever produces 16
    // or 24, so the fallback arm is unreachable in practice.
    hdr.put(3, if bits_per_sample == 24 { 0b110 } else { 0b100 });
    hdr.put(1, 0); // Reserved.
    write_coded_number(&mut hdr, frame_number);
    let block_size_minus_one = u16::try_from(block_size.saturating_sub(1)).unwrap_or(u16::MAX);
    hdr.put(16, u32::from(block_size_minus_one));

    let header_bytes = hdr.bytes().to_vec();
    let crc = crc8(&header_bytes);
    hdr.put(8, u32::from(crc));

    for channel_samples in per_channel {
        let plan = choose_subframe(channel_samples, bits_per_sample);
        write_subframe(&mut hdr, &plan, channel_samples, bits_per_sample);
    }
    hdr.align_zero();

    let frame_bytes = hdr.bytes().to_vec();
    let footer_crc = crc16(&frame_bytes);
    hdr.put(16, u32::from(footer_crc));

    hdr.finish()
}

/// FLAC's "UTF-8-like" variable-length coded number (RFC 9639 §9.1.5),
/// restricted here to the range this encoder actually produces: a fixed
/// block size stream's frame number, which the spec caps at 31 bits (this
/// crate's `frame_number: u32` field never gets close).
fn write_coded_number(bw: &mut BitWriter, value: u32) {
    if value < 0x80 {
        bw.put(8, value);
    } else if value < 0x0800 {
        bw.put(8, 0xC0 | (value >> 6));
        bw.put(8, 0x80 | (value & 0x3F));
    } else if value < 0x1_0000 {
        bw.put(8, 0xE0 | (value >> 12));
        bw.put(8, 0x80 | ((value >> 6) & 0x3F));
        bw.put(8, 0x80 | (value & 0x3F));
    } else if value < 0x20_0000 {
        bw.put(8, 0xF0 | (value >> 18));
        bw.put(8, 0x80 | ((value >> 12) & 0x3F));
        bw.put(8, 0x80 | ((value >> 6) & 0x3F));
        bw.put(8, 0x80 | (value & 0x3F));
    } else {
        bw.put(8, 0xF8 | (value >> 24));
        bw.put(8, 0x80 | ((value >> 18) & 0x3F));
        bw.put(8, 0x80 | ((value >> 12) & 0x3F));
        bw.put(8, 0x80 | ((value >> 6) & 0x3F));
        bw.put(8, 0x80 | (value & 0x3F));
    }
}

#[cfg(test)]
mod tests {
    use super::write_coded_number;
    use vaco_bitstream::BitWriter;

    #[test]
    fn coded_number_matches_the_spec_range_boundary() {
        // RFC 9639 §9.1.5, Table 18: the 4-byte form's upper bound.
        let mut bw = BitWriter::new();
        write_coded_number(&mut bw, 0x1F_FFFF);
        assert_eq!(bw.finish(), vec![0xF7, 0xBF, 0xBF, 0xBF]);
    }

    #[test]
    fn coded_number_single_byte_boundary() {
        let mut bw = BitWriter::new();
        write_coded_number(&mut bw, 0x7F);
        assert_eq!(bw.finish(), vec![0x7F]);
    }

    #[test]
    fn coded_number_two_byte_boundary() {
        let mut bw = BitWriter::new();
        write_coded_number(&mut bw, 0x80);
        assert_eq!(bw.finish(), vec![0xC2, 0x80]);
    }
}
