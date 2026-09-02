//! One 8x8 block: entropy-coded coefficients in, a residual sample block
//! out. Shared in *shape* by both formats (decode coefficients, inverse
//! zig-zag, dequantise, inverse DCT) but each format cites its own clause
//! for the coefficient VLC and dequantisation formula.
//!
//! `Vaco-Spec-Ref: itu-t-h261` §3.2.5/Table 5/Table 6 (dequantisation,
//! `TCOEFF`, `INTRA-DC` reconstruction) and `itu-t-h263` §6.2/§5.4.1/
//! Table 13/Table 14 (the same three, H.263's own numbering).
//!
//! The two formats' dequantisation formulas turn out to be the *same*
//! formula once stated in signed terms (H.261 §3.2.5 states it as two
//! sign-conditional cases; H.263 §6.2.1 states it as an absolute-value
//! formula with the sign reapplied afterward — substituting `level =
//! sign * |level|` into H.263's form reproduces H.261's two cases exactly),
//! so [`dequant_ac`] is one function for both, cited against both clauses.

use vaco_bitstream::BitReader;
use vaco_codec_dsp_idct::mpeg2::Idct8x8;
use vaco_core::{Error, Result};

use crate::tables::{self, H261Coeff, H263Coeff};
use crate::vlc;

/// This crate's inverse DCT precision — see `vaco-codec-mpeg12`'s own
/// `Mpeg2Idct` alias for why `f32`: Annex A of both H.261 (§3.2.4) and
/// H.263 (implicitly, via the same accuracy-bound framing) specifies an
/// error bound on the inverse transform, not a mandated bit-exact integer
/// algorithm, so any implementation meeting the bound is conforming.
pub(crate) type H26xIdct = Idct8x8<f32>;

/// H.261 Table 6 / H.263 Table 12: an 8-bit `INTRADC` codeword reconstructs
/// to `8 * n`, except the all-ones codeword (255), which reconstructs to
/// 1024 rather than the `8 * 255 = 2040` the linear rule would give — both
/// specs state this exception in the identical words ("the code 1000 0000
/// is not used, the reconstruction level of 1024 being coded as
/// 1111 1111").
#[must_use]
pub(crate) fn intra_dc(codeword: u8) -> i32 {
    if codeword == 255 {
        1024
    } else {
        i32::from(codeword) * 8
    }
}

/// H.261 §3.2.5 / H.263 §6.2.1: dequantise one non-`INTRADC` coefficient.
/// `level` is the signed decoded value (magnitude from the VLC table, sign
/// from the trailing sign bit or the escape code's own sign). Clipped to
/// `-2048..=2047` per both specs' own clipping clause.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "H.263 §6.2.1's own formula uses '-1' on the even-QUANT branch, not a '/2'-style truncation — there is no division here to approximate"
)]
pub(crate) fn dequant_ac(level: i32, quant: u8) -> i32 {
    dequant_ac_ranged(level, quant, -2048, 2047)
}

/// As [`dequant_ac`], but with an explicit clip range. Annex T §T.5's
/// restriction 1 (`Vaco-Spec-Ref: itu-t-h263` T.5) widens the
/// reconstruction-level clip to `|REC| < 4096` when the Modified
/// Quantization mode is in use, in place of the baseline formula's own
/// implicit 12-bit signed clip — see [`dequant_ac_mq`].
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "H.263 §6.2.1's own formula uses '-1' on the even-QUANT branch, not a '/2'-style truncation — there is no division here to approximate"
)]
fn dequant_ac_ranged(level: i32, quant: u8, lo: i32, hi: i32) -> i32 {
    if level == 0 {
        return 0;
    }
    let q = i32::from(quant);
    let mag = i32::try_from(level.unsigned_abs()).unwrap_or(i32::MAX);
    let unsigned = if quant % 2 == 1 {
        q * (2 * mag + 1)
    } else {
        q * (2 * mag + 1) - 1
    };
    let rec = if level < 0 { -unsigned } else { unsigned };
    rec.clamp(lo, hi)
}

/// Annex T §T.5 (`Vaco-Spec-Ref: itu-t-h263` T.5): the same dequantisation
/// formula as [`dequant_ac`], with the Modified Quantization mode's own
/// wider `|REC| < 4096` restriction used as the clip bound instead of the
/// baseline mode's 12-bit signed range.
#[must_use]
pub(crate) fn dequant_ac_mq(level: i32, quant: u8) -> i32 {
    dequant_ac_ranged(level, quant, -4095, 4095)
}

/// Annex T, Table T.2 (`Vaco-Spec-Ref: itu-t-h263` T.3): `QUANT_C`, the
/// chrominance-only quantisation parameter the Modified Quantization mode
/// derives from the transmitted (luminance) `QUANT`.
#[must_use]
pub(crate) const fn quant_c(quant: u8) -> u8 {
    match quant {
        1..=6 => quant,
        7..=9 => quant - 1,
        10..=11 => 9,
        12..=13 => 10,
        14..=15 => 11,
        16..=18 => 12,
        19..=21 => 13,
        22..=26 => 14,
        _ => 15,
    }
}

/// Inverse zig-zag (H.261 Figure 12 / H.263 Figure 13, mechanically the
/// same pattern — see [`tables::ZIGZAG_SCAN`]'s own doc comment): `qfs` is
/// indexed by transmission order, the result by natural `row * 8 + col`
/// order.
#[must_use]
pub(crate) fn inverse_scan(qfs: &[i32; 64]) -> [i32; 64] {
    let mut qf = [0i32; 64];
    for (n, &(row, col)) in tables::ZIGZAG_SCAN.iter().enumerate() {
        let pos = usize::from(row) * 8 + usize::from(col);
        if let (Some(slot), Some(&v)) = (qf.get_mut(pos), qfs.get(n)) {
            *slot = v;
        }
    }
    qf
}

