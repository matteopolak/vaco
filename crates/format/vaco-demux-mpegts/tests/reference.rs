//! Fidelity checks against a real transport stream, run on demand.
//!
//! These are `#[ignore]`d because they need a file the repository does not
//! carry: a committed `.ts` large enough to exercise the duration tail scan
//! would be megabytes. Point them at one and they report what this demuxer
//! says, so the numbers in `docs/format/vaco-demux-mpegts.md` can be
//! re-measured against a newer reference rather than trusted.
//!
//! ```sh
//! ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25:duration=10 \
//!        -f lavfi -i sine=duration=10 \
//!        -c:v libx264 -c:a aac -f mpegts /tmp/a.ts
//! VACO_TS_FIXTURE=/tmp/a.ts cargo test -p vaco-demux-mpegts \
//!        --test reference -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::integer_division,
    clippy::redundant_closure_for_method_calls,
    reason = "a diagnostic harness, run by hand"
)]

use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

fn fixture() -> Option<Vec<u8>> {
    let path = std::env::var("VACO_TS_FIXTURE").ok()?;
    std::fs::read(path).ok()
}

#[test]
#[ignore = "needs VACO_TS_FIXTURE"]
fn report_what_we_say_about_a_real_file() {
    let Some(bytes) = fixture() else {
        println!("VACO_TS_FIXTURE unset or unreadable; nothing to do");
        return;
    };
    let size = bytes.len();
    println!(
        "probe_score={:?}",
        vaco_demux_mpegts::probe(&vaco_format_core::probe::ProbeData::new(&bytes))
    );
    let mut d = MpegTsDemuxer::open(
        Box::new(MemorySource::new(bytes)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("opens");
    println!("size={size} stride={:?}", d.stride());
    for p in d.programs() {
        println!("program id={} streams={:?}", p.id, p.stream_indices);
        for (k, v) in &p.metadata {
            println!("  {k}={v}");
        }
    }
    for s in d.streams() {
        println!(
            "stream {} pid={:?} codec={:?} ts_codec={:?} tag={:?} start_pts={:?} duration={:?}",
            s.index,
            s.id,
            s.params.codec_id,
            s.metadata_get("ts_codec"),
            s.params.codec_tag,
            s.start_time.ticks(),
            s.duration().map(|d| d.as_micros()),
        );
    }
    let dur = d.duration();
    println!("container duration = {:?} us", dur.map(|d| d.as_micros()));
    if let Some(dur) = dur
        && dur.as_micros() > 0
    {
        let bits = (size as u128) * 8 * 1_000_000;
        println!("bit_rate = {}", bits / dur.as_micros() as u128);
    }
    let mut counts = vec![0u64; d.streams().len()];
    let mut keys = vec![0u64; d.streams().len()];
    let mut first = vec![None; d.streams().len()];
    let mut last = vec![None; d.streams().len()];
    let mut first_pos = vec![None; d.streams().len()];
    while let Ok(p) = d.read_packet() {
        let i = p.stream_index as usize;
        if let Some(c) = counts.get_mut(i) {
            *c += 1;
        }
        if p.is_key()
            && let Some(k) = keys.get_mut(i)
        {
            *k += 1;
        }
        if let Some(slot) = first_pos.get_mut(i)
            && slot.is_none()
        {
            *slot = p.pos;
        }
        if let Some(slot) = first.get_mut(i)
            && slot.is_none()
        {
            *slot = p.pts.ticks();
        }
        if let Some(slot) = last.get_mut(i) {
            *slot = p.pts.ticks();
        }
    }
    for i in 0..counts.len() {
        println!(
            "stream {i}: {} packets ({} key), pts {:?}..{:?}, first pos {:?}",
            counts[i], keys[i], first[i], last[i], first_pos[i]
        );
    }
    println!("stats = {:?}", d.stats());
}
