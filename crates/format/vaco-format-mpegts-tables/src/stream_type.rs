//! `stream_type` and registration-identifier resolution.
//!
//! ISO/IEC 13818-1 Table 2-34 assigns a codec to about forty of the 256
//! `stream_type` values. The rest are either reserved, or — far more
//! importantly — say only *how* the elementary stream is carried and leave
//! *what it is* to a descriptor. `0x06`, "PES packets containing private
//! data", is by a wide margin the most common value in real DVB and ATSC
//! multiplexes, and on its own it means nothing at all.
//!
//! So resolution is two-stage: the table gives a first answer, and the
//! descriptor loop overrides it. [`resolve`] is that, and it is the only entry
//! point a demuxer needs.
//!
//! # Why this crate has its own codec enum
//!
//! [`vaco_codec_core::CodecId`] has grown since this was written (it now
//! names AC-3, E-AC-3, DTS and VC-1 among others), but MPEG-TS still routinely
//! carries things with no `CodecId` at all: DVB subtitles, teletext, SCTE-35,
//! timed ID3. Collapsing them all onto "unknown" would lose the one fact the
//! PMT actually stated, so [`TsCodec`] carries the full repertoire and
//! [`TsCodec::codec_id`] maps across where a `CodecId` exists. When the enum
//! grows, only that one function changes.
//!
//! # The `CodecId` gap, closed
//!
//! Eight [`TsCodec`] variants had no [`CodecId`] counterpart, so
//! [`TsCodec::codec_id`] returned `None` for them and `vaco-probe` printed
//! `codec_name=unknown` for a stream whose PMT had said exactly what it was —
//! the largest single divergence class the differential harness found.
//!
//! `vaco-codec-core` gained all eight (`avs2`, `avs3`, `jpeg2000`,
//! `dvb_subtitle`, `dvb_teletext`, `scte_35`, `timed_id3`, `klv`), with names
//! and long names probed from `ffmpeg -codecs` 8.1 rather than recalled. Four
//! of them are `MediaType::Data`, which that table had no entry for at all
//! until then: a transport stream carries SCTE-35 splice messages, timed ID3
//! and SMPTE 336M KLV as elementary streams of their own.
//!
//! One variant remains `None` deliberately: `Unknown`, a `stream_type` this
//! build does not recognise at all, where the PMT declared nothing whatsoever
//! and the reference itself reports `codec_type=unknown`. `PrivateData` used
//! to sit alongside it — `stream_type` 0x05/0x06 with no descriptor saying
//! what it holds — but that case *is* the reference's own `bin_data`
//! pseudo-codec, measured directly: a hand-built PMT entry with a
//! zero-length descriptor loop reports `codec_name=bin_data`
//! `codec_type=data`, not `unknown`. Collapsing it onto `None` alongside
//! genuinely unrecognised types was the same shape of loss finding 4
//! described for the eight variants below — the PMT said "this is data" and
//! that fact was being discarded.
//!
//! The `(decoders: …)`/`(encoders: …)` suffix `ffmpeg -codecs` appends to
//! `dvb_subtitle`'s long name is not part of `codec_long_name` — the same
//! thing `vaco-codec-core`'s own docs note for `subrip` — so it is omitted
//! above.
//!
//! # The mux direction
//!
//! [`resolve`] and [`from_stream_type`] answer "what codec does this
//! `stream_type` mean" for a demuxer. [`for_codec`] answers the reverse
//! question a muxer asks — "what `stream_type` (and, in a private range,
//! what `registration_descriptor`) does this codec need" — and lives here
//! rather than being re-derived in `vaco-mux-mpegts`, per the one-definition-
//! per-concept rule (D19): a demuxer and a muxer disagreeing about what
//! `0x81` means is exactly the kind of drift a shared table is for.

use vaco_codec_core::CodecId;
use vaco_core::MediaType;

use crate::descriptor::{
    DescriptorIter, TAG_ATSC_AC3, TAG_ATSC_EAC3, TAG_DVB_AAC, TAG_DVB_AC3, TAG_DVB_DTS,
    TAG_DVB_EAC3, TAG_SUBTITLING, TAG_TELETEXT, TAG_VBI_TELETEXT,
};

