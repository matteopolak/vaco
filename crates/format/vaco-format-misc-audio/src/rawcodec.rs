//! Headerless ITU-T/3GPP2/Bluetooth speech and audio codecs, stored as a raw
//! byte stream with no container framing at all.
//!
//! Every one of these codecs has a fixed, spec-mandated ratio between bytes
//! stored and sample frames decoded, so "demuxing" is exactly
//! [`crate::block::BlockDemuxer`] parametrised by that ratio — no header to
//! read, no magic to probe. `ffprobe` needs `-f <name>` to open any of them
//! for the same reason: there is nothing in the bytes themselves to detect.
//!
//! | Format | `sample_rate` | `channels` | bytes : frames | Source |
//! |---|---|---|---|---|
//! | `gsm` | 8000 | 1 | 33 : 160 | ETSI/3GPP GSM 06.10 full-rate frame |
//! | `sln` | 8000 | 1 | 2 : 1 | Asterisk signed-linear, i.e. `pcm_s16le` |
//! | `dfpwm` | 48000 | 1 | 1 : 8 | measured: `ffprobe` on an `ffmpeg -f dfpwm` file reports exactly this ratio |
//! | `g722` | 16000 | 1 | 1 : 2 | measured: `ffprobe` on an `ffmpeg -c:a adpcm_g722 -f g722` file reports `sample_rate=16000 bit_rate=64000`, i.e. one byte per two input samples |
//! | `g726`, `g726le` | 8000 | 1 | 1 : 2 | ITU-T G.726 at its default 32 kbit/s (4 bits/sample); big- vs little-endian is a bit-packing convention the container framing does not need |
//! | `g728` | 8000 | 1 | 1 : 4 | ITU-T G.728, 16 kbit/s (2 bits/sample) |
//! | `g729` | 8000 | 1 | 10 : 80 | ITU-T G.729, 8 kbit/s, 10 ms frames |
//! | `aptx` | 48000 | 2 | 4 : 4 | measured: `ffmpeg -c:a aptx -f aptx` writes exactly a multiple of 4 bytes, and aptX is a published constant 4:1 ratio codec |
//! | `aptx_hd` | 48000 | 2 | 6 : 4 | measured: `ffmpeg -c:a aptx_hd -f aptx_hd` writes exactly a multiple of 6 bytes |
//!
//! "frames" always means one sample instant across every channel, matching
//! [`vaco_format_audio_simple::pcm::PcmLayout`]'s convention in spirit (this
//! crate does not depend on that one; the convention is restated here).
//!
//! None of these formats has a detectable magic, so every [`probe`] here
//! returns [`ProbeScore::EXTENSION`] on a matching filename extension and
//! [`ProbeScore::NONE`] otherwise — the table's own documented meaning for
//! "filename extension matched, content did not confirm".

