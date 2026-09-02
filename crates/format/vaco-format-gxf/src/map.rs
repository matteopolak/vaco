//! The MAP packet (SMPTE 360-2009 clause 7.1): a 2-byte preamble, a
//! variable-length material data section, then a variable-length track
//! description section — each section a run of `tag(1) len(1) value(len)`
//! items, all multi-byte values big-endian ("most significant byte first",
//! clause 7.1.2.2 — the one exception to this Standard's otherwise
//! little-endian default, clause 4.3).
//!
//! Tag numbers below are Table 4 (material) and Table 6 (track); media
//! type numbers are Table 5. Every numeric tag this module decodes was
//! cross-checked against `tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf` byte for
//! byte before being trusted (see `demux.rs`'s own tests) — this crate's
//! understanding of GXF has both a published-standard leg and a real-file
//! leg, the same posture this project's MXF work reached.

use std::collections::HashMap;

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// The preamble's own fixed high nibble (clause 7.1.2.1, Table 3): version 0
/// in the low 5 bits, `1`s in the high 3.
const MAP_VERSION_0: u8 = 0xE0;

/// One `tag(1) len(1) value(len)` run, shared by the material data section
/// and every track's own description section (clause 7.1.3: "the same tag,
/// length, and value format as described above for the material data
/// section").
fn parse_tlv_items<'a>(bytes: &'a [u8], budget: &mut Budget) -> Result<HashMap<u8, &'a [u8]>> {
    let mut items = HashMap::new();
    let mut off = 0usize;
    while off < bytes.len() {
        budget.consume_fuel(1)?;
        let tag = *bytes
            .get(off)
            .ok_or(Error::InvalidData("gxf: truncated tag/length/value item"))?;
        let len = usize::from(
            *bytes
                .get(off + 1)
                .ok_or(Error::InvalidData("gxf: truncated tag/length/value item"))?,
        );
        let value = bytes.get(off + 2..off + 2 + len).ok_or(Error::InvalidData(
            "gxf: tag/length/value item overruns its section",
        ))?;
        items.insert(tag, value);
        off += 2 + len;
    }
    if off != bytes.len() {
        return Err(Error::InvalidData(
            "gxf: tag/length/value items did not exactly fill their declared section length",
        ));
    }
    Ok(items)
}

fn u32be(v: &[u8]) -> Result<u32> {
    <[u8; 4]>::try_from(v)
        .map(u32::from_be_bytes)
        .map_err(|_| Error::InvalidData("gxf: expected a 4-byte UINT32 value"))
}

fn i32be(v: &[u8]) -> Result<i32> {
    <[u8; 4]>::try_from(v)
        .map(i32::from_be_bytes)
        .map_err(|_| Error::InvalidData("gxf: expected a 4-byte INT32 value"))
}

/// A `STRING` value (clause 4.3): ASCII, `0x00`-terminated. The terminator
/// and anything past it (padding, reserved trailing bytes) are not part of
/// the string.
fn ascii_string(v: &[u8]) -> String {
    let end = v.iter().position(|&b| b == 0).unwrap_or(v.len());
    String::from_utf8_lossy(v.get(..end).unwrap_or(v)).into_owned()
}

/// Tag 0x40 through 0x45 (Table 4) — the values this crate has a use for.
/// Tags 0x46-0x4B are reserved and not surfaced.
#[derive(Debug, Clone, Default)]
pub struct MaterialData {
    pub media_file_name: Option<String>,
    pub first_field: u32,
    pub last_field: u32,
    pub mark_in: u32,
    pub mark_out: u32,
    pub estimated_size_1024_bytes: u32,
}

fn parse_material(items: &HashMap<u8, &[u8]>) -> Result<MaterialData> {
    let mut m = MaterialData::default();
    if let Some(&v) = items.get(&0x40) {
        m.media_file_name = Some(ascii_string(v));
    }
    if let Some(&v) = items.get(&0x41) {
        m.first_field = u32be(v)?;
    }
    if let Some(&v) = items.get(&0x42) {
        m.last_field = u32be(v)?;
    }
    if let Some(&v) = items.get(&0x43) {
        m.mark_in = u32be(v)?;
    }
    if let Some(&v) = items.get(&0x44) {
        m.mark_out = u32be(v)?;
    }
    if let Some(&v) = items.get(&0x45) {
        m.estimated_size_1024_bytes = u32be(v)?;
    }
    Ok(m)
}

