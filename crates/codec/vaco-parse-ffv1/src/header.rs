//! Reading just enough of `Parameters()` (RFC 9043 §4.2, Figure 28) to name a
//! [`PixFmt`] -- `colorspace_type`, `bits_per_raw_sample`,
//! `log2_h_chroma_subsample`, `log2_v_chroma_subsample` -- and stopping.
//!
//! Every field these four come after in the record's fixed order (`version`,
//! `micro_version`, `coder_type`, and `state_transition_delta` when a custom
//! coder table is signalled) is still decoded, because the range coder has no
//! way to skip to an offset -- it is read and its value used exactly as
//! `Parameters()` needs it, then discarded once the fields this crate reports
//! have been read. Nothing past `log2_v_chroma_subsample` is read at all:
//! `extra_plane`, the slice counts, the quantization table sets, the initial
//! context states, `ec` and `intra` are all still ahead of where parsing
//! stops.

use vaco_pixfmt::PixFmt;

use crate::crc::extradata_crc_ok;
use crate::rangecoder::{RangeDecoder, StateTransition, fresh_states, skip_state_transition_delta};

/// The subset of `Parameters()` this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    colorspace: u32,
    pub(crate) bits_per_raw_sample: u32,
    log2_h_chroma_subsample: u32,
    log2_v_chroma_subsample: u32,
}

impl Header {
    /// Parse a Configuration Record: verify its trailing CRC (RFC 9043
    /// §4.3.2) first, exactly as `vaco-codec-ffv1`'s own `parse_extradata`
    /// does, so a corrupt or truncated record is rejected before any field
    /// read runs -- then read the prefix described in this module's doc.
    ///
    /// # Errors
    ///
    /// A generic parse failure for anything short of a valid, CRC-checked
    /// record: too short, a bad CRC, or a `version`/`coder_type` this reader
    /// does not recognise. The caller ([`crate::Ffv1Parser::set_extradata`])
    /// treats every one of these the same way -- "this record told me
    /// nothing" -- so no error carries detail beyond that.
    pub(crate) fn parse(data: &[u8]) -> Result<Self, ()> {
        if data.len() < 4 || !extradata_crc_ok(data) {
            return Err(());
        }
        let mut dec = RangeDecoder::new(data);
        let mut states = fresh_states();
        let bootstrap = StateTransition::default_table();

        let version = dec.get_symbol(&mut states, &bootstrap, false);
        if !(0..=3).contains(&version) {
            return Err(());
        }
        let version = version as u32;
        if version >= 3 {
            let _micro_version = dec.get_symbol(&mut states, &bootstrap, false);
        }

        let coder_type = dec.get_symbol(&mut states, &bootstrap, false);
        if !(0..=2).contains(&coder_type) {
            return Err(());
        }
        if coder_type > 1 {
            // Custom transition table: 255 further symbols to consume so the
            // fields below stay correctly aligned. The table itself is never
            // applied to anything here -- see this module's doc.
            skip_state_transition_delta(&mut dec, &bootstrap);
        }

        let colorspace = dec.get_symbol(&mut states, &bootstrap, false);
        if colorspace < 0 {
            return Err(());
        }
        let bits_per_raw_sample = if version >= 1 {
            let v = dec.get_symbol(&mut states, &bootstrap, false);
            if v <= 0 { 8 } else { v as u32 }
        } else {
            8
        };
        let _chroma_planes = get_flag(&mut dec, &mut states, &bootstrap);
        let log2_h_chroma_subsample = dec.get_symbol(&mut states, &bootstrap, false);
        let log2_v_chroma_subsample = dec.get_symbol(&mut states, &bootstrap, false);
        if log2_h_chroma_subsample < 0 || log2_v_chroma_subsample < 0 {
            return Err(());
        }

        Ok(Self {
            colorspace: colorspace as u32,
            bits_per_raw_sample,
            log2_h_chroma_subsample: log2_h_chroma_subsample as u32,
            log2_v_chroma_subsample: log2_v_chroma_subsample as u32,
        })
    }

    /// The [`PixFmt`] this header names, mirroring
    /// `vaco-codec-ffv1::codec::format_for` exactly -- the same four
    /// combinations that crate's own decoder covers, `None` for anything
    /// else, since reporting a format this workspace's own FFV1 decoder
    /// cannot actually produce would be worse than reporting nothing.
    #[must_use]
    pub(crate) fn pix_fmt(&self) -> Option<PixFmt> {
        match (
            self.colorspace,
            self.log2_h_chroma_subsample,
            self.log2_v_chroma_subsample,
        ) {
            (0, 1, 1) => Some(PixFmt::Yuv420p),
            (0, 1, 0) => Some(PixFmt::Yuv422p),
            (0, 0, 0) => Some(PixFmt::Yuv444p),
            (1, 0, 0) => Some(PixFmt::Gbrp),
            _ => None,
        }
    }
}

fn get_flag(
    dec: &mut RangeDecoder<'_>,
    states: &mut crate::rangecoder::SymbolStates,
    table: &StateTransition,
) -> bool {
    let mut s0 = states.first().copied().unwrap_or(128);
    let bit = dec.get_rac(&mut s0, table);
    if let Some(slot) = states.first_mut() {
        *slot = s0;
    }
    bit
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code exercising the parser, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;

    /// A real `ffmpeg 9.0.1`-encoded FFV1 Configuration Record, extracted
    /// directly from a `yuv420p` source's Matroska `CodecPrivate` (`0x63A2`)
    /// by hand-walking the file's EBML structure -- not synthesised, since
    /// this record is range-coded and there is no fixed byte offset to
    /// construct one at by hand.
    const REAL_YUV420P_RECORD: [u8; 42] = [
        0x56, 0x2b, 0x84, 0xd1, 0x9c, 0x05, 0x2f, 0x41, 0x3c, 0x60, 0x26, 0xe9, 0x5c, 0x37, 0x6f,
        0x5d, 0x1b, 0x76, 0x97, 0x9d, 0x3a, 0xc9, 0xc4, 0x20, 0x43, 0x1e, 0x8b, 0x9f, 0x55, 0x20,
        0x51, 0x2f, 0x4e, 0xf8, 0xa1, 0x68, 0x3b, 0x9b, 0x17, 0x13, 0x7c, 0x03,
    ];

    #[test]
    fn a_real_ffmpeg_yuv420p_record_parses_to_yuv420p() {
        let header = Header::parse(&REAL_YUV420P_RECORD).expect("valid record");
        assert_eq!(header.pix_fmt(), Some(PixFmt::Yuv420p));
        assert_eq!(header.bits_per_raw_sample, 8);
    }

    #[test]
    fn a_bad_crc_is_rejected() {
        let mut broken = REAL_YUV420P_RECORD;
        if let Some(b) = broken.first_mut() {
            *b ^= 0xFF;
        }
        assert!(Header::parse(&broken).is_err());
    }

    #[test]
    fn too_short_to_hold_a_crc_is_rejected() {
        assert!(Header::parse(&[0, 1, 2]).is_err());
    }
}
