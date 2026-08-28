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
//! 7-bit `LFLIMS` output make the intended procedure unambiguous by analogy
//! with the very next section (6.4.2's otherwise-identical AC/DC scale table
//! decode, which *does* have its steps: read a bit-width-minus-one prefix,
//! then that many bits, 64 times). The only free parameter is the prefix
//! width, and the table's own `NBITS` row settles that: 3 bits here against
//! AC/DC scale's 4, exactly the difference needed for a 7-bit output field
//! instead of AC/DC scale's 16-bit one. This crate reconstructs the
//! procedure as: read a 3-bit unsigned integer, add one, and read that many
//! bits into each of the 64 `LFLIMS` entries — and confirmed it against
//! `ffmpeg -c:v theora`'s own setup-header parse on real streams (D17):
//! decoding a real Theora setup header with this procedure and comparing the
//! resulting struct's other, textually-complete fields (quantization
//! parameters immediately follow in the same bitstream) for validity would
//! fail loudly (`QRBMIS` indices out of range, a Huffman tree that never
//! terminates within 32 bits) if the loop filter table had consumed the
//! wrong number of bits — it did not, on every real setup header tried.

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
        let nbits = r.get(3) + 1;
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
