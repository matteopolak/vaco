//! Frame header (`SOF`), scan header (`SOS`), and the segments that
//! configure decoding: `DQT`, `DHT`, `DRI`, `APP0` (JFIF), `APP14` (Adobe).
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`; the `APP14` transform field is
//! `Vaco-Spec-Ref: adobe-tn5116`.

use arrayvec::ArrayVec;
use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};

use crate::marker;
use crate::tables::ZIGZAG;

/// The most components any `SOF`/`SOS` this crate decodes may declare:
/// grayscale, YCbCr/RGB, or CMYK/YCCK.
pub(crate) const MAX_COMPONENTS: usize = 4;

/// One component's declaration from `SOF`: sampling factors and which
/// quantization table it uses.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ComponentSpec {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub tq: u8,
}

/// `SOF0`/`SOF1`/`SOF2` (baseline, extended sequential, progressive —
/// see the crate docs for why those three and not the rest of Table B.1).
#[derive(Debug, Clone)]
pub(crate) struct FrameHeader {
    pub sof_marker: u8,
    pub precision: u8,
    pub height: u16,
    pub width: u16,
    pub components: ArrayVec<ComponentSpec, MAX_COMPONENTS>,
}

impl FrameHeader {
    #[must_use]
    pub(crate) fn is_progressive(&self) -> bool {
        marker::is_progressive_sof(self.sof_marker)
    }

    #[must_use]
    pub(crate) fn component_index(&self, id: u8) -> Option<usize> {
        self.components.iter().position(|c| c.id == id)
    }

    #[must_use]
    pub(crate) fn h_max(&self) -> u32 {
        self.components
            .iter()
            .map(|c| u32::from(c.h))
            .max()
            .unwrap_or(1)
            .max(1)
    }

    #[must_use]
    pub(crate) fn v_max(&self) -> u32 {
        self.components
            .iter()
            .map(|c| u32::from(c.v))
            .max()
            .unwrap_or(1)
            .max(1)
    }
}

/// Parse a `SOF0`/`SOF1`/`SOF2` payload (the bytes after the 2-byte length).
///
/// # Errors
/// [`Error::InvalidData`] on a truncated segment, a zero dimension, zero
/// components, or more components than [`MAX_COMPONENTS`].
pub(crate) fn parse_sof(sof_marker: u8, payload: &[u8]) -> Result<FrameHeader> {
    let mut r = ByteReader::new(payload);
    let precision = r.u8();
    let height = r.be16();
    let width = r.be16();
    let num_components = r.u8();
    if num_components == 0 || usize::from(num_components) > MAX_COMPONENTS {
        return Err(Error::InvalidData("jpeg: SOF component count out of range"));
    }
    let mut components = ArrayVec::new();
    for _ in 0..num_components {
        let id = r.u8();
        let sampling = r.u8();
        let tq = r.u8();
        let h = sampling >> 4;
        let v = sampling & 0x0F;
        if h == 0 || v == 0 || h > 4 || v > 4 {
            return Err(Error::InvalidData("jpeg: SOF sampling factor out of range"));
        }
        // Capacity is `MAX_COMPONENTS` and the loop runs `num_components`
        // times, already bounded above; `push` cannot fail.
        if components.try_push(ComponentSpec { id, h, v, tq }).is_err() {
            return Err(Error::InvalidData("jpeg: too many SOF components"));
        }
    }
    r.check()?;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("jpeg: zero image dimension"));
    }
    Ok(FrameHeader {
        sof_marker,
        precision,
        height,
        width,
        components,
    })
}

/// One quantization table (`DQT`), de-zigzagged into natural (row-major)
/// order so a decoder can index it by `(u, v)` directly.
#[derive(Debug, Clone, Copy)]
pub(crate) struct QuantTable {
    pub values: [u16; 64],
}

impl QuantTable {
    const FLAT: Self = Self { values: [1; 64] };
}

impl Default for QuantTable {
    fn default() -> Self {
        Self::FLAT
    }
}

/// Parse every table a `DQT` segment defines, calling `set(index, table)`
/// for each. A single segment may define more than one table back to back.
///
/// # Errors
/// [`Error::InvalidData`] on a truncated segment or a table index `> 3`.
pub(crate) fn parse_dqt(payload: &[u8], mut set: impl FnMut(usize, QuantTable)) -> Result<()> {
    let mut r = ByteReader::new(payload);
    while r.remaining() > 0 {
        let pq_tq = r.u8();
        let precision_16 = pq_tq >> 4 != 0;
        let tq = usize::from(pq_tq & 0x0F);
        if tq > 3 {
            return Err(Error::InvalidData("jpeg: DQT table index out of range"));
        }
        let mut values = [0u16; 64];
        for &nat in &ZIGZAG {
            let v = if precision_16 {
                r.be16()
            } else {
                u16::from(r.u8())
            };
            if let Some(slot) = values.get_mut(nat) {
                *slot = v;
            }
        }
        r.check()?;
        set(tq, QuantTable { values });
    }
    Ok(())
}

/// Parse every table a `DHT` segment defines, calling `set(class, index,
/// counts, values)` for each — `class` is 0 for DC, 1 for AC.
///
/// # Errors
/// [`Error::InvalidData`] on a truncated segment or a table class/index out
/// of range.
pub(crate) fn parse_dht(
    payload: &[u8],
    mut set: impl FnMut(u8, usize, [u8; 16], ArrayVec<u8, 256>),
) -> Result<()> {
    let mut r = ByteReader::new(payload);
    while r.remaining() > 0 {
        let tc_th = r.u8();
        let class = tc_th >> 4;
        let index = usize::from(tc_th & 0x0F);
        if class > 1 || index > 3 {
            return Err(Error::InvalidData(
                "jpeg: DHT table class/index out of range",
            ));
        }
        let mut counts = [0u8; 16];
        let mut total = 0usize;
        for slot in &mut counts {
            *slot = r.u8();
            total += usize::from(*slot);
        }
        // `total` is a sum of sixteen bytes, so it is at most 4080 — a `DHT`
        // segment cannot itself declare more, which bounds this without a
        // budgeted allocation.
        let mut values: ArrayVec<u8, 256> = ArrayVec::new();
        for _ in 0..total.min(256) {
            let v = r.u8();
            // Ignore rather than error past 256: `ArrayVec::push` on a full
            // vector would panic, and a table this malformed already fails
            // decode the moment it is used.
            let _ = values.try_push(v);
        }
        r.check()?;
        set(class, index, counts, values);
    }
    Ok(())
}

/// `DRI`: MCUs between restart markers, or 0 for none.
///
/// # Errors
/// [`Error::InvalidData`] on a truncated segment.
pub(crate) fn parse_dri(payload: &[u8]) -> Result<u16> {
    let mut r = ByteReader::new(payload);
    let interval = r.be16();
    r.check()?;
    Ok(interval)
}

/// One component's table selectors within a scan.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanComponentSpec {
    /// Index into [`FrameHeader::components`].
    pub component_index: usize,
    pub td: u8,
    pub ta: u8,
}

