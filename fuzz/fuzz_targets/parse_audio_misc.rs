//! Vorbis identification header, Xiph header unpacking, FLAC `STREAMINFO`
//! and `ALACSpecificConfig` over arbitrary bytes.
//!
//! None of the four take a budget — they only read fixed-size or
//! declared-length fields out of a caller-supplied slice — so panic-freedom
//! on any length, including zero, is most of what there is to check.
//! `unpack_headers` is the one with a real amplification shape (a
//! Xiph-laced length can run past the blob), the same class
//! `parse_opus_head` exists to catch for `OpusTags`.
//!
//! fuzz-crate: vaco-parse-audio-misc
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_parse_audio_misc::alac::AlacSpecificConfig;
use vaco_parse_audio_misc::flac::StreamInfo;
use vaco_parse_audio_misc::vorbis::{IdentificationHeader, unpack_headers};

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = IdentificationHeader::parse(data) {
        assert!(header.channels > 0);
        assert!(header.sample_rate > 0);
        let params = header.to_codec_parameters();
        assert!(params.check_consistent().is_ok());
    }

    if let Ok(headers) = unpack_headers(data) {
        let total: usize = headers.iter().map(|h| h.len()).sum();
        assert!(total <= data.len());
    }

    if let Ok(info) = StreamInfo::parse(data) {
        assert!(info.sample_rate > 0);
        assert!(info.channels > 0);
        let params = info.to_codec_parameters();
        assert!(params.check_consistent().is_ok());
    }

    if let Ok(config) = AlacSpecificConfig::parse(data) {
        assert!(config.num_channels > 0);
        assert!(config.sample_rate > 0);
        let params = config.to_codec_parameters();
        assert!(params.check_consistent().is_ok());
    }
});
