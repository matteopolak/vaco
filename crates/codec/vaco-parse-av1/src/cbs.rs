//! AV1's [`CbsCodec`] implementation — and the answer to the question this
//! crate was specifically asked to settle: **does the read/modify/write layer
//! `vaco-codec-cbs` proved against H.264 and HEVC serve a non-NAL codec?**
//!
//! # The short answer
//!
//! **Mostly yes, and where it does not, the reason is not "OBUs nest inside
//! temporal units."** [`CbsFragment`] is a flat list, and the module docs on
//! `vaco-codec-cbs` already anticipated exactly this shape of question by
//! making [`CbsCodec::Av1Framing`] an associated type — "the codec brings its
//! own" framing, and everything past the split is generic. That design
//! choice, made before this crate existed, is what makes the fit work. AV1
//! genuinely has two framings, and they get two different verdicts:
//!
//! 1. **[`Av1Framing::ObuStream`] — the framing every real encoder in this
//!    project's test environment produces** (MP4/Matroska sample data,
//!    `av1C` `configOBUs`, and measured `ffmpeg -f obu` output): a flat
//!    concatenation of self-sized OBUs with no wrapper at all. `split` and
//!    `assemble` for it are as simple as HEVC's Annex B case — arguably
//!    simpler, since there is no start-code escaping to get right. **This
//!    framing fits the flat-list model perfectly, at zero cost.** The
//!    "nesting" the brief's hypothesis raises does not exist here: a temporal
//!    delimiter is just another OBU with a recognisable type, and a
//!    `filter_units`-style caller can find frame boundaries by scanning
//!    `unit_type` values exactly the way it already does for HEVC's AUD.
//!
//! 2. **[`Av1Framing::LowOverheadBitstream`] — Annex B's actual nested
//!    `temporal_unit_size`/`frame_unit_size`/`obu_length` wrapper.** Here the
//!    hypothesis is right, but not for the "nesting" reason given: the
//!    problem is not that OBUs nest inside temporal units (H.264 access
//!    units nest NAL units too, and Annex B copes fine by not encoding that
//!    nesting explicitly at all). The problem is that Annex B's
//!    `frame_unit_size` boundary is **encoder-chosen framing data with no
//!    required correspondence to OBU content** — nothing says a frame unit
//!    must correspond to one decoded frame, so two encoders may wrap the
//!    identical OBU sequence into a different number of `frame_unit`
//!    groups, and [`CbsUnit`] has nowhere to record which grouping was used.
//!    [`Av1Cbs::assemble`] below produces *a* conformant wrapper — one
//!    `frame_unit` per temporal unit — that decodes identically, but it is
//!    demonstrably not always the *same bytes* the source used. See
//!    [`FRAME_UNIT_GRANULARITY_DIVERGENCE`] and the test that pins it.
//!
//! So the honest verdict is not "it fits" or "it does not fit" — it is **"it
//! fits for the framing that matters in practice, and the one place it does
//! not is a property of that specific framing's own specification (framing
//! metadata with no content-derivable meaning), not of OBUs-in-general or of
//! `vaco-codec-cbs`'s flat-list design."** Nothing here suggests
//! `vaco-codec-cbs` needs a new capability: the fix, if `LowOverheadBitstream`
//! round-tripping ever matters, is a codec-side one — carrying group
//! boundaries in [`Av1Content`] the way the module doc predicted, not a
//! change to the trait.

use vaco_codec_cbs::{CbsCodec, CbsFragment, CbsUnit, UnitOrigin};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::metadata::{self, Metadata};
use crate::obu::{Av1Framing, ObuHeader, ObuType, units};
use crate::seq::SequenceHeader;

/// What [`Av1Framing::LowOverheadBitstream`] cannot always carry through a
/// split/assemble round trip.
///
/// A `frame_unit_size` boundary in Annex B (the AV1 specification's "Low
/// overhead bitstream format") is a length the *encoder* chose; nothing in
/// the OBUs it wraps says where it falls. [`Av1Cbs::assemble`] reconstructs
/// one `frame_unit` per temporal unit, which is always conformant — a decoder
/// reads the same OBUs in the same order either way — but is not always the
/// same number of `frame_unit_size` wrappers, and therefore not always the
/// same bytes, as whatever produced the input.
///
/// This is *not* the same divergence `vaco-parse-hevc`'s
/// `ANNEXB_EXPRESSIVENESS_DIVERGENCE` documents: HEVC's is a format that
/// cannot express certain bytes at all. This one is a format that can express
/// the same content multiple distinct ways, and this crate's `assemble`
/// picks one of them.
pub const FRAME_UNIT_GRANULARITY_DIVERGENCE: &str = "frame_unit_size boundaries in the Low overhead \
    bitstream format are an encoder choice with no content-derivable meaning, so re-assembling from \
    a flat OBU list cannot reproduce the source's original grouping in general";

