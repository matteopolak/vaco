//! OBU framing: `obu_header()`, `open_bitstream_unit()`, and the two ways AV1
//! delimits a sequence of OBUs in a byte stream. AV1 spec §5.2–§5.3, Annex B.
//!
//! # AV1 has no NAL start codes
//!
//! Every OBU carries its own type in a fixed-position header byte, and either
//! sizes itself (`obu_has_size_field`) or is sized by whatever wraps it. There
//! is no marker to scan for, so splitting a buffer into OBUs is arithmetic on
//! declared lengths rather than a byte-pattern search — which also means a
//! corrupt length is the whole attack surface, not a scan miss.
//!
//! # Two framings, both real
//!
//! - [`Av1Framing::ObuStream`]: OBUs concatenated directly, each carrying its own
//!   `obu_size` (`obu_has_size_field == 1`). This is what MP4's and Matroska's
//!   sample data use, what `av1C`'s `configOBUs` use, and — measured, see
//!   `docs/codec/vaco-parse-av1.md` — what `ffmpeg -f obu` writes for a raw
//!   elementary stream. **This is not the format the specification's Annex B
//!   describes**; it has no name of its own in the spec beyond "a sequence of
//!   OBUs each with `obu_has_size_field == 1`", so `ObuStream` is this crate's
//!   name for it.
//! - [`Av1Framing::LowOverheadBitstream`]: Annex B's actual wrapper —
//!   `temporal_unit_size`, then per-frame `frame_unit_size`, then per-OBU
//!   `obu_length`, each a `leb128()` — which lets a contained OBU omit its own
//!   size and be sized by the wrapper instead. No fixture in this crate's test
//!   corpus uses it (every encoder probed writes `ObuStream`), so it is
//!   implemented from the specification text alone and exercised by hand-built
//!   unit tests and the fuzzer, not by a real capture. Said plainly in the
//!   report: this path is the less-verified half of the framing code.

use vaco_bitstream::BitReader;

use crate::leb::leb128;

/// `obu_type`, §6.2.2 Table 6.2. Four bits, so 16 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObuType(u8);

impl ObuType {
    /// 1 — `sequence_header_obu()`.
    pub const SEQUENCE_HEADER: Self = Self(1);
    /// 2 — empty payload; marks the start of a temporal unit.
    pub const TEMPORAL_DELIMITER: Self = Self(2);
    /// 3 — `frame_header_obu()` on its own, tile groups to follow.
    pub const FRAME_HEADER: Self = Self(3);
    /// 4 — `tile_group_obu()`.
    pub const TILE_GROUP: Self = Self(4);
    /// 5 — `metadata_obu()`.
    pub const METADATA: Self = Self(5);
    /// 6 — a frame header immediately followed by its own tile group, as one
    /// OBU (`frame_obu()`).
    pub const FRAME: Self = Self(6);
    /// 7 — a verbatim repeat of the previous frame header, for error
    /// resilience.
    pub const REDUNDANT_FRAME_HEADER: Self = Self(7);
    /// 8 — `tile_list_obu()`, the large-scale-tile use case (Annex D).
    pub const TILE_LIST: Self = Self(8);
    /// 15 — padding; a decoder skips the payload unread.
    pub const PADDING: Self = Self(15);

    /// Wrap a raw four-bit value. Masked, not rejected, so a caller that
    /// shifts wrong gets a wrong answer rather than a panic.
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        Self(v & 0x0F)
    }

    /// The raw four-bit value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Whether a decoder must retain this OBU's syntax across frames — the
    /// sequence header only. Not used by this crate's own state, but useful to
    /// a caller building a `filter_units`-style filter that must never drop
    /// one.
    #[must_use]
    pub const fn is_sequence_header(self) -> bool {
        self.0 == Self::SEQUENCE_HEADER.0
    }

    /// Diagnostics only.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self.0 {
            1 => "OBU_SEQUENCE_HEADER",
            2 => "OBU_TEMPORAL_DELIMITER",
            3 => "OBU_FRAME_HEADER",
            4 => "OBU_TILE_GROUP",
            5 => "OBU_METADATA",
            6 => "OBU_FRAME",
            7 => "OBU_REDUNDANT_FRAME_HEADER",
            8 => "OBU_TILE_LIST",
            15 => "OBU_PADDING",
            0 => "OBU_RESERVED_0",
            _ => "OBU_RESERVED",
        }
    }
}

