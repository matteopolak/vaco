//! Setup header (`Vaco-Spec-Ref: theora-spec-20170603 section 6.4`): loop
//! filter limits, quantization parameters, and the 80 DCT token Huffman
//! tables.
//!
//! # The loop filter limit table's decode procedure is missing from the spec
//!
//! Section 6.4.1 ("Loop Filter Limit Table Decode") ends its prose at "It is
//! decoded as follows:" and the numbered steps that should follow are simply
//! absent from the published PDF — the text jumps straight to that section's
//! "VP3 Compatibility" box and then into section 6.4.2. This was confirmed by
//! rendering the actual page images (page 50-51 of the PDF), not just the
//! text extraction: the steps are not merely a text-layer extraction
//! artifact, they are not on the page at all. This is a gap in the primary
//! source itself, not a transcription error on this crate's part.
//!
//! The table's own variable list (`qi`: 6 bits, `NBITS`: 3 bits) and its
//! 7-bit `LFLIMS` output make the shape of the procedure unambiguous by
//! analogy with the very next section (6.4.2's otherwise-identical AC/DC
//! scale table decode, which *does* have its steps: read a prefix giving a
//! bit width, then read that many bits, 64 times). The one thing analogy
//! alone could not settle is whether the 3-bit prefix is used directly or
//! read-plus-one, the way AC/DC scale's 4-bit prefix is (spec text,
//! section 6.4.2 steps 1/3: "read a 4-bit unsigned integer... assign NBITS
//! the value read, plus one"). This crate first assumed the same
//! read-plus-one convention here, by analogy — **that guess was wrong**,
//! caught only once a real encoded file was decoded end to end (see
//! `Vaco-Spec-Ref` below): it desynchronised the bitstream immediately
//! after the loop filter table, producing a nonsensical `NBMS` (number of
//! base matrices) of 356 against a setup header barely 2600 bytes long.
//!
//! The actual convention, confirmed by exhaustively searching every
//! candidate bit-length for the loop filter section against a real
//! `ffmpeg`-encoded setup header (`bear.ogv`, `ffmpeg` FATE suite) until the
//! *entire rest* of the header — AC/DC scale, the base matrix table, all
//! six quant-range chains summing to exactly 63, and all 80 Huffman trees —
//! decoded cleanly down to the packet's own byte-alignment padding: the
//! 3-bit prefix is used **directly, with no plus-one**, unlike AC/DC
//! scale's 4-bit one. In hindsight this is the more consistent reading of
//! the two output field widths, not an arbitrary special case: AC/DC
//! scale's registers are 16 bits wide, so a 4-bit prefix needs the
//! plus-one to ever reach a width of 16 (a bare 4-bit field maxes out at
//! 15); `LFLIMS` is a 7-bit register, and a bare 3-bit prefix already
//! reaches exactly 7 with no offset needed. Only one candidate bit-length
//! out of several hundred tried made the rest of the header parse validly
//! at all (`crates/codec/vaco-codec-theora/tests/oracle.rs` and its
//! `bear.ogv` fixture), and it is the one this convention predicts.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

use crate::huffman::{self, HuffTable};
use crate::quant::QuantParams;

pub(crate) const IDENT_MAGIC: u8 = 0x80;
pub(crate) const COMMENT_MAGIC: u8 = 0x81;
pub(crate) const SETUP_MAGIC: u8 = 0x82;
pub(crate) const THEORA_TAG: &[u8; 6] = b"theora";

/// Everything decoded from the setup header that frame decode needs.
#[derive(Debug, Clone)]
pub(crate) struct Setup {
    pub lflims: [u32; 64],
    pub quant: QuantParams,
    pub tables: Box<[HuffTable; 80]>,
}

impl Setup {
    /// Parse the body of a setup header packet (everything after the common
    /// `\x82theora` prologue).
    pub(crate) fn parse(body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body);

        // Section 6.4.1 (reconstructed procedure; see module doc).
        let mut lflims = [0u32; 64];
        let nbits = r.get(3);
        for v in &mut lflims {
            *v = r.get(nbits);
        }

        let quant = QuantParams::parse(&mut r)?;
        let tables = huffman::parse_tables(&mut r);

        r.check()
            .map_err(|_| Error::InvalidData("theora: truncated setup header"))?;

        Ok(Self {
            lflims,
            quant,
            tables,
        })
    }
}

/// Check the common header prologue (section 6.1) and return the header
/// type byte, or `None` if this is not a Theora header packet at all.
pub(crate) fn common_header_type(packet: &[u8]) -> Option<u8> {
    let (&first, rest) = packet.split_first()?;
    if first & 0x80 == 0 {
        return None;
    }
    let tag = rest.get(..6)?;
    if tag != THEORA_TAG {
        return None;
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_header_type_reads_the_tag() {
        let mut pkt = vec![0x80];
        pkt.extend_from_slice(THEORA_TAG);
        pkt.push(3); // vmaj
        assert_eq!(common_header_type(&pkt), Some(0x80));
    }

    #[test]
    fn common_header_type_rejects_wrong_tag() {
        let mut pkt = vec![0x80];
        pkt.extend_from_slice(b"vorbis");
        assert_eq!(common_header_type(&pkt), None);
    }

    #[test]
    fn common_header_type_rejects_data_packets() {
        // MSB clear: this is a coded frame, not a header.
        let pkt = vec![0x00, 0, 0, 0];
        assert_eq!(common_header_type(&pkt), None);
    }
}