/// Dequantise an already inverse-scanned, natural-order block. Position 0
/// is passed through unchanged when `intra` — it already holds the final
/// reconstructed `INTRADC` sample value written by the coefficient decoder,
/// not a quantised level — and dequantised like every other position
/// otherwise (an INTER block's position 0 is an ordinary coded
/// coefficient, not an `INTRADC`).
#[must_use]
pub(crate) fn dequantise(qf: &[i32; 64], quant: u8, intra: bool) -> [i32; 64] {
    dequantise_ranged(qf, quant, intra, false)
}

/// As [`dequantise`], but selecting Annex T's widened clip range (see
/// [`dequant_ac_mq`]) when `mq` (Modified Quantization mode active) is
/// set. `quant` here is whichever of `QUANT`/`QUANT_C` applies to this
/// block's own plane — the caller (not this function) is what knows the
/// plane, per Annex T §T.3's chrominance-only substitution.
#[must_use]
pub(crate) fn dequantise_ranged(qf: &[i32; 64], quant: u8, intra: bool, mq: bool) -> [i32; 64] {
    let mut f = [0i32; 64];
    for (i, slot) in f.iter_mut().enumerate() {
        let coeff = qf.get(i).copied().unwrap_or(0);
        *slot = if intra && i == 0 {
            coeff
        } else if mq {
            dequant_ac_mq(coeff, quant)
        } else {
            dequant_ac(coeff, quant)
        };
    }
    f
}

/// Run the inverse DCT and saturate to `[-256, 255]`, matching
/// `vaco-codec-mpeg12::block::inverse_transform`'s own rounding rationale
/// (round-to-nearest against a floating-point transform, since neither
/// spec's Annex A mandates bit-exact integer arithmetic).
#[must_use]
pub(crate) fn inverse_transform(idct: &mut H26xIdct, f: &[i32; 64]) -> [i32; 64] {
    let mut coeffs = [0f32; 64];
    for (dst, &src) in coeffs.iter_mut().zip(f.iter()) {
        *dst = src as f32;
    }
    let mut out_f = [0f32; 64];
    idct.apply(&coeffs, &mut out_f);
    let mut out = [0i32; 64];
    for (dst, &src) in out.iter_mut().zip(out_f.iter()) {
        *dst = (src.round() as i32).clamp(-256, 255);
    }
    out
}

/// H.261 §4.3.3 (`TCOEFF`): decode one block's coefficients in transmission
/// order. `intra` selects whether the first coefficient is instead the
/// separate 8-bit `INTRADC` codeword (§4.3.1) — never true for `TCOEFF`
/// data itself.
///
/// The `run=0, level=1` event has two codes: `"1"`, legal **only** as the
/// very first coefficient of a non-intra block (Table 5's own footnote:
/// "EOB cannot occur as the first coefficient", which is exactly what
/// makes `"1"` unambiguous there — with EOB excluded from the candidate
/// set, no other code starts with `1`), and `"11"`, legal everywhere else.
/// Both codes cannot be simultaneously decodable (`"1"` would always win,
/// having matched first), so which one is offered is picked by position,
/// not discovered from the bits.
pub(crate) fn decode_h261_coefficients(r: &mut BitReader<'_>, intra: bool) -> Result<[i32; 64]> {
    let mut qfs = [0i32; 64];
    let mut n: usize = if intra {
        let dc = r.get(8) as u8;
        if let Some(slot) = qfs.first_mut() {
            *slot = intra_dc(dc);
        }
        1
    } else {
        0
    };

    // `n < 65`, not `n < 64`: exactly the same lesson
    // `vaco-codec-mpeg12::block::decode_coefficients` documents at its own
    // loop bound. An encoder still writes an explicit EOB even when
    // coefficients have already filled every position (`n == 64`) — a
    // `n < 64` bound stops one VLC read short of that EOB, leaving it
    // unconsumed for the *next* block to misread as its own first code.
    // Found by exactly that symptom: an otherwise-correct-looking H.261
    // intra macroblock decoding two clean blocks and then corrupting the
    // third, purely from this off-by-one.
    while n < 65 {
        let is_first = n == 0;
        let matched: Option<&H261Coeff> = vlc::decode(
            r,
            tables::H261_TCOEFF.iter().filter(|c: &&H261Coeff| {
                if is_first {
                    c.run != tables::H261_EOB && !(!c.first_only && c.run == 0 && c.level == 1)
                } else {
                    !c.first_only
                }
            }),
            |c| c.bits,
            13,
        );
        let Some(row) = matched else {
            return Err(Error::InvalidData("h261: no TCOEFF VLC matched"));
        };
        if row.run == tables::H261_EOB {
            break;
        }
        if row.run == tables::H261_ESCAPE {
            let run = r.get(6) as usize;
            let level = r.get_signed(8);
            n += run;
            if let Some(slot) = qfs.get_mut(n) {
                *slot = level;
            }
            n += 1;
        } else {
            let run = usize::try_from(row.run).unwrap_or(0);
            let sign = r.get_bit();
            let level = if sign == 1 {
                -i32::from(row.level)
            } else {
                i32::from(row.level)
            };
            n += run;
            if let Some(slot) = qfs.get_mut(n) {
                *slot = level;
            }
            n += 1;
        }
        if r.check().is_err() {
            return Err(Error::InvalidData("h261: bitstream overrun mid-block"));
        }
    }
    Ok(qfs)
}

