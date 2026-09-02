//! ALAC's real adaptive Golomb-Rice code (`dyn_comp`/`dyn_decomp` in Apple's
//! reference).
//!
//! # Provenance — this file supersedes an earlier, self-invented design
//!
//! An earlier version of this crate implemented a Rice coder of its own
//! design (grow/shrink `k` by doubling), stated as such and citing no
//! source, because issue #285's brief read as a strict clean-room
//! prohibition on any ALAC reference. That was too strict: Apple's ALAC
//! reference implementation (<https://github.com/macosforge/alac>) is
//! licensed Apache License 2.0 — confirmed directly,
//! `curl -sL https://raw.githubusercontent.com/macosforge/alac/master/LICENSE`
//! — which is not `FFmpeg`, not GPL/LGPL, and outside the scope of this
//! project's D7/D15 clean-room rule (that rule is specifically about
//! FFmpeg/libav-family GPL code). Reading it is legitimate, and the
//! self-invented coder above was not bitstream-compatible with any real
//! ALAC encoder or decoder — self-interoperable only, which is exactly the
//! defect this rewrite fixes.
//!
//! This module is a line-for-line port of `codec/ag_dec.c` and
//! `codec/ag_enc.c`'s `dyn_decomp`/`dyn_comp` (the outer adaptive-mean loop)
//! and `dyn_get_32bit`/`dyn_code_32bit` plus the 16-bit run-length variant
//! `dyn_get`/`dyn_code` (both files, same repository, same licence). Ported
//! deliberately close to the original control flow rather than "cleaned up"
//! — the encoder's bit-saving trick (one fewer bit is written whenever a
//! residual's remainder happens to be exactly zero — see
//! [`dyn_get_32bit`]'s doc) is easy to get off-by-one wrong, and the
//! reference's own shape is the safest guide to get it right. Genuinely
//! this crate's own translation, not copied verbatim: variable names,
//! control structure (peek/skip instead of C's raw bit-position arithmetic
//! and 32-bit word loads) and error handling (bounded loops instead of
//! `Assert`) are this crate's, per `vaco_bitstream::BitReader`'s own API.
//!
//! `Vaco-Spec-Ref: alac-agc-source codec/ag_dec.c dyn_decomp/dyn_get_32bit/
//! dyn_get, codec/ag_enc.c dyn_comp/dyn_code_32bit/dyn_code, codec/aglib.h
//! constants (QBSHIFT/MB0/PB0/KB0/MMULSHIFT/MDENSHIFT/MOFF/BITOFF/
//! MAX_PREFIX_16/MAX_PREFIX_32/MAX_DATATYPE_BITS_16), Apple Inc., Apache
//! License 2.0`

use vaco_bitstream::{BitReader, BitWriter};

/// `QBSHIFT`/`QB`: the fixed-point shift the running mean `mb` is tracked at.
pub(crate) const QBSHIFT: u32 = 9;
const QB: u32 = 1 << QBSHIFT;
/// Defaults `set_standard_ag_params` uses; this crate always gets `pb`/`mb`/
/// `kb` from the stream's `ALACSpecificConfig` instead (see `cookie.rs`), so
/// these exist only as the fallback a config that omits them would imply.
pub(crate) const PB0: u32 = 40;
pub(crate) const MB0: u32 = 10;
pub(crate) const KB0: u32 = 14;
#[expect(
    dead_code,
    reason = "kept for parity with aglib.h's AgParams.maxrun -- unused there too: neither dyn_decomp nor \
              dyn_comp ever reads params->maxrun, confirmed directly in both functions' bodies"
)]
pub(crate) const MAX_RUN_DEFAULT: u32 = 255;

const MMULSHIFT: u32 = 2;
const MDENSHIFT: u32 = QBSHIFT - MMULSHIFT - 1;
const MOFF: u32 = 1 << (MDENSHIFT - 2);
const BITOFF: u32 = 24;

const MAX_PREFIX_16: u32 = 9;
const MAX_PREFIX_32: u32 = 9;
const MAX_DATATYPE_BITS_16: u32 = 16;

const N_MAX_MEAN_CLAMP: u32 = 0xffff;
const N_MEAN_CLAMP_VAL: u32 = 0xffff;

/// `lead()`: count of leading zero bits, MSB first. The reference implements
/// this as an explicit 32-iteration bit scan (a comment above it apologises
/// for the same thing: "implementing this with some kind of 'count leading
/// zeros' assembly is a big performance win") — `u32::leading_zeros` is
/// exactly that instruction, so this crate uses it directly rather than
/// porting the scan loop.
const fn lead(x: u32) -> u32 {
    x.leading_zeros()
}

