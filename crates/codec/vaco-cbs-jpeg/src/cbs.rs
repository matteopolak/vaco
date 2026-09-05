//! [`vaco_codec_cbs::CbsCodec`] for JPEG: [`JpegCbs`] splits a file into
//! marker segments and entropy-coded scan spans, and decodes/encodes the
//! three typed segments [`crate::header`] covers.

use vaco_codec_cbs::{CbsCodec, CbsFragment, CbsUnit, UnitOrigin};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::header::{
    FrameHeader, HuffmanTable, QuantTable, parse_dht, parse_dqt, write_dht, write_dqt,
};

const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const TEM: u8 = 0x01;
const RST0: u8 = 0xD0;
const RST7: u8 = 0xD7;
const SOS: u8 = 0xDA;
const DQT: u8 = 0xDB;
const DHT: u8 = 0xC4;

/// Every `SOF` marker byte, Table B.1: `0xC0..=0xCF` except `DHT` (`0xC4`),
/// the reserved `JPG` (`0xC8`) and `DAC` (`0xCC`).
fn is_sof(marker: u8) -> bool {
    (0xC0..=0xCF).contains(&marker) && !matches!(marker, DHT | 0xC8 | 0xCC)
}

/// Whether `marker` carries no length-prefixed payload at all: `SOI`, `EOI`,
/// `TEM`, the restart markers, and the reserved `0x02..=0xBF` range.
fn has_no_payload(marker: u8) -> bool {
    matches!(marker, SOI | EOI | TEM | RST0..=RST7) || matches!(marker, 0x02..=0xBF)
}

/// The sentinel [`CbsUnit::unit_type`] for an entropy-coded scan span —
/// distinct from any real marker byte, which is at most `0xFF` (255).
pub const SCAN_DATA_UNIT_TYPE: u32 = 0x100;

/// The sentinel [`CbsUnit::unit_type`] for one `0xFF` padding-fill byte
/// (§B.1.1.5) ahead of a real marker. Read back as
/// [`JpegContent::Marker(0xFF)`](JpegContent::Marker) — see that variant's
/// write-side handling for why `0xFF` is special-cased to one byte rather
/// than the usual two.
pub const FILL_UNIT_TYPE: u32 = 0x102;

/// JPEG has one framing shape: a file starting `SOI`, ending `EOI`. Unit
/// struct purely to satisfy [`CbsCodec::Framing`]'s shape, the same reason
/// `vaco-cbs-vp9::Vp9Framing` is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JpegFraming;

/// One unit's typed content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JpegContent {
    /// `SOI`, `EOI`, another no-payload marker, or (`Marker(0xFF)`, a special
    /// case) one `0xFF` padding-fill byte ahead of a real marker — see
    /// [`FILL_UNIT_TYPE`].
    Marker(u8),
    /// The entropy-coded bytes following an `SOS` segment, exactly as they
    /// appear — restart markers and `0xFF 0x00` byte-stuffing intact.
    ScanData(Vec<u8>),
    Sof(FrameHeader),
    Dqt(Vec<QuantTable>),
    Dht(Vec<HuffmanTable>),
    /// Anything else: `APPn`, `COM`, `DRI`, `SOS`'s own header, and any
    /// marker this crate does not type.
    Raw {
        marker: u8,
        payload: Vec<u8>,
    },
}

impl JpegContent {
    /// The marker byte this content would be written as, or
    /// [`SCAN_DATA_UNIT_TYPE`] for scan data.
    #[must_use]
    pub fn unit_type(&self) -> u32 {
        match self {
            Self::Marker(0xFF) => FILL_UNIT_TYPE,
            Self::Marker(m) | Self::Raw { marker: m, .. } => u32::from(*m),
            Self::ScanData(_) => SCAN_DATA_UNIT_TYPE,
            Self::Sof(h) => u32::from(h.sof_marker),
            Self::Dqt(_) => u32::from(DQT),
            Self::Dht(_) => u32::from(DHT),
        }
    }
}