/// The typed content of one AV1 OBU.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Av1Content {
    SequenceHeader(Box<SequenceHeader>),
    Metadata(Metadata),
    /// Anything else — most of all `OBU_FRAME`/`OBU_FRAME_HEADER`/
    /// `OBU_TILE_GROUP`, which this crate parses only far enough to find a
    /// key frame (see [`crate::frame_header`]), not as a `CbsCodec::Content`
    /// a filter could edit and rewrite.
    Raw {
        obu_type: ObuType,
        data: Vec<u8>,
    },
}

impl Av1Content {
    /// The `obu_type` this content would be written as.
    #[must_use]
    pub const fn obu_type(&self) -> ObuType {
        match self {
            Self::SequenceHeader(_) => ObuType::SEQUENCE_HEADER,
            Self::Metadata(_) => ObuType::METADATA,
            Self::Raw { obu_type, .. } => *obu_type,
        }
    }
}

/// The AV1 [`CbsCodec`].
///
/// Holds nothing: unlike HEVC's parameter-set store, there is no per-unit
/// escaping to undo (AV1 has no emulation prevention at all — an `obu_size`
/// is a byte count, not a delimiter, so nothing in an OBU's payload can be
/// mistaken for one), so a fresh, zero-sized codec is enough for every call.
#[derive(Debug, Default, Clone, Copy)]
pub struct Av1Cbs;

impl Av1Cbs {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CbsCodec for Av1Cbs {
    type Content = Av1Content;
    type Framing = Av1Framing;
    const NAME: &'static str = "av1";

    fn split(
        &self,
        data: &[u8],
        framing: Av1Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        for obu in units(data, framing) {
            fragment.push(
                CbsUnit::from_source(
                    u32::from(obu.header.obu_type.get()),
                    obu.bytes(data).to_vec(),
                    UnitOrigin {
                        offset: obu.offset,
                        // Neither framing has a fixed-width prefix *per unit*
                        // the way a NAL start code or length prefix is: an
                        // `ObuStream` unit sizes itself inline (already inside
                        // `data` above), and a `LowOverheadBitstream` unit's
                        // wrapping is a *group* property, not a per-unit one.
                        // Zero is the honest answer for both, not an
                        // approximation of a number that does not exist.
                        framing_len: 0,
                    },
                ),
                budget,
            )?;
        }
        Ok(())
    }

    fn assemble(
        &self,
        fragment: &CbsFragment,
        framing: Av1Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        match framing {
            Av1Framing::ObuStream => {
                let total: u64 = fragment.units().iter().map(|u| u.data.len() as u64).sum();
                budget.check(total)?;
                for unit in fragment.units() {
                    out.extend_from_slice(&unit.data);
                }
                Ok(())
            }
            Av1Framing::LowOverheadBitstream => assemble_low_overhead(fragment, out, budget),
        }
    }

    fn read_unit(&mut self, unit: &CbsUnit, budget: &mut Budget) -> Result<Av1Content> {
        let header = ObuHeader::parse(&unit.data).ok_or(Error::UnexpectedEof)?;
        let payload = unit.data.get(header.header_len as usize..).unwrap_or(&[]);
        // `unit.data` already excludes any external `sz` wrapper, so a
        // `LowOverheadBitstream` OBU with `has_size_field == 0` has no size
        // field to skip here either — `header_len` alone is the whole
        // prefix in both framings.
        let payload = if header.has_size_field {
            skip_leb128(payload)
        } else {
            payload
        };
        Ok(match header.obu_type {
            ObuType::SEQUENCE_HEADER => {
                Av1Content::SequenceHeader(Box::new(SequenceHeader::parse(payload, budget)?))
            }
            ObuType::METADATA => Av1Content::Metadata(metadata::parse(payload, budget)?),
            t => Av1Content::Raw {
                obu_type: t,
                data: unit.data.clone(),
            },
        })
    }

    fn write_unit(
        &mut self,
        content: &Av1Content,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        match content {
            Av1Content::Raw { data, .. } => {
                budget.check(data.len() as u64)?;
                out.extend_from_slice(data);
                Ok(())
            }
            // As `vaco-parse-hevc::cbs`: re-encoding a sequence header or a
            // metadata payload bit-exactly is real work this wave did not
            // budget, and a writer that is not bit-exact corrupts a stream
            // silently rather than failing loudly.
            Av1Content::SequenceHeader(_) => Err(Error::Unsupported(
                "writing an AV1 sequence header back out is not implemented",
            )),
            Av1Content::Metadata(_) => Err(Error::Unsupported(
                "writing an AV1 metadata OBU back out is not implemented",
            )),
        }
    }

