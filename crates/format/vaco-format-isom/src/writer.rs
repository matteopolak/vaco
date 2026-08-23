//! Box **writers**: the production counterpart to this crate's readers.
//!
//! [`crate::build`] exists to make *fixtures* — deliberately literal, and
//! willing to write invalid shapes on request, because half of what it feeds
//! is negative tests. This module is the other thing: every function here
//! writes a box that this crate's own reader is expected to parse back to the
//! same meaning, and none of them accept a shape the specification forbids.
//!
//! `vaco-mux-mp4` is the only intended caller. It supplies typed fields it
//! already has (from `CodecParameters`, from the sample tables it has been
//! accumulating); this module supplies the byte layout ISO/IEC 14496-12/-14/-15
//! and 23001-7 specify. Nothing here allocates a sample-indexed structure —
//! the caller passes already-computed runs, exactly as [`crate::stbl`] expects
//! to read them back.
//!
//! Box framing (the four-byte size, the four-character type, and the
//! version/flags header of a full box) is not redefined here: every function
//! composes [`crate::build::bx`] and [`crate::build::fullbx`], which is the
//! one place that concept lives (D19).

use crate::build::{bx, fullbx};
use crate::esds::{
    TAG_DECODER_CONFIG, TAG_DECODER_SPECIFIC, TAG_ES, TAG_SL_CONFIG, write_expandable,
};
use crate::fourcc::FourCc;

// --------------------------------------------------------------- file type

/// `ftyp`/`styp`: major brand, minor version, compatible brands.
#[must_use]
pub fn file_type(
    kind: &[u8; 4],
    major: FourCc,
    minor_version: u32,
    compatible: &[FourCc],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&major.as_bytes());
    body.extend_from_slice(&minor_version.to_be_bytes());
    for c in compatible {
        body.extend_from_slice(&c.as_bytes());
    }
    bx(kind, &body)
}

// -------------------------------------------------------------- movie/track

/// The fields [`mvhd`] needs, in the units the box stores them.
#[derive(Debug, Clone, Copy)]
pub struct MvhdFields {
    /// Seconds since the 1904 epoch, or 0 for "unstated" ([`crate::movie::from_unix_time`]).
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    /// 16.16 fixed-point playback rate. `0x0001_0000` is normal speed.
    pub rate: i32,
    /// 8.8 fixed-point volume. `0x0100` is full.
    pub volume: i16,
    /// Row-major display matrix, file order. [`crate::fixed::IDENTITY_MATRIX`]
    /// for no transform.
    pub matrix: [u32; 9],
    pub next_track_id: u32,
}

/// `mvhd` (ISO/IEC 14496-12 §8.2.2). Version 1 is chosen automatically when a
/// time or the duration does not fit 32 bits, matching what a reader must
/// already handle either way.
#[must_use]
pub fn mvhd(f: &MvhdFields) -> Vec<u8> {
    let wide = f.creation_time > u64::from(u32::MAX)
        || f.modification_time > u64::from(u32::MAX)
        || f.duration > u64::from(u32::MAX);
    let mut b = Vec::new();
    if wide {
        b.extend_from_slice(&f.creation_time.to_be_bytes());
        b.extend_from_slice(&f.modification_time.to_be_bytes());
        b.extend_from_slice(&f.timescale.to_be_bytes());
        b.extend_from_slice(&f.duration.to_be_bytes());
    } else {
        b.extend_from_slice(&(f.creation_time as u32).to_be_bytes());
        b.extend_from_slice(&(f.modification_time as u32).to_be_bytes());
        b.extend_from_slice(&f.timescale.to_be_bytes());
        b.extend_from_slice(&(f.duration as u32).to_be_bytes());
    }
    b.extend_from_slice(&f.rate.to_be_bytes());
    b.extend_from_slice(&f.volume.to_be_bytes());
    b.extend_from_slice(&[0u8; 10]); // reserved
    for v in f.matrix {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&[0u8; 24]); // pre_defined
    b.extend_from_slice(&f.next_track_id.to_be_bytes());
    fullbx(b"mvhd", u8::from(wide), 0, &b)
}

/// `tkhd` flag bits (§8.3.2.2). Plain constants rather than a `bitflags` type:
/// this crate has no dependency on that crate and three bits do not need one.
pub mod tkhd_flags {
    pub const ENABLED: u32 = 0x0000_0001;
    pub const IN_MOVIE: u32 = 0x0000_0002;
    pub const IN_PREVIEW: u32 = 0x0000_0004;
}

/// The fields [`tkhd`] needs.
#[derive(Debug, Clone, Copy)]
pub struct TkhdFields {
    /// OR of [`tkhd_flags`] bits.
    pub flags: u32,
    pub creation_time: u64,
    pub modification_time: u64,
    pub track_id: u32,
    /// In the *movie* timescale, not the track's own.
    pub duration: u64,
    pub layer: i16,
    pub alternate_group: i16,
    /// 8.8 fixed-point; `0x0100` for an audio track, `0` for video.
    pub volume: i16,
    pub matrix: [u32; 9],
    /// 16.16 fixed-point display width/height.
    pub width: u32,
    pub height: u32,
}

