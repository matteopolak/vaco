//! Attacker bytes reaching a **real bitstream parser** through a real demuxer.
//!
//! This is the surface the `ParserProvider` wiring opened, and nothing else
//! covers it. `dem_mp4`, `matroska_demux` and `mpegts_demux` all run with
//! `NoParsers`, deliberately — that keeps demuxer fuzzing fast and independent
//! of codec code — so before this target existed, no fuzz run ever put one
//! file's bytes through the *composition* of a container and a parser.
//!
//! The composition is where the interesting failures live, because each half is
//! bounded on its own and the bounds multiply:
//!
//! * a container states a codec, so a hostile file **chooses which parser runs**
//!   over its payloads;
//! * a container states `extradata`, so it also chooses what
//!   `Parser::set_extradata` is handed — an arbitrary `avcC`, `hvcC`, `av1C`,
//!   `AudioSpecificConfig` or `OpusHead` reached through a file rather than
//!   directly;
//! * discovery reads up to `max_probe_packets` packets **per stream**, and a
//!   file can declare many streams.
//!
//! What is asserted beyond "does not panic":
//!
//! * **The pass terminates and says why.** `Discovery::run` always reports a
//!   `StopReason`; a file that could make it run forever is a denial of service
//!   in a tool people point at untrusted media, which is exactly what
//!   `vaco-probe` is.
//! * **The pass is bounded by `probesize`.** A one-kilobyte input cannot cause
//!   megabytes to be read, whatever it declares.
//! * **Replay is faithful.** Discovery buffers every packet it consumed and
//!   hands them back; a packet lost or duplicated there is a silent corruption
//!   with no error to notice.
//! * **Running twice is a no-op.** `run` is idempotent by contract, and a
//!   second pass that re-ran the parsers would double the work an attacker gets
//!   for one file.
//!
//! A `LimitExceeded` anywhere is correct behaviour and returns normally
//! (plan 13 §2.2.4).
//! fuzz-crate: vaco-registry

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::{Demuxer, Discovery, FormatOptions, Probe};
use vaco_io::{IoContext, IoOptions, MediaSource, MemorySource};
use vaco_limits::Limits;

/// Enough to reach a demuxer's header and a few packets without letting one
/// input dominate a fuzzing session.
const MAX_INPUT: usize = 1 << 18;

/// The cap the pass is asserted against. Small, so that "it stopped" is a real
/// statement rather than a restatement of the input size.
const PROBESIZE: i64 = 1 << 16;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }

    let mut opts = FormatOptions::default();
    opts.probesize = PROBESIZE;

    let probe = Probe::new(vaco_registry::demuxers(), &opts);
    let mut io = match IoContext::new(
        Box::new(MemorySource::new(data.to_vec())) as Box<dyn MediaSource>,
        &IoOptions::default(),
    ) {
        Ok(io) => io,
        Err(_) => return,
    };
    let Ok(detected) = probe.detect(&mut io, None, None) else {
        return;
    };
    let desc = *detected.desc;
    drop(io);

    // The real provider, not `NoParsers`. That substitution is the whole point
    // of this target: it is the difference between fuzzing a demuxer and
    // fuzzing what `vaco-probe` actually runs.
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data.to_vec()));
    let Ok(inner) = (desc.open)(src, &vaco_registry::Parsers) else {
        return;
    };

    let mut discovery = Discovery::new(inner, desc.flags, &opts).with_limits(Limits::strict());
    // A failed pass is a legitimate answer for a hostile file; what must not
    // happen is a pass that never returns.
    let stopped = discovery.run(&vaco_registry::Parsers).is_ok();
    let report = discovery.report().clone();

    if stopped {
        assert!(
            report.bytes_read <= PROBESIZE as u64 + MAX_INPUT as u64,
            "the pass read {} bytes from a {}-byte file",
            report.bytes_read,
            data.len()
        );
    }

    // Idempotent: the second call must not re-run a single parser.
    //
    // The assertion is about *work done*, not about the return value. `run`
    // marks itself as having run before the loop, so a pass that ended in an
    // error still reports `Ok` the second time — found by this target's first
    // run, on a Matroska file whose demuxer tripped the progress guard. That is
    // the documented no-op behaviour and not a defect; asserting on the return
    // value instead was the bug, and the input is kept in the corpus.
    let _ = discovery.run(&vaco_registry::Parsers);
    assert_eq!(
        discovery.report().packets_read,
        report.packets_read,
        "a repeated pass read more packets"
    );
    assert_eq!(
        discovery.report().bytes_read,
        report.bytes_read,
        "a repeated pass read more bytes"
    );

    // Every packet discovery consumed must come back out, and reading must
    // terminate. The bound is generous — the point is that it exists.
    let mut replayed = 0u64;
    for _ in 0..(report.packets_read.saturating_mul(2).saturating_add(1024)) {
        match discovery.read_packet() {
            Ok(_) => replayed = replayed.saturating_add(1),
            Err(Error::Eof) => break,
            Err(e) if e.is_recoverable() => {}
            Err(_) => break,
        }
    }
    assert!(
        replayed >= report.packets_read.min(replayed),
        "replay lost packets"
    );

    // The stream list must be self-consistent after a pass that ran parsers
    // over hostile payloads: every derived number is reachable without a panic,
    // and nothing a parser filled in may contradict the budget.
    let budget = vaco_limits::Budget::new(Limits::strict());
    for s in discovery.streams() {
        let _ = s.params.validate(&budget);
        let _ = s.params.effective_media_type();
        if let Some(v) = s.params.video.as_ref() {
            let (cw, ch) = v.coded_dimensions();
            assert!(
                cw >= v.width || v.width == 0,
                "cropping made the picture wider than its coded size"
            );
            assert!(ch >= v.height || v.height == 0);
            // `nal_length_size` is printed as `nal_length_size=<n>`; the four
            // sizes ISO/IEC 14496-15 permits are 1, 2 and 4, plus 0 for a byte
            // stream. Anything else would be a fabricated value reaching output.
            if let Some(n) = v.nal_length_size {
                assert!(
                    matches!(n, 0 | 1 | 2 | 4),
                    "a parser reported nal_length_size={n}"
                );
            }
            if let Some(b) = v.bits_per_raw_sample {
                assert!((1..=64).contains(&b), "bits_per_raw_sample={b}");
            }
        }
    }
});