/// The JPEG [`CbsCodec`]. Holds nothing: every segment is self-delimiting,
/// so there is no cross-segment state a split or a typed read needs.
#[derive(Debug, Default, Clone, Copy)]
pub struct JpegCbs;

impl JpegCbs {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CbsCodec for JpegCbs {
    type Content = JpegContent;
    type Framing = JpegFraming;
    const NAME: &'static str = "jpeg";

    fn split(
        &self,
        data: &[u8],
        _framing: JpegFraming,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        let mut i = 0usize;
        while i < data.len() {
            if data.get(i) != Some(&0xFF) {
                return Err(Error::InvalidData("jpeg: expected a marker"));
            }
            let Some(&marker) = data.get(i + 1) else {
                return Err(Error::InvalidData("jpeg: truncated marker"));
            };
            // A `0xFF` padding-fill byte ahead of a real marker (§B.1.1.5):
            // kept as its own one-byte unit rather than skipped outright —
            // losing it was a real bug this crate's own fuzzing caught
            // (`FF FF 0A` split into one two-byte unit, discarding the
            // leading `FF` with no unit to account for it). Emitting it
            // separately, rather than folding it into the next marker's
            // unit, keeps every other unit's bytes starting exactly at their
            // own `0xFF <marker>` pair — the assumption `read_unit`'s fixed
            // 4-byte-header skip depends on.
            if marker == 0xFF {
                fragment.push(
                    CbsUnit::from_source(FILL_UNIT_TYPE, vec![0xFF], origin(i)),
                    budget,
                )?;
                i += 1;
                continue;
            }
            if has_no_payload(marker) {
                let unit_data = data.get(i..i + 2).unwrap_or(&[]).to_vec();
                fragment.push(
                    CbsUnit::from_source(u32::from(marker), unit_data, origin(i)),
                    budget,
                )?;
                i += 2;
                continue;
            }
            let Some(len_bytes) = data.get(i + 2..i + 4) else {
                return Err(Error::InvalidData("jpeg: truncated segment length"));
            };
            let length = usize::from(len_bytes.first().copied().unwrap_or(0)) << 8
                | usize::from(len_bytes.get(1).copied().unwrap_or(0));
            if length < 2 {
                return Err(Error::InvalidData("jpeg: segment length under 2"));
            }
            let seg_end = i.checked_add(2 + length).ok_or(Error::InvalidData(
                "jpeg: segment length overflows the buffer",
            ))?;
            let unit_data = data
                .get(i..seg_end)
                .ok_or(Error::InvalidData("jpeg: segment runs past the buffer"))?
                .to_vec();
            fragment.push(
                CbsUnit::from_source(u32::from(marker), unit_data, origin(i)),
                budget,
            )?;
            i = seg_end;

            if marker == SOS {
                let scan_start = i;
                // The smallest `j >= scan_start` where `data[j..j+2]` is a
                // real marker (not a byte-stuffed `0xFF 0x00` and not a
                // restart marker) — that is where the scan data ends and the
                // next unit begins. A truncated scan (no such `j`) still
                // yields whatever bytes are there: a demuxer sample cut
                // mid-scan is not this layer's error to raise, the same
                // forgiving-demux stance `vaco_codec_cbs`'s own H.26x/AV1
                // codecs take on a short final unit.
                let mut scan_end = data.len();
                let mut j = scan_start;
                while j + 1 < data.len() {
                    let (Some(&a), Some(&b)) = (data.get(j), data.get(j + 1)) else {
                        break;
                    };
                    if a == 0xFF && b != 0x00 && !(RST0..=RST7).contains(&b) {
                        scan_end = j;
                        break;
                    }
                    j += 1;
                }
                let scan_bytes = data.get(scan_start..scan_end).unwrap_or(&[]).to_vec();
                if !scan_bytes.is_empty() {
                    fragment.push(
                        CbsUnit::from_source(SCAN_DATA_UNIT_TYPE, scan_bytes, origin(scan_start)),
                        budget,
                    )?;
                }
                i = scan_end;
            }
        }
        Ok(())
    }