/// Annex T §T.2 (`Vaco-Spec-Ref: itu-t-h263` T.2), Table T.1: decode the
/// variable-length `DQUANT` field the Modified Quantization mode replaces
/// the baseline fixed 2-bit `DQUANT` with, returning the new (already
/// clamped to `1..=31`) `QUANT` value. `prior_quant` is the macroblock's
/// `QUANT` value *before* this update, since Table T.1's small-step delta
/// depends on it.
#[must_use]
pub(crate) fn decode_mq_dquant(r: &mut BitReader<'_>, prior_quant: u8) -> u8 {
    if r.get_bit() == 0 {
        // §T.2.2: five more bits give the new QUANT value directly.
        return (r.get(5) as u8).clamp(1, 31);
    }
    // §T.2.1: one more bit selects between Table T.1's two delta columns.
    let second = r.get_bit();
    let delta: i32 = match prior_quant {
        1 => {
            if second == 0 {
                2
            } else {
                1
            }
        }
        2..=10 => {
            if second == 0 {
                -1
            } else {
                1
            }
        }
        11..=20 => {
            if second == 0 {
                -2
            } else {
                2
            }
        }
        21..=28 => {
            if second == 0 {
                -3
            } else {
                3
            }
        }
        29 => {
            if second == 0 {
                -3
            } else {
                2
            }
        }
        30 => {
            if second == 0 {
                -3
            } else {
                1
            }
        }
        _ => {
            if second == 0 {
                -3
            } else {
                -5
            }
        }
    };
    (i32::from(prior_quant) + delta).clamp(1, 31) as u8
}

/// Annex D, Table D.3 (`Vaco-Spec-Ref: itu-t-h263` D.2): decode one
/// signed motion vector difference component from the "regularly
/// constructed reversible" table PLUSPTYPE's Unrestricted Motion Vector
/// mode uses in place of Table 14. See [`crate::motion::h263_umv_vector_plus`]
/// for how the result combines with the predictor.
///
/// Every row but magnitude 0 has the shape `0 x_{m-1} 1 x_{m-2} 1 ... x_0
/// 1 s 0` (leading `0`, `m` `(bit, continuation)` pairs carrying the
/// magnitude's bits most-significant first, then a sign bit and a final
/// terminating `0`) with `magnitude = 2^m + (accumulated bits)` — so this
/// reads as one bit, one "more bits follow?" marker, repeated until that
/// marker reads `0`, at which point the bit just read was the sign. The
/// spec's own worked example (`-13`, sign 1, binary `1101`, encoded as
/// `0 11 01 11 10`) is this function's test.
#[must_use]
pub(crate) fn decode_table_d3(r: &mut BitReader<'_>) -> i32 {
    if r.get_bit() == 1 {
        return 0;
    }
    let mut accum: i64 = 0;
    let mut count: u32 = 0;
    loop {
        let b = r.get_bit();
        let m = r.get_bit();
        if m == 1 {
            accum = (accum << 1) | i64::from(b);
            count += 1;
            if count >= 24 {
                return 0; // pathological input guard; never hit by a conforming stream.
            }
            continue;
        }
        let magnitude = (1i64 << count) + accum;
        let magnitude = i32::try_from(magnitude).unwrap_or(i32::MAX);
        return if b == 1 { -magnitude } else { magnitude };
    }
}

/// Annex T §T.4, Figure T.1 (`Vaco-Spec-Ref: itu-t-h263` T.4): decode the
/// 11-bit `EXTENDED-LEVEL` field following an `EXTENDED-ESCAPE` marker.
/// Figure T.1 gives the *encoder's* bit order directly: transmitted bits
/// `t0..t10` are the original 11-bit two's-complement `LEVEL` field's own
/// bits `b5,b4,b3,b2,b1,b11,b10,b9,b8,b7,b6` (a right-rotation by 5,
/// stated in the spec as a diagram rather than an arithmetic rotate, and
/// implemented here the same direct way to keep it checkable against that
/// diagram bit-by-bit rather than against a rotate-amount derivation).
#[must_use]
fn decode_extended_level(r: &mut BitReader<'_>) -> i32 {
    let mut t = [0u32; 11];
    for slot in &mut t {
        *slot = r.get_bit();
    }
    // b11..b1, restored to their original (pre-rotation) positions.
    let bits = [
        t[5], t[6], t[7], t[8], t[9], t[10], t[0], t[1], t[2], t[3], t[4],
    ];
    let mut magnitude: u32 = 0;
    for b in bits {
        magnitude = (magnitude << 1) | b;
    }
    // `bits[0]` is b11, the sign bit of this 11-bit two's-complement field.
    (i32::try_from(magnitude).unwrap_or(0) << 21) >> 21
}

