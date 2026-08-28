//! `Parameters` (RFC 9043 §4.2): the stream-wide configuration that, for
//! version >= 3, lives entirely in the out-of-band Configuration Record.
//!
//! `Vaco-Spec-Ref: rfc9043 RFC 9043 §4.2 (Parameters pseudocode, Figure 28)
//! and §4.2.1-§4.2.17 (each field's semantics)`.

use vaco_core::{Error, Result};

use crate::quant::QuantTableSet;
use crate::rangecoder::{
    CONTEXT_SIZE, RangeDecoder, RangeEncoder, StateTransition, fresh_states,
    read_state_transition_delta, write_state_transition_delta,
};

/// `colorspace_type` (RFC 9043 §4.2.5). Only the two values RFC 9043 defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorSpace {
    /// YCbCr, no pixel transformation, Plane-then-Line interleave.
    YCbCr,
    /// RGB via the JPEG 2000 Reversible Color Transform, Line-then-Plane
    /// interleave (RFC 9043 §3.7.2).
    JpegRct,
}

impl ColorSpace {
    const fn from_u32(v: u32) -> Result<Self> {
        match v {
            0 => Ok(Self::YCbCr),
            1 => Ok(Self::JpegRct),
            _ => Err(Error::Unsupported("ffv1: unknown colorspace_type")),
        }
    }

    const fn as_u32(self) -> u32 {
        match self {
            Self::YCbCr => 0,
            Self::JpegRct => 1,
        }
    }
}

/// `coder_type` (RFC 9043 §4.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoderType {
    /// Golomb-Rice (decode-only in this crate — see `rice.rs`).
    GolombRice,
    /// Range coder, default state transition table.
    RangeDefault,
    /// Range coder, custom state transition table (`state_transition_delta`
    /// present).
    RangeCustom,
}

impl CoderType {
    const fn from_u32(v: u32) -> Result<Self> {
        match v {
            0 => Ok(Self::GolombRice),
            1 => Ok(Self::RangeDefault),
            2 => Ok(Self::RangeCustom),
            _ => Err(Error::Unsupported("ffv1: unknown coder_type")),
        }
    }

    const fn as_u32(self) -> u32 {
        match self {
            Self::GolombRice => 0,
            Self::RangeDefault => 1,
            Self::RangeCustom => 2,
        }
    }
}

/// The decoded/constructed `Parameters` (RFC 9043 §4.2), plus the derived
/// [`StateTransition`] table and per-quant-table-set initial context states
/// a slice needs to start decoding from.
#[derive(Debug, Clone)]
pub(crate) struct Parameters {
    pub version: u32,
    pub micro_version: u32,
    pub coder_type: CoderType,
    pub state_transition: StateTransition,
    pub colorspace: ColorSpace,
    pub bits_per_raw_sample: u32,
    pub chroma_planes: bool,
    pub log2_h_chroma_subsample: u32,
    pub log2_v_chroma_subsample: u32,
    pub extra_plane: bool,
    pub num_h_slices: u32,
    pub num_v_slices: u32,
    pub quant_tables: Vec<QuantTableSet>,
    /// `initial_states[i]`: one `[u8; CONTEXT_SIZE]` per context of quant
    /// table set `i`, present only when `states_coded[i]` was set. Absent
    /// means "all 128" (RFC 9043 §4.2.14).
    pub initial_states: Vec<Option<Vec<[u8; CONTEXT_SIZE]>>>,
    pub ec: u32,
    pub intra: u32,
}

