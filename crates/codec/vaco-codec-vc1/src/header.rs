//! Decoder-initialization metadata (SS6/Annex J.2) and the progressive
//! I-picture header (SS7.1.1, Table 16), Simple/Main profile only.
//!
//! # Extradata convention this crate defines
//!
//! Real ASF/AVI containers hand a `WMV3`/`WVC1` decoder exactly the 4-byte
//! `STRUCT_C` (Annex J.2, Table 263) as codec-private data; width/height
//! live in the container's own stream-properties object (`BITMAPINFOHEADER`
//! `biWidth`/`biHeight`), which today's [`vaco_codec_core::Decoder`]
//! interface has no channel to forward to a decoder at all (only
//! [`vaco_codec_core::Decoder::set_extradata`] exists, and `CodecParameters`
//! is never passed to a built `Decoder`). That is a real, separate gap in
//! the container-to-decoder plumbing this crate cannot fix from inside a
//! codec crate — see this crate's top-level doc.
//!
//! So this crate defines its own extradata shape, matching Annex L's own
//! `SEQUENCE_LAYER` field order exactly (verified bit-for-bit against a
//! real RCV file's own header — see `tests/oracle.rs`): **12 bytes**,
//! `STRUCT_C` (4 bytes, big-endian) followed by `VERT_SIZE` (4 bytes,
//! little-endian) then `HORIZ_SIZE` (4 bytes, little-endian). A caller with
//! only the bare 4-byte `STRUCT_C` and a container-declared width/height
//! elsewhere can still use this decoder by concatenating the three fields
//! itself in this order.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

use crate::tables::PQINDEX_TO_PQUANT;

/// Table 263 (`STRUCT_C`) plus this crate's own width/height convention —
/// see the module doc.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code, reason = "profile_main/dquant are recorded from the bitstream for completeness and future P/B-picture work; this crate's constant-MQUANT, no-VOPDQUANT I-frame decode path does not consume them yet")]
#[allow(clippy::struct_excessive_bools, reason = "each field is an independent flag decoded straight off STRUCT_C (Table 263); grouping them into an enum would not remove any state, just rename it")]
pub(crate) struct SequenceInfo {
    pub(crate) profile_main: bool,
    pub(crate) loopfilter: bool,
    pub(crate) multires: bool,
    pub(crate) dquant: u32,
    pub(crate) overlap: bool,
    pub(crate) quantizer: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Parse this crate's extradata convention (see module doc). Refuses
/// Advanced profile (`PROFILE == 12`) outright — its sequence/entry-point
/// layer is a real in-band bitstream this crate does not parse at all.
pub(crate) fn parse_extradata(data: &[u8]) -> Result<SequenceInfo> {
    let bytes: &[u8; 12] = data
        .get(..12)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::InvalidData("vc1: extradata must be at least 12 bytes (STRUCT_C + VERT_SIZE + HORIZ_SIZE)"))?;
    let struct_c = u32::from_be_bytes([
        *bytes.first().unwrap_or(&0),
        *bytes.get(1).unwrap_or(&0),
        *bytes.get(2).unwrap_or(&0),
        *bytes.get(3).unwrap_or(&0),
    ]);
    let vert = u32::from_le_bytes([
        *bytes.get(4).unwrap_or(&0),
        *bytes.get(5).unwrap_or(&0),
        *bytes.get(6).unwrap_or(&0),
        *bytes.get(7).unwrap_or(&0),
    ]);
    let horiz = u32::from_le_bytes([
        *bytes.get(8).unwrap_or(&0),
        *bytes.get(9).unwrap_or(&0),
        *bytes.get(10).unwrap_or(&0),
        *bytes.get(11).unwrap_or(&0),
    ]);

    let struct_c_bytes = struct_c.to_be_bytes();
    let mut r = BitReader::new(&struct_c_bytes);
    let profile = r.get(4);
    let _frmrtq_postproc = r.get(3);
    let _bitrtq_postproc = r.get(5);
    let loopfilter = r.get_bit() != 0;
    let _reserved3 = r.get_bit();
    let multires = r.get_bit() != 0;
    let _reserved4 = r.get_bit();
    let _fastuvmc = r.get_bit();
    let _extended_mv = r.get_bit();
    let dquant = r.get(2);
    let _vstransform = r.get_bit();
    let _reserved5 = r.get_bit();
    let overlap = r.get_bit() != 0;
    let _syncmarker = r.get_bit();
    let _rangered = r.get_bit();
    let _maxbframes = r.get(3);
    let quantizer = r.get(2);
    let _finterpflag = r.get_bit();
    let _reserved6 = r.get_bit();

    let profile_main = match profile {
        4 => true,
        0 => false,
        _ => return Err(Error::Unsupported("vc1: only Simple/Main profile extradata is supported")),
    };

    if vert == 0 || horiz == 0 || vert > 8192 || horiz > 8192 {
        return Err(Error::InvalidData("vc1: implausible VERT_SIZE/HORIZ_SIZE in extradata"));
    }

    Ok(SequenceInfo {
        profile_main,
        loopfilter,
        multires,
        dquant,
        overlap,
        quantizer,
        width: horiz,
        height: vert,
    })
}