/// One decoded `TCOEF` event (H.263 §5.4.2): either a table row or the
/// 7-bit escape marker, which is not itself a [`H263Coeff`] row (see that
/// struct's own doc comment) so it needs its own small variant here.
enum H263Event {
    Row(&'static H263Coeff),
    Escape,
}

/// Manually walks the same prefix-matching algorithm as
/// [`vlc::decode`], extended with the one extra candidate ([`H263Event::Escape`])
/// that isn't a [`H263Coeff`] and so can't share that generic function's
/// single-type table.
fn decode_h263_event(r: &mut BitReader<'_>) -> Option<H263Event> {
    let (escape_code, escape_len) = tables::bits_of(tables::H263_TCOEF_ESCAPE);
    let mut accum: u32 = 0;
    let mut len: u8 = 0;
    while len < 13 {
        accum = (accum << 1) | r.get_bit();
        len += 1;
        if len == escape_len && accum == escape_code {
            return Some(H263Event::Escape);
        }
        for row in tables::H263_TCOEF {
            let (code, code_len) = tables::bits_of(row.bits);
            if code_len == len && code == accum {
                return Some(H263Event::Row(row));
            }
        }
        if r.check().is_err() {
            break;
        }
    }
    None
}

/// H.263 §5.4.2/§6.2.1 (`TCOEF`): decode one block's coefficients in
/// transmission order. Unlike H.261, the end-of-block condition (`LAST`)
/// is a bit embedded in every code (or in the escape word) rather than a
/// separate End-of-Block code, so there is no first-coefficient ambiguity
/// to resolve by position here.
///
/// `intra` and `has_tcoef` are independent, unlike H.261's own "intra
/// implies every block is fully coded" rule: §5.4 states plainly that
/// `INTRADC` is unconditional for an intra block (`MCBPC` type 3/4) while
/// "`TCOEF` is present if indicated by `MCBPC` or `CBPY`" — the same `CBP`
/// gate an inter block's AC data uses. `has_tcoef` is that gate; passing
/// `intra` alone would read a `TCOEF` sequence for a block whose `CBPY`/
/// `CBPC` bit says none exists, silently consuming the *next* block's own
/// `INTRADC` and beyond as garbage coefficient codes.
/// As [`decode_h263_coefficients_mq`] with `mq` false — kept as a
/// distinct name at call sites and in tests that have nothing to do with
/// Annex T, so they read the same as before that annex existed.
#[cfg(test)]
pub(crate) fn decode_h263_coefficients(
    r: &mut BitReader<'_>,
    intra: bool,
    has_tcoef: bool,
) -> Result<[i32; 64]> {
    decode_h263_coefficients_mq(r, intra, has_tcoef, false)
}

/// H.263 §5.4.2/§6.2.1 (`TCOEF`): decode one block's coefficients in
/// transmission order, additionally recognising Annex T's
/// EXTENDED-ESCAPE marker (`Vaco-Spec-Ref: itu-t-h263` T.4) when `mq`
/// (Modified Quantization mode active) is set. Without `mq`, the 8-bit
/// LEVEL codeword `1000 0000` a conforming encoder never sends is simply
/// decoded as its literal two's-complement value (`-128`), same as any
/// other bit pattern — this crate does not reject bitstreams for
/// violating an encoder-side "shall not send" restriction.
pub(crate) fn decode_h263_coefficients_mq(
    r: &mut BitReader<'_>,
    intra: bool,
    has_tcoef: bool,
    mq: bool,
) -> Result<[i32; 64]> {
    let mut qfs = [0i32; 64];
    let mut n: usize = if intra {
        let dc = r.get(8) as u8;
        if let Some(slot) = qfs.first_mut() {
            *slot = intra_dc(dc);
        }
        1
    } else {
        0
    };
    if !has_tcoef {
        return Ok(qfs);
    }

    loop {
        let Some(event) = decode_h263_event(r) else {
            return Err(Error::InvalidData("h263: no TCOEF VLC matched"));
        };
        let (last, run, level) = match event {
            H263Event::Escape => {
                let last = r.get_bit() == 1;
                let run = usize::try_from(r.get(6)).unwrap_or(0);
                let level_bits = r.get(8);
                let level = if mq && level_bits == 0b1000_0000 {
                    decode_extended_level(r)
                } else {
                    // Sign-extend the raw 8-bit two's-complement field
                    // exactly as `BitReader::get_signed` would.
                    (i32::try_from(level_bits).unwrap_or(0) << 24) >> 24
                };
                (last, run, level)
            }
            H263Event::Row(row) => {
                let sign = r.get_bit();
                let level = if sign == 1 {
                    -i32::from(row.level)
                } else {
                    i32::from(row.level)
                };
                (row.last, usize::from(row.run), level)
            }
        };
        n += run;
        if let Some(slot) = qfs.get_mut(n) {
            *slot = level;
        }
        n += 1;
        if r.check().is_err() {
            return Err(Error::InvalidData("h263: bitstream overrun mid-block"));
        }
        if last || n >= 64 {
            break;
        }
    }
    Ok(qfs)
}

// ------------------------------------------------------------ Annex I

/// Annex I §I.3 (`Vaco-Spec-Ref: itu-t-h263` I.3): dequantise one
/// Advanced INTRA Coding coefficient — "RecC(u,v) = 2 * QUANT *
/// LEVEL(u,v)" for *every* coefficient, DC included, with no
/// "dead-zone". Distinct from the baseline INTRA path
/// [`dequantise_ranged`] uses (a fixed step of 8 for DC via Table 15,
/// the ordinary dead-zone formula for AC) — Annex I replaces both with
/// this one formula.
#[must_use]
#[allow(
    dead_code,
    reason = "landed ahead of its consumer: Annex I's macroblock-layer wiring (INTRA_MODE dispatch, neighbour-availability tracking) is not written yet -- see H263_INTRA_TCOEF's own note"
)]
pub(crate) fn annex_i_dequant(level: i32, quant: u8) -> i32 {
    2 * i32::from(quant) * level
}

