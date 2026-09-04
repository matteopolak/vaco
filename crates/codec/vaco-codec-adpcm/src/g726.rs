//! ITU-T G.726 32 kbit/s ADPCM decoding for a uniform PCM interface.
//!
//! The predictor and adaptation state below transcribe the fixed-width blocks
//! in ITU-T G.726 (12/1990), clauses 3 and 4. Output follows Annex A
//! (11/1994), clauses A.2-A.3: the G.711 conversion and synchronous coding
//! adjustment are omitted, and the reconstructed signal passes through
//! `LIMO`. The `SR = 57344` boundary uses Corrigendum 1 (05/2005).
//!
//! Only the four-bit, 32 kbit/s mode is implemented. The public codec IDs do
//! not carry a bitrate parameter, so accepting another rate under the same
//! name would be ambiguous.

#![allow(
    clippy::many_single_char_names,
    reason = "G.726 clause 4 defines these fixed-width signal names"
)]

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Delayed variables in Table 6/G.726.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecoderState {
    yl: u32,
    yu: u32,
    dms: u32,
    dml: u32,
    ap: u32,
    a: [u32; 2],
    b: [u32; 6],
    pk: [u32; 2],
    dq: [u32; 6],
    sr: [u32; 2],
    td: u32,
}

impl Default for DecoderState {
    fn default() -> Self {
        Self {
            yl: 34_816,
            yu: 544,
            dms: 0,
            dml: 0,
            ap: 0,
            a: [0; 2],
            b: [0; 6],
            pk: [0; 2],
            dq: [32; 6],
            sr: [32; 2],
            td: 0,
        }
    }
}

const fn magnitude_index(code: u32) -> u32 {
    if code & 8 == 0 {
        code & 7
    } else {
        (15 - code) & 7
    }
}

/// Table 12/G.726 (`RECONST`) for 32 kbit/s operation.
const fn reconst_32(code: u32) -> (u32, u32) {
    let dqln = match code & 15 {
        0 | 15 => 2048,
        1 | 14 => 4,
        2 | 13 => 135,
        3 | 12 => 213,
        4 | 11 => 273,
        5 | 10 => 323,
        6 | 9 => 373,
        7 | 8 => 425,
        _ => 2048,
    };
    (dqln, (code >> 3) & 1)
}

/// `FUNCTW` for 32 kbit/s operation.
const fn functw_32(code: u32) -> u32 {
    match magnitude_index(code) {
        0 => 4084,
        1 => 18,
        2 => 41,
        3 => 64,
        4 => 112,
        5 => 198,
        6 => 355,
        7 => 1122,
        _ => 4084,
    }
}

/// `FUNCTF` for 32 kbit/s operation.
const fn functf_32(code: u32) -> u32 {
    match magnitude_index(code) {
        0..=2 => 0,
        3..=5 => 1,
        6 => 3,
        7 => 7,
        _ => 0,
    }
}

/// `ADDA`: add the current scale factor in the logarithmic domain.
const fn adda(dqln: u32, y: u32) -> u32 {
    (dqln + (y >> 2)) & 4095
}

/// `ANTILOG`: convert a logarithmic difference to 15-bit signed magnitude.
const fn antilog(dql: u32, dqs: u32) -> u32 {
    let ds = dql >> 11;
    let dex = (dql >> 7) & 15;
    let dmn = dql & 127;
    let dqt = 128 + dmn;
    let dqmag = if ds == 0 {
        if dex <= 14 {
            (dqt << 7) >> (14 - dex)
        } else {
            (dqt << 7) << (dex - 14)
        }
    } else {
        0
    };
    ((dqs << 14) + dqmag) & 32_767
}

const fn signed_magnitude_to_tc(dq: u32) -> u32 {
    if dq >> 14 == 0 {
        dq
    } else {
        (65_536 - (dq & 16_383)) & 65_535
    }
}

const fn sign_extend_15(value: u32) -> u32 {
    if value >> 14 == 0 {
        value
    } else {
        32_768 + value
    }
}

/// `ADDB`: add the quantized difference and signal estimate.
const fn addb(dq: u32, se: u32) -> u32 {
    (signed_magnitude_to_tc(dq) + sign_extend_15(se)) & 65_535
}

