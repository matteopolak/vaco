//! `program_config_element()` — ISO/IEC 14496-3 subpart 1 §1.A.6.2 (also
//! reproduced, identically, in later editions' clause numbering; the syntax
//! table's name is not disputed the way section numbers occasionally are
//! across editions).
//!
//! # Why this exists
//!
//! `channelConfiguration == 0` in an `AudioSpecificConfig` (or an ADTS
//! `channel_configuration` field, though real ADTS streams essentially never
//! use it) means "the actual channel count and assignment is not implied by a
//! small integer — read a program config element out of the bitstream
//! itself." A header parser cannot resolve that (`vaco-parse-aac::asc`'s own
//! doc: "channelConfiguration == 0 ... a header parser cannot resolve"); a
//! decoder, which does read the `raw_data_block`, can and must.

use vaco_bitstream::BitReader;
use vaco_chlayout::{Channel, ChannelLayout};
#[cfg(doc)]
use vaco_core::Error;
use vaco_core::Result;
use vaco_parse_aac::AudioObjectType;

/// One channel-element reference inside a program config element: whether the
/// element is a pair (`CPE`, two channels) or single (`SCE`, one channel), and
/// its `element_instance_tag` (which the corresponding `SCE`/`CPE` header in
/// the bitstream must repeat, so the decoder can match the two up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelElementRef {
    /// `true` for a channel pair element (`CPE`, 2 channels); `false` for a
    /// single channel element (`SCE`, 1 channel).
    pub is_cpe: bool,
    /// `element_instance_tag`, 4 bits.
    pub tag: u8,
}

/// A parsed `program_config_element()`.
///
/// Everything the syntax carries is kept except the comment field's actual
/// bytes (`comment_field_data`), which are free-form ASCII/UTF-8 metadata
/// with no effect on decode; only its declared length is consumed so the
/// reader ends up positioned correctly after it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProgramConfigElement {
    /// `element_instance_tag`, 4 bits — identifies *this* PCE, distinct from
    /// the tags inside `front`/`side`/`back`/`lfe`, which identify the
    /// channel elements it describes.
    pub element_instance_tag: u8,
    /// The 2-bit `object_type` field. Unlike `AudioSpecificConfig`'s 5/6-bit
    /// `audioObjectType`, the PCE's own field is a **plain 2-bit index**
    /// (0=Main, 1=LC, 2=SSR, 3=LTP by Table 1.14) — a second, narrower object
    /// type encoding that exists only inside this element.
    pub object_type: AudioObjectType,
    /// `sampling_frequency_index`, 4 bits.
    pub sampling_frequency_index: u8,
    /// Front channel elements, in bitstream order.
    pub front: Vec<ChannelElementRef>,
    /// Side channel elements, in bitstream order.
    pub side: Vec<ChannelElementRef>,
    /// Back channel elements, in bitstream order.
    pub back: Vec<ChannelElementRef>,
    /// LFE channel elements (always single, never a pair) — the tags in
    /// bitstream order.
    pub lfe: Vec<u8>,
    /// `mono_mixdown_element_number`, when `mono_mixdown_present`.
    pub mono_mixdown_element_number: Option<u8>,
    /// `stereo_mixdown_element_number`, when `stereo_mixdown_present`.
    pub stereo_mixdown_element_number: Option<u8>,
    /// `(matrix_mixdown_idx, pseudo_surround_enable)`, when
    /// `matrix_mixdown_idx_present`.
    pub matrix_mixdown: Option<(u8, bool)>,
}

impl ProgramConfigElement {
    /// The total channel count this element describes: front + side + back
    /// (each pair counting 2) plus one per LFE element. Does not count
    /// associated-data or valid-CC elements — neither carries audio.
    #[must_use]
    pub fn channel_count(&self) -> u32 {
        let pairs = |v: &[ChannelElementRef]| -> u32 {
            v.iter().map(|e| if e.is_cpe { 2 } else { 1 }).sum()
        };
        pairs(&self.front) + pairs(&self.side) + pairs(&self.back) + self.lfe.len() as u32
    }

    /// The output layout when this PCE's element lists describe one of the
    /// layouts whose syntactic order already matches native plane order.
    ///
    /// More complex PCEs can describe a centre `SCE` before a front `CPE`,
    /// which requires a plane permutation before assigning a native layout.
    /// Returning `None` for those preserves the channel count without naming
    /// a layout whose ordering the decoder has not established.
    #[must_use]
    pub fn known_output_layout(&self) -> Option<ChannelLayout> {
        if !self.side.is_empty() || !self.back.is_empty() {
            return None;
        }
        match (self.front.as_slice(), self.lfe.as_slice()) {
            ([ChannelElementRef { is_cpe: false, .. }], []) => Some(ChannelLayout::MONO),
            ([ChannelElementRef { is_cpe: true, .. }], []) => Some(ChannelLayout::STEREO),
            ([ChannelElementRef { is_cpe: true, .. }], [_]) => ChannelLayout::custom([
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::LowFrequency,
            ]),
            _ => None,
        }
    }