/// [`annex_i_dequant`] applied to a whole block's 64 coefficients (after
/// [`inverse_scan`] has already put them in raster order).
#[must_use]
#[allow(
    dead_code,
    reason = "landed ahead of its consumer -- see annex_i_dequant"
)]
pub(crate) fn annex_i_dequantise(qf: &[i32; 64], quant: u8) -> [i32; 64] {
    let mut out = [0i32; 64];
    for (o, &level) in out.iter_mut().zip(qf.iter()) {
        *o = annex_i_dequant(level, quant);
    }
    out
}

/// Annex I §I.3: `clipAC()` — "clipping to the range -2048 to 2047".
#[must_use]
#[allow(
    dead_code,
    reason = "landed ahead of its consumer -- see annex_i_dequant"
)]
pub(crate) fn clip_ac(x: i32) -> i32 {
    x.clamp(-2048, 2047)
}

/// Annex I §I.3: `oddifyclipDC(x)` — "If (x is even) { result =
/// clipDC(x+1) } else { result = clipDC(x) }", where `clipDC()` clips to
/// `0..=2047`. Forces the reconstructed DC coefficient odd, to reduce
/// the chance of an IDCT-mismatch-prone exact-half sample value (the
/// primary text's own example: a DC of `8k + 4` inverse-transforms to a
/// constant `k + 0.5`, which different IDCT implementations can round
/// either way).
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the evenness test `x % 2` is not an approximated division; there is no truncating division anywhere in this formula"
)]
#[allow(
    dead_code,
    reason = "landed ahead of its consumer -- see annex_i_dequant"
)]
pub(crate) fn oddify_clip_dc(x: i32) -> i32 {
    let x = if x % 2 == 0 { x + 1 } else { x };
    x.clamp(0, 2047)
}

/// Annex I §I.3: reconstruct one INTRA block's final coefficient values
/// `RecC'(u,v)` (returned row-major, index `v * 8 + u` — `u` is the
/// primary text's own column/horizontal index, `v` its row/vertical
/// one) from `rec_c` (`RecC(u,v)`, already dequantised via
/// [`annex_i_dequantise`]), the macroblock's `INTRA_MODE` (`0` = DC
/// only, `1` = vertical/from-above, `2` = horizontal/from-left,
/// [`tables::H263_INTRA_MODE`]), and its two spatial neighbours' own
/// already-fully-reconstructed values (`RecA'`/`RecB'`) — `None` when
/// that neighbour is not an INTRA block in the same video picture
/// segment (§I.3's own three-part test: within the picture; within the
/// same GOB, or no GOB header sent for it, outside Slice Structured
/// mode; within the same slice, inside it — resolved by the caller, not
/// this function).
///
/// Transcribed directly from the primary text's own pseudocode for all
/// three modes, including the constant-1024/zero fallback when the
/// referenced neighbour is unavailable, and the DC "oddification"
/// ([`oddify_clip_dc`]) every mode applies at the end.
#[must_use]
#[allow(
    dead_code,
    reason = "landed ahead of its consumer -- see annex_i_dequant"
)]
pub(crate) fn annex_i_reconstruct(
    rec_c: &[i32; 64],
    mode: u8,
    block_a: Option<&[i32; 64]>,
    block_b: Option<&[i32; 64]>,
) -> [i32; 64] {
    let at = |block: &[i32; 64], u: usize, v: usize| -> i32 {
        block.get(v * 8 + u).copied().unwrap_or(0)
    };
    let c = |u: usize, v: usize| -> i32 { rec_c.get(v * 8 + u).copied().unwrap_or(0) };

    // Kept as one arm per Annex I §I.3 mode rather than merged by clippy's
    // `match_same_arms` suggestion: modes 0/1/2 have textually identical
    // single-neighbour formulas by coincidence, not by any shared rule in
    // the primary text, and each arm here maps to that mode's own
    // pseudocode paragraph -- merging them would trade that traceability
    // for a few fewer lines.
    #[allow(
        clippy::match_same_arms,
        reason = "each arm mirrors a distinct paragraph of Annex I's own per-mode pseudocode; a textual coincidence between modes is not a reason to fold them together"
    )]
    // The two-neighbour Mode 0 average must truncate towards zero, per
    // §I.3's own "operator '/' ... division by truncation" -- `i32::midpoint`
    // instead rounds towards negative infinity, which disagrees with the
    // spec whenever the sum is odd and negative.
    #[allow(
        clippy::manual_midpoint,
        reason = "i32::midpoint rounds towards negative infinity; Annex I's own '/' is truncating division, so the manual form is the correct one, not a missed simplification"
    )]
    #[allow(
        clippy::integer_division,
        reason = "Annex I §I.3's own Mode 0 formula for two valid neighbours is literally '(RecA'(0,0) + RecB'(0,0)) / 2', operator '/' explicitly defined in that section as division by truncation -- not an approximation of anything else"
    )]
    let temp_dc = match (mode, block_a, block_b) {
        (0, Some(a), Some(b)) => c(0, 0) + (at(a, 0, 0) + at(b, 0, 0)) / 2,
        (0, Some(a), None) => c(0, 0) + at(a, 0, 0),
        (0, None, Some(b)) => c(0, 0) + at(b, 0, 0),
        (1, Some(a), _) => c(0, 0) + at(a, 0, 0),
        (2, _, Some(b)) => c(0, 0) + at(b, 0, 0),
        _ => c(0, 0) + 1024,
    };

    let mut out = [0i32; 64];
    for v in 0..8usize {
        for u in 0..8usize {
            if u == 0 && v == 0 {
                continue; // DC handled separately below (oddification).
            }
            let mut val = c(u, v);
            match mode {
                1 if v == 0 => {
                    if let Some(a) = block_a {
                        val += at(a, u, 0);
                    }
                }
                2 if u == 0 => {
                    if let Some(b) = block_b {
                        val += at(b, 0, v);
                    }
                }
                _ => {}
            }
            if let Some(slot) = out.get_mut(v * 8 + u) {
                *slot = clip_ac(val);
            }
        }
    }
    if let Some(slot) = out.get_mut(0) {
        *slot = oddify_clip_dc(temp_dc);
    }
    out
}