/// One track's description (clause 7.1.3). `media_type`/`track_id` are
/// already de-biased (the `+0x80`/`+0xC0` clause 7.1.3 adds on the wire is
/// removed here, once, rather than left for every caller to repeat).
#[derive(Debug, Clone, Default)]
pub struct TrackDescription {
    pub media_type: u8,
    pub track_id: u8,
    pub media_file_name: Option<String>,
    /// Tag 0x4D: present for Motion JPEG, DV-based and audio/time-code
    /// tracks. Exactly one of this and `mpeg_video_aux` is ever set (clause
    /// 7.1.3: "Only one form of auxiliary information ... shall be valid
    /// for any one track").
    pub aux_binary: Option<[u8; 8]>,
    /// Tag 0x4F: present for MPEG (525/625 SD, HD, MPEG-1) tracks — the
    /// `Ipg`/`Ppi`/`Bpiop`/`Cf`/`Cg`/... newline-separated parameter string
    /// (clause 7.1.3.1, Table 7). Parsed only as far as this crate needs
    /// (see [`TrackDescription::mpeg_closed_gop`]); kept whole otherwise,
    /// since nothing downstream needs a structured form of it yet.
    pub mpeg_video_aux: Option<String>,
    /// Tag 0x50 (Table 6), verbatim: `-1` is "not applicable for this
    /// track type" (every non-video/time-code track states this, or the
    /// `-2` below), `-2` is "not available", and 1-8 are the eight defined
    /// rate codes. `None` only when the tag is absent. Resolving a code to
    /// an actual rate (and treating anything outside 1-8 as "no rate
    /// stated") is `demux.rs::frame_rate_code_to_fps`'s job, not this
    /// struct's — this field is the raw wire value.
    pub frame_rate_code: Option<i32>,
    /// Tag 0x51 (Table 6): raw code (1=525, 2=625, 4=1080, 6=720); not
    /// resolved further here since only `demux.rs`'s field-rate derivation
    /// needs it, and that only needs `frame_rate_code`.
    pub lines_per_frame_code: Option<i32>,
    /// Tag 0x52 (Table 6): `1` = progressive, `2` = interlaced.
    pub fields_per_frame_code: Option<i32>,
}

impl TrackDescription {
    /// One parameter out of `mpeg_video_aux`'s newline-separated list
    /// (Table 7) — linear scan over what is at most a few dozen bytes, so
    /// no need for a persistent parsed form.
    fn mpeg_aux_param<'a>(&'a self, name: &str) -> Option<&'a str> {
        let aux = self.mpeg_video_aux.as_deref()?;
        aux.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
    }

    /// `Cg` (Table 7): `true` for a closed GOP structure. `None` when this
    /// is not an MPEG track, or the parameter is absent.
    #[must_use]
    pub fn mpeg_closed_gop(&self) -> Option<bool> {
        self.mpeg_aux_param("Cg").map(|v| v != "0")
    }
}