/// `lg3a()`: `floor(log2(x + 3))`, computed via `lead` the same way the
/// reference does.
const fn lg3a(x: u32) -> u32 {
    31 - lead(x.wrapping_add(3))
}

/// Adaptive Golomb-Rice parameters for one channel, read from
/// `ALACSpecificConfig` (`cookie.rs`'s `pb`/`mb`/`kb`) or, for a stereo
/// element's second channel, `(pb * pb_factor) / 4` per
/// `ALACDecoder::Decode`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AgParams {
    pub mb0: u32,
    pub pb: u32,
    pub kb: u32,
    pub wb: u32,
}

impl AgParams {
    pub(crate) const fn new(mb: u32, pb: u32, kb: u32) -> Self {
        Self {
            mb0: mb,
            pb,
            kb,
            wb: (1u32 << kb).wrapping_sub(1),
        }
    }
}

/// `dyn_get_32bit`: read one adaptive-Rice-coded value, `maxbits`-wide
/// escape.
///
/// The unary prefix here counts **1-bits**, terminated by a **0-bit** — the
/// opposite convention from e.g. FLAC's zero-run-terminated-by-one. A prefix
/// that reaches [`MAX_PREFIX_32`] ones without a terminating zero is the
/// escape: the raw `maxbits`-bit value follows immediately, no separate
/// terminator.
///
/// The bit-saving trick: when the true remainder is exactly zero, the
/// reference's encoder ([`dyn_code_32bit`] below) writes one fewer bit than
/// the general case, by making the low bit of what looks like a `k`-bit
/// value field belong to the *next* codeword instead. This function
/// reproduces that by peeking `k` bits, and un-consuming one of them (via
/// `BitReader::skip(k - 1)` instead of `k`) whenever the peeked value is `0`
/// or `1` — those are exactly the two peeked values a zero remainder can
/// produce, since the last bit is borrowed from whatever the next codeword
/// starts with.
fn dyn_get_32bit(r: &mut BitReader<'_>, m: u32, k: u32, maxbits: u32) -> u32 {
    let mut pre = 0u32;
    while pre < MAX_PREFIX_32 {
        if r.get_bit() == 0 {
            break;
        }
        pre += 1;
    }
    if pre >= MAX_PREFIX_32 {
        return r.get_long(maxbits.min(32)) as u32;
    }
    if k == 1 {
        // The reference skips the remainder field entirely when k == 1
        // (m == 1, so the remainder is always zero and carries no
        // information).
        return pre * m;
    }
    let v = r.peek(k);
    if v < 2 {
        r.skip(k.saturating_sub(1));
        pre * m
    } else {
        r.skip(k);
        pre * m + (v - 1)
    }
}

/// `dyn_get`: the 16-bit-escape variant used only for the zero-run length
/// inside [`dyn_decomp`]'s zero-run branch. Same shape as
/// [`dyn_get_32bit`], without the `k == 1` shortcut (the reference does not
/// special-case it here either).
fn dyn_get_16bit(r: &mut BitReader<'_>, m: u32, k: u32) -> u32 {
    let mut pre = 0u32;
    while pre < MAX_PREFIX_16 {
        if r.get_bit() == 0 {
            break;
        }
        pre += 1;
    }
    if pre >= MAX_PREFIX_16 {
        return r.get_long(MAX_DATATYPE_BITS_16) as u32;
    }
    if k == 0 {
        return pre * m;
    }
    let v = r.peek(k);
    if v < 2 {
        r.skip(k.saturating_sub(1));
        pre * m
    } else {
        r.skip(k);
        pre * m + (v - 1)
    }
}

/// Write `n` one-bits — the unary prefix's run, MSB first. The reference's
/// own unary code counts **1-bits**, terminated by a **0-bit** (`lead(~x)`
/// on decode counts leading ones); `BitWriter::put_zeros` is the wrong
/// polarity for this format, unlike e.g. FLAC's zero-run-terminated-by-one
/// Rice code.
fn put_ones(w: &mut BitWriter, mut n: u32) {
    while n > 32 {
        w.put(32, u32::MAX);
        n -= 32;
    }
    if n > 0 {
        w.put(n, u32::MAX);
    }
}