/// `tkhd` (§8.3.2).
#[must_use]
pub fn tkhd(f: &TkhdFields) -> Vec<u8> {
    let wide = f.creation_time > u64::from(u32::MAX)
        || f.modification_time > u64::from(u32::MAX)
        || f.duration > u64::from(u32::MAX);
    let mut b = Vec::new();
    if wide {
        b.extend_from_slice(&f.creation_time.to_be_bytes());
        b.extend_from_slice(&f.modification_time.to_be_bytes());
        b.extend_from_slice(&f.track_id.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // reserved
        b.extend_from_slice(&f.duration.to_be_bytes());
    } else {
        b.extend_from_slice(&(f.creation_time as u32).to_be_bytes());
        b.extend_from_slice(&(f.modification_time as u32).to_be_bytes());
        b.extend_from_slice(&f.track_id.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(f.duration as u32).to_be_bytes());
    }
    b.extend_from_slice(&[0u8; 8]); // reserved
    b.extend_from_slice(&f.layer.to_be_bytes());
    b.extend_from_slice(&f.alternate_group.to_be_bytes());
    b.extend_from_slice(&f.volume.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for v in f.matrix {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&f.width.to_be_bytes());
    b.extend_from_slice(&f.height.to_be_bytes());
    fullbx(b"tkhd", u8::from(wide), f.flags, &b)
}

/// The fields [`mdhd`] needs.
#[derive(Debug, Clone, Copy)]
pub struct MdhdFields {
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    /// Packed ISO-639-2/T, e.g. [`crate::lang::Language::pack`].
    pub language: u16,
}

/// `mdhd` (§8.4.2).
#[must_use]
pub fn mdhd(f: &MdhdFields) -> Vec<u8> {
    let wide = f.creation_time > u64::from(u32::MAX)
        || f.modification_time > u64::from(u32::MAX)
        || f.duration > u64::from(u32::MAX);
    let mut b = Vec::new();
    if wide {
        b.extend_from_slice(&f.creation_time.to_be_bytes());
        b.extend_from_slice(&f.modification_time.to_be_bytes());
        b.extend_from_slice(&f.timescale.to_be_bytes());
        b.extend_from_slice(&f.duration.to_be_bytes());
    } else {
        b.extend_from_slice(&(f.creation_time as u32).to_be_bytes());
        b.extend_from_slice(&(f.modification_time as u32).to_be_bytes());
        b.extend_from_slice(&f.timescale.to_be_bytes());
        b.extend_from_slice(&(f.duration as u32).to_be_bytes());
    }
    b.extend_from_slice(&f.language.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    fullbx(b"mdhd", u8::from(wide), 0, &b)
}

/// `hdlr` (§8.4.3). `name` is written as a NUL-terminated UTF-8 string, which
/// is what every modern writer does even though the spec calls it a Pascal
/// string; `vaco-demux-mp4`'s own reader accepts either.
#[must_use]
pub fn hdlr(handler: FourCc, name: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    b.extend_from_slice(&handler.as_bytes());
    b.extend_from_slice(&[0u8; 12]); // reserved
    b.extend_from_slice(name.as_bytes());
    b.push(0);
    fullbx(b"hdlr", 0, 0, &b)
}

/// `dinf` holding one self-contained `url ` entry — every sample this crate
/// writes lives in the same file, so there is never a second data reference.
#[must_use]
pub fn dinf_self_contained() -> Vec<u8> {
    let url = fullbx(b"url ", 0, 1, &[]); // flag 1: media data is in this file
    let mut dref = Vec::new();
    dref.extend_from_slice(&1u32.to_be_bytes());
    dref.extend_from_slice(&url);
    bx(b"dinf", &fullbx(b"dref", 0, 0, &dref))
}

/// `vmhd` (§8.4.5.2). `graphicsmode`/`opcolor` are always zero; nothing in
/// this workspace writes a video composite mode.
#[must_use]
pub fn vmhd() -> Vec<u8> {
    fullbx(b"vmhd", 0, 1, &[0u8; 8])
}

/// `smhd` (§8.4.5.3): `balance` fixed at centre, as every encoder does absent
/// explicit panning.
#[must_use]
pub fn smhd() -> Vec<u8> {
    fullbx(b"smhd", 0, 0, &[0u8; 4])
}

/// `nmhd` (§8.4.5.5), for handler types with no specific header — the
/// `QuickTime` timed-text and chapter tracks this crate writes.
#[must_use]
pub fn nmhd() -> Vec<u8> {
    fullbx(b"nmhd", 0, 0, &[])
}

// -------------------------------------------------------------- sample entries

/// The fixed fields of a `VisualSampleEntry`, plus its already-serialised
/// extension boxes (`avcC`/`hvcC`/`av1C`, `pasp`, `colr`, ...).
#[derive(Debug, Clone, Copy)]
pub struct VisualEntryFields<'a> {
    pub format: FourCc,
    pub width: u16,
    pub height: u16,
    /// 16.16 dpi; `72 << 16` is the conventional value.
    pub horiz_resolution: u32,
    pub vert_resolution: u32,
    pub depth: u16,
    pub compressor: &'a str,
    /// Concatenated child boxes, already framed.
    pub extensions: &'a [u8],
}

/// One `VisualSampleEntry` (§8.5.2.2), `data_reference_index` fixed at 1 —
/// this crate never writes a second data reference.
#[must_use]
pub fn visual_sample_entry(f: &VisualEntryFields<'_>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0u8; 6]); // reserved
    b.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    b.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    b.extend_from_slice(&0u16.to_be_bytes()); // reserved
    b.extend_from_slice(&[0u8; 12]); // pre_defined[3]
    b.extend_from_slice(&f.width.to_be_bytes());
    b.extend_from_slice(&f.height.to_be_bytes());
    b.extend_from_slice(&f.horiz_resolution.to_be_bytes());
    b.extend_from_slice(&f.vert_resolution.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes()); // reserved
    b.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    let mut name = [0u8; 32];
    let raw = f.compressor.as_bytes();
    let n = raw.len().min(31);
    name[0] = n as u8;
    if let Some(slot) = name.get_mut(1..1 + n)
        && let Some(src) = raw.get(..n)
    {
        slot.copy_from_slice(src);
    }
    b.extend_from_slice(&name);
    b.extend_from_slice(&f.depth.to_be_bytes());
    b.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined
    b.extend_from_slice(f.extensions);
    bx(&f.format.as_bytes(), &b)
}

/// The fixed fields of an `AudioSampleEntry` (version 0), plus extensions.
#[derive(Debug, Clone, Copy)]
pub struct AudioEntryFields<'a> {
    pub format: FourCc,
    pub channel_count: u16,
    pub sample_size: u16,
    /// 16.16 fixed-point sample rate. Values above 16 bits of integer part
    /// (e.g. 96 000 Hz) still fit the field itself (`96000 << 16` overflows a
    /// `u32`? no — `96000 * 65536` is `6_291_456_000`, over `u32::MAX`), so a
    /// rate that large is clamped to `u16::MAX` in the integer part by the
    /// caller before packing; see `vaco-mux-mp4`'s `entry` module for the
    /// exact policy, mirroring what `ffmpeg 8.1` itself does (an `srat`-style
    /// extension is not written since it is not part of the base spec).
    pub sample_rate_fp16: u32,
    pub extensions: &'a [u8],
}

/// One `AudioSampleEntry`, `QuickTime` version 0 (§8.5.2.2 / QTFF).
#[must_use]
pub fn audio_sample_entry(f: &AudioEntryFields<'_>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0u8; 6]); // reserved
    b.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    b.extend_from_slice(&0u32.to_be_bytes()); // version/revision (v0)
    b.extend_from_slice(&0u32.to_be_bytes()); // vendor
    b.extend_from_slice(&f.channel_count.to_be_bytes());
    b.extend_from_slice(&f.sample_size.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes()); // pre_defined (compression id)
    b.extend_from_slice(&0u16.to_be_bytes()); // reserved (packet size)
    b.extend_from_slice(&f.sample_rate_fp16.to_be_bytes());
    b.extend_from_slice(f.extensions);
    bx(&f.format.as_bytes(), &b)
}

/// `stsd`: the entry count plus already-framed entries (§8.5.2).
#[must_use]
pub fn stsd(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(
        &u32::try_from(entries.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for e in entries {
        b.extend_from_slice(e);
    }
    fullbx(b"stsd", 0, 0, &b)
}

/// `avcC`: the `AVCDecoderConfigurationRecord` (14496-15 §5.3.3.1), written
/// verbatim from `CodecParameters::extradata` — which is already in this
/// exact record form for an H.264 stream sourced from a container that had
/// one (D14.1: no NAL parsing happens in this crate or its callers).
#[must_use]
pub fn avcc(record: &[u8]) -> Vec<u8> {
    bx(b"avcC", record)
}

/// `hvcC`, mirroring [`avcc`] for HEVC (14496-15 §8.3.3.1).
#[must_use]
pub fn hvcc(record: &[u8]) -> Vec<u8> {
    bx(b"hvcC", record)
}

/// `av1C`, mirroring [`avcc`] for AV1 (the AOM ISOBMFF binding §2.3).
#[must_use]
pub fn av1c(record: &[u8]) -> Vec<u8> {
    bx(b"av1C", record)
}

/// `vpcC`, a **full** box, for VP8/VP9 (`WebM` Project's ISOBMFF binding).
#[must_use]
pub fn vpcc(record: &[u8]) -> Vec<u8> {
    fullbx(b"vpcC", 1, 0, record)
}

/// `dOps`: the Opus specific box, `record` being `OpusHead` minus its magic
/// and version byte, exactly as `vaco-demux-mp4` reads it back.
#[must_use]
pub fn dops(record: &[u8]) -> Vec<u8> {
    bx(b"dOps", record)
}

/// `dfLa`: the FLAC specific box, a full box whose payload is one or more
/// FLAC metadata blocks (§ Xiph's FLAC-in-ISOBMFF mapping) — the same bytes
/// `CodecParameters::extradata` already carries for a FLAC stream.
#[must_use]
pub fn dfla(record: &[u8]) -> Vec<u8> {
    fullbx(b"dfLa", 0, 0, record)
}

/// One MPEG-4 descriptor: tag, expandable length, payload.
fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(tag);
    out.extend_from_slice(&write_expandable(
        u32::try_from(payload.len()).unwrap_or(u32::MAX),
    ));
    out.extend_from_slice(payload);
    out
}

/// `esds` (ISO/IEC 14496-1 §7.2.6.6, wrapped per 14496-14 §5.6): an
/// `ES_Descriptor` holding one `DecoderConfigDescriptor` (which carries
/// `object_type`/`stream_type`/the bitrates) and, when `decoder_specific` is
/// non-empty, one `DecoderSpecificInfo`.
///
/// `es_id` is fixed at 1: nothing in this workspace's model gives a track
/// more than one elementary stream, so a second value would only be a
/// constant nobody reads.
#[must_use]
pub fn esds(
    object_type: u8,
    stream_type: u8,
    max_bitrate: u32,
    avg_bitrate: u32,
    decoder_specific: &[u8],
) -> Vec<u8> {
    let mut dcd = vec![object_type, (stream_type << 2) | 0x01]; // upStream=0, reserved=1
    dcd.extend_from_slice(&[0, 0, 0]); // bufferSizeDB
    dcd.extend_from_slice(&max_bitrate.to_be_bytes());
    dcd.extend_from_slice(&avg_bitrate.to_be_bytes());
    if !decoder_specific.is_empty() {
        dcd.extend_from_slice(&descriptor(TAG_DECODER_SPECIFIC, decoder_specific));
    }
    let mut es = vec![0x00, 0x01, 0x00]; // ES_ID=1, flags=0
    es.extend_from_slice(&descriptor(TAG_DECODER_CONFIG, &dcd));
    es.extend_from_slice(&descriptor(TAG_SL_CONFIG, &[0x02])); // predefined=2 (reserved for use in MP4)
    fullbx(b"esds", 0, 0, &descriptor(TAG_ES, &es))
}

/// `btrt` (§8.5.2.2's informative note; widely written anyway): decoding
/// buffer size and bitrates, outside the `esds` for codecs that have no ES
/// descriptor of their own.
#[must_use]
pub fn btrt(buffer_size_db: u32, max_bitrate: u32, avg_bitrate: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&buffer_size_db.to_be_bytes());
    b.extend_from_slice(&max_bitrate.to_be_bytes());
    b.extend_from_slice(&avg_bitrate.to_be_bytes());
    bx(b"btrt", &b)
}

/// `pasp`: pixel aspect ratio as `(h_spacing, v_spacing)` (§8.5.2.2, note 3).
#[must_use]
pub fn pasp(h_spacing: u32, v_spacing: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&h_spacing.to_be_bytes());
    b.extend_from_slice(&v_spacing.to_be_bytes());
    bx(b"pasp", &b)
}

