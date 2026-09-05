//! `raw_data_block()` — ISO/IEC 14496-3 subpart 4 Table 4.3: the top-level
//! per-frame element loop (`SCE`/`CPE`/`CCE`/`LFE`/`DSE`/`PCE`/`FIL`/`END`,
//! ids 0..=7 per Table 4.68).
//!
//! # What is fully decoded, what is skipped, and what is refused
//!
//! - `SCE`, `LFE`: one [`crate::ics_stream`] each (`common_window = false`).
//! - `CPE`: `common_window`, and — when set — a shared `ics_info()` plus
//!   `ms_mask_present`/`ms_used` (stored as [`MsMask`] for `reconstruct`'s
//!   "joint stereo" step), then two `individual_channel_stream()`s.
//! - `DSE`: skipped wholesale by its self-delimiting byte count.
//! - `FIL`: skipped by its self-delimiting byte count unless its first
//!   `extension_type` names SBR, which is refused before AAC-LC PCM decode.
//! - `PCE`: parsed in full ([`crate::pce::ProgramConfigElement`]). The
//!   decoder consumes a leading PCE while resolving a pending configuration;
//!   a PCE found after audio elements is refused rather than ignored under a
//!   stale configuration — see `docs/codec/vaco-codec-aac.md`.
//! - `CCE` (`coupling_channel_element()`): **not implemented** —
//!   `Error::Unsupported`. It carries its own `individual_channel_stream()`
//!   plus a per-coupled-element gain list this crate has not transcribed,
//!   and is rare in real 1/2/6-channel content (this crate's own resolved
//!   configurations); gated rather than guessed at.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

use crate::ics::IcsInfo;
use crate::ics_stream::{self, IcsStream};
use crate::pce::{ChannelElementRef, ProgramConfigElement};

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;
const EXT_SBR_DATA: u32 = 13;
const EXT_SBR_DATA_CRC: u32 = 14;

/// One decoded syntactic element of a `raw_data_block()`.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the decoded elements are the point of this parse (bit-exact \
              consumption) even though nothing reads their fields back yet; \
              #445 will read window/scalefactor/spectral data from them, and \
              a future PCE-mid-stream config update will read ProgramConfig"
)]
pub(crate) enum Element {
    Single {
        tag: u8,
        stream: IcsStream,
    },
    /// `(ms_mask, ch0, ch1)`. `common_window` is implied: `ms_mask` is
    /// `None` exactly when `common_window` was `0` (M/S is never legal
    /// without a shared `ics_info()` for AAC-LC — §4.6.8.1.1).
    Pair {
        tag: u8,
        ms_mask: Option<MsMask>,
        ch0: IcsStream,
        ch1: IcsStream,
    },
    Lfe {
        tag: u8,
        stream: IcsStream,
    },
    ProgramConfig(ProgramConfigElement),
}

impl Element {
    /// The PCE-visible identity of an audio-bearing element, if this element
    /// carries PCM. PCE and non-audio elements have no corresponding entry.
    #[must_use]
    pub(crate) const fn channel_element_ref(&self) -> Option<ChannelElementRef> {
        match self {
            Self::Single { tag, .. } | Self::Lfe { tag, .. } => Some(ChannelElementRef {
                is_cpe: false,
                tag: *tag,
            }),
            Self::Pair { tag, .. } => Some(ChannelElementRef {
                is_cpe: true,
                tag: *tag,
            }),
            Self::ProgramConfig(_) => None,
        }
    }
}

/// A `channel_pair_element()`'s M/S signalling (Table 4.5, §4.6.8.1.2):
/// `ms_mask_present == 0` means no band uses M/S, `== 2` means every band
/// does, and `== 1` carries an explicit per-`(group, band)` bit — `used`
/// covers all three uniformly, one entry per `(group, band)` pair in the
/// same order [`crate::section::read_all_groups`] produces.
#[derive(Debug, Clone)]
pub(crate) struct MsMask {
    pub(crate) used: Vec<Vec<bool>>,
}

/// Skip a `data_stream_element()`: `element_instance_tag`(4),
/// `data_byte_align_flag`(1), `count`(8, `+= esc_count`(8) if 255), then
/// `byte_alignment()` if the flag was set, then exactly `cnt` raw bytes.
fn skip_data_stream_element(r: &mut BitReader<'_>) -> Result<()> {
    let _tag = r.get(4);
    let align = r.get_bit() != 0;
    let mut cnt = r.get(8);
    if cnt == 255 {
        cnt += r.get(8);
    }
    if align {
        r.align();
    }
    r.skip_bytes(cnt as usize);
    Ok(r.check()?)
}

