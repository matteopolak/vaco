//! Whole-file MP4 demuxing over arbitrary bytes.
//!
//! The highest-value target in the crate: one input is one file, and every
//! stage — the top-level scan, the `moov` parse, track building, metadata,
//! the sample tables, the fragment chain, packet emission and seeking — is
//! reachable from it.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Reading terminates.** A uniform `stsz` can declare four billion samples
//!   in twelve bytes and a `trun` can do the same with no per-sample payload
//!   at all. The demuxer bounds both against the source's own size, so a small
//!   input must produce a small number of packets — the assertion below is
//!   what would catch that bound being lost.
//! * **Packets lie inside the file.** `pos + len <= size` for every packet,
//!   whatever the chunk offsets claimed.
//! * **Decode times do not go backwards** within a stream — for a *progressive*
//!   file. `stts` deltas are unsigned, so a non-fragmented track's decode times
//!   are non-decreasing by construction and a regression means the accumulator
//!   wrapped. A **fragmented** file is deliberately exempt: `tfdt` places each
//!   fragment absolutely, so a corrupt sample duration inside one fragment
//!   followed by the next fragment's `tfdt` is a genuine backwards jump that
//!   the file states and the demuxer must report. Repairing it is
//!   `vaco-format-core`'s job (rule R22), on the far side of a boundary this
//!   crate does not cross. The first run of this target found exactly that
//!   input, and the finding was in the assertion.
//! * **`Eof` is stable.** Once end of stream is reported it must keep being
//!   reported; `vaco-format-core`'s docs record this as a bug class every
//!   demuxer has to close for itself.
//! * **Seeking is total, and lands on a real packet.** Every packet a seek
//!   produces must appear in the sequence a straight read produces — a seek may
//!   not invent one — and for a progressive file it must not land *after* the
//!   last packet a straight read would have reached at the target.
//!
//!   Stated that way on purpose. The obvious form, "the first packet after a
//!   backward seek has `dts <= target`", is **false**, and three separate real
//!   inputs proved it: a track whose edit list starts after the target, a
//!   non-reference stream placed relative to where the reference landed, and a
//!   truncated file where the samples between the landing point and the first
//!   readable one lie outside the file. Comparing against the file's own packet
//!   sequence is both stronger and actually true.
//! fuzz-crate: vaco-demux-mp4

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Timestamp};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget};
use vaco_io::{MediaSource, MemorySource};

/// Packets read per drain. Bounded so one enormous table cannot spend the whole
/// execution; the termination assertion is separate and tighter.
const MAX_PACKETS: usize = 4096;

/// The identity of a packet, for comparing two reads of the same file.
type Row = (u32, Option<i64>, Option<u64>, usize);

fn row(p: &vaco_packet::Packet) -> Row {
    (p.stream_index, p.dts.ticks(), p.pos, p.payload().len())
}

fn drain(demux: &mut Mp4Demuxer, size: u64) -> Vec<Row> {
    let streams = demux.streams().len();
    let monotonic = !demux.is_fragmented();
    let mut last = vec![i64::MIN; streams];
    let mut out = Vec::new();
    // Only `Eof` terminates. `InvalidData` is recoverable by design — the
    // demuxer drops the bad sample and carries on — so a drain that stopped at
    // the first one would compare a prefix against a whole file.
    for _ in 0..MAX_PACKETS {
        match demux.read_packet() {
            Ok(p) => {
                out.push(row(&p));
                let len = p.payload().len() as u64;
                let pos = p.pos.unwrap_or(0);
                assert!(
                    pos.saturating_add(len) <= size,
                    "packet at {pos}+{len} lies past the {size}-byte source"
                );
                let i = p.stream_index as usize;
                assert!(i < streams, "packet names stream {i} of {streams}");
                if let (Some(dts), Some(slot)) = (p.dts.ticks(), last.get_mut(i)) {
                    assert!(
                        !monotonic || dts >= *slot,
                        "progressive stream {i} decode time went backwards"
                    );
                    *slot = dts;
                }
            }
            Err(Error::Eof) => {
                // Sticky, and the trait does not promise it — so check.
                assert!(
                    matches!(demux.read_packet(), Err(Error::Eof)),
                    "Eof is not sticky"
                );
                break;
            }
            Err(_) => {}
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let size = data.len() as u64;
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options::default(),
    ) else {
        return;
    };

    // Every reported field must be answerable for any input.
    for (i, s) in demux.streams().iter().enumerate() {
        let _ = s.media_type();
        let _ = s.start_time_absolute();
        let _ = s.params.bit_rate;
        let _ = demux.duration_ts(i);
        let _ = demux.frame_rates(i);
        let _ = demux.display_matrix(i);
        assert!(
            s.time_base.den >= 0,
            "a negative time base is not a time base"
        );
    }
    let _ = demux.duration();
    let _ = demux.metadata().len();
    let _ = demux.chapters().len();

    // A file smaller than the packet cap cannot contain more packets than it
    // has bytes: every sample occupies at least one byte, and two samples of
    // one track never share one. This is the assertion that a lost bound on a
    // uniform `stsz` trips.
    //
    // The sequence a straight read produces is also the oracle for every seek
    // below. It has to come from *this* read rather than from a seek to the
    // beginning: seeking places the non-reference tracks at the instant the
    // reference landed on, so on a file whose two tracks disagree about where
    // zero is, seeking to the start legitimately skips the head of one of
    // them. A real input proved that too.
    let full = drain(&mut demux, size);
    assert!(
        full.len() as u64
            <= size
                .saturating_add(1)
                .saturating_mul(demux.streams().len() as u64 + 1)
            || full.len() == MAX_PACKETS,
        "{} packets out of a {size}-byte file",
        full.len()
    );

    if demux.streams().is_empty() {
        return;
    }
    // The ordering check below compares *emitted* packets, so it is only
    // meaningful when every declared sample was emitted. On a truncated file
    // the samples between the landing point and the first readable one are
    // dropped, and a seek that landed correctly still reports a later packet
    // than the straight read's own "last packet at or before the target" — a
    // real input proved that too. Membership stays unconditional.
    let declared: u64 = demux.streams().iter().filter_map(|s| s.frame_count).sum();
    let complete = full.len() as u64 == declared;
    let progressive = !demux.is_fragmented() && complete;

    for target in [i64::MIN, -1, 0, 1, 1 << 20, i64::MAX] {
        let ts = Timestamp::new(target);
        if demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts,
                },
                SeekFlags::BACKWARD,
            )
            .is_err()
        {
            continue;
        }
        let mut landed = None;
        for _ in 0..MAX_PACKETS {
            match demux.read_packet() {
                Ok(p) if p.stream_index == 0 => {
                    landed = Some(row(&p));
                    break;
                }
                Ok(_) => {}
                Err(Error::Eof) => break,
                Err(_) => {}
            }
        }
        if let Some(landed) = landed {
            let at = full.iter().position(|r| *r == landed);
            let Some(at) = at else {
                assert!(
                    full.len() == MAX_PACKETS,
                    "a seek produced a packet a straight read never did"
                );
                continue;
            };
            if progressive {
                // The last packet a straight read reaches at or before the
                // target, or the very first one when nothing is behind it.
                let bound = full
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.0 == 0 && r.1.is_some_and(|d| d <= target))
                    .map(|(i, _)| i)
                    .next_back()
                    .unwrap_or_else(|| full.iter().position(|r| r.0 == 0).unwrap_or(0));
                assert!(
                    at <= bound,
                    "a backward seek to {target} landed at packet {at}, past {bound}"
                );
            }
        }
        drain(&mut demux, size);
    }
});