impl Parameters {
    /// This crate's own encoder configuration: version 3, range coder with
    /// the default table, one quantization table set, single slice, no
    /// alpha, `ec = 0` (a Configuration Record CRC only, no per-slice CRC —
    /// this crate's own round trip does not need slice-level error
    /// detection), `intra = 1` (every frame is a keyframe, matching the
    /// crate's intra-only scope).
    #[must_use]
    pub(crate) fn own_encoder(
        colorspace: ColorSpace,
        bits_per_raw_sample: u32,
        chroma_planes: bool,
        log2_h: u32,
        log2_v: u32,
    ) -> Self {
        Self {
            version: 3,
            micro_version: 4,
            coder_type: CoderType::RangeDefault,
            state_transition: StateTransition::default_table(),
            colorspace,
            bits_per_raw_sample,
            chroma_planes,
            log2_h_chroma_subsample: log2_h,
            log2_v_chroma_subsample: log2_v,
            extra_plane: false,
            num_h_slices: 1,
            num_v_slices: 1,
            quant_tables: vec![QuantTableSet::small_default()],
            initial_states: vec![None],
            ec: 0,
            intra: 1,
        }
    }

    /// `quant_table_set_index_count`, RFC 9043 §4.6.5.
    #[must_use]
    pub(crate) fn quant_table_set_index_count(&self) -> usize {
        1 + usize::from(self.chroma_planes || self.version <= 3) + usize::from(self.extra_plane)
    }

    /// Parse `Parameters()` (RFC 9043 Figure 28). Uses a fresh state array,
    /// per "`Parameters` has its own initial states, all set to 128" (§4.2).
    ///
    /// # Errors
    /// [`Error::Unsupported`] for a `version`/`coder_type`/`colorspace_type`
    /// this crate does not implement decode for, [`Error::InvalidData`] for
    /// field combinations RFC 9043 rules out.
    pub(crate) fn parse(dec: &mut RangeDecoder<'_>) -> Result<Self> {
        let mut states = fresh_states();
        let bootstrap = StateTransition::default_table();

        let version = dec.get_symbol(&mut states, &bootstrap, false) as u32;
        if version > 3 {
            return Err(Error::Unsupported("ffv1: version > 3 not implemented"));
        }
        let micro_version = if version >= 3 {
            dec.get_symbol(&mut states, &bootstrap, false) as u32
        } else {
            0
        };

        let coder_type_raw = dec.get_symbol(&mut states, &bootstrap, false) as u32;
        let coder_type = CoderType::from_u32(coder_type_raw)?;
        let state_transition = if coder_type_raw > 1 {
            let delta = read_state_transition_delta(dec, &bootstrap);
            StateTransition::with_delta(&delta)
        } else {
            StateTransition::default_table()
        };

        let colorspace =
            ColorSpace::from_u32(dec.get_symbol(&mut states, &bootstrap, false) as u32)?;
        let bits_per_raw_sample = if version >= 1 {
            let v = dec.get_symbol(&mut states, &bootstrap, false) as u32;
            if v == 0 { 8 } else { v }
        } else {
            8
        };
        let chroma_planes = get_flag(dec, &mut states, &bootstrap);
        let log2_h_chroma_subsample = dec.get_symbol(&mut states, &bootstrap, false) as u32;
        let log2_v_chroma_subsample = dec.get_symbol(&mut states, &bootstrap, false) as u32;
        let extra_plane = get_flag(dec, &mut states, &bootstrap);

        let (num_h_slices, num_v_slices, quant_table_set_count) = if version >= 3 {
            let h = dec.get_symbol(&mut states, &bootstrap, false) as u32 + 1;
            let v = dec.get_symbol(&mut states, &bootstrap, false) as u32 + 1;
            let count = dec.get_symbol(&mut states, &bootstrap, false) as u32;
            (h, v, count)
        } else {
            (1, 1, 1)
        };
        if quant_table_set_count == 0 || quant_table_set_count > 8 {
            return Err(Error::InvalidData(
                "ffv1: quant_table_set_count out of range",
            ));
        }

        let mut quant_tables = Vec::new();
        for _ in 0..quant_table_set_count {
            quant_tables.push(QuantTableSet::parse(dec, &state_transition)?);
        }

        let mut initial_states: Vec<Option<Vec<[u8; CONTEXT_SIZE]>>> =
            vec![None; quant_table_set_count as usize];
        if version >= 3 {
            for (i, qts) in quant_tables.iter().enumerate() {
                let coded = get_flag(dec, &mut states, &bootstrap);
                if coded {
                    let mut per_context = Vec::new();
                    let mut prev = [128u8; CONTEXT_SIZE];
                    for _ in 0..qts.context_count {
                        let mut cur = [128u8; CONTEXT_SIZE];
                        for (k, slot) in cur.iter_mut().enumerate() {
                            let delta = dec.get_symbol(&mut states, &bootstrap, true);
                            let pred = i32::from(prev.get(k).copied().unwrap_or(128));
                            *slot = ((pred + delta).rem_euclid(256)) as u8;
                        }
                        prev = cur;
                        per_context.push(cur);
                    }
                    if let Some(slot) = initial_states.get_mut(i) {
                        *slot = Some(per_context);
                    }
                }
            }
        }

        let (ec, intra) = if version >= 3 {
            let ec = dec.get_symbol(&mut states, &bootstrap, false) as u32;
            let intra = dec.get_symbol(&mut states, &bootstrap, false) as u32;
            (ec, intra)
        } else {
            (0, 0)
        };

        Ok(Self {
            version,
            micro_version,
            coder_type,
            state_transition,
            colorspace,
            bits_per_raw_sample,
            chroma_planes,
            log2_h_chroma_subsample,
            log2_v_chroma_subsample,
            extra_plane,
            num_h_slices,
            num_v_slices,
            quant_tables,
            initial_states,
            ec,
            intra,
        })
    }