/// `dyn_code_32bit`: the write-side mirror of [`dyn_get_32bit`]. `n` is the
/// already zigzag-folded, non-negative value to encode.
fn dyn_code_32bit(w: &mut BitWriter, m: u32, k: u32, n: u32, maxbits: u32) {
    if m == 0 {
        // The reference guards div-by-zero implicitly (`k` is always >= 1 in
        // dyn_comp's caller, so `m = (1 << k) - 1 >= 1`); kept explicit here
        // since `clippy::indexing_slicing`-style discipline in this crate
        // wants every arithmetic hazard named, not just the ones the
        // reference happened to avoid by construction.
        put_ones(w, MAX_PREFIX_32);
        w.put_long(u32::from(maxbits.min(32) != 0), 0); // unreachable in practice; see caller
        return;
    }
    #[expect(
        clippy::integer_division,
        reason = "Golomb coding's quotient/remainder split is exactly this division"
    )]
    let div = n / m;
    if div < MAX_PREFIX_32 {
        let modulo = n - m * div;
        let de = u32::from(modulo == 0);
        let num_bits = div + k + 1 - de;
        if num_bits <= 25 {
            // `div` ones, then a (k + 1 - de)-bit field holding `modulo + 1
            // - de` — the terminator and the value share that field: a
            // nonzero remainder's field starts with an implicit zero (its
            // top bit, since modulo + 1 <= m < 2^k fits in k bits and the
            // field is k + 1 bits wide), and a zero remainder's field is
            // all zero and one bit shorter.
            put_ones(w, div);
            w.put(1, 0); // the stop bit ends the unary prefix unconditionally
            let value_bits = num_bits - div - 1;
            if value_bits > 0 {
                w.put(value_bits, modulo + 1 - de);
            }
            return;
        }
    }
    // Escape: MAX_PREFIX_32 ones, no terminator, then the raw value.
    for _ in 0..MAX_PREFIX_32 {
        w.put(1, 1);
    }
    w.put_long(maxbits.min(32), u64::from(n));
}

/// `dyn_code`: the 16-bit-escape write-side mirror of [`dyn_get_16bit`].
fn dyn_code_16bit(w: &mut BitWriter, m: u32, k: u32, n: u32) {
    if m == 0 {
        for _ in 0..MAX_PREFIX_16 {
            w.put(1, 1);
        }
        w.put(MAX_DATATYPE_BITS_16, n);
        return;
    }
    #[expect(
        clippy::integer_division,
        reason = "Golomb coding's quotient/remainder split is exactly this division"
    )]
    let div = n / m;
    if div < MAX_PREFIX_16 {
        let modulo = n - m * div;
        let de = u32::from(modulo == 0);
        let num_bits = div + k + 1 - de;
        if num_bits <= MAX_PREFIX_16 + MAX_DATATYPE_BITS_16 {
            put_ones(w, div);
            w.put(1, 0);
            let value_bits = num_bits - div - 1;
            if value_bits > 0 {
                w.put(value_bits, modulo + 1 - de);
            }
            return;
        }
    }
    for _ in 0..MAX_PREFIX_16 {
        w.put(1, 1);
    }
    w.put(MAX_DATATYPE_BITS_16, n);
}

/// `dyn_decomp`: decode `num_samples` residuals. `maxbits` is the channel's
/// bit width (`chanBits` in the reference) — the escape-code width.
#[allow(
    clippy::many_single_char_names,
    reason = "names (m, k, n, c, mb, pb, kb) deliberately match the reference's own dyn_decomp for auditability against the cited source"
)]
pub(crate) fn dyn_decomp(
    params: &AgParams,
    r: &mut BitReader<'_>,
    num_samples: usize,
    maxbits: u32,
) -> Vec<i32> {
    let mut out = Vec::new();
    let mut mb = params.mb0;
    let mut zmode = false;
    let mut c = 0usize;

    while c < num_samples {
        let m0 = mb >> QBSHIFT;
        let k = lg3a(m0).min(params.kb).max(1);
        let m = (1u32 << k) - 1;

        let n = dyn_get_32bit(r, m, k, maxbits);
        let ndecode = n + u32::from(zmode);
        let multiplier: i64 = if ndecode & 1 == 1 { -1 } else { 1 };
        let del = (i64::from(ndecode).wrapping_add(1) >> 1).wrapping_mul(multiplier);
        out.push(del as i32);
        c += 1;

        mb = params
            .pb
            .wrapping_mul(n + u32::from(zmode))
            .wrapping_add(mb)
            .wrapping_sub((params.pb.wrapping_mul(mb)) >> QBSHIFT);
        if n > N_MAX_MEAN_CLAMP {
            mb = N_MEAN_CLAMP_VAL;
        }
        zmode = false;

        if (mb << MMULSHIFT) < QB && c < num_samples {
            zmode = true;
            let k2 = lead(mb)
                .wrapping_sub(BITOFF)
                .wrapping_add((mb.wrapping_add(MOFF)) >> MDENSHIFT);
            let mz = ((1u32 << (k2 & 31)) - 1) & params.wb;
            let run = dyn_get_16bit(r, mz, k2 & 31);
            let run = run.min((num_samples - c) as u32);
            let run_usize = run as usize; // u32 -> usize always widens on supported targets
            out.extend(std::iter::repeat_n(0i32, run_usize));
            c += run_usize;
            if run >= 65535 {
                zmode = false;
            }
            mb = 0;
        }
    }
    out
}

