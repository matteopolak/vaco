//! The generic conversion routine every `PcmFormat` row drives (plan 15 §4.9:
//! "one 300-line crate and a table", not 38 hand-written decoders).
//!
//! # The one subtlety
//!
//! A container width narrower than the decoded format's own width (`s24le`
//! stores 3 bytes, decodes to `S32`) is widened by **left-shifting into the
//! high-order bits**, not by sign-extending into the low ones — the standard
//! bit-depth up-conversion convention (a 24-bit sample becomes the loudest
//! quarter of the 32-bit range, not a quiet 24-bit value sitting inside a
//! wide zero field). Encoding reverses this with a right shift, which is a
//! lossy truncation exactly the way narrowing a sample's precision always is.

use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::table::{PcmFormat, WireKind};

fn read_uint(bytes: &[u8], big_endian: bool) -> Result<u64> {
    if bytes.len() > 8 {
        return Err(Error::Unsupported("pcm: container wider than 8 bytes"));
    }
    let mut buf = [0u8; 8];
    if big_endian {
        let start = 8usize.saturating_sub(bytes.len());
        if let Some(dst) = buf.get_mut(start..) {
            dst.copy_from_slice(bytes);
        }
        Ok(u64::from_be_bytes(buf))
    } else {
        if let Some(dst) = buf.get_mut(..bytes.len()) {
            dst.copy_from_slice(bytes);
        }
        Ok(u64::from_le_bytes(buf))
    }
}

fn write_uint(value: u64, out: &mut [u8], big_endian: bool) {
    let n = out.len();
    // Big-endian: the low-order `n` bytes of the 8-byte form are the ones we
    // want, in order (`full[8-n..]`). Little-endian: the low-order `n` bytes
    // are simply the *first* `n` bytes of the little-endian form.
    let src = if big_endian {
        let full = value.to_be_bytes();
        full.get(8usize.saturating_sub(n)..).map(<[u8]>::to_vec)
    } else {
        let full = value.to_le_bytes();
        full.get(..n).map(<[u8]>::to_vec)
    };
    if let Some(src) = src
        && let Some(dst) = out.get_mut(..src.len())
    {
        dst.copy_from_slice(&src);
    }
}

/// Decode one interleaved sample of `bytes` (exactly `format.container_bytes`
/// long) into `format.decoded`'s native representation, written into `slot`
/// (exactly `format.decoded.bytes_per_sample()` long — pre-sized and
/// pre-charged by the caller via [`vaco_limits::Budget::alloc`]).
fn decode_sample(format: PcmFormat, bytes: &[u8], slot: &mut [u8]) -> Result<()> {
    // A fixed 8-byte stack buffer (the widest decoded sample is `F64`), never
    // a heap allocation — nothing attacker-controlled sizes this.
    let mut tmp = ScratchBuf::new();
    decode_sample_into(format, bytes, &mut tmp)?;
    if slot.len() != tmp.len() {
        return Err(Error::InvalidData("pcm: decoded sample width mismatch"));
    }
    slot.copy_from_slice(tmp.as_slice());
    Ok(())
}

/// A push/extend-only view over a fixed 8-byte stack buffer, so
/// [`decode_sample_into`]'s match arms can use the same `push`/
/// `extend_from_slice` calls a `Vec` would need, without a heap allocation —
/// this workspace requires attacker-influenced-size allocations to go through
/// [`vaco_limits::Budget::alloc`], and a per-sample scratch buffer here is
/// exactly the kind of allocation that rule exists to rule out.
struct ScratchBuf {
    buf: [u8; 8],
    len: usize,
}

impl ScratchBuf {
    const fn new() -> Self {
        Self { buf: [0; 8], len: 0 }
    }

    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = byte;
            self.len = self.len.saturating_add(1);
        }
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.push(b);
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or(&[])
    }
}

