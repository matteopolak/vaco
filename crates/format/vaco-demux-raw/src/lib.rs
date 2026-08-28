//! Raw / headerless elementary-stream demuxers: 49 registrations across
//! four families.
//!
//! A raw format has no container: the file *is* the elementary stream.
//! `-f h264 in.h264`, `-f s16le in.pcm`, `-f rawvideo -s 1920x1080
//! -pix_fmt yuv420p in.yuv`. Geometry, sample rate and frame rate come from
//! options, never from the file — except `yuv4mpegpipe`, which carries its
//! own text header.
//!
//! # Layout
//!
//! | Module | Family | Count |
//! |---|---|---|
//! | [`pcm`] | Linear PCM: `alaw` … `vidc` | 21 |
//! | [`rawvideo`] | `rawvideo`, `bitpacked`, `v210`, `v210x` | 4 |
//! | [`y4m`] | `yuv4mpegpipe` (self-describing) | 1 |
//! | [`bitstream`] | Bitstream-with-sync-pattern: `h264`, `hevc`, `av1`/`obu`, and 17 more | 21 |
//! | [`ac3`] | `ac3`, `eac3`: syncframe-driven, own streaming demuxer (not the whole-buffer `bitstream` shape — frame length and sample count come from the header, so per-packet timestamps are exact) | 2 |
//!
//! 21 + 4 + 1 + 21 + 2 = 49. `s337m` moved to `vaco-format-spdif` (see
//! `planning/TECH-DEBT.md`), dropping this family from 22 to 21. The first
//! 48 (before that move) matched FM-26a and
//! `ffmpeg -demuxers`' own count for this family (captured under `LC_ALL=C`
//! against ffmpeg 8.1 — see `docs/format/vaco-demux-raw.md` for the exact
//! commands); `ac3`/`eac3` are FM-26a's one deferred pair (#653).
//!
//! # The layering seam (D14.1)
//!
//! This crate depends on `vaco-codec-core` (layer 3a: `CodecId`,
//! `CodecParameters`, the `Parser` trait) but on no `vaco-parse-*` or
//! `vaco-codec-<name>` crate. Where a real parser would change behaviour —
//! `h264`, `hevc`, `av1`, `obu` — this crate asks for one through the
//! injected [`vaco_format_core::ParserProvider`], exactly as
//! `vaco-demux-mp4` and `vaco-demux-mpegts` do. Every other codec in this
//! crate's scope (the other 18 bitstream registrations) has no parser
//! anywhere in the workspace yet, so they fall back to the structural
//! framing documented in [`bitstream`]. (17, not 18, since `s337m` moved
//! to `vaco-format-spdif`.)
//!
//! # `CodecId` cannot yet name most of these codecs
//!
//! `vaco_codec_core::CodecId` has 16 variants today: enough for `h264`,
//! `hevc`, `av1`/`obu`, `pcm` (generic) and `jpeg` (used here for `mjpeg`,
//! approximately), and no others this crate needs — no `Rawvideo`, no
//! `Vc1`/`Mpeg1Video`/`Mpeg2Video`/`H263`/`Mpeg4`/`Vvc`/`Evc`/`Avs2`/`Avs3`/
//! `Cavs`/`Dirac`/`Dnxhd`, and no per-subtype PCM tag (`pcm_s16le` is one
//! `CodecId::Pcm`, not its own variant). Every demuxer here that hits this
//! gap sets `codec_id = None` and records the reference's exact name as
//! stream metadata (`raw_codec_name`), the same convention
//! `vaco-demux-mpegts` uses for `TsCodec` values `CodecId` cannot express.
//! **This means `vaco-probe -show_streams` cannot yet print a byte-identical
//! `codec_name` for roughly forty of this crate's forty-seven registrations**
//! — the single biggest reported gap in this delivery. See the docs file.
//!
//! # The registry seam has no options parameter
//!
//! The frozen `DemuxerDesc::open` is `fn(Box<dyn MediaSource>, &dyn
//! ParserProvider) -> Result<Box<dyn Demuxer>>` — no options. Every format in
//! this crate is *defined* by its options (`-sample_rate`, `-ch_layout`,
//! `-video_size`, `-pixel_format`, `-framerate`): a raw PCM file opened
//! through the registry's `open` function gets the reference's own defaults
//! (44100 Hz mono; `yuv420p`; 25 fps) because there is nowhere else for a
//! caller to put a different value. `PcmDemuxer::open`,
//! `RawVideoDemuxer::open` and `BitstreamDemuxer::open` all take an explicit
//! options struct for a caller that reaches them directly (an embedder, the
//! eventual CLI layer, a test); only the registry path is stuck at defaults.
//! This is the same gap `vaco-demux-mpegts` and `vaco-demux-mp4` already
//! carry for `FormatOptions`, generalised to formats whose *entire*
//! behaviour is option-driven rather than just tuned by it. Reported, not
//! worked around.

#![forbid(unsafe_code)]

pub mod ac3;
pub mod bitstream;
pub mod obu;
pub mod pcm;
pub mod rawvideo;
mod startcode;
pub mod y4m;

use vaco_format_core::DemuxerDesc;

/// Every demuxer this crate registers, in the order `ffmpeg -demuxers`
/// prints the family (PCM, then raw video, then bitstream — `yuv4mpegpipe`
/// sorts alphabetically among them in the reference's actual listing, but
/// grouping by family here is clearer and the registry itself sorts by name
/// per `planning/18-formats.md` §1.5 R6).
#[must_use]
pub fn all_demuxers() -> Vec<&'static DemuxerDesc> {
    let mut out = Vec::new();
    out.extend(pcm::PCM_DEMUXERS.iter().copied());
    out.push(&rawvideo::DEMUXER_RAWVIDEO);
    out.push(&rawvideo::DEMUXER_BITPACKED);
    out.push(&rawvideo::DEMUXER_V210);
    out.push(&rawvideo::DEMUXER_V210X);
    out.push(&y4m::DEMUXER_YUV4MPEGPIPE);
    out.extend(bitstream::BITSTREAM_DEMUXERS.iter().copied());
    out.push(&ac3::DEMUXER_AC3);
    out.push(&ac3::DEMUXER_EAC3);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_forty_nine_registrations() {
        assert_eq!(all_demuxers().len(), 49);
    }

    #[test]
    fn every_name_is_unique() {
        let all = all_demuxers();
        let mut names: Vec<&str> = all.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "duplicate demuxer name registered");
    }

    #[test]
    fn every_probe_is_total_over_an_empty_buffer() {
        // The registry's scoring engine calls every candidate's probe on a
        // small prefix of attacker-controlled bytes; nothing here may panic.
        use vaco_format_core::probe::ProbeData;
        for d in all_demuxers() {
            let _ = (d.probe)(&ProbeData::new(&[]));
            let _ = (d.probe)(&ProbeData::new(&[0u8; 64]));
        }
    }
}