// -------------------------------------------------------------- sample tables

/// `stts` (§8.6.1.2): `(sample_count, sample_delta)` runs.
#[must_use]
pub fn stts(runs: &[(u32, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32::try_from(runs.len()).unwrap_or(u32::MAX).to_be_bytes());
    for (c, d) in runs {
        b.extend_from_slice(&c.to_be_bytes());
        b.extend_from_slice(&d.to_be_bytes());
    }
    fullbx(b"stts", 0, 0, &b)
}

/// `ctts` (§8.6.1.3). Version 1 when any offset is negative, matching what a
/// reader needs to see to interpret the field as signed.
#[must_use]
pub fn ctts(runs: &[(u32, i32)]) -> Vec<u8> {
    let version = u8::from(runs.iter().any(|(_, o)| *o < 0));
    let mut b = Vec::new();
    b.extend_from_slice(&u32::try_from(runs.len()).unwrap_or(u32::MAX).to_be_bytes());
    for (c, o) in runs {
        b.extend_from_slice(&c.to_be_bytes());
        b.extend_from_slice(&o.to_be_bytes());
    }
    fullbx(b"ctts", version, 0, &b)
}

/// `stsc` (§8.7.4): `(first_chunk, samples_per_chunk, sample_description_index)` runs.
#[must_use]
pub fn stsc(runs: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32::try_from(runs.len()).unwrap_or(u32::MAX).to_be_bytes());
    for (first, count, idx) in runs {
        b.extend_from_slice(&first.to_be_bytes());
        b.extend_from_slice(&count.to_be_bytes());
        b.extend_from_slice(&idx.to_be_bytes());
    }
    fullbx(b"stsc", 0, 0, &b)
}

