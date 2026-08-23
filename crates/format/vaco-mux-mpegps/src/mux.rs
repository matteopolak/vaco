//! The MPEG-PS muxer core, shared by all five registered profiles.
//!
//! # What is, and is not, byte-identical to the reference
//!
//! The pack-header and system-header bit layouts are verified against real
//! `ffmpeg -f mpeg`/`-f vob`/`-f vcd` output (see `pack.rs`). The
//! *multiplexing policy* — how many packets share a pack, exactly when a
//! pack is padded versus a PES packet is allowed to overrun it, the SCR
//! step between packs — is not: this muxer writes one PES packet per pack,
//! padding to the profile's fixed pack size only when the packet is
//! smaller than it, and does not split an oversized packet across pack
//! boundaries the way the reference's VOB/SVCD/DVD profiles do. A payload
//! larger than the fixed pack size simply makes that one pack larger than
//! nominal. That keeps every pack independently self-describing and
//! trivially round-trippable through `vaco-demux-mpegps`, at the cost of
//! not reproducing the reference's exact sector alignment on real DVD-sized
//! video frames. See the docs file.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use crate::pack::{
    MuxPackSyntax, MuxStreamBound, PROGRAM_END_CODE, encode_pack_header, encode_system_header,
};
use crate::pes::{MuxPesSyntax, SID_PRIVATE_1, encode_padding, encode_pes};

/// The 90 kHz SCR/PTS/DTS time base every profile writes.
pub const TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};

/// A muxer profile: what makes `mpeg`/`vcd`/`vob`/`svcd`/`dvd` different from
/// each other.
#[derive(Debug, Clone, Copy)]
pub struct MuxProfile {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub pes_syntax: MuxPesSyntax,
    pub pack_syntax: MuxPackSyntax,
    /// `Some(n)` when this profile pads every pack to a fixed size (VOB,
    /// SVCD and DVD all measured at 2048 bytes against the reference,
    /// 2026-08-23 — *not* the 2324-byte CD-ROM/XA sector size the White
    /// Book nominally specifies; the reference does not use it here).
    /// `None` for `mpeg`, which the reference leaves unpadded.
    pub fixed_pack_size: Option<usize>,
}

pub const PROFILE_MPEG: MuxProfile = MuxProfile {
    name: "mpeg",
    long_name: "MPEG-1 Systems / MPEG program stream",
    extensions: &["mpg", "mpeg"],
    pes_syntax: MuxPesSyntax::Mpeg1,
    pack_syntax: MuxPackSyntax::Mpeg1,
    fixed_pack_size: None,
};

pub const PROFILE_VCD: MuxProfile = MuxProfile {
    name: "vcd",
    long_name: "MPEG-1 Systems / MPEG program stream (VCD)",
    extensions: &["dat"],
    pes_syntax: MuxPesSyntax::Mpeg1,
    pack_syntax: MuxPackSyntax::Mpeg1,
    fixed_pack_size: Some(2048),
};

pub const PROFILE_VOB: MuxProfile = MuxProfile {
    name: "vob",
    long_name: "MPEG-2 PS (VOB)",
    extensions: &["vob"],
    pes_syntax: MuxPesSyntax::Mpeg2,
    pack_syntax: MuxPackSyntax::Mpeg2,
    fixed_pack_size: Some(2048),
};

pub const PROFILE_SVCD: MuxProfile = MuxProfile {
    name: "svcd",
    long_name: "MPEG-2 PS (SVCD)",
    extensions: &["mpg"],
    pes_syntax: MuxPesSyntax::Mpeg2,
    pack_syntax: MuxPackSyntax::Mpeg2,
    fixed_pack_size: Some(2048),
};

pub const PROFILE_DVD: MuxProfile = MuxProfile {
    name: "dvd",
    long_name: "MPEG-2 PS (DVD VOB)",
    extensions: &["vob"],
    pes_syntax: MuxPesSyntax::Mpeg2,
    pack_syntax: MuxPackSyntax::Mpeg2,
    fixed_pack_size: Some(2048),
};

/// A stream this muxer has been told about.
struct MuxStream {
    media_type: MediaType,
    stream_id: u8,
    sub_id: Option<u8>,
}

/// The muxer, generic over which [`MuxProfile`] it writes.
pub struct PsMuxer {
    // `Box<dyn MediaSink>` carries no `Debug` bound, so this type gets a
    // hand-written `Debug` impl below instead of `#[derive(Debug)]`.
    sink: Box<dyn MediaSink>,
    profile: &'static MuxProfile,
    streams: Vec<MuxStream>,
    next_video_id: u8,
    next_audio_id: u8,
    /// Next free sub-stream id within each `private_stream_1` range this
    /// crate's own tag convention can name: AC-3 (`0x80..=0x87`), DTS
    /// (`0x88..=0x8F`), LPCM (`0xA0..=0xA7`), subpicture (`0x20..=0x3F`).
    next_ac3: u8,
    next_dts: u8,
    next_lpcm: u8,
    next_subpicture: u8,
    scr: i64,
    wrote_header: bool,
    /// Nominal per-pack SCR step used when a packet carries no duration —
    /// arbitrary but monotonic, since no downstream player sees this crate's
    /// output as anything but a freshly-muxed stream.
    scr_step: i64,
}

impl std::fmt::Debug for PsMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PsMuxer")
            .field("profile", &self.profile.name)
            .field("streams", &self.streams.len())
            .field("scr", &self.scr)
            .finish_non_exhaustive()
    }
}