    /// Every channel element this PCE describes, in the order a decoder
    /// should expect to encounter the corresponding `SCE`/`CPE`/`LFE` headers
    /// in the bitstream: front, then side, then back, then LFE — the same
    /// order the syntax table itself declares these four lists in.
    #[must_use]
    pub fn element_order(&self) -> Vec<ChannelElementRef> {
        let mut order = Vec::new();
        order.extend_from_slice(&self.front);
        order.extend_from_slice(&self.side);
        order.extend_from_slice(&self.back);
        for &tag in &self.lfe {
            order.push(ChannelElementRef { is_cpe: false, tag });
        }
        order
    }

    /// Parse a `program_config_element()` from a bit reader positioned right
    /// after its `id_syn_ele` (the 3-bit element-type tag naming this a PCE)
    /// has already been consumed by the caller.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] on truncation. This element has no field
    /// whose value alone is "invalid" the way e.g. a reserved
    /// `channelConfiguration` is — every counted list is bounded by its own
    /// small bit width (4, 4, 4 or 2 bits — at most 15 elements per list,
    /// 3 for LFE), so there is no unbounded loop to guard against here
    /// either.
    pub fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let element_instance_tag = r.get(4) as u8;
        let object_type = AudioObjectType(r.get(2) as u8);
        let sampling_frequency_index = r.get(4) as u8;
        let num_front = r.get(4);
        let num_side = r.get(4);
        let num_back = r.get(4);
        let num_lfe = r.get(2);
        let num_assoc_data = r.get(3);
        let num_valid_cc = r.get(4);

        let mono_mixdown_element_number = if r.get_bit() != 0 {
            Some(r.get(4) as u8)
        } else {
            None
        };
        let stereo_mixdown_element_number = if r.get_bit() != 0 {
            Some(r.get(4) as u8)
        } else {
            None
        };
        let matrix_mixdown = if r.get_bit() != 0 {
            let idx = r.get(2) as u8;
            let pseudo_surround = r.get_bit() != 0;
            Some((idx, pseudo_surround))
        } else {
            None
        };

        let read_refs = |r: &mut BitReader<'_>, count: u32| -> Vec<ChannelElementRef> {
            let mut v = Vec::new();
            for _ in 0..count {
                let is_cpe = r.get_bit() != 0;
                let tag = r.get(4) as u8;
                v.push(ChannelElementRef { is_cpe, tag });
            }
            v
        };
        let front = read_refs(r, num_front);
        let side = read_refs(r, num_side);
        let back = read_refs(r, num_back);

        let mut lfe = Vec::new();
        for _ in 0..num_lfe {
            lfe.push(r.get(4) as u8);
        }

        // Associated-data elements: one tag each, no audio content.
        for _ in 0..num_assoc_data {
            r.skip(4);
        }

        // Valid CC elements: `cc_element_is_ind_sw` (1 bit) + tag (4 bits).
        for _ in 0..num_valid_cc {
            r.skip(1 + 4);
        }

        r.align();
        let comment_field_bytes = r.get(8);
        r.skip_bytes(usize::try_from(comment_field_bytes).unwrap_or(0));

        r.check()?;

        Ok(Self {
            element_instance_tag,
            object_type,
            sampling_frequency_index,
            front,
            side,
            back,
            lfe,
            mono_mixdown_element_number,
            stereo_mixdown_element_number,
            matrix_mixdown,
        })
    }
}

/// The 3-bit `id_syn_ele` value naming a `program_config_element` inside a
/// `raw_data_block`. ISO/IEC 14496-3 subpart 4 Table 4.68 (`ID_PCE`).
pub const ID_SYN_ELE_PCE: u32 = 5;

