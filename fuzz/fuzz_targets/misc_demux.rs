//! Whole-file demux over arbitrary bytes, for every format in
//! `vaco-format-misc` at once: `ivf`, `ffmetadata`, `roq`, `flic`, `cdg`.
//!
//! One target for the same reason `audio_simple_demux` covers nine formats
//! at once: each header parser is independent, the signatures are long
//! enough that one input essentially never satisfies two of them, and the
//! fuzzer's corpus partitions across all five over time.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Eof is sticky** — a second `read_packet` after `Eof` reports `Eof`
//!   again, never something a caller could mistake for real data.
//! * **A packet's payload never exceeds its own buffer.**
//! * **Reading terminates**, via a packet-count cap. `roq`'s chunk-by-chunk
//!   accumulation loop and `ivf`'s frame walk are both driven purely by
//!   attacker-controlled length fields, which is exactly the shape this
//!   family's chunk-length prefixes are expected to stress.
//! * **Every stream reports the media type its own module promises**: `ivf`,
//!   `flic` and `cdg` always video and exactly one stream; `roq` one video
//!   stream plus, sometimes, one audio stream; `ffmetadata` never any
//!   stream at all (module docs — `[STREAM]` sections are tags, not
//!   phantom streams here).
//! * **`Probe::probe` never scores above 100** for any of the five, over the
//!   same bytes `open` was tried on.
//!
//! fuzz-crate: vaco-format-misc

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, MediaType};
use vaco_format_core::Demuxer;
use vaco_io::MediaSource;

const MAX_PACKETS: u32 = 50_000;

fn drain(d: &mut dyn Demuxer) {
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert!(
                    p.len <= p.data.len(),
                    "packet payload longer than its buffer"
                );
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)), "Eof is not sticky");
                return;
            }
            Err(_) => return,
        }
    }
}

fn media_types(streams: &[vaco_format_core::Stream]) -> Vec<Option<MediaType>> {
    streams.iter().map(vaco_format_core::Stream::media_type).collect()
}

fn src(data: &[u8]) -> Box<dyn MediaSource> {
    Box::new(vaco_io::MemorySource::new(data.to_vec()))
}

fuzz_target!(|data: &[u8]| {
    if let Ok(mut d) = vaco_format_misc::ivf::IvfDemuxer::open(src(data)) {
        let types = media_types(d.streams());
        assert_eq!(types.len(), 1, "ivf: expected exactly one stream");
        assert_eq!(types.first().copied().flatten(), Some(MediaType::Video));
        drain(&mut d);
    }

    if let Ok(mut d) = vaco_format_misc::ffmetadata::FfmetadataDemuxer::open(src(data)) {
        assert!(d.streams().is_empty(), "ffmetadata: never reports a phantom stream");
        drain(&mut d);
    }

    if let Ok(mut d) = vaco_format_misc::roq::RoqDemuxer::open(src(data)) {
        let types = media_types(d.streams());
        assert!(
            (1..=2).contains(&types.len()),
            "roq: expected one or two streams, got {}",
            types.len()
        );
        assert_eq!(types.first().copied().flatten(), Some(MediaType::Video));
        if let Some(second) = types.get(1) {
            assert_eq!(*second, Some(MediaType::Audio));
        }
        drain(&mut d);
    }

    if let Ok(mut d) = vaco_format_misc::flic::FlicDemuxer::open(src(data)) {
        let types = media_types(d.streams());
        assert_eq!(types.len(), 1, "flic: expected exactly one stream");
        assert_eq!(types.first().copied().flatten(), Some(MediaType::Video));
        drain(&mut d);
    }

    if let Ok(mut d) = vaco_format_misc::cdg::CdgDemuxer::open(src(data)) {
        let types = media_types(d.streams());
        assert_eq!(types.len(), 1, "cdg: expected exactly one stream");
        assert_eq!(types.first().copied().flatten(), Some(MediaType::Video));
        drain(&mut d);
    }

    let probe_data = vaco_format_core::probe::ProbeData::new(data);
    for score in [
        vaco_format_misc::ivf::probe(&probe_data),
        vaco_format_misc::ffmetadata::probe(&probe_data),
        vaco_format_misc::roq::probe(&probe_data),
        vaco_format_misc::flic::probe(&probe_data),
        vaco_format_misc::cdg::probe(&probe_data),
    ] {
        assert!(score.value() <= 100, "probe score out of range");
    }
});