#[cfg(test)]
mod annex_i_reconstruct_tests {
    use super::*;

    fn block(values: &[(usize, usize, i32)]) -> [i32; 64] {
        let mut b = [0i32; 64];
        for &(u, v, val) in values {
            if let Some(slot) = b.get_mut(v * 8 + u) {
                *slot = val;
            }
        }
        b
    }

    #[test]
    fn dequant_matches_the_no_dead_zone_doubling_formula() {
        // Annex I §I.3: RecC = 2 * QUANT * LEVEL, for every coefficient,
        // unlike the baseline path's separate dead-zone/Table-15 rules.
        assert_eq!(annex_i_dequant(5, 4), 40);
        assert_eq!(annex_i_dequant(-3, 10), -60);
        assert_eq!(annex_i_dequant(0, 31), 0);
    }

    #[test]
    fn dequantise_applies_dequant_to_every_coefficient_independently() {
        let mut qf = [0i32; 64];
        if let Some(s) = qf.get_mut(0) {
            *s = 2;
        }
        if let Some(s) = qf.get_mut(63) {
            *s = -1;
        }
        let out = annex_i_dequantise(&qf, 5);
        assert_eq!(out.first().copied(), Some(20));
        assert_eq!(out.get(63).copied(), Some(-10));
        assert_eq!(out.get(1).copied(), Some(0));
    }

    #[test]
    fn clip_ac_clamps_to_minus_2048_2047() {
        assert_eq!(clip_ac(-3000), -2048);
        assert_eq!(clip_ac(3000), 2047);
        assert_eq!(clip_ac(0), 0);
        assert_eq!(clip_ac(-2048), -2048);
        assert_eq!(clip_ac(2047), 2047);
    }

    #[test]
    fn oddify_clip_dc_forces_the_result_odd_before_clipping() {
        assert_eq!(oddify_clip_dc(4), 5); // even -> +1
        assert_eq!(oddify_clip_dc(5), 5); // already odd -> unchanged
        assert_eq!(oddify_clip_dc(0), 1);
        assert_eq!(oddify_clip_dc(-2), 0); // even negative -> +1 = -1, then clipDC clamps to 0
    }

    #[test]
    fn oddify_clip_dc_clips_after_oddifying() {
        // clipDC is 0..=2047: an even value one below the ceiling still
        // oddifies past it and must be clamped back down.
        assert_eq!(oddify_clip_dc(2046), 2047);
        assert_eq!(oddify_clip_dc(2047), 2047);
        assert_eq!(oddify_clip_dc(-1), 0); // odd, unchanged by oddify, then clamped to 0
        assert_eq!(oddify_clip_dc(-4), 0); // even -> -3, clamped to 0
    }

    #[test]
    fn mode0_dc_only_averages_both_neighbours_and_leaves_ac_untouched() {
        let c = block(&[(0, 0, 100), (3, 2, 7)]);
        let a = block(&[(0, 0, 10)]);
        let b = block(&[(0, 0, 21)]);
        let out = annex_i_reconstruct(&c, 0, Some(&a), Some(&b));
        // tempDC = 100 + (10 + 21) / 2 = 100 + 15 (truncated) = 115, odd already.
        assert_eq!(out.first().copied(), Some(115));
        // Mode 0 never predicts AC coefficients, only clips them.
        assert_eq!(out.get(2 * 8 + 3).copied(), Some(7));
    }

    #[test]
    fn mode0_dc_average_truncates_towards_zero_on_a_negative_sum() {
        let c = block(&[(0, 0, 0)]);
        let a = block(&[(0, 0, -3)]);
        let b = block(&[(0, 0, -2)]);
        // (-3 + -2) / 2 = -5 / 2 = -2 in Rust's truncating division, matching
        // Annex I §I.3's own "division by truncation" -- not -3 (floor).
        let out = annex_i_reconstruct(&c, 0, Some(&a), Some(&b));
        // tempDC = 0 + -2 = -2 (even) -> oddify -> -1 -> clipDC -> 0.
        assert_eq!(out.first().copied(), Some(0));
    }

    #[test]
    fn mode0_falls_back_to_single_neighbour_when_only_one_is_available() {
        let c = block(&[(0, 0, 50)]);
        let a = block(&[(0, 0, 8)]);
        let only_a = annex_i_reconstruct(&c, 0, Some(&a), None);
        assert_eq!(only_a.first().copied(), Some(59)); // 50+8=58 even -> 59

        let b = block(&[(0, 0, 8)]);
        let only_b = annex_i_reconstruct(&c, 0, None, Some(&b));
        assert_eq!(only_b.first().copied(), Some(59));
    }

    #[test]
    fn mode0_falls_back_to_the_constant_when_neither_neighbour_is_available() {
        let c = block(&[(0, 0, 3)]);
        let out = annex_i_reconstruct(&c, 0, None, None);
        // tempDC = 3 + 1024 = 1027, already odd.
        assert_eq!(out.first().copied(), Some(1027));
    }