/// If the very next syntax element in `r` is a program config element,
/// consume and parse it; otherwise leave `r` exactly as it was and return
/// `Ok(None)`.
///
/// This only looks at the **leading** element of a `raw_data_block`. A PCE
/// that follows one or more channel elements (`SCE`/`CPE`/`CCE`/`LFE`) cannot
/// be found this way, because those elements carry no length prefix of their
/// own — skipping past one requires actually decoding it (window sequence,
/// section data, and spectral Huffman data), which this probe does not do.
/// Real encoders place a stream's PCE first in its very first
/// frame, which is the case this function serves; a nonconforming stream that
/// puts its PCE later is a disclosed gap, not silently mishandled — the
/// caller sees `Ok(None)` and can report "channel layout undetermined"
/// honestly rather than guessing.
///
/// # Errors
///
/// Whatever [`ProgramConfigElement::read`] returns, if the leading element
/// claims to be a PCE but is truncated.
pub fn find_leading_program_config_element(
    r: &mut BitReader<'_>,
) -> Result<Option<ProgramConfigElement>> {
    if r.bits_left() < 3 {
        return Ok(None);
    }
    let mark = r.mark();
    let id_syn_ele = r.get(3);
    if id_syn_ele != ID_SYN_ELE_PCE {
        r.restore(mark);
        return Ok(None);
    }
    match ProgramConfigElement::read(r) {
        Ok(pce) => Ok(Some(pce)),
        Err(e) => {
            r.restore(mark);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::{ChannelElementRef, ProgramConfigElement, find_leading_program_config_element};
    use vaco_bitstream::{BitReader, BitWriter};
    use vaco_parse_aac::AudioObjectType;

    /// Hand-build a minimal PCE bitstream: 5.1 laid out as one SCE (centre)
    /// front, one CPE front, one CPE back... no, simpler: mirror
    /// `channel_configuration == 6`'s own shape (1 SCE front, 1 CPE front, 1
    /// CPE back, 1 LFE) entirely through explicit element lists, so the
    /// resulting channel count (1+2+2+1=6) is independently checkable against
    /// the well-known 5.1 case.
    fn build_51_like_pce() -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(4, 0); // element_instance_tag
        w.put(2, 1); // object_type = LC
        w.put(4, 3); // sampling_frequency_index (48000)
        w.put(4, 2); // num_front_channel_elements: SCE + CPE
        w.put(4, 0); // num_side
        w.put(4, 1); // num_back: CPE
        w.put(2, 1); // num_lfe
        w.put(3, 0); // num_assoc_data
        w.put(4, 0); // num_valid_cc
        w.put(1, 0); // mono_mixdown_present
        w.put(1, 0); // stereo_mixdown_present
        w.put(1, 0); // matrix_mixdown_idx_present
        // front[0]: SCE, tag 0
        w.put(1, 0);
        w.put(4, 0);
        // front[1]: CPE, tag 1
        w.put(1, 1);
        w.put(4, 1);
        // back[0]: CPE, tag 2
        w.put(1, 1);
        w.put(4, 2);
        // lfe[0]: tag 3
        w.put(4, 3);
        w.align_zero();
        w.put(8, 0); // comment_field_bytes = 0
        w.align_zero();
        w.finish()
    }

    #[test]
    fn parses_a_51_shaped_pce_with_the_right_channel_count() {
        let bytes = build_51_like_pce();
        let mut r = BitReader::new(&bytes);
        let pce = ProgramConfigElement::read(&mut r).unwrap();
        assert_eq!(pce.channel_count(), 6);
        assert_eq!(pce.front.len(), 2);
        assert_eq!(pce.back.len(), 1);
        assert_eq!(pce.lfe, vec![3]);
        let order = pce.element_order();
        assert_eq!(
            order,
            vec![
                ChannelElementRef {
                    is_cpe: false,
                    tag: 0
                },
                ChannelElementRef {
                    is_cpe: true,
                    tag: 1
                },
                ChannelElementRef {
                    is_cpe: true,
                    tag: 2
                },
                ChannelElementRef {
                    is_cpe: false,
                    tag: 3
                },
            ]
        );
    }

    #[test]
    fn a_front_pair_with_one_lfe_has_the_21_output_layout() {
        let pce = ProgramConfigElement {
            element_instance_tag: 0,
            object_type: AudioObjectType::AAC_LC,
            sampling_frequency_index: 3,
            front: vec![ChannelElementRef {
                is_cpe: true,
                tag: 0,
            }],
            side: Vec::new(),
            back: Vec::new(),
            lfe: vec![1],
            mono_mixdown_element_number: None,
            stereo_mixdown_element_number: None,
            matrix_mixdown: None,
        };
        assert_eq!(
            pce.known_output_layout().map(|layout| layout.mask()),
            Some(0xb)
        );
    }

    #[test]
    fn find_leading_pce_detects_and_consumes_a_real_one() {
        // `build_51_like_pce` starts mid-element (matching
        // `ProgramConfigElement::read`'s own contract of being called after
        // `id_syn_ele` is already consumed), so exercising
        // `find_leading_program_config_element` — which reads `id_syn_ele`
        // itself — needs its own encode with that 3-bit prefix included.
        let mut full = BitWriter::new();
        full.put(3, super::ID_SYN_ELE_PCE);
        full.put(4, 0);
        full.put(2, 1);
        full.put(4, 3);
        full.put(4, 0);
        full.put(4, 0);
        full.put(4, 0);
        full.put(2, 0);
        full.put(3, 0);
        full.put(4, 0);
        full.put(1, 0);
        full.put(1, 0);
        full.put(1, 0);
        full.align_zero();
        full.put(8, 0);
        full.align_zero();
        let full_bytes = full.finish();
        let mut r2 = BitReader::new(&full_bytes);
        let found = find_leading_program_config_element(&mut r2).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().channel_count(), 0);
    }

    #[test]
    fn a_non_pce_leading_element_leaves_the_reader_untouched() {
        let mut w = BitWriter::new();
        w.put(3, 0); // id_syn_ele = SCE, not PCE
        w.put(29, 0);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let before = r.bit_pos();
        let found = find_leading_program_config_element(&mut r).unwrap();
        assert!(found.is_none());
        assert_eq!(r.bit_pos(), before);
    }
}
