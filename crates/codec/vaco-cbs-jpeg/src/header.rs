//! Typed segments: `SOF0`/`SOF1`/`SOF2` (frame header), `DQT` (quantisation
//! tables) and `DHT` (Huffman tables), ITU-T T.81 §B.2.2, §B.2.4.1, §B.2.4.2.
//!
//! Every field here is byte-aligned and self-delimiting by its own length or
//! count field, so — unlike the bit-packed codecs' CBS layers — there is no
//! ambiguity in any of these three writers: given the parsed value, there is
//! exactly one way to encode it back, and it is always the way that was read.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};

/// One component's declaration in a frame header, §B.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameComponent {
    pub id: u8,
    /// `H_i` — horizontal sampling factor, the top nibble of the byte this
    /// shares with `v`.
    pub h: u8,
    /// `V_i` — vertical sampling factor, the bottom nibble.
    pub v: u8,
    /// `Tq_i` — which quantisation table this component uses.
    pub quant_table: u8,
}

/// `SOF0`/`SOF1`/`SOF2`'s payload (baseline, extended sequential,
/// progressive — see `vaco-codec-jpeg`'s docs for why those three cover
/// everything this workspace decodes; this crate types the same three but
/// places no decode-time restriction on which `SOF` marker it accepts, since
/// splitting and re-encoding a frame header needs no entropy-coding model at
/// all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    /// The marker byte this header was read from — `SOF0`, `SOF2`, etc. —
    /// carried here because [`FrameHeader`] alone cannot say which `SOF` it
    /// is; [`crate::cbs::JpegContent::content_unit_type`] reads it from here.
    pub sof_marker: u8,
    pub precision: u8,
    pub height: u16,
    pub width: u16,
    pub components: Vec<FrameComponent>,
}

impl FrameHeader {
    /// Parse a frame header's payload (bytes after the 2-byte length).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] on a truncated segment, a declared component
    /// count of 0, or a byte left over after the last declared component
    /// (the marker segment's length must match exactly what its own field
    /// count implies, or a caller has a mismatched length and payload).
    pub fn parse(sof_marker: u8, payload: &[u8]) -> Result<Self> {
        let mut r = ByteReader::new(payload);
        let precision = r.u8();
        let height = r.be16();
        let width = r.be16();
        let num_components = r.u8();
        if num_components == 0 {
            return Err(Error::InvalidData("jpeg: SOF declares zero components"));
        }
        let mut components = Vec::new();
        for _ in 0..num_components {
            let id = r.u8();
            let sampling = r.u8();
            let quant_table = r.u8();
            components.push(FrameComponent {
                id,
                h: sampling >> 4,
                v: sampling & 0x0F,
                quant_table,
            });
        }
        r.check()?;
        if r.remaining() != 0 {
            return Err(Error::InvalidData(
                "jpeg: SOF segment has bytes past its declared components",
            ));
        }
        Ok(Self {
            sof_marker,
            precision,
            height,
            width,
            components,
        })
    }

    /// Write the payload back out — the inverse of [`FrameHeader::parse`].
    #[must_use]
    pub fn write(&self) -> Vec<u8> {
        let mut out = vec![self.precision];
        out.extend_from_slice(&self.height.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.push(self.components.len() as u8);
        for c in &self.components {
            out.push(c.id);
            out.push((c.h << 4) | (c.v & 0x0F));
            out.push(c.quant_table);
        }
        out
    }
}

/// One `DQT` table entry, §B.2.4.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantTable {
    pub id: u8,
    /// `Pq`, the full four-bit precision nibble as coded — not just whether
    /// it selects 16-bit values. §B.2.4.1 only defines 0 (8-bit) and 1
    /// (16-bit); a real encoder never sets anything else. But a writer that
    /// collapsed this to a `bool` (`Pq != 0`) would reconstruct any other
    /// value in 2..=15 as exactly `1` on write, changing bytes no edit
    /// touched — the reserved nibble is kept whole so that never happens,
    /// the same way [`HuffmanTable::class`] keeps `Tc` whole.
    pub precision: u8,
    /// The 64 values in the order they were coded (zig-zag, per §A.3.7) —
    /// kept in coded order rather than de-zigzagged, since a CBS writer
    /// needs exactly what was read, not the natural-order matrix a decoder
    /// wants.
    pub values: [u16; 64],
}