    #[test]
    fn mode1_vertical_predicts_only_row_zero_from_block_above() {
        let c = block(&[(0, 0, 40), (1, 0, 5), (4, 0, -5), (1, 3, 9)]);
        let a = block(&[(0, 0, 20), (1, 0, 100), (4, 0, -3000)]);
        let out = annex_i_reconstruct(&c, 1, Some(&a), None);
        // DC: tempDC = 40 + 20 = 60 (even) -> 61.
        assert_eq!(out.first().copied(), Some(61));
        // Row 0, u=1: 5 + 100 = 105, within clipAC range.
        assert_eq!(out.get(1).copied(), Some(105));
        // Row 0, u=4: -5 + -3000 = -3005, clipped to -2048.
        assert_eq!(out.get(4).copied(), Some(-2048));
        // Row v=3 is untouched by Mode 1's prediction (only row 0 is).
        assert_eq!(out.get(3 * 8 + 1).copied(), Some(9));
    }

    #[test]
    fn mode1_without_block_above_adds_nothing_to_any_ac_coefficient() {
        let c = block(&[(0, 0, 12), (2, 0, 7)]);
        let out = annex_i_reconstruct(&c, 1, None, None);
        assert_eq!(out.first().copied(), Some(1037)); // 12 + 1024 = 1036 -> 1037
        assert_eq!(out.get(2).copied(), Some(7)); // untouched, just clipped
    }

    #[test]
    fn mode2_horizontal_predicts_only_column_zero_from_block_to_the_left() {
        let c = block(&[(0, 0, 40), (0, 1, 5), (0, 4, -5), (3, 1, 9)]);
        let b = block(&[(0, 0, 20), (0, 1, 100), (0, 4, -3000)]);
        let out = annex_i_reconstruct(&c, 2, None, Some(&b));
        assert_eq!(out.first().copied(), Some(61)); // 40+20=60 -> 61
        assert_eq!(out.get(8).copied(), Some(105)); // column 0, v=1: 5+100
        assert_eq!(out.get(4 * 8).copied(), Some(-2048)); // -5 + -3000, clipped
        // Column u=3 is untouched by Mode 2's prediction (only column 0 is).
        assert_eq!(out.get(8 + 3).copied(), Some(9));
    }

    #[test]
    fn mode2_without_block_left_adds_nothing_to_any_ac_coefficient() {
        let c = block(&[(0, 0, 12), (0, 2, 7)]);
        let out = annex_i_reconstruct(&c, 2, None, None);
        assert_eq!(out.first().copied(), Some(1037));
        assert_eq!(out.get(2 * 8).copied(), Some(7));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut byte = 0u8;
        let mut count = 0u8;
        for b in bits.bytes() {
            byte = (byte << 1) | u8::from(b == b'1');
            count += 1;
            if count == 8 {
                bytes.push(byte);
                byte = 0;
                count = 0;
            }
        }
        if count > 0 {
            byte <<= 8 - count;
            bytes.push(byte);
        }
        bytes
    }

    #[test]
    fn intra_dc_is_linear_except_the_1024_exception() {
        assert_eq!(intra_dc(16), 128);
        assert_eq!(intra_dc(1), 8);
        assert_eq!(intra_dc(255), 1024);
        assert_ne!(intra_dc(255), 255 * 8);
    }

    #[test]
    fn dequant_ac_matches_h261s_own_worked_table() {
        // H.261 Table (§3.2.5's own reconstruction-level table): QUANT=8
        // (even), level=1 -> REC = 8*(2*1+1)-1 = 23.
        assert_eq!(dequant_ac(1, 8), 23);
        assert_eq!(dequant_ac(-1, 8), -23);
        // QUANT=1 (odd), level=127 -> REC = 1*(2*127+1) = 255.
        assert_eq!(dequant_ac(127, 1), 255);
        assert_eq!(dequant_ac(0, 8), 0);
    }

    #[test]
    fn dequant_ac_clips_to_the_12_bit_range() {
        assert_eq!(dequant_ac(127, 31), 2047);
        assert_eq!(dequant_ac(-127, 31), -2048);
    }

    #[test]
    fn h261_first_coefficient_of_a_non_intra_block_accepts_the_short_code() {
        // "1" (first_only run=0/level=1) + sign=0, then EOB "10".
        let bytes = bits_to_bytes("1010");
        let mut r = BitReader::new(&bytes);
        // `unwrap_or_default` rather than asserting `Ok` directly: a
        // decode failure here still surfaces as a test failure, via the
        // `assert_eq` below comparing against an all-zero array instead
        // of the expected one.
        let qfs = decode_h261_coefficients(&mut r, false).unwrap_or([0i32; 64]);
        assert_eq!(qfs.first().copied(), Some(1));
    }

    #[test]
    fn h261_general_code_is_used_after_the_first_coefficient() {
        // First coeff: "1" + sign 0 (run=0, level=1). Second: "11" + sign 0
        // (run=0, level=1) at position 1. Then EOB "10".
        let bytes = bits_to_bytes("1011010");
        let mut r = BitReader::new(&bytes);
        let qfs = decode_h261_coefficients(&mut r, false).unwrap_or([0i32; 64]);
        assert_eq!(qfs.first().copied(), Some(1));
        assert_eq!(qfs.get(1).copied(), Some(1));
    }

    #[test]
    fn h261_intra_block_dc_then_eob() {
        // 8-bit INTRADC codeword (16 -> 128), then EOB "10".
        let bytes = bits_to_bytes("0001000010");
        let mut r = BitReader::new(&bytes);
        let qfs = decode_h261_coefficients(&mut r, true).unwrap_or([0i32; 64]);
        assert_eq!(qfs.first().copied(), Some(128));
    }

