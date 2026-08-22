//! HEVC's implementation of [`CbsCodec`] — the read/modify/write face of the
//! crate.
//!
//! # What this adds over the parsers
//!
//! The rest of the crate reads a NAL unit and tells you what it says. This
//! module makes a *stream* editable: split an access unit or an `hvcC` into a
//! [`CbsFragment`], drop or insert units, decode the ones you care about,
//! change a field, write it back, and re-assemble — in the same framing or a
//! different one. That is the whole of `hevc_metadata`, `filter_units`,
//! `hevc_mp4toannexb` and `extract_extradata`.
//!
//! # The write path is partial, deliberately
//!
//! [`HevcCbs::write_unit`] can write back a unit whose content is
//! [`HevcContent::Raw`] — every unit, since that is what a unit that was not
//! decoded becomes — but it cannot yet *re-encode* a typed SPS or PPS. Writing
//! an SPS means writing `profile_tier_level()`, every reference picture set and
//! the whole VUI back out bit-exactly, and a writer that is not bit-exact
//! silently corrupts a stream rather than failing.
//!
//! So the split is: **anything a filter does by moving whole units works
//! today**, and anything that edits a parameter set's *fields* returns
//! [`Error::Unsupported`]. Plan 15 §D-19 budgets the write path separately for
//! exactly this reason, and the shape it has to fill in is
//! [`HevcCbs::write_unit`]'s `Raw` arm plus one per typed variant.

use vaco_codec_cbs::{CbsCodec, CbsFragment, CbsUnit, UnitOrigin};
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, RbspBuf, units};
use vaco_limits::Budget;

use crate::nal::{HevcNalHeader, NalUnitType};

/// What a fragment cannot carry unchanged through Annex B.
///
/// Two shapes, both of which [`annexb_safe`] tests for, and **neither of which
/// can occur in a conforming stream**:
///
/// 1. **A unit whose bytes end in `0x00`.** §B.1 permits `trailing_zero_8bits`
///    after a NAL unit and they are indistinguishable from payload zeros — the
///    four-byte start code's own leading zero is one of them — so
///    `vaco-format-nalu`'s Annex B iterator trims them. §7.4.1.1's
///    `rbsp_trailing_bits()` ends every conforming unit with a `1` bit, so the
///    last byte is never zero.
/// 2. **A unit whose bytes contain `00 00 01`.** Writing it as Annex B makes
///    that sequence a start code, and reading it back yields *two* units.
///    §7.4.1.1's emulation prevention exists precisely so an EBSP never contains
///    one.
///
/// Both are properties of the *format*, not of this crate: Annex B is a strictly
/// less expressive container than a length prefix. Both were found by the
/// `cbs_hevc` fuzz target, which excludes exactly these cases from its
/// round-trip assertion.
///
/// The name exists so a conformance audit can find it, and
/// `a_unit_annex_b_cannot_express_is_reported` asserts the divergence is still
/// real rather than quietly closing.
pub const ANNEXB_EXPRESSIVENESS_DIVERGENCE: &str =
    "a NAL unit ending in 0x00, or containing 00 00 01, cannot round-trip through Annex B";

/// Whether `unit` survives being written as Annex B and read back.
///
/// A `hevc_mp4toannexb`-shaped filter should check this before reframing: a unit
/// that fails it is not a conforming NAL unit, and Annex B has no way to carry
/// it. See [`ANNEXB_EXPRESSIVENESS_DIVERGENCE`].
#[must_use]
pub fn annexb_safe(unit: &[u8]) -> bool {
    unit.last() != Some(&0) && !vaco_codec_cbs::violates_ebsp_constraint(unit)
}
use crate::pps::Pps;
use crate::sei::SeiMessage;
use crate::sps::Sps;
use crate::vps::Vps;

/// The typed content of one HEVC NAL unit.
///
/// [`HevcContent::Raw`] is not a failure: a unit whose syntax this crate does
/// not decode — a slice's payload, filler, a reserved type — is kept whole so
/// it can be re-emitted byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HevcContent {
    /// A video parameter set.
    Vps(Box<Vps>),
    /// A sequence parameter set.
    Sps(Box<Sps>),
    /// A picture parameter set.
    Pps(Box<Pps>),
    /// The messages of one SEI NAL unit, and whether it was a suffix unit.
    ///
    /// Owned rather than borrowed, because the borrow would be of the
    /// fragment's own bytes and would stop a caller editing the fragment while
    /// holding the decoded value.
    Sei {
        /// Whether this came from a `SUFFIX_SEI_NUT`.
        suffix: bool,
        /// The messages, with their payloads re-owned.
        messages: Vec<OwnedSeiMessage>,
    },
    /// Anything else: the unit's bytes, escaping intact.
    Raw {
        /// The NAL unit type.
        nal_unit_type: NalUnitType,
        /// The bytes, header included.
        data: Vec<u8>,
    },
}