/// What a transport stream can carry, named as `vaco-probe` prints it.
///
/// The names are interface facts (D9) and are reproduced; the enum shape is
/// ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TsCodec {
    Unknown,
    // --- video
    Mpeg1Video,
    Mpeg2Video,
    Mpeg4Video,
    H264,
    Hevc,
    Vvc,
    Av1,
    Vc1,
    Dirac,
    Cavs,
    Avs2,
    Avs3,
    Jpeg2000,
    // --- audio
    Mp1,
    Mp2,
    Mp3,
    Aac,
    AacLatm,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    Opus,
    /// SMPTE 302M PCM, carried under registration `BSSD`.
    Pcm302m,
    /// Blu-ray LPCM.
    PcmBluray,
    // --- subtitle
    DvbSubtitle,
    DvbTeletext,
    /// Blu-ray presentation graphics.
    PgsSubtitle,
    // --- data
    /// SCTE-35 splice information, exposed but not interpreted.
    Scte35,
    /// Timed ID3 metadata, as HLS uses.
    TimedId3,
    /// SMPTE 336M KLV metadata.
    Klv,
    /// A stream the PMT declares and we can only report as private data.
    PrivateData,
}

impl TsCodec {
    /// The short name `vaco-probe` prints as `codec_name`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Mpeg1Video => "mpeg1video",
            Self::Mpeg2Video => "mpeg2video",
            Self::Mpeg4Video => "mpeg4",
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Vvc => "vvc",
            Self::Av1 => "av1",
            Self::Vc1 => "vc1",
            Self::Dirac => "dirac",
            Self::Cavs => "cavs",
            Self::Avs2 => "avs2",
            Self::Avs3 => "avs3",
            Self::Jpeg2000 => "jpeg2000",
            Self::Mp1 => "mp1",
            Self::Mp2 => "mp2",
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::AacLatm => "aac_latm",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Dts => "dts",
            Self::TrueHd => "truehd",
            Self::Opus => "opus",
            Self::Pcm302m => "pcm_s16be",
            Self::PcmBluray => "pcm_bluray",
            Self::DvbSubtitle => "dvb_subtitle",
            Self::DvbTeletext => "dvb_teletext",
            Self::PgsSubtitle => "hdmv_pgs_subtitle",
            Self::Scte35 => "scte_35",
            Self::TimedId3 => "timed_id3",
            Self::Klv => "klv",
            Self::PrivateData => "bin_data",
        }
    }

    /// Which stream list this codec belongs in.
    #[must_use]
    pub const fn media_type(self) -> MediaType {
        match self {
            Self::Mpeg1Video
            | Self::Mpeg2Video
            | Self::Mpeg4Video
            | Self::H264
            | Self::Hevc
            | Self::Vvc
            | Self::Av1
            | Self::Vc1
            | Self::Dirac
            | Self::Cavs
            | Self::Avs2
            | Self::Avs3
            | Self::Jpeg2000 => MediaType::Video,
            Self::Mp1
            | Self::Mp2
            | Self::Mp3
            | Self::Aac
            | Self::AacLatm
            | Self::Ac3
            | Self::Eac3
            | Self::Dts
            | Self::TrueHd
            | Self::Opus
            | Self::Pcm302m
            | Self::PcmBluray => MediaType::Audio,
            Self::DvbSubtitle | Self::DvbTeletext | Self::PgsSubtitle => MediaType::Subtitle,
            Self::Unknown | Self::Scte35 | Self::TimedId3 | Self::Klv | Self::PrivateData => {
                MediaType::Data
            }
        }
    }

    /// The workspace [`CodecId`], where one exists yet.
    ///
    /// `None` is not "unknown codec": it is "this codec is real, the PMT named
    /// it, and `vaco-codec-core` has no variant for it". A demuxer must keep
    /// reporting the stream — with its media type, its PID and its language —
    /// rather than dropping it.
    ///
    /// Finding 4 of `planning/CONFORMANCE-FINDINGS.md`: this used to map
    /// eight of the roughly thirty variants above and fall through to `None`
    /// — "unknown codec" — for the rest, even where `vaco_codec_core::CodecId`
    /// already had a matching variant sitting unused (`Mpeg2Video`, `Mp2`,
    /// `Ac3`… were real gaps in this function, not in that enum). Every arm
    /// below that resolves to `Some` was checked against an existing
    /// [`CodecId`] variant, not added speculatively.
    ///
    /// Eight variants still have no home: [`Self::Avs2`], [`Self::Avs3`],
    /// [`Self::Jpeg2000`], [`Self::DvbSubtitle`], [`Self::DvbTeletext`],
    /// [`Self::Scte35`], [`Self::TimedId3`], [`Self::Klv`] all fall to `None`
    /// because `vaco-codec-core` — owned by another agent — has no matching
    /// variant. Their exact names and long names, probed from `ffmpeg
    /// -codecs` (8.1) rather than recalled, are reported in this crate's
    /// docs for whoever adds them. [`Self::Unknown`] stays correctly `None`:
    /// it names no codec at all, real or generic.
    #[must_use]
    pub const fn codec_id(self) -> Option<CodecId> {
        match self {
            Self::Mpeg1Video => Some(CodecId::Mpeg1video),
            Self::Mpeg2Video => Some(CodecId::Mpeg2video),
            Self::Mpeg4Video => Some(CodecId::Mpeg4),
            Self::H264 => Some(CodecId::H264),
            Self::Hevc => Some(CodecId::Hevc),
            Self::Vvc => Some(CodecId::Vvc),
            Self::Av1 => Some(CodecId::Av1),
            Self::Vc1 => Some(CodecId::Vc1),
            Self::Dirac => Some(CodecId::Dirac),
            Self::Cavs => Some(CodecId::Cavs),
            Self::Mp1 => Some(CodecId::Mp1),
            Self::Mp2 => Some(CodecId::Mp2),
            Self::Mp3 => Some(CodecId::Mp3),
            Self::Aac => Some(CodecId::Aac),
            Self::AacLatm => Some(CodecId::AacLatm),
            Self::Ac3 => Some(CodecId::Ac3),
            Self::Eac3 => Some(CodecId::Eac3),
            Self::Dts => Some(CodecId::Dts),
            Self::TrueHd => Some(CodecId::Truehd),
            Self::Opus => Some(CodecId::Opus),
            Self::Pcm302m | Self::PcmBluray => Some(CodecId::Pcm),
            Self::PgsSubtitle => Some(CodecId::HdmvPgsSubtitle),
            Self::Avs2 => Some(CodecId::Avs2),
            Self::Avs3 => Some(CodecId::Avs3),
            Self::Jpeg2000 => Some(CodecId::Jpeg2000),
            Self::DvbSubtitle => Some(CodecId::DvbSubtitle),
            Self::DvbTeletext => Some(CodecId::DvbTeletext),
            Self::Scte35 => Some(CodecId::Scte35),
            Self::TimedId3 => Some(CodecId::TimedId3),
            Self::Klv => Some(CodecId::Klv),
            // `stream_type` 0x05/0x06 with no descriptor that says what it
            // is — the PMT genuinely declared nothing more than "data" — is
            // exactly what the reference's own `bin_data` pseudo-codec names.
            // Measured: `ffprobe` on a hand-built PMT entry, stream_type
            // 0x06, zero-length descriptor loop, reports `codec_name=bin_data`
            // `codec_type=data`, not `unknown`.
            Self::PrivateData => Some(CodecId::BinData),
            // `Unknown` is a `stream_type` this build does not recognise at
            // all — the PMT declared nothing whatsoever, not even "data" —
            // and stays `None` on purpose: measured, the reference reports
            // `codec_type=unknown` for one of these, not `data`.
            Self::Unknown => None,
        }
    }

    /// Whether a PES packet on this stream begins with a start code that a
    /// keyframe test can look at.
    ///
    /// Used to decide whether the adaptation field's `random_access_indicator`
    /// is the only key-frame evidence available.
    #[must_use]
    pub const fn is_video(self) -> bool {
        matches!(self.media_type(), MediaType::Video)
    }
}

