//! The genericity check: the same [`CbsFragment`] serves two codecs whose
//! framing, header width and unit numbering all differ.
//!
//! A shared layer that has only ever been exercised by one codec is not a
//! shared layer, it is that codec's helper with the names filed off. So this
//! test implements [`CbsCodec`] twice — once H.264-shaped (one-byte header,
//! five-bit type, `nal_ref_idc` in the header) and once HEVC-shaped (two-byte
//! header, six-bit type, layer and temporal ids) — and runs the same four
//! operations through both: split, drop by type, insert, assemble.
//!
//! The implementations here are deliberately minimal and local. This crate sits
//! at layer 3 and `vaco-format-nalu` at layer 4, so the real framing helpers
//! are *above* it and cannot be used; that is also why [`CbsCodec::split`] is a
//! trait method rather than something this crate provides.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]

use vaco_codec_cbs::{Cbs, CbsCodec, CbsFragment, CbsUnit, UnitOrigin};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};

/// The two framings both H.264 and HEVC arrive in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    AnnexB,
    /// ISO/IEC 14496-15 length prefix, in bytes.
    Length(u8),
}

/// Split an Annex B buffer, calling `type_of` for each unit's type.
fn split_annexb(
    data: &[u8],
    header_len: usize,
    type_of: &dyn Fn(&[u8]) -> Option<u32>,
    fragment: &mut CbsFragment,
    budget: &mut Budget,
) -> Result<()> {
    let mut i = 0usize;
    let mut starts: Vec<(usize, u8)> = Vec::new();
    while i + 2 < data.len() {
        if data.get(i) == Some(&0) && data.get(i + 1) == Some(&0) && data.get(i + 2) == Some(&1) {
            let four = i > 0 && data.get(i - 1) == Some(&0);
            starts.push((i + 3, if four { 4 } else { 3 }));
            i += 3;
        } else {
            i += 1;
        }
    }
    for (k, &(start, framing_len)) in starts.iter().enumerate() {
        let end = starts
            .get(k + 1)
            .map_or(data.len(), |&(s, l)| s - usize::from(l));
        let body = data.get(start..end).unwrap_or(&[]);
        if body.len() < header_len {
            continue;
        }
        let Some(t) = type_of(body) else { continue };
        fragment.push(
            CbsUnit::from_source(
                t,
                body.to_vec(),
                UnitOrigin {
                    offset: start,
                    framing_len,
                },
            ),
            budget,
        )?;
    }
    Ok(())
}

/// Split a length-prefixed buffer.
fn split_length(
    data: &[u8],
    size: u8,
    header_len: usize,
    type_of: &dyn Fn(&[u8]) -> Option<u32>,
    fragment: &mut CbsFragment,
    budget: &mut Budget,
) -> Result<()> {
    let n = usize::from(size);
    let mut pos = 0usize;
    while pos + n <= data.len() {
        let mut len = 0usize;
        for k in 0..n {
            len = (len << 8) | usize::from(*data.get(pos + k).unwrap_or(&0));
        }
        pos += n;
        let end = pos.checked_add(len).ok_or(Error::UnexpectedEof)?;
        if end > data.len() {
            return Err(Error::UnexpectedEof);
        }
        let body = data.get(pos..end).unwrap_or(&[]);
        pos = end;
        if body.len() < header_len {
            continue;
        }
        let Some(t) = type_of(body) else { continue };
        fragment.push(
            CbsUnit::from_source(
                t,
                body.to_vec(),
                UnitOrigin {
                    offset: pos - body.len(),
                    framing_len: size,
                },
            ),
            budget,
        )?;
    }
    Ok(())
}

fn assemble(fragment: &CbsFragment, framing: Framing, out: &mut Vec<u8>) -> Result<()> {
    for unit in fragment.units() {
        match framing {
            Framing::AnnexB => {
                // Four bytes when the source used four, three otherwise; a
                // synthesised unit gets four, which every decoder accepts.
                let n = unit.origin.map_or(4, |o| o.framing_len).clamp(3, 4);
                if n == 4 {
                    out.push(0);
                }
                out.extend_from_slice(&[0, 0, 1]);
            }
            Framing::Length(size) => {
                let len = unit.data.len() as u64;
                let cap = match size {
                    1 => 0xFF,
                    2 => 0xFFFF,
                    _ => 0xFFFF_FFFF,
                };
                if len > cap {
                    return Err(Error::InvalidData("unit too long for this length prefix"));
                }
                for k in (0..usize::from(size)).rev() {
                    out.push(((len >> (k * 8)) & 0xFF) as u8);
                }
            }
        }
        out.extend_from_slice(&unit.data);
    }
    Ok(())
}

// ------------------------------------------------------------------- H.264

