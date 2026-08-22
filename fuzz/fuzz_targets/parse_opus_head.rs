//! The Opus identification and comment headers.
//!
//! Both arrive as raw container payloads — an Ogg packet, a Matroska
//! `CodecPrivate`, an MP4 `dOps` box — so neither has any framing in front of
//! it. The identification header's mapping table is length-driven by a byte the
//! attacker chooses; the comment header is worse, carrying a 32-bit count of
//! 32-bit-length strings, which is the classic "declared length" amplification
//! plan 13 §2.2.2 exists to stop.
//!
//! Properties:
//!
//! 1. Every accepted identification header re-serialises to the bytes it came
//!    from, and re-parses to an equal value.
//! 2. Its channel count, layout and stream counts agree with each other.
//! 3. A comment header never reports more comments than its iterator yields,
//!    and iterating one is bounded by the packet, not by the declared count.
//!
//! fuzz-crate: vaco-parse-opus
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_parse_opus::{CommentHeader, IdentificationHeader, MappingFamily, OUTPUT_SAMPLE_RATE};

fuzz_target!(|data: &[u8]| {
    if let Ok(head) = IdentificationHeader::parse(data) {
        assert!(head.channel_count > 0);
        assert_eq!(head.version >> 4, 0);
        assert!(head.stream_count > 0);
        assert!(head.coupled_count <= head.stream_count);
        assert!(u16::from(head.stream_count) + u16::from(head.coupled_count) <= 255);

        let re = head.to_opus_head();
        assert_eq!(re.as_slice(), &data[..re.len()], "header did not round-trip");
        let again = IdentificationHeader::parse(re.as_slice())
            .unwrap_or_else(|e| panic!("re-serialised header does not parse: {e}"));
        assert_eq!(again, head);

        if let Some(layout) = head.channel_layout() {
            assert_eq!(layout.channels, u32::from(head.channel_count));
        }
        if head.mapping_family.has_mapping_table() {
            assert_eq!(head.channel_mapping.len(), usize::from(head.channel_count));
        } else {
            assert_eq!(head.mapping_family, MappingFamily::Rtp);
            assert!(head.channel_count <= 2);
            assert!(head.channel_mapping.is_empty());
        }

        let params = head.to_codec_parameters();
        assert!(params.check_consistent().is_ok());
        let audio = params.audio.expect("an Opus stream is audio");
        assert_eq!(audio.sample_rate, OUTPUT_SAMPLE_RATE);
        assert_eq!(audio.initial_padding, u32::from(head.pre_skip));
    }

    // `dOps` is the same fields big-endian, so it gets the same treatment.
    let _ = IdentificationHeader::parse_dops(data);

    if let Ok(comments) = CommentHeader::parse(data) {
        let yielded = comments.iter().count();
        assert_eq!(
            yielded as u64,
            u64::from(comments.len()),
            "the declared comment count and the list disagree"
        );
        // Every comment lies inside the packet, so the total cannot exceed it.
        let bytes: usize = comments.iter().map(str::len).sum();
        assert!(bytes <= data.len());
        for pair in comments.pairs() {
            assert!(!pair.0.contains('='));
        }
        let _ = comments.r128_track_gain();
        let _ = comments.r128_album_gain();
    }
});