    fn assemble(
        &self,
        fragment: &CbsFragment,
        _framing: JpegFraming,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        let total: u64 = fragment.units().iter().map(|u| u.data.len() as u64).sum();
        budget.check(total)?;
        for u in fragment.units() {
            out.extend_from_slice(&u.data);
        }
        Ok(())
    }

    fn read_unit(&mut self, unit: &CbsUnit, _budget: &mut Budget) -> Result<JpegContent> {
        if unit.unit_type == SCAN_DATA_UNIT_TYPE {
            return Ok(JpegContent::ScanData(unit.data.clone()));
        }
        if unit.unit_type == FILL_UNIT_TYPE {
            return Ok(JpegContent::Marker(0xFF));
        }
        let marker = u8::try_from(unit.unit_type)
            .map_err(|_| Error::InvalidData("jpeg: unit_type out of range for a marker byte"))?;
        if has_no_payload(marker) {
            return Ok(JpegContent::Marker(marker));
        }
        let payload = unit.data.get(4..).unwrap_or(&[]);
        Ok(match marker {
            m if is_sof(m) => JpegContent::Sof(FrameHeader::parse(m, payload)?),
            DQT => JpegContent::Dqt(parse_dqt(payload)?),
            DHT => JpegContent::Dht(parse_dht(payload)?),
            m => JpegContent::Raw {
                marker: m,
                payload: payload.to_vec(),
            },
        })
    }

    fn write_unit(
        &mut self,
        content: &JpegContent,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        match content {
            JpegContent::Marker(0xFF) => {
                budget.check(1)?;
                out.push(0xFF);
            }
            JpegContent::Marker(m) => {
                budget.check(2)?;
                out.push(0xFF);
                out.push(*m);
            }
            JpegContent::ScanData(bytes) => {
                budget.check(bytes.len() as u64)?;
                out.extend_from_slice(bytes);
            }
            JpegContent::Sof(h) => write_segment(out, budget, h.sof_marker, &h.write())?,
            JpegContent::Dqt(tables) => write_segment(out, budget, DQT, &write_dqt(tables))?,
            JpegContent::Dht(tables) => write_segment(out, budget, DHT, &write_dht(tables))?,
            JpegContent::Raw { marker, payload } => write_segment(out, budget, *marker, payload)?,
        }
        Ok(())
    }