/// A resolved elementary stream declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub codec: TsCodec,
    /// What `vaco-probe` prints as `codec_tag`.
    ///
    /// **Measured, not assumed**: for a stream carrying a registration
    /// descriptor the reference reports the four-character identifier
    /// (`ffprobe` shows `0x332d4341` for AC-3, which is `"AC-3"`); otherwise
    /// it reports the `stream_type` in the low byte (`0x001b` for H.264).
    pub codec_tag: [u8; 4],
    /// The raw `stream_type`, kept because two different codecs can share one
    /// and a diagnostic wants to say which value it saw.
    pub stream_type: u8,
}

/// The Table 2-34 answer, before descriptors are consulted.
#[must_use]
pub const fn from_stream_type(stream_type: u8) -> TsCodec {
    match stream_type {
        0x01 => TsCodec::Mpeg1Video,
        // Both MPEG-1 and MPEG-2 video are muxed as 0x02 in practice; the
        // elementary stream distinguishes them, so a parser refines this.
        0x02 | 0x80 => TsCodec::Mpeg2Video,
        // Layer is a property of the frames, not of the PMT. `mp2` is the
        // starting point because it is overwhelmingly what layer-II-capable
        // broadcast multiplexes carry; a parser corrects it to mp1 or mp3.
        0x03 | 0x04 => TsCodec::Mp2,
        0x0F | 0x1C => TsCodec::Aac,
        0x10 => TsCodec::Mpeg4Video,
        0x11 => TsCodec::AacLatm,
        0x15 => TsCodec::TimedId3,
        0x1B => TsCodec::H264,
        0x21 => TsCodec::Jpeg2000,
        0x24 | 0x25 => TsCodec::Hevc,
        0x33 => TsCodec::Vvc,
        0x42 => TsCodec::Cavs,
        0x81 => TsCodec::Ac3,
        0x83 => TsCodec::TrueHd,
        0x84 | 0x87 | 0xA1 => TsCodec::Eac3,
        0x82 | 0x85 | 0x86 | 0x8A | 0xA2 => TsCodec::Dts,
        0x90 => TsCodec::PgsSubtitle,
        0xD1 => TsCodec::Dirac,
        0xD2 => TsCodec::Avs2,
        0xD3 => TsCodec::Avs3,
        0xEA => TsCodec::Vc1,
        // 0x05 private sections and 0x06 private PES data say nothing at all;
        // a descriptor has to.
        _ => TsCodec::Unknown,
    }
}