/// One SEI message with its payload owned rather than borrowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedSeiMessage {
    /// `payloadType`.
    pub payload_type: u32,
    /// `payloadSize`, as declared.
    pub payload_size: u32,
    /// Whether the declared size ran past the end of the unit.
    pub truncated: bool,
    /// The payload bytes, un-decoded.
    pub data: Vec<u8>,
}

impl HevcContent {
    /// The NAL unit type this content would be written as.
    #[must_use]
    pub const fn nal_unit_type(&self) -> NalUnitType {
        match self {
            Self::Vps(_) => NalUnitType::VPS_NUT,
            Self::Sps(_) => NalUnitType::SPS_NUT,
            Self::Pps(_) => NalUnitType::PPS_NUT,
            Self::Sei { suffix: true, .. } => NalUnitType::SUFFIX_SEI_NUT,
            Self::Sei { suffix: false, .. } => NalUnitType::PREFIX_SEI_NUT,
            Self::Raw { nal_unit_type, .. } => *nal_unit_type,
        }
    }
}

/// The HEVC [`CbsCodec`].
///
/// Holds an [`RbspBuf`] so de-escaping a whole stream's units is one allocation
/// rather than one per unit, and nothing else — the parameter-set store belongs
/// to [`HevcParser`](crate::parser::HevcParser), which is the stateful reader;
/// a bitstream filter wants each unit decoded on its own terms.
#[derive(Debug, Default)]
pub struct HevcCbs {
    rbsp: RbspBuf,
}

impl HevcCbs {
    /// A fresh codec.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl CbsCodec for HevcCbs {
    type Content = HevcContent;
    type Framing = Framing;
    const NAME: &'static str = "hevc";