/// Skip a `fill_element()`: `count`(4, `+= esc_count - 1`(8) if 15), then
/// exactly `cnt` raw bytes. The first payload nibble is `extension_type`; SBR
/// types are not skipped because that would silently treat implicit HE-AAC as
/// AAC-LC.
fn skip_fill_element(r: &mut BitReader<'_>) -> Result<()> {
    let mut cnt = r.get(4);
    if cnt == 15 {
        cnt = cnt.saturating_add(r.get(8)).saturating_sub(1);
    }
    if cnt == 0 {
        return Ok(r.check()?);
    }
    let extension_type = r.get(4);
    if matches!(extension_type, EXT_SBR_DATA | EXT_SBR_DATA_CRC) {
        return Err(Error::Unsupported(
            "vaco-codec-aac: SBR fill payload is not implemented — refusing implicit HE-AAC",
        ));
    }
    r.skip(cnt.saturating_mul(8).saturating_sub(4));
    Ok(r.check()?)
}

/// Read `ms_mask_present`/`ms_used` for a `common_window` `CPE`
/// (§4.6.8.1.2/Table 4.5): `0` = no band uses M/S, `1` = an explicit
/// per-`(group, band)` bit follows, `2` = every band does (no further
/// bits), `3` is reserved and treated as `0` (no bits follow it either,
/// matching the syntax table exactly — only value `1` has a payload).
fn read_ms_mask(r: &mut BitReader<'_>, ics: &IcsInfo) -> Result<MsMask> {
    let ms_mask_present = r.get(2);
    let num_groups = ics.num_window_groups();
    let max_sfb = usize::from(ics.max_sfb);
    let used = match ms_mask_present {
        1 => {
            let mut used = Vec::new();
            for _ in 0..num_groups {
                let mut group = Vec::new();
                for _ in 0..max_sfb {
                    group.push(r.get_bit() != 0);
                }
                used.push(group);
            }
            used
        }
        2 => vec![vec![true; max_sfb]; num_groups],
        _ => vec![vec![false; max_sfb]; num_groups],
    };
    r.check()?;
    Ok(MsMask { used })
}

