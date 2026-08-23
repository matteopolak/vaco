//! AV1 Open Bitstream Unit framing, for the `av1`/`obu` raw demuxers.
//!
//! Implemented directly from the publicly available `AOMedia` AV1 Bitstream &
//! Decoding Process Specification §5.3 (OBU syntax) — a spec is exactly what
//! D7's clean-room policy asks for, and OBU framing is simple enough that a
//! dedicated `vaco-parse-av1` dependency is not needed for this fallback
//! (D14.1 forbids one anyway; `crate::bitstream` still prefers the real
//! parser through `ParserProvider` when the caller supplies one).
//!
//! An OBU is: one header byte (`forbidden_bit`, 4-bit `obu_type`,
//! `extension_flag`, `has_size_field`, `reserved_bit`), one more header byte
//! iff `extension_flag`, then — iff `has_size_field` — a `leb128` payload
//! size. The "low overhead bitstream format" that `obu`/`av1` demux (as
//! opposed to the length-prefixed form MP4 uses) requires `has_size_field`
//! on every OBU, so that is the only case handled; an OBU without a size
//! field is treated as running to the end of the buffer.
//!
//! A **temporal unit** — what this module packetises as one [`Packet`] — is
//! the run of OBUs up to (but not including) the next `OBU_TEMPORAL_DELIMITER`
//! (`obu_type == 2`).

/// `obu_type` values named in the spec that this module inspects.
const OBU_TEMPORAL_DELIMITER: u8 = 2;

/// One parsed OBU header, plus where its payload starts and ends in the
/// original buffer.
#[derive(Debug, Clone, Copy)]
struct Obu {
    kind: u8,
    /// Byte offset of this OBU's header (its very first byte).
    start: usize,
    /// Byte offset one past the end of this OBU (header + payload).
    end: usize,
}

/// Decode a `leb128` value starting at `at`, per the AV1 spec (little-endian,
/// 7 bits per byte, up to 8 bytes). Returns `(value, bytes_consumed)`.
fn read_leb128(data: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for i in 0..8usize {
        let byte = *data.get(at.checked_add(i)?)?;
        value |= u64::from(byte & 0x7f).checked_shl(u32::try_from(i * 7).ok()?)?;
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Parse the OBU starting at `at`, if a complete header (and, when declared,
/// a complete payload) is available.
fn parse_one(data: &[u8], at: usize) -> Option<Obu> {
    let header = *data.get(at)?;
    let kind = (header >> 3) & 0x0f;
    let extension_flag = header & 0x04 != 0;
    let has_size_field = header & 0x02 != 0;
    let mut cursor = at.checked_add(1)?;
    if extension_flag {
        cursor = cursor.checked_add(1)?;
    }
    let end = if has_size_field {
        let (size, used) = read_leb128(data, cursor)?;
        cursor = cursor.checked_add(used)?;
        cursor.checked_add(usize::try_from(size).ok()?)?
    } else {
        data.len()
    };
    if end > data.len() {
        return None;
    }
    Some(Obu {
        kind,
        start: at,
        end,
    })
}

/// Split `data` into temporal-unit spans `(start, end)`.
///
/// Bounded: each OBU consumes at least one byte of header, so the scan
/// always makes progress or stops at the first malformed OBU (whatever
/// bytes preceded it are still reported as a final, best-effort span).
#[must_use]
pub fn temporal_units(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut unit_start = 0usize;
    let mut pos = 0usize;
    let mut seen_any_in_unit = false;
    while pos < data.len() {
        let Some(obu) = parse_one(data, pos) else {
            break;
        };
        if obu.kind == OBU_TEMPORAL_DELIMITER && seen_any_in_unit {
            out.push((unit_start, obu.start));
            unit_start = obu.start;
        }
        seen_any_in_unit = true;
        pos = obu.end;
    }
    if pos > unit_start {
        out.push((unit_start, pos));
    } else if pos < data.len() && !out.is_empty() {
        // Trailing malformed bytes after at least one good unit: fold them
        // into a final span rather than silently dropping them.
        out.push((pos, data.len()));
    } else if pos == 0 && !data.is_empty() {
        // Nothing parsed at all: report the whole buffer as one span so the
        // caller still sees *a* packet instead of silence.
        out.push((0, data.len()));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Build a minimal OBU: header with `has_size_field=1`, no extension.
    fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![(obu_type << 3) | 0x02];
        // payload.len() fits in one leb128 byte for these tests.
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn leb128_round_trips_small_values() {
        assert_eq!(read_leb128(&[0x05], 0), Some((5, 1)));
        assert_eq!(read_leb128(&[0x81, 0x01], 0), Some((129, 2)));
    }

    #[test]
    fn one_temporal_unit_per_delimiter() {
        let mut data = Vec::new();
        data.extend(obu(OBU_TEMPORAL_DELIMITER, &[]));
        data.extend(obu(1, &[0xAA, 0xBB])); // sequence header
        data.extend(obu(6, &[0xCC])); // frame
        data.extend(obu(OBU_TEMPORAL_DELIMITER, &[]));
        data.extend(obu(6, &[0xDD]));
        let spans = temporal_units(&data);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0], (0, 9));
        assert_eq!(spans[1], (9, data.len()));
    }

    #[test]
    fn empty_input_yields_no_spans() {
        assert!(temporal_units(&[]).is_empty());
    }
}
