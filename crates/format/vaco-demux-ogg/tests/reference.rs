//! Fidelity checks against real Ogg files, run on demand.
//!
//! `#[ignore]`d because they need files the repository does not carry.
//! Point them at ones built the same way `crate::granule`'s doc comments
//! were measured, and they report what this demuxer says, so the numbers in
//! `docs/format/vaco-demux-ogg.md` can be re-measured against a newer
//! reference rather than trusted.
//!
//! ```sh
//! ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=48000 \
//!        -c:a libopus opus.ogg
//! ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=44100 -ac 2 \
//!        -c:a vorbis -q:a 4 -strict -2 vorbis.ogg
//! ffmpeg -f lavfi -i sine=frequency=440:duration=2:sample_rate=44100 \
//!        -c:a flac flac.oga
//! VACO_OGG_FIXTURE=/tmp/opus.ogg cargo test -p vaco-demux-ogg \
//!        --test reference -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    reason = "a diagnostic harness, run by hand"
)]

use vaco_demux_ogg::OggDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::probe::ProbeData;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

fn fixture() -> Option<Vec<u8>> {
    let path = std::env::var("VACO_OGG_FIXTURE").ok()?;
    std::fs::read(path).ok()
}

#[test]
#[ignore = "needs VACO_OGG_FIXTURE"]
fn report_what_we_say_about_a_real_file() {
    let Some(bytes) = fixture() else {
        println!("VACO_OGG_FIXTURE unset or unreadable; nothing to do");
        return;
    };
    println!(
        "probe_score={:?}",
        vaco_demux_ogg::probe(&ProbeData::new(&bytes))
    );
    let mut d = OggDemuxer::open(
        Box::new(MemorySource::new(bytes)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("opens");
    for s in d.streams() {
        println!(
            "stream {} id={:?} codec={:?} ogg_codec={:?} time_base={:?} sample_rate={:?} channels={:?}",
            s.index,
            s.id,
            s.params.codec_id,
            s.metadata_get("ogg_codec"),
            s.time_base,
            s.params.audio.as_ref().map(|a| a.sample_rate),
            s.params
                .audio
                .as_ref()
                .and_then(|a| a.layout.as_ref())
                .map(ToString::to_string),
        );
    }
    let mut n = 0u64;
    let mut first = vec![None; d.streams().len()];
    let mut last_pts = vec![None; d.streams().len()];
    let mut last_dur = vec![None; d.streams().len()];
    while let Ok(p) = d.read_packet() {
        let i = p.stream_index as usize;
        if let Some(slot) = first.get_mut(i)
            && slot.is_none()
        {
            *slot = Some((p.pts.ticks(), p.len));
        }
        if let Some(slot) = last_pts.get_mut(i) {
            *slot = p.pts.ticks();
        }
        if let Some(slot) = last_dur.get_mut(i) {
            *slot = Some(p.duration.as_micros());
        }
        n += 1;
    }
    println!("packets={n}");
    for i in 0..first.len() {
        println!(
            "stream {i}: first={:?} last_pts={:?} last_dur_us={:?}",
            first[i], last_pts[i], last_dur[i]
        );
    }
    println!("stats={:?}", d.stats());
}