/// The actual conversion, appending to a fixed-size [`ScratchBuf`] — kept
/// separate from [`decode_sample`] so the match arms can use `push`/
/// `extend_from_slice` without threading an offset through every branch.
fn decode_sample_into(format: PcmFormat, bytes: &[u8], out: &mut ScratchBuf) -> Result<()> {
    let container_bits = u32::from(format.container_bytes) * 8;
    match format.wire {
        WireKind::SignedInt { big_endian } | WireKind::UnsignedInt { big_endian } => {
            let raw = read_uint(bytes, big_endian)?;
            let centred: i64 = match format.wire {
                WireKind::SignedInt { .. } => {
                    let sign_bit = 1u64 << (container_bits - 1);
                    if raw & sign_bit != 0 {
                        raw.cast_signed().wrapping_sub(1i64 << container_bits)
                    } else {
                        raw.cast_signed()
                    }
                }
                WireKind::UnsignedInt { .. } => {
                    raw.cast_signed() - (1i64 << (container_bits.saturating_sub(1)))
                }
                _ => unreachable!("matched above"),
            };
            match format.decoded {
                SampleFmt::U8 => {
                    // The only 8-bit decoded target: unsigned offset-binary,
                    // silence at 128 — recentre rather than widen.
                    let u = (centred + 128).clamp(0, 255) as u8;
                    out.push(u);
                }
                SampleFmt::S16 => {
                    let widened = widen(centred, container_bits, 16);
                    out.extend_from_slice(&(widened as i16).to_ne_bytes());
                }
                SampleFmt::S32 => {
                    let widened = widen(centred, container_bits, 32);
                    out.extend_from_slice(&(widened as i32).to_ne_bytes());
                }
                _ => return Err(Error::Unsupported("pcm: unexpected decoded format for int wire")),
            }
        }
        WireKind::Float { big_endian } => {
            let raw = read_uint(bytes, big_endian)?;
            match format.decoded {
                SampleFmt::F32 => {
                    let v = f32::from_bits(raw as u32);
                    out.extend_from_slice(&v.to_ne_bytes());
                }
                SampleFmt::F64 => {
                    let v = f64::from_bits(raw);
                    out.extend_from_slice(&v.to_ne_bytes());
                }
                _ => return Err(Error::Unsupported("pcm: unexpected decoded format for float wire")),
            }
        }
        WireKind::ALaw => {
            let &[b] = bytes else {
                return Err(Error::UnexpectedEof);
            };
            out.extend_from_slice(&alaw_to_linear(b).to_ne_bytes());
        }
        WireKind::MuLaw => {
            let &[b] = bytes else {
                return Err(Error::UnexpectedEof);
            };
            out.extend_from_slice(&mulaw_to_linear(b).to_ne_bytes());
        }
        WireKind::Vidc => {
            let &[b] = bytes else {
                return Err(Error::UnexpectedEof);
            };
            out.extend_from_slice(&vidc_to_linear(b).to_ne_bytes());
        }
    }
    Ok(())
}