/// `ADDC`: obtain the sign of `DQ + SEZ` and its zero flag.
const fn addc(dq: u32, sez: u32) -> (u32, u32) {
    let dqsez = (signed_magnitude_to_tc(dq) + sign_extend_15(sez)) & 65_535;
    (dqsez >> 15, if dqsez == 0 { 1 } else { 0 })
}

const fn bit_length(value: u32) -> u32 {
    u32::BITS - value.leading_zeros()
}

/// `FLOATA`: convert 15-bit signed magnitude to the 11-bit predictor float.
const fn floata(dq: u32) -> u32 {
    let sign = dq >> 14;
    let mag = dq & 16_383;
    let exp = bit_length(mag);
    let mant = if mag == 0 { 32 } else { (mag << 6) >> exp };
    (sign << 10) + (exp << 6) + mant
}

/// `FLOATB`: convert 16-bit two's complement to the 11-bit predictor float.
const fn floatb(sr: u32) -> u32 {
    let sign = sr >> 15;
    let mag = if sign == 0 {
        sr
    } else {
        (65_536 - sr) & 32_767
    };
    let exp = bit_length(mag);
    let mant = if mag == 0 { 32 } else { (mag << 6) >> exp };
    (sign << 10) + (exp << 6) + mant
}

/// `FMULT`: multiply a 16-bit predictor coefficient by an 11-bit float.
const fn fmult(coefficient: u32, delayed: u32) -> u32 {
    let coefficient_sign = coefficient >> 15;
    let coefficient_mag = if coefficient_sign == 0 {
        coefficient >> 2
    } else {
        (16_384 - (coefficient >> 2)) & 8191
    };
    let coefficient_exp = bit_length(coefficient_mag);
    let coefficient_mant = if coefficient_mag == 0 {
        32
    } else {
        (coefficient_mag << 6) >> coefficient_exp
    };

    let delayed_sign = delayed >> 10;
    let delayed_exp = (delayed >> 6) & 15;
    let delayed_mant = delayed & 63;
    let product_sign = delayed_sign ^ coefficient_sign;
    let product_exp = delayed_exp + coefficient_exp;
    let product_mant = ((delayed_mant * coefficient_mant) + 48) >> 4;
    let product_mag = if product_exp <= 26 {
        (product_mant << 7) >> (26 - product_exp)
    } else {
        ((product_mant << 7) << (product_exp - 26)) & 32_767
    };
    if product_sign == 0 {
        product_mag
    } else {
        (65_536 - product_mag) & 65_535
    }
}

/// `ACCUM`: form the sixth-order partial and full signal estimates.
fn accum(a: [u32; 2], b: [u32; 6], sr: [u32; 2], dq: [u32; 6]) -> (u32, u32) {
    let [a1, a2] = a;
    let [b1, b2, b3, b4, b5, b6] = b;
    let [sr1, sr2] = sr;
    let [dq1, dq2, dq3, dq4, dq5, dq6] = dq;
    let wb = [
        fmult(b1, dq1),
        fmult(b2, dq2),
        fmult(b3, dq3),
        fmult(b4, dq4),
        fmult(b5, dq5),
        fmult(b6, dq6),
    ];
    let sezi = wb.into_iter().fold(0, |sum, value| (sum + value) & 65_535);
    let sei = (sezi + fmult(a2, sr2) + fmult(a1, sr1)) & 65_535;
    (sei >> 1, sezi >> 1)
}

/// `UPA1`: update the first pole coefficient.
const fn upa1(pk0: u32, pk1: u32, a1: u32, sigpk: u32) -> u32 {
    let pks = pk0 ^ pk1;
    let uga1 = if sigpk == 1 {
        0
    } else if pks == 0 {
        192
    } else {
        65_344
    };
    let ula1 = if a1 >> 15 == 0 {
        (65_536 - (a1 >> 8)) & 65_535
    } else {
        (65_536 - ((a1 >> 8) + 65_280)) & 65_535
    };
    (a1 + ((uga1 + ula1) & 65_535)) & 65_535
}