/// Read `raw_data_block()`: every element up to and including `ID_END`,
/// then `byte_alignment()`.
///
/// # Errors
///
/// [`Error::Unsupported`] for a `CCE` (see module doc). Otherwise whatever
/// the underlying element readers return.
pub(crate) fn read(r: &mut BitReader<'_>, sfi: u8) -> Result<Vec<Element>> {
    let mut elements = Vec::new();
    loop {
        // `try_get`, not `get`: `BitReader::get` pads exhausted input with
        // zero bits rather than erroring, so a stream truncated (or simply
        // malformed) before it ever presents `ID_END` would otherwise read
        // `id == 0` (`ID_SCE`) forever — an unbounded loop that pushes a
        // real `Element` onto `elements` every iteration. Found by fuzzing
        // (`aac_config`, an out-of-memory artifact), not by inspection.
        let id = r.try_get(3)?;
        match id {
            ID_SCE => {
                let tag = r.get(4) as u8;
                let stream = ics_stream::read(r, false, None, sfi)?;
                elements.push(Element::Single { tag, stream });
            }
            ID_LFE => {
                let tag = r.get(4) as u8;
                let stream = ics_stream::read(r, false, None, sfi)?;
                elements.push(Element::Lfe { tag, stream });
            }
            ID_CPE => {
                let tag = r.get(4) as u8;
                let common_window = r.get_bit() != 0;
                let (shared_ics, ms_mask) = if common_window {
                    let ics = IcsInfo::read(r)?;
                    let mask = read_ms_mask(r, &ics)?;
                    (Some(ics), Some(mask))
                } else {
                    (None, None)
                };
                let ch0 = ics_stream::read(r, common_window, shared_ics.as_ref(), sfi)?;
                let ch1 = ics_stream::read(r, common_window, shared_ics.as_ref(), sfi)?;
                elements.push(Element::Pair {
                    tag,
                    ms_mask,
                    ch0,
                    ch1,
                });
            }
            ID_CCE => {
                return Err(Error::Unsupported(
                    "vaco-codec-aac: coupling_channel_element() is not implemented",
                ));
            }
            ID_DSE => skip_data_stream_element(r)?,
            ID_PCE => {
                let pce = ProgramConfigElement::read(r)?;
                elements.push(Element::ProgramConfig(pce));
            }
            ID_FIL => skip_fill_element(r)?,
            ID_END => break,
            _ => unreachable!("id_syn_ele is a 3-bit field, id={id}"),
        }
    }
    r.align();
    r.check()?;
    Ok(elements)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::{Element, read};
    use vaco_bitstream::{BitReader, BitWriter};

    fn minimal_sce_bits(w: &mut BitWriter) {
        w.put(3, super::ID_SCE);
        w.put(4, 0); // tag
        w.put(8, 100); // global_gain
        w.put(1, 0);
        w.put(2, 0); // ONLY_LONG
        w.put(1, 0);
        w.put(6, 1); // max_sfb=1
        w.put(1, 0);
        w.put(4, 0); // ZERO_HCB section
        w.put(5, 1);
        w.put(1, 0); // pulse
        w.put(1, 0); // tns
        w.put(1, 0); // gain_control
    }

    #[test]
    fn an_sce_followed_by_end_decodes_one_element() {
        let mut w = BitWriter::new();
        minimal_sce_bits(&mut w);
        w.put(3, super::ID_END);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let elements = read(&mut r, 4).unwrap();
        assert_eq!(elements.len(), 1);
        assert!(matches!(elements[0], Element::Single { .. }));
    }

    #[test]
    fn a_fill_element_is_skipped_by_its_own_declared_length() {
        let mut w = BitWriter::new();
        w.put(3, super::ID_FIL);
        w.put(4, 3); // cnt = 3 bytes
        w.put(8, 0xaa);
        w.put(8, 0xbb);
        w.put(8, 0xcc);
        w.put(3, super::ID_END);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let elements = read(&mut r, 4).unwrap();
        assert!(elements.is_empty());
    }

    #[test]
    fn sbr_fill_payloads_are_refused_before_pcm_decode() {
        for extension_type in [super::EXT_SBR_DATA, super::EXT_SBR_DATA_CRC] {
            let mut w = BitWriter::new();
            w.put(3, super::ID_FIL);
            w.put(4, 1); // cnt = 1 byte
            w.put(4, extension_type);
            w.put(4, 0); // remaining extension payload bits
            w.put(3, super::ID_END);
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            let error = read(&mut r, 4).unwrap_err();
            assert!(error.to_string().contains("SBR fill payload"));
        }
    }

    #[test]
    fn a_data_stream_element_is_skipped_by_its_own_declared_length() {
        let mut w = BitWriter::new();
        w.put(3, super::ID_DSE);
        w.put(4, 0); // tag
        w.put(1, 0); // align flag
        w.put(8, 2); // cnt = 2 bytes
        w.put(8, 0x11);
        w.put(8, 0x22);
        w.put(3, super::ID_END);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let elements = read(&mut r, 4).unwrap();
        assert!(elements.is_empty());
    }

    #[test]
    fn a_cce_is_refused_rather_than_misread() {
        let mut w = BitWriter::new();
        w.put(3, super::ID_CCE);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(read(&mut r, 4).is_err());
    }

    #[test]
    fn a_stream_that_never_presents_id_end_errors_instead_of_looping_forever() {
        // Regression for a real fuzz-found OOM: `BitReader::get` pads
        // exhausted input with zero bits rather than erroring, so reading
        // `id_syn_ele` with plain `get(3)` saw an endless run of `ID_SCE`
        // (0) once real data ran out, decoding one all-zero SCE after
        // another forever. `try_get` must turn that into a clean error.
        let bytes: Vec<u8> = vec![0u8; 3]; // far too short for even one SCE
        let mut r = BitReader::new(&bytes);
        assert!(read(&mut r, 4).is_err());
    }

    #[test]
    fn byte_alignment_after_end_leaves_the_reader_byte_aligned() {
        let mut w = BitWriter::new();
        minimal_sce_bits(&mut w);
        w.put(3, super::ID_END);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        read(&mut r, 4).unwrap();
        assert!(r.is_aligned());
    }
}