/// The codec a four-character registration identifier declares.
#[must_use]
pub fn from_registration(id: [u8; 4]) -> Option<TsCodec> {
    Some(match &id {
        b"AC-3" | b"ac-3" => TsCodec::Ac3,
        b"EAC3" | b"eac3" | b"EC-3" => TsCodec::Eac3,
        b"AV01" => TsCodec::Av1,
        b"HEVC" => TsCodec::Hevc,
        b"VC-1" => TsCodec::Vc1,
        b"Opus" => TsCodec::Opus,
        b"drac" => TsCodec::Dirac,
        b"DTS1" | b"DTS2" | b"DTS3" | b"dtsh" => TsCodec::Dts,
        b"BSSD" => TsCodec::Pcm302m,
        b"KLVA" => TsCodec::Klv,
        b"ID3 " => TsCodec::TimedId3,
        b"CUEI" => TsCodec::Scte35,
        b"AVSV" | b"CAVS" => TsCodec::Cavs,
        b"AVS2" => TsCodec::Avs2,
        b"AVS3" => TsCodec::Avs3,
        b"VVC1" | b"vvc1" => TsCodec::Vvc,
        b"mp4a" => TsCodec::Aac,
        // `HDMV` and `GA94` say which *private range* applies, not which
        // codec; `from_stream_type` already reads the private range the way
        // both of them define it.
        _ => return None,
    })
}