/// `stsz` (§8.7.3.2), non-uniform form (`sample_size` field zero, one entry
/// per sample). The uniform form is never emitted: a caller with a constant
/// sample size still gets a correct, if slightly larger, file, and the
/// non-uniform form has exactly one shape to get right instead of two.
#[must_use]
pub fn stsz(sizes: &[u32]) -> Vec<u8> {
    let mut b = vec![0u8, 0, 0, 0];
    b.extend_from_slice(&u32::try_from(sizes.len()).unwrap_or(u32::MAX).to_be_bytes());
    for s in sizes {
        b.extend_from_slice(&s.to_be_bytes());
    }
    fullbx(b"stsz", 0, 0, &b)
}

/// `stss` (§8.6.2): one-based sync-sample numbers.
#[must_use]
pub fn stss(syncs: &[u32]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32::try_from(syncs.len()).unwrap_or(u32::MAX).to_be_bytes());
    for s in syncs {
        b.extend_from_slice(&s.to_be_bytes());
    }
    fullbx(b"stss", 0, 0, &b)
}

/// `stco`/`co64` (§8.7.4): 32-bit unless any offset needs more.
#[must_use]
pub fn chunk_offsets(offsets: &[u64]) -> Vec<u8> {
    let wide = offsets.iter().any(|o| *o > u64::from(u32::MAX));
    let mut b = Vec::new();
    b.extend_from_slice(
        &u32::try_from(offsets.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    if wide {
        for o in offsets {
            b.extend_from_slice(&o.to_be_bytes());
        }
        fullbx(b"co64", 0, 0, &b)
    } else {
        for o in offsets {
            b.extend_from_slice(&(*o as u32).to_be_bytes());
        }
        fullbx(b"stco", 0, 0, &b)
    }
}

/// Whether [`chunk_offsets`] would choose `co64` for these offsets — so a
/// caller doing the faststart fixed-point (§ the crate's own docs) can decide
/// the table width before the final values are known.
#[must_use]
pub fn needs_co64(offsets: &[u64]) -> bool {
    offsets.iter().any(|o| *o > u64::from(u32::MAX))
}

// -------------------------------------------------------------- fragments

/// `mvex` wrapping already-framed `trex` (and optional `mehd`) children.
#[must_use]
pub fn mvex(children: &[u8]) -> Vec<u8> {
    bx(b"mvex", children)
}

/// `mehd` (§8.8.2): the fragmented movie's total duration, when known ahead
/// of time. Version 1 when it does not fit 32 bits.
#[must_use]
pub fn mehd(fragment_duration: u64) -> Vec<u8> {
    if fragment_duration > u64::from(u32::MAX) {
        fullbx(b"mehd", 1, 0, &fragment_duration.to_be_bytes())
    } else {
        fullbx(b"mehd", 0, 0, &(fragment_duration as u32).to_be_bytes())
    }
}

/// `trex` (§8.8.3): per-track fragment defaults.
#[must_use]
pub fn trex(
    track_id: u32,
    default_sample_description_index: u32,
    default_sample_duration: u32,
    default_sample_size: u32,
    default_sample_flags: u32,
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&track_id.to_be_bytes());
    b.extend_from_slice(&default_sample_description_index.to_be_bytes());
    b.extend_from_slice(&default_sample_duration.to_be_bytes());
    b.extend_from_slice(&default_sample_size.to_be_bytes());
    b.extend_from_slice(&default_sample_flags.to_be_bytes());
    fullbx(b"trex", 0, 0, &b)
}

/// `mfhd` (§8.8.5): the fragment's one-based sequence number.
#[must_use]
pub fn mfhd(sequence_number: u32) -> Vec<u8> {
    fullbx(b"mfhd", 0, 0, &sequence_number.to_be_bytes())
}

/// The fields [`tfhd`] needs; presence of each `Option` decides the flag bit
/// and whether the field is written, exactly mirroring
/// [`crate::frag::TrackFragmentHeader`] on the read side.
#[derive(Debug, Clone, Copy, Default)]
pub struct TfhdFields {
    pub track_id: u32,
    pub base_data_offset: Option<u64>,
    pub sample_description_index: Option<u32>,
    pub default_sample_duration: Option<u32>,
    pub default_sample_size: Option<u32>,
    pub default_sample_flags: Option<u32>,
    pub duration_is_empty: bool,
    pub default_base_is_moof: bool,
}

/// `tfhd` (§8.8.7).
#[must_use]
pub fn tfhd(f: &TfhdFields) -> Vec<u8> {
    use crate::frag::{
        TF_BASE_DATA_OFFSET, TF_DEFAULT_BASE_IS_MOOF, TF_DEFAULT_SAMPLE_DURATION,
        TF_DEFAULT_SAMPLE_FLAGS, TF_DEFAULT_SAMPLE_SIZE, TF_DURATION_IS_EMPTY,
        TF_SAMPLE_DESCRIPTION_INDEX,
    };
    let mut flags = 0u32;
    let mut b = Vec::new();
    b.extend_from_slice(&f.track_id.to_be_bytes());
    if let Some(v) = f.base_data_offset {
        flags |= TF_BASE_DATA_OFFSET;
        b.extend_from_slice(&v.to_be_bytes());
    }
    if let Some(v) = f.sample_description_index {
        flags |= TF_SAMPLE_DESCRIPTION_INDEX;
        b.extend_from_slice(&v.to_be_bytes());
    }
    if let Some(v) = f.default_sample_duration {
        flags |= TF_DEFAULT_SAMPLE_DURATION;
        b.extend_from_slice(&v.to_be_bytes());
    }
    if let Some(v) = f.default_sample_size {
        flags |= TF_DEFAULT_SAMPLE_SIZE;
        b.extend_from_slice(&v.to_be_bytes());
    }
    if let Some(v) = f.default_sample_flags {
        flags |= TF_DEFAULT_SAMPLE_FLAGS;
        b.extend_from_slice(&v.to_be_bytes());
    }
    if f.duration_is_empty {
        flags |= TF_DURATION_IS_EMPTY;
    }
    if f.default_base_is_moof {
        flags |= TF_DEFAULT_BASE_IS_MOOF;
    }
    fullbx(b"tfhd", 0, flags, &b)
}

/// `tfdt` (§8.8.12): the base media decode time of the fragment's first
/// sample. Version 1 when it does not fit 32 bits.
#[must_use]
pub fn tfdt(base_media_decode_time: u64) -> Vec<u8> {
    if base_media_decode_time > u64::from(u32::MAX) {
        fullbx(b"tfdt", 1, 0, &base_media_decode_time.to_be_bytes())
    } else {
        fullbx(
            b"tfdt",
            0,
            0,
            &(base_media_decode_time as u32).to_be_bytes(),
        )
    }
}

/// One `trun` sample entry. Which fields are actually written is decided by
/// `tr_flags`, passed separately to [`trun`] — a field here is ignored, not
/// erroring, when its flag bit is clear, so a caller can reuse one struct
/// across runs with different flag sets.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrunSample {
    pub duration: u32,
    pub size: u32,
    pub flags: u32,
    pub cts: i32,
}

