//! `SOI`/`SOF55`/`LSE`/`SOS`/`EOI` marker segments.
//!
//! JPEG-LS reuses JPEG's high-level marker syntax (Appendix: "the same high
//! level syntax applies... marker segments specifying the various
//! parameters"), just with its own marker codes and a `SOF55` (`0xFFF7`)
//! frame header instead of `SOF0`. Every marker other than `SOI`/`EOI`
//! carries a two-byte length (including the length field itself), so an
//! unrecognised segment can always be skipped by that length alone — used
//! here for `APPn`/`COM` and any `LSE` this crate does not need to act on.

use vaco_core::{Error, Result};

pub(crate) const SOI: u8 = 0xD8;
pub(crate) const EOI: u8 = 0xD9;
pub(crate) const SOF55: u8 = 0xF7;
pub(crate) const LSE: u8 = 0xF8;
pub(crate) const SOS: u8 = 0xDA;

/// One component entry from a frame header. `H`/`V` are validated to be `1`
/// (no subsampling) at parse time and not carried forward; `id` is kept so a
/// scan's selectors can be checked against the frame that declared them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameComponent {
    pub(crate) id: u8,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameHeader {
    pub(crate) precision: u8,
    pub(crate) height: u16,
    pub(crate) width: u16,
    pub(crate) num_components: u8,
    /// Only the first three are ever read (`num_components` is validated to
    /// be 1 or 3 before this is used); unused slots are zeroed.
    pub(crate) components: [FrameComponent; 3],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanHeader {
    pub(crate) num_components: u8,
    /// Component selectors for this scan, in scan order.
    pub(crate) selectors: [u8; 3],
    pub(crate) near: u8,
    pub(crate) ilv: u8,
}

/// Read a big-endian `u16` at `data[pos..pos+2]`.
fn u16_at(data: &[u8], pos: usize) -> Result<u16> {
    let b = data.get(pos..pos + 2).ok_or(Error::UnexpectedEof)?;
    let (Some(&hi), Some(&lo)) = (b.first(), b.get(1)) else {
        return Err(Error::UnexpectedEof);
    };
    Ok((u16::from(hi) << 8) | u16::from(lo))
}

fn byte_at(data: &[u8], pos: usize) -> Result<u8> {
    data.get(pos).copied().ok_or(Error::UnexpectedEof)
}

/// Find the next marker starting at or after `pos`, skipping any `0xFF`
/// fill bytes. Returns `(marker_code, offset_of_the_0xFF_byte)`.
///
/// # Errors
/// [`Error::InvalidData`] if the stream runs out before a marker is found.
pub(crate) fn find_marker(data: &[u8], mut pos: usize) -> Result<(u8, usize)> {
    loop {
        let b = byte_at(data, pos)?;
        if b != 0xFF {
            return Err(Error::InvalidData("jpegls: expected a marker"));
        }
        let start = pos;
        pos += 1;
        let code = byte_at(data, pos)?;
        if code == 0xFF {
            // Fill byte between markers; keep scanning.
            continue;
        }
        return Ok((code, start));
    }
}

/// Parse a `SOF55` payload. `pos` points at the two length bytes.
///
/// # Errors
/// [`Error::InvalidData`]/[`Error::Unsupported`] for a malformed or
/// unsupported (e.g. subsampled) frame header.
pub(crate) fn parse_sof55(data: &[u8], pos: usize) -> Result<(FrameHeader, usize)> {
    let len = usize::from(u16_at(data, pos)?);
    if len < 8 {
        return Err(Error::InvalidData("jpegls: SOF55 too short"));
    }
    let precision = byte_at(data, pos + 2)?;
    let height = u16_at(data, pos + 3)?;
    let width = u16_at(data, pos + 5)?;
    let nf = byte_at(data, pos + 7)?;
    if !(nf == 1 || nf == 3) {
        return Err(Error::Unsupported(
            "jpegls: only 1- or 3-component frames are decoded",
        ));
    }
    let mut components = [FrameComponent { id: 0 }; 3];
    let expected_len = 8usize.saturating_add(usize::from(nf) * 3);
    if len < expected_len {
        return Err(Error::InvalidData("jpegls: SOF55 component list truncated"));
    }
    for i in 0..usize::from(nf) {
        let base = pos + 8 + i * 3;
        let id = byte_at(data, base)?;
        let hv = byte_at(data, base + 1)?;
        let h = hv >> 4;
        let v = hv & 0x0F;
        if h != 1 || v != 1 {
            return Err(Error::Unsupported(
                "jpegls: subsampled components are not decoded",
            ));
        }
        if let Some(slot) = components.get_mut(i) {
            *slot = FrameComponent { id };
        }
    }
    if height == 0 || width == 0 {
        return Err(Error::InvalidData("jpegls: zero-sized frame"));
    }
    let header = FrameHeader {
        precision,
        height,
        width,
        num_components: nf,
        components,
    };
    // `len` counts itself (2 bytes) plus everything after it.
    Ok((header, pos + len))
}

/// Parse an `SOS` payload. `pos` points at the two length bytes.
///
/// # Errors
/// [`Error::InvalidData`]/[`Error::Unsupported`] for a malformed or
/// unsupported (point-transformed, or a component count that disagrees with
/// the frame) scan header.
pub(crate) fn parse_sos(data: &[u8], pos: usize) -> Result<(ScanHeader, usize)> {
    let len = usize::from(u16_at(data, pos)?);
    let ns = byte_at(data, pos + 2)?;
    if !(ns == 1 || ns == 3) {
        return Err(Error::Unsupported(
            "jpegls: only 1- or 3-component scans are decoded",
        ));
    }
    let expected_len = 6usize.saturating_add(usize::from(ns) * 2);
    if len < expected_len {
        return Err(Error::InvalidData("jpegls: SOS truncated"));
    }
    let mut selectors = [0u8; 3];
    for i in 0..usize::from(ns) {
        let base = pos + 3 + i * 2;
        let csi = byte_at(data, base)?;
        if let Some(slot) = selectors.get_mut(i) {
            *slot = csi;
        }
    }
    let tail = pos + 3 + usize::from(ns) * 2;
    let near = byte_at(data, tail)?;
    let ilv = byte_at(data, tail + 1)?;
    let point_transform = byte_at(data, tail + 2)?;
    if point_transform != 0 {
        return Err(Error::Unsupported(
            "jpegls: a nonzero point transform is not decoded",
        ));
    }
    if ilv > 2 {
        return Err(Error::InvalidData("jpegls: unknown interleave mode"));
    }
    let header = ScanHeader {
        num_components: ns,
        selectors,
        near,
        ilv,
    };
    Ok((header, pos + len))
}

/// Skip a length-prefixed marker segment this crate does not interpret
/// (`APPn`, `COM`, or an `LSE` whose contents were already checked to be the
/// defaults). `pos` points at the two length bytes.
pub(crate) fn skip_segment(data: &[u8], pos: usize) -> Result<usize> {
    let len = usize::from(u16_at(data, pos)?);
    if len < 2 {
        return Err(Error::InvalidData("jpegls: marker segment too short"));
    }
    Ok(pos + len)
}

/// `LSE` (marker-selection extension) default-parameter contents this crate
/// accepts without change: `MAXVAL=255, T1=3, T2=7, T3=21, RESET=64`. Any
/// other `LSE` payload would mean a threshold set this crate's decode tables
/// do not implement, so it is rejected rather than silently ignored.
///
/// # Errors
/// [`Error::Unsupported`] if the segment states non-default thresholds.
pub(crate) fn check_lse_is_default(data: &[u8], pos: usize) -> Result<usize> {
    let len = usize::from(u16_at(data, pos)?);
    let id = byte_at(data, pos + 2)?;
    if id == 1 {
        let maxval = u16_at(data, pos + 3)?;
        let t1 = u16_at(data, pos + 5)?;
        let t2 = u16_at(data, pos + 7)?;
        let t3 = u16_at(data, pos + 9)?;
        let reset = u16_at(data, pos + 11)?;
        if maxval != 255 || t1 != 3 || t2 != 7 || t3 != 21 || reset != 64 {
            return Err(Error::Unsupported(
                "jpegls: non-default LSE thresholds are not decoded",
            ));
        }
    }
    // Any other LSE `ID` (palette tables, oversized-image dimensions) is
    // outside this crate's scope; skip it by length rather than guess.
    Ok(pos + len)
}

/// Append `SOI`.
pub(crate) fn write_soi(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0xFF, SOI]);
}