    fn split(
        &self,
        data: &[u8],
        framing: Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        for nal in units(data, framing) {
            let Some(header) = HevcNalHeader::parse(nal.data) else {
                // A unit shorter than its own header is not a unit. Dropping it
                // rather than failing keeps a filter working on a stream with
                // one stray byte in it.
                continue;
            };
            fragment.push(
                CbsUnit::from_source(
                    u32::from(header.nal_unit_type.get()),
                    nal.data.to_vec(),
                    UnitOrigin {
                        offset: nal.offset,
                        framing_len: nal.start_code_len,
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
        framing: Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        let total: usize = fragment
            .units()
            .iter()
            .map(|u| u.data.len().saturating_add(4))
            .sum();
        budget.check(total as u64)?;
        for unit in fragment.units() {
            match framing {
                Framing::AnnexB => {
                    // Three bytes only when the source was Annex B and used
                    // three; four otherwise. A unit that came from a
                    // length-prefixed buffer has `framing_len` equal to the
                    // prefix width, which is not a start-code length at all —
                    // reading it as one is how a reframe round trip comes back
                    // three bytes per unit shorter than it went out.
                    if unit.origin.map_or(4, |o| o.framing_len) != 3 {
                        out.push(0);
                    }
                    out.extend_from_slice(&[0, 0, 1]);
                }
                Framing::LengthPrefixed(size) => {
                    let len = unit.data.len() as u64;
                    if len > size.max_unit_len() {
                        return Err(Error::InvalidData(
                            "NAL unit too long for this length prefix",
                        ));
                    }
                    for k in (0..size.len()).rev() {
                        out.push(((len >> (k * 8)) & 0xFF) as u8);
                    }
                }
            }
            out.extend_from_slice(&unit.data);
        }
        Ok(())
    }

    fn read_unit(&mut self, unit: &CbsUnit, budget: &mut Budget) -> Result<HevcContent> {
        let header = HevcNalHeader::parse(&unit.data).ok_or(Error::UnexpectedEof)?;
        let t = header.nal_unit_type;
        // Only the base layer's syntax is described here; a unit from any other
        // layer is kept whole so a filter can still move or drop it.
        if !header.is_base_layer() {
            return Ok(HevcContent::Raw {
                nal_unit_type: t,
                data: unit.data.clone(),
            });
        }
        self.rbsp.fill(&unit.data, budget)?;
        let rbsp = self.rbsp.as_slice();
        Ok(match t {
            NalUnitType::VPS_NUT => HevcContent::Vps(Box::new(Vps::parse(rbsp, budget)?)),
            NalUnitType::SPS_NUT => HevcContent::Sps(Box::new(Sps::parse(rbsp, budget)?)),
            NalUnitType::PPS_NUT => HevcContent::Pps(Box::new(Pps::parse(rbsp, budget)?)),
            t if t.is_sei() => {
                let messages = crate::sei::parse(rbsp, None, budget)?;
                HevcContent::Sei {
                    suffix: t == NalUnitType::SUFFIX_SEI_NUT,
                    messages: messages.iter().map(own_message).collect(),
                }
            }
            _ => HevcContent::Raw {
                nal_unit_type: t,
                data: unit.data.clone(),
            },
        })
    }

    fn write_unit(
        &mut self,
        content: &HevcContent,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        match content {
            HevcContent::Raw { data, .. } => {
                budget.check(data.len() as u64)?;
                out.extend_from_slice(data);
                Ok(())
            }
            // See the module documentation: a parameter-set writer that is not
            // bit-exact corrupts a stream silently, so there is none yet.
            HevcContent::Vps(_) | HevcContent::Sps(_) | HevcContent::Pps(_) => Err(
                Error::Unsupported("writing an HEVC parameter set back out is not implemented"),
            ),
            HevcContent::Sei { .. } => Err(Error::Unsupported(
                "writing an HEVC SEI unit back out is not implemented",
            )),
        }
    }

    fn content_unit_type(&self, content: &HevcContent) -> u32 {
        u32::from(content.nal_unit_type().get())
    }
}

/// Copy one [`SeiMessage`]'s payload out of the fragment's buffer.
fn own_message(m: &SeiMessage<'_>) -> OwnedSeiMessage {
    OwnedSeiMessage {
        payload_type: m.payload_type,
        payload_size: m.payload_size,
        truncated: m.truncated,
        data: match &m.payload {
            crate::sei::SeiPayload::Other { data, .. }
            | crate::sei::SeiPayload::DecodedPictureHash { data, .. } => (*data).to_vec(),
            _ => Vec::new(),
        },
    }
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
    use vaco_format_nalu::LengthSize;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// VPS, SPS, PPS, prefix SEI and an IDR slice, in Annex B, from `sd.265`.
    fn stream() -> Vec<u8> {
        let mut v = Vec::new();
        for nal in [
            &[
                0x40u8, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90,
                0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09,
            ][..],
            &[
                0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
                0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
                0xbc, 0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
            ][..],
            &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40][..],
            &[0x4e, 0x01, 0x05, 0x02, 0x11, 0x22, 0x80][..],
            &[0x28, 0x01, 0xaf, 0x1d, 0x30, 0xc6, 0x23, 0x40, 0xf2, 0xcd][..],
        ] {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(nal);
        }
        v
    }

    #[test]
    fn a_stream_splits_into_its_five_units() {
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&stream(), Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        assert_eq!(
            f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
            [32, 33, 34, 39, 20]
        );
    }

    /// The property the whole layer rests on.
    #[test]
    fn an_untouched_fragment_round_trips_byte_for_byte() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            Framing::AnnexB,
            Framing::AnnexB,
            &mut out,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("transform");
        assert_eq!(out, data);
    }

    /// `filter_units`: drop every SEI unit, keep everything else exactly.
    #[test]
    fn dropping_sei_leaves_the_rest_untouched() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            Framing::AnnexB,
            Framing::AnnexB,
            &mut out,
            &mut b,
            |_, f, _| {
                f.retain(|u| u.unit_type != 39 && u.unit_type != 40);
                Ok(())
            },
        )
        .expect("transform");
        // The SEI unit was 7 bytes plus a 4-byte start code.
        assert_eq!(out.len(), data.len() - 11);
        assert!(!out.windows(2).any(|w| w == [0x4e, 0x01]));
    }