/// Encode one native-endian decoded sample (exactly `format.decoded`'s own
/// byte width) into `format.container_bytes` wire bytes, appended to `out`.
#[allow(
    clippy::many_single_char_names,
    reason = "byte-slice destructuring names each of a sample's raw bytes a, b, c, ...; \
              a loop over an index would be less clear for a fixed 1/2/4/8-byte pattern"
)]
fn encode_sample(format: PcmFormat, bytes: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let container_bits = u32::from(format.container_bytes) * 8;
    match format.wire {
        WireKind::SignedInt { big_endian } | WireKind::UnsignedInt { big_endian } => {
            let centred: i64 = match format.decoded {
                SampleFmt::U8 => {
                    let &[u] = bytes else {
                        return Err(Error::UnexpectedEof);
                    };
                    i64::from(u) - 128
                }
                SampleFmt::S16 => {
                    let &[a, b] = bytes else {
                        return Err(Error::UnexpectedEof);
                    };
                    let v = i16::from_ne_bytes([a, b]);
                    narrow(i64::from(v), 16, container_bits)
                }
                SampleFmt::S32 => {
                    let &[a, b, c, d] = bytes else {
                        return Err(Error::UnexpectedEof);
                    };
                    let v = i32::from_ne_bytes([a, b, c, d]);
                    narrow(i64::from(v), 32, container_bits)
                }
                _ => return Err(Error::Unsupported("pcm: unexpected sample format for int wire")),
            };
            let raw = match format.wire {
                WireKind::SignedInt { .. } => (centred as u64) & mask(container_bits),
                WireKind::UnsignedInt { .. } => {
                    (centred + (1i64 << (container_bits.saturating_sub(1)))) as u64
                        & mask(container_bits)
                }
                _ => unreachable!("matched above"),
            };
            let mut buf = [0u8; 8];
            let Some(dst) = buf.get_mut(..format.container_bytes as usize) else {
                return Err(Error::Unsupported("pcm: container too wide"));
            };
            write_uint(raw, dst, big_endian);
            out.extend_from_slice(dst);
        }
        WireKind::Float { big_endian } => match format.decoded {
            SampleFmt::F32 => {
                let &[a, b, c, d] = bytes else {
                    return Err(Error::UnexpectedEof);
                };
                let v = f32::from_ne_bytes([a, b, c, d]);
                let mut buf = [0u8; 4];
                write_uint(u64::from(v.to_bits()), &mut buf, big_endian);
                out.extend_from_slice(&buf);
            }
            SampleFmt::F64 => {
                let &[a, b, c, d, e, f, g, h] = bytes else {
                    return Err(Error::UnexpectedEof);
                };
                let v = f64::from_ne_bytes([a, b, c, d, e, f, g, h]);
                let mut buf = [0u8; 8];
                write_uint(v.to_bits(), &mut buf, big_endian);
                out.extend_from_slice(&buf);
            }
            _ => return Err(Error::Unsupported("pcm: unexpected sample format for float wire")),
        },
        WireKind::ALaw | WireKind::MuLaw | WireKind::Vidc => {
            let &[a, b] = bytes else {
                return Err(Error::UnexpectedEof);
            };
            let v = i16::from_ne_bytes([a, b]);
            out.push(match format.wire {
                WireKind::ALaw => linear_to_alaw(v),
                WireKind::MuLaw => linear_to_mulaw(v),
                _ => return Err(Error::Unsupported("pcm: vidc has no registered encoder")),
            });
        }
    }
    Ok(())
}

const fn mask(bits: u32) -> u64 {
    if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 }
}

/// Left-shift a centred value from `from_bits` of precision to `to_bits`,
/// saturating so a value already at the wider width is a no-op.
fn widen(value: i64, from_bits: u32, to_bits: u32) -> i64 {
    if to_bits <= from_bits {
        value
    } else {
        value.saturating_mul(1i64 << (to_bits - from_bits))
    }
}

/// The inverse of [`widen`]: an arithmetic right shift, which is where the
/// precision loss of a narrowing encode actually happens.
fn narrow(value: i64, from_bits: u32, to_bits: u32) -> i64 {
    if to_bits >= from_bits {
        value
    } else {
        value >> (from_bits - to_bits)
    }
}

// ------------------------------------------------------------------ G.711
//
// The standard piecewise-linear approximation to the A-law/mu-law companding
// curves, worked from the segment structure ITU-T G.711 itself defines (8
// segments per polarity, doubling in step size each segment) rather than
// transcribed from any codebase. No `provenance/sources.toml` entry names
// G.711 today, so no `Vaco-Spec-Ref` is attached (the crate docs explain why).

/// The eight A-law segment boundaries: `seg_aend[i]` is the largest 13-bit
/// magnitude segment `i` can represent. Same shape as [`SEG_UEND`], scaled
/// differently because A-law and mu-law bias their magnitude before search
/// in different ways below.
const SEG_AEND: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
const SEG_UEND: [i32; 8] = [
    0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF,
];

/// The first segment whose boundary `val` does not exceed, or 8 (out of
/// range — clamped by the caller before this is ever reached in practice).
fn search_segment(val: i32, table: &[i32; 8]) -> u32 {
    for (i, bound) in table.iter().enumerate() {
        if val <= *bound {
            return i as u32;
        }
    }
    8
}

fn alaw_to_linear(a_val: u8) -> i16 {
    let a_val = a_val ^ 0x55;
    let sign = a_val & 0x80 != 0;
    let seg = (a_val & 0x70) >> 4;
    let mut t = i32::from(a_val & 0x0F) << 4;
    t = match seg {
        0 => t + 8,
        1 => t + 0x108,
        _ => (t + 0x108) << (seg - 1),
    };
    let t = t.clamp(0, i32::from(i16::MAX));
    (if sign { t } else { -t }) as i16
}