    /// Write `Parameters()`. Only ever called with a value produced by
    /// [`Parameters::own_encoder`] (version 3, `coder_type = RangeDefault`,
    /// no custom initial states), so several branches (`state_transition_delta`,
    /// `states_coded`) are always the "not present" case — still implemented
    /// generically rather than special-cased, so a future caller passing a
    /// richer `Parameters` is not silently mishandled.
    ///
    /// # Errors
    /// Never fails for a `Parameters` this crate's own encoder builds;
    /// `Result` kept for symmetry with [`Parameters::parse`] and because
    /// [`QuantTableSet::write`]/[`write_state_transition_delta`] return one.
    pub(crate) fn write(&self, enc: &mut RangeEncoder) -> Result<()> {
        let mut states = fresh_states();
        let bootstrap = StateTransition::default_table();

        enc.put_symbol(&mut states, &bootstrap, self.version.cast_signed(), false);
        if self.version >= 3 {
            enc.put_symbol(
                &mut states,
                &bootstrap,
                self.micro_version.cast_signed(),
                false,
            );
        }
        enc.put_symbol(
            &mut states,
            &bootstrap,
            self.coder_type.as_u32().cast_signed(),
            false,
        );
        if self.coder_type == CoderType::RangeCustom {
            // This crate's own encoder never picks RangeCustom, but keep the
            // write side complete: all-zero delta (the table used is the
            // default one either way).
            write_state_transition_delta(enc, &bootstrap, &[0i32; 255]);
        }
        enc.put_symbol(
            &mut states,
            &bootstrap,
            self.colorspace.as_u32().cast_signed(),
            false,
        );
        if self.version >= 1 {
            enc.put_symbol(
                &mut states,
                &bootstrap,
                self.bits_per_raw_sample.cast_signed(),
                false,
            );
        }
        put_flag(enc, &mut states, &bootstrap, self.chroma_planes);
        enc.put_symbol(
            &mut states,
            &bootstrap,
            self.log2_h_chroma_subsample.cast_signed(),
            false,
        );
        enc.put_symbol(
            &mut states,
            &bootstrap,
            self.log2_v_chroma_subsample.cast_signed(),
            false,
        );
        put_flag(enc, &mut states, &bootstrap, self.extra_plane);

        if self.version >= 3 {
            enc.put_symbol(
                &mut states,
                &bootstrap,
                self.num_h_slices.cast_signed() - 1,
                false,
            );
            enc.put_symbol(
                &mut states,
                &bootstrap,
                self.num_v_slices.cast_signed() - 1,
                false,
            );
            enc.put_symbol(
                &mut states,
                &bootstrap,
                i32::try_from(self.quant_tables.len()).unwrap_or(i32::MAX),
                false,
            );
        }
        for qts in &self.quant_tables {
            qts.write(enc, &self.state_transition)?;
        }
        if self.version >= 3 {
            for maybe_states in &self.initial_states {
                put_flag(enc, &mut states, &bootstrap, maybe_states.is_some());
                if let Some(per_context) = maybe_states {
                    let mut prev = [128u8; CONTEXT_SIZE];
                    for cur in per_context {
                        for (k, &v) in cur.iter().enumerate() {
                            let pred = i32::from(prev.get(k).copied().unwrap_or(128));
                            let delta = i32::from(v) - pred;
                            enc.put_symbol(&mut states, &bootstrap, delta, true);
                        }
                        prev = *cur;
                    }
                }
            }
            enc.put_symbol(&mut states, &bootstrap, self.ec.cast_signed(), false);
            enc.put_symbol(&mut states, &bootstrap, self.intra.cast_signed(), false);
        }
        Ok(())
    }
}