/// `UPA2`: update the second pole coefficient.
const fn upa2(pk0: u32, pk1: u32, pk2: u32, a1: u32, a2: u32, sigpk: u32) -> u32 {
    let pks1 = pk0 ^ pk1;
    let pks2 = pk0 ^ pk2;
    let uga2a = if pks2 == 0 { 16_384 } else { 114_688 };
    let fa1 = if a1 >> 15 == 0 {
        if a1 <= 8191 { a1 << 2 } else { 8191 << 2 }
    } else if a1 >= 57_345 {
        (a1 << 2) & 131_071
    } else {
        24_577 << 2
    };
    let fa = if pks1 == 1 {
        fa1
    } else {
        (131_072 - fa1) & 131_071
    };
    let uga2b = (uga2a + fa) & 131_071;
    let uga2 = if sigpk == 1 {
        0
    } else if uga2b >> 16 == 0 {
        uga2b >> 7
    } else {
        (uga2b >> 7) + 64_512
    };
    let ula2 = if a2 >> 15 == 0 {
        (65_536 - (a2 >> 7)) & 65_535
    } else {
        (65_536 - ((a2 >> 7) + 65_024)) & 65_535
    };
    (a2 + ((uga2 + ula2) & 65_535)) & 65_535
}

/// `UPB`: update one zero coefficient.
const fn upb(sign_xor: u32, coefficient: u32, dq: u32) -> u32 {
    let dqmag = dq & 16_383;
    let gain = if dqmag == 0 {
        0
    } else if sign_xor == 0 {
        128
    } else {
        65_408
    };
    let leak = if coefficient >> 15 == 0 {
        (65_536 - (coefficient >> 8)) & 65_535
    } else {
        (65_536 - ((coefficient >> 8) + 65_280)) & 65_535
    };
    (coefficient + ((gain + leak) & 65_535)) & 65_535
}

/// `LIMC`: constrain the second pole coefficient to +/-0.75.
const fn limc(a2t: u32) -> u32 {
    if a2t >= 32_768 && a2t <= 53_248 {
        53_248
    } else if a2t >= 12_288 && a2t <= 32_767 {
        12_288
    } else {
        a2t
    }
}

/// `LIMD`: constrain the first pole coefficient using the second coefficient.
const fn limd(a1t: u32, a2p: u32) -> u32 {
    let upper = (15_360 + 65_536 - a2p) & 65_535;
    let lower = (a2p + 65_536 - 15_360) & 65_535;
    if a1t >= 32_768 && a1t <= lower {
        lower
    } else if a1t >= upper && a1t <= 32_767 {
        upper
    } else {
        a1t
    }
}

/// `FILTD`: update the fast quantizer scale factor.
const fn filtd(wi: u32, y: u32) -> u32 {
    let difference = ((wi << 5) + 131_072 - y) & 131_071;
    let signed = if difference >> 16 == 0 {
        difference >> 5
    } else {
        (difference >> 5) + 4096
    };
    (y + signed) & 8191
}

/// `FILTE`: update the slow quantizer scale factor.
const fn filte(yup: u32, yl: u32) -> u32 {
    let difference = (yup + ((1_048_576 - yl) >> 6)) & 16_383;
    let signed = if difference >> 13 == 0 {
        difference
    } else {
        difference + 507_904
    };
    (yl + signed) & 524_287
}

/// `LIMB`: constrain the fast scale factor.
const fn limb(yut: u32) -> u32 {
    let above_lower = ((yut + 15_840) & 16_383) >> 13;
    let below_upper = ((yut + 11_264) & 16_383) >> 13;
    if above_lower == 1 {
        544
    } else if below_upper == 0 {
        5120
    } else {
        yut
    }
}

/// `MIX`: combine fast and slow scale factors.
const fn mix(al: u32, yu: u32, yl: u32) -> u32 {
    let difference = (yu + 16_384 - (yl >> 6)) & 16_383;
    let sign = difference >> 13;
    let magnitude = if sign == 0 {
        difference
    } else {
        (16_384 - difference) & 8191
    };
    let product_magnitude = (magnitude * al) >> 6;
    let product = if sign == 0 {
        product_magnitude
    } else {
        (16_384 - product_magnitude) & 16_383
    };
    ((yl >> 6) + product) & 8191
}

/// `FILTA`: update the short-term average of `F(I)`.
const fn filta(fi: u32, dms: u32) -> u32 {
    let difference = ((fi << 9) + 8192 - dms) & 8191;
    let signed = if difference >> 12 == 0 {
        difference >> 5
    } else {
        (difference >> 5) + 3840
    };
    (signed + dms) & 4095
}