/// The matched inverse of [`alaw_to_linear`] — the standard ITU-T G.711
/// piecewise-linear encode (segment search over [`SEG_AEND`]), not a
/// backwards derivation of the decode formula above. Round-trips every one
/// of the 256 A-law codes to itself: `alaw_full_byte_range_round_trips_to_itself`.
fn linear_to_alaw(pcm: i16) -> u8 {
    let mut v = i32::from(pcm) >> 3;
    let mask: u8 = if v >= 0 {
        0xD5
    } else {
        v = -v - 1;
        0x55
    };
    let seg = search_segment(v, &SEG_AEND);
    let byte = if seg >= 8 {
        0x7F
    } else {
        let low = if seg < 2 { (v >> 1) & 0x0F } else { (v >> seg) & 0x0F };
        ((seg as u8) << 4) | (low as u8)
    };
    byte ^ mask
}

fn mulaw_to_linear(u_val: u8) -> i16 {
    const BIAS: i32 = 0x84;
    let u_val = !u_val;
    let sign = u_val & 0x80 != 0;
    let seg = (u_val & 0x70) >> 4;
    let mut t = (i32::from(u_val & 0x0F) << 3) + BIAS;
    t <<= seg;
    let magnitude = (if sign { BIAS - t } else { t - BIAS }).clamp(
        i32::from(i16::MIN),
        i32::from(i16::MAX),
    );
    magnitude as i16
}

/// The matched inverse of [`mulaw_to_linear`], same shape as [`linear_to_alaw`]
/// but over [`SEG_UEND`] and mu-law's own bias/clip constants.
fn linear_to_mulaw(pcm: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 8159;
    let mut v = i32::from(pcm) >> 2;
    let mask: u8 = if v < 0 {
        v = -v;
        0x7F
    } else {
        0xFF
    };
    let v = v.min(CLIP) + (BIAS >> 2);
    let seg = search_segment(v, &SEG_UEND);
    let byte = if seg >= 8 {
        0x7F
    } else {
        ((seg as u8) << 4) | (((v >> (seg + 1)) & 0x0F) as u8)
    };
    byte ^ mask
}

/// Acorn VIDC logarithmic PCM. See [`WireKind::Vidc`] — best-effort, not
/// checked against real hardware output.
fn vidc_to_linear(v: u8) -> i16 {
    let sign = v & 0x80 != 0;
    let exponent = (v >> 4) & 0x07;
    let mantissa = i32::from(v & 0x0F);
    let magnitude = if exponent == 0 {
        mantissa << 4
    } else {
        ((mantissa << 4) + 0x100) << (exponent - 1)
    };
    let magnitude = magnitude.clamp(0, i32::from(i16::MAX));
    (if sign { -magnitude } else { magnitude }) as i16
}

/// Decode `payload` (raw interleaved container bytes) into `channels` planes'
/// worth of native-endian decoded samples, one flat interleaved buffer.
///
/// Trailing bytes that do not complete a whole frame (all channels' worth of
/// one sample) are dropped, mirroring `vaco-demux-raw::pcm`'s own truncation
/// of a short final packet.
///
/// # Errors
/// [`Error::LimitExceeded`] if the implied sample count would exceed
/// `budget`; whatever `decode_sample` returns for a malformed tail.
pub fn decode_interleaved(
    format: PcmFormat,
    payload: &[u8],
    channels: u32,
    budget: &mut Budget,
) -> Result<(Vec<u8>, u32)> {
    let channels = channels.max(1);
    let bytes_per_frame = usize::from(format.container_bytes) * channels as usize;
    if bytes_per_frame == 0 {
        return Err(Error::InvalidData("pcm: zero-width frame"));
    }
    #[allow(
        clippy::integer_division,
        reason = "a short trailing packet legitimately drops its incomplete final frame, \
                  matching vaco-demux-raw's own PCM packetiser"
    )]
    let frames = payload.len() / bytes_per_frame;
    let out_bytes_per_sample = format.decoded.bytes_per_sample();
    let total_out = frames
        .checked_mul(channels as usize)
        .and_then(|n| n.checked_mul(out_bytes_per_sample))
        .ok_or(Error::LimitExceeded {
            limit: "pcm_decoded_bytes",
            requested: u64::MAX,
            cap: usize::MAX as u64,
        })?;
    let mut out: Vec<u8> = budget.alloc(total_out)?;
    let mut offset = 0usize;
    for frame in payload.chunks_exact(bytes_per_frame) {
        for sample in frame.chunks_exact(format.container_bytes as usize) {
            let end = offset.saturating_add(out_bytes_per_sample);
            let slot = out
                .get_mut(offset..end)
                .ok_or(Error::InvalidData("pcm: output buffer too short"))?;
            decode_sample(format, sample, slot)?;
            offset = end;
        }
    }
    Ok((out, frames as u32))
}