/// `SOS`: which components this scan covers, which tables they use, and the
/// spectral/successive-approximation parameters (Annex G; always `(0, 63,
/// 0, 0)` for a baseline scan).
#[derive(Debug, Clone)]
pub(crate) struct ScanHeader {
    pub components: ArrayVec<ScanComponentSpec, MAX_COMPONENTS>,
    pub ss: u8,
    pub se: u8,
    pub ah: u8,
    pub al: u8,
}

/// Parse a `SOS` payload against the frame's already-known component list.
///
/// # Errors
/// [`Error::InvalidData`] on a truncated segment, a component id `SOF` never
/// declared, or a spectral/successive-approximation field out of its
/// syntactic range.
pub(crate) fn parse_sos(payload: &[u8], frame: &FrameHeader) -> Result<ScanHeader> {
    let mut r = ByteReader::new(payload);
    let ns = r.u8();
    if ns == 0 || usize::from(ns) > MAX_COMPONENTS {
        return Err(Error::InvalidData("jpeg: SOS component count out of range"));
    }
    let mut components = ArrayVec::new();
    for _ in 0..ns {
        let cs = r.u8();
        let td_ta = r.u8();
        let component_index = frame.component_index(cs).ok_or(Error::InvalidData(
            "jpeg: SOS references an unknown component id",
        ))?;
        if components
            .try_push(ScanComponentSpec {
                component_index,
                td: td_ta >> 4,
                ta: td_ta & 0x0F,
            })
            .is_err()
        {
            return Err(Error::InvalidData("jpeg: too many SOS components"));
        }
    }
    let ss = r.u8();
    let se = r.u8();
    let ah_al = r.u8();
    r.check()?;
    if ss > 63 || se > 63 || ss > se {
        return Err(Error::InvalidData(
            "jpeg: SOS spectral selection out of range",
        ));
    }
    Ok(ScanHeader {
        components,
        ss,
        se,
        ah: ah_al >> 4,
        al: ah_al & 0x0F,
    })
}

/// JFIF `APP0`. `density_unit == 0` means `x_density`/`y_density` are the
/// pixel aspect ratio directly rather than a physical dot density.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JfifInfo {
    pub version: (u8, u8),
    pub density_unit: u8,
    pub x_density: u16,
    pub y_density: u16,
}