/// `obu_header()` plus `obu_extension_header()`, §5.3.2–§5.3.3, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObuHeader {
    pub obu_type: ObuType,
    pub extension_flag: bool,
    pub has_size_field: bool,
    /// `temporal_id`, present only when `extension_flag`; 0 otherwise.
    pub temporal_id: u8,
    /// `spatial_id`, present only when `extension_flag`; 0 otherwise.
    pub spatial_id: u8,
    /// Bytes the header itself occupies: 1, or 2 with the extension.
    pub header_len: u8,
}

impl ObuHeader {
    /// Decode the header at the start of `data`. `None` if `data` is empty, or
    /// the extension flag is set but the second byte is missing.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        let first = *data.first()?;
        // obu_forbidden_bit: f(1) — bit 7. A conforming OBU always has this
        // zero; a nonzero value most often means these bytes are not the start
        // of an OBU at all (e.g. mid-payload noise a corrupt length pointed
        // into). Reported via `forbidden_bit_set`, not fatal here: a caller
        // splitting a fragment wants to see the byte and decide.
        let obu_type = ObuType::from_u8((first >> 3) & 0x0F);
        let extension_flag = (first >> 2) & 1 != 0;
        let has_size_field = (first >> 1) & 1 != 0;
        let (temporal_id, spatial_id, header_len) = if extension_flag {
            let ext = *data.get(1)?;
            (ext >> 5, (ext >> 3) & 0x03, 2u8)
        } else {
            (0, 0, 1u8)
        };
        Some(Self {
            obu_type,
            extension_flag,
            has_size_field,
            temporal_id,
            spatial_id,
            header_len,
        })
    }

    /// `obu_forbidden_bit`, re-derived from the same byte `parse` read, for a
    /// caller that wants to reject non-conforming input rather than merely
    /// note it.
    #[must_use]
    pub fn forbidden_bit_set(data: &[u8]) -> bool {
        data.first().is_some_and(|b| b & 0x80 != 0)
    }
}

/// One OBU as it appears in a buffer: the header, the declared payload size,
/// and the byte range of the whole unit (header, size field if present, and
/// payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObuUnit {
    pub header: ObuHeader,
    /// `obu_size`: payload bytes, header excluded.
    pub obu_size: u64,
    /// Offset of this unit's first byte (the header) within the buffer it was
    /// found in.
    pub offset: usize,
    /// Total bytes this unit occupies, header and any size field included.
    pub total_len: usize,
}

impl ObuUnit {
    /// The unit's bytes: header, size field if present, and payload — exactly
    /// what a [`vaco_codec_cbs::CbsUnit`] should hold.
    #[must_use]
    pub fn bytes<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        buf.get(self.offset..self.offset + self.total_len)
            .unwrap_or(&[])
    }

    /// The payload alone, size field and header excluded.
    #[must_use]
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        let start = self.offset + self.total_len - self.obu_size as usize;
        buf.get(start..start + self.obu_size as usize)
            .unwrap_or(&[])
    }
}

/// `open_bitstream_unit(sz)`'s header-and-size half, §5.3.1.
///
/// `external_sz` is `sz` from the pseudocode: the number of bytes the *wrapper*
/// says this OBU occupies, used only when `obu_has_size_field` is 0. `None`
/// when there is no wrapper (an `ObuStream`, where every OBU must size itself).
///
/// Returns `None` when the buffer is too short for even the header, when
/// `obu_has_size_field` is 0 and no `external_sz` was supplied, or when the
/// derived `obu_size` would make the unit run past `buf.len()`.
fn read_unit_at(buf: &[u8], offset: usize, external_sz: Option<u64>) -> Option<ObuUnit> {
    let rest = buf.get(offset..)?;
    let header = ObuHeader::parse(rest)?;
    let header_len = header.header_len as usize;
    let after_header = rest.get(header_len..)?;

    let (obu_size, size_field_len) = if header.has_size_field {
        let mut r = BitReader::new(after_header);
        let (size, bytes) = leb128(&mut r);
        if r.overrun() {
            return None;
        }
        (size, bytes as usize)
    } else {
        // §5.3.1: `obu_size = sz - 1 - obu_extension_flag`. `sz` counts every
        // byte of the unit including the one-byte header; the extension byte,
        // if present, is subtracted too.
        let sz = external_sz?;
        let fixed = 1 + u64::from(header.extension_flag);
        (sz.checked_sub(fixed)?, 0)
    };

    let total_len = header_len
        .checked_add(size_field_len)?
        .checked_add(usize::try_from(obu_size).ok()?)?;
    if offset.checked_add(total_len)? > buf.len() {
        return None;
    }
    Some(ObuUnit {
        header,
        obu_size,
        offset,
        total_len,
    })
}