impl PsMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>, profile: &'static MuxProfile) -> Self {
        Self {
            sink,
            profile,
            streams: Vec::new(),
            next_video_id: 0xE0,
            next_audio_id: 0xC0,
            next_ac3: 0x80,
            next_dts: 0x88,
            next_lpcm: 0xA0,
            next_subpicture: 0x20,
            scr: 0,
            wrote_header: false,
            scr_step: 900, // 10 ms at 90 kHz
        }
    }

    /// Classify a caller-supplied codec tag as a `private_stream_1`
    /// substream kind, using this crate's own placeholder tags (see the
    /// docs file: `vaco_codec_core::CodecId` has no AC-3/DTS/LPCM/subpicture
    /// variant yet, so there is no standard way for a caller to ask for
    /// one). Returns the range base and inclusive last id.
    fn substream_range(params: &CodecParameters) -> Option<(u8, u8)> {
        let tag = params.codec_tag?;
        match &tag {
            b"AC-3" => Some((0x80, 0x87)),
            b"DTS " => Some((0x88, 0x8F)),
            b"LPCM" => Some((0xA0, 0xA7)),
            b"dvsp" => Some((0x20, 0x3F)),
            _ => None,
        }
    }

    fn next_substream_id(&mut self, base: u8, last: u8) -> Result<u8> {
        let slot = match base {
            0x80 => &mut self.next_ac3,
            0x88 => &mut self.next_dts,
            0xA0 => &mut self.next_lpcm,
            _ => &mut self.next_subpicture,
        };
        let id = *slot;
        if id > last {
            return Err(Error::Unsupported(
                "mpegps: too many private_stream_1 substreams of this kind",
            ));
        }
        *slot = id.saturating_add(1);
        Ok(id)
    }

    fn assign_stream_id(&mut self, params: &CodecParameters) -> Result<(u8, Option<u8>)> {
        if let Some((base, last)) = Self::substream_range(params) {
            let id = self.next_substream_id(base, last)?;
            return Ok((SID_PRIVATE_1, Some(id)));
        }
        match params.media_type {
            Some(MediaType::Video) => {
                let id = self.next_video_id;
                if id > 0xEF {
                    return Err(Error::Unsupported("mpegps: too many video streams"));
                }
                self.next_video_id = id.saturating_add(1);
                Ok((id, None))
            }
            Some(MediaType::Audio) => {
                let id = self.next_audio_id;
                if id > 0xDF {
                    return Err(Error::Unsupported("mpegps: too many audio streams"));
                }
                self.next_audio_id = id.saturating_add(1);
                Ok((id, None))
            }
            other => Err(Error::Unsupported(match other {
                None => "mpegps: stream has no media type",
                _ => "mpegps: unsupported media type for a program stream",
            })),
        }
    }

    fn write_pack_and_pes(&mut self, pes: &[u8]) -> Result<()> {
        let mut out = encode_pack_header(self.profile.pack_syntax, self.scr, 0);
        if !self.wrote_header {
            out.extend_from_slice(&self.system_header());
            self.wrote_header = true;
        }
        out.extend_from_slice(pes);
        if let Some(size) = self.profile.fixed_pack_size
            && out.len() < size
        {
            out.extend_from_slice(&encode_padding(size - out.len()));
        }
        self.sink.write(&out)?;
        self.scr = self.scr.wrapping_add(self.scr_step) & ((1 << 33) - 1);
        Ok(())
    }

    fn system_header(&self) -> Vec<u8> {
        let bounds: Vec<MuxStreamBound> = self
            .streams
            .iter()
            .map(|s| MuxStreamBound {
                stream_id: s.stream_id,
                buffer_scale: s.media_type == MediaType::Video,
                buffer_size_bound: if s.media_type == MediaType::Video {
                    230
                } else {
                    32
                },
            })
            .collect();
        let audio_bound = self
            .streams
            .iter()
            .filter(|s| s.media_type == MediaType::Audio)
            .count() as u8;
        let video_bound = self
            .streams
            .iter()
            .filter(|s| s.media_type == MediaType::Video)
            .count() as u8;
        encode_system_header(1_000_000, audio_bound, video_bound, &bounds)
    }
}

impl Muxer for PsMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::SHOW_IDS.union(FormatFlags::GENERIC_INDEX)
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let (stream_id, sub_id) = self.assign_stream_id(params)?;
        let index = self.streams.len() as u32;
        self.streams.push(MuxStream {
            media_type: params.media_type.unwrap_or(MediaType::Data),
            stream_id,
            sub_id,
        });
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        // The system header is deferred to the first `write_packet` call so
        // it rides along with the first real pack, matching every measured
        // reference output (system header immediately follows the first
        // pack header, never on its own). `write_header` itself writes
        // nothing: there is no standalone place for it to go.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let stream = self
            .streams
            .get(packet.stream_index as usize)
            .ok_or(Error::InvalidData("mpegps: packet names an unknown stream"))?;
        let pts = packet.pts.ticks();
        let dts = if packet.dts.ticks() == pts {
            None
        } else {
            packet.dts.ticks()
        };
        let mut payload = packet.payload().to_vec();
        if let Some(sub_id) = stream.sub_id {
            payload.insert(0, sub_id);
        }
        let pes = encode_pes(
            self.profile.pes_syntax,
            stream.stream_id,
            pts,
            dts,
            &payload,
        );
        self.write_pack_and_pes(&pes)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.sink.write(&PROGRAM_END_CODE)?;
        self.sink.flush()
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        let _ = stream_index; // every stream in this muxer shares one clock
        Some(TIME_BASE)
    }
}
