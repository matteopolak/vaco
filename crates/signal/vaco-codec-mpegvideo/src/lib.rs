//! Shared decode-side machinery for the MPEG-heritage macroblock family
//! (D-22, epic #25): H.261, H.263, MPEG-1/2, MPEG-4 Part 2, MSMPEG4,
//! WMV1/2, FLV1, RV10/20.
//!
//! # What "shared" means here
//!
//! These ten codecs share a macroblock-layer *shape* — a per-macroblock type
//! codeword that gates what else gets read, half-pel-precision motion
//! compensation, an 8x8 residual pipeline of scan/dequantise/IDCT, and (for
//! every member with B-pictures) the same decode-order-to-display-order
//! fix-up — while differing in header syntax, exact VLC tables, and
//! quantisation constants. D-22's own brief is to factor the first kind of
//! thing out without forcing the second kind into a single API, and this
//! crate is built to that rule module by module:
//!
//! | Module | What it is | Confidence |
//! |---|---|---|
//! | [`refpic`] | B-picture display reordering | Extracted from `vaco-codec-mpeg12`'s working, `ffmpeg`-checked decoder |
//! | [`sequential_mv`] | MPEG-1/2's own motion-vector predictor (carry-forward, not spatial median) | Same |
//! | [`motioncomp`] | Half-pel motion compensation (bilinear averaging, frame/field addressing) | Same, plus cross-checked against `vaco-codec-dsp-mc`'s independently-authored bilinear tap set |
//! | [`mbtype`] | The macroblock-type flags vocabulary + a thin VLC wrapper — no table data | Structural; each family supplies its own table |
//! | [`coeff`] | Inverse scan, MPEG-1/2 dequantisation + mismatch control, IDCT hand-off, and a *generic* flat-step dequantisation shape for families whose formula has not been measured here yet | MPEG parts extracted from working code; the generic shape is deliberately unparametrised for H.263 (see that module's docs) |
//! | [`resync`] | Bit-level (not necessarily byte-aligned) marker search, for GOB/slice resynchronisation | New, structural |
//!
//! # What is deliberately not here
//!
//! - **MPEG-4 Part 2 is out of scope for this whole crate**, per this
//!   package's own brief: a previous agent stopped rather than write
//!   coefficient tables it could not honestly source, and that stands. This
//!   crate is built so MPEG-4 Part 2 *could* sit on it later — the pieces
//!   above do not assume MPEG-1/2-only syntax — but no MPEG-4-specific table
//!   or formula is included.
//! - **H.263's own dequantisation constants, and every family's own
//!   spatial (median-of-neighbours) motion vector predictor.** Both are
//!   genuinely different mechanisms from MPEG-1/2's (see [`coeff`] and
//!   [`sequential_mv`]'s own docs), and neither has been measured against a
//!   real decoder in this codebase yet — shipping a recalled numeric
//!   formula unverified is the mistake `planning/AGENT-CONSTRAINTS.md`'s
//!   "how confident should a transcribed table be" section warns against.
//!   Whoever implements H.263/MPEG-4 for real should add these here (or in
//!   their own crate, and move them up once proven) rather than this crate
//!   guessing ahead of a real consumer.
//! - **Quarter-pel MC, OBMC, GMC, and 4MV** (MPEG-4 Part 2's own
//!   extensions), and **VC-1/WMV3-specific transforms** (D-22's `T2-10` row
//!   depends on this crate but adds its own DSP dependencies, per the
//!   roadmap). Out of scope for the reasons above.
//! - A single unifying trait every family must implement. [`mbtype`]'s own
//!   docs explain why a concrete flags struct plus family-supplied table
//!   data was chosen over a trait object or a generic macroblock-loop
//!   parameter: the acceptance criterion this satisfies (two independent
//!   table shapes compiling against the same shared functions without
//!   forcing identical structure) is demonstrated by this crate's own
//!   `two_families_use_the_shared_pipeline_differently` test, which
//!   exercises a small MPEG-style table (with a `quant` bit) and a small
//!   H.263-style table (without one) through the same
//!   [`mbtype::decode_mb_type`] call.
//!
//! # Not yet done
//!
//! `vaco-codec-mpeg12` has **not** been refactored onto this crate. Its own
//! brief allows that only if its existing tests keep passing unchanged, and
//! `vaco-codec-mpeg12` is a different agent's active crate this session —
//! touching it here would violate single-writer ownership
//! (`planning/AGENT-CONSTRAINTS.md`'s scope rule). The extraction was done
//! by generalising *from* a read-only copy of that crate's logic, not by
//! editing it; `vaco-codec-mpeg12` should switch to depending on this crate
//! module by module once its own owner has room to verify each swap keeps
//! its differential fixtures passing.
//!
//! Full slice/GOB error-resilience validation across all ten families
//! (D-22d's own acceptance criterion) has not been attempted: only
//! `vaco-codec-mpeg12` (not touched here) and possibly `vaco-codec-h263`
//! (a different agent's crate) exist among the ten today, so "each of the
//! ten families instantiates the core and decodes a smoke stream" has no
//! second real family to test against yet from inside this crate.
#![forbid(unsafe_code)]

