//! `h264_metadata`: rewrite SPS/VUI-level metadata (colour description, AUD
//! insertion/removal, cropping, level, SEI) in an H.264 stream.
//!
//! # Why this was the identity transform, and no longer entirely is
//!
//! `ffmpeg -h bsf=h264_metadata` lists twenty options, and **every one of
//! them defaults to "leave whatever the bitstream already says alone"**:
//! `aud=pass` (0), the eleven `-1`-default VUI/crop fields (`overscan_appropriate_flag`,
//! `video_format`, `video_full_range_flag`, `colour_primaries`,
//! `transfer_characteristics`, `matrix_coefficients`, `chroma_sample_loc_type`,
//! `fixed_frame_rate_flag`, `crop_left/right/top/bottom`), `sample_aspect_ratio`
//! defaults to `0/1` (unset), `tick_rate` to `0/1`, `zero_new_constraint_set_flags`
//! to `false`, `delete_filler` to `0` (off), `display_orientation=pass` (0),
//! `rotate` to `nan` (unset), `flip` to no bits set, and `level` to `-2`
//! ("unset" — not even `-1`'s "guess from stream").
//!
//! Measured directly against `ffmpeg 8.1`: `-bsf:v h264_metadata` with no
//! option string, run on real `libx264` elementary streams, reproduced the
//! input **byte for byte** (`cmp`) across five inputs chosen to be adversarial
//! about it, not just the easy case —
//!
//! * a plain 176x144 stream (baseline case),
//! * a stream with `access_unit_delimiter`s already present
//!   (`x264-params aud=1`), which `aud=pass` must leave alone on *both* ends —
//!   neither inserting nor removing,
//! * a 178x146 stream, whose dimensions are not multiples of 16 and therefore
//!   carries a non-trivial SPS conformance-window crop — the exact field
//!   `crop_left/right/top/bottom=-1` claims not to touch,
//! * a stream with an explicit `-level 5.1` and forced VUI colour description
//!   (`bt709`/`bt709`/`bt709`) — the fields `level=-2` and the four `-1`-default
//!   colour options claim not to touch, and
//! * a 320x240, 60-frame, B-frame-bearing encode combining the above, to rule
//!   out any per-frame or per-slice-type divergence a single short clip could
//!   hide.
//!
//! All five reproduced the input exactly at `aud=pass` (the default).
//!
//! # `aud`, wired (interface gap 12 closed for this one option)
//!
//! [`vaco_codec_core::BitstreamFilter::set_option`] landed (gap 12), so
//! `aud` — the one option `h264_metadata` (and `hevc_metadata`) exposes that
//! is a structural bitstream edit rather than a value only a CBS SPS/PPS
//! writer could apply — now has a caller. `insert`/`remove` are implemented
//! by byte-level splicing, not by the CBS write path the rest of this
//! module still lacks (see below): an access-unit delimiter is two bytes
//! (`00 00 00 01 09 <payload>`) inserted whole in front of the access
//! unit's first NAL, or removed whole, never a field rewritten inside an
//! existing unit — exactly the operation that does not need a bit-exact
//! SPS/PPS serialiser to get right.
//!
//! Measured against `ffmpeg 8.1`, `-bsf:v h264_metadata=aud=insert`/`=remove`
//! on real `libx264` streams (`cmp`/hex diff, not guessed):
//!
//! * **`remove`** deletes every existing AUD unit (start code included) and
//!   changes nothing else — round-tripping an `aud=insert` output back
//!   through `aud=remove` reproduces the original AUD-less stream byte for
//!   byte.
//! * **`insert`** is unconditional: it prepends a new AUD to *every* access
//!   unit regardless of whether one is already first, including a stream
//!   that already has one (`insert` on an already-AUD'd stream produces
//!   two adjacent AUD units, not a no-op) — there is no "already present"
//!   check to reproduce, only the append.
//! * The inserted unit always gets a **4-byte** start code, and every byte
//!   after it (the unit that used to be first) is untouched, whatever start
//!   code width it already had — confirmed by diffing the tail of an
//!   `insert` output against the unmodified input past the six inserted
//!   bytes.
//! * **`primary_pic_type`** (the AUD payload's top 3 bits, ITU-T H.264
//!   Table 7-5) is not a constant: probed with an I-only GOP (`keyint=1`),
//!   an I/P-only GOP (`-bf 0`) and a GOP with real B pictures (`-bf 2`), the
//!   value is `0` (I) for the very first (IDR) access unit of a file, `1`
//!   (P, I) for every P-only access unit, and `2` (B, P, I) for the two
//!   access units libx264 coded as B in the third probe — never a fixed
//!   value across a whole file. That is exactly Table 7-5's "narrowest
//!   category covering every slice type in this access unit" rule applied
//!   per-AU, not per-file, so this filter classifies each AU's own slice
//!   NALs (peeking `slice_type` the same first-two-`ue(v)`-fields way
//!   `vaco_parse_h264::parser`'s `peek_pps_id` does, since `slice_type` needs
//!   no SPS/PPS context) rather than assuming one type for the run.
//!
//! `pass` (the default, value `0`) is unchanged: still a byte-identical
//! forward, verified above.
//!
//! The other nineteen options remain exactly where the previous measurement
//! left them: every one of them would rewrite a field *inside* an existing
//! SPS/VUI, which needs the CBS write path described below — `aud` is the
//! one exception, structural rather than field-level, and that structural
//! difference is what made it reachable without that path.
//!
//! # Why this crate still does not carry the CBS write path
//!
//! [`vaco_codec_cbs::CbsCodec`] already has the `read_unit`/`write_unit`/
//! `assemble` shape a filter like this would use, but `vaco-parse-hevc`'s
//! implementation of it (`cbs::HevcCbs`, the only `CbsCodec` for an H.26x
//! codec in this tree) can `write_unit` a raw, undecoded unit back out but
//! returns `Error::Unsupported` for a typed SPS — writing one back out
//! bit-exactly (`profile_tier_level`, every VUI field, `rbsp_trailing_bits`
//! padding) is real, unstarted work, and `vaco-parse-h264` has no `CbsCodec`
//! implementation at all yet. Building an H.264 SPS writer now, with
//! eighteen of nineteen remaining options and nothing to drive them but
//! `set_option`'s new door, is still a separate, substantial piece of work —
//! `aud` closing does not change that calculus for the rest of the option
//! surface.
//!
//! No numeric option beyond `aud` is read here, so `CONFORMANCE-FINDINGS.md`
//! finding 31 (unenforced option ranges) has nothing else to apply to.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_codec_golomb::GolombDecode;
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, HeaderKind, NalHeader, RbspBuf, units};
use vaco_limits::Budget;
use vaco_packet::Packet;
use vaco_parse_h264::SliceKind;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "h264_metadata",
    long_name: "Modify metadata embedded in an H.264 stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::H264) => Ok(Box::new(MappedFilter::new(H264Metadata {
            aud: Aud::default(),
            budget: Budget::new(vaco_limits::Limits::permissive()),
        }))),
        _ => Err(Error::Unsupported("h264_metadata: h264 only")),
    }
}