    fn content_unit_type(&self, content: &JpegContent) -> u32 {
        content.unit_type()
    }
}

fn origin(offset: usize) -> UnitOrigin {
    UnitOrigin {
        offset,
        // JPEG's framing has no fixed-width prefix to report the way a NAL
        // start code does: a marker is always two bytes, but a length-
        // prefixed segment's own two-byte length field is *part of* the unit
        // this crate keeps (see the module doc — `unit.data` includes it),
        // not framing ahead of it. Zero is the honest answer.
        framing_len: 0,
    }
}

/// Write one length-prefixed marker segment: `0xFF <marker> <len:be16>
/// <payload>`, where `len` is `payload.len() + 2` (the length field counts
/// itself, §B.1.1.4).
fn write_segment(out: &mut Vec<u8>, budget: &mut Budget, marker: u8, payload: &[u8]) -> Result<()> {
    let len = payload.len().checked_add(2).ok_or(Error::InvalidData(
        "jpeg: segment too long to have a length field",
    ))?;
    let len_u16 = u16::try_from(len)
        .map_err(|_| Error::InvalidData("jpeg: segment too long for a 16-bit length"))?;
    budget.check((payload.len() + 4) as u64)?;
    out.push(0xFF);
    out.push(marker);
    out.extend_from_slice(&len_u16.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_codec_cbs::Cbs;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// `ffmpeg -f lavfi -i testsrc2=size=64x48 -frames:v 1`: `APP0` (JFIF),
    /// `COM`, `DQT`, `DHT`, `SOF0`, `SOS` plus its scan data, `EOI`.
    fn baseline_jpeg() -> Vec<u8> {
        include_bytes!("../tests/fixtures/baseline.jpg").to_vec()
    }

    #[test]
    fn a_real_jpeg_splits_into_the_expected_marker_sequence() {
        let data = baseline_jpeg();
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&data, JpegFraming, &mut f, &mut b)
            .expect("splits");
        let types: Vec<u32> = f.units().iter().map(|u| u.unit_type).collect();
        assert_eq!(
            types,
            vec![
                u32::from(SOI),
                0xE0,
                0xFE,
                u32::from(DQT),
                u32::from(DHT),
                0xC0,
                u32::from(SOS),
                SCAN_DATA_UNIT_TYPE,
                u32::from(EOI),
            ]
        );
        f.release(&mut b);
    }

    /// The property every CBS layer in this project rests on.
    #[test]
    fn an_untouched_fragment_round_trips_byte_for_byte() {
        let data = baseline_jpeg();
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            JpegFraming,
            JpegFraming,
            &mut out,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("transform");
        assert_eq!(out, data);
    }

    /// The real `SOF0`, `DQT` and `DHT` segments read to their typed form
    /// and write straight back with no edit, byte for byte.
    #[test]
    fn sof_dqt_and_dht_round_trip_bit_exactly_with_no_edit() {
        let data = baseline_jpeg();
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, JpegFraming, &mut f, &mut b)
            .expect("splits");

        for (idx, want_kind) in [(3, "dqt"), (4, "dht"), (5, "sof")] {
            let content = cbs.read_unit(&f, idx, &mut b).expect("reads");
            match (&content, want_kind) {
                (JpegContent::Dqt(_), "dqt")
                | (JpegContent::Dht(_), "dht")
                | (JpegContent::Sof(_), "sof") => {}
                other => panic!("unit {idx}: expected {want_kind}, got {other:?}"),
            }
            let before = f.units()[idx].data.clone();
            cbs.update_unit(&mut f, idx, &content, &mut b)
                .expect("rewrites");
            assert_eq!(
                f.units()[idx].data,
                before,
                "unit {idx} re-encodes identically"
            );
        }
        f.release(&mut b);
    }

    /// A field edit through the typed `SOF0` changes only that field.
    #[test]
    fn editing_a_typed_field_changes_only_that_field() {
        let data = baseline_jpeg();
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, JpegFraming, &mut f, &mut b)
            .expect("splits");

        let JpegContent::Sof(mut sof) = cbs.read_unit(&f, 5, &mut b).expect("reads") else {
            panic!("expected a SOF");
        };
        let original_components = sof.components.clone();
        sof.precision = 12;
        cbs.update_unit(&mut f, 5, &JpegContent::Sof(sof), &mut b)
            .expect("rewrites");