/// How a caller's buffer delimits its OBUs. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1Framing {
    /// Every OBU carries its own `obu_size`; the buffer is a flat
    /// concatenation with no outer wrapper. MP4/Matroska sample data, `av1C`
    /// `configOBUs`, and — measured — `ffmpeg -f obu`.
    ObuStream,
    /// Annex B: `temporal_unit_size` / `frame_unit_size` / `obu_length`
    /// `leb128()` wrappers nest around groups of OBUs, and a contained OBU may
    /// omit its own size.
    LowOverheadBitstream,
}

/// Parse a single OBU at `offset` in an [`Av1Framing::ObuStream`]-framed buffer,
/// without splitting the whole thing.
///
/// For a streaming parser that wants to advance one OBU at a time and keep its
/// own cursor, rather than re-scanning from the start of a growing buffer on
/// every call — see [`crate::parser::Av1Parser`].
#[must_use]
pub fn next_obu_stream_unit(data: &[u8], offset: usize) -> Option<ObuUnit> {
    read_unit_at(data, offset, None)
}

/// Split `data` into OBUs under `framing`.
///
/// Stops at the first unit that fails to parse — a truncated tail is common
/// (the last OBU of a stream cut mid-transfer) and is not an error the caller
/// needs reported unit by unit; whatever parsed cleanly is still returned.
/// Malformed bytes in the *middle* of the buffer are indistinguishable from a
/// truncated tail at this layer, which is the same trade-off
/// `vaco_format_nalu::units` makes for NAL units.
#[must_use]
pub fn units(data: &[u8], framing: Av1Framing) -> Vec<ObuUnit> {
    match framing {
        Av1Framing::ObuStream => units_flat(data),
        Av1Framing::LowOverheadBitstream => units_low_overhead(data),
    }
}

fn units_flat(data: &[u8]) -> Vec<ObuUnit> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let Some(unit) = read_unit_at(data, offset, None) else {
            break;
        };
        if unit.total_len == 0 {
            break;
        }
        offset += unit.total_len;
        out.push(unit);
    }
    out
}

/// Read one `leb128()` at `offset`, returning `(value, bytes_consumed)`.
fn read_leb128_at(data: &[u8], offset: usize) -> Option<(u64, usize)> {
    let rest = data.get(offset..)?;
    let mut r = BitReader::new(rest);
    let (v, n) = leb128(&mut r);
    if r.overrun() {
        return None;
    }
    Some((v, n as usize))
}