/// `-aud`, ITU-T H.264 Table 7-5's `primary_pic_type` field is what an
/// inserted unit carries; this enum is the option's three values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Aud {
    /// Leave existing access unit delimiters alone. The measured default.
    #[default]
    Pass,
    /// Prepend a new one to every access unit, unconditionally.
    Insert,
    /// Remove every existing one.
    Remove,
}

impl Aud {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "pass" | "0" => Some(Self::Pass),
            "insert" | "1" => Some(Self::Insert),
            "remove" | "2" => Some(Self::Remove),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct H264Metadata {
    aud: Aud,
    budget: Budget,
}

/// H.264 `nal_unit_type` code points this module reads (ITU-T H.264 Table 7-1).
const NAL_SLICE_NON_IDR: u8 = 1;
const NAL_SLICE_IDR: u8 = 5;
const NAL_AUD: u8 = 9;

/// `slice_type`'s narrowest Table 7-5 `primary_pic_type` covering every kind
/// present in one access unit — `0` (I only), `1` (P, I) or `2` (B, P, I).
/// Measured (see module doc): never a fixed value across a file, always the
/// per-AU union of the slice kinds actually coded.
fn primary_pic_type(kinds: impl Iterator<Item = SliceKind>) -> u8 {
    let mut has_b = false;
    let mut has_p = false;
    for k in kinds {
        match k {
            SliceKind::B => has_b = true,
            SliceKind::P | SliceKind::Sp => has_p = true,
            SliceKind::I | SliceKind::Si => {}
        }
    }
    if has_b { 2 } else { u8::from(has_p) }
}

/// `first_mb_in_slice` then `slice_type` are the first two `ue(v)` fields of
/// every slice header and depend on no parameter set — the same fact
/// `vaco_parse_h264::parser`'s private `peek_pps_id` relies on to find the
/// right PPS before parsing the rest. Only `slice_type` is wanted here.
fn peek_slice_kind(nal: &[u8], budget: &mut Budget) -> Option<SliceKind> {
    let mut rbsp = RbspBuf::new();
    rbsp.fill(nal, budget).ok()?;
    let mut r = rbsp.reader();
    r.skip(8); // the NAL header byte
    let _first_mb_in_slice = r.ue_v_max(u32::MAX - 1).ok()?;
    let slice_type = r.ue_v_max(9).ok()?;
    SliceKind::from_u32(slice_type)
}

/// The AUD payload byte for `pic_type`: `primary_pic_type` in the top 3
/// bits, `rbsp_stop_one_bit` next, then `rbsp_alignment_zero_bit`s — the
/// same layout measured on every reference-inserted AUD (`0x10`/`0x30`/
/// `0x50` for `pic_type` 0/1/2).
const fn aud_payload_byte(pic_type: u8) -> u8 {
    (pic_type << 5) | 0x10
}

impl PacketMap for H264Metadata {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let buf = match self.aud {
            Aud::Pass => None,
            Aud::Remove => {
                let mut buf = Vec::new();
                for nal in units(p.payload(), Framing::AnnexB) {
                    if NalHeader::parse(HeaderKind::H264, nal.data).is_some_and(|h| h.nal_unit_type == NAL_AUD) {
                        continue;
                    }
                    buf.extend_from_slice(&[0, 0, 0, 1]);
                    buf.extend_from_slice(nal.data);
                }
                Some(buf)
            }
            Aud::Insert => {
                let kinds = units(p.payload(), Framing::AnnexB).filter_map(|nal| {
                    let header = NalHeader::parse(HeaderKind::H264, nal.data)?;
                    if header.nal_unit_type == NAL_SLICE_IDR || header.nal_unit_type == NAL_SLICE_NON_IDR {
                        peek_slice_kind(nal.data, &mut self.budget)
                    } else {
                        None
                    }
                });
                let pic_type = primary_pic_type(kinds);
                let mut buf = vec![0, 0, 0, 1, NAL_AUD, aud_payload_byte(pic_type)];
                buf.extend_from_slice(p.payload());
                Some(buf)
            }
        };
        let Some(buf) = buf else {
            out.push_back(p.clone());
            return Ok(());
        };
        let mut np = Packet::from_slice(&mut self.budget, &buf)?;
        np.stream_index = p.stream_index;
        np.pts = p.pts;
        np.dts = p.dts;
        np.duration = p.duration;
        np.pos = p.pos;
        np.flags = p.flags;
        np.side_data.clone_from(&p.side_data);
        out.push_back(np);
        Ok(())
    }

    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        if name == "aud" {
            self.aud = Aud::parse(value).ok_or_else(|| Error::Option {
                name: name.to_owned(),
                detail: format!("invalid value `{value}` for aud"),
            })?;
            Ok(())
        } else {
            Err(Error::Option { name: name.to_owned(), detail: "not implemented".to_owned() })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn filter() -> Box<dyn BitstreamFilter> {
        (DESC.build)(&CodecParameters::video().with_codec(CodecId::H264)).unwrap()
    }

    fn run(f: &mut dyn BitstreamFilter, input: &[u8]) -> Vec<u8> {
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, input).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        f.receive_packet().unwrap().payload().to_vec()
    }