use vaco_codec_core::CodecId;
use vaco_core::{Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::Packet;

use crate::block::BlockDemuxer;

/// What one headerless raw codec needs to build a [`BlockDemuxer`].
#[derive(Debug, Clone, Copy)]
pub struct RawCodecSpec {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub sample_rate: u32,
    pub channels: u16,
    pub bytes_per_block: u32,
    pub frames_per_block: u32,
    pub codec_id: Option<CodecId>,
    /// The reference's own packet size, measured directly against
    /// `ffprobe -show_packets` over a large hand-built fixture — see
    /// `block.rs`'s module doc for the full measurement and why it is not
    /// one formula shared across every entry here.
    pub target_packet_bytes: u32,
}

pub const GSM: RawCodecSpec = RawCodecSpec {
    name: "gsm",
    long_name: "raw GSM",
    extensions: &["gsm"],
    sample_rate: 8000,
    channels: 1,
    bytes_per_block: 33,
    frames_per_block: 160,
    codec_id: Some(CodecId::Gsm),
    target_packet_bytes: 33,
};

pub const SLN: RawCodecSpec = RawCodecSpec {
    name: "sln",
    long_name: "Asterisk raw pcm",
    extensions: &["sln"],
    sample_rate: 8000,
    channels: 1,
    bytes_per_block: 2,
    frames_per_block: 1,
    codec_id: Some(CodecId::PcmS16le),
    target_packet_bytes: 1024,
};

pub const DFPWM: RawCodecSpec = RawCodecSpec {
    name: "dfpwm",
    long_name: "raw DFPWM1a",
    extensions: &["dfpwm"],
    sample_rate: 48000,
    channels: 1,
    bytes_per_block: 1,
    frames_per_block: 8,
    codec_id: Some(CodecId::Dfpwm),
    target_packet_bytes: 512,
};

pub const G722: RawCodecSpec = RawCodecSpec {
    name: "g722",
    long_name: "raw G.722",
    extensions: &["g722"],
    sample_rate: 16000,
    channels: 1,
    bytes_per_block: 1,
    frames_per_block: 2,
    codec_id: Some(CodecId::AdpcmG722),
    target_packet_bytes: 1024,
};

pub const G726: RawCodecSpec = RawCodecSpec {
    name: "g726",
    long_name: "raw big-endian G.726 (\"left-justified\")",
    extensions: &["g726"],
    sample_rate: 8000,
    channels: 1,
    bytes_per_block: 1,
    frames_per_block: 2,
    codec_id: Some(CodecId::AdpcmG726),
    target_packet_bytes: 1020,
};

pub const G726LE: RawCodecSpec = RawCodecSpec {
    name: "g726le",
    long_name: "raw little-endian G.726 (\"right-justified\")",
    extensions: &["g726le"],
    sample_rate: 8000,
    channels: 1,
    bytes_per_block: 1,
    frames_per_block: 2,
    codec_id: Some(CodecId::AdpcmG726le),
    target_packet_bytes: 1020,
};

pub const G728: RawCodecSpec = RawCodecSpec {
    name: "g728",
    long_name: "raw G.728",
    extensions: &["g728"],
    sample_rate: 8000,
    channels: 1,
    bytes_per_block: 1,
    frames_per_block: 4,
    codec_id: Some(CodecId::G728),
    target_packet_bytes: 1020,
};

pub const G729: RawCodecSpec = RawCodecSpec {
    name: "g729",
    long_name: "G.729 raw format",
    extensions: &["g729"],
    sample_rate: 8000,
    channels: 1,
    bytes_per_block: 10,
    frames_per_block: 80,
    codec_id: Some(CodecId::G729),
    target_packet_bytes: 10,
};

pub const APTX: RawCodecSpec = RawCodecSpec {
    name: "aptx",
    long_name: "raw aptX (Audio Processing Technology for Bluetooth)",
    extensions: &["aptx"],
    sample_rate: 48000,
    channels: 2,
    bytes_per_block: 4,
    frames_per_block: 4,
    codec_id: Some(CodecId::Aptx),
    target_packet_bytes: 1024,
};

pub const APTX_HD: RawCodecSpec = RawCodecSpec {
    name: "aptx_hd",
    long_name: "raw aptX HD (Audio Processing Technology for Bluetooth)",
    extensions: &["aptx_hd"],
    sample_rate: 48000,
    channels: 2,
    bytes_per_block: 6,
    frames_per_block: 4,
    codec_id: Some(CodecId::AptxHd),
    target_packet_bytes: 1536,
};

/// Every [`probe`] in this module answers only from the filename extension,
/// per the module docs.
#[must_use]
pub fn probe_by_extension(data: &ProbeData<'_>, spec: &RawCodecSpec) -> ProbeScore {
    if data.extension_matches(spec.extensions) {
        ProbeScore::EXTENSION
    } else {
        ProbeScore::NONE
    }
}

fn build(spec: &RawCodecSpec, src: Box<dyn MediaSource>) -> Result<RawCodecDemuxer> {
    let io = IoContext::new(src, &IoOptions::default())?;
    let mut stream = Stream::new(
        0,
        vaco_core::MediaType::Audio,
        Rational::new(1, spec.sample_rate.cast_signed().max(1)),
    );
    let mut params = vaco_codec_core::CodecParameters::audio();
    params.codec_id = spec.codec_id;
    if let Some(audio) = params.audio.as_mut() {
        audio.sample_rate = spec.sample_rate.max(1);
        audio.layout = vaco_chlayout::ChannelLayout::default_for(u32::from(spec.channels.max(1)));
    }
    stream.params = params;
    let size = io.size();
    let inner = BlockDemuxer::new(
        io,
        stream,
        0,
        size,
        spec.bytes_per_block,
        spec.frames_per_block,
        spec.target_packet_bytes,
    );
    Ok(RawCodecDemuxer {
        inner,
        budget: Budget::new(vaco_limits::Limits::permissive()),
    })
}

#[derive(Debug)]
pub struct RawCodecDemuxer {
    inner: BlockDemuxer,
    budget: Budget,
}

impl Demuxer for RawCodecDemuxer {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }
    fn read_packet(&mut self) -> Result<Packet> {
        self.inner.read_packet(&mut self.budget)
    }
    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        self.inner.seek(target, flags)
    }
    fn duration(&self) -> Option<vaco_core::Duration> {
        self.inner.duration()
    }
}