/// The bit of H.264 syntax this test decodes: enough of an SPS to prove a
/// typed round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
enum H264Content {
    /// `nal_ref_idc` and the raw payload after the header byte.
    Other { nal_ref_idc: u8, payload: Vec<u8> },
}

#[derive(Debug, Default)]
struct H264Cbs;

impl CbsCodec for H264Cbs {
    type Content = H264Content;
    type Framing = Framing;
    const NAME: &'static str = "h264";

    fn split(
        &self,
        data: &[u8],
        framing: Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        let type_of = |b: &[u8]| b.first().map(|&h| u32::from(h & 0x1F));
        match framing {
            Framing::AnnexB => split_annexb(data, 1, &type_of, fragment, budget),
            Framing::Length(s) => split_length(data, s, 1, &type_of, fragment, budget),
        }
    }

    fn assemble(
        &self,
        fragment: &CbsFragment,
        framing: Framing,
        out: &mut Vec<u8>,
        _budget: &mut Budget,
    ) -> Result<()> {
        assemble(fragment, framing, out)
    }

    fn read_unit(&mut self, unit: &CbsUnit, _budget: &mut Budget) -> Result<H264Content> {
        let h = *unit.data.first().ok_or(Error::UnexpectedEof)?;
        Ok(H264Content::Other {
            nal_ref_idc: (h >> 5) & 0x03,
            payload: unit.data.get(1..).unwrap_or(&[]).to_vec(),
        })
    }

    fn write_unit(
        &mut self,
        content: &H264Content,
        out: &mut Vec<u8>,
        _budget: &mut Budget,
    ) -> Result<()> {
        let H264Content::Other {
            nal_ref_idc,
            payload,
        } = content;
        out.push((nal_ref_idc << 5) | 7);
        out.extend_from_slice(payload);
        Ok(())
    }

    fn content_unit_type(&self, _content: &H264Content) -> u32 {
        7 // SPS
    }
}

// -------------------------------------------------------------------- HEVC

#[derive(Debug, Clone, PartialEq, Eq)]
enum HevcContent {
    Other {
        nal_unit_type: u8,
        temporal_id: u8,
        payload: Vec<u8>,
    },
}

#[derive(Debug, Default)]
struct HevcCbs;

impl CbsCodec for HevcCbs {
    type Content = HevcContent;
    type Framing = Framing;
    const NAME: &'static str = "hevc";

    fn split(
        &self,
        data: &[u8],
        framing: Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        let type_of = |b: &[u8]| b.first().map(|&h| u32::from((h >> 1) & 0x3F));
        match framing {
            Framing::AnnexB => split_annexb(data, 2, &type_of, fragment, budget),
            Framing::Length(s) => split_length(data, s, 2, &type_of, fragment, budget),
        }
    }

    fn assemble(
        &self,
        fragment: &CbsFragment,
        framing: Framing,
        out: &mut Vec<u8>,
        _budget: &mut Budget,
    ) -> Result<()> {
        assemble(fragment, framing, out)
    }

    fn read_unit(&mut self, unit: &CbsUnit, _budget: &mut Budget) -> Result<HevcContent> {
        let a = *unit.data.first().ok_or(Error::UnexpectedEof)?;
        let b = *unit.data.get(1).ok_or(Error::UnexpectedEof)?;
        Ok(HevcContent::Other {
            nal_unit_type: (a >> 1) & 0x3F,
            temporal_id: (b & 0x07).saturating_sub(1),
            payload: unit.data.get(2..).unwrap_or(&[]).to_vec(),
        })
    }

    fn write_unit(
        &mut self,
        content: &HevcContent,
        out: &mut Vec<u8>,
        _budget: &mut Budget,
    ) -> Result<()> {
        let HevcContent::Other {
            nal_unit_type,
            temporal_id,
            payload,
        } = content;
        out.push(nal_unit_type << 1);
        out.push(temporal_id.saturating_add(1) & 0x07);
        out.extend_from_slice(payload);
        Ok(())
    }

    fn content_unit_type(&self, content: &HevcContent) -> u32 {
        let HevcContent::Other { nal_unit_type, .. } = content;
        u32::from(*nal_unit_type)
    }
}

// -------------------------------------------------------------------- tests

fn budget() -> Budget {
    Budget::new(Limits::strict())
}

/// SPS, PPS, SEI, IDR slice — H.264 numbering, four-byte start codes.
const H264_STREAM: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1E, //
    0, 0, 0, 1, 0x68, 0xEB, 0xE3, //
    0, 0, 0, 1, 0x06, 0x05, 0x02, //
    0, 0, 0, 1, 0x65, 0x88, 0x84,
];

