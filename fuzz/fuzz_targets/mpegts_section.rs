//! PSI section reassembly over arbitrary bytes.
//!
//! The highest-value target in the MPEG-TS layer, because the section
//! assembler is the one piece of the PSI stack that carries state across
//! calls: a `pointer_field` splits a payload between the section in progress
//! and the one starting, and a section may span any number of transport
//! packets. Every off-by-one in that has to show up as a mis-framed section
//! rather than as a crash, and there is no natural test that covers the
//! combinations.
//!
//! The input is read as a sequence of `(payload_unit_start, payload)` pushes,
//! with the payload lengths taken from the input itself, so the fuzzer
//! controls the *splitting* as well as the bytes.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Every emitted section is self-consistent.** `SectionHeader::parse`
//!   succeeds on it and `3 + section_length` equals its length exactly. That
//!   is the assembler's entire contract, and a violation means some caller
//!   downstream would read past a table's end.
//! * **Nothing exceeds the twelve-bit ceiling.** A section longer than 4096
//!   bytes cannot be described by the length field, so producing one would
//!   mean the framing invented bytes.
//! * **Progress is bounded.** The assembler holds one fixed array and never
//!   allocates, so a hostile input cannot make it grow; the loop below is
//!   bounded by the input length, which is what makes a hang here a real
//!   finding rather than a large input.
//! * **The table parsers are total.** Every section is offered to `Pat`,
//!   `Pmt`, `Cat` and `Sdt`, and every iterator they hand back is drained.
//!
//! fuzz-crate: vaco-format-mpegts-tables

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_mpegts_tables::psi::{Cat, Pat, Pmt, Sdt};
use vaco_format_mpegts_tables::section::{MAX_SECTION_LEN, Section, SectionAssembler};
use vaco_format_mpegts_tables::stream_type::resolve;
use vaco_format_mpegts_tables::text;

/// Cap on pushes, so one input cannot cost time superlinear in its size.
const MAX_PUSHES: usize = 4096;

fn walk(raw: &[u8]) {
    assert!(
        raw.len() <= MAX_SECTION_LEN,
        "assembler emitted {} bytes, past the twelve-bit ceiling",
        raw.len()
    );
    let Some(section) = Section::new(raw) else {
        // The assembler frames by `section_length` alone, which a long-form
        // section can declare smaller than its own eight-byte header — found
        // by this target on its 30th execution, from `01 80 00`. That is
        // malformed input rather than a framing bug, and the framing layer is
        // right to hand it over; the *table* parsers are what must refuse it.
        // What must hold is that nothing long enough to hold a header fails.
        assert!(
            raw.len() < 8,
            "a section of {} bytes must parse its own header",
            raw.len()
        );
        return;
    };
    assert_eq!(
        section.header.total_len(),
        raw.len(),
        "emitted section length disagrees with its own section_length"
    );
    // Every accessor must be total on every section.
    let _ = section.crc_ok();
    let _ = section.body();
    let _ = section.is_applicable();

    if let Some(pat) = Pat::parse(&section) {
        for e in pat.entries() {
            assert!(e.pid <= 0x1FFF, "PAT PID out of the thirteen-bit range");
        }
        let _ = pat.nit_pid();
    }
    if let Some(pmt) = Pmt::parse(&section) {
        assert!(pmt.pcr_pid <= 0x1FFF);
        for _ in pmt.program_descriptors() {}
        for s in pmt.streams() {
            assert!(s.elementary_pid <= 0x1FFF);
            let r = resolve(s.stream_type, s.descriptors);
            let _ = r.codec.media_type();
            let _ = r.codec.codec_id();
            for d in s.descriptor_iter() {
                let _ = d.registration_format();
                let _ = d.maximum_bitrate();
                for _ in d.iso639_languages() {}
                for e in d.teletext_pages() {
                    let _ = e.page();
                    let _ = e.language_str();
                }
                for e in d.subtitling_entries() {
                    let _ = e.is_hearing_impaired();
                }
                if let Some(svc) = d.service() {
                    // DVB text decoding is reachable from any SI table and is
                    // the only part of this crate that allocates.
                    let _ = text::decode(svc.name);
                    let _ = text::decode(svc.provider);
                }
            }
        }
    }
    if let Some(cat) = Cat::parse(&section) {
        for _ in cat.descriptors() {}
    }
    if let Some(sdt) = Sdt::parse(&section) {
        for s in sdt.services() {
            let _ = s.names();
            let _ = s.service_type();
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut asm = SectionAssembler::new();
    let mut rest = data;
    let mut pushes = 0usize;
    while !rest.is_empty() && pushes < MAX_PUSHES {
        pushes += 1;
        // The first byte of each record chooses the split: bit 7 is the
        // payload-unit-start indicator and the low seven bits are the payload
        // length, so the fuzzer decides where packet boundaries fall.
        let Some((&control, tail)) = rest.split_first() else {
            break;
        };
        let pusi = control & 0x80 != 0;
        let want = usize::from(control & 0x7F).min(tail.len());
        let (payload, next) = tail.split_at(want);
        asm.push(pusi, payload, walk);
        rest = next;
    }
    let stats = asm.stats();
    // A section can only be emitted from bytes that were pushed, so the count
    // is bounded by the input. This catches an assembler that re-emits.
    assert!(
        stats.emitted <= data.len() as u64,
        "emitted more sections than there were input bytes"
    );
});
