//! `AudioSpecificConfig` and the LATM `StreamMuxConfig` that embeds it.
//!
//! This is the structure a demuxer hands straight from an MP4 `esds` box to the
//! parser, so the bytes are attacker-chosen with no framing in front of them.
//! The syntax nests — a hierarchical object type reads a second object type,
//! which selects a different specific configuration, which may be followed by a
//! sync extension that reads a third — and each of those steps is somewhere a
//! parser can be steered into reading past the end or into a loop.
//!
//! The properties asserted here hold for every accepted configuration:
//!
//! 1. The reported sample rate is one of the two rates the configuration
//!    actually names, never a computed third value.
//! 2. The reported channel count is the configuration's own, or exactly two
//!    when Parametric Stereo turns a mono core into a stereo output.
//! 3. `bits_read` never exceeds the input, so a `StreamMuxConfig` that skips a
//!    configuration by its declared length cannot be made to skip backwards.
//! 4. A layout, when there is one, has the channel count the header implies.
//!
//! fuzz-crate: vaco-parse-aac
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::BitReader;
use vaco_parse_aac::{AudioSpecificConfig, Signal, StreamMuxConfig, tables};

fuzz_target!(|data: &[u8]| {
    if let Ok(cfg) = AudioSpecificConfig::parse(data) {
        let bits = u64::from(cfg.bits_read);
        assert!(
            bits <= (data.len() as u64) * 8,
            "claimed {bits} bits from {} bytes",
            data.len()
        );

        let rate = cfg.output_sample_rate();
        assert!(
            rate == cfg.sampling_frequency || rate == cfg.extension_sampling_frequency,
            "reported rate {rate} is neither the core rate nor the extension rate"
        );
        assert!(cfg.sampling_frequency != 0, "an accepted core rate is never zero");

        if let Some(channels) = cfg.output_channels() {
            let base = tables::channels_for_config(cfg.channel_configuration)
                .expect("output_channels agreed there was one");
            assert!(
                channels == base || (base == 1 && channels == 2),
                "{base} channels became {channels}"
            );
            if channels == 2 && base == 1 {
                assert!(matches!(cfg.sbr, Signal::Present));
                assert!(cfg.ps.is_not_absent());
            }
            if let Some(layout) = cfg.channel_layout() {
                assert_eq!(layout.channels, channels, "layout disagrees with the count");
            }
        }

        let frame = cfg.frame_length();
        assert!(matches!(frame, 480 | 512 | 960 | 1024));

        // The parameters a container would report must be self-consistent.
        let params = cfg.to_codec_parameters();
        assert!(params.check_consistent().is_ok());
    }

    // The same bytes read as a `StreamMuxConfig`, which embeds the above.
    let mut reader = BitReader::new(data);
    if let Ok(config) = StreamMuxConfig::read(&mut reader) {
        assert!(!config.streams.is_empty());
        assert!(config.programs >= 1);
        assert!(config.sub_frames >= 1);
        assert!(
            reader.bit_pos() <= reader.logical_bits(),
            "an accepted StreamMuxConfig read past the end"
        );
        // The first layer of the first program always carries a configuration:
        // `useSameConfig` is defined to be zero there.
        assert!(config.primary_config().is_some());
    }
});