fn units_low_overhead(data: &[u8]) -> Vec<ObuUnit> {
    let mut out = Vec::new();
    let mut tu_off = 0usize;
    'temporal_units: while tu_off < data.len() {
        let Some((tu_size, tu_hdr)) = read_leb128_at(data, tu_off) else {
            break;
        };
        let Some(tu_size) = usize::try_from(tu_size).ok() else {
            break;
        };
        let tu_body_start = tu_off + tu_hdr;
        let Some(tu_body_end) = tu_body_start.checked_add(tu_size) else {
            break;
        };
        if tu_body_end > data.len() {
            break;
        }
        let mut fu_off = tu_body_start;
        while fu_off < tu_body_end {
            let Some((fu_size, fu_hdr)) = read_leb128_at(data, fu_off) else {
                break 'temporal_units;
            };
            let Some(fu_size) = usize::try_from(fu_size).ok() else {
                break 'temporal_units;
            };
            let fu_body_start = fu_off + fu_hdr;
            let Some(fu_body_end) = fu_body_start.checked_add(fu_size) else {
                break 'temporal_units;
            };
            if fu_body_end > tu_body_end {
                break 'temporal_units;
            }
            let mut obu_off = fu_body_start;
            while obu_off < fu_body_end {
                let Some((obu_len, obu_hdr)) = read_leb128_at(data, obu_off) else {
                    break 'temporal_units;
                };
                let Some(obu_len) = usize::try_from(obu_len).ok() else {
                    break 'temporal_units;
                };
                let obu_body_start = obu_off + obu_hdr;
                let Some(obu_end) = obu_body_start.checked_add(obu_len) else {
                    break 'temporal_units;
                };
                if obu_end > fu_body_end {
                    break 'temporal_units;
                }
                let Some(unit) = read_unit_at(data, obu_body_start, Some(obu_len as u64)) else {
                    break 'temporal_units;
                };
                out.push(unit);
                obu_off = obu_end;
            }
            fu_off = fu_body_end;
        }
        tu_off = tu_body_end;
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

    /// Temporal delimiter (0x12 0x00), then the sequence header from a real
    /// `libsvtav1` raw `.obu` capture (`ffmpeg -f obu`), truncated to its
    /// declared size.
    fn flat_stream() -> Vec<u8> {
        vec![
            0x12, 0x00, // OBU_TEMPORAL_DELIMITER, size 0
            0x0a, 0x0b, // OBU_SEQUENCE_HEADER, size 11
            0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x1a, 0xbe, 0x60, 0x10,
        ]
    }

    #[test]
    fn obu_stream_splits_into_its_two_units() {
        let data = flat_stream();
        let units = units(&data, Av1Framing::ObuStream);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].header.obu_type, ObuType::TEMPORAL_DELIMITER);
        assert_eq!(units[0].obu_size, 0);
        assert_eq!(units[1].header.obu_type, ObuType::SEQUENCE_HEADER);
        assert_eq!(units[1].obu_size, 11);
        assert_eq!(units[1].payload(&data).len(), 11);
    }

    #[test]
    fn a_truncated_final_obu_is_dropped_not_erred() {
        let mut data = flat_stream();
        data.truncate(data.len() - 3);
        let units = units(&data, Av1Framing::ObuStream);
        // The temporal delimiter still parses; the truncated sequence header
        // does not.
        assert_eq!(units.len(), 1);
    }

    #[test]
    fn low_overhead_wraps_the_same_two_units() {
        // temporal_unit_size wraps one frame_unit_size, which wraps one
        // obu_length, which wraps a bare (has_size_field = 0) temporal
        // delimiter header.
        let obu_header = [0x10u8]; // type=2 (TD), ext=0, has_size=0
        let obu_length = obu_header.len(); // sz passed to open_bitstream_unit
        let mut frame_unit = vec![obu_length as u8]; // leb128(obu_length), 1 byte since <128
        frame_unit.extend_from_slice(&obu_header);
        let mut temporal_unit = vec![frame_unit.len() as u8]; // leb128(frame_unit_size)
        temporal_unit.extend_from_slice(&frame_unit);
        let mut data = vec![temporal_unit.len() as u8]; // leb128(temporal_unit_size)
        data.extend_from_slice(&temporal_unit);

        let units = units(&data, Av1Framing::LowOverheadBitstream);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].header.obu_type, ObuType::TEMPORAL_DELIMITER);
        assert_eq!(units[0].obu_size, 0);
    }

    #[test]
    fn every_truncation_splits_without_panicking() {
        let data = flat_stream();
        for n in 0..=data.len() {
            let _ = units(&data[..n], Av1Framing::ObuStream);
            let _ = units(&data[..n], Av1Framing::LowOverheadBitstream);
        }
    }

    #[test]
    fn a_size_field_claiming_past_the_buffer_is_rejected() {
        // OBU_METADATA header (type 5) with has_size_field=1, size leb128 =
        // 200, but only 2 bytes follow.
        let data = [0x2Au8, 0xC8, 0x01, 0x00, 0x00];
        assert!(units(&data, Av1Framing::ObuStream).is_empty());
    }
}
