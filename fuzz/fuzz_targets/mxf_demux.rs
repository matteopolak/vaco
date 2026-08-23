//! Whole-file MXF demuxing over arbitrary bytes.
//!
//! MXF is length-prefixed at every layer — a BER length on every KLV
//! triplet, a `Count`/`ItemLength` pair on every batch (essence containers,
//! packages, tracks, structural components, index entries), a `Count` on
//! the primer pack and the Random Index Pack — which makes this the
//! highest-value fuzzing surface in the crate (see the brief this crate was
//! built from: "you parse untrusted length-prefixed input"). Every stage is
//! reachable from one input: partition-pack discovery, the primer, the
//! structural-metadata graph (including the cycle-guarded source-package
//! chase in [`vaco_demux_mxf::metadata::resolve_essence`]), the Index Table
//! Segment (CBE and VBE), packet emission and seeking.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Reading terminates.** A `Count` field can declare 4 billion batch
//!   items or index entries in eight bytes; every batch and array reader in
//!   this crate checks its declared size against a cap and against what
//!   actually remains in the buffer before iterating it, so a small input
//!   must still produce a small number of packets.
//! * **Every packet names a stream this demuxer actually built** — the
//!   indexing-panic surface the same shape of assertion closes off in every
//!   other demux fuzz target in this workspace.
//! * **`Eof` is stable**: a second `read_packet` after `Eof` returns `Eof`
//!   again, not a resumed read.
//! * **A metadata graph with a cycle in it terminates the open, not the
//!   fuzzer.** [`vaco_demux_mxf::metadata::resolve_essence`]'s cycle guard
//!   (a visited-UMID set, plus a hard depth cap independent of it) is
//!   exercised directly by every input that reaches header metadata at all,
//!   since `MxfDemuxer::open` calls it unconditionally.
//!
//! fuzz-crate: vaco-demux-mxf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_mxf::MxfDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::Demuxer;
use vaco_io::MemorySource;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut MxfDemuxer) -> u32 {
    let streams = d.streams().len() as u32;
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert!(
                    p.stream_index < streams,
                    "packet names stream {} of {streams}",
                    p.stream_index
                );
                assert!(p.len <= p.data.len());
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                // Sticky: the second call must agree with the first.
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return n;
            }
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = MxfDemuxer::open(src, &NoParsers) else {
        return;
    };

    for (i, s) in demux.streams().iter().enumerate() {
        assert_eq!(s.index as usize, i, "stream index does not match its slot");
    }
    if let Some(d) = demux.duration() {
        assert!(d.as_micros() >= 0);
    }

    let read = drain(&mut demux);

    if demux.streams().is_empty() {
        return;
    }
    for ts in [0i64, 1, i64::from(u32::from_le_bytes([
        data.first().copied().unwrap_or(0),
        data.get(1).copied().unwrap_or(0),
        data.get(2).copied().unwrap_or(0),
        data.get(3).copied().unwrap_or(0),
    ]))] {
        let target = SeekTarget::Timestamp {
            stream_index: 0,
            ts: vaco_core::Timestamp::new(ts),
        };
        if demux
            .seek(target, SeekFlags::ANY | SeekFlags::BACKWARD)
            .is_ok()
        {
            let after = drain(&mut demux);
            assert!(after <= read.saturating_add(MAX_PACKETS));
        }
    }
});