fn parse_track(payload: &[u8], off: &mut usize) -> Result<TrackDescription> {
    let media_type = *payload
        .get(*off)
        .ok_or(Error::InvalidData("gxf: truncated track description"))?;
    let media_type = media_type.checked_sub(0x80).ok_or(Error::InvalidData(
        "gxf: track media type byte has no 0x80 bias",
    ))?;
    let track_id = *payload
        .get(*off + 1)
        .ok_or(Error::InvalidData("gxf: truncated track description"))?;
    let track_id = track_id
        .checked_sub(0xC0)
        .ok_or(Error::InvalidData("gxf: track id byte has no 0xC0 bias"))?;
    let desc_len = usize::from(u16::from_be_bytes(
        payload
            .get(*off + 2..*off + 4)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())
            .ok_or(Error::InvalidData(
                "gxf: truncated track description length",
            ))?,
    ));
    *off += 4;
    let desc = payload
        .get(*off..*off + desc_len)
        .ok_or(Error::InvalidData(
            "gxf: track description overruns the track section",
        ))?;
    *off += desc_len;

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let items = parse_tlv_items(desc, &mut budget)?;

    let mut t = TrackDescription {
        media_type,
        track_id,
        ..TrackDescription::default()
    };
    if let Some(&v) = items.get(&0x4C) {
        t.media_file_name = Some(ascii_string(v));
    }
    if let Some(&v) = items.get(&0x4D) {
        t.aux_binary = <[u8; 8]>::try_from(v).ok();
    }
    if let Some(&v) = items.get(&0x4F) {
        t.mpeg_video_aux = Some(ascii_string(v));
    }
    if let Some(&v) = items.get(&0x50) {
        t.frame_rate_code = i32be(v).ok();
    }
    if let Some(&v) = items.get(&0x51) {
        t.lines_per_frame_code = i32be(v).ok();
    }
    if let Some(&v) = items.get(&0x52) {
        t.fields_per_frame_code = i32be(v).ok();
    }
    Ok(t)
}

/// The parsed MAP packet payload (the packet header is [`crate::packet`]'s
/// concern, not this module's).
#[derive(Debug, Clone, Default)]
pub struct MapPacket {
    pub material: MaterialData,
    pub tracks: Vec<TrackDescription>,
}

/// Largest number of tracks this crate will build a [`TrackDescription`]
/// for. The Standard's own ceiling (clause 7.4.2.1.2): "Clips shall have 1
/// to 48 tracks in any combination of types."
pub const MAX_TRACKS: usize = 48;