/// VPS, SPS, PPS, prefix SEI, IDR slice — HEVC numbering.
const HEVC_STREAM: &[u8] = &[
    0, 0, 0, 1, 0x40, 0x01, 0x0C, //
    0, 0, 0, 1, 0x42, 0x01, 0x01, //
    0, 0, 0, 1, 0x44, 0x01, 0xC1, //
    0, 0, 0, 1, 0x4E, 0x01, 0x05, //
    0, 0, 0, 1, 0x26, 0x01, 0xAF,
];

#[test]
fn both_codecs_split_into_the_same_shape() {
    let mut b = budget();
    let mut f = CbsFragment::new();
    Cbs::new(H264Cbs)
        .split(H264_STREAM, Framing::AnnexB, &mut f, &mut b)
        .unwrap();
    assert_eq!(
        f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
        [7, 8, 6, 5]
    );

    let mut g = CbsFragment::new();
    Cbs::new(HevcCbs)
        .split(HEVC_STREAM, Framing::AnnexB, &mut g, &mut b)
        .unwrap();
    assert_eq!(
        g.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
        [32, 33, 34, 39, 19]
    );
}

/// The property the whole layer rests on: split then assemble, unchanged, must
/// give the bytes back.
#[test]
fn an_untouched_fragment_round_trips_byte_for_byte() {
    let mut b = budget();
    for (name, stream) in [("h264", H264_STREAM), ("hevc", HEVC_STREAM)] {
        let mut f = CbsFragment::new();
        let mut out = Vec::new();
        if name == "h264" {
            let mut c = Cbs::new(H264Cbs);
            c.split(stream, Framing::AnnexB, &mut f, &mut b).unwrap();
            c.assemble(&f, Framing::AnnexB, &mut out, &mut b).unwrap();
        } else {
            let mut c = Cbs::new(HevcCbs);
            c.split(stream, Framing::AnnexB, &mut f, &mut b).unwrap();
            c.assemble(&f, Framing::AnnexB, &mut out, &mut b).unwrap();
        }
        assert_eq!(out, stream, "{name}: round trip changed the bytes");
    }
}