    /// `hevc_mp4toannexb`, and its inverse.
    #[test]
    fn reframing_is_lossless_in_both_directions() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut prefixed = Vec::new();
        cbs.transform(
            &data,
            Framing::AnnexB,
            Framing::LengthPrefixed(LengthSize::FOUR),
            &mut prefixed,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("to length-prefixed");
        // Five units, each losing a four-byte start code and gaining a
        // four-byte length: the same size.
        assert_eq!(prefixed.len(), data.len());

        let mut back = Vec::new();
        cbs.transform(
            &prefixed,
            Framing::LengthPrefixed(LengthSize::FOUR),
            Framing::AnnexB,
            &mut back,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("back to Annex B");
        assert_eq!(back, data);
    }

    /// The typed read path, over every unit the crate understands.
    #[test]
    fn each_parameter_set_decodes_to_its_own_type() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        assert!(matches!(
            cbs.read_unit(&f, 0, &mut b),
            Ok(HevcContent::Vps(_))
        ));
        match cbs.read_unit(&f, 1, &mut b) {
            Ok(HevcContent::Sps(sps)) => assert_eq!(sps.dimensions(), Some((640, 360))),
            other => panic!("expected an SPS, got {other:?}"),
        }
        assert!(matches!(
            cbs.read_unit(&f, 2, &mut b),
            Ok(HevcContent::Pps(_))
        ));
        match cbs.read_unit(&f, 3, &mut b) {
            Ok(HevcContent::Sei { suffix, messages }) => {
                assert!(!suffix);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].payload_type, 5);
            }
            other => panic!("expected an SEI unit, got {other:?}"),
        }
        // A slice is kept whole.
        match cbs.read_unit(&f, 4, &mut b) {
            Ok(HevcContent::Raw { nal_unit_type, .. }) => {
                assert_eq!(nal_unit_type, NalUnitType::IDR_N_LP);
            }
            other => panic!("expected a raw unit, got {other:?}"),
        }
    }

    /// A raw unit writes back byte for byte, and a typed parameter set says so
    /// rather than writing something wrong.
    #[test]
    fn the_write_path_is_honest_about_what_it_cannot_do() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        let before = f.units()[4].data.clone();
        let raw = cbs.read_unit(&f, 4, &mut b).expect("a raw unit");
        cbs.update_unit(&mut f, 4, &raw, &mut b).expect("writes");
        assert_eq!(f.units()[4].data, before, "a raw rewrite changes nothing");

        let sps = cbs.read_unit(&f, 1, &mut b).expect("an SPS");
        assert!(matches!(
            cbs.update_unit(&mut f, 1, &sps, &mut b),
            Err(Error::Unsupported(_))
        ));
        // ...and the fragment is unchanged by the refusal.
        assert_eq!(f.units()[1].data[0], 0x42);
    }

    /// `extract_extradata`: lift the parameter sets out of an access unit into
    /// a fragment of their own.
    #[test]
    fn parameter_sets_can_be_lifted_into_their_own_fragment() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        let mut extradata = CbsFragment::new();
        for unit in f.units() {
            if NalUnitType::from_u8(unit.unit_type as u8).is_parameter_set() {
                extradata.push(unit.clone(), &mut b).expect("push");
            }
        }
        assert_eq!(extradata.len(), 3);
        let mut out = Vec::new();
        cbs.assemble(&extradata, Framing::AnnexB, &mut out, &mut b)
            .expect("assembles");
        assert_eq!(out.len(), 4 * 3 + 24 + 42 + 7);
        extradata.release(&mut b);
        f.release(&mut b);
    }

    /// Both reframing divergences, pinned. See
    /// [`ANNEXB_EXPRESSIVENESS_DIVERGENCE`].
    #[test]
    fn a_unit_annex_b_cannot_express_is_reported() {
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();

        // Case 1: a unit whose last two bytes are zero. Length-prefixed says
        // five bytes; Annex B gives three back.
        let trailing = [0x00u8, 0x00, 0x00, 0x05, 0x40, 0x01, 0x0c, 0x00, 0x00];
        // Case 2: a unit containing a start code, which Annex B splits in two.
        // The second unit needs two bytes of its own, or the split drops it as
        // too short to hold a header — which would hide the divergence.
        let embedded = [
            0x00u8, 0x00, 0x00, 0x08, 0x40, 0x01, 0x0c, 0x00, 0x00, 0x01, 0x0d, 0x0e,
        ];
        // ...and one that is a conforming EBSP, which survives untouched.
        let ok = [0x00u8, 0x00, 0x00, 0x03, 0x40, 0x01, 0x0c];

        for (name, prefixed, safe, expect_units) in [
            ("trailing zeros", &trailing[..], false, 1usize),
            ("embedded start code", &embedded[..], false, 2),
            ("conforming", &ok[..], true, 1),
        ] {
            let mut f = CbsFragment::new();
            cbs.split(
                prefixed,
                Framing::LengthPrefixed(LengthSize::FOUR),
                &mut f,
                &mut b,
            )
            .expect("splits");
            assert_eq!(f.len(), 1, "{name}: one unit in");
            let before = f.units()[0].data.clone();
            assert_eq!(
                annexb_safe(&before),
                safe,
                "{name}: {ANNEXB_EXPRESSIVENESS_DIVERGENCE}"
            );

            let mut annexb = Vec::new();
            cbs.assemble(&f, Framing::AnnexB, &mut annexb, &mut b)
                .expect("assembles");
            let mut back = CbsFragment::new();
            cbs.split(&annexb, Framing::AnnexB, &mut back, &mut b)
                .expect("splits");
            assert_eq!(back.len(), expect_units, "{name}: units out");
            if safe {
                assert_eq!(back.units()[0].data, before, "{name}: survives");
            } else {
                assert_ne!(back.units()[0].data, before, "{name}: diverges");
            }
            f.release(&mut b);
            back.release(&mut b);
        }
    }

    #[test]
    fn every_truncation_splits_without_panicking() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        for n in 0..data.len() {
            let mut f = CbsFragment::new();
            let _ = cbs.split(&data[..n], Framing::AnnexB, &mut f, &mut b);
            for i in 0..f.len() {
                let _ = cbs.read_unit(&f, i, &mut b);
            }
            f.release(&mut b);
        }
    }
}