        let JpegContent::Sof(sof) = cbs.read_unit(&f, 5, &mut b).expect("re-reads") else {
            panic!("expected a SOF");
        };
        assert_eq!(sof.precision, 12, "the edited field stuck");
        assert_eq!(sof.components, original_components, "nothing else moved");
        f.release(&mut b);
    }

    /// `filter_units`, over marker segments: drop the `COM` marker, keep the
    /// scan data untouched.
    #[test]
    fn dropping_a_marker_leaves_the_scan_data_untouched() {
        let data = baseline_jpeg();
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            JpegFraming,
            JpegFraming,
            &mut out,
            &mut b,
            |_, f, _| {
                f.retain(|u| u.unit_type != 0xFE);
                Ok(())
            },
        )
        .expect("transform");
        // The COM segment's own length field (16) already counts its own
        // two bytes, so the whole segment on the wire is `2 + 16` bytes:
        // the `0xFF 0xFE` marker plus everything the length field spans.
        assert_eq!(out.len(), data.len() - 18, "the whole COM segment is gone");
        assert!(!out.windows(2).any(|w| w == [0xFF, 0xFE]));
    }

    /// A hand-built two-scan file (real marker bytes, arbitrary scan
    /// payload) exercises the loop that must not stop at the first `SOS`'s
    /// own scan data — a real progressive `mjpeg` encode was not available
    /// in this environment to capture one (`ffmpeg`'s built-in `mjpeg`
    /// encoder is baseline-only), so this is built directly from §B.2.3's
    /// shape instead.
    #[test]
    fn a_hand_built_two_scan_file_splits_and_reassembles() {
        let mut data = vec![0xFF, SOI];
        // First SOS: header length 8, then scan bytes including a stuffed 0xFF00
        // and a restart marker, both of which must NOT end the span.
        data.extend_from_slice(&[0xFF, SOS, 0x00, 0x08, 1, 2, 3, 4, 5, 6]);
        data.extend_from_slice(&[0xAA, 0xFF, 0x00, 0xBB, 0xFF, RST0, 0xCC]);
        // Second SOS: a fresh header, ending the file at EOI.
        data.extend_from_slice(&[0xFF, SOS, 0x00, 0x08, 7, 8, 9, 10, 11, 12]);
        data.extend_from_slice(&[0xDD, 0xEE]);
        data.extend_from_slice(&[0xFF, EOI]);

        let mut cbs = Cbs::new(JpegCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, JpegFraming, &mut f, &mut b)
            .expect("splits");
        let types: Vec<u32> = f.units().iter().map(|u| u.unit_type).collect();
        assert_eq!(
            types,
            vec![
                u32::from(SOI),
                u32::from(SOS),
                SCAN_DATA_UNIT_TYPE,
                u32::from(SOS),
                SCAN_DATA_UNIT_TYPE,
                u32::from(EOI),
            ]
        );
        let mut out = Vec::new();
        cbs.assemble(&f, JpegFraming, &mut out, &mut b)
            .expect("assembles");
        assert_eq!(out, data);
        f.release(&mut b);
    }

    #[test]
    fn every_truncation_splits_and_reads_without_panicking() {
        let data = baseline_jpeg();
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut b = budget();
        for n in (0..data.len()).step_by(23) {
            let mut f = CbsFragment::new();
            let _ = cbs.split(&data[..n], JpegFraming, &mut f, &mut b);
            for i in 0..f.len() {
                let _ = cbs.read_unit(&f, i, &mut b);
            }
            f.release(&mut b);
        }
    }

    /// §B.1.1.5: a `0xFF` fill byte may precede any marker. `split` used to
    /// silently drop it (`i += 1; continue;` with no unit ever recording it),
    /// so `assemble` reproduced two bytes instead of three. Each fill byte is
    /// now its own [`FILL_UNIT_TYPE`] unit that reads back as
    /// `JpegContent::Marker(0xFF)` and writes back as the single byte it is.
    #[test]
    fn a_fill_byte_ahead_of_a_marker_is_not_dropped() {
        let data = vec![0xFF, 0xFF, TEM]; // fill byte, then TEM marker
        let mut cbs = Cbs::new(JpegCbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&data, JpegFraming, &mut f, &mut b)
            .expect("splits");
        let types: Vec<u32> = f.units().iter().map(|u| u.unit_type).collect();
        assert_eq!(types, vec![FILL_UNIT_TYPE, u32::from(TEM)]);

        let content = cbs.read_unit(&f, 0, &mut b).expect("reads");
        assert_eq!(content, JpegContent::Marker(0xFF));

        let mut out = Vec::new();
        cbs.assemble(&f, JpegFraming, &mut out, &mut b)
            .expect("assembles");
        assert_eq!(
            out, data,
            "the fill byte must survive an untouched round trip"
        );
        f.release(&mut b);
    }
}