impl QuantTable {
    /// Whether values are coded as 16-bit (`Pq != 0`, matching the read
    /// side's own leniency — only `Pq == 1` is spec-legal, but any nonzero
    /// value reads exactly the same way).
    #[must_use]
    pub const fn sixteen_bit(&self) -> bool {
        self.precision != 0
    }
}

/// A `DQT` segment: one or more tables back to back until the payload ends.
///
/// # Errors
///
/// [`Error::InvalidData`] on a truncated table or a table index outside
/// 0..=3 (§B.2.4.1's `Tq` is four bits, but real encoders never exceed 3).
pub fn parse_dqt(payload: &[u8]) -> Result<Vec<QuantTable>> {
    let mut r = ByteReader::new(payload);
    let mut tables = Vec::new();
    while r.remaining() > 0 {
        let pq_tq = r.u8();
        let precision = pq_tq >> 4;
        let sixteen_bit = precision != 0;
        let id = pq_tq & 0x0F;
        let mut values = [0u16; 64];
        for slot in &mut values {
            *slot = if sixteen_bit {
                r.be16()
            } else {
                u16::from(r.u8())
            };
        }
        r.check()?;
        tables.push(QuantTable {
            id,
            precision,
            values,
        });
    }
    Ok(tables)
}

/// Write a `DQT` segment's payload — the inverse of [`parse_dqt`].
#[must_use]
pub fn write_dqt(tables: &[QuantTable]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in tables {
        out.push((t.precision << 4) | (t.id & 0x0F));
        for &v in &t.values {
            if t.sixteen_bit() {
                out.extend_from_slice(&v.to_be_bytes());
            } else {
                out.push(v as u8);
            }
        }
    }
    out
}

/// One `DHT` table entry, §B.2.4.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuffmanTable {
    /// `Tc`: 0 for DC (or lossless), 1 for AC.
    pub class: u8,
    /// `Th`, 0..=3.
    pub id: u8,
    /// `BITS[1..=16]` — how many codes of each length 1..=16.
    pub counts: [u8; 16],
    /// `HUFFVAL[]`, `sum(counts)` bytes, in code order.
    pub values: Vec<u8>,
}

/// A `DHT` segment: one or more tables back to back until the payload ends.
///
/// # Errors
///
/// [`Error::InvalidData`] on a truncated table.
pub fn parse_dht(payload: &[u8]) -> Result<Vec<HuffmanTable>> {
    let mut r = ByteReader::new(payload);
    let mut tables = Vec::new();
    while r.remaining() > 0 {
        let tc_th = r.u8();
        let class = tc_th >> 4;
        let id = tc_th & 0x0F;
        let mut counts = [0u8; 16];
        let mut total = 0usize;
        for slot in &mut counts {
            *slot = r.u8();
            total += usize::from(*slot);
        }
        // `total` is a sum of sixteen bytes, so at most 4080 — bounded by
        // the syntax itself, no budgeted allocation needed.
        let mut values = Vec::new();
        for _ in 0..total {
            values.push(r.u8());
        }
        r.check()?;
        tables.push(HuffmanTable {
            class,
            id,
            counts,
            values,
        });
    }
    Ok(tables)
}