fn get_flag(
    dec: &mut RangeDecoder<'_>,
    states: &mut crate::rangecoder::SymbolStates,
    table: &StateTransition,
) -> bool {
    // `br` fields use a single binary symbol at state offset 0, same as
    // get_symbol's own "is this zero" bit — a plain get_rac against the
    // first state slot models a boolean field directly.
    let mut s0 = states.first().copied().unwrap_or(128);
    let bit = dec.get_rac(&mut s0, table);
    if let Some(slot) = states.first_mut() {
        *slot = s0;
    }
    bit
}

fn put_flag(
    enc: &mut RangeEncoder,
    states: &mut crate::rangecoder::SymbolStates,
    table: &StateTransition,
    value: bool,
) {
    let mut s0 = states.first().copied().unwrap_or(128);
    enc.put_rac(&mut s0, table, value);
    if let Some(slot) = states.first_mut() {
        *slot = s0;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code exercising the module, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;

    #[test]
    fn own_encoder_parameters_round_trip() {
        let params = Parameters::own_encoder(ColorSpace::YCbCr, 8, true, 1, 1);
        let mut enc = RangeEncoder::new();
        params.write(&mut enc).expect("write");
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let parsed = Parameters::parse(&mut dec).expect("parse");
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.coder_type, CoderType::RangeDefault);
        assert_eq!(parsed.colorspace, ColorSpace::YCbCr);
        assert_eq!(parsed.bits_per_raw_sample, 8);
        assert!(parsed.chroma_planes);
        assert_eq!(parsed.log2_h_chroma_subsample, 1);
        assert_eq!(parsed.log2_v_chroma_subsample, 1);
        assert!(!parsed.extra_plane);
        assert_eq!(parsed.num_h_slices, 1);
        assert_eq!(parsed.num_v_slices, 1);
        assert_eq!(parsed.quant_tables.len(), 1);
        assert_eq!(parsed.intra, 1);
    }

    #[test]
    fn gbr_colorspace_round_trips() {
        let params = Parameters::own_encoder(ColorSpace::JpegRct, 8, true, 0, 0);
        let mut enc = RangeEncoder::new();
        params.write(&mut enc).expect("write");
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        let parsed = Parameters::parse(&mut dec).expect("parse");
        assert_eq!(parsed.colorspace, ColorSpace::JpegRct);
        assert_eq!(parsed.log2_h_chroma_subsample, 0);
        assert_eq!(parsed.log2_v_chroma_subsample, 0);
    }
}