/// Parse a MAP packet's payload (everything after its 16-byte packet
/// header).
///
/// # Errors
/// [`Error::InvalidData`] for a preamble that is not version 0, a
/// section-length field, or a tag/length/value item that runs past its
/// declared section. [`Error::Unsupported`] past [`MAX_TRACKS`].
pub fn parse(payload: &[u8], budget: &mut Budget) -> Result<MapPacket> {
    let preamble = payload.get(0..2).ok_or(Error::InvalidData(
        "gxf: map packet payload is shorter than its preamble",
    ))?;
    let version_byte = *preamble.first().ok_or(Error::InvalidData(
        "gxf: map packet payload is shorter than its preamble",
    ))?;
    if version_byte & 0xE0 != MAP_VERSION_0 {
        return Err(Error::Unsupported(
            "gxf: map packet version is not 0 (this crate reads only version 0, per SMPTE 360-2009)",
        ));
    }

    let mat_len = usize::from(u16::from_be_bytes(
        payload
            .get(2..4)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())
            .ok_or(Error::InvalidData(
                "gxf: truncated material data section length",
            ))?,
    ));
    let mat_bytes = payload.get(4..4 + mat_len).ok_or(Error::InvalidData(
        "gxf: material data section overruns the map packet",
    ))?;
    let material = parse_material(&parse_tlv_items(mat_bytes, budget)?)?;

    let mut off = 4 + mat_len;
    let trk_len = usize::from(u16::from_be_bytes(
        payload
            .get(off..off + 2)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())
            .ok_or(Error::InvalidData(
                "gxf: truncated track description section length",
            ))?,
    ));
    off += 2;
    let trk_end = off
        .checked_add(trk_len)
        .filter(|&e| e <= payload.len())
        .ok_or(Error::InvalidData(
            "gxf: track description section overruns the map packet",
        ))?;

    let mut tracks = Vec::new();
    while off < trk_end {
        budget.consume_fuel(1)?;
        if tracks.len() >= MAX_TRACKS {
            return Err(Error::Unsupported(
                "gxf: more tracks than SMPTE 360-2009's own stated maximum of 48",
            ));
        }
        tracks.push(parse_track(payload, &mut off)?);
    }
    if off != trk_end {
        return Err(Error::InvalidData(
            "gxf: track descriptions did not exactly fill their declared section length",
        ));
    }

    Ok(MapPacket { material, tracks })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    /// Bytes 16..368 of `tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf` — this
    /// crate's own first MAP packet's payload, transcribed once and used to
    /// pin the parser's understanding of the real file's shape without
    /// re-reading the fixture from every test.
    fn real_map_payload() -> Vec<u8> {
        let full = include_bytes!("../tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf");
        full.get(16..16 + 336).unwrap().to_vec()
    }

    #[test]
    fn parses_the_real_fixtures_material_section() {
        let mut b = Budget::new(Limits::permissive());
        let map = parse(&real_map_payload(), &mut b).unwrap();
        assert_eq!(
            map.material.media_file_name.as_deref(),
            Some("EXT:/PDR/default/sample_pal.gxf")
        );
        assert_eq!(map.material.first_field, 0);
        assert_eq!(map.material.last_field, 100);
        assert_eq!(map.material.estimated_size_1024_bytes, 465);
    }

    #[test]
    fn parses_the_real_fixtures_three_tracks() {
        let mut b = Budget::new(Limits::permissive());
        let map = parse(&real_map_payload(), &mut b).unwrap();
        assert_eq!(map.tracks.len(), 3);

        let video = &map.tracks[0];
        assert_eq!(video.media_type, 12); // MPEG-2 625 (Table 5)
        assert_eq!(video.track_id, 0);
        assert_eq!(
            video.media_file_name.as_deref(),
            Some("EXT:/PDR/default/ES.M0")
        );
        assert_eq!(video.frame_rate_code, Some(6)); // 25 fps (Table 6)
        assert_eq!(video.lines_per_frame_code, Some(2)); // 625 lines
        assert_eq!(video.fields_per_frame_code, Some(2)); // interlaced storage
        assert!(video.mpeg_video_aux.as_deref().unwrap().contains("Ipg 1"));
        assert_eq!(video.mpeg_closed_gop(), Some(true));

        let audio = &map.tracks[1];
        assert_eq!(audio.media_type, 10); // Audio PCM 16 (Table 5)
        assert_eq!(audio.track_id, 1);
        assert_eq!(
            audio.media_file_name.as_deref(),
            Some("EXT:/PDR/default/ES.A0")
        );
        assert_eq!(audio.frame_rate_code, Some(-2)); // "not available" (Table 6)
        assert_eq!(audio.aux_binary, Some([0u8; 8]));

        let tc = &map.tracks[2];
        assert_eq!(tc.media_type, 8); // Time code 625 (Table 5)
        assert_eq!(tc.track_id, 2);
    }

    #[test]
    fn a_wrong_map_version_is_unsupported() {
        let mut bytes = real_map_payload();
        bytes[0] = 0x1F; // low 5 bits changed; high 3 bits (0xE0) also cleared
        let mut b = Budget::new(Limits::permissive());
        assert!(matches!(parse(&bytes, &mut b), Err(Error::Unsupported(_))));
    }

    #[test]
    fn a_truncated_map_payload_is_invalid_data_not_a_panic() {
        let bytes = &real_map_payload()[..10];
        let mut b = Budget::new(Limits::permissive());
        assert!(parse(bytes, &mut b).is_err());
    }
}

fn push_tlv(buf: &mut Vec<u8>, tag: u8, value: &[u8]) {
    debug_assert!(
        value.len() <= 0xFF,
        "gxf: tag/length/value item value longer than a byte can state"
    );
    buf.push(tag);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "callers only ever pass values this crate itself sized to fit a u8 length"
    )]
    buf.push(value.len() as u8);
    buf.extend_from_slice(value);
}