/// Resolve one PMT entry to a codec.
///
/// Order, and it matters: the `stream_type` table first, then the registration
/// descriptor, then the DVB codec descriptors, then the subtitle descriptors.
/// Each later stage only fires where it has something to say, so a stream whose
/// `stream_type` is unambiguous is never second-guessed by a stray descriptor —
/// except for the two cases where a descriptor is *definitionally* stronger:
/// `teletext` and `subtitling` on a private-data stream.
#[must_use]
pub fn resolve(stream_type: u8, descriptors: &[u8]) -> Resolved {
    let mut codec = from_stream_type(stream_type);
    let iter = DescriptorIter::new(descriptors);
    let registration = iter.registration();

    if let Some(id) = registration
        && let Some(from_reg) = from_registration(id)
        && (codec == TsCodec::Unknown || is_private_range(stream_type))
    {
        codec = from_reg;
    }

    if codec == TsCodec::Unknown || stream_type == 0x06 || stream_type == 0x05 {
        // EN 300 468 §6.2: a private-data stream is identified by which DVB
        // descriptor accompanies it.
        for d in DescriptorIter::new(descriptors) {
            codec = match d.tag {
                TAG_DVB_AC3 | TAG_ATSC_AC3 => TsCodec::Ac3,
                TAG_DVB_EAC3 | TAG_ATSC_EAC3 => TsCodec::Eac3,
                TAG_DVB_DTS => TsCodec::Dts,
                TAG_DVB_AAC => TsCodec::Aac,
                TAG_TELETEXT | TAG_VBI_TELETEXT => TsCodec::DvbTeletext,
                TAG_SUBTITLING => TsCodec::DvbSubtitle,
                _ => continue,
            };
            break;
        }
    }

    if codec == TsCodec::Unknown && (stream_type == 0x05 || stream_type == 0x06) {
        codec = TsCodec::PrivateData;
    }

    let codec_tag = match registration {
        Some(id) => id,
        None => [stream_type, 0, 0, 0],
    };

    Resolved {
        codec,
        codec_tag,
        stream_type,
    }
}

/// Whether `stream_type` sits in a range whose meaning depends on which
/// organisation's private assignment applies.
const fn is_private_range(stream_type: u8) -> bool {
    stream_type >= 0x80
}

/// What a muxer must write into a PMT for one codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxStreamType {
    pub stream_type: u8,
    /// A `registration_descriptor` the elementary stream's `ES_info` loop
    /// must also carry.
    ///
    /// Only set where `stream_type` alone is ambiguous — `0x06`, "private
    /// data" — because that is the only case [`resolve`] needs the
    /// descriptor to disambiguate on the read side. A codec that already has
    /// an unambiguous Table 2-34 assignment gets `None`: adding a
    /// registration descriptor nobody needs is not wrong, but it is not
    /// measured against anything either, so this only writes one where the
    /// round trip actually depends on it.
    pub registration: Option<[u8; 4]>,
}