macro_rules! raw_codec_format {
    ($spec:ident, $probe_fn:ident, $open_fn:ident, $demuxer:ident) => {
        #[must_use]
        pub fn $probe_fn(data: &ProbeData<'_>) -> ProbeScore {
            probe_by_extension(data, &$spec)
        }

        fn $open_fn(
            src: Box<dyn MediaSource>,
            _parsers: &dyn ParserProvider,
        ) -> Result<Box<dyn Demuxer>> {
            Ok(Box::new(build(&$spec, src)?))
        }

        pub const $demuxer: DemuxerDesc = DemuxerDesc {
            name: $spec.name,
            long_name: $spec.long_name,
            extensions: $spec.extensions,
            mime_types: &[],
            flags: FormatFlags::GENERIC_INDEX,
            probe: $probe_fn,
            open: $open_fn,
        };
    };
}

raw_codec_format!(GSM, probe_gsm, open_gsm, DEMUXER_GSM);
raw_codec_format!(SLN, probe_sln, open_sln, DEMUXER_SLN);
raw_codec_format!(DFPWM, probe_dfpwm, open_dfpwm, DEMUXER_DFPWM);
raw_codec_format!(G722, probe_g722, open_g722, DEMUXER_G722);
raw_codec_format!(G726, probe_g726, open_g726, DEMUXER_G726);
raw_codec_format!(G726LE, probe_g726le, open_g726le, DEMUXER_G726LE);
raw_codec_format!(G728, probe_g728, open_g728, DEMUXER_G728);
raw_codec_format!(G729, probe_g729, open_g729, DEMUXER_G729);
raw_codec_format!(APTX, probe_aptx, open_aptx, DEMUXER_APTX);
raw_codec_format!(APTX_HD, probe_aptx_hd, open_aptx_hd, DEMUXER_APTX_HD);

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    #[test]
    fn every_spec_opens_and_reports_its_rate_and_channels() {
        for spec in [
            GSM, SLN, DFPWM, G722, G726, G726LE, G728, G729, APTX, APTX_HD,
        ] {
            let data = vec![0u8; spec.bytes_per_block as usize * 10];
            let d = build(&spec, Box::new(MemorySource::new(data))).unwrap();
            let s = d.streams().first().unwrap();
            assert_eq!(
                s.params.audio.as_ref().unwrap().sample_rate,
                spec.sample_rate
            );
            assert_eq!(s.params.codec_id, spec.codec_id);
        }
    }

    #[test]
    fn probe_never_claims_unrelated_content() {
        let text = ProbeData::new(b"just some prose, not a media file at all");
        for probe in [
            probe_gsm,
            probe_sln,
            probe_dfpwm,
            probe_g722,
            probe_g726,
            probe_g726le,
            probe_g728,
            probe_g729,
            probe_aptx,
            probe_aptx_hd,
        ] {
            assert_eq!(probe(&text), ProbeScore::NONE);
        }
    }

    #[test]
    fn probe_scores_are_from_the_published_table() {
        let named = ProbeData::new(b"whatever").with_filename("clip.gsm");
        assert_eq!(probe_gsm(&named), ProbeScore::EXTENSION);
    }

    #[test]
    fn g726_packings_select_distinct_decoder_ids() {
        assert_eq!(G726.codec_id, Some(CodecId::AdpcmG726));
        assert_eq!(G726LE.codec_id, Some(CodecId::AdpcmG726le));
    }
}