/// `trun` (§8.8.8). `tr_flags` follows [`crate::frag`]'s `TR_*` constants
/// exactly; `data_offset`/`first_sample_flags` are written when the
/// corresponding flag is set regardless of whether the `Option` matches
/// (mismatch is a caller bug, not something to paper over silently — the
/// `Option` values are what get written, `tr_flags` is what a reader is told
/// to expect).
#[must_use]
pub fn trun(
    tr_flags: u32,
    samples: &[TrunSample],
    data_offset: i32,
    first_sample_flags: u32,
) -> Vec<u8> {
    use crate::frag::{
        TR_DATA_OFFSET, TR_FIRST_SAMPLE_FLAGS, TR_SAMPLE_CTS_OFFSET, TR_SAMPLE_DURATION,
        TR_SAMPLE_FLAGS, TR_SAMPLE_SIZE,
    };
    let mut b = Vec::new();
    b.extend_from_slice(
        &u32::try_from(samples.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    if tr_flags & TR_DATA_OFFSET != 0 {
        b.extend_from_slice(&data_offset.to_be_bytes());
    }
    if tr_flags & TR_FIRST_SAMPLE_FLAGS != 0 {
        b.extend_from_slice(&first_sample_flags.to_be_bytes());
    }
    for s in samples {
        if tr_flags & TR_SAMPLE_DURATION != 0 {
            b.extend_from_slice(&s.duration.to_be_bytes());
        }
        if tr_flags & TR_SAMPLE_SIZE != 0 {
            b.extend_from_slice(&s.size.to_be_bytes());
        }
        if tr_flags & TR_SAMPLE_FLAGS != 0 {
            b.extend_from_slice(&s.flags.to_be_bytes());
        }
        if tr_flags & TR_SAMPLE_CTS_OFFSET != 0 {
            b.extend_from_slice(&s.cts.to_be_bytes());
        }
    }
    fullbx(
        b"trun",
        u8::from(tr_flags & TR_SAMPLE_CTS_OFFSET != 0),
        tr_flags,
        &b,
    )
}

/// `traf`, wrapping already-framed `tfhd`/`tfdt`/`trun` (and, in future, CENC
/// auxiliary-information boxes) children.
#[must_use]
pub fn traf(children: &[u8]) -> Vec<u8> {
    bx(b"traf", children)
}

/// `moof`, wrapping `mfhd` and one `traf` per track fragment.
#[must_use]
pub fn moof(mfhd_bytes: &[u8], trafs: &[Vec<u8>]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(mfhd_bytes);
    for t in trafs {
        b.extend_from_slice(t);
    }
    bx(b"moof", &b)
}

// -------------------------------------------------------------- sidx / mfra

/// One `sidx` reference, mirroring [`crate::frag::SegmentReference`].
#[derive(Debug, Clone, Copy)]
pub struct SidxReference {
    pub is_index: bool,
    pub referenced_size: u32,
    pub subsegment_duration: u32,
    pub starts_with_sap: bool,
    pub sap_type: u8,
    pub sap_delta_time: u32,
}

/// `sidx` (§8.16.3). Version 1 (64-bit times) is always used: the caller
/// already tracks absolute byte offsets as `u64`, and a fixed version means
/// the reference-count/size relationship never has to be recomputed after the
/// fact the way choosing version 0-or-1 by value would require.
#[must_use]
pub fn sidx(
    reference_id: u32,
    timescale: u32,
    earliest_presentation_time: u64,
    first_offset: u64,
    references: &[SidxReference],
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&reference_id.to_be_bytes());
    b.extend_from_slice(&timescale.to_be_bytes());
    b.extend_from_slice(&earliest_presentation_time.to_be_bytes());
    b.extend_from_slice(&first_offset.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes()); // reserved
    b.extend_from_slice(
        &u16::try_from(references.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    for r in references {
        let a = (u32::from(r.is_index) << 31) | (r.referenced_size & 0x7FFF_FFFF);
        let c = (u32::from(r.starts_with_sap) << 31)
            | (u32::from(r.sap_type & 0x7) << 28)
            | (r.sap_delta_time & 0x0FFF_FFFF);
        b.extend_from_slice(&a.to_be_bytes());
        b.extend_from_slice(&r.subsegment_duration.to_be_bytes());
        b.extend_from_slice(&c.to_be_bytes());
    }
    fullbx(b"sidx", 1, 0, &b)
}

/// One `tfra` entry, mirroring [`crate::frag::RandomAccessEntry`].
#[derive(Debug, Clone, Copy)]
pub struct TfraEntry {
    pub time: u64,
    pub moof_offset: u64,
    pub traf_number: u32,
    pub trun_number: u32,
    pub sample_number: u32,
}

/// `tfra` (§8.8.10), version 1 (64-bit time/offset) with every length field
/// fixed at its maximum (4 bytes): larger than strictly necessary, and one
/// shape to get right rather than the four the length-size bits allow.
#[must_use]
pub fn tfra(track_id: u32, entries: &[TfraEntry]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&track_id.to_be_bytes());
    // length_size_of_traf_num=3, _trun_num=3, _sample_num=3 (each stores len-1).
    b.extend_from_slice(&0x0000_003Fu32.to_be_bytes());
    b.extend_from_slice(
        &u32::try_from(entries.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for e in entries {
        b.extend_from_slice(&e.time.to_be_bytes());
        b.extend_from_slice(&e.moof_offset.to_be_bytes());
        b.extend_from_slice(&e.traf_number.to_be_bytes());
        b.extend_from_slice(&e.trun_number.to_be_bytes());
        b.extend_from_slice(&e.sample_number.to_be_bytes());
    }
    fullbx(b"tfra", 1, 0, &b)
}

/// `mfro` (§8.8.11): the size of the whole enclosing `mfra`, itself included.
#[must_use]
pub fn mfro(mfra_size: u32) -> Vec<u8> {
    fullbx(b"mfro", 0, 0, &mfra_size.to_be_bytes())
}

/// `mfra`: one `tfra` per track, plus a trailing `mfro` sized to the whole box.
#[must_use]
pub fn mfra(tfras: &[Vec<u8>]) -> Vec<u8> {
    let mut body: Vec<u8> = tfras.iter().flatten().copied().collect();
    // `mfro` is 16 bytes; the box header of `mfra` itself is 8. The value is
    // computed with a placeholder-free closed form because `mfro`'s own size
    // never varies.
    let total = 8usize.saturating_add(body.len()).saturating_add(16);
    body.extend_from_slice(&mfro(u32::try_from(total).unwrap_or(u32::MAX)));
    bx(b"mfra", &body)
}

// -------------------------------------------------------------- metadata

/// `udta`, wrapping already-framed children (`meta`, `chpl`, ...).
#[must_use]
pub fn udta(children: &[u8]) -> Vec<u8> {
    bx(b"udta", children)
}

/// `meta` (§8.11.1): a full box in ISO files, wrapping a `hdlr` and (for
/// iTunes-style tags) an `ilst`. Measured against `ffmpeg 8.1`: its `mdta`/
/// `mdir` handler-based `meta` always carries the four-byte version/flags
/// header, so this never writes the QuickTime-only headerless form.
#[must_use]
pub fn meta(hdlr_bytes: &[u8], ilst_bytes: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(hdlr_bytes);
    b.extend_from_slice(ilst_bytes);
    fullbx(b"meta", 0, 0, &b)
}

/// iTunes `data` box type indicators (§ Apple's `iTunes` metadata spec).
pub mod data_type {
    pub const UTF8: u32 = 1;
    pub const JPEG: u32 = 13;
    pub const PNG: u32 = 14;
    pub const BE_SIGNED_INT: u32 = 21;
}

/// One `ilst` item: an outer box named `key` holding one `data` box.
#[must_use]
pub fn ilst_item(key: FourCc, type_indicator: u32, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&0u32.to_be_bytes()); // locale/country+language
    data.extend_from_slice(payload);
    // `data`'s flags field carries the type indicator, not a real 24-bit
    // flag set — this is `fullbx` used exactly as the format uses it.
    let inner = fullbx(b"data", 0, type_indicator, &data);
    bx(&key.as_bytes(), &inner)
}

/// A UTF-8 text tag, e.g. `©nam`/`©ART`/`©alb`.
#[must_use]
pub fn ilst_text(key: FourCc, text: &str) -> Vec<u8> {
    ilst_item(key, data_type::UTF8, text.as_bytes())
}

/// `covr`: cover art, JPEG or PNG.
#[must_use]
pub fn covr(is_png: bool, image: &[u8]) -> Vec<u8> {
    let ty = if is_png {
        data_type::PNG
    } else {
        data_type::JPEG
    };
    ilst_item(FourCc::new(b"covr"), ty, image)
}

/// `ilst`, wrapping already-framed items.
#[must_use]
pub fn ilst(items: &[u8]) -> Vec<u8> {
    bx(b"ilst", items)
}

/// `tref`, one reference type holding the given track ids (§8.3.3).
#[must_use]
pub fn tref_entry(reference_type: FourCc, track_ids: &[u32]) -> Vec<u8> {
    let mut b = Vec::new();
    for id in track_ids {
        b.extend_from_slice(&id.to_be_bytes());
    }
    bx(&reference_type.as_bytes(), &b)
}

/// `tref`, wrapping already-framed reference-type children.
#[must_use]
pub fn tref(entries: &[u8]) -> Vec<u8> {
    bx(b"tref", entries)
}

/// One Nero-style chapter: `start_time` in 100 ns units since midnight, and a
/// title. Measured against `ffmpeg 8.1`'s `mov` muxer, which writes chapters
/// this way (`udta ▸ chpl`) rather than as a `QuickTime` text track by default.
#[must_use]
pub fn chpl_entry(start_time_100ns: u64, title: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&start_time_100ns.to_be_bytes());
    let bytes = title.as_bytes();
    let n = bytes.len().min(255);
    b.push(u8::try_from(n).unwrap_or(255));
    if let Some(s) = bytes.get(..n) {
        b.extend_from_slice(s);
    }
    b
}

/// `chpl`: the Nero chapter list, version 0 — count immediately, no leading
/// reserved field. `vaco-demux-mp4`'s own reader documents version 1 as
/// inserting a four-byte reserved field before the count instead; writing
/// version 0 avoids that field entirely rather than getting its width wrong.
#[must_use]
pub fn chpl(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![u8::try_from(entries.len().min(255)).unwrap_or(255)];
    for e in entries.iter().take(255) {
        b.extend_from_slice(e);
    }
    fullbx(b"chpl", 0, 0, &b)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::boxes::{BoxIter, IsoBox};
    use crate::fourcc::boxes;
    use crate::frag::{MovieFragment, SegmentIndex};
    use crate::movie::{FileType, Track};

    fn only_box(data: &[u8]) -> IsoBox<'_> {
        BoxIter::new(data, 0).flatten().next().unwrap()
    }

    #[test]
    fn ftyp_round_trips_through_the_reader() {
        let compat = [
            FourCc::new(b"isom"),
            FourCc::new(b"iso2"),
            FourCc::new(b"mp41"),
        ];
        let bytes = file_type(b"ftyp", FourCc::new(b"mp42"), 512, &compat);
        let ft = FileType::parse(&only_box(&bytes));
        assert_eq!(ft.major_brand, FourCc::new(b"mp42"));
        assert!(ft.has_brand(FourCc::new(b"iso2")));
        assert!(!ft.has_brand(FourCc::new(b"avc1")));
    }

    #[test]
    fn mvhd_chooses_version_from_the_values_it_carries() {
        let f = MvhdFields {
            creation_time: 10,
            modification_time: 20,
            timescale: 1000,
            duration: 5_000,
            rate: 0x0001_0000,
            volume: 0x0100,
            matrix: crate::fixed::IDENTITY_MATRIX,
            next_track_id: 2,
        };
        let bytes = mvhd(&f);
        let full = only_box(&bytes).full().unwrap();
        assert_eq!(full.version, 0);
        let header = crate::movie::MovieHeader::parse(&full);
        assert_eq!(header.timescale, 1000);
        assert_eq!(header.duration, 5_000);

        let wide = MvhdFields {
            duration: u64::from(u32::MAX) + 1,
            ..f
        };
        let bytes = mvhd(&wide);
        let full = only_box(&bytes).full().unwrap();
        assert_eq!(full.version, 1);
        let header = crate::movie::MovieHeader::parse(&full);
        assert_eq!(header.duration, u64::from(u32::MAX) + 1);
    }

    #[test]
    fn a_whole_track_round_trips_with_real_sample_tables() {
        let entry = visual_sample_entry(&VisualEntryFields {
            format: FourCc::new(b"avc1"),
            width: 64,
            height: 48,
            horiz_resolution: 72 << 16,
            vert_resolution: 72 << 16,
            depth: 0x18,
            compressor: "",
            extensions: &avcc(&[0x01, 0x42, 0x00, 0x0A, 0xFF]),
        });
        let stsd_box = stsd(&[entry]);

        let mut stbl_body = Vec::new();
        stbl_body.extend_from_slice(&stsd_box);
        stbl_body.extend_from_slice(&stts(&[(2, 100)]));
        stbl_body.extend_from_slice(&ctts(&[(2, 10)]));
        stbl_body.extend_from_slice(&stss(&[1]));
        stbl_body.extend_from_slice(&stsc(&[(1, 2, 1)]));
        stbl_body.extend_from_slice(&stsz(&[64, 32]));
        stbl_body.extend_from_slice(&chunk_offsets(&[400]));
        let stbl_box = bx(b"stbl", &stbl_body);

        let mut minf = Vec::new();
        minf.extend_from_slice(&vmhd());
        minf.extend_from_slice(&dinf_self_contained());
        minf.extend_from_slice(&stbl_box);

        let mut mdia = Vec::new();
        mdia.extend_from_slice(&mdhd(&MdhdFields {
            creation_time: 0,
            modification_time: 0,
            timescale: 1000,
            duration: 200,
            language: crate::lang::PACKED_UND,
        }));
        mdia.extend_from_slice(&hdlr(boxes::VIDE, "VideoHandler"));
        mdia.extend_from_slice(&bx(b"minf", &minf));

        let mut trak = Vec::new();
        trak.extend_from_slice(&tkhd(&TkhdFields {
            flags: tkhd_flags::ENABLED | tkhd_flags::IN_MOVIE,
            creation_time: 0,
            modification_time: 0,
            track_id: 1,
            duration: 200,
            layer: 0,
            alternate_group: 0,
            volume: 0,
            matrix: crate::fixed::IDENTITY_MATRIX,
            width: 64 << 16,
            height: 48 << 16,
        }));
        trak.extend_from_slice(&bx(b"mdia", &mdia));
        let trak_box = bx(b"trak", &trak);

        let track = Track::parse(&only_box(&trak_box)).unwrap();
        assert_eq!(track.header.track_id, 1);
        assert_eq!(track.media.timescale, 1000);
        let s0 = track.sample_table.sample(0).unwrap();
        let s1 = track.sample_table.sample(1).unwrap();
        assert_eq!((s0.offset, s0.size, s0.dts), (400, 64, 0));
        assert_eq!((s1.offset, s1.size, s1.dts), (464, 32, 100));
        assert!(s0.is_sync);
        assert!(!s1.is_sync);

        let entries = crate::stsd::parse_stsd(
            &track.sample_table.sample_descriptions.unwrap(),
            boxes::VIDE,
        )
        .unwrap();
        let e0 = entries.first().unwrap();
        assert_eq!(e0.codec(), Some(vaco_codec_core::CodecId::H264));
        let cfg = e0.config().unwrap();
        assert_eq!(cfg.flavour, crate::stsd::ConfigFlavour::Avcc);
        assert_eq!(cfg.data, &[0x01, 0x42, 0x00, 0x0A, 0xFF]);
    }

    #[test]
    fn esds_round_trips_the_decoder_specific_info() {
        let aac_dsi = [0x12, 0x08];
        let bytes = esds(
            0x40,
            crate::esds::stream_type::AUDIO,
            96_000,
            69_655,
            &aac_dsi,
        );
        let full = only_box(&bytes).full().unwrap();
        let d = crate::esds::EsDescriptor::parse(&full).unwrap();
        assert_eq!(d.object_type, 0x40);
        assert_eq!(d.stream_type, crate::esds::stream_type::AUDIO);
        assert_eq!(d.max_bitrate, 96_000);
        assert_eq!(d.avg_bitrate, 69_655);
        assert_eq!(d.decoder_specific, Some(&aac_dsi[..]));
        assert_eq!(d.codec(), Some(vaco_codec_core::CodecId::Aac));
    }

    #[test]
    fn a_fragment_round_trips_through_movie_fragment_parse() {
        let mfhd_bytes = mfhd(1);
        let tfhd_bytes = tfhd(&TfhdFields {
            track_id: 1,
            default_base_is_moof: true,
            default_sample_duration: Some(512),
            default_sample_size: Some(100),
            default_sample_flags: Some(0x0100_0000),
            ..TfhdFields::default()
        });
        let tfdt_bytes = tfdt(1_000);
        let samples = [
            TrunSample {
                duration: 512,
                size: 1000,
                flags: 0x0200_0000,
                cts: 0,
            },
            TrunSample {
                duration: 512,
                size: 200,
                flags: 0x0101_0000,
                cts: 10,
            },
        ];
        let tr_flags = crate::frag::TR_SAMPLE_DURATION
            | crate::frag::TR_SAMPLE_SIZE
            | crate::frag::TR_SAMPLE_FLAGS
            | crate::frag::TR_SAMPLE_CTS_OFFSET
            | crate::frag::TR_DATA_OFFSET;
        let trun_bytes = trun(tr_flags, &samples, 64, 0);
        let mut traf_body = Vec::new();
        traf_body.extend_from_slice(&tfhd_bytes);
        traf_body.extend_from_slice(&tfdt_bytes);
        traf_body.extend_from_slice(&trun_bytes);
        let traf_bytes = traf(&traf_body);
        let moof_bytes = moof(&mfhd_bytes, &[traf_bytes]);

        let parsed = MovieFragment::parse(&only_box(&moof_bytes)).unwrap();
        assert_eq!(parsed.sequence_number, 1);
        let tf = parsed.tracks.first().unwrap();
        assert_eq!(tf.header.track_id, 1);
        assert!(tf.header.default_base_is_moof);
        assert_eq!(tf.base_media_decode_time, Some(1_000));
        let run = tf.runs.first().unwrap();
        assert_eq!(run.sample_count(), 2);
    }

    #[test]
    fn sidx_round_trips_its_references() {
        let refs = [SidxReference {
            is_index: false,
            referenced_size: 1234,
            subsegment_duration: 5000,
            starts_with_sap: true,
            sap_type: 1,
            sap_delta_time: 0,
        }];
        let bytes = sidx(1, 1000, 0, 8, &refs);
        let parsed = SegmentIndex::parse(&only_box(&bytes)).unwrap();
        assert_eq!(parsed.reference_id, 1);
        assert_eq!(parsed.timescale, 1000);
        assert_eq!(parsed.references.len(), 1);
        assert_eq!(parsed.references[0].referenced_size, 1234);
        assert!(parsed.references[0].starts_with_sap);
    }

    #[test]
    fn mfra_round_trips_its_tfra_entries() {
        let entries = [TfraEntry {
            time: 0,
            moof_offset: 100,
            traf_number: 1,
            trun_number: 1,
            sample_number: 1,
        }];
        let tfra_bytes = tfra(1, &entries);
        let mfra_bytes = mfra(&[tfra_bytes]);
        let parsed = crate::frag::parse_mfra(&only_box(&mfra_bytes));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].track_id, 1);
        assert_eq!(parsed[0].entries.len(), 1);
        assert_eq!(parsed[0].entries[0].moof_offset, 100);
    }

    #[test]
    fn ilst_and_udta_hold_the_tag_this_crate_wrote() {
        let title = ilst_text(FourCc::new(b"\xa9nam"), "hello");
        let ilst_bytes = ilst(&title);
        let hdlr_bytes = hdlr(boxes::META_HDLR, "");
        let meta_bytes = meta(&hdlr_bytes, &ilst_bytes);
        let udta_bytes = udta(&meta_bytes);

        let udta_box = only_box(&udta_bytes);
        assert_eq!(udta_box.kind(), boxes::UDTA);
        let meta_box = udta_box.children().find(boxes::META).unwrap();
        // `meta` is a full box: its children start after the four-byte
        // version/flags header, not at the raw payload start.
        let ilst_box = meta_box.children_after(4).find(boxes::ILST).unwrap();
        let nam_box = ilst_box.children().next().unwrap().unwrap();
        let data_box = nam_box.children().find(boxes::DATA).unwrap();
        let full = data_box.full().unwrap();
        assert_eq!(full.flags, data_type::UTF8);
        assert_eq!(&full.body[4..], b"hello");
    }

    #[test]
    fn tref_names_the_chapter_track() {
        let chap = tref_entry(FourCc::new(b"chap"), &[2, 3]);
        let bytes = tref(&chap);
        let tref_box = only_box(&bytes);
        let chap_box = tref_box.children().find(FourCc::new(b"chap")).unwrap();
        assert_eq!(chap_box.payload.len(), 8);
    }

    #[test]
    fn needs_co64_matches_what_chunk_offsets_actually_writes() {
        let small = [10u64, 20, 30];
        assert!(!needs_co64(&small));
        assert_eq!(only_box(&chunk_offsets(&small)).kind(), boxes::STCO);

        let big = [10u64, u64::from(u32::MAX) + 1];
        assert!(needs_co64(&big));
        assert_eq!(only_box(&chunk_offsets(&big)).kind(), boxes::CO64);
    }

    proptest::proptest! {
        /// A written `stsz`/`stco` pair reports back exactly the sizes and
        /// offsets it was given, for any sample count and any offset range —
        /// including one that forces `co64`.
        #[test]
        fn stsz_and_chunk_offsets_round_trip(
            sizes in proptest::collection::vec(0u32..100_000, 0..64),
            base_offset in 0u64..(u64::from(u32::MAX) * 2),
        ) {
            let offsets: Vec<u64> = (0..sizes.len() as u64).map(|i| base_offset + i).collect();
            let sizes_box = stsz(&sizes);
            let offsets_box = chunk_offsets(&offsets);
            let full_sizes = only_box(&sizes_box).full().unwrap();
            let full_offsets = only_box(&offsets_box).full().unwrap();
            let table = crate::stbl::SampleSizes::parse_stsz(&full_sizes);
            let chunks = if only_box(&offsets_box).kind() == boxes::CO64 {
                crate::stbl::ChunkOffsets::parse_co64(&full_offsets)
            } else {
                crate::stbl::ChunkOffsets::parse_stco(&full_offsets)
            };
            // One sample per chunk, so chunk index and sample index coincide;
            // `ChunkOffsets::offset` is one-based, as `stsc` counts chunks.
            for (i, want) in sizes.iter().enumerate() {
                proptest::prop_assert_eq!(table.size(i as u32), Some(*want));
            }
            for (i, want) in offsets.iter().enumerate() {
                proptest::prop_assert_eq!(chunks.offset(i as u32 + 1), Some(*want));
            }
        }
    }
}