/// `FILTB`: update the long-term average of `F(I)`.
const fn filtb(fi: u32, dml: u32) -> u32 {
    let difference = ((fi << 11) + 32_768 - dml) & 32_767;
    let signed = if difference >> 14 == 0 {
        difference >> 7
    } else {
        (difference >> 7) + 16_128
    };
    (signed + dml) & 16_383
}

/// `FILTC`: low-pass filter the adaptation-speed control.
const fn filtc(ax: u32, ap: u32) -> u32 {
    let difference = ((ax << 9) + 2048 - ap) & 2047;
    let signed = if difference >> 10 == 0 {
        difference >> 4
    } else {
        (difference >> 4) + 896
    };
    (signed + ap) & 1023
}

/// `LIMA`: constrain the speed-control parameter.
const fn lima(ap: u32) -> u32 {
    if ap >= 256 { 64 } else { ap >> 2 }
}

/// `SUBTC`: choose fast or slow adaptation.
const fn subtc(dmsp: u32, dmlp: u32, tdp: u32, y: u32) -> u32 {
    let difference = ((dmsp << 2) + 32_768 - dmlp) & 32_767;
    let magnitude = if difference >> 14 == 0 {
        difference
    } else {
        (32_768 - difference) & 16_383
    };
    let threshold = dmlp >> 3;
    if y >= 1536 && magnitude < threshold && tdp == 0 {
        0
    } else {
        1
    }
}

/// `TRIGA`: force fast adaptation after a transition.
const fn triga(tr: u32, app: u32) -> u32 {
    if tr == 0 { app } else { 256 }
}

/// `TRIGB`: reset predictor/tone state after a transition.
const fn trigb(tr: u32, value: u32) -> u32 {
    if tr == 0 { value } else { 0 }
}

/// `TONE`: detect a partial-band signal from the second pole coefficient.
const fn tone(a2p: u32) -> u32 {
    if a2p >= 32_768 && a2p < 53_760 { 1 } else { 0 }
}

/// `TRANS`: detect a signal transition.
const fn trans(td: u32, yl: u32, dq: u32) -> u32 {
    let magnitude = dq & 16_383;
    let exponent = yl >> 15;
    let fraction = (yl >> 10) & 31;
    let threshold_1 = (32 + fraction) << exponent;
    let threshold_2 = if exponent > 8 { 31 << 9 } else { threshold_1 };
    let dq_threshold = (threshold_2 + (threshold_2 >> 1)) >> 1;
    if magnitude > dq_threshold && td == 1 {
        1
    } else {
        0
    }
}

/// Corrected Annex-A `LIMO`, returned as a signed 14-bit value.
fn limo(sr: u32) -> i16 {
    let limited = if sr > 8191 && sr < 32_768 {
        8191
    } else if !(8192..=57_343).contains(&sr) {
        sr & 16_383
    } else {
        8192
    };
    let limited = i16::try_from(limited).unwrap_or(0);
    if limited & 8192 == 0 {
        limited
    } else {
        limited - 16_384
    }
}