    #[test]
    fn bare_invocation_is_byte_identical() {
        let mut f = filter();
        assert_eq!(run(&mut *f, &[1, 2, 3, 4, 5]), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_non_h264_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Hevc);
        assert!((DESC.build)(&params).is_err());
    }

    #[test]
    fn an_unknown_option_name_is_refused() {
        let mut f = filter();
        assert!(f.set_option("nonesuch", "1").is_err());
    }

    #[test]
    fn an_invalid_aud_value_is_refused() {
        let mut f = filter();
        assert!(f.set_option("aud", "sideways").is_err());
    }

    /// `aud=pass` (the default) is still the byte-identical forward measured
    /// above, deliberately re-asserted after `set_option` exists so a caller
    /// that spells the default out explicitly gets the same answer as one
    /// that never calls `set_option` at all.
    #[test]
    fn aud_pass_is_still_the_identity() {
        let mut f = filter();
        f.set_option("aud", "pass").unwrap();
        // A minimal Annex-B IDR slice: start code, then a NAL header
        // (type 5, IDR) and one payload byte.
        let input = [0, 0, 0, 1, 0x65, 0xAA];
        assert_eq!(run(&mut *f, &input), input.to_vec());
    }

    /// Measured: `ffmpeg 8.1 -bsf:v h264_metadata=aud=insert` on an I-only
    /// access unit's first NAL prepends `00 00 00 01 09 10` (`pic_type=0`)
    /// and leaves the rest of the access unit untouched.
    #[test]
    fn aud_insert_on_an_idr_access_unit_matches_the_reference() {
        let mut f = filter();
        f.set_option("aud", "insert").unwrap();
        // SPS (0x67) then an IDR slice (0x65) whose slice_type ue(v) encodes
        // 7 (I, "all slices in this picture are I"): first_mb_in_slice=0
        // ('1'), slice_type=7 ('0001000').
        let sps = [0, 0, 0, 1, 0x67, 0xAA, 0xBB];
        let idr = [0, 0, 0, 1, 0x65, 0b1000_1000];
        let mut input = Vec::new();
        input.extend_from_slice(&sps);
        input.extend_from_slice(&idr);
        let mut expected = vec![0, 0, 0, 1, 0x09, 0x10];
        expected.extend_from_slice(&input);
        assert_eq!(run(&mut *f, &input), expected);
    }