/// The inverse of [`decode_interleaved`]: pack `samples` native-endian
/// decoded frames back into on-wire bytes.
///
/// # Errors
/// [`Error::Unsupported`] if `format.encodable` is false; otherwise whatever
/// `encode_sample` returns.
pub fn encode_interleaved(
    format: PcmFormat,
    samples: &[u8],
    channels: u32,
) -> Result<Vec<u8>> {
    if !format.encodable {
        return Err(Error::Unsupported("pcm: this format has no registered encoder"));
    }
    let channels = channels.max(1);
    let in_bytes_per_sample = format.decoded.bytes_per_sample();
    let bytes_per_frame = in_bytes_per_sample * channels as usize;
    if bytes_per_frame == 0 {
        return Err(Error::InvalidData("pcm: zero-width frame"));
    }
    let mut out = Vec::new();
    for frame in samples.chunks_exact(bytes_per_frame) {
        for sample in frame.chunks_exact(in_bytes_per_sample) {
            encode_sample(format, sample, &mut out)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::expect_used,
    clippy::cast_possible_wrap,
    reason = "test code exercising the codec, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use crate::table::format_for;
    use vaco_codec_core::CodecId;
    use vaco_limits::{Budget, Limits};

    fn fmt(id: CodecId) -> PcmFormat {
        *format_for(id).expect("registered")
    }

    #[test]
    fn s16le_round_trips_exactly() {
        let format = fmt(CodecId::PcmS16le);
        let mut budget = Budget::new(Limits::permissive());
        let samples: [i16; 4] = [0, 1, -1, i16::MIN + 1];
        let mut wire = Vec::new();
        for s in samples {
            wire.extend_from_slice(&s.to_le_bytes());
        }
        let (decoded, frames) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        assert_eq!(frames, 4);
        let re_encoded = encode_interleaved(format, &decoded, 1).unwrap();
        assert_eq!(re_encoded, wire);
    }

    #[test]
    fn u8_round_trips() {
        let format = fmt(CodecId::PcmU8);
        let mut budget = Budget::new(Limits::permissive());
        let wire = vec![0u8, 128, 255, 64];
        let (decoded, frames) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        assert_eq!(frames, 4);
        assert_eq!(decoded, wire); // U8 wire == U8 decoded, identity mapping
        let re = encode_interleaved(format, &decoded, 1).unwrap();
        assert_eq!(re, wire);
    }

    #[test]
    fn s8_decodes_to_offset_binary_u8() {
        let format = fmt(CodecId::PcmS8);
        let mut budget = Budget::new(Limits::permissive());
        let wire = vec![0u8, 127, 0x80]; // 0, +127, -128
        let (decoded, _) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        assert_eq!(decoded, vec![128, 255, 0]);
    }

    #[test]
    fn s24le_widens_into_full_scale_s32() {
        let format = fmt(CodecId::PcmS24le);
        let mut budget = Budget::new(Limits::permissive());
        // Max positive 24-bit value 0x7FFFFF, little-endian.
        let wire = vec![0xFF, 0xFF, 0x7F];
        let (decoded, _) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        let v = i32::from_ne_bytes(decoded.as_slice().try_into().unwrap());
        // Widened by 8 bits: 0x7FFFFF << 8 == 0x7FFFFF00.
        assert_eq!(v, 0x7FFF_FF00u32 as i32);
        let re = encode_interleaved(format, &decoded, 1).unwrap();
        assert_eq!(re, wire);
    }

    #[test]
    fn f32le_round_trips() {
        let format = fmt(CodecId::PcmF32le);
        let mut budget = Budget::new(Limits::permissive());
        let mut wire = Vec::new();
        for v in [0.0f32, 1.0, -0.5, 123.456] {
            wire.extend_from_slice(&v.to_le_bytes());
        }
        let (decoded, _) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        let re = encode_interleaved(format, &decoded, 1).unwrap();
        assert_eq!(re, wire);
    }

    #[test]
    fn alaw_silence_round_trips_near_zero() {
        let format = fmt(CodecId::PcmAlaw);
        let mut budget = Budget::new(Limits::permissive());
        // The A-law encoding of linear zero.
        let wire = vec![0xD5u8];
        let (decoded, _) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        let v = i16::from_ne_bytes(decoded.as_slice().try_into().unwrap());
        assert_eq!(v, 8); // measured against the standard A-law table's own zero code
        let re = encode_interleaved(format, &decoded, 1).unwrap();
        assert_eq!(re, wire);
    }

    #[test]
    fn alaw_full_byte_range_round_trips_to_itself() {
        // Every A-law byte decodes to some i16 and re-encodes back to the
        // *same* byte — the defining property of a companding codec's
        // canonical form (not a claim of bit-exactness against any other
        // implementation).
        let format = fmt(CodecId::PcmAlaw);
        for b in 0..=255u8 {
            let lin = alaw_to_linear(b);
            let back = linear_to_alaw(lin);
            assert_eq!(back, b, "byte {b:#04x} -> {lin} -> {back:#04x}");
        }
        let _ = format;
    }

    #[test]
    fn mulaw_full_byte_range_round_trips_to_itself() {
        // mu-law has two codes for linear zero (0x7F and 0xFF, "negative" and
        // "positive" zero) — a real, well-known property of this exact
        // standard, not a bug: encoding zero always produces the canonical
        // positive-zero code, so decoding the negative-zero code and
        // re-encoding it lands on the *other* zero code. Every other byte is
        // a bijection; that single pair is checked by the weaker but true
        // property below (the decoded *value* is stable) instead.
        for b in 0..=255u8 {
            let lin = mulaw_to_linear(b);
            let back = linear_to_mulaw(lin);
            if b == 0x7F {
                assert_eq!(mulaw_to_linear(back), lin, "the two zero codes both decode to 0");
                continue;
            }
            assert_eq!(back, b, "byte {b:#04x} -> {lin} -> {back:#04x}");
        }
    }

    #[test]
    fn vidc_has_no_encoder() {
        let format = fmt(CodecId::PcmVidc);
        assert!(!format.encodable);
        let mut budget = Budget::new(Limits::permissive());
        let (decoded, _) = decode_interleaved(format, &[0x00], 1, &mut budget).unwrap();
        assert!(encode_interleaved(format, &decoded, 1).is_err());
    }

    #[test]
    fn a_short_trailing_frame_is_dropped_not_erred() {
        let format = fmt(CodecId::PcmS16le);
        let mut budget = Budget::new(Limits::permissive());
        // Two whole frames (4 bytes) plus one dangling byte.
        let wire = vec![1, 0, 2, 0, 9];
        let (_, frames) = decode_interleaved(format, &wire, 1, &mut budget).unwrap();
        assert_eq!(frames, 2);
    }

    #[test]
    fn stereo_interleaving_is_preserved() {
        let format = fmt(CodecId::PcmS16le);
        let mut budget = Budget::new(Limits::permissive());
        let wire: Vec<u8> = vec![1, 0, 2, 0, 3, 0, 4, 0]; // L=1,R=2 ; L=3,R=4
        let (decoded, frames) = decode_interleaved(format, &wire, 2, &mut budget).unwrap();
        assert_eq!(frames, 2);
        assert_eq!(decoded, wire); // s16le -> S16 is byte-identical
    }
}