pub mod coeff;
pub mod mbtype;
pub mod motioncomp;
pub mod refpic;
pub mod resync;
pub mod sequential_mv;

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test code")]

    use crate::mbtype::{MbTypeEntry, MbTypeFlags, decode_mb_type};
    use vaco_bitstream::{BitReader, BitWriter};

    /// D-22a's own acceptance criterion, adapted to this crate's
    /// table-driven (not trait-driven) design: two structurally different
    /// macroblock-type tables — one carrying MPEG-1/2's `quant` bit, one
    /// shaped like H.263's simpler vocabulary with no such bit — both
    /// compile and decode correctly against the exact same
    /// [`crate::mbtype::decode_mb_type`] function, with no family-specific
    /// code above the seam.
    #[test]
    fn two_families_use_the_shared_pipeline_differently() {
        // "MPEG-style": a P-picture row that can carry a quantiser change.
        let mpeg_style = vec![
            MbTypeEntry::new(0b1, 1, MbTypeFlags { motion_forward: true, ..MbTypeFlags::default() }),
            MbTypeEntry::new(
                0b01,
                2,
                MbTypeFlags {
                    motion_forward: true,
                    quant: true,
                    ..MbTypeFlags::default()
                },
            ),
        ];
        // "H.263-style": no `quant` bit ever set anywhere in this table —
        // a different family's table simply never sets a flag it has no
        // syntax element for, rather than this crate forcing every family
        // to populate every field meaningfully.
        let h263_style = vec![
            MbTypeEntry::new(0b0, 1, MbTypeFlags { intra: true, ..MbTypeFlags::default() }),
            MbTypeEntry::new(
                0b1,
                1,
                MbTypeFlags { motion_forward: true, pattern: true, ..MbTypeFlags::default() },
            ),
        ];

        let mut w = BitWriter::new();
        w.put(2, 0b01);
        w.align_zero();
        let mpeg_bytes = w.finish();
        let mut r = BitReader::new(&mpeg_bytes);
        let flags = decode_mb_type(&mut r, &mpeg_style);
        assert_eq!(
            flags,
            Some(MbTypeFlags { motion_forward: true, quant: true, ..MbTypeFlags::default() })
        );

        let mut w2 = BitWriter::new();
        w2.put(1, 0b0);
        w2.align_zero();
        let h263_bytes = w2.finish();
        let mut r2 = BitReader::new(&h263_bytes);
        let flags2 = decode_mb_type(&mut r2, &h263_style);
        assert_eq!(flags2, Some(MbTypeFlags { intra: true, ..MbTypeFlags::default() }));
    }
}
