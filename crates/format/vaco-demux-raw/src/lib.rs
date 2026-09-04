//! Raw and headerless elementary-stream demuxers: 50 registrations.
//!
//! A raw file is the elementary stream itself. Geometry, sample rate, and
//! frame rate therefore come from options, except for self-describing
//! `yuv4mpegpipe`.
//!
//! The registrations comprise 21 linear-PCM formats, four raw-video formats,
//! one Y4M format, 21 sync-pattern bitstreams, two AC-3 variants, and bare ADTS
//! AAC. AC-3 and AAC use streaming syncframe readers whose headers supply
//! exact frame lengths and sample counts; [`bitstream`] describes the
//! whole-buffer framing strategies used by the other compressed streams.
//!
//! H.264, HEVC, AV1, and OBU demuxers request stateful parsers through the
//! injected [`vaco_format_core::ParserProvider`]. Formats without a workspace
//! parser use structural framing. `s337m` is deliberately registered by
//! `vaco-format-spdif`, not this crate.
//!
//! [`vaco_codec_core::CodecId`] cannot name every raw codec. Those demuxers set
//! `codec_id = None` and retain the reference name in `raw_codec_name`, so the
//! identity is not silently guessed.
//!
//! The registry's `DemuxerDesc::open` callback has no options parameter.
//! Registry construction therefore uses the reference defaults (44,100 Hz
//! mono, `yuv420p`, and 25 fps), while the direct PCM, raw-video, and bitstream
//! constructors accept explicit option structs.

#![forbid(unsafe_code)]

pub mod aac;
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
/// grouping by family here is clearer; the registry sorts by name.
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
    out.push(&aac::DEMUXER);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_fifty_registrations() {
        assert_eq!(all_demuxers().len(), 50);
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