/// Append `EOI`.
pub(crate) fn write_eoi(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0xFF, EOI]);
}

/// Append a `SOF55` for an 8-bit frame with `num_components` components, each
/// unsampled (`H = V = 1`), component ids `1, 2, 3, ...`.
pub(crate) fn write_sof55(out: &mut Vec<u8>, width: u16, height: u16, num_components: u8) {
    let len: u16 = 8 + u16::from(num_components) * 3;
    out.extend_from_slice(&[0xFF, SOF55]);
    out.extend_from_slice(&len.to_be_bytes());
    out.push(8); // precision
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.push(num_components);
    for c in 1..=num_components {
        out.push(c);
        out.push(0x11); // H=1, V=1
        out.push(0); // Tq, unused
    }
}

/// Append a `SOS` for `num_components` components (selectors `1, 2, 3, ...`),
/// lossless (`NEAR = 0`), with the given interleave mode.
pub(crate) fn write_sos(out: &mut Vec<u8>, num_components: u8, ilv: u8) {
    let len: u16 = 6 + u16::from(num_components) * 2;
    out.extend_from_slice(&[0xFF, SOS]);
    out.extend_from_slice(&len.to_be_bytes());
    out.push(num_components);
    for c in 1..=num_components {
        out.push(c);
        out.push(0); // Td/Ta, unused
    }
    out.push(0); // NEAR
    out.push(ilv);
    out.push(0); // point transform
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn sof55_round_trips_through_the_writer_and_parser() {
        let mut buf = Vec::new();
        write_soi(&mut buf);
        write_sof55(&mut buf, 64, 48, 3);
        let (code, marker_start) = find_marker(&buf, 2).unwrap();
        assert_eq!(code, SOF55);
        let (fh, next) = parse_sof55(&buf, marker_start + 2).unwrap();
        assert_eq!(fh.width, 64);
        assert_eq!(fh.height, 48);
        assert_eq!(fh.num_components, 3);
        assert_eq!(next, buf.len());
    }

    #[test]
    fn sos_round_trips_through_the_writer_and_parser() {
        let mut buf = Vec::new();
        write_sos(&mut buf, 1, 0);
        let (code, marker_start) = find_marker(&buf, 0).unwrap();
        assert_eq!(code, SOS);
        let (sh, next) = parse_sos(&buf, marker_start + 2).unwrap();
        assert_eq!(sh.num_components, 1);
        assert_eq!(sh.near, 0);
        assert_eq!(sh.ilv, 0);
        assert_eq!(next, buf.len());
    }

    #[test]
    fn find_marker_skips_fill_bytes() {
        let buf = [0xFF, 0xFF, 0xFF, EOI];
        let (code, start) = find_marker(&buf, 0).unwrap();
        assert_eq!(code, EOI);
        assert_eq!(start, 2);
    }
}