    fn content_unit_type(&self, content: &Av1Content) -> u32 {
        u32::from(content.obu_type().get())
    }
}

/// Skip past one `leb128()` value at the start of `data`, without decoding
/// it — used only to step over an OBU's own `obu_size` field, whose *value*
/// [`crate::obu::ObuHeader`]'s caller already knows from [`crate::obu::ObuUnit`].
fn skip_leb128(data: &[u8]) -> &[u8] {
    for (i, &b) in data.iter().enumerate().take(8) {
        if b & 0x80 == 0 {
            return data.get(i + 1..).unwrap_or(&[]);
        }
    }
    data.get(8..).unwrap_or(&[])
}

/// `leb128()`-encode `v`, appending to `out`. Used only to write the
/// `temporal_unit_size`/`frame_unit_size`/`obu_length` wrappers this module
/// reconstructs.
fn write_leb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reassemble in [`Av1Framing::LowOverheadBitstream`]: one `temporal_unit` per
/// run starting at an `OBU_TEMPORAL_DELIMITER` (or at the very start, for
/// units before the first one), each wrapped as exactly one `frame_unit`.
///
/// See [`FRAME_UNIT_GRANULARITY_DIVERGENCE`] for what this does not preserve.
fn assemble_low_overhead(
    fragment: &CbsFragment,
    out: &mut Vec<u8>,
    budget: &mut Budget,
) -> Result<()> {
    let td = u32::from(ObuType::TEMPORAL_DELIMITER.get());
    let all_units = fragment.units();
    let mut i = 0usize;
    while i < all_units.len() {
        let start = i;
        i += 1;
        while all_units.get(i).is_some_and(|u| u.unit_type != td) {
            i += 1;
        }
        let group = all_units.get(start..i).unwrap_or(&[]);
        let frame_unit_len: u64 = group.iter().map(obu_wrapped_len).sum();
        budget.check(frame_unit_len)?;

        let mut frame_unit = Vec::new();
        for u in group {
            write_leb128(&mut frame_unit, u.data.len() as u64);
            frame_unit.extend_from_slice(&u.data);
        }
        let mut temporal_unit = Vec::new();
        write_leb128(&mut temporal_unit, frame_unit.len() as u64);
        temporal_unit.extend_from_slice(&frame_unit);

        write_leb128(out, temporal_unit.len() as u64);
        out.extend_from_slice(&temporal_unit);
    }
    Ok(())
}

/// Bytes one unit costs once wrapped in its `obu_length` prefix — an
/// over-estimate by at most 9 (the widest a `leb128()` of a `u64` can be),
/// used only to size a budget check before allocating.
fn obu_wrapped_len(u: &CbsUnit) -> u64 {
    u.data.len() as u64 + 9
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_codec_cbs::Cbs;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// Temporal delimiter, sequence header — the real `libsvtav1` capture
    /// used throughout this crate's tests — then a second temporal delimiter
    /// and a minimal `OBU_FRAME`.
    fn obu_stream() -> Vec<u8> {
        let mut v = vec![0x12, 0x00];
        v.extend_from_slice(&[
            0x0a, 0x0b, 0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
        ]);
        v.extend_from_slice(&[0x12, 0x00, 0x32, 0x02, 0x10, 0x00]);
        v
    }

    #[test]
    fn obu_stream_splits_into_its_four_units() {
        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&obu_stream(), Av1Framing::ObuStream, &mut f, &mut b)
            .expect("splits");
        assert_eq!(
            f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
            [2, 1, 2, 6]
        );
    }

    /// The property `vaco-parse-hevc::cbs` calls "the property the whole
    /// layer rests on" — and it holds for AV1's real framing exactly as it
    /// does for HEVC's.
    #[test]
    fn an_untouched_obu_stream_round_trips_byte_for_byte() {
        let data = obu_stream();
        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            Av1Framing::ObuStream,
            Av1Framing::ObuStream,
            &mut out,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("transform");
        assert_eq!(out, data);
    }