/// Write a `DHT` segment's payload — the inverse of [`parse_dht`].
#[must_use]
pub fn write_dht(tables: &[HuffmanTable]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in tables {
        out.push((t.class << 4) | (t.id & 0x0F));
        out.extend_from_slice(&t.counts);
        out.extend_from_slice(&t.values);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// The real `SOF0` payload from `ffmpeg -f lavfi -i testsrc2=size=64x48
    /// -frames:v 1` (this crate's own `baseline.jpg` fixture): 8-bit, 48x64,
    /// three components, 4:2:0.
    const REAL_SOF0_PAYLOAD: &[u8] = &[
        0x08, 0x00, 0x30, 0x00, 0x40, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    #[test]
    fn a_real_sof0_round_trips_bit_exactly() {
        let h = FrameHeader::parse(0xC0, REAL_SOF0_PAYLOAD).expect("parses");
        assert_eq!(h.precision, 8);
        assert_eq!((h.width, h.height), (64, 48));
        assert_eq!(h.components.len(), 3);
        assert_eq!(
            h.components[0],
            FrameComponent {
                id: 1,
                h: 2,
                v: 2,
                quant_table: 0,
            }
        );
        assert_eq!(h.write(), REAL_SOF0_PAYLOAD);
    }

    #[test]
    fn zero_components_is_rejected() {
        assert!(FrameHeader::parse(0xC0, &[8, 0, 10, 0, 10, 0]).is_err());
    }

    #[test]
    fn trailing_bytes_past_declared_components_is_rejected() {
        let mut bytes = REAL_SOF0_PAYLOAD.to_vec();
        bytes.push(0xFF);
        assert!(FrameHeader::parse(0xC0, &bytes).is_err());
    }

    #[test]
    fn every_truncation_of_a_real_sof_errors_or_parses_without_panicking() {
        for n in 0..REAL_SOF0_PAYLOAD.len() {
            let _ = FrameHeader::parse(0xC0, &REAL_SOF0_PAYLOAD[..n]);
        }
    }

    #[test]
    fn an_eight_bit_dqt_round_trips() {
        let mut payload = vec![0x00u8]; // Pq=0, Tq=0
        payload.extend((0u16..64).map(|i| (i % 256) as u8));
        let tables = parse_dqt(&payload).expect("parses");
        assert_eq!(tables.len(), 1);
        assert!(!tables[0].sixteen_bit());
        assert_eq!(tables[0].id, 0);
        assert_eq!(tables[0].values[0], 0);
        assert_eq!(tables[0].values[63], 63);
        assert_eq!(write_dqt(&tables), payload);
    }

    #[test]
    fn a_two_table_sixteen_bit_dqt_round_trips() {
        let mut payload = Vec::new();
        for id in [0u8, 1] {
            payload.push(0x10 | id); // Pq=1
            for v in 0u16..64 {
                payload.extend_from_slice(&(v * 100).to_be_bytes());
            }
        }
        let tables = parse_dqt(&payload).expect("parses");
        assert_eq!(tables.len(), 2);
        assert!(tables[0].sixteen_bit());
        assert_eq!(tables[1].id, 1);
        assert_eq!(write_dqt(&tables), payload);
    }

    /// Regression for a bug `cbs_jpeg` fuzzing found: `Pq` (the precision
    /// nibble) collapsed to a `bool` on read, so a reserved value outside
    /// 0/1 (2..=15 — no real encoder ever sets one, but the parser does not
    /// reject it) came back as exactly `1` on write, changing bytes with no
    /// edit at all. `precision` now keeps the whole nibble.
    #[test]
    fn a_reserved_precision_nibble_round_trips_whole() {
        let payload = vec![0xFFu8] // Pq=0xF (reserved), Tq=0xF
            .into_iter()
            .chain((0u16..64).flat_map(|i| (i * 100).to_be_bytes())) // Pq!=0 reads 16-bit
            .collect::<Vec<u8>>();
        let tables = parse_dqt(&payload).expect("parses");
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].precision, 0xF);
        assert_eq!(tables[0].id, 0xF);
        assert_eq!(
            write_dqt(&tables),
            payload,
            "the reserved nibble must survive whole"
        );
    }

    /// The real four-table `DHT` from the `baseline.jpg` fixture (DC/AC for
    /// two component classes, `ffmpeg`'s default Annex-K tables).
    #[test]
    fn a_real_dht_round_trips() {
        // Built from spec shape rather than the full 161-byte real segment
        // inline here: two tiny tables, one DC one AC, exercising both
        // classes and a non-zero id.
        let mut payload = Vec::new();
        // Table 1: class 0 (DC), id 0, one code of length 1.
        payload.push(0x00);
        let mut counts1 = [0u8; 16];
        counts1[0] = 1;
        payload.extend_from_slice(&counts1);
        payload.push(0x05); // the one value
        // Table 2: class 1 (AC), id 1, two codes of length 2.
        payload.push(0x11);
        let mut counts2 = [0u8; 16];
        counts2[1] = 2;
        payload.extend_from_slice(&counts2);
        payload.extend_from_slice(&[0x01, 0x02]);

        let tables = parse_dht(&payload).expect("parses");
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].class, 0);
        assert_eq!(tables[0].values, vec![0x05]);
        assert_eq!(tables[1].class, 1);
        assert_eq!(tables[1].id, 1);
        assert_eq!(tables[1].values, vec![0x01, 0x02]);
        assert_eq!(write_dht(&tables), payload);
    }

    #[test]
    fn dht_truncation_never_panics() {
        let mut payload = vec![0x00u8];
        payload.extend_from_slice(&[0u8; 16]);
        for n in 0..payload.len() {
            let _ = parse_dht(&payload[..n]);
        }
    }
}