/// `dyn_comp`: the write-side mirror of [`dyn_decomp`]. `residuals` are
/// the predictor's output (`frame_codec::encode`'s `pc_block` call), or
/// the raw samples for the `order == 0` case.
#[allow(
    clippy::many_single_char_names,
    reason = "names deliberately match the reference's own dyn_comp for auditability against the cited source"
)]
pub(crate) fn dyn_comp(params: &AgParams, w: &mut BitWriter, residuals: &[i32], maxbits: u32) {
    let mut mb = params.mb0;
    let mut zmode = false;
    let mut c = 0usize;
    let num_samples = residuals.len();

    while c < num_samples {
        let m0 = mb >> QBSHIFT;
        let k = lg3a(m0).min(params.kb).max(1);
        let m = (1u32 << k) - 1;

        let del = i64::from(residuals.get(c).copied().unwrap_or(0));
        let n = (del.unsigned_abs() << 1) as u32;
        let sign_bit = u32::from(del < 0);
        let n = n.wrapping_sub(sign_bit).wrapping_sub(u32::from(zmode));

        dyn_code_32bit(w, m, k, n, maxbits);
        c += 1;

        mb = params
            .pb
            .wrapping_mul(n + u32::from(zmode))
            .wrapping_add(mb)
            .wrapping_sub((params.pb.wrapping_mul(mb)) >> QBSHIFT);
        if n > N_MAX_MEAN_CLAMP {
            mb = N_MEAN_CLAMP_VAL;
        }
        zmode = false;

        if (mb << MMULSHIFT) < QB && c < num_samples {
            zmode = true;
            let mut run = 0u32;
            while c < num_samples && residuals.get(c).copied().unwrap_or(1) == 0 {
                c += 1;
                run += 1;
                if run >= 65535 {
                    zmode = false;
                    break;
                }
            }
            let k2 = lead(mb)
                .wrapping_sub(BITOFF)
                .wrapping_add((mb.wrapping_add(MOFF)) >> MDENSHIFT);
            let mz = ((1u32 << (k2 & 31)) - 1) & params.wb;
            dyn_code_16bit(w, mz, k2 & 31, run);
            mb = 0;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn round_trip(params: &AgParams, residuals: &[i32], maxbits: u32) -> Vec<i32> {
        let mut w = BitWriter::new();
        dyn_comp(params, &mut w, residuals, maxbits);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        dyn_decomp(params, &mut r, residuals.len(), maxbits)
    }

    #[test]
    fn small_residuals_round_trip() {
        let params = AgParams::new(10, 40, 14);
        let residuals = [0, 1, -1, 2, -2, 3, -3, 0, 0, 0, 5, -5];
        assert_eq!(round_trip(&params, &residuals, 18), residuals);
    }

    #[test]
    fn a_run_of_zeros_round_trips() {
        let params = AgParams::new(10, 40, 14);
        let mut residuals = vec![7, -3];
        residuals.extend(std::iter::repeat_n(0, 200));
        residuals.push(9);
        assert_eq!(round_trip(&params, &residuals, 18), residuals);
    }

    #[test]
    fn large_values_trigger_escape_and_still_round_trip() {
        let params = AgParams::new(10, 40, 14);
        let residuals = [1_000_000, -1_000_000, 0, 5, 65535, -65535];
        assert_eq!(round_trip(&params, &residuals, 24), residuals);
    }

    #[test]
    fn full_scale_16_bit_residuals_round_trip() {
        let params = AgParams::new(10, 40, 14);
        let residuals: Vec<i32> = (0..500)
            .map(|i: u32| {
                let hashed = i.wrapping_mul(2_654_435_761_u32);
                (hashed >> 16).cast_signed() % 65536 - 32768
            })
            .collect();
        assert_eq!(round_trip(&params, &residuals, 17), residuals);
    }

    #[test]
    fn lg3a_matches_hand_computed_values() {
        // lg3a(0) = 1 (x=3, lead(3) = 30, 31-30=1).
        assert_eq!(lg3a(0), 1);
        // lg3a(5) = floor(log2(8)) = 3.
        assert_eq!(lg3a(5), 3);
        // lg3a(509) -> x=512 -> lead(512)=22 -> 31-22=9.
        assert_eq!(lg3a(509), 9);
    }
}