    #[test]
    fn h263_last_bit_ends_the_block_without_a_separate_eob_code() {
        // Pick any row whose own LAST bit is set and feed just that one
        // code plus a sign bit: no separate EOB code is needed to stop.
        // `unwrap_or` falls back to a row that would fail the assertion
        // below rather than needing `.expect`/`panic!` if the table were
        // ever (incorrectly) empty of `LAST` rows.
        let last_row = tables::H263_TCOEF
            .iter()
            .find(|c| c.last)
            .unwrap_or(&H263Coeff {
                bits: "0",
                last: false,
                run: 0,
                level: 0,
            });
        let bytes = bits_to_bytes(&format!("{}0", last_row.bits));
        let mut r = BitReader::new(&bytes);
        let qfs = decode_h263_coefficients(&mut r, false, true).unwrap_or([0i32; 64]);
        assert_eq!(
            qfs.get(usize::from(last_row.run)).copied(),
            Some(i32::from(last_row.level))
        );
    }

    #[test]
    fn h263_escape_code_reads_last_run_and_a_signed_level() {
        // Escape "0000011", then LAST=1, RUN=000010 (2), LEVEL = 8-bit
        // two's complement for -5 (11111011).
        let mut bits = String::from("0000011");
        bits.push('1'); // LAST
        bits.push_str("000010"); // RUN = 2
        bits.push_str("11111011"); // LEVEL = -5
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let qfs = decode_h263_coefficients(&mut r, false, true).unwrap_or([0i32; 64]);
        assert_eq!(qfs.get(2).copied(), Some(-5));
    }

    #[test]
    fn quant_c_matches_table_t2s_own_bands() {
        assert_eq!(quant_c(1), 1);
        assert_eq!(quant_c(6), 6);
        assert_eq!(quant_c(7), 6);
        assert_eq!(quant_c(9), 8);
        assert_eq!(quant_c(10), 9);
        assert_eq!(quant_c(13), 10);
        assert_eq!(quant_c(18), 12);
        assert_eq!(quant_c(21), 13);
        assert_eq!(quant_c(26), 14);
        assert_eq!(quant_c(31), 15);
    }

    #[test]
    fn dequant_ac_mq_allows_magnitudes_the_baseline_clip_would_cut_off() {
        // QUANT=31 (odd), level=64 -> unsigned = 31*(2*64+1) = 3999, well
        // past the baseline clip's 2047 ceiling but under Annex T's 4095.
        assert_eq!(dequant_ac(64, 31), 2047);
        assert_eq!(dequant_ac_mq(64, 31), 3999);
    }

    #[test]
    fn mq_dquant_small_step_follows_table_t1() {
        // Worked example straight from §T.2.1: prior QUANT 29, DQUANT
        // "11" (first bit 1, second bit 1) -> delta +2 -> new QUANT 31.
        let bytes = bits_to_bytes("11");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_mq_dquant(&mut r, 29), 31);
        // Prior QUANT 1, DQUANT "10" -> delta +2 -> new QUANT 3.
        let bytes = bits_to_bytes("10");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_mq_dquant(&mut r, 1), 3);
    }

    #[test]
    fn mq_dquant_arbitrary_selection_follows_section_t2_2() {
        // §T.2.2's own worked example: "001111" -> new QUANT 15,
        // regardless of the prior value.
        let bytes = bits_to_bytes("001111");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_mq_dquant(&mut r, 7), 15);
    }

    #[test]
    fn table_d3_decodes_zero_as_the_single_bit_one() {
        let bytes = bits_to_bytes("1");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_table_d3(&mut r), 0);
    }

    #[test]
    fn table_d3_matches_the_specs_own_worked_example() {
        // §D.2's worked example: -13, sign=1, binary 1101, encoded as
        // "0 11 01 11 10" -> concatenated "011011110".
        let bytes = bits_to_bytes("011011110");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_table_d3(&mut r), -13);
    }

    #[test]
    fn table_d3_round_trips_small_positive_and_negative_values() {
        // magnitude 1, positive: "0" + s(0) + "0" = "000".
        let bytes = bits_to_bytes("000");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_table_d3(&mut r), 1);
        // magnitude 2 ("x0"+2 row, x0=0), negative: "0" x0(0) "1" s(1) "0" = "00110".
        let bytes = bits_to_bytes("00110");
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_table_d3(&mut r), -2);
    }

    #[test]
    fn extended_level_round_trips_the_figure_t1_rotation() {
        // Encode LEVEL = 200 (b11..b1 = 00011001000) by rotating right 5:
        // transmitted = b5..b1,b11..b6 = 01000 00011.. let's just build it
        // from the known-good decode direction instead: pick transmitted
        // bits directly and check the reconstructed magnitude/sign by hand.
        // t = [t0..t10], decoded bits order is [t5,t6,t7,t8,t9,t10,t0,t1,t2,t3,t4].
        // Choose t so decoded bits = 00000000101 (value 5, positive).
        // decoded[0..11] = b11..b1 = 0,0,0,0,0,0,0,0,1,0,1
        // t5=0,t6=0,t7=0,t8=0,t9=0,t10=0,t0=0,t1=0,t2=1,t3=0,t4=1
        let t = ["0", "0", "1", "0", "1", "0", "0", "0", "0", "0", "0"];
        let bytes = bits_to_bytes(&t.concat());
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_extended_level(&mut r), 5);
    }
}