fn ascii_string_value(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// Encode a [`MapPacket`] back into wire bytes (its packet payload, not
/// including the 16-byte packet header `crate::packet` owns).
///
/// This is the write side `crate::mux` needs; it is also what makes
/// `map::tests::round_trips_through_encode_and_parse` a real check that
/// [`parse`] and this function agree on the wire format, independent of
/// the real fixture.
///
/// # Panics
/// In debug builds, if a string or auxiliary value this crate itself built
/// is longer than 255 bytes (a tag/length/value item's length is one
/// byte) — a bug in the caller composing the [`MapPacket`], not a
/// user-facing error.
#[must_use]
pub fn encode(map: &MapPacket) -> Vec<u8> {
    let mut material = Vec::new();
    if let Some(name) = &map.material.media_file_name {
        push_tlv(&mut material, 0x40, &ascii_string_value(name));
    }
    push_tlv(&mut material, 0x41, &map.material.first_field.to_be_bytes());
    push_tlv(&mut material, 0x42, &map.material.last_field.to_be_bytes());
    push_tlv(&mut material, 0x43, &map.material.mark_in.to_be_bytes());
    push_tlv(&mut material, 0x44, &map.material.mark_out.to_be_bytes());
    push_tlv(
        &mut material,
        0x45,
        &map.material.estimated_size_1024_bytes.to_be_bytes(),
    );

    let mut tracks = Vec::new();
    for t in &map.tracks {
        tracks.push(t.media_type.wrapping_add(0x80));
        tracks.push(t.track_id.wrapping_add(0xC0));
        let mut desc = Vec::new();
        if let Some(name) = &t.media_file_name {
            push_tlv(&mut desc, 0x4C, &ascii_string_value(name));
        }
        if let Some(aux) = &t.aux_binary {
            push_tlv(&mut desc, 0x4D, aux);
        }
        if let Some(aux) = &t.mpeg_video_aux {
            push_tlv(&mut desc, 0x4F, &ascii_string_value(aux));
        }
        if let Some(v) = t.frame_rate_code {
            push_tlv(&mut desc, 0x50, &v.to_be_bytes());
        }
        if let Some(v) = t.lines_per_frame_code {
            push_tlv(&mut desc, 0x51, &v.to_be_bytes());
        }
        if let Some(v) = t.fields_per_frame_code {
            push_tlv(&mut desc, 0x52, &v.to_be_bytes());
        }
        let desc_len = u16::try_from(desc.len()).unwrap_or(u16::MAX);
        tracks.extend_from_slice(&desc_len.to_be_bytes());
        tracks.extend_from_slice(&desc);
    }

    let mut out = vec![MAP_VERSION_0, 0xFF];
    let mat_len = u16::try_from(material.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&mat_len.to_be_bytes());
    out.extend_from_slice(&material);
    let trk_len = u16::try_from(tracks.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&trk_len.to_be_bytes());
    out.extend_from_slice(&tracks);
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod encode_tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn round_trips_through_encode_and_parse() {
        let original = MapPacket {
            material: MaterialData {
                media_file_name: Some("EXT:/PDR/default/out.gxf".to_owned()),
                first_field: 0,
                last_field: 100,
                mark_in: 0,
                mark_out: 100,
                estimated_size_1024_bytes: 465,
            },
            tracks: vec![
                TrackDescription {
                    media_type: 12,
                    track_id: 0,
                    media_file_name: Some("EXT:/PDR/default/ES.M0".to_owned()),
                    mpeg_video_aux: Some("Ver 1\nBr 200000.000000\n".to_owned()),
                    frame_rate_code: Some(6),
                    lines_per_frame_code: Some(2),
                    fields_per_frame_code: Some(2),
                    ..TrackDescription::default()
                },
                TrackDescription {
                    media_type: 10,
                    track_id: 1,
                    media_file_name: Some("EXT:/PDR/default/ES.A0".to_owned()),
                    aux_binary: Some([0u8; 8]),
                    ..TrackDescription::default()
                },
            ],
        };
        let bytes = encode(&original);
        let mut b = Budget::new(Limits::permissive());
        let parsed = parse(&bytes, &mut b).unwrap();
        assert_eq!(
            parsed.material.media_file_name,
            original.material.media_file_name
        );
        assert_eq!(parsed.material.last_field, original.material.last_field);
        assert_eq!(
            parsed.material.estimated_size_1024_bytes,
            original.material.estimated_size_1024_bytes
        );
        assert_eq!(parsed.tracks.len(), 2);
        assert_eq!(parsed.tracks[0].media_type, 12);
        assert_eq!(parsed.tracks[0].track_id, 0);
        assert_eq!(parsed.tracks[0].frame_rate_code, Some(6));
        assert_eq!(parsed.tracks[1].media_type, 10);
        assert_eq!(parsed.tracks[1].track_id, 1);
        assert_eq!(parsed.tracks[1].aux_binary, Some([0u8; 8]));
    }
}