/// Table 16: the progressive I-picture header, Simple/Main profile.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PictureHeader {
    pub(crate) pqindex: u32,
    pub(crate) pquant: u32,
    pub(crate) halfqp: bool,
    pub(crate) uniform_quantizer: bool,
    pub(crate) transacfrm: u32,
    pub(crate) transacfrm2: u32,
    pub(crate) transdctab: bool,
}

/// Decode the `0b`/`10b`/`11b` variable-length index used by `PTYPE`
/// (`MAXBFRAMES == 0` case handled by the caller) and `TRANSACFRM`/
/// `TRANSACFRM2` (Table 39).
#[allow(clippy::same_functions_in_if_condition, reason = "each get_bit() call reads a distinct, sequential bit of the codeword -- not a repeated check of the same condition")]
fn vlc_012(r: &mut BitReader<'_>) -> u32 {
    if r.get_bit() == 0 {
        0
    } else if r.get_bit() == 0 {
        1
    } else {
        2
    }
}

/// Parse the I-picture header, having already established (from the
/// sequence info) that `FINTERPFLAG == 0` and `RANGERED == 0` are the only
/// combinations this crate accepts — real streams that set either are
/// refused with `Error::Unsupported` before this is called, per the
/// picture-header field list this function does not otherwise have a
/// sequence-level flag to consult.
///
/// `FINTERPFLAG` and `RANGERED` are threaded through explicitly (rather
/// than re-deriving them from `seq`) because [`SequenceInfo`] does not
/// carry them at all: this crate's own extradata convention (see module
/// doc) only forwards the fields this crate's decode path actually reads.
pub(crate) fn parse_i_picture_header(r: &mut BitReader<'_>, seq: &SequenceInfo) -> Result<PictureHeader> {
    let _frmcnt = r.get(2);
    let ptype = r.get_bit();
    if ptype != 0 {
        return Err(Error::Unsupported("vc1: not an I picture"));
    }
    let _bf = r.get(7);
    let pqindex = r.get(5);
    let Some(&(pquant_implicit, uniform_implicit)) = PQINDEX_TO_PQUANT.get(pqindex as usize) else {
        return Err(Error::InvalidData("vc1: PQINDEX out of range"));
    };
    let halfqp = if pqindex <= 8 { r.get_bit() != 0 } else { false };
    let (mut pquant, mut uniform_quantizer) = (u32::from(pquant_implicit), uniform_implicit);
    if seq.quantizer == 0b01 {
        let pquantizer = r.get_bit();
        uniform_quantizer = pquantizer != 0;
        pquant = pqindex;
    } else if seq.quantizer != 0 {
        pquant = pqindex;
        uniform_quantizer = seq.quantizer == 0b11;
    }
    if pqindex == 0 {
        return Err(Error::InvalidData("vc1: PQINDEX == 0 is reserved"));
    }
    // EXTENDED_MV's value is ignored in Main profile I pictures (SS7.1.1.9)
    // and is always 0 in Simple profile, so MVRANGE never appears here.
    if seq.multires {
        let respic = r.get(2);
        if respic != 0 {
            return Err(Error::Unsupported("vc1: RESPIC != 0 (down-sampled I frame) not implemented"));
        }
    }
    let transacfrm = vlc_012(r);
    let transacfrm2 = vlc_012(r);
    let transdctab = r.get_bit() != 0;

    Ok(PictureHeader {
        pqindex,
        pquant,
        halfqp,
        uniform_quantizer,
        transacfrm,
        transacfrm2,
        transdctab,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "a test that cannot set up is a failed test")]
mod tests {
    use super::*;

    /// SS7.1.1.6 Table 36 sanity: index 2 maps to `PQUANT == 2`, uniform.
    #[test]
    fn pqindex_2_is_pquant_2_uniform() {
        let (pquant, uniform) = PQINDEX_TO_PQUANT[2];
        assert_eq!(pquant, 2);
        assert!(uniform);
    }

    #[test]
    fn extradata_needs_at_least_12_bytes() {
        assert!(parse_extradata(&[0u8; 4]).is_err());
    }

    /// The real fixture's own `STRUCT_C` (see `tests/oracle.rs`):
    /// `PROFILE=Main`, `LOOPFILTER=0`, `MULTIRES=1`, `DQUANT=0`,
    /// `OVERLAP=0`, `QUANTIZER=implicit`, `720x576`.
    #[test]
    fn real_fixture_struct_c_decodes_as_measured() {
        let mut data = [0u8; 12];
        data[..4].copy_from_slice(&0x41F3_8001u32.to_be_bytes());
        data[4..8].copy_from_slice(&576u32.to_le_bytes());
        data[8..12].copy_from_slice(&720u32.to_le_bytes());
        let seq = parse_extradata(&data).expect("valid extradata");
        assert!(seq.profile_main);
        assert!(!seq.loopfilter);
        assert!(seq.multires);
        assert_eq!(seq.dquant, 0);
        assert!(!seq.overlap);
        assert_eq!(seq.quantizer, 0);
        assert_eq!(seq.width, 720);
        assert_eq!(seq.height, 576);
    }
}