/// The `stream_type` (and, in a private range, the `registration_descriptor`)
/// a muxer should write for `codec`.
///
/// `dvb` selects between the two real-world conventions for AC-3, E-AC-3 and
/// DTS: ATSC (`-mpegts_flags` without `system_b`) assigns them their own
/// `stream_type` values directly; DVB (`system_b`) instead uses the private
/// `0x06` plus a registration descriptor, which is what
/// [`resolve`]'s `is_private_range`/registration handling already expects on
/// the read side. Every value returned here round-trips through [`resolve`]
/// back to the same [`TsCodec`], with one documented exception: `stream_type`
/// `0x03` means "MPEG-1 Audio, layer not yet known" on the wire — 13818-1
/// gives layers I/II/III no separate values — so [`CodecId::Mp1`] and
/// [`CodecId::Mp3`] both write `0x03` and both read back as [`TsCodec::Mp2`]
/// until something parses the actual audio frames, exactly the caveat
/// [`from_stream_type`]'s own docs already carry. Asserted, exception
/// included, in this module's tests.
///
/// `None` for a codec this table has no TS mapping for at all (`SubRip`,
/// `MovText`, image codecs, and the like): [`vaco_codec_core::CodecId`] names
/// many things MPEG-TS has no assignment for, and a muxer must refuse those
/// rather than invent a `stream_type`.
#[must_use]
pub fn for_codec(codec: CodecId, dvb: bool) -> Option<MuxStreamType> {
    let plain = |stream_type: u8| {
        Some(MuxStreamType {
            stream_type,
            registration: None,
        })
    };
    let private = |id: [u8; 4]| {
        Some(MuxStreamType {
            stream_type: 0x06,
            registration: Some(id),
        })
    };
    match codec {
        CodecId::Mpeg1video => plain(0x01),
        CodecId::Mpeg2video => plain(0x02),
        CodecId::Mpeg4 => plain(0x10),
        CodecId::H264 => plain(0x1B),
        CodecId::Hevc => plain(0x24),
        CodecId::Vvc => plain(0x33),
        CodecId::Cavs => plain(0x42),
        CodecId::Dirac => plain(0xD1),
        CodecId::Vc1 => private(*b"VC-1"),
        // No Table 2-34 assignment exists; every real-world muxer and
        // demuxer identifies AV1 in a transport stream by this registration
        // on the private stream_type, per the AOM's own TS carriage note.
        CodecId::Av1 => private(*b"AV01"),
        CodecId::Aac => plain(0x0F),
        CodecId::AacLatm => plain(0x11),
        CodecId::Mp1 | CodecId::Mp2 | CodecId::Mp3 => plain(0x03),
        CodecId::Ac3 => {
            if dvb {
                private(*b"AC-3")
            } else {
                plain(0x81)
            }
        }
        CodecId::Eac3 => {
            if dvb {
                private(*b"EAC3")
            } else {
                plain(0x87)
            }
        }
        CodecId::Dts => {
            if dvb {
                private(*b"DTS1")
            } else {
                plain(0x8A)
            }
        }
        CodecId::Truehd => plain(0x83),
        CodecId::HdmvPgsSubtitle => plain(0x90),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn the_table_covers_the_common_broadcast_types() {
        assert_eq!(from_stream_type(0x02), TsCodec::Mpeg2Video);
        assert_eq!(from_stream_type(0x03), TsCodec::Mp2);
        assert_eq!(from_stream_type(0x0F), TsCodec::Aac);
        assert_eq!(from_stream_type(0x1B), TsCodec::H264);
        assert_eq!(from_stream_type(0x24), TsCodec::Hevc);
        assert_eq!(from_stream_type(0x81), TsCodec::Ac3);
        assert_eq!(from_stream_type(0x06), TsCodec::Unknown);
    }

    #[test]
    fn a_private_stream_becomes_ac3_through_its_registration() {
        // stream_type 0x06 plus registration "AC-3".
        let desc = [0x05u8, 4, b'A', b'C', b'-', b'3'];
        let r = resolve(0x06, &desc);
        assert_eq!(r.codec, TsCodec::Ac3);
        // Measured against ffprobe 8.1: the reported codec_tag is the
        // registration identifier, not the stream type.
        assert_eq!(r.codec_tag, *b"AC-3");
    }

    #[test]
    fn h264_reports_its_stream_type_as_the_tag() {
        let r = resolve(0x1B, &[]);
        assert_eq!(r.codec, TsCodec::H264);
        assert_eq!(r.codec_tag, [0x1B, 0, 0, 0]);
    }

    #[test]
    fn a_registration_never_overrides_an_unambiguous_stream_type() {
        // A stray `Opus` registration on an H.264 stream must not win.
        let desc = [0x05u8, 4, b'O', b'p', b'u', b's'];
        assert_eq!(resolve(0x1B, &desc).codec, TsCodec::H264);
    }

    #[test]
    fn the_private_range_defers_to_the_registration() {
        // 0x87 is E-AC-3 in the ATSC range, but a `HDMV` multiplex using the
        // same value with a DTS registration must resolve to DTS.
        let desc = [0x05u8, 4, b'D', b'T', b'S', b'1'];
        assert_eq!(resolve(0x87, &desc).codec, TsCodec::Dts);
        assert_eq!(resolve(0x87, &[]).codec, TsCodec::Eac3);
    }

    #[test]
    fn dvb_descriptors_identify_private_streams() {
        assert_eq!(resolve(0x06, &[0x6A, 0]).codec, TsCodec::Ac3);
        assert_eq!(resolve(0x06, &[0x7A, 0]).codec, TsCodec::Eac3);
        assert_eq!(resolve(0x06, &[0x7B, 0]).codec, TsCodec::Dts);
        assert_eq!(
            resolve(0x06, &[0x56, 5, b'e', b'n', b'g', 0x11, 0x00]).codec,
            TsCodec::DvbTeletext
        );
        assert_eq!(
            resolve(0x06, &[0x59, 8, b'e', b'n', b'g', 0x20, 0, 1, 0, 2]).codec,
            TsCodec::DvbSubtitle
        );
    }

    /// Measured against real `ffprobe`: a hand-built PMT entry for
    /// `stream_type` 0x06 with a zero-length descriptor loop reports
    /// `codec_name=bin_data`, `codec_type=data` — not `unknown`, the two
    /// values [`TsCodec::codec_id`] returned for this case before the fix.
    #[test]
    fn an_unidentifiable_private_stream_is_still_a_stream() {
        let r = resolve(0x06, &[]);
        assert_eq!(r.codec, TsCodec::PrivateData);
        assert_eq!(r.codec.media_type(), MediaType::Data);
        assert_eq!(r.codec.codec_id(), Some(CodecId::BinData));
    }

    #[test]
    fn a_reserved_stream_type_resolves_to_unknown_not_to_a_guess() {
        assert_eq!(resolve(0x00, &[]).codec, TsCodec::Unknown);
        assert_eq!(resolve(0x77, &[]).codec, TsCodec::Unknown);
    }

    #[test]
    fn media_types_are_assigned_to_every_variant() {
        // Every codec must land in a stream list, or a demuxer cannot report
        // it at all.
        for c in [
            TsCodec::Unknown,
            TsCodec::Mpeg2Video,
            TsCodec::Ac3,
            TsCodec::DvbSubtitle,
            TsCodec::Scte35,
            TsCodec::Klv,
        ] {
            let _ = c.media_type();
            assert!(!c.name().is_empty());
        }
    }

    #[test]
    fn codec_ids_agree_with_media_types_where_they_exist() {
        // Finding 4: every variant this table can map onto an existing
        // `CodecId`, not just the eight it used to. Each one is asserted by
        // name — the positive mapping, not the eight variants' absence,
        // per `planning/AGENT-CONSTRAINTS.md` "Never pin the absence of
        // something the project is building": `Avs2`/`Avs3`/`Jpeg2000`/
        // `DvbSubtitle`/`DvbTeletext`/`Scte35`/`TimedId3`/`Klv` are
        // deliberately not asserted `None` here, so this test does not fail
        // on the day `vaco-codec-core` gains those variants.
        for c in [
            TsCodec::Mpeg1Video,
            TsCodec::Mpeg2Video,
            TsCodec::Mpeg4Video,
            TsCodec::H264,
            TsCodec::Hevc,
            TsCodec::Vvc,
            TsCodec::Av1,
            TsCodec::Vc1,
            TsCodec::Dirac,
            TsCodec::Cavs,
            TsCodec::Mp1,
            TsCodec::Mp2,
            TsCodec::Mp3,
            TsCodec::Aac,
            TsCodec::AacLatm,
            TsCodec::Ac3,
            TsCodec::Eac3,
            TsCodec::Dts,
            TsCodec::TrueHd,
            TsCodec::Opus,
            TsCodec::PgsSubtitle,
        ] {
            let id = c.codec_id().unwrap();
            assert_eq!(id.media_type(), c.media_type(), "{}", c.name());
            assert_eq!(id.name(), c.name(), "{}", c.name());
        }
    }

    #[test]
    fn pcm_variants_map_to_the_generic_pcm_codec_id() {
        for c in [TsCodec::Pcm302m, TsCodec::PcmBluray] {
            assert_eq!(c.codec_id(), Some(CodecId::Pcm), "{}", c.name());
        }
    }

    #[test]
    fn every_stream_type_resolves_without_panicking() {
        for t in 0..=255u8 {
            let r = resolve(t, &[0x05, 4, b'X', b'Y', b'Z', b'W']);
            let _ = r.codec.media_type();
            let _ = r.codec.codec_id();
        }
    }

    /// Muxer-direction assignments round-trip through `resolve`, with the
    /// one documented MPEG audio layer exception `for_codec`'s own docs
    /// carry.
    #[test]
    fn for_codec_round_trips_through_resolve() {
        let cases = [
            (CodecId::H264, TsCodec::H264),
            (CodecId::Hevc, TsCodec::Hevc),
            (CodecId::Vvc, TsCodec::Vvc),
            (CodecId::Mpeg1video, TsCodec::Mpeg1Video),
            (CodecId::Mpeg2video, TsCodec::Mpeg2Video),
            (CodecId::Mpeg4, TsCodec::Mpeg4Video),
            (CodecId::Cavs, TsCodec::Cavs),
            (CodecId::Dirac, TsCodec::Dirac),
            (CodecId::Aac, TsCodec::Aac),
            (CodecId::AacLatm, TsCodec::AacLatm),
            (CodecId::Mp2, TsCodec::Mp2),
            (CodecId::Truehd, TsCodec::TrueHd),
            (CodecId::HdmvPgsSubtitle, TsCodec::PgsSubtitle),
        ];
        for (codec, want) in cases {
            for dvb in [false, true] {
                let assign = for_codec(codec, dvb).unwrap();
                let r = resolve(
                    assign.stream_type,
                    &assign
                        .registration
                        .map_or_else(Vec::new, crate::write::registration_descriptor),
                );
                assert_eq!(r.codec, want, "{codec:?} dvb={dvb}");
            }
        }
        // The documented exception: layer is not recoverable from the PMT
        // alone, so both Mp1 and Mp3 land on Mp2 until frame parsing corrects
        // it — matching `from_stream_type`'s own behaviour.
        for codec in [CodecId::Mp1, CodecId::Mp3] {
            let assign = for_codec(codec, false).unwrap();
            assert_eq!(assign.stream_type, 0x03);
            assert_eq!(resolve(assign.stream_type, &[]).codec, TsCodec::Mp2);
        }
    }

    /// The DVB and ATSC conventions for AC-3/E-AC-3/DTS both resolve to the
    /// right codec, via two different mechanisms (a private `stream_type` with
    /// a registration descriptor, versus a dedicated `stream_type`).
    #[test]
    fn ac3_family_resolves_under_both_atsc_and_dvb_conventions() {
        for (codec, want) in [
            (CodecId::Ac3, TsCodec::Ac3),
            (CodecId::Eac3, TsCodec::Eac3),
            (CodecId::Dts, TsCodec::Dts),
        ] {
            let atsc = for_codec(codec, false).unwrap();
            assert_eq!(atsc.registration, None);
            assert_eq!(resolve(atsc.stream_type, &[]).codec, want);

            let dvb = for_codec(codec, true).unwrap();
            assert_eq!(dvb.stream_type, 0x06);
            let reg = dvb.registration.unwrap();
            let desc = crate::write::registration_descriptor(reg);
            assert_eq!(resolve(0x06, &desc).codec, want);
        }
    }

    #[test]
    fn for_codec_is_none_for_a_codec_ts_has_no_assignment_for() {
        assert_eq!(for_codec(CodecId::SubRip, false), None);
        assert_eq!(for_codec(CodecId::Flac, false), None);
    }
}