impl DecoderState {
    /// Decode one four-bit 32 kbit/s word and update all delays simultaneously.
    #[must_use]
    pub(crate) fn decode_code(&mut self, code: u8) -> i16 {
        let code = u32::from(code & 15);
        let [a1, a2] = self.a;
        let [b1, b2, b3, b4, b5, b6] = self.b;
        let [pk1, pk2] = self.pk;
        let [dq1, dq2, dq3, dq4, dq5, dq6] = self.dq;
        let [sr1, _sr2] = self.sr;

        let al = lima(self.ap);
        let y = mix(al, self.yu, self.yl);
        let (dqln, dqs) = reconst_32(code);
        let dq = antilog(adda(dqln, y), dqs);
        let (se, sez) = accum(self.a, self.b, self.sr, self.dq);
        let sr = addb(dq, se);
        let (pk0, sigpk) = addc(dq, sez);
        let tr = trans(self.td, self.yl, dq);

        let a2p = limc(upa2(pk0, pk1, pk2, a1, a2, sigpk));
        let a1p = limd(upa1(pk0, pk1, a1, sigpk), a2p);
        let dq_sign = dq >> 14;
        let bp = [
            upb(dq_sign ^ (dq1 >> 10), b1, dq),
            upb(dq_sign ^ (dq2 >> 10), b2, dq),
            upb(dq_sign ^ (dq3 >> 10), b3, dq),
            upb(dq_sign ^ (dq4 >> 10), b4, dq),
            upb(dq_sign ^ (dq5 >> 10), b5, dq),
            upb(dq_sign ^ (dq6 >> 10), b6, dq),
        ];

        let yup = limb(filtd(functw_32(code), y));
        let ylp = filte(yup, self.yl);
        let fi = functf_32(code);
        let dmsp = filta(fi, self.dms);
        let dmlp = filtb(fi, self.dml);
        let tdp = tone(a2p);
        let ax = subtc(dmsp, dmlp, tdp, y);
        let app = filtc(ax, self.ap);

        self.a = [trigb(tr, a1p), trigb(tr, a2p)];
        self.b = bp.map(|value| trigb(tr, value));
        self.pk = [pk0, pk1];
        self.dq = [floata(dq), dq1, dq2, dq3, dq4, dq5];
        self.sr = [floatb(sr), sr1];
        self.yu = yup;
        self.yl = ylp;
        self.dms = dmsp;
        self.dml = dmlp;
        self.ap = triga(tr, app);
        self.td = trigb(tr, tdp);

        limo(sr) << 2
    }
}

const fn codes_from_byte(byte: u8, low_nibble_first: bool) -> [u8; 2] {
    if low_nibble_first {
        [byte & 15, byte >> 4]
    } else {
        [byte >> 4, byte & 15]
    }
}

/// Decode a packet while retaining `state` for the next packet.
pub(crate) fn decode(
    budget: &mut Budget,
    state: &mut DecoderState,
    data: &[u8],
    low_nibble_first: bool,
) -> Result<Vec<i16>> {
    let sample_count = data
        .len()
        .checked_mul(2)
        .ok_or(Error::InvalidData("g726: sample count overflow"))?;
    let mut output = budget.alloc::<i16>(sample_count)?;
    for (samples, &byte) in output.chunks_exact_mut(2).zip(data) {
        let [first, second] = samples else {
            return Err(Error::InvalidData("g726: internal sample shape"));
        };
        let [first_code, second_code] = codes_from_byte(byte, low_nibble_first);
        *first = state.decode_code(first_code);
        *second = state.decode_code(second_code);
    }
    Ok(output)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "tests use trusted fixed-size data"
)]
mod tests {
    use super::*;

    #[test]
    fn both_nibble_orders_are_explicit() {
        assert_eq!(codes_from_byte(0xab, false), [0x0a, 0x0b]);
        assert_eq!(codes_from_byte(0xab, true), [0x0b, 0x0a]);
    }

    #[test]
    fn corrected_limo_includes_57344_in_the_negative_range() {
        assert_eq!(limo(57_343), -8192);
        assert_eq!(limo(57_344), -8192);
        assert_eq!(limo(57_345), -8191);
    }

    #[test]
    fn reset_state_matches_table_six() {
        let state = DecoderState::default();
        assert_eq!(state.yl, 34_816);
        assert_eq!(state.yu, 544);
        assert_eq!(state.dq, [32; 6]);
        assert_eq!(state.sr, [32; 2]);
    }

    #[test]
    fn packet_split_preserves_decoder_state() {
        let payload = [0x12, 0x34, 0x56, 0x78];
        let mut whole_budget = Budget::new(vaco_limits::Limits::permissive());
        let whole = decode(
            &mut whole_budget,
            &mut DecoderState::default(),
            &payload,
            false,
        )
        .unwrap();
        let mut split_state = DecoderState::default();
        let mut first_budget = Budget::new(vaco_limits::Limits::permissive());
        let mut second_budget = Budget::new(vaco_limits::Limits::permissive());
        let mut split = decode(&mut first_budget, &mut split_state, &payload[..2], false).unwrap();
        split.extend(decode(&mut second_budget, &mut split_state, &payload[2..], false).unwrap());
        assert_eq!(split, whole);
        assert_eq!(whole.len(), payload.len() * 2);
    }
}