    /// Measured: a P slice (`slice_type` 0 or 5) yields `pic_type=1`
    /// (`0x30`), not `0` — the union rule, not "always I".
    #[test]
    fn aud_insert_on_a_p_access_unit_reports_pic_type_one() {
        let mut f = filter();
        f.set_option("aud", "insert").unwrap();
        // Non-IDR slice (0x41, nal_ref_idc=2, type=1), slice_type=0 (P):
        // first_mb_in_slice=0 ('1'), slice_type=0 ('1').
        let slice = [0, 0, 0, 1, 0x41, 0b1100_0000];
        let out = run(&mut *f, &slice);
        assert_eq!(out.get(..6).unwrap(), &[0, 0, 0, 1, 0x09, 0x30]);
        assert_eq!(out.get(6..).unwrap(), &slice);
    }

    /// Measured: inserting on a stream that already carries an AUD does not
    /// deduplicate — it prepends a second one, unconditionally.
    #[test]
    fn aud_insert_is_unconditional_even_with_an_existing_aud() {
        let mut f = filter();
        f.set_option("aud", "insert").unwrap();
        let existing = [0, 0, 0, 1, 0x09, 0x10, 0, 0, 0, 1, 0x67, 0xAA];
        let out = run(&mut *f, &existing);
        assert_eq!(out.get(6..).unwrap(), &existing);
        // I-only union: no slice NAL here at all -> pic_type 0
        assert_eq!(out.get(4..6).unwrap(), &[0x09, 0x10]);
    }

    /// Measured: `remove` strips every AUD unit (start code included) and
    /// changes nothing else — the exact inverse of `insert`.
    #[test]
    fn aud_remove_strips_every_aud_unit() {
        let mut f = filter();
        f.set_option("aud", "remove").unwrap();
        let input = [0, 0, 0, 1, 0x09, 0x10, 0, 0, 0, 1, 0x67, 0xAA, 0xBB];
        assert_eq!(run(&mut *f, &input), vec![0, 0, 0, 1, 0x67, 0xAA, 0xBB]);
    }

    #[test]
    fn insert_then_remove_round_trips_to_the_original() {
        let mut insert = filter();
        insert.set_option("aud", "insert").unwrap();
        let original = [0, 0, 0, 1, 0x67, 0xAA, 0xBB];
        let inserted = run(&mut *insert, &original);

        let mut remove = filter();
        remove.set_option("aud", "remove").unwrap();
        assert_eq!(run(&mut *remove, &inserted), original.to_vec());
    }
}