/// `filter_units` on both codecs, through the identical fragment operation.
#[test]
fn dropping_sei_is_the_same_operation_in_both() {
    let mut b = budget();
    let mut c = Cbs::new(H264Cbs);
    let mut out = Vec::new();
    c.transform(
        H264_STREAM,
        Framing::AnnexB,
        Framing::AnnexB,
        &mut out,
        &mut b,
        |_, f, _| {
            f.retain(|u| u.unit_type != 6);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(out.len(), H264_STREAM.len() - 7);

    let mut c = Cbs::new(HevcCbs);
    let mut out = Vec::new();
    c.transform(
        HEVC_STREAM,
        Framing::AnnexB,
        Framing::AnnexB,
        &mut out,
        &mut b,
        |_, f, _| {
            f.retain(|u| u.unit_type != 39 && u.unit_type != 40);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(out.len(), HEVC_STREAM.len() - 7);
}

/// `*_mp4toannexb` in reverse: reframe without touching a unit.
#[test]
fn reframing_preserves_every_unit() {
    let mut b = budget();
    let mut c = Cbs::new(HevcCbs);
    let mut prefixed = Vec::new();
    c.transform(
        HEVC_STREAM,
        Framing::AnnexB,
        Framing::Length(4),
        &mut prefixed,
        &mut b,
        |_, _, _| Ok(()),
    )
    .unwrap();
    // Five units of three bytes each, plus five four-byte lengths.
    assert_eq!(prefixed.len(), 5 * (4 + 3));

    let mut back = Vec::new();
    c.transform(
        &prefixed,
        Framing::Length(4),
        Framing::AnnexB,
        &mut back,
        &mut b,
        |_, _, _| Ok(()),
    )
    .unwrap();
    // Annex B again, with four-byte start codes for units that have no origin
    // in an Annex B buffer — which is what a real `mp4toannexb` emits.
    assert_eq!(back, HEVC_STREAM);
}

/// A unit longer than a one-byte length prefix can express is refused, not
/// truncated.
#[test]
fn a_unit_too_long_for_the_framing_is_refused() {
    let mut b = budget();
    let mut c = Cbs::new(HevcCbs);
    let mut f = CbsFragment::new();
    f.push(CbsUnit::new(33, vec![0x42; 300]), &mut b).unwrap();
    let mut out = Vec::new();
    assert!(matches!(
        c.assemble(&f, Framing::Length(1), &mut out, &mut b),
        Err(Error::InvalidData(_))
    ));
}

/// The typed cycle: read a unit, change a field, write it back, and see the
/// change in the assembled bytes.
#[test]
fn the_typed_read_modify_write_cycle_changes_only_that_unit() {
    let mut b = budget();
    let mut c = Cbs::new(HevcCbs);
    let mut f = CbsFragment::new();
    c.split(HEVC_STREAM, Framing::AnnexB, &mut f, &mut b)
        .unwrap();

    let mut content = c.read_unit(&f, 1, &mut b).unwrap();
    let HevcContent::Other {
        ref mut temporal_id,
        ..
    } = content;
    *temporal_id = 3;
    c.update_unit(&mut f, 1, &content, &mut b).unwrap();

    let mut out = Vec::new();
    c.assemble(&f, Framing::AnnexB, &mut out, &mut b).unwrap();
    // Only the SPS's second header byte moved: 0x01 -> 0x04.
    let mut expected = HEVC_STREAM.to_vec();
    expected[12] = 0x04;
    assert_eq!(out, expected);
}

/// Inserting a parameter set at the front, which every `extract_extradata`
/// inverse and every `*_metadata` filter needs.
#[test]
fn an_inserted_unit_takes_its_type_from_the_content() {
    let mut b = budget();
    let mut c = Cbs::new(HevcCbs);
    let mut f = CbsFragment::new();
    c.split(HEVC_STREAM, Framing::AnnexB, &mut f, &mut b)
        .unwrap();
    let aud = HevcContent::Other {
        nal_unit_type: 35,
        temporal_id: 0,
        payload: vec![0x50],
    };
    c.insert_unit(&mut f, 0, &aud, &mut b).unwrap();
    assert_eq!(f.units()[0].unit_type, 35);
    assert!(f.units()[0].origin.is_none(), "a synthesised unit has none");
}

/// The H.264 typed path, so the trait is not merely *implementable* for a
/// second codec but actually exercised through the same session type.
#[test]
fn the_typed_cycle_works_for_h264_too() {
    let mut b = budget();
    let mut c = Cbs::new(H264Cbs);
    let mut f = CbsFragment::new();
    c.split(H264_STREAM, Framing::AnnexB, &mut f, &mut b)
        .unwrap();
    let content = c.read_unit(&f, 0, &mut b).unwrap();
    let H264Content::Other { nal_ref_idc, .. } = content;
    assert_eq!(nal_ref_idc, 3, "0x67 carries nal_ref_idc 3");
    c.update_unit(&mut f, 0, &content, &mut b).unwrap();
    let mut out = Vec::new();
    c.assemble(&f, Framing::AnnexB, &mut out, &mut b).unwrap();
    assert_eq!(out, H264_STREAM, "a no-op typed rewrite changes nothing");
}

/// Truncated and empty inputs produce an empty fragment or a clean error, and
/// never a panic.
#[test]
fn truncations_never_panic() {
    let mut b = budget();
    for n in 0..HEVC_STREAM.len() {
        let mut f = CbsFragment::new();
        let mut c = Cbs::new(HevcCbs);
        let _ = c.split(&HEVC_STREAM[..n], Framing::AnnexB, &mut f, &mut b);
        for size in [1u8, 2, 4] {
            let mut g = CbsFragment::new();
            let _ = c.split(&HEVC_STREAM[..n], Framing::Length(size), &mut g, &mut b);
            g.release(&mut b);
        }
        f.release(&mut b);
    }
}

/// Over arbitrary bytes: splitting never panics, every unit's bytes really came
/// from the input, and re-assembling in the framing it was read in gives a
/// buffer no longer than the original.
///
/// The last clause is the interesting one. A round trip cannot be *equality*
/// for arbitrary bytes — leading garbage before the first start code has no
/// unit to belong to and is dropped — but it must never grow, because a filter
/// that inflates a stream it did not edit is rewriting framing it was asked to
/// preserve.
#[test]
fn arbitrary_bytes_split_without_panicking() {
    use proptest::prelude::*;

    proptest!(|(data in proptest::collection::vec(
        prop_oneof![
            3 => Just(0u8),
            2 => Just(1u8),
            1 => Just(3u8),
            8 => any::<u8>(),
        ],
        0..512usize,
    ))| {
        let mut b = budget();
        let mut c = Cbs::new(HevcCbs);
        let mut f = CbsFragment::new();
        if c.split(&data, Framing::AnnexB, &mut f, &mut b).is_ok() {
            for unit in f.units() {
                let o = unit.origin.expect("a split unit has an origin");
                prop_assert!(o.offset + unit.data.len() <= data.len());
                prop_assert_eq!(
                    data.get(o.offset..o.offset + unit.data.len()),
                    Some(unit.data.as_slice())
                );
            }
            let mut out = Vec::new();
            c.assemble(&f, Framing::AnnexB, &mut out, &mut b).unwrap();
            prop_assert!(out.len() <= data.len());
        }
        f.release(&mut b);
    });
}