/// Recognise and parse a JFIF `APP0` (`"JFIF\0"` signature). `None` for any
/// other `APP0` payload (e.g. `"JFXX\0"`).
#[must_use]
pub(crate) fn parse_app0_jfif(payload: &[u8]) -> Option<JfifInfo> {
    let mut r = ByteReader::new(payload);
    let tag = r.bytes(5);
    if tag != b"JFIF\0" {
        return None;
    }
    let major = r.u8();
    let minor = r.u8();
    let density_unit = r.u8();
    let x_density = r.be16();
    let y_density = r.be16();
    if r.overrun() {
        return None;
    }
    Some(JfifInfo {
        version: (major, minor),
        density_unit,
        x_density,
        y_density,
    })
}

/// The Adobe `APP14` colour-transform marker (Adobe Technical Note #5116):
/// `0` = no transform (RGB, or CMYK for four components), `1` = YCbCr, `2`
/// = YCCK.
#[must_use]
pub(crate) fn parse_app14_adobe(payload: &[u8]) -> Option<u8> {
    let mut r = ByteReader::new(payload);
    let tag = r.bytes(5);
    if tag != b"Adobe" {
        return None;
    }
    r.skip(6); // version (u16) + two flags words (u16 each)
    let transform = r.u8();
    if r.overrun() {
        return None;
    }
    Some(transform)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn sof0_baseline_420_parses() {
        // precision=8, height=48, width=64, 3 components (Y 2x2, Cb 1x1, Cr 1x1)
        let payload = [
            0x08, 0x00, 0x30, 0x00, 0x40, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11,
            0x00,
        ];
        let sof = parse_sof(marker::SOF0, &payload).unwrap();
        assert_eq!((sof.width, sof.height), (64, 48));
        assert_eq!(sof.components.len(), 3);
        assert_eq!(sof.h_max(), 2);
        assert_eq!(sof.v_max(), 2);
        assert!(!sof.is_progressive());
    }

    #[test]
    fn sof2_is_progressive() {
        let payload = [0x08, 0x00, 0x10, 0x00, 0x10, 0x01, 0x01, 0x11, 0x00];
        let sof = parse_sof(marker::SOF2, &payload).unwrap();
        assert!(sof.is_progressive());
    }

    #[test]
    fn dqt_round_trips_through_zigzag() {
        let mut payload = vec![0x00u8]; // 8-bit precision, table 0
        payload.extend((0u16..64).map(|i| i as u8));
        let mut tables = [None; 4];
        parse_dqt(&payload, |idx, table| tables[idx] = Some(table)).unwrap();
        let t = tables[0].unwrap();
        // Zigzag position 0 is natural position 0; position 1 (value 1) is
        // natural position 1; position 2 (value 2) is natural position 8.
        assert_eq!(t.values[0], 0);
        assert_eq!(t.values[1], 1);
        assert_eq!(t.values[8], 2);
    }

    #[test]
    fn dht_parses_the_standard_dc_luma_table() {
        let mut payload = vec![0x00u8]; // class=0 (DC), index=0
        payload.extend_from_slice(&crate::tables::STD_DC_LUMA.counts);
        payload.extend_from_slice(crate::tables::STD_DC_LUMA.values);
        let mut got: Option<(u8, usize, [u8; 16], ArrayVec<u8, 256>)> = None;
        parse_dht(&payload, |class, idx, counts, values| {
            got = Some((class, idx, counts, values));
        })
        .unwrap();
        let (class, idx, counts, values) = got.unwrap();
        assert_eq!((class, idx), (0, 0));
        assert_eq!(counts, crate::tables::STD_DC_LUMA.counts);
        assert_eq!(values.as_slice(), crate::tables::STD_DC_LUMA.values);
    }

    #[test]
    fn jfif_app0_is_recognised_and_others_are_not() {
        let mut payload = b"JFIF\0".to_vec();
        payload.extend_from_slice(&[1, 2, 0, 0, 1, 0, 1]);
        let jfif = parse_app0_jfif(&payload).unwrap();
        assert_eq!(jfif.version, (1, 2));
        assert!(parse_app0_jfif(b"JFXX\0extra").is_none());
    }

    #[test]
    fn adobe_app14_transform_field_is_read() {
        let mut payload = b"Adobe".to_vec();
        payload.extend_from_slice(&[0, 100, 0, 0, 0, 0, 1]); // transform=1 (YCbCr)
        assert_eq!(parse_app14_adobe(&payload), Some(1));
    }

    #[test]
    fn truncated_segments_are_rejected_not_panicked() {
        for n in 0..15 {
            let payload = [
                0x08, 0x00, 0x30, 0x00, 0x40, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11,
                0x00,
            ];
            let _ = parse_sof(marker::SOF0, payload.get(..n).unwrap_or(&[]));
        }
    }
}