    /// `filter_units`, over OBUs: drop the sequence header, keep the rest.
    #[test]
    fn dropping_a_unit_leaves_the_rest_untouched() {
        let data = obu_stream();
        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            Av1Framing::ObuStream,
            Av1Framing::ObuStream,
            &mut out,
            &mut b,
            |_, f, _| {
                f.retain(|u| u.unit_type != u32::from(ObuType::SEQUENCE_HEADER.get()));
                Ok(())
            },
        )
        .expect("transform");
        assert_eq!(out.len(), data.len() - 13);
        assert!(!out.windows(2).any(|w| w == [0x0a, 0x0b]));
    }

    /// The typed read path.
    #[test]
    fn sequence_header_and_temporal_delimiter_decode_to_their_own_types() {
        let data = obu_stream();
        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Av1Framing::ObuStream, &mut f, &mut b)
            .expect("splits");
        match cbs.read_unit(&f, 1, &mut b) {
            Ok(Av1Content::SequenceHeader(sh)) => {
                assert_eq!((sh.max_frame_width, sh.max_frame_height), (642, 358));
            }
            other => panic!("expected a sequence header, got {other:?}"),
        }
        match cbs.read_unit(&f, 0, &mut b) {
            Ok(Av1Content::Raw { obu_type, data }) => {
                assert_eq!(obu_type, ObuType::TEMPORAL_DELIMITER);
                assert!(data.is_empty() || data.len() == 2);
            }
            other => panic!("expected a raw temporal delimiter, got {other:?}"),
        }
    }

    /// A raw rewrite changes nothing; a typed rewrite says so rather than
    /// writing something wrong.
    #[test]
    fn the_write_path_is_honest_about_what_it_cannot_do() {
        let data = obu_stream();
        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Av1Framing::ObuStream, &mut f, &mut b)
            .expect("splits");
        let sh = cbs.read_unit(&f, 1, &mut b).expect("a sequence header");
        assert!(matches!(
            cbs.update_unit(&mut f, 1, &sh, &mut b),
            Err(Error::Unsupported(_))
        ));
    }

    /// [`FRAME_UNIT_GRANULARITY_DIVERGENCE`], pinned: two encoder-chosen
    /// `frame_unit_size` groups inside one temporal unit collapse to one on
    /// re-assembly, because nothing in the OBUs said there were two.
    #[test]
    fn low_overhead_frame_unit_granularity_does_not_round_trip() {
        // Hand-built Annex B: one temporal_unit containing two separate
        // frame_unit wrappers, each holding one OBU_PADDING (type 15) unit —
        // a type this test picks specifically because it carries no semantic
        // boundary of its own, so the *only* thing distinguishing "two frame
        // units" from "one" is the wrapper the encoder chose to use.
        let obu_a = [0x78u8, 0x00]; // OBU_PADDING, has_size_field=0 not set... see below
        let obu_b = [0x78u8, 0x00];
        // obu header: type=15 (PADDING) => bits6-3=1111, has_size_field=1 =>
        // byte = 0b0111_1010 = 0x7A, then leb128(size=0).
        let obu = [0x7Au8, 0x00];
        let _ = (obu_a, obu_b); // silence unused while keeping the derivation visible

        let mut frame_unit_1 = Vec::new();
        write_leb128(&mut frame_unit_1, obu.len() as u64);
        frame_unit_1.extend_from_slice(&obu);
        let mut frame_unit_2 = Vec::new();
        write_leb128(&mut frame_unit_2, obu.len() as u64);
        frame_unit_2.extend_from_slice(&obu);

        let mut temporal_unit = Vec::new();
        write_leb128(&mut temporal_unit, frame_unit_1.len() as u64);
        temporal_unit.extend_from_slice(&frame_unit_1);
        write_leb128(&mut temporal_unit, frame_unit_2.len() as u64);
        temporal_unit.extend_from_slice(&frame_unit_2);

        let mut data = Vec::new();
        write_leb128(&mut data, temporal_unit.len() as u64);
        data.extend_from_slice(&temporal_unit);

        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Av1Framing::LowOverheadBitstream, &mut f, &mut b)
            .expect("splits");
        // Both OBU_PADDING units are recovered — content is preserved.
        assert_eq!(f.len(), 2);

        let mut out = Vec::new();
        cbs.assemble(&f, Av1Framing::LowOverheadBitstream, &mut out, &mut b)
            .expect("assembles");
        // But the byte-level wrapper is not: this crate always emits one
        // frame_unit per temporal unit, so the two-frame_unit source does not
        // round-trip byte for byte even though it re-splits to the same two
        // units. `FRAME_UNIT_GRANULARITY_DIVERGENCE` names exactly this.
        assert_ne!(out, data, "{FRAME_UNIT_GRANULARITY_DIVERGENCE}");

        let mut back = CbsFragment::new();
        cbs.split(&out, Av1Framing::LowOverheadBitstream, &mut back, &mut b)
            .expect("re-splits");
        assert_eq!(
            back.len(),
            2,
            "content still recovers, just not the framing"
        );
    }

    #[test]
    fn every_truncation_splits_without_panicking() {
        let data = obu_stream();
        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut b = budget();
        for n in 0..data.len() {
            let mut f = CbsFragment::new();
            let _ = cbs.split(&data[..n], Av1Framing::ObuStream, &mut f, &mut b);
            for i in 0..f.len() {
                let _ = cbs.read_unit(&f, i, &mut b);
            }
            f.release(&mut b);
        }
    }
}
