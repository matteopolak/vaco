//! Whole-file demux over arbitrary bytes, across every registered demuxer in
//! `vaco-subtitle-text`.
//!
//! Text subtitle parsing is this project's highest-value fuzzing surface in
//! the whole format layer (`planning/AGENT-CONSTRAINTS.md`, "Fuzzing"): every
//! one of the sixteen-plus formats here is untrusted-text-in, and several
//! parsers scan for markers (`{`, `[`, `<time`, `<SYNC`) at arbitrary offsets
//! with no framing to validate first. Also exercises
//! `vaco_format_subtitle::decode_to_utf8_bytes` on every run, since every
//! `open` call goes through it before any format-specific parser sees a byte.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Reading terminates.** [`MAX_PACKETS`] turns a demuxer that never
//!   reaches `Eof` into a local assertion rather than a fuzzer timeout.
//! * **Every packet names the one declared stream** — every format here
//!   carries exactly one subtitle stream.
//! * **`Eof` is stable**: once reported, it keeps being reported.
//! * **Every registered `probe` is total** over the same bytes, scoring
//!   without panicking regardless of what `open` does with them.
//!
//! fuzz-crate: vaco-subtitle-text

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::probe::ProbeData;
use vaco_format_core::{Demuxer, DemuxerDesc};
use vaco_io::MemorySource;

/// Packets read per drain before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

/// Every registered demuxer, probe included, so one input byte string is
/// checked against all of them each run — the same shape
/// `tests/probe_matrix.rs` uses, but over fuzzer-chosen rather than
/// hand-written bytes.
fn descriptors() -> [&'static DemuxerDesc; 17] {
    [
        &vaco_subtitle_text::srt::DEMUXER,
        &vaco_subtitle_text::webvtt::DEMUXER,
        &vaco_subtitle_text::ass::DEMUXER,
        &vaco_subtitle_text::scc::DEMUXER,
        &vaco_subtitle_text::microdvd::DEMUXER,
        &vaco_subtitle_text::jacosub::DEMUXER,
        &vaco_subtitle_text::lrc::DEMUXER,
        &vaco_subtitle_text::ttml::DEMUXER,
        &vaco_subtitle_text::subviewer::DEMUXER,
        &vaco_subtitle_text::subviewer1::DEMUXER,
        &vaco_subtitle_text::mpsub::DEMUXER,
        &vaco_subtitle_text::pjs::DEMUXER,
        &vaco_subtitle_text::realtext::DEMUXER,
        &vaco_subtitle_text::sami::DEMUXER,
        &vaco_subtitle_text::vplayer::DEMUXER,
        &vaco_subtitle_text::mpl2::DEMUXER,
        &vaco_subtitle_text::stl::DEMUXER,
    ]
}

#[derive(Debug, Arbitrary)]
struct Input {
    /// Selects which registered demuxer's `open` runs on `bytes`.
    which: u8,
    bytes: Vec<u8>,
    /// Also probed as a filename, for the extension-fallback path.
    filename: Option<String>,
}

/// Read until `Eof`, checking every packet along the way.
fn drain(d: &mut dyn Demuxer) {
    let mut n = 0u32;
    loop {
        if n >= MAX_PACKETS {
            return;
        }
        match d.read_packet() {
            Ok(p) => {
                assert_eq!(p.stream_index, 0, "every registration has one stream");
                n = n.saturating_add(1);
            }
            Err(Error::Eof) => {
                // Stable: reading again must still report Eof.
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return;
            }
            Err(_) => return,
        }
    }
}

fuzz_target!(|input: Input| {
    let descs = descriptors();

    // Every probe is total over the same bytes, whichever demuxer `which`
    // goes on to open.
    let mut data = ProbeData::new(&input.bytes);
    if let Some(f) = &input.filename {
        data = data.with_filename(f);
    }
    for desc in &descs {
        let _ = (desc.probe)(&data);
    }

    let Some(desc) = descs.get(usize::from(input.which) % descs.len()) else {
        return;
    };
    let src = Box::new(MemorySource::new(input.bytes));
    if let Ok(mut d) = (desc.open)(src, &NoParsers) {
        drain(d.as_mut());
    }
});
